use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::BNL;
use crate::configuration::{BasisTransportConfigStore, Configuration, IrohTransportConfig, LNLTransportConfig};
use crate::p2p::IPeerIntroducer;

use super::basis_network_shell::{EventBasedNetListener, NetManagerRef};
use super::connection_target::{ConnectionTarget, IConnectionTargetParser};
use super::iroh_connection_target_parser::IrohConnectionTargetParser;
use super::iroh_network_impl::IrohNetManager;
use super::lnl_connection_target_parser::LNLConnectionTargetParser;

#[derive(Clone, Debug, Default)]
pub struct ServerProbeResult {
    pub reachable: bool,
    pub error: String,
    pub timed_out: bool,
    pub online: u16,
    pub max: u16,
    pub protocol_version: u16,
    pub name: String,
    pub motd: String,
    pub round_trip_ms: i32,
    /// Case-insensitive keys in C#; stored lower-cased here.
    pub extras: HashMap<String, String>,
    /// The IP address that successfully responded to the probe. `None` when unreachable.
    pub resolved_address: Option<std::net::IpAddr>,
    /// iroh: the endpoint id the probe learned, so a plain `host:port` target can be dialled.
    pub endpoint_id: String,
}

pub type ProbeFuture = Pin<Box<dyn Future<Output = ServerProbeResult> + Send>>;
pub type StackProbe = Arc<dyn Fn(ConnectionTarget, i32) -> ProbeFuture + Send + Sync>;
pub type PeerIntroducerFactory = Arc<dyn Fn(Option<NetManagerRef>) -> Arc<dyn IPeerIntroducer> + Send + Sync>;
pub type NetManagerFactory = Arc<dyn Fn(Arc<EventBasedNetListener>, &Configuration) -> Option<NetManagerRef> + Send + Sync>;
pub type StackTick = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackInfo {
    pub id: String,
    pub display_name: String,
}

struct Slot {
    id: String,
    display_name: String,
    factory: NetManagerFactory,
    parser: Option<Arc<dyn IConnectionTargetParser>>,
    probe: Option<StackProbe>,
    tick: Option<StackTick>,
    introducer_factory: Option<PeerIntroducerFactory>,
}

struct RegistryState {
    slots: HashMap<String, Slot>,
    stacks: Vec<StackInfo>,
    active_stack_id: String,
    active_stack_changed: Vec<Arc<dyn Fn(&str) + Send + Sync>>,
}

static STATE: Mutex<Option<RegistryState>> = Mutex::new(None);

fn key(id: &str) -> String {
    id.to_ascii_lowercase()
}

/// Which transport a server or client runs on, by id. The C# static constructor registered
/// LiteNetLib; here [`BasisNetworkStackRegistry::ensure_initialized`] registers iroh (the
/// default) and the LiteNetLib id, whose manager factory is the slot the API-compatible
/// LiteNetLib-protocol transport will fill in.
pub struct BasisNetworkStackRegistry;

impl BasisNetworkStackRegistry {
    pub const LITE_NET_LIB_ID: &'static str = "litenetlib";
    pub const IROH_ID: &'static str = "iroh";
    pub const DEFAULT_ID: &'static str = Self::IROH_ID;

    fn with_state<R>(f: impl FnOnce(&mut RegistryState) -> R) -> R {
        let mut guard = STATE.lock();
        let fresh = guard.is_none();
        let state = guard.get_or_insert_with(|| RegistryState {
            slots: HashMap::new(),
            stacks: Vec::new(),
            active_stack_id: String::new(),
            active_stack_changed: Vec::new(),
        });
        if fresh {
            Self::register_builtins(state);
        }
        f(state)
    }

    fn register_builtins(state: &mut RegistryState) {
        let iroh: NetManagerFactory = Arc::new(|listener, config| IrohNetManager::create(listener, config));
        Self::register_into(state, Self::IROH_ID, "iroh", iroh);
        if let Some(slot) = state.slots.get_mut(&key(Self::IROH_ID)) {
            slot.parser = Some(Arc::new(IrohConnectionTargetParser));
        }
        let lnl: NetManagerFactory = Arc::new(|_, _| {
            BNL::log_error("The LiteNetLib-protocol transport is not part of this build yet; use the 'iroh' stack.");
            None
        });
        Self::register_into(state, Self::LITE_NET_LIB_ID, "LiteNetLib", lnl);
        if let Some(slot) = state.slots.get_mut(&key(Self::LITE_NET_LIB_ID)) {
            slot.parser = Some(Arc::new(LNLConnectionTargetParser));
        }
        BasisTransportConfigStore::register_type::<IrohTransportConfig>(Self::IROH_ID);
        BasisTransportConfigStore::register_type::<LNLTransportConfig>(Self::LITE_NET_LIB_ID);
    }

