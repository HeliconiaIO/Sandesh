//! The IMAP server: binding, accepting, and shutting down.
//!
//! Lifecycle is expressed in the type system, as `sandesh_smtp::server` does:
//! [`ImapServer::bind`] consumes its configuration and yields a
//! [`BoundServer`]; only a `BoundServer` can [`serve`](BoundServer::serve), and
//! `serve` consumes it. Double-start and use-after-shutdown do not compile.
//!
//! # Two listeners, one server
//!
//! IMAP conventionally offers cleartext (143, optionally upgradable with
//! `STARTTLS`) and implicit TLS (993) simultaneously, because clients differ in
//! which they will use. Both are accepted by one server so that a single
//! shutdown token stops both and the connection cap is shared (ADR-041).
//!
//! ```text
//! cleartext:  TCP accept ──▶ greeting ──▶ session
//!                                │
//!                            STARTTLS ──▶ handshake ──▶ *fresh* session
//!
//! implicit:   TCP accept ──▶ handshake ──▶ greeting ──▶ session
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use sandesh_config::{ConfigError, ImapConfig};
use sandesh_core::{
    AccountStore, ConnectionDisposition, EventSource, MailboxStore, MessageStore, Protocol,
};
use sandesh_proto::{
    Accepted, BindError, CancellationToken, Connection, ConnectionRecorder, Listener,
    OpenConnection, ProtocolError,
};
use tokio_rustls::TlsAcceptor;

use crate::session::{Outcome, Session};

/// Why the server could not start or run.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The configuration is unusable.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// A listener could not bind.
    #[error(transparent)]
    Bind(#[from] BindError),

    /// Accepting a connection failed.
    #[error("accept failed: {0}")]
    Accept(#[from] ProtocolError),

    /// TLS is configured but no acceptor was supplied.
    ///
    /// Startup fails rather than continuing, because the alternative for the
    /// implicit listener is a port the operator believes is encrypted quietly
    /// serving cleartext — a downgrade nobody would observe (ADR-037).
    #[error(
        "IMAP {posture} is configured but no TLS certificate was supplied: \
         provide one, or disable it"
    )]
    MissingTlsAcceptor {
        /// Which setting needs the certificate.
        posture: &'static str,
    },
}

/// What a running server reports about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerStatus {
    /// The cleartext address actually bound, when there is one.
    pub local_addr: Option<SocketAddr>,
    /// The implicit-TLS address actually bound, when there is one.
    pub tls_addr: Option<SocketAddr>,
    /// Connections currently being served, across both listeners.
    pub active_connections: usize,
    /// The configured concurrency cap, per listener.
    pub max_connections: usize,
}

/// A configured, not-yet-bound IMAP server.
pub struct ImapServer {
    config: Arc<ImapConfig>,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Option<Arc<dyn MailboxStore>>,
    messages: Option<Arc<dyn MessageStore>>,
    events: Option<Arc<dyn EventSource>>,
    /// Narrows `SEARCH`; never changes what it returns (`STORAGE_DESIGN.md` §7).
    search_index: Option<Arc<dyn sandesh_core::SearchIndex>>,
    tls: Option<TlsAcceptor>,
    /// Records per-connection transcripts (ADR-066). Disabled by default.
    recorder: ConnectionRecorder,
}

impl std::fmt::Debug for ImapServer {
    /// Reports only *whether* TLS is configured: `TlsAcceptor` holds key
    /// material and is never rendered.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapServer")
            .field("bind", &self.config.bind)
            .field("implicit_tls_bind", &self.config.implicit_tls_bind)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

impl ImapServer {
    /// Creates a server from configuration and its dependencies.
    ///
    /// The account store is injected rather than constructed here, so the
    /// server never opens a database and a test can run it against an
    /// in-memory implementation of the port.
    #[must_use]
    pub fn new(config: ImapConfig, accounts: Arc<dyn AccountStore>) -> Self {
        Self {
            config: Arc::new(config),
            accounts,
            mailboxes: None,
            messages: None,
            events: None,
            search_index: None,
            tls: None,
            recorder: ConnectionRecorder::disabled(Protocol::Imap),
        }
    }

