//! # Sandesh HTTP API
//!
//! The REST and `WebSocket` adapter (`API_DESIGN.md`). Like every adapter in this
//! workspace it is a *translation layer*: it turns HTTP requests into calls on
//! `sandesh-core` ports and domain results into JSON, and it makes no decision
//! about mail.
//!
//! ## What this crate cannot see
//!
//! There is no `sqlx` dependency and no database handle. [`ApiState`] holds
//! `sandesh-core` traits, so a handler physically cannot write SQL or learn
//! that storage is `SQLite` — the "REST does not know storage internals" rule is
//! enforced by the dependency graph, not by review. It likewise depends on no
//! protocol crate: the API does not know SMTP exists, and SMTP does not know
//! the API exists.
//!
//! ## Layout
//!
//! | Module | Responsibility |
//! | --- | --- |
//! | [`dto`] | Wire types. The public contract, written out rather than derived from the domain |
//! | [`error`] | The one place an internal failure becomes something a client may see |
//! | [`routes`] | Request → port call → response |
//! | [`ws`] | Event-bus subscription → JSON frames |
//! | [`state`] | The dependencies a handler is allowed to reach |
//!
//! ## Running it
//!
//! ```no_run
//! # async fn example(state: sandesh_api::ApiState, config: sandesh_config::HttpConfig)
//! # -> Result<(), sandesh_api::ServerError> {
//! let server = sandesh_api::HttpServer::bind(config, state).await?;
//! println!("listening on {}", server.local_addr());
//! server.serve(tokio_util::sync::CancellationToken::new()).await?;
//! # Ok(())
//! # }
//! ```

#![doc(html_root_url = "https://docs.rs/sandesh-api")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]
#![allow(clippy::module_name_repetitions)]

