use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;

use crate::BNL;
use crate::transport::basis_network_stack_registry::BasisNetworkStackRegistry;

use super::{BasisXmlConfig, ConfigFieldError};
use super::basis_config_xml_docs::{BasisConfigXmlDocs, ConfigXmlError};

/// A transport config held by the store without knowing its concrete type — the object-typed
/// half of the C# store (`object Get(string)`), with the XML operations the store needs.
pub trait BasisTransportConfigObject: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn clone_box(&self) -> Box<dyn BasisTransportConfigObject>;
    fn type_name(&self) -> &'static str;
    fn xml_root(&self) -> &'static str;
    fn to_xml(&self) -> Result<String, ConfigXmlError>;
    fn load_xml(&mut self, xml: &str) -> Result<(), ConfigXmlError>;
    fn needs_upgrade(&self, path: &Path) -> bool;
    fn stamp_version(&mut self);
    fn read_version(&self) -> i32;
    fn migrate_from(&mut self, loaded_version: i32);
    fn get_field(&self, name: &str) -> Option<String>;
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), ConfigFieldError>;
    fn field_kind(&self, name: &str) -> Option<super::FieldKind>;
}

impl<T: BasisXmlConfig> BasisTransportConfigObject for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn clone_box(&self) -> Box<dyn BasisTransportConfigObject> {
        Box::new(self.clone())
    }
    fn type_name(&self) -> &'static str {
        std::any::type_name::<T>()
    }
    fn xml_root(&self) -> &'static str {
        T::XML_ROOT
    }
    fn to_xml(&self) -> Result<String, ConfigXmlError> {
        BasisConfigXmlDocs::serialize(self)
    }
    fn load_xml(&mut self, xml: &str) -> Result<(), ConfigXmlError> {
        *self = BasisConfigXmlDocs::deserialize::<T>(xml)?;
        Ok(())
    }
    fn needs_upgrade(&self, path: &Path) -> bool {
        BasisConfigXmlDocs::needs_upgrade(path, self)
    }
    fn stamp_version(&mut self) {
        BasisConfigXmlDocs::stamp_version(self);
    }
    fn read_version(&self) -> i32 {
        BasisConfigXmlDocs::read_version(self)
    }
    fn migrate_from(&mut self, loaded_version: i32) {
        BasisXmlConfig::migrate_from(self, loaded_version);
    }
    fn get_field(&self, name: &str) -> Option<String> {
        BasisXmlConfig::get_field(self, name)
    }
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), ConfigFieldError> {
        BasisXmlConfig::set_field(self, name, value)
    }
    fn field_kind(&self, name: &str) -> Option<super::FieldKind> {
        T::field_kind(name)
    }
}

struct RegisteredType {
    type_name: &'static str,
    create: fn() -> Box<dyn BasisTransportConfigObject>,
}

struct StoreState {
    configs: HashMap<String, Box<dyn BasisTransportConfigObject>>,
    types: HashMap<String, RegisteredType>,
    /// Display form of each registered id (the map keys are lower-cased for the C#
    /// case-insensitive comparer).
    ids: HashMap<String, String>,
}

static STATE: Mutex<Option<StoreState>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut StoreState) -> R) -> R {
    let mut guard = STATE.lock();
    let state = guard.get_or_insert_with(|| StoreState { configs: HashMap::new(), types: HashMap::new(), ids: HashMap::new() });
    f(state)
}

fn key(stack_id: &str) -> String {
    stack_id.to_ascii_lowercase()
}

/// Holds one config object per network stack id and reads/writes them as
/// `{configDir}/transports/{stackId}.xml` sidecars.
///
/// The C# `Get<T>` returned the live object; here `get` returns a clone and
/// [`BasisTransportConfigStore::with_mut`] edits in place, which is the same contract without a
/// shared mutable reference escaping the lock.
pub struct BasisTransportConfigStore;

impl BasisTransportConfigStore {
    pub const TRANSPORTS_FOLDER_NAME: &'static str = "transports";

