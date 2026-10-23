//! `DATA` content collection: transport framing removal (RFC 5321 §4.5.2).
//!
//! Two transformations separate what arrives on the wire from what is stored,
//! and getting either wrong corrupts every message:
//!
//! 1. **Termination.** Content ends at `<CRLF>.<CRLF>`. That terminator is
//!    framing and must not be stored.
//! 2. **Dot-unstuffing.** A client prefixes any line starting with `.` with a
//!    second `.`, so the terminator cannot appear in content. The server
//!    removes it. A body line `..hidden` was sent as content `.hidden`.
//!
//! # Bytes throughout
//!
//! Content is `Vec<u8>`, never `String`. A message body may be any byte
//! sequence — 8-bit MIME, binary attachments, invalid UTF-8 the sender chose
//! deliberately — and converting through UTF-8 would corrupt it. The
//! raw-fidelity guarantee (ADR-003) starts here.
//!
//! # Size enforcement
//!
//! The limit is checked **as lines arrive**, not after buffering. Otherwise a
//! client could exhaust memory by sending an unbounded stream, and the
//! rejection would come too late to matter.

use sandesh_proto::ProtocolResult;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// The outcome of collecting `DATA`.
#[derive(Debug)]
pub enum DataOutcome {
    /// Content was received in full.
    Complete(Vec<u8>),
    /// The content exceeded the limit.
    ///
    /// The stream was still read to its terminator, so the connection stays
    /// synchronised and the session can reply `552` and continue rather than
    /// having to disconnect.
    TooLarge {
        /// The configured limit.
        limit: usize,
    },
}

/// Collects `DATA` content until `<CRLF>.<CRLF>`.
///
/// # Errors
/// Fails if the peer disconnects mid-content, a line exceeds the line limit, or
/// the timeout elapses.
pub async fn collect<R, W>(
    connection: &mut sandesh_proto::Connection<R, W>,
    max_size: usize,
    timeout: Duration,
) -> ProtocolResult<DataOutcome>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut content: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut exceeded = false;

    loop {
        // Content, not a command: never transcribed (ADR-066).
        let line = connection.read_content_line(timeout).await?;

        // The terminator is a line containing exactly one dot.
        if line == b"." {
            return Ok(if exceeded {
                DataOutcome::TooLarge { limit: max_size }
            } else {
                DataOutcome::Complete(content)
            });
        }

        // Undo dot-stuffing: a leading '.' added by the client is removed.
        let line = if let Some(rest) = line.strip_prefix(b".") {
            rest
        } else {
            line
        };

        // Enforce the limit as content arrives. Once exceeded, stop
        // accumulating but keep reading, so the stream stays framed and the
        // session can answer 552 without dropping the connection.
        if !exceeded {
            // +2 for the CRLF this line will contribute.
            if content.len() + line.len() + 2 > max_size {
                exceeded = true;
                content = Vec::new();
                content.shrink_to_fit();
            } else {
                content.extend_from_slice(line);
                content.extend_from_slice(b"\r\n");
            }
        }
    }
}

