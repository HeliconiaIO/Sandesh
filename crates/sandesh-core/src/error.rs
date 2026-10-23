//! The protocol-neutral error model (`CORE_SPEC.md` §10).
//!
//! `CoreError` carries no protocol status codes. Each adapter maps it onto its
//! own vocabulary — SMTP `550`, IMAP `NO`, POP3 `-ERR`, HTTP `404`. Keeping the
//! mapping in the adapter is what lets adapters be replaced without touching
//! the core.

use core::fmt;

/// The kind of entity an error refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    /// An account.
    Account,
    /// An email identity.
    Identity,
    /// A mailbox.
    Mailbox,
    /// A message.
    Message,
    /// A message placement.
    Entry,
    /// An attachment.
    Attachment,
    /// A delivery record.
    Delivery,
    /// A registered webhook endpoint.
    WebhookEndpoint,
    /// An auto-tagging rule.
    Rule,
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Account => "account",
            Self::Identity => "identity",
            Self::Mailbox => "mailbox",
            Self::Message => "message",
            Self::Entry => "entry",
            Self::Attachment => "attachment",
            Self::Delivery => "delivery",
            Self::WebhookEndpoint => "webhook endpoint",
            Self::Rule => "rule",
        };
        f.write_str(name)
    }
}

/// An opaque storage failure.
///
/// Deliberately a string rather than a typed database error: no `sqlx` type may
/// cross the core boundary, or a `PostgreSQL` backend (or an in-memory test
/// double) would stop being a drop-in substitution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("storage failure: {message}")]
pub struct StoreError {
    message: String,
}

impl StoreError {
    /// Wraps a storage-layer failure description.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The failure description.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A domain error, independent of any protocol.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    /// The entity does not exist.
    #[error("{entity} not found: {id}")]
    NotFound {
        /// Which kind of entity.
        entity: EntityKind,
        /// The identifier or key looked up.
        id: String,
    },

    /// An entity with the same unique key already exists.
    #[error("{entity} already exists: {key}")]
    AlreadyExists {
        /// Which kind of entity.
        entity: EntityKind,
        /// The conflicting key.
        key: String,
    },

    /// An argument failed validation.
    #[error("invalid {field}: {reason}")]
    InvalidArgument {
        /// The offending field.
        field: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A domain invariant was broken. This indicates a bug.
    #[error("invariant violated: {invariant}")]
    InvariantViolated {
        /// The invariant that failed.
        invariant: String,
    },

    /// A concurrent modification prevented the operation.
    #[error("conflict: {reason}")]
    Conflict {
        /// What conflicted.
        reason: String,
    },

    /// A configured limit was exceeded.
    #[error("quota exceeded: {limit}")]
    QuotaExceeded {
        /// The limit that was hit.
        limit: String,
    },

    /// The storage layer failed.
    #[error(transparent)]
    Storage(#[from] StoreError),
}

impl CoreError {
    /// Builds a [`CoreError::NotFound`].
    #[must_use]
    pub fn not_found(entity: EntityKind, id: impl fmt::Display) -> Self {
        Self::NotFound {
            entity,
            id: id.to_string(),
        }
    }

    /// Builds a [`CoreError::AlreadyExists`].
    #[must_use]
    pub fn already_exists(entity: EntityKind, key: impl fmt::Display) -> Self {
        Self::AlreadyExists {
            entity,
            key: key.to_string(),
        }
    }

    /// Builds a [`CoreError::InvalidArgument`].
    #[must_use]
    pub fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidArgument {
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// Builds a [`CoreError::InvariantViolated`].
    #[must_use]
    pub fn invariant(invariant: impl Into<String>) -> Self {
        Self::InvariantViolated {
            invariant: invariant.into(),
        }
    }
}

/// A domain result.
pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_render_usefully() {
        let err = CoreError::not_found(EntityKind::Mailbox, "INBOX");
        assert_eq!(err.to_string(), "mailbox not found: INBOX");
    }

    #[test]
    fn store_errors_convert_into_core_errors() {
        let err: CoreError = StoreError::new("disk full").into();
        assert!(matches!(err, CoreError::Storage(_)));
        assert!(err.to_string().contains("disk full"));
    }

    #[test]
    fn no_protocol_codes_leak_into_the_domain() {
        // Mapping to 550/NO/-ERR/404 belongs to the adapters.
        let rendered = CoreError::not_found(EntityKind::Message, "x").to_string();
        for code in ["550", "NO", "-ERR", "404"] {
            assert!(!rendered.contains(code), "protocol code leaked: {code}");
        }
    }
}
