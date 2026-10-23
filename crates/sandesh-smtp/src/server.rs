//! The SMTP server: binding, accepting, and shutting down.
//!
//! Lifecycle is expressed in the type system rather than in a status field
//! nobody checks. [`SmtpServer::bind`] consumes its configuration and returns a
//! [`BoundServer`]; only a `BoundServer` can [`serve`](BoundServer::serve), and
//! `serve` consumes it. Double-start and use-after-shutdown are therefore not
//! runtime errors — they do not compile.

use crate::session::{Outcome, Session};
use sandesh_config::{ConfigError, SmtpConfig, TlsMode};
use sandesh_core::{ConnectionDisposition, Protocol};
use sandesh_core::{DeliveryService, FaultRegistry};
use sandesh_proto::{
    Accepted, BindError, CancellationToken, Connection, ConnectionRecorder, Listener,
    OpenConnection,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

/// Why the server could not start or run.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The configuration is unusable.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// The listener could not bind.
    #[error(transparent)]
    Bind(#[from] BindError),

    /// Accepting a connection failed.
    #[error("accept failed: {0}")]
    Accept(#[from] sandesh_proto::ProtocolError),

    /// TLS is configured but no acceptor was supplied.
    ///
    /// Names the mode, because the fix differs: `implicit` and `start_tls`
    /// both need a certificate, and the alternative is to set
    /// `tls_mode = disabled` deliberately rather than by accident.
    #[error(
        "SMTP tls_mode is `{mode}` but no TLS certificate was supplied: \
         provide one, or set tls_mode to `disabled`"
    )]
    MissingTlsAcceptor {
        /// The configured mode, as its wire word.
        mode: &'static str,
    },
}

/// What a running server reports about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerStatus {
    /// The address actually bound.
    pub local_addr: SocketAddr,
    /// Connections currently being served.
    pub active_connections: usize,
    /// The configured concurrency cap.
    pub max_connections: usize,
}

/// A configured, not-yet-bound SMTP server.
pub struct SmtpServer {
    config: Arc<SmtpConfig>,
    delivery: Arc<DeliveryService>,
    /// Supplied when TLS is configured. Absent means `STARTTLS` cannot be
    /// honoured, and the session will not advertise it.
    tls: Option<TlsAcceptor>,
    /// Records per-connection transcripts (ADR-066). Disabled by default.
    recorder: ConnectionRecorder,
    /// Runtime fault rules (ADR-084). Empty by default.
    faults: Arc<FaultRegistry>,
}

impl std::fmt::Debug for SmtpServer {
    /// Reports only *whether* TLS is configured. `TlsAcceptor` holds key
    /// material, so it is never rendered.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpServer")
            .field("bind", &self.config.bind)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

impl SmtpServer {
    /// Creates a server from configuration and its dependencies.
    ///
    /// Dependencies are injected rather than constructed here: the server never
    /// opens a database, which is what lets tests run it against in-memory
    /// stores.
    #[must_use]
    pub fn new(config: SmtpConfig, delivery: Arc<DeliveryService>) -> Self {
        Self {
            config: Arc::new(config),
            delivery,
            tls: None,
            recorder: ConnectionRecorder::disabled(Protocol::Smtp),
            faults: Arc::new(FaultRegistry::new()),
        }
    }

    /// Shares the runtime fault registry with this server (ADR-084).
    ///
    /// The same `Arc` the API mutates, so a rule set through `/api/v1/faults`
    /// applies to the next command without a restart — which is the entire
    /// reason faults are runtime state and the rest of the configuration is
    /// not.
    #[must_use]
    pub fn with_faults(mut self, faults: Arc<FaultRegistry>) -> Self {
        self.faults = faults;
        self
    }

    /// The fault registry this server consults.
    #[must_use]
    pub fn faults(&self) -> Arc<FaultRegistry> {
        Arc::clone(&self.faults)
    }

    /// Supplies the TLS acceptor that `STARTTLS` upgrades onto.
    ///
    /// Injected rather than built here for the same reason the stores are: the
    /// server never reads a certificate from disk, so a test can drive the
    /// upgrade with an in-memory identity, and the composition root stays the
    /// only place that touches the filesystem.
    #[must_use]
    pub fn with_tls(mut self, acceptor: TlsAcceptor) -> Self {
        self.tls = Some(acceptor);
        self
    }