pub mod assets;
pub mod auth;
pub mod dto;
pub mod error;
pub mod metrics;
pub mod openapi;
pub mod routes;
pub mod state;
pub mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{State, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use sandesh_config::HttpConfig;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

pub use error::{ApiError, ApiResult};
pub use state::{AlwaysReady, ApiState, ReadinessProbe, ServerInfo};

/// The API's base path. Everything is versioned from day one so a breaking
/// change can arrive as `/api/v2` rather than as a silent behaviour change.
pub const API_PREFIX: &str = "/api/v1";

/// The `WebSocket` route, reported by `/api/v1/info`.
pub const WEBSOCKET_PATH: &str = "/api/v1/events";

/// Why the HTTP server could not start.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The configuration is unusable.
    #[error(transparent)]
    Config(#[from] sandesh_config::ConfigError),

    /// The listener could not bind.
    ///
    /// Names the address, so a port clash is actionable rather than opaque.
    #[error("cannot bind HTTP listener to {address}: {source}")]
    Bind {
        /// The address that could not be bound.
        address: SocketAddr,
        /// The underlying cause.
        source: std::io::Error,
    },

    /// Serving failed after startup.
    #[error("HTTP server failed: {0}")]
    Serve(#[source] std::io::Error),
}

/// Builds the router.
///
/// Exposed so tests can exercise handlers without binding a socket, and so the
/// route table is one readable list rather than something assembled across
/// several files.
#[allow(
    clippy::too_many_lines,
    reason = "the route table is one readable list"
)]
pub fn router(state: ApiState, config: &HttpConfig) -> Router {
    let api = Router::new()
        // Health and information.
        .route("/health", get(routes::health))
        .route("/health/ready", get(routes::ready))
        .route("/info", get(routes::info))
        // Accounts.
        .route(
            "/accounts",
            get(routes::list_accounts).post(routes::create_account),
        )
        .route(
            "/accounts/{id}",
            get(routes::get_account)
                .patch(routes::patch_account)
                .delete(routes::delete_account),
        )
        // Mailboxes.
        .route(
            "/mailboxes",
            get(routes::list_mailboxes).post(routes::create_mailbox),
        )
        .route(
            "/mailboxes/{id}",
            get(routes::get_mailbox)
                .patch(routes::patch_mailbox)
                .delete(routes::delete_mailbox),
        )
        .route("/mailboxes/{id}/entries", get(routes::list_mailbox_entries))
        // Entries: placement, UID and flags.
        .route("/entries/{id}", get(routes::get_entry))
        .route("/entries/{id}", delete(routes::delete_entry))
        .route("/entries/{id}/flags", patch(routes::patch_entry_flags))
        .route("/entries/{id}/move", post(routes::move_entry))
        .route("/entries/{id}/copy", post(routes::copy_entry))
        .route("/entries/bulk", post(routes::bulk_entries))
        // Messages: immutable content.
        .route(
            "/messages",
            get(routes::list_messages).post(routes::inject_message),
        )
        .route(
            "/messages/{id}",
            get(routes::get_message).delete(routes::delete_message),
        )
        .route("/messages/{id}/raw", get(routes::get_message_raw))
        .route("/messages/{id}/headers", get(routes::get_message_headers))
        .route(
            "/messages/{id}/headers/analysis",
            get(routes::get_message_header_analysis),
        )
        .route("/messages/{id}/links", get(routes::get_message_links))
        .route("/messages/{id}/mime", get(routes::get_message_mime))
        .route("/messages/{id}/part/{path}", get(routes::get_message_part))
        .route("/messages/{id}/delivery", get(routes::get_message_delivery))
        .route("/messages/{id}/replay", post(routes::replay_message))
        .route("/export", get(routes::export_messages))
        .route("/import", post(routes::import_messages))
        // Delivery records in their own right, including rejected transactions
        // that stored no message and so have no message id to be found by.
        .route("/deliveries", get(routes::list_deliveries))
        // Per-connection protocol transcripts (Phase 7 A2). The capture side
        // has been writing these since ADR-066; these are what read them back.
        .route("/connections", get(routes::list_connections))
        .route("/connections/{id}", get(routes::get_connection))
        .route(
            "/messages/{id}/attachments",
            get(routes::list_message_attachments),
        )
        // Attachments: download and thumbnail.
        .route("/attachments/{id}", get(routes::get_attachment))
        .route(
            "/attachments/{id}/thumbnail",
            get(routes::get_attachment_thumbnail),
        )
        // Fault injection (Phase 8 A1). Runtime-mutable by design (ADR-084):
        // a CI scenario turns a fault on and off in-process, which env-var
        // configuration could not do without a restart.
        .route("/faults", get(routes::list_faults))
        .route("/faults", post(routes::create_fault))
        .route("/faults", delete(routes::clear_faults))
        // Webhooks (Phase 8 A2). Registration is runtime, like faults, so a CI
        // suite can point one at a per-test server; unlike faults, endpoints
        // persist (ADR-086) because the outbox they own has to survive too.
        .route("/webhooks", get(routes::list_webhooks))
        .route("/webhooks", post(routes::create_webhook))
        .route("/webhooks/outbox", get(routes::list_outbox))
        .route("/webhooks/{id}", delete(routes::delete_webhook))
        // Message rules (Phase 8 A3). Persisted, unlike faults: a tagging rule
        // is configuration to curate, not a hazard restart must clear.
        .route("/rules", get(routes::list_rules))
        .route("/rules", post(routes::create_rule))
        .route("/rules/{id}", delete(routes::delete_rule))
        // Search
        .route("/search", get(routes::search_messages))
        .route("/search/explain", get(routes::explain_search))
        // Compose & send message.
        .route("/send", post(routes::send_message))
        // The log stream (Phase 7 A6): the backlog and the live tail.
        .route("/logs", get(routes::list_logs))
        .route("/logs/stream", get(routes::logs_sse))
        // Workspaces & Database management
        .route(
            "/workspaces",
            get(routes::list_workspaces).post(routes::create_workspace),
        )
        .route(
            "/workspaces/{id}",
            get(routes::get_workspace)
                .patch(routes::update_workspace)
                .delete(routes::delete_workspace),
        )
        .route(
            "/workspaces/{id}/activate",
            post(routes::activate_workspace),
        )
        .route("/workspaces/{id}/purge", post(routes::purge_workspace))
        .route("/workspaces/{id}/export", get(routes::export_workspace_db))
        .route("/purge", post(routes::purge_active_workspace))
        // The live event stream.
        .route("/events", get(websocket_handler))
        .route("/events/sse", get(routes::events_sse));

    // The contract itself, served next to the API it describes so a developer
    // (or a code generator) can fetch it from a running instance rather than
    // needing the repository. Outside the versioned prefix because the document
    // describes every version it knows about.
    let contract = Router::new().route("/openapi.json", get(openapi_document));

    Router::new()
        .nest(API_PREFIX, api)
        .nest("/api", contract)
        // The Prometheus scrape, at the bare path every scraper defaults to
        // and outside the versioned prefix: it is an operational surface with
        // its own contract and lifecycle, not part of the JSON API. It is
        // still behind the authentication layer below — see `routes::metrics`.
        .route("/metrics", get(routes::metrics))
        // An unmatched path under `/api` is a client error, not a UI route.
        // Without this the SPA fallback below would answer it with the HTML
        // shell and a 200, so a typo'd endpoint would look like a success to
        // every client (§45). Nested routers do not inherit the outer
        // fallback, so this must be registered explicitly for `/api`.
        .route_service(
            "/api/{*rest}",
            axum::routing::any(|| async { ApiError::NotFound("Endpoint").into_response() }),
        )
        // The Web UI, last, for everything that is not the API.
        .fallback(assets::serve)
        // Authentication wraps *everything*, including the UI and the SPA
        // fallback. Applied to the whole router rather than per route: a
        // per-route list is one somebody forgets to extend, and the cost of
        // forgetting is an unauthenticated endpoint. Here the default is
        // "protected" and the exemptions are explicit (see `auth`).
        // `layer`, not `route_layer`: `route_layer` runs only for requests that
        // matched a route, so the SPA fallback — every path the API does not
        // claim, including `/` — would have skipped authentication entirely.
        // That is the difference between "the UI needs a credential" and "the
        // UI is the one way in without one", and a test pins it.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_credential,
        ))
        .layer(TraceLayer::new_for_http())
        // An unbounded request body is a memory exhaustion vector reachable
        // with one POST; cap it even though Phase 3 has few write endpoints.
        .layer(RequestBodyLimitLayer::new(config.max_body_bytes))
        .layer(cors_layer(config))
        .with_state(state)
}

