//! # Sandesh application
//!
//! The composition root: the one crate permitted to know about all the others
//! (`ARCHITECTURE.md` §3). Everything here is construction, configuration,
//! lifecycle and shutdown. There is no business logic — if a mail-domain
//! decision appears in this crate, it is in the wrong place.
//!
//! ```text
//! AppConfig
//!     │
//!     ▼
//! Database ──▶ Repositories ──┐
//! BlobStore ──────────────────┤
//! EventBus ───────────────────┤
//!                             ▼
//!                        ┌─────────┐
//!                        │ Running │
//!                        └────┬────┘
//!                    ┌────────┴────────┐
//!                    ▼                 ▼
//!                  SMTP           HTTP + `WebSocket`
//! ```
//!
//! ## Startup is all-or-nothing
//!
//! [`Application::start`] validates configuration, opens storage, builds the
//! bus and services, and binds **every** enabled listener before returning. If
//! any mandatory subsystem fails, the whole start fails and everything already
//! built is dropped: a half-initialised Sandesh that accepts SMTP but has no
//! API is worse than one that refused to start and said why.
//!
//! Binding happens during `start`, not during `run`, which is what lets a
//! caller — including a test on port 0 — learn the real addresses before any
//! traffic is served, and makes a port clash a clean startup failure.
//!
//! ## SMTP and HTTP do not depend on each other
//!
//! Each can be disabled independently. SMTP knows nothing about the API and the
//! API knows nothing about SMTP; they meet only here, and only because both are
//! given the same ports and the same bus.

#![doc(html_root_url = "https://docs.rs/sandesh-app")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]

pub mod bootstrap;
mod cli;
pub mod dynamic_stores;
pub mod env;
pub mod logging;

pub use cli::run_cli;

use std::net::SocketAddr;
use std::sync::Arc;

use sandesh_api::state::{ApiState, ServerInfo};
use sandesh_api::{HttpServer, WEBSOCKET_PATH};
use sandesh_config::AppConfig;
use sandesh_core::{
    AccountStore, BlobStore, DeliveryStore, MailboxStore, MessageStore, UnknownRecipient,
};
use sandesh_events::EventBus;
use sandesh_imap::ImapServer;
use sandesh_pop3::Pop3Server;
use sandesh_smtp::{DeliveryService, SmtpServer};
use sandesh_storage::{
    Database, EventRecorder, FilesystemBlobStore, SqliteAccountStore, SqliteDeliveryStore,
    SqliteMailboxStore, SqliteMessageStore,
};
use tokio_util::sync::CancellationToken;