/// Applies dot-stuffing, for tests and any future relay path.
///
/// The inverse of what [`collect`] undoes.
#[must_use]
pub fn stuff(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 16);
    let mut at_line_start = true;

    for &byte in content {
        if at_line_start && byte == b'.' {
            out.push(b'.');
        }
        out.push(byte);
        at_line_start = byte == b'\n';
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use sandesh_proto::Connection;
    use tokio::io::{AsyncWriteExt, duplex};

    const T: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn message_content_never_reaches_the_transcript() {
        // The privacy property ADR-066 turns on. DATA is framed exactly like a
        // command, so the ordinary read path would transcribe every body line
        // — including the password-reset links and one-time codes this server
        // exists to hold.
        let (mut client, server) = duplex(64 * 1024);
        let (read, write) = tokio::io::split(server);
        let transcript = sandesh_proto::Transcript::recording();
        let mut connection =
            Connection::with_transcript(read, write, 64 * 1024, None, transcript.clone());

        let wire = b"Subject: reset\r\n\r\nyour one-time code is 123456\r\n.\r\n";
        tokio::spawn(async move {
            let _ = client.write_all(wire).await;
        });

        let outcome = collect(&mut connection, 1024 * 1024, T)
            .await
            .expect("collect");
        match outcome {
            DataOutcome::Complete(content) => {
                assert!(
                    String::from_utf8_lossy(&content).contains("123456"),
                    "the server must still receive the real content"
                );
            }
            DataOutcome::TooLarge { .. } => panic!("unexpectedly rejected"),
        }

        let recorded = transcript.snapshot().0.join("\n");
        assert!(
            !recorded.contains("123456"),
            "a one-time code reached the transcript: {recorded}"
        );
        assert!(
            !recorded.contains("Subject: reset"),
            "message headers are content too: {recorded}"
        );
    }

    /// Feeds `wire` to a collector and returns the outcome.
    async fn collect_wire(wire: &[u8], max_size: usize) -> DataOutcome {
        let (mut client, server) = duplex(64 * 1024);
        let (read, write) = tokio::io::split(server);
        let mut connection = Connection::new(read, write, 64 * 1024, None);

        let payload = wire.to_vec();
        tokio::spawn(async move {
            let _ = client.write_all(&payload).await;
        });

        collect(&mut connection, max_size, T)
            .await
            .unwrap_or(DataOutcome::TooLarge { limit: 0 })
    }

    fn content_of(outcome: DataOutcome) -> Vec<u8> {
        match outcome {
            DataOutcome::Complete(bytes) => bytes,
            DataOutcome::TooLarge { .. } => panic!("expected complete content"),
        }
    }

    #[tokio::test]
    async fn terminator_is_not_stored() {
        let stored = content_of(collect_wire(b"Subject: hi\r\n\r\nbody\r\n.\r\n", 4096).await);
        assert_eq!(stored, b"Subject: hi\r\n\r\nbody\r\n");
        assert!(
            !stored.ends_with(b".\r\n"),
            "the SMTP terminator is framing, not content"
        );
    }

    #[tokio::test]
    async fn dot_stuffing_is_reversed() {
        // The client sent "..hidden"; the content is ".hidden".
        let stored = content_of(collect_wire(b"line\r\n..hidden\r\n.\r\n", 4096).await);
        assert_eq!(stored, b"line\r\n.hidden\r\n");
    }

    #[tokio::test]
    async fn a_line_of_many_dots_loses_exactly_one() {
        let stored = content_of(collect_wire(b"...\r\n.\r\n", 4096).await);
        assert_eq!(stored, b"..\r\n", "only the stuffing dot is removed");
    }

    #[tokio::test]
    async fn dots_not_at_line_start_are_untouched() {
        let stored = content_of(collect_wire(b"a.b.c\r\nx . y\r\n.\r\n", 4096).await);
        assert_eq!(stored, b"a.b.c\r\nx . y\r\n");
    }

    #[tokio::test]
    async fn empty_content_is_valid() {
        // A message with no content at all is unusual but legal.
        let stored = content_of(collect_wire(b".\r\n", 4096).await);
        assert!(stored.is_empty());
    }

    #[tokio::test]
    async fn blank_lines_are_preserved() {
        // The header/body separator depends on this.
        let stored = content_of(collect_wire(b"From: a\r\n\r\nbody\r\n.\r\n", 4096).await);
        assert_eq!(stored, b"From: a\r\n\r\nbody\r\n");
    }

    #[tokio::test]
    async fn arbitrary_bytes_survive() {
        // 8-bit and invalid-UTF-8 content must pass through untouched.
        let mut wire = b"Content-Type: application/octet-stream\r\n\r\n".to_vec();
        wire.extend_from_slice(&[0xff, 0xfe, 0x00, 0x01, b'A']);
        wire.extend_from_slice(b"\r\n.\r\n");

        let stored = content_of(collect_wire(&wire, 4096).await);
        assert!(
            stored.ends_with(&[0xff, 0xfe, 0x00, 0x01, b'A', b'\r', b'\n']),
            "binary content must not be mangled"
        );
    }

    #[tokio::test]
    async fn oversize_content_is_reported_without_buffering_it() {
        let mut wire = Vec::new();
        for _ in 0..500 {
            wire.extend_from_slice(&[b'x'; 100]);
            wire.extend_from_slice(b"\r\n");
        }
        wire.extend_from_slice(b".\r\n");

        let outcome = collect_wire(&wire, 1024).await;
        assert!(
            matches!(outcome, DataOutcome::TooLarge { limit: 1024 }),
            "the limit must be enforced during collection"
        );
    }

    #[tokio::test]
    async fn stream_stays_framed_after_an_oversize_message() {
        // The terminator is still consumed, so the session can continue.
        let mut wire = vec![b'x'; 5000];
        wire.extend_from_slice(b"\r\n.\r\n");

        let (mut client, server) = duplex(64 * 1024);
        let (read, write) = tokio::io::split(server);
        let mut connection = Connection::new(read, write, 64 * 1024, None);

        tokio::spawn(async move {
            let _ = client.write_all(&wire).await;
            let _ = client.write_all(b"QUIT\r\n").await;
        });

        let outcome = collect(&mut connection, 1024, T).await.expect("collect");
        assert!(matches!(outcome, DataOutcome::TooLarge { .. }));

        // The next line is the following command, not stray content.
        let next = connection.read_line(T).await.unwrap_or_default().to_vec();
        assert_eq!(next, b"QUIT", "the connection must stay synchronised");
    }

    #[test]
    fn stuffing_round_trips() {
        for content in [
            &b"plain\r\n"[..],
            b".leading\r\n",
            b"..double\r\n",
            b"a\r\n.\r\nb\r\n",
            b"",
        ] {
            let stuffed = stuff(content);
            // Manually unstuff to confirm the inverse relationship.
            let mut restored = Vec::new();
            let mut at_start = true;
            for byte in stuffed.iter().copied() {
                if at_start && byte == b'.' {
                    at_start = false;
                    continue; // drop the stuffing dot
                }
                restored.push(byte);
                at_start = byte == b'\n';
            }
            assert_eq!(restored, content, "stuffing must be reversible");
        }
    }
}
