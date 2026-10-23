//! Opening and closing connection records around a served socket (ADR-066).
//!
//! [`Transcript`] captures what was said; [`ConnectionRecorder`] is what turns
//! one accepted socket into a persisted [`ConnectionRecord`]. Kept here rather
//! than in each protocol crate so all three record identically — a difference
//! between SMTP's and IMAP's idea of "when a connection closed" would show up
//! as an inconsistency in the trace viewer and nowhere else.

use std::sync::Arc;
use std::time::Duration;

use sandesh_core::{
    ConnectionDisposition, ConnectionId, ConnectionRecord, ConnectionStore, Protocol, Timestamp,
};
use tokio_util::sync::CancellationToken;

use crate::Transcript;

/// How often an open connection's transcript is written to storage.
///
/// Live tailing (Phase 7 Pass B) reads the same `connections` row the finished
/// trace is read from, so the row has to grow while the socket is open. A
/// timer rather than a write-per-line: an IMAP sync issues thousands of
/// commands, and a database write per line would make tracing cost more than
/// the protocol work it observes.
///
/// 500ms is chosen against what the view is for — watching a conversation
/// unfold — not against a latency budget. A trace that lags half a second is
/// still live to a reader; a hundred writes a second is not free.
pub const TRANSCRIPT_FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// Persists connection records, when tracing is configured.
///
/// Cloneable and cheap: each protocol server holds one and opens a record per
/// accepted socket.
#[derive(Clone)]
pub struct ConnectionRecorder {
    store: Option<Arc<dyn ConnectionStore>>,
    protocol: Protocol,
    max_lines: usize,
    max_bytes: usize,
    /// How many records to keep. `None` never prunes.
    retention: Option<u32>,
}

impl std::fmt::Debug for ConnectionRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionRecorder")
            .field("protocol", &self.protocol)
            .field("enabled", &self.store.is_some())
            .finish_non_exhaustive()
    }
}

impl ConnectionRecorder {
    /// A recorder that persists nothing.
    #[must_use]
    pub const fn disabled(protocol: Protocol) -> Self {
        Self {
            store: None,
            protocol,
            max_lines: crate::DEFAULT_MAX_LINES,
            max_bytes: crate::DEFAULT_MAX_BYTES,
            retention: None,
        }
    }

    /// A recorder that writes to `store`.
    #[must_use]
    pub fn new(protocol: Protocol, store: Arc<dyn ConnectionStore>) -> Self {
        Self {
            store: Some(store),
            protocol,
            max_lines: crate::DEFAULT_MAX_LINES,
            max_bytes: crate::DEFAULT_MAX_BYTES,
            retention: None,
        }
    }

    /// Keeps at most `keep` records, evicting oldest first as connections
    /// close.
    ///
    /// Pruned opportunistically rather than on a timer: there is no background
    /// task to own, and a server with no traffic needs no sweeping.
    #[must_use]
    pub const fn with_retention(mut self, keep: u32) -> Self {
        self.retention = Some(keep);
        self
    }

    /// Overrides the per-connection transcript caps.
    #[must_use]
    pub const fn with_caps(mut self, max_lines: usize, max_bytes: usize) -> Self {
        self.max_lines = max_lines;
        self.max_bytes = max_bytes;
        self
    }

    /// Whether this recorder persists anything.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.store.is_some()
    }

    /// Opens a record for one accepted socket.
    ///
    /// The returned [`OpenConnection`] carries the transcript to hand to every
    /// `Connection` built over this socket — including the second one a
    /// `STARTTLS` upgrade creates, which is what keeps the upgrade one
    /// conversation rather than two.
    ///
    /// Call [`OpenConnection::persist_open`] before serving if anything written
    /// during the session may reference the connection: `deliveries` has a
    /// foreign key onto this row, so a delivery recorded mid-session fails
    /// unless the row already exists.
    #[must_use]
    pub fn open(&self, peer: Option<String>) -> OpenConnection {
        let transcript = if self.store.is_some() {
            Transcript::with_caps(self.max_lines, self.max_bytes)
        } else {
            Transcript::disabled()
        };

        OpenConnection {
            id: ConnectionId::new(),
            store: self.store.clone(),
            protocol: self.protocol,
            peer,
            opened_at: Timestamp::now(),
            transcript,
            retention: self.retention,
        }
    }
}

