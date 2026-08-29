//! Port of `Security/PermissionManager.cs`: permission nodes, the group/user store with
//! inheritance and deny-wins resolution, XML persistence, and the server integration layer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt};
use basis_network_core::SerializableBasis::{ClientMetaDataMessage, ServerMetaDataMessage};
use basis_network_core::configuration::ConfigXmlError;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPeerRef};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};

use crate::NetworkServer;

// =========================
// Permission Node Constants
// =========================
pub struct PermNodes;

impl PermNodes {
    pub const ALL: &'static str = "*";
    pub const HELP: &'static str = "basis.command.help";
    pub const SERVER_STATS: &'static str = "basis.server.stats";

    pub const RESOURCE_LOAD_WORLD: &'static str = "basis.resource.load.world";
    pub const RESOURCE_UNLOAD_WORLD: &'static str = "basis.resource.unload.world";
    pub const RESOURCE_LOAD_PROP: &'static str = "basis.resource.load.prop";
    pub const RESOURCE_UNLOAD_PROP: &'static str = "basis.resource.unload.prop";
    pub const RESOURCE_LOAD_AVATAR: &'static str = "basis.resource.load.avatar";
    pub const RESOURCE_UNLOAD_AVATAR: &'static str = "basis.resource.unload.avatar";

    // Bypass the global lockouts (BasisGlobalLockManager). Users without the matching bypass
    // node are blocked from loading while the lock is on.
    pub const RESOURCE_LOCK_BYPASS_AVATAR: &'static str = "basis.resource.lockbypass.avatar";
    pub const RESOURCE_LOCK_BYPASS_PROP: &'static str = "basis.resource.lockbypass.prop";
    pub const RESOURCE_LOCK_BYPASS_WORLD: &'static str = "basis.resource.lockbypass.world";
    /// Bypass `ServersLocked` when initiating a server share.
    pub const RESOURCE_LOCK_BYPASS_SERVER: &'static str = "basis.resource.lockbypass.server";
    /// Bypass `TextChatLocked` — keep sending text chat while the global chat lock is on.
    pub const CHAT_LOCK_BYPASS: &'static str = "basis.chat.lockbypass";
    /// Bypass `VoiceChatLocked` — keep transmitting voice while the global voice lock is on.
    pub const VOICE_LOCK_BYPASS: &'static str = "basis.voice.lockbypass";

    pub const OWNERSHIP_TRANSFER: &'static str = "basis.ownership.transfer";
    pub const OWNERSHIP_REMOVE: &'static str = "basis.ownership.remove";
    pub const OWNERSHIP_GET: &'static str = "basis.ownership.get";

    pub const CONTENT_SHARE_DELETE: &'static str = "basis.contentshare.delete";
    pub const CONTENT_SHARE_CREATE: &'static str = "basis.contentshare.create";

    /// Indicates that this person's actions are protected from interference.
    pub const PROTECTION: &'static str = "basis.protection";

    pub const CONFIGURATION_EDITOR: &'static str = "basis.configuration";

    pub const PLAYER_MODERATION: &'static str = "basis.moderation";

    pub const MODERATION_BAN: &'static str = "basis.moderation.ban";
    pub const MODERATION_KICK: &'static str = "basis.moderation.kick";
    pub const MODERATION_IP_BAN: &'static str = "basis.moderation.ipban";
    pub const MODERATION_UNBAN: &'static str = "basis.moderation.unban";
    pub const MODERATION_UNBAN_IP: &'static str = "basis.moderation.unbanip";
    pub const MODERATION_MESSAGE: &'static str = "basis.moderation.message";
    pub const MODERATION_MESSAGE_ALL: &'static str = "basis.moderation.messageall";
    pub const MODERATION_TELEPORT: &'static str = "basis.moderation.teleport";
    pub const MODERATION_SHOUT: &'static str = "basis.moderation.shout";
    pub const MODERATION_GLOBAL_LOCK: &'static str = "basis.moderation.globallock";
    pub const MODERATION_HEADLESS_AUDIO: &'static str = "basis.moderation.headlessaudio";
    pub const MODERATION_OPUS_BITRATE: &'static str = "basis.moderation.opusbitrate";
    pub const MODERATION_FULL_QUALITY_BROADCAST: &'static str = "basis.moderation.fullqualitybroadcast";
    /// Push a specific avatar onto another player.
    pub const MODERATION_FORCE_AVATAR: &'static str = "basis.moderation.forceavatar";
    /// Override another player's jump height, movement speeds, gravity and controller mode.
    pub const MODERATION_LOCOMOTION: &'static str = "basis.moderation.locomotion";
    /// Add/remove UUIDs on the server's allow-list (separate from ban management).
    pub const MODERATION_ALLOWLIST: &'static str = "basis.moderation.whitelist";
    pub const ADMIN_LOGS: &'static str = "basis.admin.logs";

    pub const PERMISSIONS_VIEW: &'static str = "basis.permissions.view";
    pub const PERMISSIONS_EDIT: &'static str = "basis.permissions.edit";
}

// =========================
// Case-insensitive collections (the C# used StringComparer.OrdinalIgnoreCase everywhere)
// =========================

fn fold(key: &str) -> String {
    key.to_lowercase()
}

