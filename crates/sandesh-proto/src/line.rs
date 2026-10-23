//! Line-oriented framing over a byte stream.
//!
//! TCP is a byte stream, not a message stream. A single `read()` may return
//! half a command, three commands, or a command split mid-CRLF. This reader
//! makes packet boundaries irrelevant: it buffers whatever arrives and hands
//! out exactly one line at a time.
//!
//! That property is also what makes SMTP `PIPELINING` work. Pipelining is not a
//! separate code path — it is what correct buffering gives you for free, since
//! several commands arriving in one segment are simply several lines in the
//! buffer.
//!
//! # Bytes, not strings
//!
//! Lines are handed out as `&[u8]`. Message content is not necessarily UTF-8,
//! and converting it would break the raw-fidelity guarantee that the whole
//! product rests on. Callers that need text decode the parts they know are
//! text, at the point they know it.

use crate::{ProtocolError, ProtocolResult};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// CR LF, the line terminator for every protocol this crate serves.
pub const CRLF: &[u8; 2] = b"\r\n";

/// Reads CRLF-delimited lines from an async byte stream.
///
/// Owns a growable buffer that persists across reads, so a line split across
/// any number of packets reassembles correctly.
#[derive(Debug)]
pub struct LineReader<R> {
    inner: R,
    buffer: Vec<u8>,
    /// Offset of the first unconsumed byte.
    ///
    /// Consuming by advancing a cursor rather than draining from the front
    /// keeps pipelined commands cheap: a segment holding five commands is
    /// parsed with no shifting until the buffer is compacted.
    cursor: usize,
    max_line: usize,
    /// Set once the peer has closed; distinguishes "no data yet" from EOF.
    eof: bool,
}

impl<R: AsyncRead + Unpin + Send> LineReader<R> {
    /// Creates a reader that rejects lines longer than `max_line` bytes.
    #[must_use]
    pub fn new(inner: R, max_line: usize) -> Self {
        Self {
            inner,
            buffer: Vec::with_capacity(1024),
            cursor: 0,
            max_line,
            eof: false,
        }
    }

    /// Reads one line, without its CRLF, waiting at most `timeout`.
    ///
    /// A lone LF is also accepted as a terminator. RFC 5321 §2.3.8 requires
    /// CRLF, but real clients and hand-typed `telnet` sessions send bare LF,
    /// and rejecting them would make Sandesh worse at its job for no benefit.
    ///
    /// # Errors
    /// * [`ProtocolError::Closed`] — peer disconnected before a full line
    /// * [`ProtocolError::LineTooLong`] — the limit was exceeded
    /// * [`ProtocolError::Timeout`] — nothing arrived in time
    pub async fn read_line(&mut self, timeout: Duration) -> ProtocolResult<&[u8]> {
        loop {
            // A complete line may already be buffered from an earlier read —
            // this is the pipelining case, and it must not touch the socket.
            if let Some(end) = self.find_terminator() {
                let terminator_len = self.terminator_len(end);
                let start = self.cursor;

                // The limit applies to complete lines too, not only to a buffer
                // that has grown without a terminator: an oversized line that
                // arrives in a single read must be rejected just the same.
                // The terminator is consumed either way, so the stream stays
                // framed and the caller can reply and continue.
                if end - start > self.max_line {
                    self.cursor = end + terminator_len;
                    return Err(ProtocolError::LineTooLong {
                        limit: self.max_line,
                    });
                }

                self.cursor = end + terminator_len;
                return Ok(&self.buffer[start..end]);
            }

            if self.pending_len() > self.max_line {
                // Discard what is buffered: without a terminator we cannot tell
                // where the oversized line ends, so the stream is desynchronised
                // and the caller must decide whether to continue.
                self.discard();
                return Err(ProtocolError::LineTooLong {
                    limit: self.max_line,
                });
            }

            if self.eof {
                return Err(ProtocolError::Closed);
            }

            self.fill(timeout).await?;
        }
    }

    /// Reads one line and copies it, releasing the borrow on the reader.
    ///
    /// [`read_line`](Self::read_line) borrows `self` for the lifetime of the
    /// returned slice, which prevents reading the next line while holding the
    /// current one. Callers that need to keep a line use this.
    ///
    /// # Errors
    /// As [`read_line`](Self::read_line).
    pub async fn read_line_owned(&mut self, timeout: Duration) -> ProtocolResult<Vec<u8>> {
        self.read_line(timeout).await.map(<[u8]>::to_vec)
    }

