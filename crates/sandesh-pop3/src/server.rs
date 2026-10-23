//! The POP3 server: binding, accepting, and serving connections.
//!
//! Lifecycle is expressed in the type system:
//! [`Pop3Server::bind`] consumes its configuration and yields a
//! [`BoundServer`]; only a `BoundServer` can [`serve`](BoundServer::serve).
//!
//! POP3 supports cleartext (1110, optionally upgradable with `STLS`) and
//! implicit TLS (1995/995) simultaneously.

use std::net::SocketAddr;
use std::sync::Arc;

use sandesh_config::{ConfigError, Pop3Config};
use sandesh_core::{AccountStore, ConnectionDisposition, MailboxStore, MessageStore, Protocol};
use sandesh_proto::{
    Accepted, BindError, CancellationToken, Connection, ConnectionRecorder, Listener,
    OpenConnection, ProtocolError,
};
use tokio_rustls::TlsAcceptor;

use crate::session::{Outcome, Session};

/// Why the POP3 server could not start or run.
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
    #[error(
        "POP3 {posture} is configured but no TLS certificate was supplied: \
         provide one, or disable it"
    )]
    MissingTlsAcceptor {
        /// Which setting needs the certificate.
        posture: &'static str,
    },
}

/// What a running POP3 server reports about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerStatus {
    /// The cleartext address actually bound, when there is one.
    pub local_addr: Option<SocketAddr>,
    /// The implicit-TLS address actually bound, when there is one.
    pub tls_addr: Option<SocketAddr>,
    /// The configured concurrency cap, per listener.
    pub max_connections: usize,
}

/// A configured, not-yet-bound POP3 server.
pub struct Pop3Server {
    config: Arc<Pop3Config>,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Arc<dyn MailboxStore>,
    messages: Arc<dyn MessageStore>,
    tls: Option<TlsAcceptor>,
    /// Records per-connection transcripts (ADR-066). Disabled by default.
    recorder: ConnectionRecorder,
}

impl std::fmt::Debug for Pop3Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pop3Server")
            .field("bind", &self.config.bind)
            .field("implicit_tls_bind", &self.config.implicit_tls_bind)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

impl Pop3Server {
    /// Creates a server from configuration and its storage dependencies.
    #[must_use]
    pub fn new(
        config: Pop3Config,
        accounts: Arc<dyn AccountStore>,
        mailboxes: Arc<dyn MailboxStore>,
        messages: Arc<dyn MessageStore>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            accounts,
            mailboxes,
            messages,
            tls: None,
            recorder: ConnectionRecorder::disabled(Protocol::Pop3),
        }
    }

    /// Supplies the TLS acceptor for `STLS` and the implicit listener.
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

    /// Validates the configuration and binds the listeners.
    ///
    /// Returns operator warnings alongside the bound server.
    ///
    /// # Errors
    /// [`ServerError::Config`], [`ServerError::Bind`], or [`ServerError::MissingTlsAcceptor`].
    pub async fn bind(self) -> Result<(BoundServer, Vec<String>), ServerError> {
        self.config.validate()?;

        if self.config.needs_tls() && self.tls.is_none() {
            return Err(ServerError::MissingTlsAcceptor {
                posture: if self.config.implicit_tls_bind.is_some() {
                    "implicit TLS"
                } else {
                    "STLS"
                },
            });
        }

        let cleartext = match self.config.bind {
            Some(addr) => Some(
                Listener::bind(addr, self.config.max_connections)
                    .await
                    .map_err(ServerError::Bind)?,
            ),
            None => None,
        };

        let tls_listener = match self.config.implicit_tls_bind {
            Some(addr) => Some(
                Listener::bind(addr, self.config.max_connections)
                    .await
                    .map_err(ServerError::Bind)?,
            ),
            None => None,
        };

        let warnings = self.config.warnings();
        let bound = BoundServer {
            cleartext,
            tls_listener,
            config: self.config,
            accounts: self.accounts,
            mailboxes: self.mailboxes,
            messages: self.messages,
            tls: self.tls,
            recorder: self.recorder,
        };

        Ok((bound, warnings))
    }
}

/// A bound POP3 server ready to accept connections.
pub struct BoundServer {
    cleartext: Option<Listener>,
    tls_listener: Option<Listener>,
    config: Arc<Pop3Config>,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Arc<dyn MailboxStore>,
    messages: Arc<dyn MessageStore>,
    tls: Option<TlsAcceptor>,
    recorder: ConnectionRecorder,
}

