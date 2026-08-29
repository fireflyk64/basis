//! Negative tests that need no network: a name that cannot be parsed is a permanent lookup
//! fault, a name server that never answers is a transient one, and neither panics.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use basis_handles_common::{IHandleVerifier, Identity};
use basis_handles_dns::{DnsHandle, DnsHandleError, DnsHandleResolver};
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;

/// A resolver whose only name server is in TEST-NET-1 (RFC 5737): packets to it go nowhere,
/// so every query times out quickly.
fn black_hole_resolver() -> TokioResolver {
    let config = ResolverConfig::from_parts(
        None,
        Vec::new(),
        vec![NameServerConfig::udp_and_tcp(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)))],
    );
    let mut opts = ResolverOpts::default();
    opts.timeout = Duration::from_millis(250);
    opts.attempts = 1;
    TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
        .with_options(opts)
        .build()
        .expect("resolver")
}

#[tokio::test]
async fn an_unparsable_name_is_a_permanent_lookup_fault() {
    let verifier = DnsHandleResolver::new(black_hole_resolver());
    let handle = DnsHandle { display_name: "not a valid\u{0}name..".into() };
    let identity = Identity::new("did:web:example.com");
    let err = verifier.handle_points_to_identity_async(&handle, &identity).await.unwrap_err();
    match &err {
        DnsHandleError::Lookup { name, transient, .. } => {
            assert!(name.starts_with("_nexus-handles."), "{name}");
            assert!(!transient, "{err}");
        }
        other => panic!("expected a lookup error, got {other}"),
    }
    assert!(!err.is_transient());
    // The bool-returning trait form answers false rather than failing.
    assert!(!verifier.handle_points_to_identity(&handle, &identity).await);
}

#[tokio::test]
async fn a_name_server_that_never_answers_is_a_transient_fault() {
    let verifier = DnsHandleResolver::new(black_hole_resolver());
    let handle = DnsHandle { display_name: "black-hole-probe.basis-port-test.net".into() };
    let identity = Identity::new("did:web:black-hole-probe.basis-port-test.net");
    let err = verifier.handle_points_to_identity_async(&handle, &identity).await.unwrap_err();
    assert!(err.is_transient(), "{err}");
    assert!(matches!(err, DnsHandleError::Lookup { transient: true, .. }));
    assert!(std::error::Error::source(&err).is_some());
}
