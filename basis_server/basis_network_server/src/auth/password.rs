//! Port of `Auth/Password.cs`.

use basis_network_core::BNL;

use super::IAuth;

/// Newtype on `String`. This represents the server's configured password.
struct ServerPassword(String);

/// Newtype on `String`. This represents the user's password.
struct UserPassword(String);

struct Deserialized {
    password: UserPassword,
}

impl Deserialized {
    fn new(bytes_msg: &[u8]) -> Self {
        Self { password: UserPassword(String::from_utf8_lossy(bytes_msg).into_owned()) }
    }
}

pub struct PasswordAuth {
    server_password: ServerPassword,
}

impl PasswordAuth {
    /// If `server_password` is an empty string, the server has no password and any user can connect.
    pub fn new(server_password: &str) -> Self {
        Self { server_password: ServerPassword(server_password.to_string()) }
    }

    fn check_password(server_password: &ServerPassword, user_password: &UserPassword) -> bool {
        if server_password.0.is_empty() {
            BNL::log_error("No server password set — the server is open to all users.");
            return true;
        }
        if user_password.0.is_empty() {
            BNL::log("User had an empty password, user is rejected");
            return false;
        }
        if fixed_time_equals(server_password.0.as_bytes(), user_password.0.as_bytes()) {
            true
        } else {
            BNL::log_error("Passwords do not match, user is rejected");
            false
        }
    }
}

impl IAuth for PasswordAuth {
    fn is_authenticated(&self, bytes_msg: &[u8]) -> bool {
        let deserialized = Deserialized::new(bytes_msg);
        Self::check_password(&self.server_password, &deserialized.password)
    }
}

/// `CryptographicOperations.FixedTimeEquals`: the comparison time depends only on the length.
pub fn fixed_time_equals(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut accumulator = 0u8;
    for (a, b) in left.iter().zip(right) {
        accumulator |= a ^ b;
    }
    accumulator == 0
}
