//! Port of `BasisNetworkServer/Auth`.
pub mod interface;
pub mod password;

pub use interface::{IAuth, IAuthIdentity, IAuthIdentitySupport};
pub use password::PasswordAuth;
