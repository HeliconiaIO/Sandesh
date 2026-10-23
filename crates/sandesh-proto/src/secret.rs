//! A string that cannot be logged (Phase 4.5).
//!
//! Passwords pass through several layers on their way from a SASL decoder to a
//! verifier: a command parser, a session state machine, a delivery service.
//! Every one of those layers derives `Debug`, and `tracing` will happily render
//! a `Debug` value into a log line. One `#[derive(Debug)]` on a struct that
//! holds a `String` password is all it takes to write credentials to disk
//! forever.
//!
//! [`Secret`] makes that mistake impossible rather than forbidden. Its `Debug`
//! and `Display` render `[redacted]`, so a password cannot reach a log even
//! through a struct that was derived without thinking about it. Reading the
//! value requires calling [`Secret::expose`], which is deliberately ugly and
//! greppable: a reviewer can find every place a credential is actually read.
//!
//! ```
//! use sandesh_proto::Secret;
//!
//! let password = Secret::new("hunter2");
//! assert_eq!(format!("{password:?}"), "[redacted]");
//! assert_eq!(password.expose(), "hunter2");
//! ```

use subtle::ConstantTimeEq;

/// What a redacted secret renders as, everywhere.
const REDACTED: &str = "[redacted]";

/// A credential that does not appear in logs.
///
/// Comparison is constant-time, so using one in an `==` does not leak its
/// contents through timing.
#[derive(Clone, Default)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reads the value.
    ///
    /// Named to be conspicuous: every call site is a place a credential is
    /// handled in the clear, and should be reviewable by grepping for it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the secret is empty.
    ///
    /// SASL treats an empty password as a failed authentication rather than as
    /// a valid credential, so callers need to ask without exposing it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Rendered as `[redacted]`, never as the value.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Rendered as `[redacted]`, never as the value.
///
/// `Display` matters as much as `Debug`: `tracing::info!(%password)` uses it,
/// and a type that redacts only one of the two still leaks through the other.
impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Constant-time equality.
///
/// A byte-by-byte `==` on a secret leaks its length and prefix through timing.
/// This is not the password *verification* path — that is Argon2's job — but a
/// `Secret` should not be a footgun wherever it is compared.
impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl Eq for Secret {}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_the_value() {
        // The property the whole type exists for. If this fails, a password can
        // reach a log through any struct that derives Debug.
        let secret = Secret::new("correct horse battery staple");
        assert_eq!(format!("{secret:?}"), REDACTED);
        assert!(!format!("{secret:?}").contains("horse"));
    }

    #[test]
    fn display_never_reveals_the_value() {
        // `tracing::info!(%value)` goes through Display, not Debug.
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret}"), REDACTED);
        assert!(!format!("{secret}").contains("hunter2"));
    }

    #[test]
    fn a_secret_nested_in_a_derived_debug_struct_stays_redacted() {
        // The realistic leak: nobody logs the password directly, they log the
        // session struct that happens to contain one.
        // The fields are never read directly; they exist to be rendered by
        // the derived `Debug`, which is exactly what is under test.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Session {
            user: String,
            password: Secret,
        }

        let rendered = format!(
            "{:?}",
            Session {
                user: String::from("dev"),
                password: Secret::new("hunter2"),
            }
        );

        assert!(rendered.contains("dev"), "non-secret fields still render");
        assert!(
            !rendered.contains("hunter2"),
            "the secret must not: {rendered}"
        );
    }

    #[test]
    fn the_value_is_readable_when_deliberately_exposed() {
        assert_eq!(Secret::new("hunter2").expose(), "hunter2");
    }

    #[test]
    fn equality_compares_contents() {
        assert_eq!(Secret::new("a"), Secret::new("a"));
        assert_ne!(Secret::new("a"), Secret::new("b"));
        // Different lengths must not panic in the constant-time comparison.
        assert_ne!(Secret::new("a"), Secret::new("aa"));
    }

    #[test]
    fn emptiness_is_observable_without_exposure() {
        assert!(Secret::new("").is_empty());
        assert!(!Secret::new("x").is_empty());
    }
}