    /// Forces the built-in registrations (the C# `RuntimeHelpers.RunClassConstructor`).
    pub fn ensure_initialized() {
        Self::with_state(|_| {});
    }

    fn register_into(state: &mut RegistryState, id: &str, display_name: &str, factory: NetManagerFactory) {
        let k = key(id);
        if state.slots.contains_key(&k) {
            return;
        }
        let display_name = if display_name.is_empty() { id.to_string() } else { display_name.to_string() };
        state.stacks.push(StackInfo { id: id.to_string(), display_name: display_name.clone() });
        state.slots.insert(
            k,
            Slot { id: id.to_string(), display_name, factory, parser: None, probe: None, tick: None, introducer_factory: None },
        );
    }

    /// Registers a stack. A duplicate id is ignored. Panics on an empty id (the C# `ArgumentException`).
    pub fn register(id: &str, display_name: &str, factory: NetManagerFactory) {
        assert!(!id.is_empty(), "Stack id is required (Parameter 'id')");
        Self::with_state(|s| Self::register_into(s, id, display_name, factory));
    }

    pub fn register_parser(stack_id: &str, parser: Arc<dyn IConnectionTargetParser>) {
        assert!(!stack_id.is_empty(), "Stack id is required (Parameter 'stackId')");
        Self::with_state(|s| match s.slots.get_mut(&key(stack_id)) {
            Some(slot) => slot.parser = Some(parser),
            None => BNL::log_warning(format!("Cannot register parser for unknown stack '{stack_id}'")),
        });
    }

    pub fn register_probe(stack_id: &str, probe: StackProbe) {
        assert!(!stack_id.is_empty(), "Stack id is required (Parameter 'stackId')");
        Self::with_state(|s| match s.slots.get_mut(&key(stack_id)) {
            Some(slot) => slot.probe = Some(probe),
            None => BNL::log_warning(format!("Cannot register probe for unknown stack '{stack_id}'")),
        });
    }

    pub fn register_tick(stack_id: &str, tick: StackTick) {
        assert!(!stack_id.is_empty(), "Stack id is required (Parameter 'stackId')");
        Self::with_state(|s| match s.slots.get_mut(&key(stack_id)) {
            Some(slot) => slot.tick = Some(tick),
            None => BNL::log_warning(format!("Cannot register tick for unknown stack '{stack_id}'")),
        });
    }

    pub fn register_introducer_factory(stack_id: &str, factory: PeerIntroducerFactory) {
        assert!(!stack_id.is_empty(), "Stack id is required (Parameter 'stackId')");
        Self::with_state(|s| match s.slots.get_mut(&key(stack_id)) {
            Some(slot) => slot.introducer_factory = Some(factory),
            None => BNL::log_warning(format!("Cannot register peer introducer for unknown stack '{stack_id}'")),
        });
    }

    /// Builds the manager for `id` (empty = default; unknown falls back to the default with a
    /// warning) and marks that stack active.
    pub fn create(id: &str, listener: Arc<EventBasedNetListener>, configuration: &Configuration) -> Option<NetManagerRef> {
        let mut effective = if id.is_empty() { Self::DEFAULT_ID.to_string() } else { id.to_string() };
        let factory = Self::with_state(|s| {
            match s.slots.get(&key(&effective)) {
                Some(slot) => slot.factory.clone(),
                None => {
                    BNL::log_warning(format!("Network stack '{effective}' is not registered, falling back to '{}'", Self::DEFAULT_ID));
                    effective = Self::DEFAULT_ID.to_string();
                    s.slots[&key(Self::DEFAULT_ID)].factory.clone()
                }
            }
        });
        let mgr = factory(listener, configuration);
        Self::set_active_stack_id(&effective);
        mgr
    }

    pub fn active_stack_id() -> String {
        Self::with_state(|s| s.active_stack_id.clone())
    }

    pub fn set_active_stack_id(id: &str) {
        let normalized = id.to_string();
        let (changed, handlers) = Self::with_state(|s| {
            if !s.active_stack_id.eq_ignore_ascii_case(&normalized) {
                s.active_stack_id = normalized.clone();
                (true, s.active_stack_changed.clone())
            } else {
                (false, Vec::new())
            }
        });
        if changed {
            for h in handlers {
                h(&normalized);
            }
        }
    }