/// A set of strings compared without regard to case; iteration yields the casing first inserted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaseInsensitiveSet {
    inner: HashMap<String, String>,
}

impl CaseInsensitiveSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts `value`; false when an equal (ignoring case) entry already existed.
    pub fn insert(&mut self, value: &str) -> bool {
        let key = fold(value);
        if self.inner.contains_key(&key) {
            return false;
        }
        self.inner.insert(key, value.to_string());
        true
    }

    pub fn remove(&mut self, value: &str) -> bool {
        self.inner.remove(&fold(value)).is_some()
    }

    pub fn contains(&self, value: &str) -> bool {
        self.inner.contains_key(&fold(value))
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Entries in a stable (sorted) order, so saves and listings are deterministic.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        let mut values: Vec<&str> = self.inner.values().map(String::as_str).collect();
        values.sort_unstable();
        values.into_iter()
    }

    pub fn to_vec(&self) -> Vec<String> {
        self.iter().map(str::to_string).collect()
    }
}

impl<S: AsRef<str>> FromIterator<S> for CaseInsensitiveSet {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        let mut set = Self::new();
        for value in iter {
            set.insert(value.as_ref());
        }
        set
    }
}

/// A map keyed by strings compared without regard to case.
#[derive(Clone, Debug)]
pub struct CaseInsensitiveMap<V> {
    inner: HashMap<String, (String, V)>,
}

impl<V> Default for CaseInsensitiveMap<V> {
    fn default() -> Self {
        Self { inner: HashMap::new() }
    }
}

impl<V> CaseInsensitiveMap<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.inner.get(&fold(key)).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.inner.get_mut(&fold(key)).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.contains_key(&fold(key))
    }

    pub fn insert(&mut self, key: &str, value: V) -> Option<V> {
        self.inner.insert(fold(key), (key.to_string(), value)).map(|(_, v)| v)
    }

    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.inner.remove(&fold(key)).map(|(_, v)| v)
    }

    pub fn entry_or_insert_with(&mut self, key: &str, make: impl FnOnce() -> V) -> &mut V {
        &mut self.inner.entry(fold(key)).or_insert_with(|| (key.to_string(), make())).1
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// `(original key, value)` pairs sorted by key.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        let mut entries: Vec<(&str, &V)> = self.inner.values().map(|(k, v)| (k.as_str(), v)).collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        entries.into_iter()
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.inner.values_mut().map(|(_, v)| v)
    }

    pub fn keys(&self) -> Vec<String> {
        self.iter().map(|(k, _)| k.to_string()).collect()
    }
}

