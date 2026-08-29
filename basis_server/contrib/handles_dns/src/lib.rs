//! Port of `Contrib/Handles/Dns`: a handle that is a DNS name, verified through a
//! `_nexus-handles.<name>` TXT record of the form `did=<identity>`.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unimplemented,
        clippy::todo,
        clippy::unreachable
    )
)]
#![deny(unused_must_use)]

use basis_handles_common::{
    BoxFuture, HandleKind, HandleMutability, HandleProperties, IHandle, IHandleVerifier, Identity,
};
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{CLOUDFLARE, ResolverConfig};
use hickory_resolver::net::NetError;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RData;

pub struct DnsHandleResolver {
    client: TokioResolver,
}

impl DnsHandleResolver {
    const TXT_RECORD_PREFIX: &'static str = "_nexus-handles";

    pub fn new(client: TokioResolver) -> Self {
        Self { client }
    }

    /// A resolver that asks Cloudflare, the counterpart of `NameServer.Cloudflare` in the C# tests.
    pub fn cloudflare_client() -> Result<TokioResolver, DnsHandleError> {
        TokioResolver::builder_with_config(ResolverConfig::udp_and_tcp(&CLOUDFLARE), TokioRuntimeProvider::default())
            .build()
            .map_err(|e| DnsHandleError::lookup("<resolver>".to_string(), e))
    }

    pub async fn handle_points_to_identity_async(
        &self,
        handle: &dyn IHandle,
        identity: &Identity,
    ) -> Result<bool, DnsHandleError> {
        let name = format!("{}.{}", Self::TXT_RECORD_PREFIX, handle.display_name());
        let lookup = match self.client.txt_lookup(name.as_str()).await {
            Ok(lookup) => lookup,
            // A name with no such record does not point at anyone; that is an answer, not a fault.
            Err(e) if e.is_no_records_found() || e.is_nx_domain() => return Ok(false),
            Err(e) => return Err(DnsHandleError::lookup(name, e)),
        };

        let Some(first) = lookup.answers().iter().find_map(|r| match &r.data {
            RData::TXT(txt) => Some(txt.clone()),
            _ => None,
        }) else {
            return Ok(false);
        };

        for attr in first.txt_data.iter() {
            let attr = String::from_utf8_lossy(attr);
            let mut parts = attr.splitn(2, '=');
            let prefix = parts.next().unwrap_or("");
            let suffix = parts.next().unwrap_or("");
            if prefix != "did" {
                return Err(DnsHandleError::Format);
            }
            if suffix == identity.v() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl IHandleVerifier for DnsHandleResolver {
    fn handle_points_to_identity<'a>(
        &'a self,
        handle: &'a dyn IHandle,
        identity: &'a Identity,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.handle_points_to_identity_async(handle, identity)
                .await
                .unwrap_or(false)
        })
    }

    fn kind(&self) -> HandleKind {
        DnsHandle::KIND
    }

    fn properties(&self) -> HandleProperties {
        DnsHandle::PROPERTIES
    }
}

/// Why a handle could not be verified over DNS.
#[derive(Debug, thiserror::Error)]
pub enum DnsHandleError {
    /// The lookup itself failed. `transient` is true for the faults a retry can clear — a
    /// timeout, a busy resolver, no usable connection, an I/O error — and false for a name
    /// that cannot even be parsed or a resolver that could not be built.
    #[error("dns lookup of '{name}' failed: {source}")]
    Lookup {
        name: String,
        transient: bool,
        #[source]
        source: NetError,
    },
    #[error("dns txt record did not match expected format 2")]
    Format,
}

impl DnsHandleError {
    fn lookup(name: String, source: NetError) -> Self {
        let transient = matches!(source, NetError::Timeout | NetError::Busy | NetError::NoConnections | NetError::Io(_));
        DnsHandleError::Lookup { name, transient, source }
    }

    /// Whether retrying the same verification later can succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, DnsHandleError::Lookup { transient: true, .. })
    }
}

/// Essentially just a string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct DnsHandle {
    pub display_name: String,
}

impl DnsHandle {
    pub const PROPERTIES: HandleProperties = HandleProperties {
        kind: HandleKind::Dns,
        mutability: HandleMutability::Mutable,
        is_globally_unique: true,
    };
    pub const KIND: HandleKind = HandleKind::Dns;
}

impl IHandle for DnsHandle {
    fn kind(&self) -> HandleKind {
        Self::KIND
    }
    fn properties(&self) -> HandleProperties {
        Self::PROPERTIES
    }
    fn display_name(&self) -> &str {
        &self.display_name
    }
}