/// The CORS policy.
///
/// Disabled by default (`API_DESIGN.md` §2). The Phase 4 UI is served from the
/// same origin as the API, so it needs no CORS at all; the only real use is a
/// separate dev server, which is a deliberate, explicitly-listed origin. There
/// is deliberately no wildcard option: `Allow-Origin: *` on a local mail server
/// would let any page a developer visits read their mail.
fn cors_layer(config: &HttpConfig) -> CorsLayer {
    let mut layer = CorsLayer::new();
    for origin in &config.cors_origins {
        if let Ok(value) = origin.parse::<axum::http::HeaderValue>() {
            layer = layer.allow_origin(value);
        } else {
            tracing::warn!(origin = %origin, "ignoring unparseable CORS origin");
        }
    }
    layer
}

/// `GET /api/openapi.json` — the generated contract.
///
/// Served from the same in-memory document the build generates, so a running
/// server can never advertise a contract that differs from the one the
/// TypeScript was generated from.
async fn openapi_document() -> Response {
    match openapi::document_json() {
        Ok(json) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json,
        )
            .into_response(),
        // Unreachable in practice: the document is built from static
        // annotations and a test asserts it serialises. Handled rather than
        // unwrapped because this crate forbids panicking in production paths.
        Err(error) => {
            tracing::error!(%error, "the OpenAPI document could not be serialised");
            ApiError::Internal(String::from("the OpenAPI document could not be serialised"))
                .into_response()
        }
    }
}

/// Upgrades a request to the event stream.
async fn websocket_handler(upgrade: WebSocketUpgrade, State(state): State<ApiState>) -> Response {
    let bus = Arc::clone(&state.events);
    upgrade.on_upgrade(move |socket| ws::serve_connection(socket, bus))
}

/// A bound HTTP server.
///
/// Binding is separate from serving for the same reason as in `sandesh-smtp`:
/// the composition root can learn the real address — the ephemeral port, when
/// the configuration asked for port 0 — and report it before any request is
/// served. It also means a port clash fails startup rather than surfacing
/// later.
pub struct HttpServer {
    listener: TcpListener,
    router: Router,
    local_addr: SocketAddr,
    /// Serves HTTPS when present.
    ///
    /// Received, never configured: the composition root builds it from
    /// `sandesh_proto::Identity`, so there is exactly one certificate and one
    /// TLS configuration in the process (ADR-034), shared with the mail
    /// listeners.
    tls: Option<tokio_rustls::TlsAcceptor>,
}

impl std::fmt::Debug for HttpServer {
    /// `TlsAcceptor` implements no `Debug`, and its contents are a private key
    /// and a certificate chain — nothing that belongs in a log line. Presence
    /// is the only fact worth reporting.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpServer")
            .field("local_addr", &self.local_addr)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

impl HttpServer {
    /// Validates the configuration and binds the listener.
    ///
    /// # Errors
    /// [`ServerError::Config`] for unusable settings, [`ServerError::Bind`] when
    /// the address is unavailable.
    pub async fn bind(config: HttpConfig, state: ApiState) -> Result<Self, ServerError> {
        config.validate()?;
        let listener = Self::bind_listener(config.bind).await?;
        Self::from_listener(listener, &config, state)
    }

    /// Binds only the socket, without building the router.
    ///
    /// Split out because the composition root must know the bound address
    /// *before* it can build the state — `/info` reports that address, and with
    /// port 0 it does not exist until the socket is bound.
    ///
    /// # Errors
    /// [`ServerError::Bind`] when the address is unavailable.
    pub async fn bind_listener(address: SocketAddr) -> Result<TcpListener, ServerError> {
        TcpListener::bind(address)
            .await
            .map_err(|source| ServerError::Bind { address, source })
    }