// =========================
// Data Model
// =========================
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionUser {
    pub uuid: String,
    /// Raw nodes assigned to the user (can include "-node" deny entries).
    pub nodes: CaseInsensitiveSet,
    /// Group memberships.
    pub groups: CaseInsensitiveSet,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionGroup {
    pub name: String,
    /// Raw nodes assigned to the group (can include "-node" deny entries).
    pub nodes: CaseInsensitiveSet,
    /// Parent group inheritance.
    pub parents: CaseInsensitiveSet,
}

#[derive(Clone, Debug, Default)]
pub struct PermissionStore {
    pub users: CaseInsensitiveMap<PermissionUser>,
    pub groups: CaseInsensitiveMap<PermissionGroup>,
}

/// The resolved decision table for one user: node => allow(true) / deny(false). Contains exact
/// nodes and wildcard nodes ("a.*", "*") after inheritance resolution.
#[derive(Clone, Debug, Default)]
pub struct EffectivePermissions {
    decisions: CaseInsensitiveMap<bool>,
}

impl EffectivePermissions {
    pub fn new(decisions: CaseInsensitiveMap<bool>) -> Self {
        Self { decisions }
    }

    /// O(depth) check: a.b.c -> a.b.* -> a.* -> *
    pub fn has(&self, node: &str) -> bool {
        let node = node.trim();
        if node.is_empty() {
            return false;
        }
        if let Some(exact) = self.decisions.get(node) {
            return *exact;
        }
        // climb wildcards
        let mut prefix = node;
        while let Some(idx) = prefix.rfind('.') {
            prefix = &prefix[..idx];
            if let Some(w) = self.decisions.get(&format!("{prefix}.*")) {
                return *w;
            }
        }
        // global wildcard
        self.decisions.get("*").copied().unwrap_or(false)
    }

    /// All rules with allow(true). These are effective *rules*, not an expanded node list.
    pub fn get_all_allowed_rules(&self) -> Vec<String> {
        self.decisions.iter().filter(|(_, allow)| **allow).map(|(node, _)| node.to_string()).collect()
    }

    pub fn get_all_denied_rules(&self) -> Vec<String> {
        self.decisions.iter().filter(|(_, allow)| !**allow).map(|(node, _)| node.to_string()).collect()
    }

    /// For debugging/admin UIs.
    pub fn get_decision_map(&self) -> Vec<(String, bool)> {
        self.decisions.iter().map(|(node, allow)| (node.to_string(), *allow)).collect()
    }
}

// ============================================
// Permission Manager (Thread-safe + Cached)
// ============================================

struct CacheEntry {
    version: u64,
    perms: Arc<EffectivePermissions>,
}

struct Inner {
    store: PermissionStore,
    /// uuid -> (version, effective perms)
    cache: HashMap<String, CacheEntry>,
    version: u64,
}

pub type PermissionsChangedHandler = Arc<dyn Fn(Option<&str>) + Send + Sync>;

pub struct PermissionManager {
    inner: RwLock<Inner>,
    xml_path: Mutex<PathBuf>,
    dirty: AtomicBool,
    save_debounce_ms: AtomicU64,
    /// When the pending debounced save should fire; None when nothing is pending.
    save_deadline: Arc<Mutex<Option<Instant>>>,
    saver_running: Arc<AtomicBool>,
    /// Fired after a permission mutation, outside the write lock. The argument is the affected
    /// UUID, or None when a group change affects all users.
    on_permissions_changed: RwLock<Option<PermissionsChangedHandler>>,
    weak: std::sync::Weak<PermissionManager>,
}

impl PermissionManager {
    pub const DEFAULT_SAVE_DEBOUNCE_MS: u64 = 750;

    pub fn new() -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            inner: RwLock::new(Inner { store: PermissionStore::default(), cache: HashMap::new(), version: 0 }),
            xml_path: Mutex::new(PathBuf::from("permissions.xml")),
            dirty: AtomicBool::new(false),
            save_debounce_ms: AtomicU64::new(Self::DEFAULT_SAVE_DEBOUNCE_MS),
            save_deadline: Arc::new(Mutex::new(None)),
            saver_running: Arc::new(AtomicBool::new(false)),
            on_permissions_changed: RwLock::new(None),
            weak: weak.clone(),
        })
    }

    pub fn save_debounce_ms(&self) -> u64 {
        self.save_debounce_ms.load(Ordering::Relaxed)
    }

    pub fn set_save_debounce_ms(&self, ms: u64) {
        self.save_debounce_ms.store(ms, Ordering::Relaxed);
    }

    pub fn set_on_permissions_changed(&self, handler: Option<PermissionsChangedHandler>) {
        *self.on_permissions_changed.write() = handler;
    }

    fn raise_changed(&self, uuid: Option<&str>) {
        let handler = self.on_permissions_changed.read().clone();
        if let Some(handler) = handler {
            handler(uuid);
        }
    }

    // -------------
    // Public API
    // -------------
    pub fn set_xml_path(&self, path: impl Into<PathBuf>) -> BasisResult<()> {
        let path = path.into();
        if path.as_os_str().is_empty() || path.to_string_lossy().trim().is_empty() {
            return Err(BasisError::permanent(ErrorCode::InvalidArgument, "path cannot be empty"));
        }
        *self.xml_path.lock() = path;
        Ok(())
    }

    pub fn get_xml_path(&self) -> PathBuf {
        self.xml_path.lock().clone()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Replaces the store with the contents of the XML file (empty when the file is missing). A
    /// file that exists but cannot be parsed is an error and leaves the store untouched.
    pub fn load_from_xml(&self, path_override: Option<&Path>) -> BasisResult<()> {
        let path = path_override.map(Path::to_path_buf).unwrap_or_else(|| self.get_xml_path());
        let loaded = PermissionXml::load(&path)?;
        let mut inner = self.inner.write();
        inner.store = loaded;
        inner.version += 1;
        inner.cache.clear();
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    pub fn save_to_xml(&self, path_override: Option<&Path>) -> BasisResult<()> {
        let path = path_override.map(Path::to_path_buf).unwrap_or_else(|| self.get_xml_path());
        let snapshot = self.snapshot();
        PermissionXml::save(&path, &snapshot)?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    /// Debounced save: call this after edits. The write happens on a background thread once the
    /// edits stop for `save_debounce_ms`; a failure is logged (there is nobody to return it to).
    pub fn save_to_xml_debounced(&self) {
        self.dirty.store(true, Ordering::Release);
        let debounce = Duration::from_millis(self.save_debounce_ms());
        *self.save_deadline.lock() = Some(Instant::now() + debounce);
        if self.saver_running.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = self.weak.clone();
        let deadline = self.save_deadline.clone();
        let running = self.saver_running.clone();
        let spawned = std::thread::Builder::new().name("BasisPermissionSaver".to_string()).spawn(move || {
            loop {
                let due = deadline.lock().unwrap_or_else(Instant::now);
                let now = Instant::now();
                if due > now {
                    std::thread::sleep(due - now);
                    continue;
                }
                *deadline.lock() = None;
                if let Some(manager) = weak.upgrade()
                    && manager.is_dirty()
                    && let Err(e) = manager.save_to_xml(None)
                {
                    BNL::log_error(format!("Saving permissions failed: {e}"));
                }
                // A save requested while we were writing sets a fresh deadline; go around again.
                if deadline.lock().is_none() {
                    running.store(false, Ordering::Release);
                    break;
                }
            }
        });
        if let Err(e) = spawned {
            self.saver_running.store(false, Ordering::Release);
            BNL::log_error(format!("Could not start the permission saver thread: {e}"));
        }
    }

    /// Blocks until a pending debounced save has been written. For tests and shutdown.
    pub fn flush(&self) -> BasisResult<()> {
        while self.saver_running.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.is_dirty() { self.save_to_xml(None) } else { Ok(()) }
    }

    pub fn has(&self, uuid: &str, node: &str) -> bool {
        self.get_effective(uuid).has(node)
    }

    pub fn get_all_allowed_rules(&self, uuid: &str) -> Vec<String> {
        self.get_effective(uuid).get_all_allowed_rules()
    }

    pub fn get_all_denied_rules(&self, uuid: &str) -> Vec<String> {
        self.get_effective(uuid).get_all_denied_rules()
    }

    pub fn try_get_user(&self, uuid: &str) -> Option<PermissionUser> {
        self.inner.read().store.users.get(uuid).cloned()
    }

    pub fn try_get_group(&self, name: &str) -> Option<PermissionGroup> {
        self.inner.read().store.groups.get(name).cloned()
    }

    /// Create or get user (a copy; mutate through the manager's methods).
    pub fn get_or_create_user(&self, uuid: &str) -> PermissionUser {
        if let Some(user) = self.try_get_user(uuid) {
            return user;
        }
        let mut inner = self.inner.write();
        if inner.store.users.contains_key(uuid) {
            // Created between the read and the write lock.
        } else {
            let user = Self::new_user(uuid);
            inner.store.users.insert(uuid, user);
            Self::touch_user(&mut inner, uuid);
            self.dirty.store(true, Ordering::Release);
        }
        inner.store.users.get(uuid).cloned().unwrap_or_else(|| Self::new_user(uuid))
    }

    /// Create or get group (a copy; mutate through the manager's methods).
    pub fn get_or_create_group(&self, name: &str) -> PermissionGroup {
        if let Some(group) = self.try_get_group(name) {
            return group;
        }
        let mut inner = self.inner.write();
        if !inner.store.groups.contains_key(name) {
            inner.store.groups.insert(name, PermissionGroup { name: name.to_string(), ..Default::default() });
            Self::touch_all(&mut inner);
            self.dirty.store(true, Ordering::Release);
        }
        inner.store.groups.get(name).cloned().unwrap_or_else(|| PermissionGroup { name: name.to_string(), ..Default::default() })
    }

    fn new_user(uuid: &str) -> PermissionUser {
        let mut user = PermissionUser { uuid: uuid.to_string(), ..Default::default() };
        user.groups.insert("default");
        user
    }

    fn blank(value: &str) -> bool {
        value.trim().is_empty()
    }

    // Mutators (invalidate cache)
    pub fn add_user_node(&self, uuid: &str, node: &str) {
        if Self::blank(uuid) || Self::blank(node) {
            return;
        }
        let changed = {
            let mut inner = self.inner.write();
            let user = inner.store.users.entry_or_insert_with(uuid, || Self::new_user(uuid));
            let changed = user.nodes.insert(node.trim());
            if changed {
                Self::touch_user(&mut inner, uuid);
            }
            changed
        };
        self.save_to_xml_debounced();
        if changed {
            self.raise_changed(Some(uuid));
        }
    }

    pub fn remove_user_node(&self, uuid: &str, node: &str) {
        if Self::blank(uuid) || Self::blank(node) {
            return;
        }
        let changed = {
            let mut inner = self.inner.write();
            let changed = inner.store.users.get_mut(uuid).is_some_and(|u| u.nodes.remove(node.trim()));
            if changed {
                Self::touch_user(&mut inner, uuid);
            }
            changed
        };
        self.save_to_xml_debounced();
        if changed {
            self.raise_changed(Some(uuid));
        }
    }

    pub fn add_user_to_group(&self, uuid: &str, group: &str) {
        if Self::blank(uuid) || Self::blank(group) {
            return;
        }
        let changed = {
            let mut inner = self.inner.write();
            let user = inner.store.users.entry_or_insert_with(uuid, || Self::new_user(uuid));
            let changed = user.groups.insert(group.trim());
            if changed {
                Self::touch_user(&mut inner, uuid);
            }
            changed
        };
        self.save_to_xml_debounced();
        if changed {
            self.raise_changed(Some(uuid));
        }
    }

    pub fn remove_user_from_group(&self, uuid: &str, group: &str) {
        if Self::blank(uuid) || Self::blank(group) {
            return;
        }
        let changed = {
            let mut inner = self.inner.write();
            let changed = inner.store.users.get_mut(uuid).is_some_and(|u| u.groups.remove(group.trim()));
            if changed {
                Self::touch_user(&mut inner, uuid);
            }
            changed
        };
        self.save_to_xml_debounced();
        if changed {
            self.raise_changed(Some(uuid));
        }
    }

    pub fn add_group_node(&self, group_name: &str, node: &str) {
        if Self::blank(group_name) || Self::blank(node) {
            return;
        }
        let changed = {
            let mut inner = self.inner.write();
            let group = inner
                .store
                .groups
                .entry_or_insert_with(group_name, || PermissionGroup { name: group_name.to_string(), ..Default::default() });
            let changed = group.nodes.insert(node.trim());
            if changed {
                Self::touch_all(&mut inner);
            }
            changed
        };
        self.save_to_xml_debounced();
        if changed {
            self.raise_changed(None);
        }
    }

    pub fn remove_group_node(&self, group_name: &str, node: &str) {
        if Self::blank(group_name) || Self::blank(node) {
            return;
        }
        let changed = {
            let mut inner = self.inner.write();
            let changed = inner.store.groups.get_mut(group_name).is_some_and(|g| g.nodes.remove(node.trim()));
            if changed {
                Self::touch_all(&mut inner);
            }
            changed
        };
        self.save_to_xml_debounced();
        if changed {
            self.raise_changed(None);
        }
    }

    pub fn add_group_parent(&self, group_name: &str, parent_name: &str) {
        if Self::blank(group_name) || Self::blank(parent_name) {
            return;
        }
        let changed = {
            let mut inner = self.inner.write();
            let group = inner
                .store
                .groups
                .entry_or_insert_with(group_name, || PermissionGroup { name: group_name.to_string(), ..Default::default() });
            let changed = group.parents.insert(parent_name.trim());
            if changed {
                Self::touch_all(&mut inner);
            }
            changed
        };
        self.save_to_xml_debounced();
        if changed {
            self.raise_changed(None);
        }
    }

    pub fn remove_group_parent(&self, group_name: &str, parent_name: &str) {
        if Self::blank(group_name) || Self::blank(parent_name) {
            return;
        }
        let changed = {
            let mut inner = self.inner.write();
            let changed = inner.store.groups.get_mut(group_name).is_some_and(|g| g.parents.remove(parent_name.trim()));
            if changed {
                Self::touch_all(&mut inner);
            }
            changed
        };
        self.save_to_xml_debounced();
        if changed {
            self.raise_changed(None);
        }
    }

    pub fn delete_group(&self, group_name: &str) -> bool {
        if Self::blank(group_name) {
            return false;
        }
        {
            let mut inner = self.inner.write();
            if inner.store.groups.remove(group_name).is_none() {
                return false;
            }
            // Remove this group from all users that reference it
            for user in inner.store.users.values_mut() {
                user.groups.remove(group_name);
            }
            // Remove this group as a parent from other groups
            for group in inner.store.groups.values_mut() {
                group.parents.remove(group_name);
            }
            Self::touch_all(&mut inner);
        }
        self.save_to_xml_debounced();
        self.raise_changed(None);
        true
    }

    /// Snapshot store for saving or admin viewing.
    pub fn snapshot(&self) -> PermissionStore {
        self.inner.read().store.clone()
    }

    fn touch_user(inner: &mut Inner, uuid: &str) {
        inner.version += 1;
        inner.cache.remove(&fold(uuid));
    }

    fn touch_all(inner: &mut Inner) {
        inner.version += 1;
        inner.cache.clear();
    }

    pub fn evict_user_cache(&self, uuid: &str) {
        if uuid.is_empty() {
            return;
        }
        self.inner.write().cache.remove(&fold(uuid));
    }

    fn get_effective(&self, uuid: &str) -> Arc<EffectivePermissions> {
        let key = fold(uuid);
        {
            let inner = self.inner.read();
            if let Some(entry) = inner.cache.get(&key)
                && entry.version == inner.version
            {
                return entry.perms.clone();
            }
        }
        let mut inner = self.inner.write();
        if let Some(entry) = inner.cache.get(&key)
            && entry.version == inner.version
        {
            return entry.perms.clone();
        }
        let built = Arc::new(Self::build_effective_from(&inner.store, uuid));
        let version = inner.version;
        inner.cache.insert(key, CacheEntry { version, perms: built.clone() });
        built
    }

    /// Resolves a user's permissions against the current store, bypassing the cache.
    pub fn build_effective(&self, uuid: &str) -> EffectivePermissions {
        Self::build_effective_from(&self.inner.read().store, uuid)
    }

    fn build_effective_from(store: &PermissionStore, uuid: &str) -> EffectivePermissions {
        // deny-wins decision table
        let mut decisions: CaseInsensitiveMap<bool> = CaseInsensitiveMap::new();

        // If the user doesn't exist in the store, treat them as part of the implicit "default"
        // group. This makes <Group name="default"> behave like an actual default.
        let implicit;
        let user = match store.users.get(uuid) {
            Some(user) => user,
            None => {
                implicit = Self::new_user(uuid);
                &implicit
            }
        };

        // 1) Apply groups w/ inheritance (parents first)
        let mut visited = CaseInsensitiveSet::new();
        for group in user.groups.iter() {
            Self::apply_group_recursive(store, group, &mut visited, &mut decisions);
        }
        // 2) Apply user nodes last (user overrides groups; deny still wins)
        Self::apply_raw_nodes(&user.nodes, &mut decisions);

        EffectivePermissions::new(decisions)
    }

    fn apply_group_recursive(store: &PermissionStore, group_name: &str, visited: &mut CaseInsensitiveSet, decisions: &mut CaseInsensitiveMap<bool>) {
        let group_name = group_name.trim();
        if group_name.is_empty() || !visited.insert(group_name) {
            return;
        }
        let Some(group) = store.groups.get(group_name) else {
            return;
        };
        // Parents first, then this group
        for parent in group.parents.iter() {
            Self::apply_group_recursive(store, parent, visited, decisions);
        }
        Self::apply_raw_nodes(&group.nodes, decisions);
    }

    /// Raw nodes may include "-node" denies. Deny always wins over allow.
    fn apply_raw_nodes(raw_nodes: &CaseInsensitiveSet, decisions: &mut CaseInsensitiveMap<bool>) {
        for raw in raw_nodes.iter() {
            let mut node = raw.trim();
            if node.is_empty() {
                continue;
            }
            let mut allow = true;
            if let Some(rest) = node.strip_prefix('-') {
                allow = false;
                node = rest.trim();
                if node.is_empty() {
                    continue;
                }
            }
            match decisions.get(node).copied() {
                // already denied, never overwrite
                Some(false) => continue,
                // existing allow can be overridden by deny
                Some(true) => {
                    decisions.insert(node, allow);
                }
                None => {
                    decisions.insert(node, allow);
                }
            }
        }
    }

    // -----------------------
    // Convenience: default setup
    // -----------------------
    pub fn ensure_defaults(&self) {
        {
            let mut inner = self.inner.write();
            if !inner.store.groups.contains_key("default") {
                let mut def = PermissionGroup { name: "default".to_string(), ..Default::default() };
                for node in [
                    PermNodes::HELP,
                    PermNodes::RESOURCE_LOAD_PROP,
                    PermNodes::RESOURCE_UNLOAD_PROP,
                    PermNodes::RESOURCE_LOAD_AVATAR,
                    PermNodes::RESOURCE_UNLOAD_AVATAR,
                    PermNodes::RESOURCE_LOAD_WORLD,
                    PermNodes::RESOURCE_UNLOAD_WORLD,
                    PermNodes::OWNERSHIP_TRANSFER,
                    PermNodes::OWNERSHIP_REMOVE,
                    PermNodes::OWNERSHIP_GET,
                    PermNodes::CONTENT_SHARE_DELETE,
                    PermNodes::CONTENT_SHARE_CREATE,
                ] {
                    def.nodes.insert(node);
                }
                inner.store.groups.insert("default", def);
            }
            if !inner.store.groups.contains_key("moderator") {
                let mut adm = PermissionGroup { name: "moderator".to_string(), ..Default::default() };
                adm.parents.insert("default");
                for node in [
                    PermNodes::MODERATION_BAN,
                    PermNodes::MODERATION_KICK,
                    PermNodes::MODERATION_IP_BAN,
                    PermNodes::MODERATION_UNBAN,
                    PermNodes::MODERATION_UNBAN_IP,
                    PermNodes::MODERATION_MESSAGE,
                    PermNodes::MODERATION_MESSAGE_ALL,
                    PermNodes::MODERATION_TELEPORT,
                    PermNodes::MODERATION_SHOUT,
                    PermNodes::MODERATION_GLOBAL_LOCK,
                    PermNodes::MODERATION_HEADLESS_AUDIO,
                    PermNodes::MODERATION_OPUS_BITRATE,
                    PermNodes::MODERATION_FULL_QUALITY_BROADCAST,
                    PermNodes::MODERATION_FORCE_AVATAR,
                    PermNodes::MODERATION_LOCOMOTION,
                    PermNodes::PERMISSIONS_VIEW,
                    PermNodes::RESOURCE_LOCK_BYPASS_AVATAR,
                    PermNodes::RESOURCE_LOCK_BYPASS_PROP,
                    PermNodes::RESOURCE_LOCK_BYPASS_WORLD,
                    PermNodes::RESOURCE_LOCK_BYPASS_SERVER,
                    PermNodes::CHAT_LOCK_BYPASS,
                    PermNodes::VOICE_LOCK_BYPASS,
                ] {
                    adm.nodes.insert(node);
                }
                inner.store.groups.insert("moderator", adm);
            }
            if !inner.store.groups.contains_key("admin") {
                let mut adm = PermissionGroup { name: "admin".to_string(), ..Default::default() };
                adm.nodes.insert("*");
                adm.parents.insert("moderator");
                inner.store.groups.insert("admin", adm);
            }
            Self::touch_all(&mut inner);
        }
        self.save_to_xml_debounced();
    }
}

// =========================================
// XML Persistence
// =========================================
//
// <Permissions>
//   <Groups>
//     <Group name="default">
//       <Parent name="base"/>
//       <Node value="basis.command.help"/>
//       <Node value="-basis.command.kick"/>
//     </Group>
//   </Groups>
//   <Users>
//     <User uuid="abc">
//       <Group name="admin"/>
//       <Node value="basis.resource.load"/>
//     </User>
//   </Users>
// </Permissions>
pub struct PermissionXml;

impl PermissionXml {
    /// An empty store when the file is missing; an error when it exists but cannot be read or
    /// parsed (the operator's file is left alone for them to fix).
    pub fn load(path: &Path) -> BasisResult<PermissionStore> {
        if !path.exists() {
            return Ok(PermissionStore::default());
        }
        let xml = std::fs::read_to_string(path).with_context(|| format!("reading '{}'", path.display()))?;
        Self::parse(&xml).map_err(|e| BasisError::permanent(ErrorCode::Serialization, format!("permissions file '{}': {e}", path.display())))
    }

    pub fn parse(xml: &str) -> Result<PermissionStore, ConfigXmlError> {
        use quick_xml::Reader;
        use quick_xml::events::{BytesStart, Event};

        fn attr(e: &BytesStart, name: &str) -> String {
            e.attributes()
                .flatten()
                .find(|a| a.key.as_ref() == name)
                .and_then(|a| a.normalized_value(quick_xml::XmlVersion::default()).ok().map(|v| v.into_owned()))
                .unwrap_or_default()
        }

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut store = PermissionStore::default();
        let mut current_group: Option<String> = None;
        let mut current_user: Option<String> = None;
        let mut in_groups = false;
        let mut in_users = false;
        let mut depth = 0usize;

        loop {
            let event = reader.read_event_into(&mut buf).map_err(|e| ConfigXmlError::Malformed(e.to_string()))?;
            let (start, is_empty) = match &event {
                Event::Start(e) => (Some(e), false),
                Event::Empty(e) => (Some(e), true),
                Event::End(e) => {
                    depth = depth.saturating_sub(1);
                    match e.name().as_ref() {
                        // only clear the group definition context (not user group membership)
                        "Group" if in_groups => current_group = None,
                        "User" => current_user = None,
                        "Groups" => {
                            in_groups = false;
                            current_group = None;
                        }
                        "Users" => {
                            in_users = false;
                            current_user = None;
                        }
                        _ => {}
                    }
                    buf.clear();
                    continue;
                }
                Event::Eof => {
                    if depth > 0 {
                        return Err(ConfigXmlError::Malformed("unexpected end of document".to_string()));
                    }
                    break;
                }
                _ => {
                    buf.clear();
                    continue;
                }
            };
            let Some(e) = start else {
                continue;
            };
            if !is_empty {
                depth += 1;
            }
            let name = e.name().as_ref().to_owned();
            match name.as_str() {
                "Groups" => {
                    in_groups = true;
                    in_users = false;
                }
                "Users" => {
                    in_users = true;
                    in_groups = false;
                }
                "Group" => {
                    // A group definition inside <Groups>, or a membership inside <User>.
                    let group_name = attr(e, "name");
                    if in_groups {
                        store.groups.insert(&group_name, PermissionGroup { name: group_name.clone(), ..Default::default() });
                        current_group = if is_empty { None } else { Some(group_name) };
                    } else if in_users
                        && let Some(user) = current_user.as_ref().and_then(|u| store.users.get_mut(u))
                        && !group_name.trim().is_empty()
                    {
                        user.groups.insert(group_name.trim());
                    }
                }
                "User" => {
                    let uuid = attr(e, "uuid");
                    store.users.insert(&uuid, PermissionUser { uuid: uuid.clone(), ..Default::default() });
                    current_user = if is_empty { None } else { Some(uuid) };
                }
                "Parent" => {
                    let parent = attr(e, "name");
                    if let Some(group) = current_group.as_ref().and_then(|g| store.groups.get_mut(g))
                        && !parent.trim().is_empty()
                    {
                        group.parents.insert(parent.trim());
                    }
                }
                "Node" => {
                    let node = attr(e, "value");
                    let node = node.trim();
                    if !node.is_empty() {
                        if in_groups {
                            if let Some(group) = current_group.as_ref().and_then(|g| store.groups.get_mut(g)) {
                                group.nodes.insert(node);
                            }
                        } else if in_users
                            && let Some(user) = current_user.as_ref().and_then(|u| store.users.get_mut(u))
                        {
                            user.nodes.insert(node);
                        }
                    }
                }
                _ => {}
            }
            buf.clear();
        }
        Ok(store)
    }

    pub fn save(path: &Path, store: &PermissionStore) -> BasisResult<()> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).with_context(|| format!("creating '{}'", dir.display()))?;
        }
        std::fs::write(path, Self::to_xml(store)).with_context(|| format!("writing '{}'", path.display()))
    }

    pub fn to_xml(store: &PermissionStore) -> String {
        fn esc(value: &str) -> String {
            quick_xml::escape::escape(value).into_owned()
        }
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Permissions>\n  <Groups>\n");
        for (_, group) in store.groups.iter() {
            xml.push_str(&format!("    <Group name=\"{}\">\n", esc(&group.name)));
            for parent in group.parents.iter() {
                xml.push_str(&format!("      <Parent name=\"{}\" />\n", esc(parent)));
            }
            for node in group.nodes.iter() {
                xml.push_str(&format!("      <Node value=\"{}\" />\n", esc(node)));
            }
            xml.push_str("    </Group>\n");
        }
        xml.push_str("  </Groups>\n  <Users>\n");
        for (_, user) in store.users.iter() {
            xml.push_str(&format!("    <User uuid=\"{}\">\n", esc(&user.uuid)));
            for group in user.groups.iter() {
                xml.push_str(&format!("      <Group name=\"{}\" />\n", esc(group)));
            }
            for node in user.nodes.iter() {
                xml.push_str(&format!("      <Node value=\"{}\" />\n", esc(node)));
            }
            xml.push_str("    </User>\n");
        }
        xml.push_str("  </Users>\n</Permissions>");
        xml
    }
}

