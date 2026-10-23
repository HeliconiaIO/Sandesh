//! Workspace domain models and purge specifications.
//!
//! A workspace isolates accounts, mailboxes, messages, search index, and
//! connection traces into an independent environment.

use crate::Timestamp;

/// An isolated workspace environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// Unique slug identifier (e.g. "default", "billing-service").
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Optional description of the project or environment.
    pub description: Option<String>,
    /// When the workspace was created.
    pub created_at: Timestamp,
    /// Whether this is the currently active workspace.
    pub is_active: bool,
    /// Total count of messages stored in this workspace.
    pub message_count: u64,
    /// Total size in bytes on disk for this workspace's database and blobs.
    pub size_bytes: u64,
    /// Path to the `SQLite` database file for this workspace.
    pub database_path: String,
    /// Whether this workspace requires username/password authentication for mail operations.
    pub auth_required: bool,
}

/// The depth of data purging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PurgeMode {
    /// Clears all messages, entries, attachments, blobs, traces, and resets UID counters,
    /// but preserves accounts, mailboxes, and custom message rules.
    #[default]
    MailAndTraces,
    /// Completely resets the workspace back to virgin state, re-seeding the default
    /// catch-all account.
    FactoryReset,
}

/// Options controlling a workspace data purge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurgeOptions {
    /// Purge mode (mail only vs factory reset).
    pub mode: PurgeMode,
    /// Whether to clear protocol connection transcripts (`connections`).
    pub clear_traces: bool,
    /// Whether to clear the webhook outbox (`webhook_outbox`).
    pub clear_webhooks: bool,
}

impl Default for PurgeOptions {
    fn default() -> Self {
        Self {
            mode: PurgeMode::MailAndTraces,
            clear_traces: true,
            clear_webhooks: true,
        }
    }
}

/// The summary result returned after a purge operation completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeResult {
    /// Which workspace was purged.
    pub workspace_id: String,
    /// Number of messages deleted.
    pub deleted_messages: u64,
    /// Number of connection traces deleted.
    pub deleted_traces: u64,
    /// Disk bytes reclaimed by VACUUM and blob deletion.
    pub reclaimed_bytes: u64,
    /// When the purge completed.
    pub purged_at: Timestamp,
}

/// Initial account and address bindings to configure in a brand-new workspace when not cloning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialAccountSetup {
    /// Account username (e.g. "dev", "billing", "alice").
    pub username: String,
    /// Bound email addresses (e.g. `["billing@example.com", "invoices@example.com"]`).
    pub addresses: Vec<String>,
    /// Whether this account receives all unmatched incoming recipient addresses.
    pub is_catch_all: bool,
    /// Optional Argon2id password hash for this initial account.
    pub password_hash: Option<String>,
}

/// Updates to apply to a workspace's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceUpdate {
    /// If provided, replaces the human-readable display name.
    pub name: Option<String>,
    /// If provided, replaces (or clears) the description.
    pub description: Option<Option<String>>,
    /// If provided, changes whether SMTP/IMAP/POP3 require authentication.
    pub auth_required: Option<bool>,
}