    /// Reads exactly `length` bytes, regardless of line structure.
    ///
    /// IMAP literals (RFC 9051 §4.3) are counted octets, not lines: the client
    /// declares a length and sends that many bytes, which may contain CR, LF,
    /// NUL or invalid UTF-8. Framing them as lines would corrupt them, so this
    /// bypasses the terminator search and returns the buffer verbatim.
    ///
    /// # Bounded before it is attempted
    ///
    /// `length` is checked against `max_payload` **before** any allocation, so
    /// a caller cannot be made to reserve memory by a declared length it has
    /// not sanctioned. The IMAP parser applies its own configured literal limit
    /// first (ADR-040); this is the transport-level backstop, and the reason the
    /// signature takes a limit rather than trusting the caller to have checked.
    ///
    /// Allocation grows as bytes arrive rather than reserving `length` up
    /// front: a client that declares a permitted-but-large literal and then
    /// sends nothing must not cost that much memory while it stalls.
    ///
    /// # Errors
    /// * [`ProtocolError::PayloadTooLarge`] — `length` exceeds `max_payload`
    /// * [`ProtocolError::Closed`] — peer disconnected before `length` bytes
    /// * [`ProtocolError::Timeout`] — the stream stalled
    pub async fn read_exact(
        &mut self,
        length: usize,
        max_payload: usize,
        timeout: Duration,
    ) -> ProtocolResult<Vec<u8>> {
        if length > max_payload {
            return Err(ProtocolError::PayloadTooLarge { limit: max_payload });
        }

        let mut out = Vec::new();
        while out.len() < length {
            let still_needed = length - out.len();

            if self.pending_len() > 0 {
                let take = still_needed.min(self.pending_len());
                let from = self.cursor;
                out.extend_from_slice(&self.buffer[from..from + take]);
                self.cursor += take;
                continue;
            }

            if self.eof {
                return Err(ProtocolError::Closed);
            }

            self.fill(timeout).await?;
        }

        Ok(out)
    }

