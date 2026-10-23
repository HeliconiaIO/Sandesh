//! A framed client connection: line reading plus buffered writing.

use crate::{Direction, LineReader, ProtocolResult, Transcript};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// Above this size, an outbound write is message content being served rather
/// than a protocol reply, and is recorded as a byte count instead of text.
/// Comfortably larger than the longest real reply (an IMAP `CAPABILITY` line
/// or a multi-line `EHLO`) and far smaller than any message body.
const OUTPUT_LINE_LIMIT: usize = 4096;

/// One client connection, framed into lines.
///
/// Generic over the transport rather than tied to `TcpStream`, which is what
/// lets protocol tests drive a session over an in-memory duplex — no sockets,
/// no ports, no flakiness — and what will let a TLS stream be substituted in
/// Phase 4.5 without touching any protocol code.
#[derive(Debug)]
pub struct Connection<R, W> {
    reader: LineReader<R>,
    writer: W,
    peer: Option<SocketAddr>,
    /// The transcript for the socket this connection runs over (ADR-066).
    ///
    /// Shared rather than owned: a `STARTTLS` upgrade builds a *second*
    /// `Connection` over the same socket, and both must append to one
    /// conversation. Defaults to [`Transcript::disabled`], so a connection
    /// built without one records nothing and costs nothing.
    transcript: Transcript,
}

