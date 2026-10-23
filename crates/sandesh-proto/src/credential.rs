//! Password hashing and verification (Phase 4.5, ADR-032).
//!
//! Argon2id, with the parameters from the OWASP password-storage guidance
//! (19 MiB, 2 iterations, 1 lane). Argon2id rather than bcrypt or PBKDF2
//! because it is memory-hard: an attacker with a GPU or an ASIC gains far less
//! than they would against a compute-only function.
//!
//! ## What this module guarantees
//!
//! * A stored credential is a hash. There is no recoverable secret anywhere —
//!   see ADR-032 on why CRAM-MD5 was dropped rather than implemented.
//! * Verification is constant-time with respect to the *hash*, because
//!   `password-hash` compares the derived output that way.
//! * Verifying against an account that has no credential still does the work,
//!   so "no such user" and "wrong password" take the same time. Skipping the
//!   hash for an unknown user is the classic username-enumeration oracle.
//!
//! ## What it deliberately does not do
//!
//! It does not decide *whether* authentication is required, or whether the
//! connection is encrypted. Those are policy, and they live in the session that
//! owns the connection.

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};

use crate::Secret;

/// Why a credential could not be produced or checked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// The stored hash could not be parsed.
    ///
    /// Means a corrupted or hand-edited database row, never a wrong password.
    #[error("the stored credential is not a valid Argon2 hash")]
    MalformedHash,

    /// Hashing failed.
    #[error("cannot hash the password: {0}")]
    Hashing(String),
}

/// The Argon2id parameters used for every new credential.
///
/// OWASP's recommended minimum for Argon2id: 19 MiB of memory, 2 iterations,
/// 1 degree of parallelism. The memory cost is what makes the hash expensive to
/// attack in parallel, so it is the parameter not to lower.
///
/// A hash carries its own parameters, so raising these later does not
/// invalidate existing credentials — old hashes keep verifying with the
/// parameters they were created under.
const MEMORY_KIB: u32 = 19 * 1024;
const ITERATIONS: u32 = 2;
const PARALLELISM: u32 = 1;

