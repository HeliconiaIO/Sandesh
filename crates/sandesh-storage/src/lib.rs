//! # Sandesh storage
//!
//! `SQLite` implementations of the `sandesh-core` storage ports.
//!
//! This crate depends **inward** on `sandesh-core` and is depended on by
//! nothing except the composition root. No `sqlx` type crosses back over the
//! boundary: every error becomes an opaque
//! [`CoreError::Storage`](sandesh_core::CoreError::Storage), which is what
//! keeps a future `PostgreSQL` backend — or an in-memory test double — a genuine
//! drop-in substitution.
//!
//! ## Structure
//!
//! * [`Database`] — connection pooling, pragmas, migrations
//! * [`SqliteAccountStore`], [`SqliteMailboxStore`], [`SqliteMessageStore`] —
//!   the port implementations
//! * [`FilesystemBlobStore`] — content-addressed storage for oversized messages
//!
//! ## Guarantees
//!
//! Raw message bytes round-trip **exactly**. A message is stored inline below
//! [`INLINE_THRESHOLD`] and in the blob store above it, and either way the
//! octets that come back are the octets that went in.
//!
//! UID assignment happens inside the same transaction as the entry insert, so
//! concurrent delivery into one mailbox cannot produce a duplicate UID.

#![doc(html_root_url = "https://docs.rs/sandesh-storage")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]
#![allow(clippy::module_name_repetitions)]

mod account_repo;
mod blob;
mod connection_repo;
mod db;
mod delivery_repo;
mod error;
mod events;
mod mailbox_repo;
mod message_repo;
mod metrics_repo;
mod outbox_repo;
mod parsed_repo;
mod row;
mod rule_repo;
mod search_index;
mod workspace_repo;

pub use account_repo::SqliteAccountStore;
pub use blob::{FilesystemBlobStore, INLINE_THRESHOLD, NullBlobStore, should_store_inline};
pub use db::{Database, MIGRATOR, ping};

pub use connection_repo::SqliteConnectionStore;
pub use delivery_repo::SqliteDeliveryStore;
pub use error::{StorageError, StorageResult};
pub use events::{CollectingSink, EventRecorder};
pub use mailbox_repo::SqliteMailboxStore;
pub use message_repo::SqliteMessageStore;
pub use metrics_repo::SqliteMetricsSource;
pub use outbox_repo::SqliteOutboxStore;
pub use rule_repo::SqliteRuleStore;
pub use search_index::SqliteSearchIndex;
/// The connection pool type, re-exported so the composition root can hold one
/// without depending on `sqlx` directly.
pub use sqlx::SqlitePool;
pub use workspace_repo::{
    SqliteWorkspaceStore, WorkspaceManager, WorkspaceMetadata, WorkspacesRegistry,
};
