use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

#[derive(Clone, Debug, Default)]
pub struct PlayerIdentity {
    pub uuid: String,
    pub provider: String,
    /// Case-insensitive keys, like the C# `StringComparer.OrdinalIgnoreCase` dictionary.
    pub properties: HashMap<String, String>,
}

pub trait IPlayerIdentityProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn get_or_create(&self) -> PlayerIdentity;
}

struct RegistryState {
    providers: HashMap<String, Arc<dyn IPlayerIdentityProvider>>,
    active_provider_id: String,
}

static STATE: Mutex<Option<RegistryState>> = Mutex::new(None);

fn with_state<T>(f: impl FnOnce(&mut RegistryState) -> T) -> T {
    let mut guard = STATE.lock();
    let state = guard.get_or_insert_with(|| RegistryState {
        providers: HashMap::new(),
        active_provider_id: BasisPlayerIdentityRegistry::DEFAULT_PROVIDER_ID.to_string(),
    });
    f(state)
}

pub struct BasisPlayerIdentityRegistry;

impl BasisPlayerIdentityRegistry {
    pub const DEFAULT_PROVIDER_ID: &'static str = "did";

    pub fn register(provider: Arc<dyn IPlayerIdentityProvider>) {
        assert!(!provider.provider_id().is_empty(), "ProviderId is required");
        with_state(|s| {
            s.providers.insert(provider.provider_id().to_lowercase(), provider);
        });
    }

    pub fn active_provider_id() -> String {
        with_state(|s| s.active_provider_id.clone())
    }

    pub fn set_active_provider_id(value: &str) {
        with_state(|s| {
            s.active_provider_id = if value.is_empty() {
                Self::DEFAULT_PROVIDER_ID.to_string()
            } else {
                value.to_string()
            }
        });
    }

    pub fn resolve_active() -> Option<PlayerIdentity> {
        let provider = with_state(|s| {
            s.providers
                .get(&s.active_provider_id.to_lowercase())
                .or_else(|| s.providers.get(Self::DEFAULT_PROVIDER_ID))
                .cloned()
        });
        provider.map(|p| p.get_or_create())
    }

    pub fn resolve(provider_id: &str) -> Option<PlayerIdentity> {
        let provider = with_state(|s| {
            let id = if provider_id.is_empty() { s.active_provider_id.clone() } else { provider_id.to_string() };
            s.providers.get(&id.to_lowercase()).cloned()
        });
        provider.map(|p| p.get_or_create())
    }

    pub fn is_registered(provider_id: &str) -> bool {
        if provider_id.is_empty() {
            return false;
        }
        with_state(|s| s.providers.contains_key(&provider_id.to_lowercase()))
    }
}
