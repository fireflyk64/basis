//! Port of `Contrib/Handles/Dns.Tests/DnsTests.cs`. Needs outbound DNS, exactly like the original.
use std::collections::HashMap;

use basis_handles_common::{Config, HandleKind, HandleVerifier, IHandleVerifier, Identity};
use basis_handles_dns::{DnsHandle, DnsHandleResolver};

#[tokio::test]
async fn known_example() {
    let dns_verifier = DnsHandleResolver::new(DnsHandleResolver::cloudflare_client().expect("resolver"));
    let handle = DnsHandle { display_name: "example.socialvr.net".into() };
    let identity = Identity::new("did:web:example.socialvr.net");
    assert!(
        dns_verifier.handle_points_to_identity(&handle, &identity).await,
        "should match the known, did:web that have been set in socialvr.net's DNS record"
    );

    let mut verifiers: HashMap<HandleKind, Box<dyn IHandleVerifier>> = HashMap::new();
    verifiers.insert(HandleKind::Dns, Box::new(dns_verifier));
    let verifier = HandleVerifier::new(Config { verifiers });
    assert!(
        verifier.handle_points_to_identity(&handle, &identity).await,
        "should match the known, did:web that have been set in socialvr.net's DNS record"
    );
}
