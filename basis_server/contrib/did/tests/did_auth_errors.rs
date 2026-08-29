//! Negative tests for DID challenge/response authentication: every way a response can be
//! wrong is a distinct, typed verification error, and none of them panics.

use basis_crypto::{Ed25519, Payload, PrivKey, PubKey, Signature};
use basis_did::did_auth::{DidResolveErr, DidSignatureErr, IDidVerifyErr};
use basis_did::{Config, Did, DidAuthentication, DidKeyDecodeError, DidKeyResolver, DidUrlFragment, Response};

fn keypair(seed: u8) -> (PrivKey, PubKey) {
    let private = PrivKey(vec![seed; 32]);
    let public = Ed25519::convert_privkey_to_pubkey(&private).unwrap();
    (private, public)
}

fn respond(private: &PrivKey, payload: &[u8]) -> Response {
    Response {
        signature: Ed25519::sign(private, &Payload(payload.to_vec())).unwrap(),
        did_url_fragment: DidUrlFragment(String::new()),
    }
}

#[test]
fn the_honest_flow_verifies() {
    let auth = DidAuthentication::new(Config::default());
    let (private, public) = keypair(1);
    let did = DidKeyResolver::encode_pubkey_as_did(&public);
    let challenge = auth.make_challenge(did);
    assert_eq!(challenge.nonce.0.len(), 32);
    let response = respond(&private, &challenge.nonce.0);
    assert_eq!(auth.verify_response(&response, &challenge), Ok(()));

    // Two challenges never share a nonce.
    let again = auth.make_challenge(challenge.identity.clone());
    assert_ne!(again.nonce, challenge.nonce);
}

#[test]
fn a_did_with_the_wrong_prefix_or_method_cannot_be_resolved() {
    let auth = DidAuthentication::new(Config::default());
    let (private, _) = keypair(2);
    for (did, expected) in [
        ("foo:key:z6Mk", DidResolveErr::InvalidPrefix),
        ("did", DidResolveErr::InvalidPrefix),
        ("did:key", DidResolveErr::InvalidPrefix),
        ("did:web:example.com", DidResolveErr::UnsupportedMethod),
        ("did:ion:abc", DidResolveErr::UnsupportedMethod),
    ] {
        let challenge = auth.make_challenge(Did(did.to_string()));
        let response = respond(&private, &challenge.nonce.0);
        assert_eq!(auth.verify_response(&response, &challenge), Err(IDidVerifyErr::Resolve(expected)), "{did}");
    }
}

#[test]
fn a_garbled_did_key_is_a_resolve_error_not_a_panic() {
    let auth = DidAuthentication::new(Config::default());
    let (private, _) = keypair(3);
    for did in ["did:key:", "did:key:!!!not base58!!!", "did:key:z", "did:key:zzzzzzzzzzzzzzzzzzzzzzzzzzzz", "did:key:z6Mk"] {
        let challenge = auth.make_challenge(Did(did.to_string()));
        let response = respond(&private, &challenge.nonce.0);
        assert!(
            matches!(auth.verify_response(&response, &challenge), Err(IDidVerifyErr::Resolve(_))),
            "{did} should fail to resolve"
        );
    }
}

#[test]
fn the_resolver_names_what_is_wrong_with_a_did_key() {
    let (_, public) = keypair(4);
    let good = DidKeyResolver::encode_pubkey_as_did(&public);
    assert!(DidKeyResolver::resolve(&good).is_ok());

    // Multibase prefix other than base58btc 'z'.
    let not_b58 = Did(good.0.replacen("did:key:z", "did:key:m", 1));
    assert_eq!(DidKeyResolver::resolve(&not_b58).unwrap_err().error, DidKeyDecodeError::NotBase58Btc);

    // Valid base58 that is too short to be an Ed25519 key.
    let short = Did(format!("did:key:z{}", bs58::encode([0xed, 0x01, 1, 2, 3]).into_string()));
    assert_eq!(DidKeyResolver::resolve(&short).unwrap_err().error, DidKeyDecodeError::WrongPubkeyLen);

    // A multicodec this build does not know.
    let unknown = Did(format!("did:key:z{}", bs58::encode([0xe7, 0x01, 1, 2, 3]).into_string()));
    assert_eq!(DidKeyResolver::resolve(&unknown).unwrap_err().error, DidKeyDecodeError::UnsupportedPubkeyType);

    // Non-base58 characters.
    assert_eq!(DidKeyResolver::resolve(&Did("did:key:z0OIl".into())).unwrap_err().error, DidKeyDecodeError::NotBase58Btc);
}

#[test]
fn a_signature_over_the_wrong_nonce_or_by_the_wrong_key_is_rejected() {
    let auth = DidAuthentication::new(Config::default());
    let (private, public) = keypair(5);
    let (other_private, _) = keypair(6);
    let did = DidKeyResolver::encode_pubkey_as_did(&public);
    let challenge = auth.make_challenge(did);

    let wrong_nonce = respond(&private, b"some other nonce");
    assert_eq!(
        auth.verify_response(&wrong_nonce, &challenge),
        Err(IDidVerifyErr::Signature(DidSignatureErr::InvalidSignature))
    );

    let wrong_key = respond(&other_private, &challenge.nonce.0);
    assert_eq!(
        auth.verify_response(&wrong_key, &challenge),
        Err(IDidVerifyErr::Signature(DidSignatureErr::InvalidSignature))
    );

    // A replay against a fresh challenge fails too.
    let good = respond(&private, &challenge.nonce.0);
    let fresh = auth.make_challenge(challenge.identity.clone());
    assert_eq!(auth.verify_response(&good, &fresh), Err(IDidVerifyErr::Signature(DidSignatureErr::InvalidSignature)));
}

#[test]
fn malformed_signatures_are_rejected_without_panicking() {
    let auth = DidAuthentication::new(Config::default());
    let (private, public) = keypair(7);
    let did = DidKeyResolver::encode_pubkey_as_did(&public);
    let challenge = auth.make_challenge(did);
    let mut good = respond(&private, &challenge.nonce.0);
    for bad in [Vec::new(), vec![0u8; 63], vec![0u8; 65], vec![0xFF; 64]] {
        let response = Response { signature: Signature(bad), did_url_fragment: DidUrlFragment(String::new()) };
        assert_eq!(
            auth.verify_response(&response, &challenge),
            Err(IDidVerifyErr::Signature(DidSignatureErr::InvalidSignature))
        );
    }
    good.signature.0[0] ^= 1;
    assert_eq!(auth.verify_response(&good, &challenge), Err(IDidVerifyErr::Signature(DidSignatureErr::InvalidSignature)));
}

#[test]
fn a_single_key_document_ignores_the_fragment() {
    let auth = DidAuthentication::new(Config::default());
    let (private, public) = keypair(8);
    let did = DidKeyResolver::encode_pubkey_as_did(&public);
    let challenge = auth.make_challenge(did);
    let response = Response {
        signature: Ed25519::sign(&private, &Payload(challenge.nonce.0.clone())).unwrap(),
        did_url_fragment: DidUrlFragment("no-such-key".into()),
    };
    assert_eq!(auth.verify_response(&response, &challenge), Ok(()));
}