impl std::fmt::Debug for BoundServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundServer")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl BoundServer {
    /// Addresses and limits of the bound listeners.
    #[must_use]
    pub fn status(&self) -> ServerStatus {
        ServerStatus {
            local_addr: self.cleartext.as_ref().map(Listener::local_addr),
            tls_addr: self.tls_listener.as_ref().map(Listener::local_addr),
            max_connections: self.config.max_connections,
        }
    }

    /// Serves connections until cancelled.
    ///
    /// # Errors
    /// [`ServerError::Accept`] if accepting fails.
    pub async fn serve(self, shutdown: CancellationToken) -> Result<(), ServerError> {
        let Self {
            cleartext,
            tls_listener,
            config,
            accounts,
            mailboxes,
            messages,
            tls,
            recorder,
        } = self;

        let cleartext_task = cleartext.map(|listener| {
            let (config, accounts, mailboxes, messages, tls, token) = (
                Arc::clone(&config),
                Arc::clone(&accounts),
                Arc::clone(&mailboxes),
                Arc::clone(&messages),
                tls.clone(),
                shutdown.clone(),
            );
            let recorder = recorder.clone();
            tokio::spawn(async move {
                accept_loop(
                    listener, config, accounts, mailboxes, messages, tls, false, recorder, token,
                )
                .await
            })
        });

        let tls_task = tls_listener.map(|listener| {
            let (config, accounts, mailboxes, messages, tls, token) = (
                Arc::clone(&config),
                Arc::clone(&accounts),
                Arc::clone(&mailboxes),
                Arc::clone(&messages),
                tls.clone(),
                shutdown.clone(),
            );
            let recorder = recorder.clone();
            tokio::spawn(async move {
                accept_loop(
                    listener, config, accounts, mailboxes, messages, tls, true, recorder, token,
                )
                .await
            })
        });

        for task in [cleartext_task, tls_task].into_iter().flatten() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(join_error) => {
                    tracing::error!(%join_error, "a POP3 accept loop terminated abnormally");
                }
            }
        }

        tracing::info!("POP3 server stopped accepting connections");
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: Listener,
    config: Arc<Pop3Config>,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Arc<dyn MailboxStore>,
    messages: Arc<dyn MessageStore>,
    tls: Option<TlsAcceptor>,
    implicit_tls: bool,
    recorder: ConnectionRecorder,
    shutdown: CancellationToken,
) -> Result<(), ServerError> {
    tracing::info!(
        address = %listener.local_addr(),
        implicit_tls,
        max_connections = config.max_connections,
        "POP3 server listening"
    );

    loop {
        let mut accepted = match listener.accept(&shutdown).await {
            Ok(Some(accepted)) => accepted,
            Ok(None) => break,
            Err(error) if error.is_disconnect() => continue,
            Err(error) => return Err(ServerError::Accept(error)),
        };

        let config = Arc::clone(&config);
        let accounts = Arc::clone(&accounts);
        let mailboxes = Arc::clone(&mailboxes);
        let messages = Arc::clone(&messages);
        let tls = tls.clone();
        let peer = accepted.peer;
        // One record per accepted socket, shared across an STLS upgrade.
        let open = recorder.open(Some(peer.to_string()));

        tokio::spawn(async move {
            tracing::debug!(%peer, implicit_tls, "POP3 connection opened");
            // The row must exist before the session records anything that
            // references it (ADR-066).
            let flushing = open.persist_open_streaming().await;

            let result = serve_connection(
                &mut accepted,
                config,
                accounts,
                mailboxes,
                messages,
                tls,
                implicit_tls,
                peer,
                &open,
            )
            .await;

            let disposition = match result {
                Ok(()) => {
                    tracing::debug!(%peer, "POP3 connection closed");
                    ConnectionDisposition::Closed
                }
                Err(ref error) => {
                    tracing::debug!(%peer, %error, "POP3 connection ended with error");
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

#[allow(clippy::too_many_arguments)]
async fn serve_connection(
    accepted: &mut Accepted,
    config: Arc<Pop3Config>,
    accounts: Arc<dyn AccountStore>,
    mailboxes: Arc<dyn MailboxStore>,
    messages: Arc<dyn MessageStore>,
    tls: Option<TlsAcceptor>,
    implicit_tls: bool,
    peer: SocketAddr,
    open: &OpenConnection,
) -> Result<(), ProtocolError> {
    let Accepted { stream, .. } = accepted;

    if implicit_tls {
        let Some(acceptor) = tls else {
            tracing::error!(%peer, "implicit TLS listener has no acceptor; refusing");
            return Ok(());
        };

        let tls_stream = match acceptor.accept(&mut *stream).await {
            Ok(tls_stream) => tls_stream,
            Err(error) => {
                tracing::debug!(%peer, %error, "POP3 implicit-TLS handshake failed");
                return Ok(());
            }
        };

        let (read, write) = tokio::io::split(tls_stream);
        let connection = Connection::with_transcript(
            read,
            write,
            config.max_line_length,
            Some(peer),
            open.transcript().clone(),
        );
        Session::new(connection, config, accounts, mailboxes, messages, true)
            .run(true)
            .await?;
        return Ok(());
    }

    // Cleartext
    let (read, write) = tokio::io::split(&mut *stream);
    let connection = Connection::with_transcript(
        read,
        write,
        config.max_line_length,
        Some(peer),
        open.transcript().clone(),
    );
    let session = Session::new(
        connection,
        Arc::clone(&config),
        Arc::clone(&accounts),
        Arc::clone(&mailboxes),
        Arc::clone(&messages),
        false,
    );

    match session.run(true).await? {
        Outcome::Closed => return Ok(()),
        Outcome::UpgradeToTls => {}
    }

    let Some(acceptor) = tls else {
        tracing::error!(%peer, "STLS was accepted with no TLS acceptor configured");
        return Ok(());
    };

    let tls_stream = match acceptor.accept(&mut *stream).await {
        Ok(tls_stream) => tls_stream,
        Err(error) => {
            tracing::debug!(%peer, %error, "POP3 STLS handshake failed");
            return Ok(());
        }
    };

    tracing::debug!(%peer, "POP3 STLS established");

    let (read, write) = tokio::io::split(tls_stream);
    let connection = Connection::with_transcript(
        read,
        write,
        config.max_line_length,
        Some(peer),
        open.transcript().clone(),
    );

    // RFC 2595 §4: server does NOT re-issue greeting after STLS.
    Session::new(connection, config, accounts, mailboxes, messages, true)
        .run(false)
        .await?;

    Ok(())
}