    /// Records a transcript for every accepted connection (ADR-066).
    #[must_use]
    pub fn with_tracing(mut self, recorder: ConnectionRecorder) -> Self {
        self.recorder = recorder;
        self
    }

    /// Validates the configuration and binds the listener.
    ///
    /// Returns any operator warnings alongside the bound server — chiefly the
    /// external-binding-without-authentication case, which the caller should
    /// surface rather than swallow.
    ///
    /// # Errors
    /// Returns [`ServerError::Config`] for unusable settings, or
    /// [`ServerError::Bind`] — which names the address and explains why, so a
    /// port clash is actionable rather than opaque.
    pub async fn bind(self) -> Result<(BoundServer, Vec<String>), ServerError> {
        self.config.validate()?;

        // A TLS mode with no acceptor must not start. For `Implicit` the
        // consequence would be a listener that silently served plaintext on a
        // port the operator believes is encrypted — a TLS downgrade nobody
        // would observe. Failing startup is the only safe answer (ADR-037).
        if self.config.tls_mode.is_enabled() && self.tls.is_none() {
            return Err(ServerError::MissingTlsAcceptor {
                mode: match self.config.tls_mode {
                    TlsMode::Implicit => "implicit",
                    TlsMode::StartTls => "start_tls",
                    TlsMode::Disabled => "disabled",
                },
            });
        }

        let warnings = self.config.warnings();

        let listener = Listener::bind(self.config.bind, self.config.max_connections).await?;

        Ok((
            BoundServer {
                listener,
                config: self.config,
                delivery: self.delivery,
                tls: self.tls,
                recorder: self.recorder,
                faults: self.faults,
            },
            warnings,
        ))
    }
}

/// Serves one connection, in whichever TLS posture the configuration selects.
///
/// Two shapes, chosen by [`TlsMode`] before any byte is read:
///
/// ```text
/// Implicit:  TCP accept ──▶ TLS handshake ──▶ session (encrypted from byte 0)
///
/// StartTls:  cleartext session ──STARTTLS──▶ 220 ──▶ handshake ──▶ fresh session
///                                                        │
///                                              all cleartext state discarded
/// ```
///
/// For `StartTls` the second session is deliberately **new**: RFC 3207 §4.2
/// requires the server to discard everything it learned before the handshake,
/// including the `EHLO` domain and any authentication. Reusing the session
/// object and merely flipping a flag would leave that state behind, which is
/// the subtle version of the same vulnerability (ADR-036).
///
/// For `Implicit` there is nothing to discard, because no SMTP is spoken before
/// the handshake at all — which is the property that makes the mode meaningful
/// (ADR-037).
async fn serve_connection(
    accepted: &mut Accepted,
    config: Arc<SmtpConfig>,
    delivery: Arc<DeliveryService>,
    tls: Option<TlsAcceptor>,
    peer: SocketAddr,
    open: &OpenConnection,
    faults: Arc<FaultRegistry>,
) -> Result<(), sandesh_proto::ProtocolError> {
    let Accepted { stream, .. } = accepted;

    // Implicit TLS: the handshake happens first, so the session is constructed
    // over an already-encrypted stream and never sees plaintext. Branching
    // here rather than inside the session is what guarantees it — there is no
    // code path on this mode that builds a cleartext `Connection`.
    if config.tls_mode == TlsMode::Implicit {
        let Some(acceptor) = tls else {
            // Startup validation rejects `Implicit` without an acceptor, so
            // this is unreachable. Refusing the connection rather than falling
            // back to plaintext is the only safe answer if it ever were.
            tracing::error!(
                %peer,
                "implicit TLS is configured with no acceptor; refusing the connection"
            );
            return Ok(());
        };

        return serve_implicit_tls(
            &mut *stream,
            config,
            delivery,
            &acceptor,
            peer,
            open,
            faults,
        )
        .await;
    }

    let (read, write) = tokio::io::split(&mut *stream);
    let connection = Connection::with_transcript(
        read,
        write,
        config.max_line_length,
        Some(peer),
        open.transcript().clone(),
    );
    let session = Session::with_encryption(
        connection,
        Arc::clone(&config),
        Arc::clone(&delivery),
        false,
    )
    .with_connection_id(open.id())
    .with_faults(Arc::clone(&faults));

    match session.run_to_outcome(true).await? {
        Outcome::Closed => return Ok(()),
        Outcome::UpgradeToTls => {}
    }

    // The session already sent `220 Ready to start TLS` and dropped its
    // borrow of the stream, so the raw socket can be handed to the handshake.
    let Some(acceptor) = tls else {
        // Unreachable in practice: the session only advertises and accepts
        // STARTTLS when `tls_mode` says so, and the composition root supplies
        // an acceptor whenever it does. Handled rather than unwrapped because
        // this crate forbids panicking on a connection path.
        tracing::error!(%peer, "STARTTLS was accepted with no TLS acceptor configured");
        return Ok(());
    };

    let tls_stream = match acceptor.accept(&mut *stream).await {
        Ok(tls_stream) => tls_stream,
        Err(error) => {
            // A failed handshake is ordinary: a client that does not trust the
            // self-signed certificate aborts here. Nothing can be said back —
            // the channel is no longer cleartext SMTP — so it is logged and the
            // connection dropped.
            tracing::debug!(%peer, %error, "TLS handshake failed");
            return Ok(());
        }
    };

    tracing::debug!(%peer, "TLS established");

    open.transcript().note("TLS established (STARTTLS)");

    let (read, write) = tokio::io::split(tls_stream);
    let connection = Connection::with_transcript(
        read,
        write,
        config.max_line_length,
        Some(peer),
        open.transcript().clone(),
    );

    // `greet: false` — the 220 that invited the handshake was the greeting.
    // The client now sends EHLO again over the encrypted channel.
    Session::with_encryption(connection, config, delivery, true)
        .with_connection_id(open.id())
        .with_faults(faults)
        .run_to_outcome(false)
        .await
        .map(|_| ())
}

/// Serves one connection on an implicit-TLS listener (ADR-037).
///
/// The handshake completes before an SMTP session exists, so the `220`
/// greeting is the first thing written *inside* TLS. A client that connects
/// with plaintext SMTP fails the handshake and is dropped without ever
/// reaching the protocol — which is the whole point of the mode, and is
/// asserted by a test.
async fn serve_implicit_tls(
    stream: &mut tokio::net::TcpStream,
    config: Arc<SmtpConfig>,
    delivery: Arc<DeliveryService>,
    acceptor: &TlsAcceptor,
    peer: SocketAddr,
    open: &OpenConnection,
    faults: Arc<FaultRegistry>,
) -> Result<(), sandesh_proto::ProtocolError> {
    let tls_stream = match acceptor.accept(stream).await {
        Ok(tls_stream) => tls_stream,
        Err(error) => {
            // Ordinary: a plaintext client, or one that distrusts the
            // self-signed certificate. Nothing can be replied — this port
            // never speaks cleartext — so it is logged and the socket closed.
            // Emphatically not a fallback to plaintext, which would reinstate
            // the downgrade this mode exists to prevent.
            tracing::debug!(%peer, %error, "implicit TLS handshake failed");
            return Ok(());
        }
    };

    tracing::debug!(%peer, "implicit TLS established");
    open.transcript().note("TLS established (implicit)");

    let (read, write) = tokio::io::split(tls_stream);
    let connection = Connection::with_transcript(
        read,
        write,
        config.max_line_length,
        Some(peer),
        open.transcript().clone(),
    );

    // `encrypted: true` from the outset, so `AUTH` is offered on the first
    // `EHLO` (ADR-033) and `STARTTLS` is never advertised (RFC 3207 §4.2).
    // `greet: true` because, unlike the STARTTLS path, no greeting has been
    // sent yet — this is the client's first reply of any kind.
    Session::with_encryption(connection, config, delivery, true)
        .with_connection_id(open.id())
        .with_faults(faults)
        .run_to_outcome(true)
        .await
        .map(|_| ())
}

/// A bound SMTP server, ready to serve.
pub struct BoundServer {
    listener: Listener,
    config: Arc<SmtpConfig>,
    delivery: Arc<DeliveryService>,
    tls: Option<TlsAcceptor>,
    recorder: ConnectionRecorder,
    /// Runtime fault rules (ADR-084), shared with the API that sets them.
    faults: Arc<FaultRegistry>,
}

impl std::fmt::Debug for BoundServer {
    /// As [`SmtpServer`]: the acceptor is reported as a boolean.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundServer")
            .field("local_addr", &self.listener.local_addr())
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

impl BoundServer {
    /// The address actually bound.
    ///
    /// With port 0 this is the ephemeral port the OS chose — how tests avoid
    /// fixed ports and run in parallel without colliding.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr()
    }

