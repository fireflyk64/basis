//! Port of `Contrib/Auth/Did.Tests/DidKeyTests.cs`.
use basis_crypto::PubKey;
use basis_did::{Base64UrlSafe, Did, DidKeyResolver, DidUrlFragment, IDidMethod, JsonWebKey};

// See https://w3c-ccg.github.io/did-method-key/#ed25519-x25519
fn test_vectors() -> Vec<(&'static str, JsonWebKey)> {
    vec![(
        "did:key:z6MkiTBz1ymuepAQ4HEHYSF1H8quG5GLVVQR3djdX3mDooWp",
        JsonWebKey {
            kty: Some("OKP".into()),
            crv: Some("Ed25519".into()),
            x: Some("O2onvM62pC1io6jQKm8Nc2UyFXcd4kOmOsBIoYtZ2ik".into()),
            ..Default::default()
        },
    )]
}

#[test]
fn did_key_test_vectors() {
    let resolver = DidKeyResolver;
    for (input_did, expected_jwk) in test_vectors() {
        let expected_fragment =
            DidUrlFragment(input_did.strip_prefix(DidKeyResolver::PREFIX).unwrap().to_string());
        let document = resolver.resolve_document(&Did::new(input_did)).unwrap();
        assert!(document.pubkeys.len() == 1);
        let resolved_jwk = &document.pubkeys[&expected_fragment];
        assert!(
            resolved_jwk.serialize() == expected_jwk.serialize(),
            "resolved JWK did not match expected JWK"
        );
    }
}

#[test]
fn did_key_test_encode() {
    for (expected_did, jwk_input) in test_vectors() {
        let pubkey_bytes = Base64UrlSafe::decode(jwk_input.x.as_deref().expect("the examples are not null")).unwrap();
        let encoded_did = DidKeyResolver::encode_pubkey_as_did(&PubKey(pubkey_bytes));
        assert!(
            expected_did == encoded_did.0,
            "encoded was {encoded_did}, expected {expected_did}"
        );
    }
}