/// Why the application could not start or run.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The configuration is unusable. Reported before anything is opened.
    #[error("configuration is invalid: {0}")]
    Config(#[from] sandesh_config::ConfigError),

    /// Storage could not be opened or migrated.
    #[error("cannot open storage at {path}: {source}")]
    Storage {
        /// What was being opened.
        path: String,
        /// The underlying cause.
        source: sandesh_storage::StorageError,
    },

    /// First-run setup failed.
    #[error("first-run setup failed: {0}")]
    Bootstrap(String),

    /// The SMTP listener could not start.
    #[error("SMTP server failed to start: {0}")]
    Smtp(#[from] sandesh_smtp::ServerError),

    /// The IMAP server could not start or failed while running.
    #[error(transparent)]
    Imap(#[from] sandesh_imap::ServerError),

    /// The POP3 server could not start or failed while running.
    #[error(transparent)]
    Pop3(#[from] sandesh_pop3::ServerError),

    /// The HTTP listener could not start.
    #[error("HTTP server failed to start: {0}")]
    Http(#[from] sandesh_api::ServerError),
}

/// A minimal alias so this module names no `sqlx` type in its public surface.
mod sqlx_pool {
    /// The connection pool type, kept local to the composition root.
    pub type Pool = sandesh_storage::SqlitePool;
}

/// The connection-transcript store, when tracing is enabled.
///
/// Built only when it will be used, so a disabled instance holds no handle
/// that could start recording by accident.
fn connection_store(
    config: &AppConfig,
    database: &Database,
) -> Option<Arc<dyn sandesh_core::ConnectionStore>> {
    config.tracing_enabled.then(|| {
        Arc::new(sandesh_storage::SqliteConnectionStore::new(
            database.pool().clone(),
        )) as Arc<dyn sandesh_core::ConnectionStore>
    })
}

/// Builds a connection recorder for one protocol.
///
/// Returns a disabled recorder when tracing is off, so the servers take the
/// same code path either way and "tracing disabled" cannot mean "half wired".
fn recorder_for(
    config: &AppConfig,
    protocol: sandesh_core::Protocol,
    connections: Option<&Arc<dyn sandesh_core::ConnectionStore>>,
) -> sandesh_proto::ConnectionRecorder {
    match connections {
        Some(store) => sandesh_proto::ConnectionRecorder::new(protocol, Arc::clone(store))
            .with_retention(config.trace_retention),
        None => sandesh_proto::ConnectionRecorder::disabled(protocol),
    }
}

/// Binds the HTTP listener, recording the address it actually got.
///
/// Bound before the API state is built so `/info` can report the real address —
/// which with port 0 is only known after binding, and is exactly what a test or
/// a developer needs to see.
async fn bind_http_listener(
    config: &AppConfig,
    addresses: &mut Addresses,
) -> Result<Option<tokio::net::TcpListener>, AppError> {
    if !config.http_enabled {
        return Ok(None);
    }

    let listener = HttpServer::bind_listener(config.http.bind)
        .await
        .map_err(AppError::Http)?;
    // The bound address is authoritative; the configured one is only a fallback
    // for the impossible case of a socket with no address.
    addresses.http = Some(listener.local_addr().unwrap_or(config.http.bind));
    Ok(Some(listener))
}

/// Binds the SMTP listener, supplying the TLS acceptor when configured.
///
/// The acceptor is what makes `STARTTLS` honourable: without one the session
/// refuses the upgrade rather than advertising something it cannot complete.
async fn bind_smtp(
    config: &AppConfig,
    service: Arc<DeliveryService>,
    tls: Option<&TlsSetup>,
    recorder: sandesh_proto::ConnectionRecorder,
    faults: Arc<sandesh_core::FaultRegistry>,
) -> Result<sandesh_smtp::BoundServer, AppError> {
    let server = SmtpServer::new(config.smtp.clone(), service)
        .with_tracing(recorder)
        .with_faults(faults);
    let server = match tls {
        Some(setup) => server.with_tls(setup.acceptor.clone()),
        None => server,
    };

    let (bound, warnings) = server.bind().await?;
    for warning in warnings {
        tracing::warn!("{warning}");
    }

    Ok(bound)
}

/// Binds the IMAP listeners, supplying the TLS acceptor when configured.
///
/// The acceptor is what makes `STARTTLS` and the implicit-TLS port honourable:
/// without one, `ImapServer::bind` refuses to start rather than serving a port
/// the operator believes is encrypted.
async fn bind_imap(
    config: &AppConfig,
    stores: &MailStores,
    tls: Option<&TlsSetup>,
    recorder: sandesh_proto::ConnectionRecorder,
) -> Result<sandesh_imap::BoundServer, AppError> {
    let server = ImapServer::new(config.imap.clone(), Arc::clone(&stores.accounts))
        .with_stores(Arc::clone(&stores.mailboxes), Arc::clone(&stores.messages))
        .with_events(Arc::clone(&stores.events))
        .with_search_index(Arc::clone(&stores.search_index))
        .with_tracing(recorder);
    let server = match tls {
        Some(setup) => server.with_tls(setup.acceptor.clone()),
        None => server,
    };

    let (bound, warnings) = server.bind().await?;
    for warning in warnings {
        tracing::warn!("{warning}");
    }

    Ok(bound)
}

/// Binds the POP3 listeners, supplying the TLS acceptor when configured.
async fn bind_pop3(
    config: &AppConfig,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Arc<dyn MailboxStore>,
    messages: Arc<dyn MessageStore>,
    tls: Option<&TlsSetup>,
    recorder: sandesh_proto::ConnectionRecorder,
) -> Result<sandesh_pop3::BoundServer, AppError> {
    let server =
        Pop3Server::new(config.pop3.clone(), accounts, mailboxes, messages).with_tracing(recorder);
    let server = match tls {
        Some(setup) => server.with_tls(setup.acceptor.clone()),
        None => server,
    };

    let (bound, warnings) = server.bind().await?;
    for warning in warnings {
        tracing::warn!("{warning}");
    }

    Ok(bound)
}

/// The SQLite-backed repositories, as the ports the rest of the tree sees.
///
/// Grouped so the composition root names one value: the four are always built
/// together over the same pool and the same event recorder, and constructing
/// one against a different recorder would silently stop emitting events.
#[derive(Clone)]
pub(crate) struct Repositories {
    pub accounts: Arc<dyn AccountStore>,
    pub mailboxes: Arc<dyn MailboxStore>,
    pub messages: Arc<dyn MessageStore>,
    pub deliveries: Arc<dyn DeliveryStore>,
    /// The full-text index. Built here like any other port so every surface
    /// shares one instance, and so swapping in `NullSearchIndex` is a one-line
    /// change at the composition root rather than an edit to each caller.
    pub search_index: Arc<dyn sandesh_core::SearchIndex>,
    /// Auto-tagging rules (ADR-087). One instance, shared by the API that
    /// configures them and the `DeliveryService` that applies them.
    pub rules: Arc<dyn sandesh_core::RuleStore>,
}

impl Repositories {
    /// Builds every repository over one pool, blob store and recorder.
    pub fn new(
        pool: &sqlx_pool::Pool,
        blobs: &Arc<dyn BlobStore>,
        recorder: EventRecorder,
    ) -> Self {
        Self {
            accounts: Arc::new(SqliteAccountStore::new(pool.clone(), recorder.clone())),
            mailboxes: Arc::new(SqliteMailboxStore::new(pool.clone(), recorder.clone())),
            messages: Arc::new(SqliteMessageStore::new(
                pool.clone(),
                Arc::clone(blobs),
                recorder.clone(),
            )),
            deliveries: Arc::new(SqliteDeliveryStore::new(pool.clone(), recorder)),
            search_index: Arc::new(sandesh_storage::SqliteSearchIndex::new(pool.clone())),
            rules: Arc::new(sandesh_storage::SqliteRuleStore::new(pool.clone())),
        }
    }
}

/// Indexes, in the background, every stored message the search index lacks.
///
/// Run once per workspace database as it opens. In steady state there is
/// nothing to do and this is one query; after migration 0010 emptied the index,
/// it rebuilds every row under the text-only rule. Batches keep each pass's memory bounded, and a failure stops the task
/// rather than spinning: search stays correct without the index, only slower.
pub(crate) fn spawn_index_repair(pool: &sqlx_pool::Pool, repos: &Repositories) {
    const BATCH: u32 = 200;
    let index = sandesh_storage::SqliteSearchIndex::new(pool.clone());
    let messages = Arc::clone(&repos.messages);
    let search_index = Arc::clone(&repos.search_index);
    tokio::spawn(async move {
        let mut repaired = 0usize;
        loop {
            let batch = match index.unindexed(BATCH).await {
                Ok(batch) => batch,
                Err(error) => {
                    tracing::warn!(%error, "search index repair stopped");
                    return;
                }
            };
            if batch.is_empty() {
                break;
            }
            for id in batch {
                let message = match messages.get(id).await {
                    Ok(Some(message)) => message,
                    // Deleted between listing and loading: nothing to index.
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(%error, "search index repair stopped");
                        return;
                    }
                };
                if let Err(error) = search_index.index(&message).await {
                    tracing::warn!(%error, "search index repair stopped");
                    return;
                }
                repaired += 1;
            }
        }
        if repaired > 0 {
            tracing::info!(repaired, "search index repaired");
        }
    });
}

/// The domain services, built once over the ports.
///
/// Grouped so that `Application::start` names one value rather than three, and
/// so every surface shares one instance: a handler that assembled its own could
/// do it over a different set of ports, which is the drift the service layer
/// exists to prevent (ADR-071).
struct DomainServices {
    accounts: Arc<sandesh_core::AccountService>,
    mailboxes: Arc<sandesh_core::MailboxService>,
    messages: Arc<sandesh_core::MessageService>,
}

impl DomainServices {
    /// Builds each service over the ports it needs.
    fn new(
        accounts: &Arc<dyn AccountStore>,
        mailboxes: &Arc<dyn MailboxStore>,
        messages: &Arc<dyn MessageStore>,
    ) -> Self {
        Self {
            accounts: Arc::new(sandesh_core::AccountService::new(Arc::clone(accounts))),
            mailboxes: Arc::new(sandesh_core::MailboxService::new(Arc::clone(mailboxes))),
            messages: Arc::new(sandesh_core::MessageService::new(
                Arc::clone(messages),
                Arc::clone(mailboxes),
            )),
        }
    }
}

/// Converts the configured routing policy into the domain's.
///
/// `sandesh-config` and `sandesh-core` each name these three outcomes, because
/// `CORE_SPEC.md` §7 states the routing rules in terms of them while
/// `ARCHITECTURE.md` §3.2 forbids the core a configuration dependency and
/// `tests/architecture.rs` forbids config a dependency on the core (ADR-071).
/// This is the one place that sees both, which is what a composition root is
/// for. The `match` is exhaustive, so a variant added on either side fails to
/// compile here rather than drifting silently.
const fn routing_policy(policy: sandesh_config::UnknownRecipientPolicy) -> UnknownRecipient {
    match policy {
        sandesh_config::UnknownRecipientPolicy::CatchAll => UnknownRecipient::CatchAll,
        sandesh_config::UnknownRecipientPolicy::AutoCreate => UnknownRecipient::AutoCreate,
        sandesh_config::UnknownRecipientPolicy::Strict => UnknownRecipient::Strict,
    }
}

/// The stores the mail protocol listeners need.
///
/// Grouped so that adding a protocol does not add four more parameters to the
/// binding helper.
struct MailStores {
    accounts: Arc<dyn AccountStore>,
    mailboxes: Arc<dyn MailboxStore>,
    messages: Arc<dyn MessageStore>,
    deliveries: Arc<dyn DeliveryStore>,
    /// Per-connection transcripts (ADR-066). `None` when tracing is disabled.
    connections: Option<Arc<dyn sandesh_core::ConnectionStore>>,
    /// The live event stream IMAP `IDLE` reports from.
    ///
    /// Held as the port, not as the bus: this is the composition root, so it is
    /// the one place that knows the two are the same object.
    events: Arc<dyn sandesh_core::EventSource>,
    /// The full-text index IMAP `SEARCH` narrows with. Never changes a result.
    search_index: Arc<dyn sandesh_core::SearchIndex>,
    /// Auto-tagging rules, evaluated during delivery (ADR-087).
    rules: Arc<dyn sandesh_core::RuleStore>,
    /// Workspaces management port.
    workspaces: Option<Arc<dyn sandesh_core::WorkspaceStore>>,
}

impl MailStores {
    /// Gathers the ports the mail listeners need.
    ///
    /// A constructor rather than a literal at the call site: every field is an
    /// `Arc::clone`, and spelling seven of them inline told the reader nothing
    /// the type does not already say.
    fn new(
        repos: &Repositories,
        events: &Arc<EventBus>,
        connections: Option<Arc<dyn sandesh_core::ConnectionStore>>,
        workspaces: Option<Arc<dyn sandesh_core::WorkspaceStore>>,
    ) -> Self {
        Self {
            accounts: Arc::clone(&repos.accounts),
            mailboxes: Arc::clone(&repos.mailboxes),
            messages: Arc::clone(&repos.messages),
            deliveries: Arc::clone(&repos.deliveries),
            search_index: Arc::clone(&repos.search_index),
            rules: Arc::clone(&repos.rules),
            events: Arc::clone(events) as Arc<dyn sandesh_core::EventSource>,
            connections,
            workspaces,
        }
    }
}

/// The bound mail listeners.
struct MailListeners {
    smtp: Option<sandesh_smtp::BoundServer>,
    imap: Option<sandesh_imap::BoundServer>,
    pop3: Option<sandesh_pop3::BoundServer>,
}

/// Binds the SMTP, IMAP and POP3 listeners, recording the addresses they actually got.
///
/// All are bound before anything is served, so a port clash fails startup
/// rather than surfacing on a client's first connection. The addresses are
/// written back because with port 0 the real port is only known after binding —
/// which is what a test, and the banner, need to report.
async fn bind_mail_listeners(
    config: &AppConfig,
    stores: &MailStores,
    tls: Option<&TlsSetup>,
    addresses: &mut Addresses,
    faults: Arc<sandesh_core::FaultRegistry>,
) -> Result<MailListeners, AppError> {
    let smtp = if config.smtp_enabled {
        let mut service = DeliveryService::new(
            Arc::clone(&stores.accounts),
            Arc::clone(&stores.mailboxes),
            Arc::clone(&stores.messages),
            Arc::clone(&stores.deliveries),
            routing_policy(config.smtp.unknown_recipient),
        )
        // The *same* store the API writes to, so a rule created over HTTP
        // applies to the next delivery rather than to a set nothing reads.
        .with_rules(Arc::clone(&stores.rules));

        if let Some(ws) = &stores.workspaces {
            service = service.with_workspaces(Arc::clone(ws));
        }

        let service = Arc::new(service);

        let bound = bind_smtp(
            config,
            service,
            tls,
            recorder_for(
                config,
                sandesh_core::Protocol::Smtp,
                stores.connections.as_ref(),
            ),
            faults,
        )
        .await?;
        addresses.smtp = Some(bound.local_addr());
        Some(bound)
    } else {
        None
    };

    let imap = if config.imap_enabled {
        let bound = bind_imap(
            config,
            stores,
            tls,
            recorder_for(
                config,
                sandesh_core::Protocol::Imap,
                stores.connections.as_ref(),
            ),
        )
        .await?;
        addresses.imap = bound.local_addr();
        addresses.imaps = bound.tls_addr();
        Some(bound)
    } else {
        None
    };

    let pop3 = if config.pop3_enabled {
        let bound = bind_pop3(
            config,
            Arc::clone(&stores.accounts),
            Arc::clone(&stores.mailboxes),
            Arc::clone(&stores.messages),
            tls,
            recorder_for(
                config,
                sandesh_core::Protocol::Pop3,
                stores.connections.as_ref(),
            ),
        )
        .await?;
        let status = bound.status();
        addresses.pop3 = status.local_addr;
        addresses.pop3s = status.tls_addr;
        Some(bound)
    } else {
        None
    };

    Ok(MailListeners { smtp, imap, pop3 })
}

/// The `AUTH` posture as a stable wire word.
///
/// Written out rather than derived from `Debug` so a Rust rename cannot change
/// the API contract (`API_DESIGN.md` §2).
const fn auth_posture(mode: sandesh_config::AuthMode) -> &'static str {
    match mode {
        sandesh_config::AuthMode::Disabled => "disabled",
        sandesh_config::AuthMode::Optional => "optional",
        sandesh_config::AuthMode::Required => "required",
    }
}

/// The TLS posture as a stable wire word.
const fn tls_posture(mode: sandesh_config::TlsMode) -> &'static str {
    match mode {
        sandesh_config::TlsMode::Disabled => "disabled",
        sandesh_config::TlsMode::StartTls => "starttls",
        sandesh_config::TlsMode::Implicit => "implicit",
    }
}

/// The TLS material a listener needs, plus its fingerprint for reporting.
/// Assembles what `/info` reports.
///
/// Extracted from `start` so the posture fields sit together and can be read
/// as a set: an operator asking "what is exposed and how is it protected?"
/// should find one place that answers it.
fn server_info(
    config: &AppConfig,
    addresses: &Addresses,
    tls_fingerprint: Option<String>,
) -> ServerInfo {
    ServerInfo {
        smtp_address: addresses.smtp.map(|addr| addr.to_string()),
        imap_address: addresses.imap.map(|addr| addr.to_string()),
        pop3_address: addresses.pop3.map(|addr| addr.to_string()),
        http_address: addresses.http.map(|addr| addr.to_string()),
        database: config.database_path.display().to_string(),
        // Reported so the UI can state the posture plainly rather than leaving
        // a developer to guess whether their local mail server accepts
        // anonymous mail (it does, by default).
        smtp_auth: String::from(auth_posture(config.smtp.auth_mode)),
        smtp_tls: String::from(tls_posture(config.smtp.tls_mode)),
        tls_fingerprint,
        // Separate from `smtp_auth`, which never governed the HTTP surface.
        api_auth: String::from(config.http.auth_mode.as_str()),
        api_tls: config.http.tls_enabled,
    }
}

/// Builds the HTTP server, attaching TLS when configured.
///
/// Extracted from `start` so the HTTPS decision reads as one thing, and so the
/// "configured but unavailable" case is impossible to skim past.
fn build_http_server(
    listener: tokio::net::TcpListener,
    config: &AppConfig,
    state: sandesh_api::ApiState,
    tls: Option<&TlsSetup>,
) -> Result<HttpServer, AppError> {
    let server = HttpServer::from_listener(listener, &config.http, state)?;

    // The same certificate the mail listeners use (ADR-034): one identity per
    // process, so enabling HTTPS needs no second certificate.
    match (config.http.tls_enabled, tls) {
        (true, Some(setup)) => Ok(server.with_tls(setup.acceptor.clone())),
        (true, None) => {
            // `load_tls` counts the HTTP listener, so reaching here means the
            // identity could not be built. Failing beats serving plaintext on a
            // port the operator believes is encrypted — the rule Phase 4.5
            // already applied to SMTP (`ServerError::MissingTlsAcceptor`).
            Err(AppError::Bootstrap(String::from(
                "SANDESH_API_TLS is set but no TLS identity could be loaded",
            )))
        }
        (false, _) => Ok(server),
    }
}

/// Verifies API basic-auth credentials against the accounts table.
///
/// Lives here because this is the only crate permitted to see both sides:
/// `sandesh-core` declares the port and `sandesh-proto` owns the Argon2id
/// verification, and `tests/architecture.rs` forbids `sandesh-api` from
/// reaching the latter (§59). Reimplementing verification in the API would have
/// meant two password-checking paths, which is how one of them ends up without
/// the timing-safety property the other has.
struct AccountCredentialVerifier {
    accounts: Arc<dyn AccountStore>,
}

#[async_trait::async_trait]
impl sandesh_core::CredentialVerifier for AccountCredentialVerifier {
    async fn verify(&self, username: &str, password: &str) -> bool {
        let stored = match self.accounts.get_by_username(username).await {
            Ok(account) => account.and_then(|account| account.credential.map(|c| c.hash)),
            Err(error) => {
                tracing::error!(%error, "cannot read the account for API authentication");
                None
            }
        };

        // Verified even when the account does not exist, so both paths take the
        // same time (ADR-032). `verify_optional` owns that property; passing
        // `None` through rather than returning early is what preserves it.
        sandesh_proto::verify_optional(&sandesh_proto::Secret::new(password), stored.as_deref())
    }

    fn hash(&self, password: &str) -> Result<String, String> {
        sandesh_proto::hash_password(&sandesh_proto::Secret::new(password))
            .map_err(|error| error.to_string())
    }
}

struct TlsSetup {
    acceptor: tokio_rustls::TlsAcceptor,
    fingerprint: Option<String>,
}

/// Loads or generates the TLS identity.
///
/// Done before any listener binds, so a broken certificate fails startup
/// rather than surfacing on the first client handshake. The certificate lives
/// beside the database (ADR-031) and is generated on first use, because nobody
/// has a CA-signed certificate for `localhost` (ADR-034).
///
/// Synchronous, and deliberately so despite being called from an async
/// `start`: this is one-time startup work that runs before a single connection
/// is accepted, so there is no request latency to block and no task to starve.
/// Making it async would mean `spawn_blocking` for two small file reads that
/// happen once per process — cost without benefit. Nothing on a *connection*
/// path performs file I/O; the acceptor is cloned from memory thereafter.
fn load_tls(config: &AppConfig) -> Result<Option<TlsSetup>, AppError> {
    // Either protocol needing TLS is reason enough to load the identity: one
    // certificate serves all listeners, so this must not key off SMTP alone
    // or an IMAP/POP3-only TLS configuration would fail to start.
    let smtp_needs_tls = config.smtp_enabled && config.smtp.tls_mode.is_enabled();
    let imap_needs_tls = config.imap_enabled && config.imap.needs_tls();
    let pop3_needs_tls = config.pop3_enabled && config.pop3.needs_tls();
    // The HTTP listener counts too (ADR-075). Omitting it here is exactly the
    // bug this comment warns about, one listener along: an HTTPS-only
    // configuration would load no identity and then be handed no acceptor.
    let http_needs_tls = config.http_enabled && config.http.tls_enabled;
    if !smtp_needs_tls && !imap_needs_tls && !pop3_needs_tls && !http_needs_tls {
        return Ok(None);
    }

    let data_dir = config.database_path.parent().map_or_else(
        || std::path::PathBuf::from("."),
        std::path::Path::to_path_buf,
    );

    let identity = sandesh_proto::Identity::load_or_generate(&data_dir)
        .map_err(|error| AppError::Bootstrap(error.to_string()))?;

    // Read before the identity is consumed by `acceptor()`.
    let fingerprint = identity.fingerprint();

    let acceptor = identity
        .acceptor()
        .map_err(|error| AppError::Bootstrap(error.to_string()))?;

    Ok(Some(TlsSetup {
        acceptor,
        fingerprint,
    }))
}

/// Creates the directory a database path lives in.
///
/// The blob store already creates its own root, and the asymmetry only became
/// visible once the paths were relocatable (OD-24): pointing
/// `SANDESH_DATA_DIR` at a fresh directory — the obvious first use — otherwise
/// failed with `SQLite` error 14, which says "unable to open database file" and
/// says nothing about a missing directory.
///
/// A path with no parent, or one whose parent is the current directory, needs
/// nothing created.
async fn ensure_data_directory(database_path: &std::path::Path) -> Result<(), AppError> {
    let Some(parent) = database_path.parent() else {
        return Ok(());
    };

    if parent.as_os_str().is_empty() {
        return Ok(());
    }

    tokio::fs::create_dir_all(parent).await.map_err(|source| {
        AppError::Bootstrap(format!(
            "cannot create the data directory {}: {source}",
            parent.display()
        ))
    })
}

/// `WorkspaceStore` wrapper that evicts the cached `WorkspaceBundle` from the
/// `DynamicStorageRouter` after a workspace is deleted or purged.
///
/// Without this, the router keeps a now-stale bundle in memory indefinitely:
/// - after `delete`, the bundle points at a removed database file.
/// - after `purge`, the open pool holds WAL state that predates the VACUUM,
///   so subsequent bundle users would not see the reclaimed-space state.
///
/// This is the composition root's only responsibility that the storage crate
/// cannot own: the router lives in `sandesh-app`, the store in
/// `sandesh-storage`, and the architecture forbids the latter seeing the former.
struct RouterAwareWorkspaceStore {
    inner: sandesh_storage::SqliteWorkspaceStore,
    router: Arc<dynamic_stores::DynamicStorageRouter>,
}

impl std::fmt::Debug for RouterAwareWorkspaceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterAwareWorkspaceStore")
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl sandesh_core::WorkspaceStore for RouterAwareWorkspaceStore {
    async fn list(&self) -> sandesh_core::CoreResult<Vec<sandesh_core::Workspace>> {
        self.inner.list().await
    }

    async fn get(&self, id: &str) -> sandesh_core::CoreResult<Option<sandesh_core::Workspace>> {
        self.inner.get(id).await
    }

    async fn active_id(&self) -> sandesh_core::CoreResult<String> {
        self.inner.active_id().await
    }

    async fn create(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        clone_from: Option<&str>,
        initial_account: Option<sandesh_core::InitialAccountSetup>,
        auth_required: bool,
    ) -> sandesh_core::CoreResult<sandesh_core::Workspace> {
        self.inner
            .create(
                id,
                name,
                description,
                clone_from,
                initial_account,
                auth_required,
            )
            .await
    }

    async fn update(
        &self,
        id: &str,
        update: sandesh_core::WorkspaceUpdate,
    ) -> sandesh_core::CoreResult<sandesh_core::Workspace> {
        self.inner.update(id, update).await
    }

    async fn set_active(&self, id: &str) -> sandesh_core::CoreResult<()> {
        self.inner.set_active(id).await
    }

    /// Delegates to the inner store and then evicts the bundle from the router
    /// so the next request opens a fresh pool rather than using the now-deleted
    /// workspace's cached one.
    async fn delete(&self, id: &str) -> sandesh_core::CoreResult<bool> {
        let deleted = self.inner.delete(id).await?;
        if deleted {
            self.router.invalidate(id).await;
            tracing::debug!(
                workspace_id = id,
                "router bundle evicted after workspace delete"
            );
        }
        Ok(deleted)
    }

    /// Delegates to the inner store and then evicts the bundle from the router
    /// so the next bundle open uses a fresh pool that sees the post-VACUUM state.
    async fn purge(
        &self,
        id: &str,
        options: sandesh_core::PurgeOptions,
    ) -> sandesh_core::CoreResult<sandesh_core::PurgeResult> {
        let result = self.inner.purge(id, options).await?;
        self.router.invalidate(id).await;
        tracing::debug!(
            workspace_id = id,
            "router bundle evicted after workspace purge"
        );
        Ok(result)
    }

    async fn checkpoint(&self, id: &str) -> sandesh_core::CoreResult<()> {
        self.inner.checkpoint(id).await
    }
}

/// The addresses a running application is listening on.
#[derive(Debug, Clone, Default)]
pub struct Addresses {
    /// Where SMTP is bound, if enabled.
    pub smtp: Option<SocketAddr>,
    /// The IMAP cleartext address, when the listener runs.
    pub imap: Option<SocketAddr>,
    /// The IMAP implicit-TLS address, when that listener runs.
    pub imaps: Option<SocketAddr>,
    /// The POP3 cleartext address, when the listener runs.
    pub pop3: Option<SocketAddr>,
    /// The POP3 implicit-TLS address, when that listener runs.
    pub pop3s: Option<SocketAddr>,
    /// Where HTTP is bound, if enabled.
    pub http: Option<SocketAddr>,
}

/// A started, fully-bound Sandesh.
///
/// Every listener is already bound when this exists, so [`addresses`] reports
/// real ports and [`run`](Self::run) cannot fail to bind.
///
/// [`addresses`]: Self::addresses
pub struct Application {
    config: AppConfig,
    database: Database,
    events: Arc<EventBus>,
    smtp: Option<sandesh_smtp::BoundServer>,
    imap: Option<sandesh_imap::BoundServer>,
    pop3: Option<sandesh_pop3::BoundServer>,
    http: Option<HttpServer>,
    addresses: Addresses,
    shutdown: CancellationToken,
    /// The TLS certificate fingerprint, for the banner and `/info`.
    tls_fingerprint: Option<String>,
    /// Drained by the webhook worker, which `run` starts (ADR-086).
    outbox: Arc<dyn sandesh_core::OutboxStore>,
}

impl std::fmt::Debug for Application {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Application")
            .field("addresses", &self.addresses)
            .field("subscribers", &self.events.subscriber_count())
            .finish_non_exhaustive()
    }
}

