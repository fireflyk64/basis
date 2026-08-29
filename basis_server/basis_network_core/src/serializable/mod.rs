//! Port of `BasisNetworkCore/Serializable`: every message struct, flat under one module the way
//! they were all nested in the C# `SerializableBasis` partial class. `use
//! basis_network_core::SerializableBasis::*` brings the same names into scope.
//!
//! Every `serialize` takes `&mut self` because several C# messages write derived fields back
//! (recipient counts, payload sizes) as a side effect; every `deserialize` returns a
//! [`NetResult`](crate::NetResult) where the C# threw, and logs-and-continues where the C# did.

pub mod audio;
pub mod avatar;
pub mod camera;
pub mod chat;
pub mod identity;
pub mod permissions;
pub mod protocol;
pub mod resources;
pub mod scene;

pub use audio::*;
pub use avatar::*;
pub use camera::*;
pub use chat::*;
pub use identity::*;
pub use permissions::*;
pub use protocol::*;
pub use resources::*;
pub use scene::*;