    /// Records a transcript for every accepted connection (ADR-066).
    #[must_use]
    pub fn with_tracing(mut self, recorder: ConnectionRecorder) -> Self {
        self.recorder = recorder;
        self
    }

    /// Injects mailbox and message stores for mailbox operations.
    #[must_use]
    pub fn with_stores(
        mut self,
        mailboxes: Arc<dyn MailboxStore>,
        messages: Arc<dyn MessageStore>,
    ) -> Self {
        self.mailboxes = Some(mailboxes);
        self.messages = Some(messages);
        self
    }

    /// Injects the event source that `IDLE` reports from.
    ///
    /// Without it the server runs exactly as before and refuses `IDLE`, which
    /// is why it is a separate step: the capability follows the injection, so a
    /// deployment that supplies no source never advertises what it cannot do.
    #[must_use]
    pub fn with_events(mut self, events: Arc<dyn EventSource>) -> Self {
        self.events = Some(events);
        self
    }

    /// Injects the full-text index that `SEARCH` narrows with.
    ///
    /// Unlike the event source, this adds no capability and is advertised
    /// nowhere: it may only make a search cheaper. A server without one returns
    /// identical results (`STORAGE_DESIGN.md` §7).
    #[must_use]
    pub fn with_search_index(mut self, index: Arc<dyn sandesh_core::SearchIndex>) -> Self {
        self.search_index = Some(index);
        self
    }

    /// Supplies the TLS acceptor for `STARTTLS` and the implicit listener.
    ///
    /// Injected for the same reason the store is: the server reads no
    /// certificate from disk, so the composition root stays the only place
    /// that touches the filesystem and a test can drive TLS from memory.
    #[must_use]
    pub fn with_tls(mut self, acceptor: TlsAcceptor) -> Self {
        self.tls = Some(acceptor);
        self
    }

    /// Validates the configuration and binds the listeners.
    ///
    /// Returns operator warnings alongside the bound server; the caller decides
    /// how to present them.
    ///
    /// # Errors
    /// [`ServerError::Config`] for unusable settings, [`ServerError::Bind`] when
    /// an address is unavailable — naming it, so a port clash is actionable —
    /// or [`ServerError::MissingTlsAcceptor`] when TLS is configured without a
    /// certificate.
    pub async fn bind(self) -> Result<(BoundServer, Vec<String>), ServerError> {
        self.config.validate()?;

        if self.config.needs_tls() && self.tls.is_none() {
            return Err(ServerError::MissingTlsAcceptor {
                posture: if self.config.implicit_tls_bind.is_some() {
                    "implicit TLS"
                } else {
                    "STARTTLS"
                },
            });
        }

        let warnings = self.config.warnings();

        // Bound in order, and a failure on the second releases the first: a
        // half-bound server would hold a port while reporting startup failure.
        let cleartext = match self.config.bind {
            Some(addr) => Some(Listener::bind(addr, self.config.max_connections).await?),
            None => None,
        };
        let tls_listener = match self.config.implicit_tls_bind {
            Some(addr) => Some(Listener::bind(addr, self.config.max_connections).await?),
            None => None,
        };

        Ok((
            BoundServer {
                cleartext,
                tls_listener,
                config: self.config,
                accounts: self.accounts,
                mailboxes: self.mailboxes,
                messages: self.messages,
                events: self.events,
                search_index: self.search_index,
                tls: self.tls,
                recorder: self.recorder,
            },
            warnings,
        ))
    }
}

/// A bound IMAP server, ready to serve.
pub struct BoundServer {
    cleartext: Option<Listener>,
    tls_listener: Option<Listener>,
    config: Arc<ImapConfig>,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Option<Arc<dyn MailboxStore>>,
    messages: Option<Arc<dyn MessageStore>>,
    events: Option<Arc<dyn EventSource>>,
    /// Narrows `SEARCH`; never changes what it returns (`STORAGE_DESIGN.md` §7).
    search_index: Option<Arc<dyn sandesh_core::SearchIndex>>,
    tls: Option<TlsAcceptor>,
    recorder: ConnectionRecorder,
}

impl std::fmt::Debug for BoundServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundServer")
            .field("local_addr", &self.local_addr())
            .field("tls_addr", &self.tls_addr())
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

impl BoundServer {
    /// The cleartext address actually bound.
    ///
    /// With port 0 this is the ephemeral port the OS chose — how tests avoid
    /// fixed ports and run in parallel without colliding.
    #[must_use]
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.cleartext.as_ref().map(Listener::local_addr)
    }

