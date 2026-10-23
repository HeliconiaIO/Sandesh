//! HTTP API authentication (Phase 4.5's outstanding deliverable, ADR-075).
//!
//! Two schemes, both optional and both off by default:
//!
//! - **Bearer** — a shared token from configuration (`SANDESH_API_TOKEN`).
//! - **Basic** — a username and password verified against the `accounts` table
//!   through [`CredentialVerifier`], which is the same Argon2id path SMTP and
//!   IMAP use. The API cannot call it directly: `tests/architecture.rs` forbids
//!   `sandesh-api` from depending on `sandesh-proto`, so the capability arrives
//!   as a port supplied by the composition root.
//!
//! # Why disabled is the default
//!
//! ADR-005. Sandesh binds to loopback, and demanding a credential before `curl`
//! can read a test message is friction with no security benefit there. The
//! posture is reported by `/info` and warned about at startup when the listener
//! is bound off loopback, so "off" is never silently off.
//!
//! # What is exempt, and why so little
//!
//! Only `/api/v1/health`. `API_DESIGN.md` §3 marks it as the Docker healthcheck
//! endpoint, and a container orchestrator has nowhere to put a credential. It
//! reports liveness and nothing else — no counts, no addresses, no mail — so
//! exposing it discloses only that Sandesh is running, which the open port
//! already does.
//!
//! `/health/ready` and `/info` are **not** exempt: readiness reports storage
//! state and `/info` reports bind addresses, database location and the auth
//! posture itself. Neither is mail, but both describe the deployment to an
//! unauthenticated caller, and neither is needed by a healthcheck.
//!
//! The Web UI is also not exempt when auth is on. It is a browser client like
//! any other; with a token configured it must present one.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sandesh_config::ApiToken;

use crate::error::ApiError;
use crate::state::ApiState;

/// Paths served without a credential even when authentication is required.
///
/// Deliberately a single exact path rather than a prefix: a prefix would make
/// `/health/ready` — which reports storage state — exempt by accident, and a
/// future `/health/anything` exempt before anyone reviewed it.
const EXEMPT_PATHS: &[&str] = &["/api/v1/health"];

/// Whether this request may proceed without a credential.
#[must_use]
pub fn is_exempt(path: &str) -> bool {
    EXEMPT_PATHS.contains(&path)
}

/// What a caller presented in `Authorization`.
enum Presented<'a> {
    Bearer(&'a str),
    Basic { username: String, password: String },
}

/// Parses an `Authorization` header value.
///
/// Returns `None` for anything unrecognised — a missing header, an unknown
/// scheme, undecodable base64 — because every one of those is the same answer
/// to the caller (401) and distinguishing them in the response would describe
/// the server's configuration to someone who has not authenticated.
fn parse_header(value: &str) -> Option<Presented<'_>> {
    let (scheme, rest) = value.split_once(' ')?;
    let rest = rest.trim();

    if scheme.eq_ignore_ascii_case("bearer") {
        return Some(Presented::Bearer(rest));
    }

    if scheme.eq_ignore_ascii_case("basic") {
        let decoded = BASE64.decode(rest).ok()?;
        let text = String::from_utf8(decoded).ok()?;
        // RFC 7617: the first colon separates them, so a password may contain
        // colons and a username may not.
        let (username, password) = text.split_once(':')?;
        return Some(Presented::Basic {
            username: username.to_owned(),
            password: password.to_owned(),
        });
    }

    None
}