    /// Builds a server over an already-bound listener.
    ///
    /// # Errors
    /// [`ServerError::Config`] for unusable settings.
    pub fn from_listener(
        listener: TcpListener,
        config: &HttpConfig,
        state: ApiState,
    ) -> Result<Self, ServerError> {
        config.validate()?;
        let local_addr = listener.local_addr().unwrap_or(config.bind);
        let router = router(state, config);

        Ok(Self {
            listener,
            router,
            local_addr,
            tls: None,
        })
    }

    /// Serves HTTPS using `acceptor`.
    ///
    /// A builder step rather than a constructor parameter, matching
    /// `ImapServer::with_tls`: a server without one is entirely valid, and it
    /// is the composition root's job to decide.
    #[must_use]
    pub fn with_tls(mut self, acceptor: tokio_rustls::TlsAcceptor) -> Self {
        self.tls = Some(acceptor);
        self
    }

    /// Whether this server will speak TLS.
    #[must_use]
    pub const fn is_tls(&self) -> bool {
        self.tls.is_some()
    }

    /// The address actually bound.
    ///
    /// With port 0 this is the ephemeral port the OS chose.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Serves until `shutdown` is cancelled.
    ///
    /// Consumes the server, so it cannot be started twice. On cancellation axum
    /// stops accepting and lets in-flight requests finish; open `WebSocket`
    /// connections end when their tasks observe the closed socket, releasing
    /// their bus subscriptions.
    ///
    /// # Errors
    /// [`ServerError::Serve`] if the accept loop fails.
    pub async fn serve(self, shutdown: CancellationToken) -> Result<(), ServerError> {
        match self.tls {
            None => {
                tracing::info!(address = %self.local_addr, "HTTP server listening");
                axum::serve(self.listener, self.router)
                    .with_graceful_shutdown(async move { shutdown.cancelled().await })
                    .await
                    .map_err(ServerError::Serve)
            }
            Some(acceptor) => {
                tracing::info!(address = %self.local_addr, "HTTPS server listening");
                serve_tls(self.listener, self.router, acceptor, shutdown).await
            }
        }
    }
}

/// Serves the router over TLS until `shutdown` is cancelled.
///
/// `axum::serve` owns its own TCP accept loop and has no way to wrap each
/// stream in a TLS handshake, so the loop is written out here. It is short, and
/// three properties in it are deliberate:
///
/// - **A failed handshake drops one connection, never the server.** A port scan,
///   a plaintext client, or a browser rejecting the self-signed certificate all
///   produce handshake errors, and any of them killing the listener would make
///   HTTPS trivially deniable.
/// - **The handshake happens inside the spawned task**, not before it. Doing it
///   in the accept loop would let one slow (or deliberately stalled) client
///   block every other connection — a one-line denial of service.
/// - **Shutdown stops accepting immediately** and lets in-flight connections
///   finish on their own, matching what `axum::serve`'s graceful shutdown does
///   for the plaintext path.
async fn serve_tls(
    listener: TcpListener,
    router: Router,
    acceptor: tokio_rustls::TlsAcceptor,
    shutdown: CancellationToken,
) -> Result<(), ServerError> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder;

    loop {
        let (stream, peer) = tokio::select! {
            // Cancellation is checked first so a shutdown during a burst of
            // connections is not starved by the accept arm always being ready.
            biased;
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok(pair) => pair,
                // An accept error is per-connection (a peer that vanished
                // between the SYN and the accept, or a transient fd limit);
                // failing the server over one would be wrong.
                Err(error) => {
                    tracing::warn!(%error, "HTTPS accept failed");
                    continue;
                }
            },
        };

        let acceptor = acceptor.clone();
        let service = router.clone().into_service::<hyper::body::Incoming>();
        let connection_shutdown = shutdown.clone();

        tokio::spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(stream) => stream,
                Err(error) => {
                    // Expected in normal operation: plaintext clients, port
                    // scanners, and browsers declining a self-signed
                    // certificate all land here. Debug, not warn.
                    tracing::debug!(%peer, %error, "TLS handshake failed");
                    return;
                }
            };

            let io = TokioIo::new(tls_stream);
            let builder = Builder::new(TokioExecutor::new());
            let connection = builder.serve_connection_with_upgrades(
                io,
                hyper::service::service_fn(
                    move |request: hyper::Request<hyper::body::Incoming>| {
                        // Cloning per request is the cheap `Arc` clone axum's own
                        // serve loop performs.
                        let mut service = service.clone();
                        async move {
                            use tower_service::Service as _;
                            service.call(request).await
                        }
                    },
                ),
            );
            tokio::pin!(connection);

            tokio::select! {
                result = connection.as_mut() => {
                    if let Err(error) = result {
                        tracing::debug!(%peer, %error, "HTTPS connection ended");
                    }
                }
                () = connection_shutdown.cancelled() => {
                    // Ask for a graceful close, then let the connection finish
                    // what it was already writing.
                    connection.as_mut().graceful_shutdown();
                    let _ = connection.await;
                }
            }
        });
    }

    Ok(())
}
