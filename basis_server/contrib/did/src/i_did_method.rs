use crate::did_auth::DidResolveErr;
use crate::did_document::DidDocument;
use crate::newtypes::Did;

/// The functionality that all DID methods implement.
pub trait IDidMethod: Send + Sync {
    /// Resolves to a map of DID Url fragments to a Json Web Key. This method resolves a DID
    /// to its DID Document, and inspects the `verificationMethods` field to extract a
    /// dictionary of public keys.
    ///
    /// Even though json is not what all DID methods will use to represent keys, we standardize
    /// the api to return JsonWebKey because it documents its own key algorithms and is a
    /// reasonably portable format.
    ///
    /// Synchronous: `did:key` needs no I/O. A method that does (did:web) will resolve on a
    /// blocking helper thread behind this signature rather than making every caller async.
    fn resolve_document(&self, did: &Did) -> Result<DidDocument, DidResolveErr>;

    fn kind(&self) -> DidMethodKind;
}

/// The different supported DidMethods.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DidMethodKind {
    Key,
    // TODO: Web
}
