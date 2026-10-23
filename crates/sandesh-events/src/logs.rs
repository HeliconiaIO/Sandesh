//! The log stream (Phase 7 A6).
//!
//! A `tracing` layer that copies each event into a bounded ring buffer and
//! broadcasts it to live subscribers, so the Web UI can show the same log the
//! terminal shows.
//!
//! It lives here, beside [`EventBus`](crate::EventBus), for the same reason
//! the bus does: `sandesh-app` installs it and `sandesh-api` reads it, and
//! neither may depend on the other. The mechanics are the same too — a
//! bounded queue per subscriber, and a producer that never blocks.
//!
//! ## This adds no new disclosure
//!
//! The stance in `sandesh-app`'s `logging.rs` applies unchanged: message
//! bodies, headers and credentials never reach a log in the first place, so
//! they cannot reach this buffer. This streams what already goes to stdout and
//! adds nothing to it. That is the whole safety argument — it is not a filter
//! that could be misconfigured, it is a copy of a stream that never carried
//! the material.
//!
//! What *does* change is who can read it: stdout requires the terminal, and
//! this requires an HTTP request. That is why the endpoint sits behind the
//! same authentication as everything else (ADR-075), with no exemption.
//!
//! ## Why a ring buffer and not a file
//!
//! A developer opening the log view wants the last few hundred lines, not the
//! session's history. A bounded buffer costs a known amount of memory and
//! needs no rotation, no cleanup and no disk quota — the same reasoning that
//! caps connection transcripts (ADR-066) and messages (`STORAGE_DESIGN.md` §5).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// How many lines the buffer keeps.
///
/// Enough to cover an SMTP delivery, an IMAP sync and whatever went wrong
/// around them, which is the span a developer actually reads back.
pub const DEFAULT_LOG_CAPACITY: usize = 500;

/// Per-subscriber queue capacity for the live stream.
///
/// Smaller than the backlog: a subscriber this far behind is not reading, and
/// the backlog is there for anyone who reconnects.
pub const DEFAULT_LOG_SUBSCRIBER_CAPACITY: usize = 256;

/// One captured log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Monotonic identity, assigned on capture.
    ///
    /// Lets a reader tell a gap from a quiet period, the same role `seq` plays
    /// on a [`DomainEvent`](sandesh_core::DomainEvent).
    pub seq: u64,
    /// When it was captured, as RFC 3339.
    pub at: String,
    /// The level, lowercased: `error`, `warn`, `info`, `debug`, `trace`.
    pub level: String,
    /// The emitting module path, or empty when the event carried none.
    pub target: String,
    /// The rendered message.
    pub message: String,
    /// The event's structured fields, rendered as `key=value`, excluding the
    /// message itself.
    pub fields: String,
}

impl LogLine {
    /// Whether this line passes a level and target filter.
    ///
    /// `level` matches lines *at or above* that severity, which is what a
    /// reader means by "show me warnings" — not "show me only warnings".
    #[must_use]
    pub fn matches(&self, level: Option<&str>, target: Option<&str>) -> bool {
        if let Some(wanted) = level
            && severity(&self.level) < severity(wanted)
        {
            return false;
        }

        if let Some(wanted) = target
            && !self.target.contains(wanted)
        {
            return false;
        }

        true
    }
}

/// Orders levels by severity so a filter can mean "at least this bad".
///
/// An unknown level sorts highest rather than lowest: a line nobody can
/// classify should be shown, not silently filtered away.
fn severity(level: &str) -> u8 {
    match level.to_ascii_lowercase().as_str() {
        "trace" => 0,
        "debug" => 1,
        "info" => 2,
        "warn" | "warning" => 3,
        "error" => 4,
        _ => u8::MAX,
    }
}

/// A bounded ring of recent log lines, with a live broadcast.
#[derive(Debug)]
pub struct LogBuffer {
    recent: Mutex<VecDeque<LogLine>>,
    sender: broadcast::Sender<LogLine>,
    capacity: usize,
    next_seq: AtomicU64,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl LogBuffer {
    /// A buffer with the default capacities.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_LOG_CAPACITY, DEFAULT_LOG_SUBSCRIBER_CAPACITY)
    }

