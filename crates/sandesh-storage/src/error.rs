//! Storage errors, and their conversion into domain errors.
//!
//! `sqlx` types stop here. Everything crossing back into the core becomes a
//! [`CoreError`], so the core never learns which database it is talking to.

use sandesh_core::{CoreError, StoreError};

/// A failure in the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The database rejected or failed a query.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// A migration could not be applied.
    #[error("migration error: {0}")]
    Migration(String),

    /// A stored row could not be mapped back into a domain type.
    ///
    /// Indicates corruption or a schema/code mismatch, not a caller mistake.
    #[error("corrupt row in {table}: {detail}")]
    Corrupt {
        /// The table the row came from.
        table: &'static str,
        /// What was wrong with it.
        detail: String,
    },

    /// The blob store could not read or write an object.
    #[error("blob store error: {0}")]
    Blob(String),

    /// An I/O error occurred on the filesystem.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl StorageError {
    /// Wraps a migration failure.
    pub fn migration(error: impl std::fmt::Display) -> Self {
        Self::Migration(error.to_string())
    }

    /// Builds a [`StorageError::Corrupt`].
    pub fn corrupt(table: &'static str, detail: impl Into<String>) -> Self {
        Self::Corrupt {
            table,
            detail: detail.into(),
        }
    }

    /// Whether this error is a unique-constraint violation.
    ///
    /// Used to turn a race on a unique index into a domain-level
    /// `AlreadyExists` rather than an opaque failure.
    #[must_use]
    pub fn is_unique_violation(&self) -> bool {
        match self {
            Self::Database(sqlx::Error::Database(db)) => db.is_unique_violation(),
            _ => false,
        }
    }
}

impl From<StorageError> for CoreError {
    /// Collapses a storage failure into the opaque domain error.
    ///
    /// The detail is preserved as text; the *type* is not, which is what stops
    /// `sqlx` from leaking across the boundary.
    fn from(error: StorageError) -> Self {
        Self::Storage(StoreError::new(error.to_string()))
    }
}

/// A storage-layer result.
pub type StorageResult<T> = Result<T, StorageError>;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn storage_errors_become_opaque_domain_errors() {
        let err: CoreError = StorageError::corrupt("messages", "bad uuid").into();
        assert!(matches!(err, CoreError::Storage(_)));
    }

    #[test]
    fn domain_error_text_carries_detail_without_the_type() {
        let text = CoreError::from(StorageError::Blob(String::from("no such object"))).to_string();
        assert!(text.contains("no such object"));
    }
}