    /// A snapshot of server state.
    #[must_use]
    pub fn status(&self) -> ServerStatus {
        ServerStatus {
            local_addr: self.listener.local_addr(),
            active_connections: self.listener.active_connections(),
            max_connections: self.listener.max_connections(),
        }
    }

    /// Accepts connections until `shutdown` is cancelled.
    ///
    /// Consumes the server, so it cannot be started twice.
    ///
    /// On shutdown the accept loop stops immediately and in-flight sessions are
    /// left to finish on their own. Sessions are already bounded by the command
    /// and data timeouts, so they cannot hang indefinitely, and cutting an
    /// in-progress `DATA` would lose a message the client believes is being
    /// accepted.
    ///
    /// # Errors
    /// Returns [`ServerError::Accept`] if accepting fails in a way that is not
    /// merely one client going away.
    pub async fn serve(self, shutdown: CancellationToken) -> Result<(), ServerError> {
        tracing::info!(
            address = %self.listener.local_addr(),
            max_connections = self.config.max_connections,
            "SMTP server listening"
        );

        loop {
            let mut accepted = match self.listener.accept(&shutdown).await {
                Ok(Some(accepted)) => accepted,
                // Cancelled.
                Ok(None) => break,
                Err(e) if e.is_disconnect() => continue,
                Err(e) => return Err(ServerError::Accept(e)),
            };

            let config = Arc::clone(&self.config);
            let delivery = Arc::clone(&self.delivery);
            let faults = Arc::clone(&self.faults);
            let peer = accepted.peer;

            // Each session runs independently, owning all of its own state.
            // The accepted permit moves with it, so the connection cap is
            // released exactly when the session ends.
            // The whole `Accepted` moves into the task, so its concurrency
            // permit is released when the session ends rather than when the
            // accept loop iterates. That is what makes the cap mean
            // "connections being served" rather than "connections accepted".
            let tls = self.tls.clone();
            let config_tls_mode = self.config.tls_mode;
            // One record per accepted socket: the STARTTLS upgrade below runs a
            // second session over the same socket and shares this transcript.
            let open = self.recorder.open(Some(peer.to_string()));

            tokio::spawn(async move {
                tracing::debug!(%peer, "connection opened");
                // The row must exist before the session records anything that
                // references it (ADR-066).
                let flushing = open.persist_open_streaming().await;

                let encrypted = config_tls_mode == TlsMode::Implicit;
                let disposition = match serve_connection(
                    &mut accepted,
                    config,
                    delivery,
                    tls,
                    peer,
                    &open,
                    faults,
                )
                .await
                {
                    Ok(()) => {
                        tracing::debug!(%peer, "connection closed");
                        ConnectionDisposition::Closed
                    }
                    Err(e) => {
                        tracing::debug!(%peer, error = %e, "connection ended with error");
                        // The client vanished, or the server gave up on it.
                        // Either way the conversation did not end with a
                        // `QUIT`, which is what the viewer needs to show.
                        if e.is_disconnect() {
                            ConnectionDisposition::Disconnected
                        } else {
                            ConnectionDisposition::ServerClosed
                        }
                    }
                };

                // Recorded after the session, so the transcript is complete.
                // A STARTTLS upgrade has already flipped this to encrypted by
                // way of the note in the transcript; the flag reports the
                // transport the connection *ended* on.
                open.close(disposition, encrypted, Some(flushing)).await;

                drop(accepted);
            });
        }

        tracing::info!("SMTP server stopped accepting connections");
        Ok(())
    }
}
