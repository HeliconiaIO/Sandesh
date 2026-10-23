//! The HTTP error boundary (RFC 9457 Problem Details, `API_DESIGN.md` §7).
//!
//! Every failure that reaches a client passes through [`ApiError`]. That is the
//! whole point of the type: it is the one place where an internal failure is
//! turned into something a client may see, so there is exactly one place to
//! check that `SQLite` strings, filesystem paths and `Debug` output do not escape.
//!
//! Internal failures deliberately lose their detail on the way out — the client
//! gets "an internal error occurred" — and are logged at `error` level instead.
//! Not-found and validation failures keep their detail, because it is about the
//! request the client made, not about how Sandesh is built.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sandesh_core::{CoreError, EntityKind};
use serde::Serialize;
use utoipa::ToSchema;

/// The base URI for error type identifiers.
const ERROR_TYPE_BASE: &str = "https://sandesh.dev/errors/";

/// An error on its way to a client.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The requested entity does not exist.
    #[error("{0} not found")]
    NotFound(&'static str),

    /// The request was malformed or its parameters unusable.
    #[error("{0}")]
    BadRequest(String),

    /// The request was well-formed but semantically unacceptable.
    #[error("{0}")]
    Unprocessable(String),

    /// The request conflicts with current state.
    #[error("{0}")]
    Conflict(String),

    /// Something failed inside Sandesh.
    ///
    /// The message is logged, never sent. See the module documentation.
    #[error("internal error: {0}")]
    Internal(String),

    /// A dependency is not ready.
    #[error("{0}")]
    Unavailable(String),

    /// No acceptable credential was presented.
    ///
    /// The detail is deliberately identical for a missing credential, an
    /// unknown user and a wrong password: distinguishing them would turn the
    /// API into an account-enumeration oracle, and the caller can act on none
    /// of the distinctions anyway.
    #[error("authentication required")]
    Unauthorized,
}

impl ApiError {
    /// The status code this error maps to.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
        }
    }

    /// The stable slug identifying the error class.
    ///
    /// Part of the wire contract: clients branch on this, not on the prose in
    /// `title`, which may be reworded.
    #[must_use]
    pub const fn slug(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not-found",
            Self::BadRequest(_) => "bad-request",
            Self::Unprocessable(_) => "unprocessable",
            Self::Conflict(_) => "conflict",
            Self::Internal(_) => "internal",
            Self::Unavailable(_) => "unavailable",
            Self::Unauthorized => "unauthorized",
        }
    }

    /// A short human-readable summary.
    #[must_use]
    pub const fn title(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "Not found",
            Self::BadRequest(_) => "Bad request",
            Self::Unprocessable(_) => "Unprocessable request",
            Self::Conflict(_) => "Conflict",
            Self::Internal(_) => "Internal error",
            Self::Unavailable(_) => "Service unavailable",
            Self::Unauthorized => "Unauthorized",
        }
    }

    /// The client-safe detail string.
    ///
    /// [`Internal`](Self::Internal) is deliberately redacted here rather than at
    /// the call site, so a new internal error cannot leak by forgetting to.
    #[must_use]
    pub fn public_detail(&self) -> String {
        match self {
            Self::Internal(_) => String::from("An internal error occurred."),
            other => other.to_string(),
        }
    }

    /// Convenience constructor for a bad request.
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::BadRequest(detail.into())
    }
}

/// A `CoreError` becomes an `ApiError` here and nowhere else.
///
/// Storage failures collapse into `Internal`: a client can do nothing about a
/// disk error, and the `SQLite` message would say more about Sandesh's internals
/// than about the request.
impl From<CoreError> for ApiError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::NotFound { entity, .. } => Self::NotFound(entity_name(entity)),
            CoreError::AlreadyExists { .. } | CoreError::Conflict { .. } => {
                Self::Conflict(error.to_string())
            }
            // A bad argument and an exceeded quota are both "the request is
            // well-formed but cannot be honoured", which is 422.
            CoreError::InvalidArgument { .. } | CoreError::QuotaExceeded { .. } => {
                Self::Unprocessable(error.to_string())
            }
            // An invariant violation is a bug in Sandesh, not in the request,
            // and a storage failure is nothing a client can act on.
            other @ (CoreError::InvariantViolated { .. } | CoreError::Storage(_)) => {
                Self::Internal(other.to_string())
            }
        }
    }
}

/// The client-facing name of an entity kind.
const fn entity_name(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Account => "Account",
        EntityKind::Mailbox => "Mailbox",
        EntityKind::Message => "Message",
        EntityKind::Entry => "Entry",
        EntityKind::Delivery => "Delivery",
        EntityKind::Attachment => "Attachment",
        EntityKind::Identity => "Identity",
        EntityKind::WebhookEndpoint => "Webhook endpoint",
        EntityKind::Rule => "Rule",
    }
}

