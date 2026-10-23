//! What the HTTP layer is allowed to reach.
//!
//! [`ApiState`] is the dependency container handlers are given. It holds the
//! domain ports, the domain services and the event bus — and nothing else. In
//! particular it holds no database handle, no connection pool and no `SQLite`
//! type: the API talks to `sandesh-core` traits and `sandesh-core` services, so
//! it cannot learn how storage works even by accident.
//! That is what makes the architecture test's "REST does not depend on
//! `sandesh-storage`" rule true by construction rather than by discipline.

use std::sync::Arc;

use sandesh_core::{
    AccountService, AccountStore, BlobStore, ConnectionStore, DeliveryStore, MailboxService,
    MailboxStore, MessageService, MessageStore, MetricsSource, SearchIndex,
};
use sandesh_events::EventBus;

/// A liveness check for a dependency the readiness endpoint reports on.
///
/// A trait rather than a database handle for the reason above: the API must be
/// able to say "the database answered" without knowing it is a database.
#[async_trait::async_trait]
pub trait ReadinessProbe: Send + Sync + std::fmt::Debug {
    /// Whether the dependency is currently usable.
    async fn is_ready(&self) -> bool;
}

// `async_trait` is what makes this usable as `dyn ReadinessProbe`: a bare
// `async fn` in a trait is not dyn compatible.

/// A probe that always reports ready. For tests and for a disabled dependency.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysReady;

#[async_trait::async_trait]
impl ReadinessProbe for AlwaysReady {
    async fn is_ready(&self) -> bool {
        true
    }
}

/// Addresses and locations reported by `/api/v1/info`.
///
/// Filled in by the composition root once listeners are actually bound, so the
/// ports reported are the ones in use — which matters when a test binds port 0.
#[derive(Debug, Clone, Default)]
pub struct ServerInfo {
    /// Where SMTP is listening, if it is.
    pub smtp_address: Option<String>,
    /// Where IMAP is listening, if it is.
    pub imap_address: Option<String>,
    /// Where POP3 is listening, if it is.
    pub pop3_address: Option<String>,
    /// Where HTTP is listening.
    pub http_address: Option<String>,
    /// Where the database lives, as a display string.
    pub database: String,
    /// The SMTP authentication posture, as a stable wire word.
    pub smtp_auth: String,
    /// The SMTP TLS posture, as a stable wire word.
    pub smtp_tls: String,
    /// The TLS certificate fingerprint, when TLS is enabled.
    pub tls_fingerprint: Option<String>,
    /// The **API** authentication posture, as a stable wire word.
    ///
    /// Distinct from `smtp_auth`, which governs SMTP alone. Reported because
    /// "is my API protected?" was previously unanswerable from the running
    /// server, and because `smtp_auth` being `required` invited exactly the
    /// wrong conclusion about the HTTP surface (ADR-075).
    pub api_auth: String,
    /// Whether the HTTP listener is serving HTTPS.
    pub api_tls: bool,
}