impl Application {
    /// Builds and binds the application.
    ///
    /// Ordering is deliberate: configuration is validated first so an unusable
    /// setting costs nothing, then storage, then the bus, then services, then
    /// listeners. Nothing is bound until everything it depends on exists.
    ///
    /// # Errors
    /// [`AppError`] if configuration is invalid, storage cannot be opened, or a
    /// listener cannot bind. On any failure nothing is left running.
    #[allow(
        clippy::too_many_lines,
        reason = "the startup sequence is ordered and reads best in one place"
    )]
    pub async fn start(config: AppConfig) -> Result<Self, AppError> {
        // 1. Configuration. Cheapest thing to reject, so reject it first.
        config.validate()?;

        for warning in config.warnings() {
            tracing::warn!("{warning}");
        }

        // 2. Storage & Workspaces.
        ensure_data_directory(&config.database_path).await?;

        let data_dir = config.database_path.parent().map_or_else(
            || std::path::PathBuf::from("."),
            std::path::Path::to_path_buf,
        );

        let database = Database::open(&config.database_path)
            .await
            .map_err(|source| AppError::Storage {
                path: config.database_path.display().to_string(),
                source,
            })?;

        let blobs: Arc<dyn BlobStore> = Arc::new(
            FilesystemBlobStore::new(&config.blob_path)
                .await
                .map_err(|source| AppError::Storage {
                    path: config.blob_path.display().to_string(),
                    source,
                })?,
        );

        // 3. The event bus, before any service, so every service can emit.
        let events = Arc::new(EventBus::with_capacity(config.http.websocket_queue));
        let recorder = EventRecorder::new(Arc::clone(&events) as Arc<dyn sandesh_core::EventSink>);

        // Workspace manager and dynamic storage router
        let ws_manager = Arc::new(
            sandesh_storage::WorkspaceManager::load_or_init(
                data_dir,
                config.database_path.clone(),
                config.blob_path.clone(),
                recorder.clone(),
            )
            .await
            .map_err(|source| {
                AppError::Bootstrap(format!("cannot init workspace manager: {source}"))
            })?,
        );
        ws_manager.register_pool("default", database.clone()).await;

        let router = Arc::new(dynamic_stores::DynamicStorageRouter::new(
            Arc::clone(&ws_manager),
            Arc::clone(&events),
            recorder.clone(),
            config.tracing_enabled,
            config.trace_retention,
        ));

        // 4. Default repositories over the pool.
        let pool = database.pool().clone();
        let default_repos = Repositories::new(&pool, &blobs, recorder);
        spawn_index_repair(&pool, &default_repos);
        let default_connections = connection_store(&config, &database);
        let default_outbox: Arc<dyn sandesh_core::OutboxStore> =
            Arc::new(sandesh_storage::SqliteOutboxStore::new(pool.clone()));
        let default_metrics: Arc<dyn sandesh_core::MetricsSource> =
            Arc::new(sandesh_storage::SqliteMetricsSource::new(pool));

        router
            .register_bundle(
                "default",
                Arc::new(dynamic_stores::WorkspaceBundle {
                    database: database.clone(),
                    blobs: Arc::clone(&blobs),
                    repos: default_repos,
                    connections: default_connections,
                    outbox: Arc::clone(&default_outbox),
                    metrics: Arc::clone(&default_metrics),
                }),
            )
            .await;

        // Dynamic stores delegating to whichever workspace is active
        let accounts: Arc<dyn AccountStore> = Arc::new(dynamic_stores::DynamicAccountStore::new(
            Arc::clone(&router),
        ));
        let mailboxes: Arc<dyn MailboxStore> = Arc::new(dynamic_stores::DynamicMailboxStore::new(
            Arc::clone(&router),
        ));
        let messages: Arc<dyn MessageStore> = Arc::new(dynamic_stores::DynamicMessageStore::new(
            Arc::clone(&router),
        ));
        let deliveries: Arc<dyn DeliveryStore> = Arc::new(
            dynamic_stores::DynamicDeliveryStore::new(Arc::clone(&router)),
        );
        let search_index: Arc<dyn sandesh_core::SearchIndex> =
            Arc::new(dynamic_stores::DynamicSearchIndex::new(Arc::clone(&router)));
        let rules: Arc<dyn sandesh_core::RuleStore> =
            Arc::new(dynamic_stores::DynamicRuleStore::new(Arc::clone(&router)));
        let outbox: Arc<dyn sandesh_core::OutboxStore> =
            Arc::new(dynamic_stores::DynamicOutboxStore::new(Arc::clone(&router)));
        let metrics: Arc<dyn sandesh_core::MetricsSource> = Arc::new(
            dynamic_stores::DynamicMetricsSource::new(Arc::clone(&router)),
        );
        let dynamic_blobs: Arc<dyn BlobStore> =
            Arc::new(dynamic_stores::DynamicBlobStore::new(Arc::clone(&router)));
        let connections = config.tracing_enabled.then(|| {
            Arc::new(dynamic_stores::DynamicConnectionStore::new(Arc::clone(
                &router,
            ))) as Arc<dyn sandesh_core::ConnectionStore>
        });

        let dynamic_repos = Repositories {
            accounts: Arc::clone(&accounts),
            mailboxes: Arc::clone(&mailboxes),
            messages: Arc::clone(&messages),
            deliveries: Arc::clone(&deliveries),
            search_index: Arc::clone(&search_index),
            rules: Arc::clone(&rules),
        };

        // 5. The domain services, over the dynamic ports.
        let services = DomainServices::new(&accounts, &mailboxes, &messages);

        // 6. A catch-all account, so a fresh instance accepts mail immediately.
        bootstrap::ensure_catch_all(Arc::clone(&services.accounts)).await?;

        // 7. The TLS identity, when TLS is configured (ADR-034).
        let tls = load_tls(&config)?;
        let tls_fingerprint = tls.as_ref().and_then(|setup| setup.fingerprint.clone());

        // 8. Listeners. Bound now so the real ports are known before serving.
        let shutdown = CancellationToken::new();
        let mut addresses = Addresses::default();

        let faults = Arc::new(sandesh_core::FaultRegistry::new());

        let workspace_store: Arc<dyn sandesh_core::WorkspaceStore> =
            Arc::new(RouterAwareWorkspaceStore {
                inner: sandesh_storage::SqliteWorkspaceStore::new(Arc::clone(&ws_manager)),
                router: Arc::clone(&router),
            });

        let MailListeners { smtp, imap, pop3 } = bind_mail_listeners(
            &config,
            &MailStores::new(
                &dynamic_repos,
                &events,
                connections.clone(),
                Some(Arc::clone(&workspace_store)),
            ),
            tls.as_ref(),
            &mut addresses,
            Arc::clone(&faults),
        )
        .await?;

        let http_listener = bind_http_listener(&config, &mut addresses).await?;

        let http = if let Some(listener) = http_listener {
            let credentials = Some(Arc::new(AccountCredentialVerifier {
                accounts: Arc::clone(&accounts),
            }) as Arc<dyn sandesh_core::CredentialVerifier>);

            let state =
                ApiState {
                    accounts,
                    mailboxes,
                    messages,
                    deliveries,
                    blobs: dynamic_blobs,
                    mailbox_service: services.mailboxes,
                    message_service: services.messages,
                    account_service: services.accounts,
                    // `None` when authentication is disabled, so the middleware
                    // cannot mistake a mode for permission.
                    auth: config.http.auth_mode.is_enabled().then(|| {
                        sandesh_api::auth::AuthSettings {
                            token: config.http.auth_token.clone(),
                        }
                    }),
                    credentials,
                    // The same store the protocol recorders write to, so the
                    // API reads exactly what was captured. `None` when tracing
                    // is off, which the endpoints report as 503.
                    connections: connections.clone(),
                    search_index,
                    // Reads the same database every other port reads, so a
                    // scrape reports the state the API would report (A4).
                    metrics,
                    // The same buffer `logging::init` installed its capture
                    // layer against, so the endpoint serves the log this
                    // process is actually writing (A6).
                    logs: crate::logging::buffer(),
                    events: Arc::clone(&events),
                    readiness: Arc::new(dynamic_stores::DynamicReadinessProbe::new(Arc::clone(
                        &router,
                    ))),
                    info: Arc::new(server_info(&config, &addresses, tls_fingerprint.clone())),
                    // The *same* registry the SMTP server consults, so a rule
                    // set here takes effect on the next command (ADR-084). Two
                    // registries would let this endpoint mutate one nothing
                    // reads — success reported, nothing done.
                    faults: Arc::clone(&faults),
                    // The *same* store the worker drains, for the reason the
                    // faults comment above gives: a second one would let this
                    // endpoint register an endpoint nothing ever sends to.
                    outbox: Arc::clone(&outbox),
                    rules: Arc::clone(&rules),
                    workspaces: workspace_store,
                };
            Some(build_http_server(listener, &config, state, tls.as_ref())?)
        } else {
            None
        };

        Ok(Self {
            config,
            database,
            events,
            smtp,
            imap,
            pop3,
            http,
            addresses,
            shutdown,
            tls_fingerprint,
            outbox,
        })
    }

    /// The addresses actually bound.
    #[must_use]
    pub const fn addresses(&self) -> &Addresses {
        &self.addresses
    }

    /// The event bus, for a caller that wants to observe events directly.
    #[must_use]
    pub fn events(&self) -> &Arc<EventBus> {
        &self.events
    }

    /// A token that stops the application when cancelled.
    ///
    /// Cloning it is how a test — or a signal handler — triggers shutdown.
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// The startup banner.
    ///
    /// Returned rather than printed so the caller decides where it goes and so
    /// a test can assert on it.
    #[must_use]
    pub fn banner(&self) -> String {
        let mut lines = vec![format!("Sandesh {} starting...", env!("CARGO_PKG_VERSION"))];
        match self.addresses.smtp {
            Some(addr) => lines.push(format!("SMTP:      {addr}")),
            None => lines.push(String::from("SMTP:      disabled")),
        }
        match self.addresses.imap {
            Some(addr) => lines.push(format!("IMAP:      {addr}")),
            None if self.config.imap_enabled => {}
            None => lines.push(String::from("IMAP:      disabled")),
        }
        if let Some(addr) = self.addresses.imaps {
            lines.push(format!("IMAPS:     {addr}"));
        }
        match self.addresses.pop3 {
            Some(addr) => lines.push(format!("POP3:      {addr}")),
            None if self.config.pop3_enabled => {}
            None => lines.push(String::from("POP3:      disabled")),
        }
        if let Some(addr) = self.addresses.pop3s {
            lines.push(format!("POP3S:     {addr}"));
        }
        match self.addresses.http {
            Some(addr) => {
                lines.push(format!(
                    "HTTP:      http://{addr}{}",
                    sandesh_api::API_PREFIX
                ));
                lines.push(format!("WebSocket: ws://{addr}{WEBSOCKET_PATH}"));
            }
            None => lines.push(String::from("HTTP:      disabled")),
        }
        // Both stores are relocatable (OD-24), so both are reported: "where is
        // my mail?" must be answerable from the startup output rather than by
        // reading the source for a default.
        lines.push(format!(
            "Database:  {}",
            self.config.database_path.display()
        ));
        lines.push(format!("Blobs:     {}", self.config.blob_path.display()));

        // The security posture, stated plainly. A developer must never have to
        // guess whether their local mail server is accepting anonymous mail.
        if self.config.smtp_enabled {
            lines.push(format!(
                "Auth:      {} · TLS: {}",
                auth_posture(self.config.smtp.auth_mode),
                tls_posture(self.config.smtp.tls_mode),
            ));
        }

        // Comparing this against what the client reports is the only way to
        // trust a self-signed certificate deliberately (ADR-034).
        if let Some(fingerprint) = &self.tls_fingerprint {
            lines.push(format!("Cert:      SHA-256 {fingerprint}"));
        }

        lines.join("\n")
    }

    /// Runs until shutdown is requested, then stops cleanly.
    ///
    /// Both servers are driven concurrently and both are given the same
    /// cancellation token, so one shutdown signal stops everything. Cancelling
    /// makes each stop accepting; in-flight work finishes on its own terms, and
    /// `WebSocket` tasks end when their sockets close, releasing their bus
    /// subscriptions.
    ///
    /// # Errors
    /// [`AppError`] if a server fails while running.
    pub async fn run(self) -> Result<(), AppError> {
        let shutdown = self.shutdown.clone();

        let smtp = self.smtp.map(|server| {
            let token = shutdown.clone();
            tokio::spawn(async move { server.serve(token).await })
        });
        let imap = self.imap.map(|server| {
            let token = shutdown.clone();
            tokio::spawn(async move { server.serve(token).await })
        });
        let pop3 = self.pop3.map(|server| {
            let token = shutdown.clone();
            tokio::spawn(async move { server.serve(token).await })
        });
        let http = self.http.map(|server| {
            let token = shutdown.clone();
            tokio::spawn(async move { server.serve(token).await })
        });

        // The outbox worker takes the same cancellation token as the listeners
        // (ADR-086), so one shutdown signal stops it too. It always runs: with
        // no endpoint registered the outbox stays empty and each tick is a
        // single indexed query that returns nothing.
        let webhooks = {
            let token = shutdown.clone();
            let worker = sandesh_webhook::OutboxWorker::new(Arc::clone(&self.outbox));
            tokio::spawn(async move { worker.run(token).await })
        };

        tracing::info!("Sandesh ready");

        let mut result = Ok(());

        if let Some(handle) = http {
            match handle.await {
                Ok(Ok(())) => {}
                // A server error is worth reporting, but shutdown must still
                // proceed: leaving the database open would be worse.
                Ok(Err(error)) => result = Err(AppError::Http(error)),
                Err(error) => tracing::error!(%error, "HTTP task panicked"),
            }
        }
        if let Some(handle) = smtp {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if result.is_ok() => result = Err(AppError::Smtp(error)),
                Ok(Err(error)) => tracing::error!(%error, "SMTP server failed"),
                Err(error) => tracing::error!(%error, "SMTP task panicked"),
            }
        }
        if let Some(handle) = imap {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if result.is_ok() => result = Err(AppError::Imap(error)),
                Ok(Err(error)) => tracing::error!(%error, "IMAP server failed"),
                Err(error) => tracing::error!(%error, "IMAP task panicked"),
            }
        }
        if let Some(handle) = pop3 {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if result.is_ok() => result = Err(AppError::Pop3(error)),
                Ok(Err(error)) => tracing::error!(%error, "POP3 server failed"),
                Err(error) => tracing::error!(%error, "POP3 task panicked"),
            }
        }

        // Joined before the pool closes: the worker issues queries, and
        // closing the database underneath an in-flight attempt would fail it
        // for a reason that has nothing to do with the endpoint.
        if let Err(error) = webhooks.await {
            tracing::error!(%error, "webhook worker task panicked");
        }

        // Closing the pool last releases every connection and the database
        // file. Doing it before the servers stopped could fail an in-flight
        // request with a confusing error instead of letting it finish.
        self.database.close().await;
        tracing::info!("Sandesh stopped");

        result
    }
}