/// The RFC 9457 body.
///
/// Public because it is the error half of the API contract: every non-2xx
/// response has this shape, so it must appear in the `OpenAPI` document and in
/// the generated TypeScript. Clients branch on `type`, whose slug is stable;
/// `title` is prose and may be reworded.
#[derive(Debug, Serialize, ToSchema)]
#[schema(title = "ProblemDetails", as = ProblemDetails)]
pub struct ProblemDetails {
    /// A URI identifying the error class.
    #[schema(example = "https://sandesh.dev/errors/not-found")]
    pub r#type: String,
    /// A short human-readable summary.
    #[schema(example = "Not found")]
    pub title: &'static str,
    /// The HTTP status code, repeated in the body as RFC 9457 specifies.
    #[schema(example = 404)]
    pub status: u16,
    /// Detail about this occurrence.
    pub detail: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();

        // Log the full internal message exactly where it is being withheld from
        // the client, so the information is never simply lost.
        if let Self::Internal(message) = &self {
            tracing::error!(error = %message, "request failed with an internal error");
        }

        let body = ProblemDetails {
            r#type: format!("{ERROR_TYPE_BASE}{}", self.slug()),
            title: self.title(),
            status: status.as_u16(),
            detail: self.public_detail(),
        };

        // RFC 9457 specifies this media type for problem responses.
        let mut response = (status, Json(body)).into_response();
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );

        // RFC 9110 §11.6.1 *requires* a 401 to carry this header; a 401 without
        // one is a protocol violation, and it is what tells `curl -u` and a
        // browser that a credential is what is wanted. Both schemes are named
        // because both are accepted.
        if matches!(self, Self::Unauthorized) {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static(
                    "Bearer realm=\"sandesh\", Basic realm=\"sandesh\"",
                ),
            );
        }
        response
    }
}

/// The result type every handler returns.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn internal_errors_never_reach_the_client() {
        // The single most important property of this module.
        let error = ApiError::Internal(String::from(
            "error returned from database: no such table: messages (/home/dev/sandesh.db)",
        ));
        let detail = error.public_detail();

        assert_eq!(detail, "An internal error occurred.");
        for leak in ["database", "no such table", "/home/dev"] {
            assert!(!detail.contains(leak), "{leak} leaked into {detail}");
        }
    }

    #[test]
    fn client_errors_keep_their_detail() {
        // These describe the client's own request, so withholding them would
        // only make the API harder to use.
        let error = ApiError::bad_request("limit must be between 1 and 500");
        assert_eq!(error.public_detail(), "limit must be between 1 and 500");
    }

    #[test]
    fn statuses_match_the_documented_mapping() {
        let cases = [
            (ApiError::NotFound("Message"), StatusCode::NOT_FOUND),
            (ApiError::bad_request("x"), StatusCode::BAD_REQUEST),
            (
                ApiError::Unprocessable(String::from("x")),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (ApiError::Conflict(String::from("x")), StatusCode::CONFLICT),
            (
                ApiError::Internal(String::from("x")),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                ApiError::Unavailable(String::from("x")),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.status(), expected, "{}", error.slug());
        }
    }

    #[test]
    fn core_not_found_maps_to_404_with_the_entity_name() {
        let error: ApiError =
            CoreError::not_found(EntityKind::Message, "0192f3a1-0000-7000-8000-000000000000")
                .into();

        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        assert_eq!(error.public_detail(), "Message not found");
    }

    #[test]
    fn core_storage_failure_is_redacted() {
        let error: ApiError = CoreError::Storage(sandesh_core::StoreError::new(
            "SQLITE_BUSY: database is locked at /var/lib/sandesh/db",
        ))
        .into();

        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!error.public_detail().contains("SQLITE_BUSY"));
        assert!(!error.public_detail().contains("/var/lib"));
    }

    #[test]
    fn slugs_are_stable_and_distinct() {
        // Clients branch on the slug, so it is a wire contract.
        let slugs = [
            ApiError::NotFound("x").slug(),
            ApiError::bad_request("x").slug(),
            ApiError::Unprocessable(String::new()).slug(),
            ApiError::Conflict(String::new()).slug(),
            ApiError::Internal(String::new()).slug(),
            ApiError::Unavailable(String::new()).slug(),
        ];
        let mut unique = slugs.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), slugs.len(), "slugs must be distinct");
        assert_eq!(ApiError::NotFound("x").slug(), "not-found");
    }
}
