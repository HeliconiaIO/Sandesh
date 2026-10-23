//! SASL mechanisms (Phase 4.5, ADR-032).
//!
//! Two mechanisms, both of which send the password to the server and therefore
//! both of which are only offered over TLS (ADR-033):
//!
//! | Mechanism | Exchange | RFC |
//! | --- | --- | --- |
//! | `PLAIN` | one message: `authzid \0 authcid \0 password` | RFC 4616 |
//! | `LOGIN` | two prompts: username, then password | non-standard, universally implemented |
//!
//! CRAM-MD5 is deliberately absent. It avoids sending the password, but only by
//! requiring the *server* to hold a recoverable secret — which means a database
//! read yields every password. Trading credentials at rest for credentials in
//! flight is a bad trade when TLS already protects the wire (ADR-032).
//!
//! ## This module decodes; it does not decide
//!
//! Nothing here consults an account store, checks a password, or knows whether
//! authentication is required. It turns wire bytes into a [`Credentials`] and
//! rejects what is malformed. Policy belongs to the session that owns the
//! connection, and verification to [`crate::verify_optional`].

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::Secret;

/// The longest base64 payload accepted in one SASL message.
///
/// RFC 4954 sets no limit, so one is imposed here: without it a client could
/// stream unbounded base64 and make the server allocate it. Generous enough for
/// any real username and password, small enough to be harmless.
const MAX_ENCODED_LEN: usize = 8 * 1024;

/// A SASL mechanism this server implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// RFC 4616. One message carrying authzid, authcid and password.
    Plain,
    /// Two-prompt username and password exchange.
    Login,
}

impl Mechanism {
    /// Parses a mechanism name, case-insensitively as RFC 4422 requires.
    ///
    /// Returns `None` for anything unimplemented — including `CRAM-MD5`, which
    /// must be refused rather than silently downgraded to something else.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "PLAIN" => Some(Self::Plain),
            "LOGIN" => Some(Self::Login),
            _ => None,
        }
    }

    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "PLAIN",
            Self::Login => "LOGIN",
        }
    }

    /// Every mechanism advertised, in preference order.
    ///
    /// `PLAIN` first because it completes in one round trip.
    #[must_use]
    pub const fn advertised() -> &'static [Self] {
        &[Self::Plain, Self::Login]
    }
}

/// A username and password decoded from a SASL exchange.
///
/// The password is a [`Secret`], so this struct can be held in a session that
/// derives `Debug` without the password reaching a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    /// The identity being authenticated (SASL authcid).
    pub username: String,
    /// The password.
    pub password: Secret,
}

/// Why a SASL message could not be decoded.
///
/// Every variant is reported to the client as one generic failure. The
/// distinctions exist for logging and tests, never for the wire: telling a
/// client *why* its credential was malformed helps an attacker probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SaslError {
    /// The payload was not valid base64.
    #[error("the SASL payload is not valid base64")]
    NotBase64,

    /// The decoded payload did not have the mechanism's shape.
    #[error("the SASL payload is malformed")]
    Malformed,

    /// The payload exceeded [`MAX_ENCODED_LEN`].
    #[error("the SASL payload is too large")]
    TooLarge,

    /// A field that must be UTF-8 was not.
    #[error("the SASL payload is not valid UTF-8")]
    NotUtf8,

    /// The client cancelled the exchange by sending `*`.
    #[error("the client cancelled authentication")]
    Cancelled,
}

/// Decodes a base64 SASL payload.
///
/// A lone `*` is the RFC 4954 cancellation token and is reported as such, so
/// the session can answer `501` rather than treating it as a bad credential.
fn decode(encoded: &str) -> Result<Vec<u8>, SaslError> {
    let trimmed = encoded.trim();

    if trimmed == "*" {
        return Err(SaslError::Cancelled);
    }

    if trimmed.len() > MAX_ENCODED_LEN {
        return Err(SaslError::TooLarge);
    }

    BASE64.decode(trimmed).map_err(|_| SaslError::NotBase64)
}

/// Decodes an RFC 4616 `PLAIN` message.
///
/// The wire form is `authzid \0 authcid \0 password`, where `authzid` is
/// normally empty. A non-empty authzid asks to act *as another user*; Sandesh
/// has no such delegation model, so it is accepted only when it equals the
/// authcid and rejected otherwise. Silently ignoring it would mean a client
/// believing it authorised as someone else while the server logged it as
/// someone different.
///
/// # Errors
/// [`SaslError`] when the payload is not base64, not UTF-8, or does not have
/// exactly three NUL-separated fields.
pub fn decode_plain(encoded: &str) -> Result<Credentials, SaslError> {
    let decoded = decode(encoded)?;

    // Exactly two NULs, so exactly three fields. `splitn` would silently accept
    // a password containing a NUL as part of the password; being strict here
    // means a malformed message cannot become a valid-looking credential.
    let mut fields = decoded.split(|byte| *byte == 0);
    let authzid = fields.next().ok_or(SaslError::Malformed)?;
    let identity = fields.next().ok_or(SaslError::Malformed)?;
    let password = fields.next().ok_or(SaslError::Malformed)?;

    if fields.next().is_some() {
        return Err(SaslError::Malformed);
    }

    let authzid = std::str::from_utf8(authzid).map_err(|_| SaslError::NotUtf8)?;
    let username = std::str::from_utf8(identity).map_err(|_| SaslError::NotUtf8)?;
    let password = std::str::from_utf8(password).map_err(|_| SaslError::NotUtf8)?;

    if username.is_empty() || password.is_empty() {
        return Err(SaslError::Malformed);
    }

    // No delegation model: authorising as anyone but yourself is refused.
    if !authzid.is_empty() && authzid != username {
        return Err(SaslError::Malformed);
    }

    Ok(Credentials {
        username: username.to_owned(),
        password: Secret::new(password),
    })
}

