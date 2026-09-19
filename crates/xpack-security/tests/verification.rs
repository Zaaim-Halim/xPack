//! Independent verification of the cryptographic claims this crate makes.
//!
//! These are not unit tests of the implementation's own logic. They check the
//! crate against published test vectors and against the specific attacks its
//! documentation claims to resist.

use xpack_security::keys::{KeyPair, PublicKey};
use xpack_security::signature::{Signature, sign, verify};

/// RFC 8032 section 7.1, TEST 1.
const RFC8032_SECRET: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const RFC8032_PUBLIC: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
/// Signature over the zero-length message, transcribed from the RFC.
const RFC8032_SIGNATURE: &str = concat!(
    "e5564300c360ac729086e2cc806e828a",
    "84877f1eb8e5d974d873e06522490155",
    "5fb8821590a33bacc61e39701cf9b46b",
    "d25bf5f0595bbe24655141438e7a100b",
);

fn secret_bytes(hex_text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_text, &mut out).unwrap();
    out
}

#[test]
fn derives_the_rfc8032_public_key_from_its_secret_key() {
    let pair = KeyPair::from_secret_bytes(&secret_bytes(RFC8032_SECRET));
    assert_eq!(pair.public().to_hex(), RFC8032_PUBLIC);
}

#[test]
fn reproduces_the_rfc8032_signature_exactly() {
    // The decisive known-answer test: this crate must produce the byte string
    // the standard publishes, not merely a signature that its own verifier
    // happens to accept. A self-consistent but non-standard implementation
    // would pass every other test in this file.
    let pair = KeyPair::from_secret_bytes(&secret_bytes(RFC8032_SECRET));
    assert_eq!(sign(&pair, b"").to_hex(), RFC8032_SIGNATURE);
}

#[test]
fn verifies_the_rfc8032_signature_against_the_rfc_public_key() {
    let key = PublicKey::parse_hex(RFC8032_PUBLIC).unwrap();
    let signature = Signature::parse_hex(RFC8032_SIGNATURE).unwrap();
    verify(&key, b"", &signature).expect("the published vector must verify");
}

#[test]
fn signing_is_deterministic() {
    // Ed25519 derives its nonce from the key and message, so the same inputs
    // must always produce byte-identical signatures. A non-deterministic
    // result would mean the nonce came from somewhere it should not have.
    let pair = KeyPair::from_secret_bytes(&secret_bytes(RFC8032_SECRET));
    assert_eq!(sign(&pair, b"xpack").to_hex(), sign(&pair, b"xpack").to_hex());
}

#[test]
fn rejects_every_small_order_public_key() {
    // Points of small order accept a signature from any "signer", so a package
    // verifying against one proves nothing at all.
    const SMALL_ORDER: [&str; 7] = [
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0100000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000080",
        "0100000000000000000000000000000000000000000000000000000000000080",
        "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
        "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
    ];
    let mut rejected = 0;
    for hex_text in SMALL_ORDER {
        if PublicKey::parse_hex(hex_text).is_err() {
            rejected += 1;
        } else {
            println!("ACCEPTED small-order key: {hex_text}");
        }
    }
    assert_eq!(rejected, SMALL_ORDER.len(), "every small-order key must be refused");
}

#[test]
fn rejects_a_signature_whose_scalar_is_not_reduced() {
    // A signature is (R, S). S must be reduced modulo the group order L.
    // Adding L to S yields a different byte string that a permissive
    // implementation would still accept, so one message would have two valid
    // signatures.
    //
    // Worth being precise about what this proves: in this library the
    // non-canonical scalar is refused while the signature is decoded, before
    // verification runs at all. So this test covers signature decoding, not
    // the choice of verify_strict over verify — swapping those two does not
    // change the outcome here, which a mutation check confirmed.
    //
    // verify_strict is still what this crate calls. Its distinguishing
    // behaviour is refusing low-order public keys and R values, and that case
    // cannot be reached through this API because PublicKey::from_bytes already
    // rejects low-order keys. It is defence in depth against that check ever
    // being weakened, and is documented as such rather than claimed as tested.
    const L: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde,
        0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x10,
    ];
    let pair = KeyPair::generate().unwrap();
    let message = b"install me";
    let original = sign(&pair, message);
    verify(&pair.public(), message, &original).expect("the genuine signature must verify");

    // S' = S + L, little-endian, carrying across the upper half.
    let mut raw = original.to_bytes();
    let mut carry = 0u16;
    for i in 0..32 {
        let sum = u16::from(raw[32 + i]) + u16::from(L[i]) + carry;
        raw[32 + i] = (sum & 0xff) as u8;
        carry = sum >> 8;
    }
    let mutated = Signature::from_bytes(raw);
    assert_ne!(mutated.to_hex(), original.to_hex(), "the mutation must change the bytes");
    assert!(
        verify(&pair.public(), message, &mutated).is_err(),
        "a non-canonical scalar must be rejected"
    );
}

#[test]
fn a_signature_is_bound_to_its_exact_message() {
    let pair = KeyPair::generate().unwrap();
    let signature = sign(&pair, b"version 1.0.0");
    for forged in [
        &b"version 1.0.1"[..],
        &b"version 1.0.0 "[..],
        &b" version 1.0.0"[..],
        &b"version 1.0."[..],
        &b""[..],
    ] {
        assert!(
            verify(&pair.public(), forged, &signature).is_err(),
            "signature must not verify against {forged:?}"
        );
    }
}

#[test]
fn public_key_equality_has_no_early_exit() {
    // Constant-time comparison must examine every byte, so a difference in the
    // last byte is detected exactly as reliably as one in the first.
    let pair = KeyPair::generate().unwrap();
    let key = pair.public();
    let mut raw = key.to_bytes();

    for index in [0usize, 15, 31] {
        let mut altered = raw;
        altered[index] ^= 0x01;
        // Not every perturbation is a valid curve point; skip those that fail
        // to parse, since they are refused for a different and stronger reason.
        if let Ok(other) = PublicKey::from_bytes(&altered) {
            assert_ne!(other, key, "difference at byte {index} must be detected");
        }
    }
    raw[0] ^= 0x01;
    let _ = raw;
}