    /// Registers the config type a stack id's sidecar deserializes as. Re-registering keeps the
    /// existing config instance. Panics on an empty id (the C# `ArgumentException`).
    pub fn register_type<T: BasisXmlConfig>(stack_id: &str) {
        assert!(!stack_id.is_empty(), "Stack id is required (Parameter 'stackId')");
        with_state(|s| {
            let k = key(stack_id);
            s.ids.entry(k.clone()).or_insert_with(|| stack_id.to_string());
            s.types.insert(
                k.clone(),
                RegisteredType { type_name: std::any::type_name::<T>(), create: || Box::new(T::default()) },
            );
            s.configs.entry(k).or_insert_with(|| Box::new(T::default()));
        });
    }

    /// The config for `stack_id` as `T`. A missing or differently-typed entry is replaced with a
    /// fresh default, exactly as the C# generic `Get<T>` did. Empty id routes to the default stack.
    pub fn get<T: BasisXmlConfig>(stack_id: &str) -> T {
        let id = if stack_id.is_empty() { BasisNetworkStackRegistry::DEFAULT_ID } else { stack_id };
        with_state(|s| {
            let k = key(id);
            if let Some(c) = s.configs.get(&k)
                && let Some(typed) = c.as_any().downcast_ref::<T>()
            {
                return typed.clone();
            }
            let fresh = T::default();
            s.ids.entry(k.clone()).or_insert_with(|| id.to_string());
            s.configs.insert(k, Box::new(fresh.clone()));
            fresh
        })
    }

    /// The untyped config for `stack_id` (a clone), or `None` for an empty/unknown id.
    pub fn get_object(stack_id: &str) -> Option<Box<dyn BasisTransportConfigObject>> {
        if stack_id.is_empty() {
            return None;
        }
        with_state(|s| s.configs.get(&key(stack_id)).map(|c| c.clone_box()))
    }

    /// Edits the stored config in place; creates a default when missing or differently typed.
    pub fn with_mut<T: BasisXmlConfig, R>(stack_id: &str, f: impl FnOnce(&mut T) -> R) -> R {
        let id = if stack_id.is_empty() { BasisNetworkStackRegistry::DEFAULT_ID } else { stack_id };
        with_state(|s| {
            let k = key(id);
            let needs_fresh = !matches!(s.configs.get(&k), Some(c) if c.as_any().is::<T>());
            if needs_fresh {
                s.ids.entry(k.clone()).or_insert_with(|| id.to_string());
                s.configs.insert(k.clone(), Box::new(T::default()));
            }
            match s.configs.get_mut(&k).and_then(|obj| obj.as_any_mut().downcast_mut::<T>()) {
                Some(config) => f(config),
                None => {
                    // Unreachable after the insert above; handled rather than trusted.
                    let mut fresh = T::default();
                    let result = f(&mut fresh);
                    s.configs.insert(k, Box::new(fresh));
                    result
                }
            }
        })
    }

    /// Edits the untyped config in place. Returns `None` for an unknown id.
    pub fn with_object_mut<R>(stack_id: &str, f: impl FnOnce(&mut dyn BasisTransportConfigObject) -> R) -> Option<R> {
        if stack_id.is_empty() {
            return None;
        }
        with_state(|s| s.configs.get_mut(&key(stack_id)).map(|c| f(c.as_mut())))
    }

    /// Stores `config` as the live instance for `stack_id`. Panics on an empty id.
    pub fn set<T: BasisXmlConfig>(stack_id: &str, config: T) {
        assert!(!stack_id.is_empty(), "Stack id is required (Parameter 'stackId')");
        with_state(|s| {
            let k = key(stack_id);
            s.ids.entry(k.clone()).or_insert_with(|| stack_id.to_string());
            s.configs.insert(k, Box::new(config));
        });
    }

