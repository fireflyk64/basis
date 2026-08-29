//! Wrapper types, to more safely differentiate them and help code document itself.

/// A DID. DIDs do *not* contain any fragment portion. See
/// https://www.w3.org/TR/did-core/#did-syntax
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct Did(pub String);

/// A full DID Url, which is a did along with an optional path query and fragment. See
/// https://www.w3.org/TR/did-core/#did-url-syntax
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct DidUrl(pub String);

/// A DID Url Fragment. Does not include the `#` part. Can be empty.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct DidUrlFragment(pub String);

/// A random nonce.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Nonce(pub Vec<u8>);

impl Did {
    pub fn new(v: impl Into<String>) -> Self {
        Self(v.into())
    }
    pub fn v(&self) -> &str {
        &self.0
    }
}

impl DidUrlFragment {
    pub fn new(v: impl Into<String>) -> Self {
        Self(v.into())
    }
    pub fn v(&self) -> &str {
        &self.0
    }
}

impl Nonce {
    pub fn v(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Display for Did {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