    /// The implicit-TLS address actually bound.
    #[must_use]
    pub fn tls_addr(&self) -> Option<SocketAddr> {
        self.tls_listener.as_ref().map(Listener::local_addr)
    }

    /// A snapshot of server state.
    #[must_use]
    pub fn status(&self) -> ServerStatus {
        let active = self
            .cleartext
            .as_ref()
            .map_or(0, Listener::active_connections)
            + self
                .tls_listener
                .as_ref()
                .map_or(0, Listener::active_connections);

        ServerStatus {
            local_addr: self.local_addr(),
            tls_addr: self.tls_addr(),
            active_connections: active,
            max_connections: self.config.max_connections,
        }
    }

    /// Accepts connections on both listeners until `shutdown` is cancelled.
    ///
    /// Consumes the server, so it cannot be started twice.
    ///
    /// In-flight sessions are left to finish on their own; they are bounded by
    /// the command timeout and so cannot hang indefinitely.
    ///
    /// # Errors
    /// [`ServerError::Accept`] if accepting fails in a way that is not merely
    /// one client going away.
    pub async fn serve(self, shutdown: CancellationToken) -> Result<(), ServerError> {
        let Self {
            cleartext,
            tls_listener,
            config,
            accounts,
            mailboxes,
            messages,
            events,
            search_index,
            tls,
            recorder,
        } = self;

        // Two accept loops, joined so that either failing stops the other and
        // one cancellation stops both. `tokio::try_join!` would abandon the
        // second loop's cleanup, so the tasks are spawned and awaited.
        let cleartext_task = cleartext.map(|listener| {
            let deps = ConnectionDeps {
                config: Arc::clone(&config),
                accounts: Arc::clone(&accounts),
                mailboxes: mailboxes.clone(),
                messages: messages.clone(),
                events: events.clone(),
                search_index: search_index.clone(),
                tls: tls.clone(),
                recorder: recorder.clone(),
            };
            let token = shutdown.clone();
            tokio::spawn(async move { accept_loop(listener, deps, false, token).await })
        });

        let tls_task = tls_listener.map(|listener| {
            let deps = ConnectionDeps {
                config: Arc::clone(&config),
                accounts: Arc::clone(&accounts),
                mailboxes: mailboxes.clone(),
                messages: messages.clone(),
                events: events.clone(),
                search_index: search_index.clone(),
                tls: tls.clone(),
                recorder: recorder.clone(),
            };
            let token = shutdown.clone();
            tokio::spawn(async move { accept_loop(listener, deps, true, token).await })
        });

        for task in [cleartext_task, tls_task].into_iter().flatten() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                // A panicking accept loop must not be reported as a clean stop.
                Err(join_error) => {
                    tracing::error!(%join_error, "an IMAP accept loop terminated abnormally");
                }
            }
        }

        tracing::info!("IMAP server stopped accepting connections");
        Ok(())
    }
}

/// The dependencies every connection needs, threaded as one value.
///
/// These five are always cloned and passed together; bundling them keeps the
/// accept/serve signatures stable as later increments add stores.
#[derive(Clone)]
struct ConnectionDeps {
    config: Arc<ImapConfig>,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Option<Arc<dyn MailboxStore>>,
    messages: Option<Arc<dyn MessageStore>>,
    events: Option<Arc<dyn EventSource>>,
    /// Narrows `SEARCH`; never changes what it returns (`STORAGE_DESIGN.md` §7).
    search_index: Option<Arc<dyn sandesh_core::SearchIndex>>,
    tls: Option<TlsAcceptor>,
    recorder: ConnectionRecorder,
}

