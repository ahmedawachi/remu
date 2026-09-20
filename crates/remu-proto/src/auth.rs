//! Unattended-access authentication.
//!
//! When a host has an unattended password set, a controller must prove it knows
//! that password before the host will share its screen without a human
//! accepting the prompt.
//!
//! The proof is `HMAC-SHA256(password, domain || nonce)` where the nonce is
//! fresh, random and single-use, issued by the *host* for this one attempt.
//! Consequences worth stating plainly:
//!
//! - The relay never sees the password, only a proof it cannot invert.
//! - A proof captured off the wire authenticates nothing else, because the
//!   nonce it is bound to is already spent. This is the property the Electron
//!   original lacked: it sent a bare `sha256(password)`, identical on every
//!   connection, so observing one offer was as good as knowing the password.
//! - The nonce is issued by the host rather than the relay, so a malicious
//!   relay cannot replay a proof by re-serving a nonce it chose.
//!
//! This is *not* a PAKE: a host that is impersonated can collect proofs and
//! mount an offline dictionary attack against a weak password. Pick a strong
//! unattended password, or leave it unset and accept prompts by hand.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Domain separator mixed into every proof.
///
/// Binds a proof to this protocol and version, so one can never be replayed
/// into a different Remu exchange that also HMACs with the same password.
const DOMAIN: &[u8] = b"remu/unattended/v1";

/// Bytes of entropy in a challenge nonce. 128 bits: collisions are impossible
/// in practice, so "have I issued this nonce before" is answerable by the host
/// keeping only the nonce for the attempt in flight.
pub const NONCE_BYTES: usize = 16;

/// A fresh, single-use challenge nonce as lowercase hex.
pub fn generate_nonce() -> String {
    let mut buf = [0u8; NONCE_BYTES];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Computes the controller's proof of knowledge for `nonce`.
pub fn challenge_response(password: &str, nonce: &str) -> String {
    // HMAC accepts a key of any length, so this cannot fail.
    let mut mac =
        HmacSha256::new_from_slice(password.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(DOMAIN);
    mac.update(nonce.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Verifies a controller's proof in constant time.
///
/// Returns `false` for a malformed proof rather than erroring: to a caller
/// deciding whether to share a screen, "wrong" and "not even hex" are the same
/// answer, and collapsing them keeps the rejection path free of shapes an
/// attacker could distinguish by timing or by error text.
pub fn verify(password: &str, nonce: &str, proof: &str) -> bool {
    let expected = challenge_response(password, nonce);
    let (Ok(expected), Ok(given)) = (hex::decode(&expected), hex::decode(proof)) else {
        return false;
    };
    if expected.len() != given.len() {
        return false;
    }
    expected.ct_eq(&given).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_proof_computed_from_the_same_password_and_nonce() {
        let nonce = generate_nonce();
        let proof = challenge_response("hunter2", &nonce);
        assert!(verify("hunter2", &nonce, &proof));
    }

    #[test]
    fn rejects_the_wrong_password() {
        let nonce = generate_nonce();
        let proof = challenge_response("hunter2", &nonce);
        assert!(!verify("hunter3", &nonce, &proof));
    }

    #[test]
    fn rejects_a_proof_replayed_against_a_different_nonce() {
        // The property the Electron original did not have: a captured proof is
        // worthless once the nonce it was bound to is spent.
        let first = generate_nonce();
        let second = generate_nonce();
        let proof = challenge_response("hunter2", &first);
        assert!(!verify("hunter2", &second, &proof));
    }

    #[test]
    fn rejects_malformed_proofs_without_panicking() {
        let nonce = generate_nonce();
        for bad in [
            "",
            "zz",
            "not-hex-at-all",
            "ab",
            &"ff".repeat(31),
            &"ff".repeat(33),
        ] {
            assert!(!verify("hunter2", &nonce, bad), "accepted {bad:?}");
        }
    }

    #[test]
    fn produces_a_distinct_nonce_each_time() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let nonce = generate_nonce();
            assert_eq!(nonce.len(), NONCE_BYTES * 2);
            assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(seen.insert(nonce), "generate_nonce repeated a value");
        }
    }

    #[test]
    fn produces_a_full_length_sha256_proof() {
        let proof = challenge_response("pw", "00");
        assert_eq!(proof.len(), 64);
        assert!(proof.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn is_deterministic_for_a_given_password_and_nonce() {
        let a = challenge_response("pw", "abcdef");
        let b = challenge_response("pw", "abcdef");
        assert_eq!(a, b);
    }

    #[test]
    fn domain_separation_changes_the_proof() {
        // A bare HMAC of the nonce must not equal our domain-separated proof,
        // or the separator is not actually being mixed in.
        let mut mac = HmacSha256::new_from_slice(b"pw").unwrap();
        mac.update(b"abcdef");
        let undomained = hex::encode(mac.finalize().into_bytes());
        assert_ne!(undomained, challenge_response("pw", "abcdef"));
    }

    #[test]
    fn an_empty_password_still_produces_a_checkable_proof() {
        // Callers must gate on "is a password configured", not on whether the
        // crypto refuses an empty one -- it does not.
        let nonce = generate_nonce();
        let proof = challenge_response("", &nonce);
        assert!(verify("", &nonce, &proof));
        assert!(!verify("x", &nonce, &proof));
    }
}
