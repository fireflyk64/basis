//! Port of `Contrib/Auth/Did.Tests/Base64UrlSafeTests.cs`.
use basis_did::Base64UrlSafe;

#[test]
fn test_encode() {
    let bytes = [0xDEu8, 0xAD, 0xBE, 0xEF];
    let base64 = "3q2-7w";
    assert!(
        Base64UrlSafe::encode(&bytes) == base64,
        "base64 encoding did not match expected value"
    );
}

#[test]
fn test_decode() {
    let bytes = [0xDEu8, 0xAD, 0xBE, 0xEF];
    let base64 = "3q2-7w";
    assert!(
        Base64UrlSafe::decode(base64).unwrap() == bytes,
        "base64 decoding was did not match expected value"
    );
}