/// Builds the configured hasher.
fn argon2() -> Result<Argon2<'static>, CredentialError> {
    let params = Params::new(MEMORY_KIB, ITERATIONS, PARALLELISM, None)
        .map_err(|error| CredentialError::Hashing(error.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Hashes a password for storage.
///
/// The returned string is PHC format: it carries the algorithm, version,
/// parameters and salt, so nothing else needs to be stored alongside it.
///
/// # Errors
/// [`CredentialError::Hashing`] if the parameters or the RNG fail, which in
/// practice means the platform has no entropy source.
pub fn hash_password(password: &Secret) -> Result<String, CredentialError> {
    // A fresh random salt is generated per call by the hasher itself.
    argon2()?
        .hash_password(password.expose().as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|error| CredentialError::Hashing(error.to_string()))
}

/// Verifies a password against a stored hash.
///
/// Returns `Ok(false)` for a wrong password and an error only when the stored
/// hash itself is unusable — the caller must not treat those the same way, but
/// must also not tell the client which happened.
///
/// # Errors
/// [`CredentialError::MalformedHash`] when the stored value is not a parseable
/// Argon2 hash.
pub fn verify_password(password: &Secret, stored_hash: &str) -> Result<bool, CredentialError> {
    match argon2()?.verify_password(password.expose().as_bytes(), stored_hash) {
        Ok(()) => Ok(true),
        // `password-hash` reports a mismatch as an error; only that one means
        // "wrong password". Anything else is a broken stored hash, which must
        // not be reported to the client as a simple authentication failure.
        Err(argon2::password_hash::Error::PasswordInvalid) => Ok(false),
        Err(_) => Err(CredentialError::MalformedHash),
    }
}

/// A hash to verify against when no account exists.
///
/// Authenticating an unknown user must cost the same as authenticating a known
/// one with the wrong password. Returning early on "no such account" leaks
/// which usernames exist, measurably: the unknown-user path would return in
/// microseconds where the known-user path spends milliseconds in Argon2.
///
/// This is a real hash of a value nothing can match, so verifying against it
/// performs the identical work.
fn dummy_hash() -> Option<&'static str> {
    // Computed once. A `OnceLock` rather than a constant because the salt must
    // be generated, and a hard-coded hash string would be a fingerprint of this
    // exact build.
    static DUMMY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

    DUMMY
        .get_or_init(|| hash_password(&Secret::new("*unmatchable*")).ok())
        .as_deref()
}

/// Verifies against an optional stored hash, in constant time with respect to
/// whether the account exists.
///
/// Pass `None` when the username matched no account. The work is done anyway
/// and the answer is `false`.
#[must_use]
pub fn verify_optional(password: &Secret, stored_hash: Option<&str>) -> bool {
    if let Some(hash) = stored_hash {
        return verify_password(password, hash).unwrap_or(false);
    }

    // No such account. Do the same work anyway, then fail: returning early
    // here is what would make the unknown-user path measurably faster.
    if let Some(dummy) = dummy_hash() {
        let _ = verify_password(password, dummy);
    }
    false
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_password_verifies_against_its_own_hash() {
        let password = Secret::new("correct horse battery staple");
        let hash = hash_password(&password).expect("hashes");
        assert!(verify_password(&password, &hash).expect("verifies"));
    }

    #[test]
    fn a_wrong_password_is_rejected() {
        let hash = hash_password(&Secret::new("right")).expect("hashes");
        assert!(!verify_password(&Secret::new("wrong"), &hash).expect("verifies"));
    }

    #[test]
    fn the_hash_never_contains_the_password() {
        // The whole point of storing a hash. A substring check is crude, but it
        // catches the catastrophic mistake of storing the password itself.
        let password = "an-unmistakable-password-string";
        let hash = hash_password(&Secret::new(password)).expect("hashes");
        assert!(!hash.contains(password), "the hash leaked the password");
    }

    #[test]
    fn two_hashes_of_one_password_differ() {
        // Distinct salts. Identical hashes would mean equal passwords are
        // visible to anyone who reads the database.
        let password = Secret::new("same");
        let first = hash_password(&password).expect("hashes");
        let second = hash_password(&password).expect("hashes");
        assert_ne!(first, second, "each hash must carry its own salt");
        // Both must still verify.
        assert!(verify_password(&password, &first).expect("verifies"));
        assert!(verify_password(&password, &second).expect("verifies"));
    }

    #[test]
    fn the_hash_declares_argon2id() {
        // A downgrade to argon2i or argon2d would still verify, so the
        // algorithm is asserted rather than assumed.
        let hash = hash_password(&Secret::new("x")).expect("hashes");
        assert!(hash.starts_with("$argon2id$"), "got {hash}");
    }

    #[test]
    fn the_hash_carries_the_owasp_parameters() {
        let hash = hash_password(&Secret::new("x")).expect("hashes");
        assert!(
            hash.contains(&format!("m={MEMORY_KIB}")),
            "memory cost: {hash}"
        );
        assert!(
            hash.contains(&format!("t={ITERATIONS}")),
            "iterations: {hash}"
        );
        assert!(hash.contains(&format!("p={PARALLELISM}")), "lanes: {hash}");
    }

    #[test]
    fn a_malformed_stored_hash_is_an_error_not_a_pass() {
        // A corrupted row must never authenticate anyone.
        for broken in ["", "not-a-hash", "$argon2id$garbage"] {
            assert!(
                verify_password(&Secret::new("anything"), broken).is_err(),
                "{broken} must not verify"
            );
        }
    }

    #[test]
    fn an_absent_credential_never_authenticates() {
        assert!(!verify_optional(&Secret::new("anything"), None));
    }

    #[test]
    fn an_empty_password_does_not_match_a_real_hash() {
        let hash = hash_password(&Secret::new("real")).expect("hashes");
        assert!(!verify_password(&Secret::new(""), &hash).expect("verifies"));
    }

    #[test]
    fn an_unknown_account_costs_about_as_much_as_a_wrong_password() {
        // The username-enumeration guard. Timing assertions are inherently
        // noisy, so this asserts only the property that matters: the unknown
        // path is not *orders of magnitude* faster, which is what an early
        // return would make it.
        let hash = hash_password(&Secret::new("real")).expect("hashes");
        let password = Secret::new("guess");

        let known_start = std::time::Instant::now();
        let _ = verify_optional(&password, Some(&hash));
        let known = known_start.elapsed();

        let unknown_start = std::time::Instant::now();
        let _ = verify_optional(&password, None);
        let unknown = unknown_start.elapsed();

        assert!(
            unknown * 5 >= known,
            "an unknown account returned far too quickly \
             ({unknown:?} against {known:?}), which leaks which usernames exist"
        );
    }
}
