//! The connection record — one accepted socket and what was said on it.
//!
//! A connection is not a delivery (ADR-066). One SMTP connection can carry
//! many transactions, a connection that delivers nothing still has a story
//! worth telling — a probe, a rejected recipient, a failed `STARTTLS` — and
//! IMAP and POP3 produce no delivery record at all. So the transcript hangs
//! here rather than off `deliveries`.
//!
//! ## What a transcript is allowed to contain
//!
//! Command and response lines only. `DATA` payloads are replaced with a byte
//! count and credentials are never captured — not redacted at render, but
//! never written into the buffer in the first place. The reasoning is the same
//! one that governs logging (`sandesh-app`'s `logging` module): a developer
//! mail server holds exactly the material people paste into bug reports —
//! password-reset links, one-time codes — and a transcript is a log. The
//! message body is retrievable deliberately through the API, which is where it
//! belongs.
//!
//! A transcript is therefore **not** a byte-exact recording, by construction.

use crate::{ConnectionId, Timestamp};

/// Which protocol server accepted the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// SMTP, on the submission or implicit-TLS listener.
    Smtp,
    /// IMAP.
    Imap,
    /// POP3.
    Pop3,
}

impl Protocol {
    /// The persisted discriminant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Smtp => "smtp",
            Self::Imap => "imap",
            Self::Pop3 => "pop3",
        }
    }

    /// Parses a persisted discriminant.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "smtp" => Some(Self::Smtp),
            "imap" => Some(Self::Imap),
            "pop3" => Some(Self::Pop3),
            _ => None,
        }
    }
}

/// How a connection ended.
///
/// Kept as a domain enum rather than free text so the trace viewer can filter
/// on it, but persisted as a string so a new disposition needs no migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDisposition {
    /// The client said goodbye — `QUIT`, `LOGOUT` — and the server closed.
    Closed,
    /// The client vanished without a closing command.
    Disconnected,
    /// The server closed it: a timeout, or a protocol error it could not answer.
    ServerClosed,
    /// The server was shutting down.
    ShuttingDown,
}

impl ConnectionDisposition {
    /// The persisted discriminant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Disconnected => "disconnected",
            Self::ServerClosed => "server_closed",
            Self::ShuttingDown => "shutting_down",
        }
    }

    /// Parses a persisted discriminant.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "closed" => Some(Self::Closed),
            "disconnected" => Some(Self::Disconnected),
            "server_closed" => Some(Self::ServerClosed),
            "shutting_down" => Some(Self::ShuttingDown),
            _ => None,
        }
    }
}

/// One accepted socket, with the transcript of what was said on it.
///
/// Named `ConnectionRecord` rather than `Connection` to leave that name to
/// `sandesh-proto`'s live transport type: one is a row, the other is a socket,
/// and confusing them in a signature would be easy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionRecord {
    /// The record.
    pub id: ConnectionId,
    /// Which protocol server accepted it.
    pub protocol: Protocol,
    /// The client address, when the transport has one.
    pub peer: Option<String>,
    /// Whether the transport was encrypted **when the connection closed**.
    ///
    /// A `STARTTLS` connection is one record that begins in cleartext and ends
    /// encrypted (ADR-066), so this is the final state, not the initial one.
    /// The transcript itself shows where the upgrade happened.
    pub encrypted: bool,
    /// When the socket was accepted.
    pub opened_at: Timestamp,
    /// When it closed. `None` while the connection is still open.
    pub closed_at: Option<Timestamp>,
    /// How it ended. `None` while still open.
    pub disposition: Option<ConnectionDisposition>,
    /// The transcript, one entry per line.
    pub transcript: Vec<String>,
    /// Whether the transcript hit a cap and was cut short, so a reader can
    /// tell a quiet connection from a truncated one.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_discriminants_round_trip() {
        // The persisted string is a storage contract: a typo here silently
        // orphans every row written before it.
        for protocol in [Protocol::Smtp, Protocol::Imap, Protocol::Pop3] {
            assert_eq!(Protocol::parse(protocol.as_str()), Some(protocol));
        }
        assert_eq!(Protocol::parse("jmap"), None);
    }

    #[test]
    fn disposition_discriminants_round_trip() {
        for disposition in [
            ConnectionDisposition::Closed,
            ConnectionDisposition::Disconnected,
            ConnectionDisposition::ServerClosed,
            ConnectionDisposition::ShuttingDown,
        ] {
            assert_eq!(
                ConnectionDisposition::parse(disposition.as_str()),
                Some(disposition)
            );
        }
        assert_eq!(ConnectionDisposition::parse("exploded"), None);
    }

    #[test]
    fn protocol_discriminants_match_the_schema_check_constraint() {
        // `connections.protocol` has CHECK (protocol IN ('smtp','imap','pop3')).
        // A value this enum can produce that the constraint rejects would fail
        // at INSERT, in production, on a real connection.
        for protocol in [Protocol::Smtp, Protocol::Imap, Protocol::Pop3] {
            assert!(
                ["smtp", "imap", "pop3"].contains(&protocol.as_str()),
                "{} is not permitted by the schema",
                protocol.as_str()
            );
        }
    }
}