    /// A buffer holding `capacity` lines, with `subscriber_capacity` queues.
    #[must_use]
    pub fn with_capacity(capacity: usize, subscriber_capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(subscriber_capacity.max(1));
        Self {
            recent: Mutex::new(VecDeque::with_capacity(capacity.max(1))),
            sender,
            capacity: capacity.max(1),
            next_seq: AtomicU64::new(1),
        }
    }

    /// Records a line, evicting the oldest if the ring is full.
    ///
    /// Never blocks and never fails. This runs inside `tracing`'s emit path,
    /// which every part of the server calls — including, transitively, the
    /// mail path. A capture that could block here would let a stalled browser
    /// tab slow down SMTP, the exact property the event bus exists to protect.
    pub fn push(&self, mut line: LogLine) {
        line.seq = self.next_seq.fetch_add(1, Ordering::Relaxed);

        if let Ok(mut recent) = self.recent.lock() {
            if recent.len() == self.capacity {
                recent.pop_front();
            }
            recent.push_back(line.clone());
        }

        // An error means nobody is listening, which is the normal case.
        let _ = self.sender.send(line);
    }

    /// The most recent lines, oldest first, optionally filtered.
    ///
    /// `limit` caps the result from the *newest* end: a reader asking for 50
    /// lines wants the last 50, not the first 50 of the buffer.
    #[must_use]
    pub fn recent(&self, limit: usize, level: Option<&str>, target: Option<&str>) -> Vec<LogLine> {
        let Ok(recent) = self.recent.lock() else {
            // A poisoned lock means a capture panicked mid-push. Reporting no
            // lines is honest; panicking here would take down the endpoint
            // that exists to explain what went wrong.
            return Vec::new();
        };

        let matching: Vec<LogLine> = recent
            .iter()
            .filter(|line| line.matches(level, target))
            .cloned()
            .collect();

        let start = matching.len().saturating_sub(limit);
        matching[start..].to_vec()
    }

    /// Subscribes to lines captured from now on.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.sender.subscribe()
    }

    /// How many lines the ring currently holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.recent.lock().map_or(0, |recent| recent.len())
    }

    /// Whether the ring is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Collects an event's fields into a message and a rendered remainder.
#[derive(Default)]
struct FieldCollector {
    message: String,
    fields: String,
}

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
            // `Debug` on a string quotes it; the message reads better without.
            if self.message.starts_with('"') && self.message.ends_with('"') {
                self.message = self.message[1..self.message.len() - 1].to_owned();
            }
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(self.fields, "{}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(self.fields, "{}={value}", field.name());
        }
    }
}

/// A `tracing` layer that copies events into a [`LogBuffer`].
///
/// Installed alongside the stdout layer rather than replacing it: the terminal
/// output is what a developer running the binary sees, and A6 adds a second
/// reader rather than moving the log somewhere else.
#[derive(Debug, Clone)]
pub struct LogLayer {
    buffer: std::sync::Arc<LogBuffer>,
}

impl LogLayer {
    /// Wraps a buffer as a layer.
    #[must_use]
    pub const fn new(buffer: std::sync::Arc<LogBuffer>) -> Self {
        Self { buffer }
    }
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut collector = FieldCollector::default();
        event.record(&mut collector);

        let metadata = event.metadata();
        self.buffer.push(LogLine {
            // Replaced by `push`; the capture site does not own identity.
            seq: 0,
            at: now_rfc3339(),
            level: metadata.level().as_str().to_ascii_lowercase(),
            target: metadata.target().to_owned(),
            message: collector.message,
            fields: collector.fields,
        });
    }
}