impl<R, W> Connection<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Wraps a read and write half, recording nothing.
    pub fn new(reader: R, writer: W, max_line: usize, peer: Option<SocketAddr>) -> Self {
        Self::with_transcript(reader, writer, max_line, peer, Transcript::disabled())
    }

    /// Wraps a read and write half, recording into `transcript`.
    ///
    /// The listener passes the same transcript to every `Connection` it builds
    /// over one accepted socket, so a `STARTTLS` upgrade stays one transcript
    /// (ADR-066).
    pub fn with_transcript(
        reader: R,
        writer: W,
        max_line: usize,
        peer: Option<SocketAddr>,
        transcript: Transcript,
    ) -> Self {
        Self {
            reader: LineReader::new(reader, max_line),
            writer,
            peer,
            transcript,
        }
    }

    /// The transcript this connection records into.
    #[must_use]
    pub const fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// The peer address, when the transport has one.
    #[must_use]
    pub const fn peer(&self) -> Option<SocketAddr> {
        self.peer
    }

    /// Consumes the connection, returning its transport halves.
    ///
    /// The second element is whatever was buffered but not yet consumed. A
    /// `STARTTLS` upgrade **must** refuse to proceed when it is non-empty:
    /// anything a client sent before the handshake arrived in cleartext and is
    /// therefore attacker-controlled, so carrying it across the boundary would
    /// let an active attacker inject commands that appear to have come from
    /// inside the TLS session (RFC 3207 §4).
    ///
    /// Returned rather than silently dropped so the caller has to make that
    /// decision explicitly.
    #[must_use]
    pub fn into_parts(self) -> (R, W, Vec<u8>, Option<SocketAddr>) {
        let (reader, pending) = self.reader.into_parts();
        (reader, self.writer, pending, self.peer)
    }

    /// Reads one line, without its terminator.
    ///
    /// # Errors
    /// See [`LineReader::read_line`].
    pub async fn read_line(&mut self, timeout: Duration) -> ProtocolResult<&[u8]> {
        let line = self.reader.read_line(timeout).await?;
        self.transcript.line(Direction::In, line);
        Ok(line)
    }

    /// Reads one line into an owned buffer.
    ///
    /// # Errors
    /// See [`LineReader::read_line`].
    pub async fn read_line_owned(&mut self, timeout: Duration) -> ProtocolResult<Vec<u8>> {
        let line = self.reader.read_line_owned(timeout).await?;
        self.transcript.line(Direction::In, &line);
        Ok(line)
    }

    /// Reads one line of message *content*, which is never transcribed.
    ///
    /// SMTP `DATA` is collected line by line through the same framing a command
    /// uses, so the ordinary [`read_line`](Self::read_line) would put every
    /// body line into the transcript — the exact disclosure ADR-066 forbids.
    /// Content has its own method so the distinction is made by the caller that
    /// knows it, rather than guessed at by the recorder.
    ///
    /// # Errors
    /// See [`LineReader::read_line`].
    pub async fn read_content_line(&mut self, timeout: Duration) -> ProtocolResult<&[u8]> {
        self.reader.read_line(timeout).await
    }

    /// Reads exactly `length` bytes, for a counted payload such as an IMAP
    /// literal.
    ///
    /// `length` is validated against `max_payload` before anything is
    /// allocated. See [`LineReader::read_exact`].
    ///
    /// # Errors
    /// As [`LineReader::read_exact`].
    pub async fn read_exact_owned(
        &mut self,
        length: usize,
        max_payload: usize,
        timeout: Duration,
    ) -> ProtocolResult<Vec<u8>> {
        let payload = self.reader.read_exact(length, max_payload, timeout).await?;
        self.transcript
            .elided(Direction::In, "literal", payload.len());
        Ok(payload)
    }

    /// Whether input is already buffered.
    ///
    /// A protocol uses this to know a client pipelined, and so to avoid
    /// flushing between replies it can batch.
    #[must_use]
    pub fn has_buffered_input(&self) -> bool {
        self.reader.buffered() > 0
    }

    /// Discards buffered input, resynchronising after a framing error.
    pub fn discard_input(&mut self) {
        self.reader.discard();
    }

    /// Writes bytes without flushing.
    ///
    /// # Errors
    /// Fails on an I/O error.
    pub async fn write(&mut self, bytes: &[u8]) -> ProtocolResult<()> {
        self.record_output(bytes);
        self.writer.write_all(bytes).await?;
        Ok(())
    }

    /// Writes bytes and flushes.
    ///
    /// # Errors
    /// Fails on an I/O error.
    pub async fn write_flush(&mut self, bytes: &[u8]) -> ProtocolResult<()> {
        self.record_output(bytes);
        self.writer.write_all(bytes).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Records an outbound write, splitting it into protocol lines.
    ///
    /// A single write can carry a whole multi-line reply — an SMTP `EHLO`
    /// response, an IMAP `FETCH` — and recording it as one transcript entry
    /// would make the conversation unreadable.
    ///
    /// A large write is message content being served (`RETR`, `FETCH BODY[]`)
    /// rather than a reply, so it is elided by size like an inbound literal.
    fn record_output(&self, bytes: &[u8]) {
        if !self.transcript.is_recording() {
            return;
        }
        if bytes.len() > OUTPUT_LINE_LIMIT {
            self.transcript.elided(Direction::Out, "body", bytes.len());
            return;
        }
        for line in bytes.split(|b| *b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if !line.is_empty() {
                self.transcript.line(Direction::Out, line);
            }
        }
    }

    /// Flushes pending output.
    ///
    /// # Errors
    /// Fails on an I/O error.
    pub async fn flush(&mut self) -> ProtocolResult<()> {
        self.writer.flush().await?;
        Ok(())
    }

    /// Shuts the write half down.
    ///
    /// # Errors
    /// Fails on an I/O error.
    pub async fn shutdown(&mut self) -> ProtocolResult<()> {
        self.writer.shutdown().await?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trips_over_an_in_memory_transport() {
        // The pattern every protocol test uses: a session driven with no socket.
        let (client, server) = duplex(4096);
        let (server_read, server_write) = tokio::io::split(server);
        let mut connection = Connection::new(server_read, server_write, 1024, None);

        let (mut client_read, mut client_write) = tokio::io::split(client);
        tokio::spawn(async move {
            let _ = client_write.write_all(b"HELLO\r\n").await;
        });

        let line = connection
            .read_line(Duration::from_secs(5))
            .await
            .unwrap_or_default()
            .to_vec();
        assert_eq!(line, b"HELLO");

        connection
            .write_flush(b"250 OK\r\n")
            .await
            .expect("write reply");

        let mut buf = vec![0u8; 8];
        tokio::io::AsyncReadExt::read_exact(&mut client_read, &mut buf)
            .await
            .expect("read reply");
        assert_eq!(&buf, b"250 OK\r\n");
    }
}