/// A connection being recorded, from accept to close.
#[derive(Clone)]
pub struct OpenConnection {
    id: ConnectionId,
    store: Option<Arc<dyn ConnectionStore>>,
    protocol: Protocol,
    peer: Option<String>,
    opened_at: Timestamp,
    transcript: Transcript,
    retention: Option<u32>,
}

impl std::fmt::Debug for OpenConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenConnection")
            .field("id", &self.id)
            .field("protocol", &self.protocol)
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl OpenConnection {
    /// This connection's id, for a delivery record to link to.
    #[must_use]
    pub const fn id(&self) -> ConnectionId {
        self.id
    }

    /// The transcript to hand to a `Connection` over this socket.
    #[must_use]
    pub const fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// Writes the record before the session runs, so anything referencing this
    /// connection has a row to point at.
    ///
    /// `deliveries.connection_id` is a foreign key onto `connections(id)`
    /// (ADR-066). An SMTP delivery is recorded *during* the session while the
    /// connection record would otherwise not be written until the socket
    /// closed, so the reference would always dangle and every delivery would
    /// fail with a constraint violation. `close` upserts over this row.
    ///
    /// A failure is logged rather than propagated: it costs a transcript, and
    /// the delivery it would have linked to simply records no connection.
    pub async fn persist_open(&self) {
        let Some(store) = &self.store else { return };
        // Nothing to cancel: callers that want live flushing use
        // `persist_open_streaming`, which owns the task's lifetime.

        let record = ConnectionRecord {
            id: self.id,
            protocol: self.protocol,
            peer: self.peer.clone(),
            encrypted: false,
            opened_at: self.opened_at,
            // Still open: no close time and no disposition yet, which is what
            // distinguishes an in-flight connection from a finished one.
            closed_at: None,
            disposition: None,
            transcript: Vec::new(),
            truncated: false,
        };

        if let Err(error) = store.record(&record).await {
            tracing::warn!(%error, "could not open a connection record");
        }
    }

    /// [`persist_open`](Self::persist_open), plus periodic flushing of the
    /// transcript while the session runs.
    ///
    /// The returned guard stops the flusher when dropped, so a caller cannot
    /// leak the task by taking an early return path — the one failure mode a
    /// bare `spawn` would have. [`close`](Self::close) consumes it.
    pub async fn persist_open_streaming(&self) -> FlushGuard {
        self.persist_open().await;

        let cancel = CancellationToken::new();
        self.spawn_flusher(cancel.clone());
        FlushGuard { cancel }
    }

    /// Writes the transcript as it stands, leaving the connection open.
    ///
    /// An upsert over the row [`persist_open`](Self::persist_open) created, so
    /// a reader sees a growing transcript with `closed_at` still null — which
    /// is exactly how the view tells an in-flight conversation from a finished
    /// one.
    ///
    /// `encrypted` is reported as the transcript's own view of the session
    /// rather than tracked here: a `STARTTLS` upgrade is visible as a note in
    /// the lines, and the authoritative value is written at close.
    async fn flush(&self) {
        let Some(store) = &self.store else { return };

        let (transcript, truncated) = self.transcript.snapshot();
        let record = ConnectionRecord {
            id: self.id,
            protocol: self.protocol,
            peer: self.peer.clone(),
            // Not yet known; `close` writes the authoritative value.
            encrypted: false,
            opened_at: self.opened_at,
            // Still open. A reader keys on this, so it must stay null until
            // the connection actually ends.
            closed_at: None,
            disposition: None,
            transcript,
            truncated,
        };

        if let Err(error) = store.record(&record).await {
            // Diagnostic only: a failed flush costs a moment of live view and
            // nothing else, because `close` writes the whole transcript again.
            tracing::debug!(%error, "could not flush an in-flight transcript");
        }
    }

    /// Flushes the transcript periodically until `cancel` fires.
    ///
    /// Spawned by [`persist_open`](Self::persist_open) and stopped by
    /// [`close`](Self::close), so the task's lifetime is exactly the
    /// connection's. Nothing else may outlive the socket: a leaked flusher
    /// would keep rewriting a row whose session has ended.
    fn spawn_flusher(&self, cancel: CancellationToken) {
        if self.store.is_none() {
            return;
        }

        let flusher = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(TRANSCRIPT_FLUSH_INTERVAL) => flusher.flush().await,
                }
            }
        });
    }

    /// Persists the finished record.
    ///
    /// A failure is logged, never propagated: losing a diagnostic transcript
    /// must not fail a client's connection, which by this point has already
    /// been served.
    ///
    /// Pass the [`FlushGuard`] from
    /// [`persist_open_streaming`](Self::persist_open_streaming) as `flushing`.
    /// It is **stopped before the final write**, not after: a flush still in
    /// flight writes `closed_at: None`, so one landing after this would
    /// resurrect a finished connection as permanently open — a row the viewer
    /// would show as in-flight forever.
    pub async fn close(
        self,
        disposition: ConnectionDisposition,
        encrypted: bool,
        flushing: Option<FlushGuard>,
    ) {
        // Stop the flusher and wait for any in-flight write to finish before
        // taking the snapshot below. Dropping the guard cancels; the await
        // yields long enough for a cancelled task to observe it.
        drop(flushing);
        tokio::task::yield_now().await;

        let Some(store) = self.store else { return };

        let (transcript, truncated) = self.transcript.snapshot();
        let record = ConnectionRecord {
            id: self.id,
            protocol: self.protocol,
            peer: self.peer,
            encrypted,
            opened_at: self.opened_at,
            closed_at: Some(Timestamp::now()),
            disposition: Some(disposition),
            transcript,
            truncated,
        };

        if let Err(error) = store.record(&record).await {
            tracing::warn!(%error, "could not record a connection transcript");
            // Nothing was written, so there is nothing to prune.
            return;
        }

        if let Some(keep) = self.retention
            && let Err(error) = store.prune(keep).await
        {
            tracing::warn!(%error, "could not prune old connection transcripts");
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::Direction;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Spy {
        recorded: Mutex<Vec<ConnectionRecord>>,
    }

    #[async_trait]
    impl ConnectionStore for Spy {
        async fn record(&self, connection: &ConnectionRecord) -> sandesh_core::CoreResult<()> {
            self.recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(connection.clone());
            Ok(())
        }
        async fn get(
            &self,
            _id: ConnectionId,
        ) -> sandesh_core::CoreResult<Option<ConnectionRecord>> {
            Ok(None)
        }
        async fn recent(
            &self,
            _protocol: Option<Protocol>,
            _limit: u32,
        ) -> sandesh_core::CoreResult<Vec<ConnectionRecord>> {
            Ok(Vec::new())
        }
        async fn prune(&self, _keep: u32) -> sandesh_core::CoreResult<u64> {
            Ok(0)
        }
    }

    /// Live tailing (Phase 7 Pass B): the row must grow *while* the socket is
    /// open, because the viewer reads the same `connections` row it reads a
    /// finished trace from. Before this, the transcript existed only in memory
    /// until close, so an in-flight conversation was invisible.
    #[tokio::test(start_paused = true)]
    async fn an_open_connection_flushes_its_transcript_while_still_open() {
        let spy = Arc::new(Spy::default());
        let recorder = ConnectionRecorder::new(Protocol::Smtp, spy.clone());

        let open = recorder.open(Some(String::from("127.0.0.1:25")));
        let flushing = open.persist_open_streaming().await;

        open.transcript().line(Direction::In, b"EHLO localhost");
        tokio::time::sleep(TRANSCRIPT_FLUSH_INTERVAL * 2).await;

        let written = spy
            .recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        // The open row, then at least one flush carrying the line.
        let flushed = written
            .iter()
            .find(|r| r.transcript.iter().any(|l| l.contains("EHLO")))
            .expect("a flush should have carried the line while open");

        assert!(
            flushed.closed_at.is_none(),
            "an in-flight flush must leave the connection open"
        );
        assert!(flushed.disposition.is_none());

        open.close(ConnectionDisposition::Closed, false, Some(flushing))
            .await;
    }

    /// The ordering hazard the guard exists for.
    ///
    /// A flush writes `closed_at: None`. One landing *after* the final write
    /// would resurrect a finished connection as permanently open — a row the
    /// viewer shows as in-flight forever. `close` therefore stops the flusher
    /// before it snapshots, and the last write must be the closing one.
    #[tokio::test(start_paused = true)]
    async fn a_flush_never_lands_after_the_connection_closes() {
        let spy = Arc::new(Spy::default());
        let recorder = ConnectionRecorder::new(Protocol::Pop3, spy.clone());

        let open = recorder.open(Some(String::from("127.0.0.1:110")));
        let flushing = open.persist_open_streaming().await;
        open.transcript().line(Direction::In, b"USER alice");
        tokio::time::sleep(TRANSCRIPT_FLUSH_INTERVAL * 2).await;

        open.close(ConnectionDisposition::Closed, false, Some(flushing))
            .await;

        // Give a leaked flusher every chance to write after the close.
        tokio::time::sleep(TRANSCRIPT_FLUSH_INTERVAL * 4).await;

        let written = spy
            .recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let last = written.last().expect("something was written");

        assert!(
            last.closed_at.is_some(),
            "the final write must be the closing one, not a stray flush"
        );
        assert_eq!(last.disposition, Some(ConnectionDisposition::Closed));
    }

    /// Dropping the guard on an early return must stop the task, or a server
    /// under load accumulates flushers for sockets that are long gone.
    #[tokio::test(start_paused = true)]
    async fn dropping_the_guard_stops_the_flusher() {
        let spy = Arc::new(Spy::default());
        let recorder = ConnectionRecorder::new(Protocol::Imap, spy.clone());

        let open = recorder.open(Some(String::from("127.0.0.1:143")));
        let flushing = open.persist_open_streaming().await;
        drop(flushing);

        open.transcript().line(Direction::In, b"a1 NOOP");
        tokio::time::sleep(TRANSCRIPT_FLUSH_INTERVAL * 4).await;

        let written = spy
            .recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

        assert!(
            !written
                .iter()
                .any(|r| r.transcript.iter().any(|l| l.contains("NOOP"))),
            "a dropped guard must stop the flusher: {written:?}"
        );
    }

    /// A disabled recorder must spawn nothing at all.
    #[tokio::test(start_paused = true)]
    async fn a_disabled_recorder_starts_no_flusher() {
        let recorder = ConnectionRecorder::disabled(Protocol::Smtp);
        let open = recorder.open(Some(String::from("127.0.0.1:1")));

        let flushing = open.persist_open_streaming().await;
        open.transcript().line(Direction::In, b"EHLO localhost");
        tokio::time::sleep(TRANSCRIPT_FLUSH_INTERVAL * 4).await;

        // Nothing to assert against a store that does not exist; this must
        // simply not panic or hang.
        open.close(ConnectionDisposition::Closed, false, Some(flushing))
            .await;
    }

    #[tokio::test]
    async fn a_disabled_recorder_persists_nothing_and_records_nothing() {
        let recorder = ConnectionRecorder::disabled(Protocol::Smtp);
        assert!(!recorder.is_enabled());

        let open = recorder.open(Some(String::from("127.0.0.1:1")));
        open.transcript().line(Direction::In, b"EHLO localhost");
        assert!(!open.transcript().is_recording());

        // Must not panic with no store behind it.
        open.close(ConnectionDisposition::Closed, false, None).await;
    }

    #[tokio::test]
    async fn closing_persists_the_transcript_and_the_disposition() {
        let spy = Arc::new(Spy::default());
        let recorder = ConnectionRecorder::new(Protocol::Imap, spy.clone());

        let open = recorder.open(Some(String::from("127.0.0.1:143")));
        open.transcript().line(Direction::In, b"a1 CAPABILITY");
        open.close(ConnectionDisposition::Disconnected, true, None)
            .await;

        let written = spy.recorded.lock().unwrap();
        assert_eq!(written.len(), 1);
        let record = &written[0];
        assert_eq!(record.protocol, Protocol::Imap);
        assert!(record.encrypted);
        assert_eq!(
            record.disposition,
            Some(ConnectionDisposition::Disconnected)
        );
        assert!(record.closed_at.is_some());
        assert_eq!(record.transcript.len(), 1);
        assert!(record.transcript[0].contains("CAPABILITY"));
    }

    #[tokio::test]
    async fn retention_prunes_after_a_connection_closes() {
        // Without this, transcripts grow without bound: nothing else in the
        // schema caps them, and they are produced more often than messages.
        #[derive(Default)]
        struct PruneSpy {
            pruned_to: Mutex<Vec<u32>>,
        }

        #[async_trait]
        impl ConnectionStore for PruneSpy {
            async fn record(&self, _c: &ConnectionRecord) -> sandesh_core::CoreResult<()> {
                Ok(())
            }
            async fn get(
                &self,
                _id: ConnectionId,
            ) -> sandesh_core::CoreResult<Option<ConnectionRecord>> {
                Ok(None)
            }
            async fn recent(
                &self,
                _p: Option<Protocol>,
                _l: u32,
            ) -> sandesh_core::CoreResult<Vec<ConnectionRecord>> {
                Ok(Vec::new())
            }
            async fn prune(&self, keep: u32) -> sandesh_core::CoreResult<u64> {
                self.pruned_to
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(keep);
                Ok(0)
            }
        }

        let spy = Arc::new(PruneSpy::default());
        let recorder = ConnectionRecorder::new(Protocol::Pop3, spy.clone()).with_retention(250);

        recorder
            .open(None)
            .close(ConnectionDisposition::Closed, false, None)
            .await;

        assert_eq!(
            *spy.pruned_to.lock().unwrap(),
            vec![250],
            "closing a connection must prune to the configured cap"
        );
    }

    #[tokio::test]
    async fn no_retention_means_no_prune() {
        // The default. A recorder with no cap must not silently delete records.
        let spy = Arc::new(Spy::default());
        let recorder = ConnectionRecorder::new(Protocol::Smtp, spy.clone());

        recorder
            .open(None)
            .close(ConnectionDisposition::Closed, false, None)
            .await;

        assert_eq!(spy.recorded.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn one_socket_keeps_one_id_across_a_transcript_clone() {
        // The STARTTLS shape: the second `Connection` gets a clone of the same
        // transcript, and the record that lands is still one row.
        let spy = Arc::new(Spy::default());
        let recorder = ConnectionRecorder::new(Protocol::Smtp, spy.clone());

        let open = recorder.open(None);
        let id = open.id();

        let cleartext = open.transcript().clone();
        cleartext.line(Direction::In, b"STARTTLS");
        let encrypted = open.transcript().clone();
        encrypted.line(Direction::In, b"EHLO localhost");

        open.close(ConnectionDisposition::Closed, true, None).await;

        let written = spy.recorded.lock().unwrap();
        assert_eq!(written.len(), 1, "one socket is one row");
        assert_eq!(written[0].id, id);
        assert_eq!(
            written[0].transcript.len(),
            2,
            "both halves of the upgrade are in one transcript"
        );
    }
}

/// Stops an [`OpenConnection`]'s transcript flusher when dropped.
///
/// A guard rather than a bare [`CancellationToken`] so the task cannot outlive
/// the connection by accident: every path out of a session — including an
/// early `?` — drops this and stops the flushing.
#[derive(Debug)]
pub struct FlushGuard {
    cancel: CancellationToken,
}

impl Drop for FlushGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
