//! Port of `BasisDIDAuthIdentityProvider.cs`.

use basis_network_core::identity::{BasisPlayerIdentityRegistry, IPlayerIdentityProvider, PlayerIdentity};

use crate::BasisDIDAuthIdentityClient;

pub struct BasisDIDAuthIdentityProvider;

impl BasisDIDAuthIdentityProvider {
    pub const ID: &'static str = "did";

    /// The Unity build registered itself before the first scene loaded; a headless client calls
    /// this once at startup.
    pub fn auto_register() {
        BasisPlayerIdentityRegistry::register(std::sync::Arc::new(BasisDIDAuthIdentityProvider));
        BasisPlayerIdentityRegistry::set_active_provider_id(Self::ID);
    }
}

impl IPlayerIdentityProvider for BasisDIDAuthIdentityProvider {
    fn provider_id(&self) -> &str {
        Self::ID
    }

    fn get_or_create(&self) -> PlayerIdentity {
        PlayerIdentity { uuid: BasisDIDAuthIdentityClient::get_or_save_did(), provider: Self::ID.to_string(), properties: Default::default() }
    }
}
