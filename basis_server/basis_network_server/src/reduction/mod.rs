//! Port of `BasisNetworkServer/Reduction`: the Basis Server Reduction system (BSR) — the
//! distance-tiered avatar fan-out, its load controller, bundling and profiling.
pub mod avatar_quality_repacker;
pub mod basis_compute_backend;
pub mod basis_server_reduction_system_events;
pub mod peer_tracking_data;
pub mod pending_avatar_send;
pub mod player_state;
pub mod profiling;
pub mod quant_rescale_table;
pub mod queued_message;
pub mod sharded_concurrent_dictionary;

pub use avatar_quality_repacker::AvatarQualityRepacker;
pub use basis_compute_backend::BasisComputeBackend;
pub use basis_server_reduction_system_events::{BasisServerReductionSystemEvents, DistanceSweepState, PoolTuning, now_ticks};
pub use peer_tracking_data::PeerTrackingData;
pub use pending_avatar_send::PendingAvatarSend;
pub use player_state::{PlayerState, ReceiverData, SenderFrame, SenderWork};
pub use profiling::{BSRProfiler, BSRProfilerSnapshot, BSRThreadCounters};
pub use quant_rescale_table::QuantRescaleTable;
pub use queued_message::{QueuedMessage, QueuedMessagePool};
pub use sharded_concurrent_dictionary::ShardedConcurrentDictionary;