    /// Bytes buffered but not yet consumed.
    ///
    /// Non-zero after a command when the client pipelined, which is exactly
    /// what a protocol needs to know before deciding it may block on I/O.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.pending_len()
    }

    /// Discards everything buffered.
    ///
    /// Used to resynchronise after an oversized line.
    pub fn discard(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    /// Returns the wrapped stream and any bytes already buffered.
    ///
    /// The buffered bytes must be handed back with the stream: dropping them
    /// would silently lose a pipelined command. This is the seam a future
    /// `STARTTLS` upgrade goes through — though note that per RFC 3207 §4 a
    /// server must *discard* anything buffered across that boundary rather than
    /// trust it, so the upgrade path checks that this is empty.
    #[must_use]
    pub fn into_parts(self) -> (R, Vec<u8>) {
        let pending = self.buffer[self.cursor..].to_vec();
        (self.inner, pending)
    }

    /// Position of the terminator for the current line, if one is buffered.
    fn find_terminator(&self) -> Option<usize> {
        self.buffer[self.cursor..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|lf| {
                let lf = self.cursor + lf;
                // Treat a preceding CR as part of the terminator.
                if lf > self.cursor && self.buffer[lf - 1] == b'\r' {
                    lf - 1
                } else {
                    lf
                }
            })
    }

    /// Length of the terminator ending a line at `end`.
    fn terminator_len(&self, end: usize) -> usize {
        if self.buffer.get(end) == Some(&b'\r') {
            2 // CRLF
        } else {
            1 // bare LF
        }
    }

    fn pending_len(&self) -> usize {
        self.buffer.len() - self.cursor
    }

    /// Reads more bytes, compacting the buffer first when worthwhile.
    async fn fill(&mut self, timeout: Duration) -> ProtocolResult<()> {
        // Compact only once consumed bytes dominate, so pipelined reads stay
        // cheap but the buffer cannot grow without bound across a long session.
        if self.cursor > 0 && self.cursor >= self.buffer.len() / 2 {
            self.buffer.drain(..self.cursor);
            self.cursor = 0;
        }

        let mut chunk = [0u8; 4096];
        let read = tokio::time::timeout(timeout, self.inner.read(&mut chunk))
            .await
            .map_err(|_| ProtocolError::Timeout {
                seconds: timeout.as_secs(),
            })??;

        if read == 0 {
            self.eof = true;
        } else {
            self.buffer.extend_from_slice(&chunk[..read]);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Duration;

    const T: Duration = Duration::from_secs(5);

    fn reader(input: &[u8]) -> LineReader<std::io::Cursor<Vec<u8>>> {
        LineReader::new(std::io::Cursor::new(input.to_vec()), 1024)
    }

    #[tokio::test]
    async fn reads_crlf_delimited_lines() {
        let mut r = reader(b"ONE\r\nTWO\r\n");
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"ONE");
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"TWO");
    }

    #[tokio::test]
    async fn accepts_bare_lf() {
        // Hand-typed telnet sessions and some clients send bare LF.
        let mut r = reader(b"ONE\nTWO\n");
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"ONE");
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"TWO");
    }

    #[tokio::test]
    async fn handles_several_lines_in_one_read() {
        // The pipelining case: no socket read should be needed for lines 2-4.
        let mut r = reader(b"A\r\nB\r\nC\r\nD\r\n");
        for expected in [&b"A"[..], b"B", b"C", b"D"] {
            assert_eq!(r.read_line(T).await.unwrap_or_default(), expected);
        }
    }

    #[tokio::test]
    async fn empty_lines_are_preserved() {
        // The blank line separating headers from body is semantically vital.
        let mut r = reader(b"HEAD\r\n\r\nBODY\r\n");
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"HEAD");
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"");
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"BODY");
    }

    #[tokio::test]
    async fn line_content_may_be_arbitrary_bytes() {
        // Message content is not UTF-8; the framer must not assume it is.
        let mut r = LineReader::new(
            std::io::Cursor::new(b"\xff\xfe\x00binary\r\n".to_vec()),
            1024,
        );
        assert_eq!(
            r.read_line(T).await.unwrap_or_default(),
            b"\xff\xfe\x00binary"
        );
    }

    #[tokio::test]
    async fn oversized_line_is_rejected_and_reported() {
        let long = vec![b'x'; 200];
        let mut r = LineReader::new(std::io::Cursor::new(long), 64);
        let result = r.read_line(T).await;
        assert!(matches!(
            result,
            Err(ProtocolError::LineTooLong { limit: 64 })
        ));
    }

    #[tokio::test]
    async fn oversized_line_is_rejected_even_when_complete() {
        // Regression: an over-limit line arriving whole, terminator included,
        // must still be refused — the limit is about the line, not about how
        // the buffer happened to fill.
        let mut input = vec![b'x'; 200];
        input.extend_from_slice(b"\r\nNEXT\r\n");
        let mut r = LineReader::new(std::io::Cursor::new(input), 64);

        assert!(matches!(
            r.read_line(T).await,
            Err(ProtocolError::LineTooLong { limit: 64 })
        ));
        // The terminator was consumed, so the stream is still framed.
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"NEXT");
    }

    #[tokio::test]
    async fn a_line_exactly_at_the_limit_is_accepted() {
        let mut input = vec![b'x'; 64];
        input.extend_from_slice(b"\r\n");
        let mut r = LineReader::new(std::io::Cursor::new(input), 64);
        assert_eq!(r.read_line(T).await.unwrap_or_default().len(), 64);
    }

    #[tokio::test]
    async fn eof_without_terminator_reports_closed() {
        let mut r = reader(b"PARTIAL");
        assert!(matches!(r.read_line(T).await, Err(ProtocolError::Closed)));
    }

    #[tokio::test]
    async fn reports_closed_at_clean_eof() {
        let mut r = reader(b"ONE\r\n");
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"ONE");
        assert!(matches!(r.read_line(T).await, Err(ProtocolError::Closed)));
    }

    #[tokio::test]
    async fn buffered_reports_pipelined_bytes() {
        let mut r = reader(b"A\r\nBCDEF\r\n");
        assert_eq!(r.read_line(T).await.unwrap_or_default(), b"A");
        assert_eq!(r.buffered(), 7, "BCDEF\\r\\n remains buffered");
    }

    #[tokio::test]
    async fn into_parts_returns_unconsumed_bytes() {
        let mut r = reader(b"A\r\nLEFTOVER\r\n");
        let _ = r.read_line(T).await;
        let (_stream, pending) = r.into_parts();
        assert_eq!(pending, b"LEFTOVER\r\n", "pipelined input must not be lost");
    }
}