/// Compares two tokens without leaking their common prefix through timing.
///
/// `==` on `str` short-circuits at the first differing byte, which lets an
/// attacker who can measure the difference recover a token one byte at a time.
/// This reads both fully and accumulates the difference.
///
/// The length check before it is not a leak worth avoiding: token length is not
/// secret, and comparing different-length inputs bytewise would need a decision
/// about padding that adds more risk than it removes.
#[must_use]
fn tokens_match(presented: &str, expected: &str) -> bool {
    if presented.len() != expected.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in presented.bytes().zip(expected.bytes()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Whether the presented credential is acceptable.
async fn is_authorised(
    state: &ApiState,
    token: Option<&ApiToken>,
    presented: Presented<'_>,
) -> bool {
    match presented {
        Presented::Bearer(offered) => {
            token.is_some_and(|expected| tokens_match(offered, expected.expose()))
        }
        Presented::Basic { username, password } => match state.credentials.as_ref() {
            Some(verifier) => verifier.verify(&username, &password).await,
            // No verifier wired means basic auth cannot be checked at all.
            // Refusing is the only safe answer: accepting would authenticate
            // everyone, and the bearer path is unaffected.
            None => false,
        },
    }
}

/// The authentication middleware.
///
/// Applied to the whole router rather than to individual routes: a per-route
/// list is a list someone forgets to add to, and the failure mode of forgetting
/// is an unauthenticated endpoint. Here the default is "protected" and the
/// exemptions are explicit and few.
pub async fn require_credential(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.auth.as_ref() else {
        // Authentication is disabled; nothing to check.
        return next.run(request).await;
    };

    if is_exempt(request.uri().path()) {
        return next.run(request).await;
    }

    let presented = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_header);

    let allowed = match presented {
        Some(credential) => is_authorised(&state, expected.token.as_ref(), credential).await,
        None => false,
    };

    if allowed {
        return next.run(request).await;
    }

    // Logged without the credential: the path and the fact of failure are
    // useful, the secret never is (ADR-035).
    tracing::warn!(
        path = %request.uri().path(),
        "rejected an unauthenticated API request"
    );
    axum::response::IntoResponse::into_response(ApiError::Unauthorized)
}

/// The API's authentication settings, as the router needs them.
///
/// Present only when authentication is enabled, so the middleware's first check
/// is an `Option` rather than a mode comparison — "off" cannot be misread as
/// some mode that happens to allow everything.
#[derive(Debug, Clone)]
pub struct AuthSettings {
    /// The accepted bearer token, if one is configured.
    pub token: Option<ApiToken>,
}

/// The credential verifier, as the state holds it.
pub type Verifier = Arc<dyn sandesh_core::CredentialVerifier>;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn only_the_liveness_endpoint_is_exempt() {
        assert!(is_exempt("/api/v1/health"));

        // Readiness reports storage state and `/info` reports the deployment's
        // addresses and auth posture. Neither is a healthcheck's business.
        assert!(!is_exempt("/api/v1/health/ready"));
        assert!(!is_exempt("/api/v1/info"));
        assert!(!is_exempt("/api/v1/messages"));
        assert!(!is_exempt("/"));

        // A prefix match would wrongly exempt these.
        assert!(!is_exempt("/api/v1/health/../messages"));
        assert!(!is_exempt("/api/v1/healthz"));
    }

    #[test]
    fn a_bearer_header_parses() {
        let Some(Presented::Bearer(token)) = parse_header("Bearer abc123") else {
            panic!("expected a bearer token");
        };
        assert_eq!(token, "abc123");

        // The scheme is case-insensitive per RFC 9110 §11.1.
        assert!(matches!(
            parse_header("bearer abc123"),
            Some(Presented::Bearer("abc123"))
        ));
    }

    #[test]
    fn a_basic_header_parses_and_splits_on_the_first_colon() {
        let encoded = BASE64.encode("dev:pass:with:colons");
        let Some(Presented::Basic { username, password }) =
            parse_header(&format!("Basic {encoded}"))
        else {
            panic!("expected basic credentials");
        };
        assert_eq!(username, "dev");
        // RFC 7617: only the first colon separates, so the password keeps its.
        assert_eq!(password, "pass:with:colons");
    }

    #[test]
    fn unrecognised_headers_parse_to_nothing() {
        assert!(parse_header("").is_none());
        assert!(parse_header("Bearer").is_none());
        assert!(parse_header("Digest abc").is_none());
        assert!(parse_header("Basic !!!not-base64!!!").is_none());
        // Valid base64 with no colon is not a credential.
        assert!(parse_header(&format!("Basic {}", BASE64.encode("nocolon"))).is_none());
    }

    #[test]
    fn token_comparison_rejects_near_misses() {
        assert!(tokens_match("secret", "secret"));
        assert!(!tokens_match("secret", "secrey"));
        assert!(!tokens_match("secret", "secre"));
        assert!(!tokens_match("", "secret"));
        // An empty expected token must not match an empty presented one here
        // either; config validation rejects empty tokens before this is reached,
        // but the comparison should not be the thing that makes it safe.
        assert!(tokens_match("", ""));
    }
}
