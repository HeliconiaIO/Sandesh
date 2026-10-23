//! TCP acceptance, connection limiting and shutdown.
//!
//! Everything here is protocol-agnostic: it accepts sockets, bounds how many
//! are live at once, and stops cleanly. What is spoken over them is the
//! adapter's business.

use crate::ProtocolResult;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// A bound TCP listener with a concurrency cap.
#[derive(Debug)]
pub struct Listener {
    inner: TcpListener,
    permits: Arc<Semaphore>,
    local_addr: SocketAddr,
    max_connections: usize,
}

impl Listener {
    /// Binds `addr`, allowing at most `max_connections` live at once.
    ///
    /// # Errors
    /// Returns [`BindError`] with the address included, so "port already in
    /// use" is actionable rather than an opaque `EADDRINUSE`.
    pub async fn bind(addr: SocketAddr, max_connections: usize) -> Result<Self, BindError> {
        let inner = TcpListener::bind(addr).await.map_err(|source| BindError {
            addr,
            kind: source.kind(),
            source,
        })?;
        let local_addr = inner.local_addr().map_err(|source| BindError {
            addr,
            kind: source.kind(),
            source,
        })?;

        Ok(Self {
            inner,
            permits: Arc::new(Semaphore::new(max_connections)),
            local_addr,
            max_connections,
        })
    }

    /// The address actually bound.
    ///
    /// Differs from the requested address when port 0 was given, which is how
    /// tests get an ephemeral port and avoid colliding when run in parallel.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Accepts a connection, or returns `None` when `shutdown` is cancelled.
    ///
    /// Waits for a concurrency permit *before* accepting rather than after.
    /// Accepting first and then rejecting would mean the server itself creates
    /// the sockets it cannot serve; leaving them in the kernel backlog lets the
    /// OS apply backpressure, which is what the backlog is for.
    ///
    /// # Errors
    /// Fails if the accept itself fails.
    pub async fn accept(&self, shutdown: &CancellationToken) -> ProtocolResult<Option<Accepted>> {
        let permit = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(None),
            permit = self.permits.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                // Only happens if the semaphore is closed, which means shutdown.
                Err(_) => return Ok(None),
            },
        };

        let (stream, peer) = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(None),
            result = self.inner.accept() => result?,
        };

        // Interactive line protocols suffer badly from Nagle: a reply small
        // enough to be held back stalls the client for the delayed-ack window.
        let _ = stream.set_nodelay(true);

        Ok(Some(Accepted {
            stream,
            peer,
            _permit: permit,
        }))
    }

    /// Connections currently live.
    ///
    /// Derived from outstanding permits, so it cannot drift from reality the
    /// way a manually maintained counter would.
    #[must_use]
    pub fn active_connections(&self) -> usize {
        self.max_connections
            .saturating_sub(self.permits.available_permits())
    }

    /// The configured concurrency cap.
    #[must_use]
    pub const fn max_connections(&self) -> usize {
        self.max_connections
    }
}

/// An accepted connection, holding its concurrency permit.
///
/// Dropping this returns the permit, so the cap is enforced by ownership rather
/// than by remembering to decrement a counter.
#[derive(Debug)]
pub struct Accepted {
    /// The connected socket.
    pub stream: TcpStream,
    /// The peer address.
    pub peer: SocketAddr,
    _permit: OwnedSemaphorePermit,
}

/// A listener failed to bind.
#[derive(Debug, thiserror::Error)]
#[error("failed to bind {addr}: {}", explain(*.kind, .addr, .source))]
pub struct BindError {
    /// The address that could not be bound.
    pub addr: SocketAddr,
    /// The underlying error kind.
    pub kind: io::ErrorKind,
    /// The underlying error.
    #[source]
    pub source: io::Error,
}

/// Turns a bind failure into something a developer can act on.
fn explain(kind: io::ErrorKind, addr: &SocketAddr, source: &io::Error) -> String {
    match kind {
        io::ErrorKind::AddrInUse => format!(
            "port {} is already in use — another Sandesh instance or a different \
             mail server may be running",
            addr.port()
        ),
        io::ErrorKind::PermissionDenied => format!(
            "permission denied — binding port {} may require elevated privileges \
             (ports below 1024 usually do)",
            addr.port()
        ),
        io::ErrorKind::AddrNotAvailable => {
            format!("address {} is not available on this machine", addr.ip())
        }
        // The kind alone can be `Uncategorized`, which names nothing; the OS
        // error carries the code that does (e.g. Winsock's `os error 10106`).
        _ => source.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn ephemeral() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    }

    #[tokio::test]
    async fn binds_an_ephemeral_port() {
        let listener = Listener::bind(ephemeral(), 4).await.expect("bind");
        assert_ne!(listener.local_addr().port(), 0, "a real port is assigned");
        assert!(listener.local_addr().ip().is_loopback());
    }

    #[tokio::test]
    async fn port_conflict_produces_an_actionable_error() {
        let first = Listener::bind(ephemeral(), 4).await.expect("first bind");
        let taken = first.local_addr();

        let err = Listener::bind(taken, 4)
            .await
            .expect_err("second bind fails");
        let text = err.to_string();

        assert!(
            text.contains(&taken.to_string()),
            "names the address: {text}"
        );
        assert!(text.contains("already in use"), "explains why: {text}");
    }

    #[tokio::test]
    async fn shutdown_stops_accepting() {
        let listener = Listener::bind(ephemeral(), 4).await.expect("bind");
        let shutdown = CancellationToken::new();
        shutdown.cancel();

        let accepted = listener.accept(&shutdown).await.expect("accept returns");
        assert!(accepted.is_none(), "a cancelled token must stop accepting");
    }

    #[tokio::test]
    async fn connection_accounting_follows_permits() {
        let listener = Listener::bind(ephemeral(), 2).await.expect("bind");
        let addr = listener.local_addr();
        let shutdown = CancellationToken::new();
        assert_eq!(listener.active_connections(), 0);

        let client = TcpStream::connect(addr).await.expect("connect");
        let accepted = listener
            .accept(&shutdown)
            .await
            .expect("accept")
            .expect("connection");
        assert_eq!(listener.active_connections(), 1, "permit is held");

        drop(accepted);
        drop(client);
        // Permits return on drop, so the cap is enforced by ownership.
        assert_eq!(listener.active_connections(), 0, "permit is released");
    }

    #[tokio::test]
    async fn accepts_a_connection() {
        let listener = Listener::bind(ephemeral(), 4).await.expect("bind");
        let addr = listener.local_addr();
        let shutdown = CancellationToken::new();

        tokio::spawn(async move {
            let _ = TcpStream::connect(addr).await;
        });

        let accepted = listener
            .accept(&shutdown)
            .await
            .expect("accept")
            .expect("a connection");
        assert_eq!(accepted.peer.ip(), addr.ip());
    }
}
