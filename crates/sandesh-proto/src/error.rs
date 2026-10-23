//! Transport and framing errors.
//!
//! These describe what went wrong at the *connection* level. Mapping them onto
//! a protocol's own vocabulary — an SMTP reply code, an IMAP `BAD` — is the
//! adapter's job, which is what keeps this crate protocol-agnostic.

use std::io;

/// A failure reading from or writing to a client connection.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// The peer closed the connection.
    #[error("connection closed by peer")]
    Closed,

    /// A line exceeded the configured limit.
    ///
    /// Carries the limit rather than the observed length, because the observed
    /// length is unbounded — that is the whole point of the limit.
    #[error("line exceeded the {limit} byte limit")]
    LineTooLong {
        /// The configured maximum.
        limit: usize,
    },

    /// The payload being read exceeded the configured limit.
    #[error("payload exceeded the {limit} byte limit")]
    PayloadTooLarge {
        /// The configured maximum.
        limit: usize,
    },

    /// No input arrived within the timeout.
    #[error("timed out after {seconds}s waiting for input")]
    Timeout {
        /// How long was waited.
        seconds: u64,
    },

    /// An underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

impl ProtocolError {
    /// Whether the connection can continue after this error.
    ///
    /// A limit breach is reportable — the protocol can answer with an error
    /// reply and carry on. A closed socket or an I/O failure is not.
    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::LineTooLong { .. } | Self::PayloadTooLarge { .. }
        )
    }

    /// Whether the peer simply went away.
    ///
    /// Distinguished from a genuine failure so a session can close quietly
    /// instead of logging an error for an ordinary disconnect.
    #[must_use]
    pub fn is_disconnect(&self) -> bool {
        match self {
            Self::Closed => true,
            Self::Io(e) => matches!(
                e.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionAborted
            ),
            _ => false,
        }
    }
}

/// A transport-level result.
pub type ProtocolResult<T> = Result<T, ProtocolError>;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn limit_breaches_are_recoverable() {
        assert!(ProtocolError::LineTooLong { limit: 512 }.is_recoverable());
        assert!(ProtocolError::PayloadTooLarge { limit: 1 }.is_recoverable());
    }

    #[test]
    fn transport_failures_are_not_recoverable() {
        assert!(!ProtocolError::Closed.is_recoverable());
        assert!(!ProtocolError::Timeout { seconds: 30 }.is_recoverable());
        assert!(!ProtocolError::Io(io::Error::other("boom")).is_recoverable());
    }

    #[test]
    fn ordinary_disconnects_are_recognised() {
        assert!(ProtocolError::Closed.is_disconnect());
        assert!(ProtocolError::Io(io::Error::from(io::ErrorKind::ConnectionReset)).is_disconnect());
        assert!(!ProtocolError::Io(io::Error::other("disk")).is_disconnect());
    }
}