    /// The C# `ActiveStackChanged` event.
    pub fn subscribe_active_stack_changed(handler: Arc<dyn Fn(&str) + Send + Sync>) {
        Self::with_state(|s| s.active_stack_changed.push(handler));
    }

    pub fn unsubscribe_active_stack_changed(handler: &Arc<dyn Fn(&str) + Send + Sync>) {
        Self::with_state(|s| s.active_stack_changed.retain(|h| !Arc::ptr_eq(h, handler)));
    }

    pub fn get_parser(stack_id: &str) -> Option<Arc<dyn IConnectionTargetParser>> {
        let effective = if stack_id.is_empty() { Self::DEFAULT_ID } else { stack_id };
        Self::with_state(|s| {
            if let Some(slot) = s.slots.get(&key(effective))
                && let Some(p) = &slot.parser
            {
                return Some(p.clone());
            }
            if !effective.eq_ignore_ascii_case(Self::DEFAULT_ID) {
                BNL::log_warning(format!("No connection-target parser registered for stack '{effective}', falling back to '{}'", Self::DEFAULT_ID));
                if let Some(fallback) = s.slots.get(&key(Self::DEFAULT_ID)) {
                    return fallback.parser.clone();
                }
            }
            None
        })
    }

    pub async fn probe_async(target: Option<ConnectionTarget>, timeout_ms: i32) -> ServerProbeResult {
        let Some(target) = target else {
            return ServerProbeResult { error: "Target is null".into(), ..Default::default() };
        };
        let stack_id = if target.stack_id.is_empty() { Self::DEFAULT_ID.to_string() } else { target.stack_id.clone() };
        let probe = Self::with_state(|s| {
            match s.slots.get(&key(&stack_id)).and_then(|slot| slot.probe.clone()) {
                Some(p) => Ok(p),
                None => {
                    if !stack_id.eq_ignore_ascii_case(Self::DEFAULT_ID) {
                        BNL::log_warning(format!("No probe registered for stack '{stack_id}', falling back to '{}'", Self::DEFAULT_ID));
                    }
                    match s.slots.get(&key(Self::DEFAULT_ID)).and_then(|f| f.probe.clone()) {
                        Some(p) => Ok(p),
                        None => Err(format!("No probe registered for stack '{stack_id}' (no fallback available)")),
                    }
                }
            }
        });
        match probe {
            Ok(p) => p(target, timeout_ms).await,
            Err(error) => ServerProbeResult { error, ..Default::default() },
        }
    }

    pub fn tick_active() {
        let tick = Self::with_state(|s| {
            if s.active_stack_id.is_empty() {
                return None;
            }
            s.slots.get(&key(&s.active_stack_id)).and_then(|slot| slot.tick.clone())
        });
        if let Some(tick) = tick {
            // The C# swallowed a throwing tick; a panicking tick is caught the same way.
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tick())) {
                let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                BNL::log_error(format!("Stack tick threw: {msg}"));
            }
        }
    }

    pub fn create_introducer(stack_id: &str, active_manager: Option<NetManagerRef>) -> Option<Arc<dyn IPeerIntroducer>> {
        let effective = if stack_id.is_empty() { Self::DEFAULT_ID.to_string() } else { stack_id.to_string() };
        let factory = Self::with_state(|s| {
            match s.slots.get(&key(&effective)).and_then(|slot| slot.introducer_factory.clone()) {
                Some(f) => Some(f),
                None => {
                    if !effective.eq_ignore_ascii_case(Self::DEFAULT_ID) {
                        BNL::log_warning(format!("No peer introducer registered for stack '{effective}', falling back to '{}'", Self::DEFAULT_ID));
                    }
                    s.slots.get(&key(Self::DEFAULT_ID)).and_then(|f| f.introducer_factory.clone())
                }
            }
        })?;
        Some(factory(active_manager))
    }

    pub fn is_registered(id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        Self::with_state(|s| s.slots.contains_key(&key(id)))
    }

    pub fn stacks() -> Vec<StackInfo> {
        Self::with_state(|s| s.stacks.clone())
    }

    pub fn get_display_name(id: &str) -> String {
        let id = if id.is_empty() { Self::DEFAULT_ID } else { id };
        Self::with_state(|s| s.slots.get(&key(id)).map(|slot| slot.display_name.clone()).unwrap_or_else(|| id.to_string()))
    }

    /// The registered id in its original casing.
    pub fn canonical_id(id: &str) -> Option<String> {
        Self::with_state(|s| s.slots.get(&key(id)).map(|slot| slot.id.clone()))
    }

    /// Test seam: drops every registration so the next call re-registers the built-ins.
    pub fn reset_for_tests() {
        *STATE.lock() = None;
    }
}