// =========================================
// Server integration
// =========================================

static MANAGER: LazyLock<Arc<PermissionManager>> = LazyLock::new(PermissionManager::new);
/// Per-player metadata stored at connect, used to rebuild ServerMetaDataMessage on permission
/// changes. Keyed by the folded UUID; the value keeps the original.
static PLAYER_META: LazyLock<DashMap<String, (String, ClientMetaDataMessage)>> = LazyLock::new(DashMap::new);

pub struct PermissionIntegration;

impl PermissionIntegration {
    /// The process-lifetime manager.
    pub fn manager() -> &'static PermissionManager {
        &MANAGER
    }

    /// Call at server startup. Loads the file (a corrupt one is an error), installs the default
    /// groups and subscribes the change handler.
    pub fn init(xml_path: impl Into<PathBuf>) -> BasisResult<()> {
        let manager = Self::manager();
        manager.set_xml_path(xml_path)?;
        manager.load_from_xml(None)?;
        // Optional defaults if file was empty/nonexistent
        manager.ensure_defaults();
        // Ensure saved
        manager.save_to_xml_debounced();
        // Init runs on every start_server against a process-lifetime manager; replacing (not
        // stacking) the handler means a restart cannot resend every update twice.
        manager.set_on_permissions_changed(Some(Arc::new(Self::handle_permissions_changed)));
        Ok(())
    }

    pub fn init_without_disc() {
        let manager = Self::manager();
        manager.ensure_defaults();
        manager.set_on_permissions_changed(Some(Arc::new(Self::handle_permissions_changed)));
    }

    /// Store player metadata when they connect so we can rebuild ServerMetaDataMessage later.
    pub fn store_player_meta(uuid: &str, meta: ClientMetaDataMessage) {
        PLAYER_META.insert(fold(uuid), (uuid.to_string(), meta));
    }

    /// Remove stored metadata when a player disconnects.
    pub fn remove_player_meta(uuid: &str) {
        PLAYER_META.remove(&fold(uuid));
    }

    pub fn try_get_player_meta(uuid: &str) -> Option<ClientMetaDataMessage> {
        PLAYER_META.get(&fold(uuid)).map(|e| e.1.clone())
    }

    pub fn evict_permission_cache(uuid: &str) {
        Self::manager().evict_user_cache(uuid);
    }

    /// `has()` resolves '*' itself as its last fallthrough, so a wildcard holder is still allowed
    /// anything they have not been explicitly denied. Re-checking '*' here and OR-ing it in would
    /// resurrect exactly the nodes a '-node' deny entry just refused.
    pub fn has_valid_requirement_uuid(uuid: &str, perm_node: &str) -> bool {
        Self::manager().has(uuid, perm_node)
    }

    pub fn has_valid_requirement(peer: &NetPeerRef, perm_node: &str) -> bool {
        match NetworkServer::net_id_to_uuid(peer) {
            Some(uuid) => {
                if Self::manager().has(&uuid, perm_node) {
                    true
                } else {
                    BNL::log_error(format!("Permission not found for UUID: {uuid} for perm node {perm_node}"));
                    false
                }
            }
            None => {
                BNL::log_error(format!("UUID not found for peer: {} ", peer.id()));
                false
            }
        }
    }

    fn handle_permissions_changed(uuid: Option<&str>) {
        match uuid {
            Some(uuid) => Self::send_permission_update(uuid),
            None => {
                // Group-level change: resend to all connected players
                let uuids: Vec<String> = PLAYER_META.iter().map(|e| e.value().0.clone()).collect();
                for uuid in uuids {
                    Self::send_permission_update(&uuid);
                }
            }
        }
    }

    /// Rebuild and resend ServerMetaDataMessage to a connected player with their current
    /// permissions.
    pub fn send_permission_update(uuid: &str) {
        let Some(meta) = Self::try_get_player_meta(uuid) else {
            return;
        };
        let Some(net_id) = NetworkServer::uuid_to_net_id(uuid) else {
            return;
        };
        let Some(peer) = NetworkServer::authenticated_peers().get(&net_id).map(|p| p.value().clone()) else {
            return;
        };
        let config = NetworkServer::configuration_or_default();
        let mut msg = ServerMetaDataMessage {
            client_meta_data_message: meta,
            sync_interval: config.bsrs_millisecond_default_interval,
            base_multiplier: config.bsr_base_multiplier,
            increase_rate: config.bsrs_increase_rate,
            slowest_send_rate: config.bsr_slowest_send_rate,
            peer_limit: config.peer_limit,
            uplink_delta_enabled: config.enable_uplink_avatar_delta,
            image_share_egress_megabits_per_second: config.image_share_egress_megabits_per_second,
            image_pickup_range_meters: config.image_pickup_range_meters.max(0.0),
            ..Default::default()
        };
        let manager = Self::manager();
        msg.set_permissions(&manager.get_all_allowed_rules(uuid), Some(&manager.get_all_denied_rules(uuid)));

        let mut writer = NetworkServer::rent_writer();
        if msg.serialize(&mut writer).is_ok() {
            NetworkServer::try_send(&peer, &writer, BasisNetworkCommons::META_DATA_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
    }

    /// Drops the stored player metadata. Used when the server stops and by tests.
    pub fn reset_player_meta() {
        PLAYER_META.clear();
    }
}

/// Helper for callers that want a set of nodes as a plain `HashSet`.
pub fn to_hash_set(set: &CaseInsensitiveSet) -> HashSet<String> {
    set.iter().map(str::to_string).collect()
}