/// Decodes one base64 field of a `LOGIN` exchange.
///
/// # Errors
/// [`SaslError`] when the payload is not base64 or not UTF-8.
pub fn decode_login_field(encoded: &str) -> Result<String, SaslError> {
    let decoded = decode(encoded)?;
    let value = std::str::from_utf8(&decoded).map_err(|_| SaslError::NotUtf8)?;
    Ok(value.to_owned())
}

/// The base64 prompts a `LOGIN` exchange sends, in order.
///
/// Pre-encoded because they are constants: `Username:` and `Password:`.
#[must_use]
pub fn login_prompts() -> [String; 2] {
    [BASE64.encode("Username:"), BASE64.encode("Password:")]
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn encode(raw: &[u8]) -> String {
        BASE64.encode(raw)
    }

    #[test]
    fn mechanism_names_are_case_insensitive() {
        assert_eq!(Mechanism::parse("plain"), Some(Mechanism::Plain));
        assert_eq!(Mechanism::parse("PLAIN"), Some(Mechanism::Plain));
        assert_eq!(Mechanism::parse(" Login "), Some(Mechanism::Login));
    }

    #[test]
    fn cram_md5_is_refused_rather_than_offered() {
        // ADR-032: implementing it would require a recoverable secret at rest.
        // A client asking for it must be told no, not quietly given something
        // else.
        assert_eq!(Mechanism::parse("CRAM-MD5"), None);
        assert_eq!(Mechanism::parse("DIGEST-MD5"), None);
        assert!(
            !Mechanism::advertised()
                .iter()
                .any(|m| m.as_str().contains("CRAM"))
        );
    }

    #[test]
    fn plain_decodes_the_rfc_4616_form() {
        let credentials = decode_plain(&encode(b"\0dev\0hunter2")).expect("decodes");
        assert_eq!(credentials.username, "dev");
        assert_eq!(credentials.password.expose(), "hunter2");
    }

    #[test]
    fn plain_accepts_an_authzid_equal_to_the_username() {
        let credentials = decode_plain(&encode(b"dev\0dev\0hunter2")).expect("decodes");
        assert_eq!(credentials.username, "dev");
    }

    #[test]
    fn plain_refuses_authorising_as_another_user() {
        // Sandesh has no delegation model. Ignoring authzid would let a client
        // believe it acted as `admin` while the server recorded `dev`.
        assert_eq!(
            decode_plain(&encode(b"admin\0dev\0hunter2")),
            Err(SaslError::Malformed)
        );
    }

    #[test]
    fn plain_rejects_a_payload_with_the_wrong_field_count() {
        for raw in [
            &b"dev\0hunter2"[..],          // two fields
            &b"\0dev\0hunter2\0extra"[..], // four
            &b"nonulls"[..],               // one
        ] {
            assert_eq!(
                decode_plain(&encode(raw)),
                Err(SaslError::Malformed),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn plain_rejects_empty_credentials() {
        // An empty password must never be treated as "no password required".
        assert_eq!(decode_plain(&encode(b"\0dev\0")), Err(SaslError::Malformed));
        assert_eq!(
            decode_plain(&encode(b"\0\0hunter2")),
            Err(SaslError::Malformed)
        );
    }

    #[test]
    fn a_non_base64_payload_is_rejected() {
        assert_eq!(decode_plain("not base64!!"), Err(SaslError::NotBase64));
    }

    #[test]
    fn an_oversized_payload_is_rejected_before_decoding() {
        // The allocation guard: a client must not be able to make the server
        // buffer unbounded base64.
        let huge = "A".repeat(MAX_ENCODED_LEN + 1);
        assert_eq!(decode_plain(&huge), Err(SaslError::TooLarge));
    }

    #[test]
    fn a_cancellation_is_distinguished_from_a_bad_credential() {
        // RFC 4954: `*` cancels. It deserves 501, not "authentication failed".
        assert_eq!(decode_plain("*"), Err(SaslError::Cancelled));
        assert_eq!(decode_login_field(" * "), Err(SaslError::Cancelled));
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        // 0xFF is never valid UTF-8.
        assert_eq!(
            decode_plain(&encode(b"\0dev\0\xFF\xFE")),
            Err(SaslError::NotUtf8)
        );
    }

    #[test]
    fn login_fields_decode_independently() {
        assert_eq!(decode_login_field(&encode(b"dev")).expect("decodes"), "dev");
    }

    #[test]
    fn the_login_prompts_are_the_expected_base64() {
        let [user, password] = login_prompts();
        assert_eq!(BASE64.decode(&user).expect("decodes"), b"Username:");
        assert_eq!(BASE64.decode(&password).expect("decodes"), b"Password:");
    }

    #[test]
    fn a_decoded_credential_does_not_leak_through_debug() {
        // The whole exchange ends up in a session struct that derives Debug.
        let credentials = decode_plain(&encode(b"\0dev\0hunter2")).expect("decodes");
        let rendered = format!("{credentials:?}");
        assert!(rendered.contains("dev"));
        assert!(!rendered.contains("hunter2"), "leaked: {rendered}");
    }
}
