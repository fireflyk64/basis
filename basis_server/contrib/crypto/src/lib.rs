//! Port of `Contrib/Crypto`: the asymmetric and symmetric primitives Basis relies on.
//!
//! BouncyCastle is replaced by the RustCrypto/dalek crates. The wrapper types keep their C#
//! names (`Payload`, `Signature`, `PubKey`, `PrivKey`, `SharedSecretKey`) so code that passed a
//! `PubKey` around in C# passes a `PubKey` around here.

pub mod basis_aead_cipher;
pub mod basis_hkdf;
pub mod basis_x25519;
pub mod ed25519;

pub use basis_aead_cipher::BasisAeadCipher;
pub use basis_hkdf::BasisHkdf;
pub use basis_x25519::BasisX25519;
pub use ed25519::Ed25519;

macro_rules! byte_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
        pub struct $name(pub Vec<u8>);

        impl $name {
            pub fn new(v: impl Into<Vec<u8>>) -> Self {
                Self(v.into())
            }

            /// The wrapped bytes. Named after the C# record's single property.
            pub fn v(&self) -> &[u8] {
                &self.0
            }
        }

        impl AsRef<[u8]> for $name {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }

        impl From<Vec<u8>> for $name {
            fn from(v: Vec<u8>) -> Self {
                Self(v)
            }
        }

        impl From<&[u8]> for $name {
            fn from(v: &[u8]) -> Self {
                Self(v.to_vec())
            }
        }
    };
}

byte_newtype!(Payload);
byte_newtype!(Signature);
byte_newtype!(
    /// Public asymmetric key.
    PubKey
);
byte_newtype!(
    /// Private (secret) asymmetric key.
    PrivKey
);
byte_newtype!(
    /// Private (secret) symmetric key.
    SharedSecretKey
);

/// The full set of signing algorithms we support.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SigningAlgorithm {
    Ed25519,
}