// `TlsAcceptor` and the `dyn` store traits do not implement `Debug`, so this is
// written by hand; the store fields are reported as presence flags.
#[allow(clippy::missing_fields_in_debug)]
impl std::fmt::Debug for ConnectionDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionDeps")
            .field("config", &self.config)
            .field("has_accounts", &true)
            .field("has_mailboxes", &self.mailboxes.is_some())
            .field("has_messages", &self.messages.is_some())
            .field("has_tls", &self.tls.is_some())
            .finish()
    }
}

/// Accepts connections on one listener until cancelled.
///
/// `implicit_tls` selects the posture: the handshake happens before any IMAP is
/// spoken, so a client that sends cleartext to this listener fails the
/// handshake and never reaches the protocol.
async fn accept_loop(
    listener: Listener,
    deps: ConnectionDeps,
    implicit_tls: bool,
    shutdown: CancellationToken,
) -> Result<(), ServerError> {
    tracing::info!(
        address = %listener.local_addr(),
        implicit_tls,
        max_connections = deps.config.max_connections,
        "IMAP server listening"
    );

    loop {
        let mut accepted = match listener.accept(&shutdown).await {
            Ok(Some(accepted)) => accepted,
            // Cancelled.
            Ok(None) => break,
            Err(error) if error.is_disconnect() => continue,
            Err(error) => return Err(ServerError::Accept(error)),
        };

        let deps = deps.clone();
        let peer = accepted.peer;
        // One record per accepted socket, shared across a STARTTLS upgrade.
        let open = deps.recorder.open(Some(peer.to_string()));

        // The whole `Accepted` moves into the task, so its concurrency permit
        // is released when the session ends rather than when the loop iterates.
        // That is what makes the cap mean "connections being served".
        tokio::spawn(async move {
            tracing::debug!(%peer, implicit_tls, "IMAP connection opened");
            // The row must exist before the session records anything that
            // references it (ADR-066).
            let flushing = open.persist_open_streaming().await;

            let result = serve_connection(&mut accepted, deps, implicit_tls, peer, &open).await;

            let disposition = match result {
                Ok(()) => {
                    tracing::debug!(%peer, "IMAP connection closed");
                    ConnectionDisposition::Closed
                }
                Err(ref error) => {
                    tracing::debug!(%peer, %error, "IMAP connection ended with error");
                    if error.is_disconnect() {
                        ConnectionDisposition::Disconnected
                    } else {
                        ConnectionDisposition::ServerClosed
                    }
                }
            };
            open.close(disposition, implicit_tls, Some(flushing)).await;

            drop(accepted);
        });
    }

    Ok(())
}