/// The current time, RFC 3339.
///
/// Uses the `time` crate the workspace already depends on and that
/// `sandesh-api` already formats timestamps with, rather than a hand-rolled
/// calendar: an earlier draft implemented the civil-date conversion here and
/// it was both more code and a second thing that could disagree about a leap
/// year.
fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn line(level: &str, target: &str, message: &str) -> LogLine {
        LogLine {
            seq: 0,
            at: String::from("2026-01-01T00:00:00.000Z"),
            level: String::from(level),
            target: String::from(target),
            message: String::from(message),
            fields: String::new(),
        }
    }

    #[test]
    fn the_ring_evicts_the_oldest_and_stays_bounded() {
        let buffer = LogBuffer::with_capacity(3, 16);

        for index in 0..10 {
            buffer.push(line("info", "test", &format!("line {index}")));
        }

        assert_eq!(buffer.len(), 3, "the ring must not grow past its capacity");
        let recent = buffer.recent(10, None, None);
        assert_eq!(recent[0].message, "line 7", "the oldest is evicted first");
        assert_eq!(recent[2].message, "line 9");
    }

    #[test]
    fn sequence_numbers_are_monotonic_so_a_gap_is_detectable() {
        let buffer = LogBuffer::new();
        buffer.push(line("info", "test", "one"));
        buffer.push(line("info", "test", "two"));

        let recent = buffer.recent(10, None, None);
        assert_eq!(recent[0].seq, 1);
        assert_eq!(recent[1].seq, 2);
    }

    /// "Show me warnings" means warnings *and worse*, not warnings only.
    #[test]
    fn a_level_filter_includes_higher_severities() {
        let buffer = LogBuffer::new();
        buffer.push(line("debug", "a", "debug"));
        buffer.push(line("info", "a", "info"));
        buffer.push(line("warn", "a", "warn"));
        buffer.push(line("error", "a", "error"));

        let warnings = buffer.recent(10, Some("warn"), None);
        let messages: Vec<&str> = warnings.iter().map(|l| l.message.as_str()).collect();

        assert_eq!(messages, vec!["warn", "error"]);
    }

    #[test]
    fn a_target_filter_matches_a_substring_of_the_module_path() {
        let buffer = LogBuffer::new();
        buffer.push(line("info", "sandesh_smtp::session", "smtp"));
        buffer.push(line("info", "sandesh_imap::session", "imap"));

        let smtp = buffer.recent(10, None, Some("smtp"));
        assert_eq!(smtp.len(), 1);
        assert_eq!(smtp[0].message, "smtp");
    }

    /// A limit takes the newest lines. Taking the oldest would show a reader
    /// the start of the buffer when they asked what just happened.
    #[test]
    fn a_limit_takes_the_newest_lines() {
        let buffer = LogBuffer::new();
        for index in 0..10 {
            buffer.push(line("info", "test", &format!("line {index}")));
        }

        let recent = buffer.recent(2, None, None);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].message, "line 8");
        assert_eq!(recent[1].message, "line 9");
    }

    /// An unknown level must not vanish: a line nobody can classify is
    /// precisely the one worth seeing.
    #[test]
    fn an_unrecognised_level_survives_filtering() {
        let buffer = LogBuffer::new();
        buffer.push(line("catastrophe", "a", "unknown level"));

        assert_eq!(buffer.recent(10, Some("error"), None).len(), 1);
    }

    #[test]
    fn a_subscriber_receives_lines_captured_after_it_subscribed() {
        let buffer = LogBuffer::new();
        buffer.push(line("info", "test", "before"));

        let mut subscriber = buffer.subscribe();
        buffer.push(line("info", "test", "after"));

        let received = subscriber.try_recv().expect("a line");
        assert_eq!(received.message, "after", "no replay of the backlog");
        assert!(subscriber.try_recv().is_err(), "exactly one line was sent");
    }

    /// The load-bearing property: capture must never block the caller, because
    /// the caller may be the SMTP path.
    #[test]
    fn capture_does_not_block_when_a_subscriber_stops_reading() {
        let buffer = LogBuffer::with_capacity(1000, 2);
        let _slow = buffer.subscribe();

        // Far more than the subscriber's queue can hold.
        for index in 0..100 {
            buffer.push(line("info", "test", &format!("line {index}")));
        }

        assert_eq!(buffer.len(), 100, "the ring keeps recording regardless");
    }

    /// Parseable by the same formatter the rest of the API uses, so a client
    /// that reads one timestamp can read them all.
    #[test]
    fn the_timestamp_round_trips_as_rfc3339() {
        let at = now_rfc3339();

        let parsed =
            time::OffsetDateTime::parse(&at, &time::format_description::well_known::Rfc3339);
        assert!(parsed.is_ok(), "not valid RFC 3339: {at}");
        assert!(at.ends_with('Z'), "must be UTC: {at}");
    }
}