/// The dependencies available to every handler.
///
/// Cloning is cheap: every field is an `Arc`. Axum requires `Clone` because it
/// hands each request its own copy.
#[derive(Clone)]
pub struct ApiState {
    /// Accounts and identities.
    pub accounts: Arc<dyn AccountStore>,
    /// Mailboxes.
    pub mailboxes: Arc<dyn MailboxStore>,
    /// Messages and placements.
    pub messages: Arc<dyn MessageStore>,
    /// Delivery records.
    pub deliveries: Arc<dyn DeliveryStore>,
    /// Out-of-line raw message bytes.
    pub blobs: Arc<dyn BlobStore>,
    /// Mailbox operations (`CORE_SPEC` §6), shared with the protocol adapters.
    ///
    /// Held alongside the ports rather than instead of them: most handlers read
    /// through a port directly, and only the operations with domain rules —
    /// create, rename, delete, copy, move, discard, account creation — go
    /// through a service. Constructed once here so a handler cannot accidentally
    /// build one with a different set of ports (ADR-071).
    pub mailbox_service: Arc<MailboxService>,
    /// Placement operations (`CORE_SPEC` §6).
    pub message_service: Arc<MessageService>,
    /// Account operations (`CORE_SPEC` §4.4).
    pub account_service: Arc<AccountService>,
    /// The full-text index, used only to narrow a search before evaluating it.
    ///
    /// A **prefilter, never an authority**: `SearchQuery::matches` still decides
    /// every result, and an index that declines to narrow (or is absent
    /// entirely — `NullSearchIndex`) changes how long a search takes and
    /// nothing about what it returns (`STORAGE_DESIGN.md` §7).
    pub search_index: Arc<dyn SearchIndex>,
    /// Per-connection protocol transcripts (ADR-066).
    ///
    /// `None` when tracing is disabled, which is a real configuration and not
    /// an error: `SANDESH_TRACING_ENABLED=false` means no transcripts are
    /// recorded, and the endpoints say so with `503` rather than reporting an
    /// empty list. An empty list would be indistinguishable from "tracing is on
    /// and nothing has connected yet", which is the wrong answer to give
    /// someone debugging why their trace is missing.
    pub connections: Option<Arc<dyn ConnectionStore>>,
    /// Authentication settings, present only when authentication is enabled.
    ///
    /// `None` is "disabled" — the middleware's first check is therefore an
    /// `Option`, not a mode comparison, so "off" cannot be misread as a mode
    /// that happens to permit everything.
    pub auth: Option<crate::auth::AuthSettings>,
    /// Verifies basic-auth credentials against the accounts table.
    ///
    /// A port because `tests/architecture.rs` forbids `sandesh-api` from
    /// depending on `sandesh-proto`, where Argon2id verification lives.
    pub credentials: Option<crate::auth::Verifier>,
    /// The readings behind `/metrics` (Phase 7 A4).
    ///
    /// Not an `Option`: unlike a transcript, a count is always answerable —
    /// an instance with no mail has zero messages, which is a true reading
    /// rather than a missing one.
    pub metrics: Arc<dyn MetricsSource>,
    /// Recent log lines and the live stream (Phase 7 A6).
    ///
    /// Not an `Option`: the buffer always exists, and one that no layer is
    /// feeding is simply empty. "Nothing has been logged yet" and "capture is
    /// not installed" look identical to a reader and neither is an error —
    /// unlike a missing transcript, which means a capture that was expected
    /// and did not happen.
    pub logs: Arc<sandesh_events::LogBuffer>,
    /// The event bus the `WebSocket` subscribes to.
    pub events: Arc<EventBus>,
    /// What readiness reports on.
    pub readiness: Arc<dyn ReadinessProbe>,
    /// Reported by `/info`.
    pub info: Arc<ServerInfo>,
    /// Runtime fault rules (ADR-084), shared with the SMTP server that obeys
    /// them.
    ///
    /// Not an `Option`: an empty registry is a true reading — no faults are
    /// configured — rather than a missing capability, so there is nothing for
    /// `None` to mean that empty does not already say.
    pub faults: Arc<sandesh_core::FaultRegistry>,
    /// Webhook endpoints and the outbox (ADR-085), shared with the worker that
    /// drains it — the same table, so registering here takes effect on the
    /// next tick rather than against a store nothing sends from.
    pub outbox: Arc<dyn sandesh_core::OutboxStore>,
    /// Auto-tagging rules (ADR-087), shared with the `DeliveryService` that
    /// evaluates them — the same store, so a rule created here applies to the
    /// next delivery rather than to a set nothing reads.
    pub rules: Arc<dyn sandesh_core::RuleStore>,
    /// Workspaces and database environments.
    pub workspaces: Arc<dyn sandesh_core::WorkspaceStore>,
}

impl std::fmt::Debug for ApiState {
    /// Ports are trait objects with no useful `Debug`; report the shape instead
    /// of requiring `Debug` on every port and leaking internals into logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState")
            .field("subscribers", &self.events.subscriber_count())
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}