/// Serves one connection in the posture its listener selects.
async fn serve_connection(
    accepted: &mut Accepted,
    deps: ConnectionDeps,
    implicit_tls: bool,
    peer: SocketAddr,
    open: &OpenConnection,
) -> Result<(), ProtocolError> {
    let ConnectionDeps {
        config,
        accounts,
        mailboxes,
        messages,
        events,
        search_index,
        tls,
        recorder,
    } = deps;
    let Accepted { stream, .. } = accepted;

    if implicit_tls {
        let Some(acceptor) = tls else {
            // Startup validation makes this unreachable. Refusing rather than
            // falling back to plaintext is the only safe answer if it were not:
            // this port promises encryption.
            tracing::error!(%peer, "implicit TLS listener has no acceptor; refusing");
            return Ok(());
        };
        return serve_implicit_tls(
            &mut *stream,
            ConnectionDeps {
                config,
                accounts,
                mailboxes,
                messages,
                events,
                search_index,
                tls: None,
                recorder,
            },
            &acceptor,
            peer,
            open,
        )
        .await;
    }

    // Cleartext: greet, then run. `encrypted: false` means AUTH is not offered
    // and `STARTTLS` is advertised when configured.
    let (read, write) = tokio::io::split(&mut *stream);
    let connection = Connection::with_transcript(
        read,
        write,
        config.max_line_length,
        Some(peer),
        open.transcript().clone(),
    );
    let session = Session::with_stores(
        connection,
        Arc::clone(&config),
        Arc::clone(&accounts),
        mailboxes.clone(),
        messages.clone(),
        false,
    )
    .with_events(events.clone())
    .with_search_index(search_index.clone());

    match session.run(true).await? {
        Outcome::Closed => return Ok(()),
        Outcome::UpgradeToTls => {}
    }

    // The session sent the tagged OK inviting the handshake and dropped its
    // borrow, so the raw socket can be handed to rustls.
    let Some(acceptor) = tls else {
        // Unreachable: STARTTLS is only accepted when advertised, and it is
        // only advertised when an acceptor exists. Handled rather than
        // unwrapped because this crate forbids panicking on a connection path.
        tracing::error!(%peer, "STARTTLS was accepted with no TLS acceptor configured");
        return Ok(());
    };

    let tls_stream = match acceptor.accept(&mut *stream).await {
        Ok(tls_stream) => tls_stream,
        Err(error) => {
            // Ordinary: a client that distrusts the self-signed certificate
            // aborts here. Nothing can be said back — the channel is no longer
            // cleartext IMAP — so it is logged and the connection dropped.
            tracing::debug!(%peer, %error, "IMAP STARTTLS handshake failed");
            return Ok(());
        }
    };

    tracing::debug!(%peer, "IMAP STARTTLS established");
    open.transcript().note("TLS established (STARTTLS)");

    let (read, write) = tokio::io::split(tls_stream);
    let connection = Connection::with_transcript(
        read,
        write,
        config.max_line_length,
        Some(peer),
        open.transcript().clone(),
    );

    // A *fresh* session: RFC 9051 §6.2.1 requires the server to discard
    // everything it learned in cleartext, and reusing the session object with a
    // flag flipped would leave that state behind (ADR-036).
    //
    // `greet: false` — RFC 9051 §6.2.1 says the server must not re-issue a
    // greeting after STARTTLS; the client sends CAPABILITY again instead.
    Session::with_stores(connection, config, accounts, mailboxes, messages, true)
        .with_events(events)
        .with_search_index(search_index)
        .run(false)
        .await
        .map(|_| ())
}

/// Serves one connection on the implicit-TLS listener.
///
/// The handshake completes before an IMAP session exists, so the greeting is
/// the first thing written *inside* TLS. A client that speaks cleartext IMAP
/// here fails the handshake and is dropped without reaching the protocol —
/// which is the property that makes the mode meaningful, and is asserted by a
/// test.
async fn serve_implicit_tls(
    stream: &mut tokio::net::TcpStream,
    deps: ConnectionDeps,
    acceptor: &TlsAcceptor,
    peer: SocketAddr,
    open: &OpenConnection,
) -> Result<(), ProtocolError> {
    let ConnectionDeps {
        config,
        accounts,
        mailboxes,
        messages,
        events,
        search_index,
        ..
    } = deps;

    let tls_stream = match acceptor.accept(stream).await {
        Ok(tls_stream) => tls_stream,
        Err(error) => {
            // Emphatically not a fallback to plaintext, which would reinstate
            // the downgrade this mode exists to prevent.
            tracing::debug!(%peer, %error, "IMAP implicit TLS handshake failed");
            return Ok(());
        }
    };

    tracing::debug!(%peer, "IMAP implicit TLS established");
    open.transcript().note("TLS established (implicit)");

    let (read, write) = tokio::io::split(tls_stream);
    let connection = Connection::with_transcript(
        read,
        write,
        config.max_line_length,
        Some(peer),
        open.transcript().clone(),
    );

    // `encrypted: true` from the outset, so AUTH is offered on the first
    // CAPABILITY and STARTTLS is never advertised.
    Session::with_stores(connection, config, accounts, mailboxes, messages, true)
        .with_events(events)
        .with_search_index(search_index)
        .run(true)
        .await
        .map(|_| ())
}
