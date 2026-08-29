use std::collections::HashMap;

use crate::json_web_key::JsonWebKey;
use crate::newtypes::DidUrlFragment;

/// Contains the info that we care about in the DID Document. A DID Document is what a DID is
/// resolved into. See https://www.w3.org/TR/did-core/#did-resolution
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DidDocument {
    pub pubkeys: HashMap<DidUrlFragment, JsonWebKey>,
}

impl DidDocument {
    pub fn new(pubkeys: HashMap<DidUrlFragment, JsonWebKey>) -> Self {
        Self { pubkeys }
    }
}