    /// Registered stack id → Rust type name.
    pub fn registered_types() -> HashMap<String, &'static str> {
        with_state(|s| {
            s.types
                .iter()
                .map(|(k, t)| (s.ids.get(k).cloned().unwrap_or_else(|| k.clone()), t.type_name))
                .collect()
        })
    }

    pub fn is_type_registered(stack_id: &str) -> bool {
        with_state(|s| s.types.contains_key(&key(stack_id)))
    }

    fn sidecar_path(config_base_dir: &Path, stack_id: &str) -> PathBuf {
        config_base_dir.join(Self::TRANSPORTS_FOLDER_NAME).join(format!("{stack_id}.xml"))
    }

    pub fn load_all(config_base_dir: &Path) {
        if config_base_dir.as_os_str().is_empty() {
            return;
        }
        let transports_dir = config_base_dir.join(Self::TRANSPORTS_FOLDER_NAME);
        if let Err(e) = std::fs::create_dir_all(&transports_dir) {
            BNL::log_warning(format!("Could not create transports dir '{}': {e}", transports_dir.display()));
        }
        let types: Vec<(String, String, fn() -> Box<dyn BasisTransportConfigObject>)> = with_state(|s| {
            s.types
                .iter()
                .map(|(k, t)| (k.clone(), s.ids.get(k).cloned().unwrap_or_else(|| k.clone()), t.create))
                .collect()
        });
        for (k, id, create) in types {
            let path = Self::sidecar_path(config_base_dir, &id);
            let loaded = Self::load_or_create(create, &path);
            with_state(|s| {
                s.configs.insert(k, loaded);
            });
        }
    }

    pub fn save_all(config_base_dir: &Path) {
        if config_base_dir.as_os_str().is_empty() {
            return;
        }
        let transports_dir = config_base_dir.join(Self::TRANSPORTS_FOLDER_NAME);
        if let Err(e) = std::fs::create_dir_all(&transports_dir) {
            BNL::log_warning(format!("Could not create transports dir '{}': {e}", transports_dir.display()));
            return;
        }
        let snapshot: Vec<(String, Box<dyn BasisTransportConfigObject>)> = with_state(|s| {
            s.types
                .keys()
                .filter_map(|k| {
                    let id = s.ids.get(k).cloned().unwrap_or_else(|| k.clone());
                    s.configs.get(k).map(|c| (id, c.clone_box()))
                })
                .collect()
        });
        for (id, mut config) in snapshot {
            let path = Self::sidecar_path(config_base_dir, &id);
            Self::save_atomic(config.as_mut(), &path);
            // The stamp lives on the stored instance too, as it did in C#.
            with_state(|s| {
                if let Some(c) = s.configs.get_mut(&key(&id)) {
                    c.stamp_version();
                }
            });
        }
    }

    fn load_or_create(create: fn() -> Box<dyn BasisTransportConfigObject>, path: &Path) -> Box<dyn BasisTransportConfigObject> {
        if path.exists() {
            let attempt = (|| -> Result<Box<dyn BasisTransportConfigObject>, ConfigXmlError> {
                let xml = std::fs::read_to_string(path)?;
                let mut loaded = create();
                loaded.load_xml(&xml)?;
                // Retire values a newer build knows are harmful, before the upgrade re-saves.
                let version = loaded.read_version();
                loaded.migrate_from(version);
                // Heal an older sidecar: re-save when it predates the current schema version or is
                // missing any setting we now write, so new settings get added.
                if loaded.needs_upgrade(path) {
                    loaded.stamp_version();
                    Self::save_atomic(loaded.as_mut(), path);
                    BNL::log(format!("Transport config '{}' is from an older version; adding missing settings.", path.display()));
                }
                Ok(loaded)
            })();
            match attempt {
                Ok(loaded) => return loaded,
                Err(e) => BNL::log_warning(format!("Failed to load transport config '{}': {e}. Recreating.", path.display())),
            }
        }
        let mut created = create();
        created.stamp_version();
        match created.to_xml() {
            Ok(xml) => {
                if let Err(e) = std::fs::write(path, xml) {
                    BNL::log_warning(format!("Failed to write transport config '{}': {e}", path.display()));
                }
            }
            Err(e) => BNL::log_warning(format!("Failed to serialize transport config '{}': {e}", path.display())),
        }
        created
    }

    fn save_atomic(config: &mut dyn BasisTransportConfigObject, path: &Path) {
        config.stamp_version();
        let temp_path = PathBuf::from(format!("{}.tmp", path.display()));
        let result = config
            .to_xml()
            .and_then(|xml| Ok(std::fs::write(&temp_path, xml).and_then(|_| std::fs::rename(&temp_path, path))?));
        if let Err(e) = result {
            BNL::log_warning(format!("Failed to save transport config '{}': {e}", path.display()));
        }
    }

    /// Test seam: forgets every registered type and config, so a test can start from an empty
    /// store. The stack registry re-registers its own types on its next `ensure_initialized`.
    pub fn reset_for_tests() {
        *STATE.lock() = None;
        BasisNetworkStackRegistry::reset_for_tests();
    }
}
