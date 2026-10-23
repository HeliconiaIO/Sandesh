//! # Sandesh event bus
//!
//! The in-process broadcast bus that connects the mail core to everyone
//! interested in what it just did (`docs/EVENT_MODEL.md`).
//!
//! ```text
//! Core service ──emit()──▶ EventBus ──┬──▶ `WebSocket` fan-out
//!                                     ├──▶ SSE fan-out
//!                                     ├──▶ IMAP IDLE (Phase 5)
//!                                     └──▶ logging / metrics
//! ```
//!
//! ## What this crate is not
//!
//! It holds no domain logic and knows no transport. There is no HTTP type, no
//! `WebSocket` type, and no SQL here — the bus moves [`DomainEvent`]s and nothing
//! else, which is what lets IMAP `IDLE` and the Web UI consume the same facts
//! without either one shaping the other. `tests/architecture.rs` enforces it.
//!
//! ## The three properties that matter
//!
//! **Total order.** [`EventBus::emit`] assigns each event a monotonically
//! increasing `seq` under one lock, so every subscriber observes the same
//! sequence. A consumer detects a gap by comparing sequence numbers.
//!
//! **Bounded memory.** Every subscriber gets its own fixed-capacity queue
//! ([`DEFAULT_CAPACITY`]). The bus never allocates without limit, however slow
//! or numerous its consumers.
//!
//! **A slow consumer cannot block the producer.** This is the load-bearing one.
//! [`EventBus::emit`] never awaits and never blocks; when a subscriber's queue
//! is full the *oldest* events are dropped and it is told how many it missed
//! ([`Lagged`](RecvError::Lagged)). A stalled browser tab therefore cannot
//! throttle SMTP delivery — the property Phase 3 exists to guarantee.
//!
//! Dropping oldest rather than newest is deliberate: a live inbox cares most
//! about what just happened, and a lagged consumer must resynchronise anyway.

#![doc(html_root_url = "https://docs.rs/sandesh-events")]
#![warn(missing_docs, clippy::pedantic)]
// Pedantic since clippy 1.99; `assert!(x.is_empty())` is the idiomatic test assertion.
#![allow(clippy::assert_is_empty)]

pub mod logs;

pub use logs::{
    DEFAULT_LOG_CAPACITY, DEFAULT_LOG_SUBSCRIBER_CAPACITY, LogBuffer, LogLayer, LogLine,
};

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use sandesh_core::{DomainEvent, EventSink, EventSource, EventStream};
use tokio::sync::broadcast;

/// Per-subscriber queue capacity unless overridden.
///
/// `EVENT_MODEL.md` §6. Events carry identifiers rather than bodies, so a
/// thousand of them is a small, *bounded* amount of memory regardless of how
/// large the messages they describe are.
pub const DEFAULT_CAPACITY: usize = 1000;

/// How many recently published events the bus retains for `resume` replay.
///
/// The bus is still not a log: this is a fixed-size window, not durability. It
/// survives no restart, and a client asking for a `seq` older than the window
/// is told the gap is unrecoverable rather than handed a partial history
/// (`EVENT_MODEL.md` §13, ADR-100). Events carry identifiers rather than
/// bodies, so the window is bounded memory regardless of message size.
pub const DEFAULT_REPLAY_CAPACITY: usize = 1024;

/// Why a subscriber's receive failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecvError {
    /// The bus was dropped and no further events will arrive.
    #[error("the event bus has closed")]
    Closed,

    /// The subscriber fell behind and events were dropped.
    ///
    /// Carries how many were lost. The subscriber is still usable and the next
    /// receive yields the oldest event still buffered; the consumer is expected
    /// to resynchronise by refetching, which it must be able to do after a
    /// reconnect anyway.
    #[error("subscriber lagged; {0} events were dropped")]
    Lagged(u64),
}

/// A broadcast bus over [`DomainEvent`]s.
///
/// Cloning is not needed: share it as an `Arc`. It is the [`EventSink`] the
/// composition root hands to the core services.
#[derive(Debug)]
pub struct EventBus {
    sender: broadcast::Sender<DomainEvent>,
    /// Guards `seq` assignment so the total order has no duplicates or gaps.
    ///
    /// A mutex rather than an atomic because stamping and publishing must be
    /// one step: with an atomic, two threads could take sequence numbers 1 and
    /// 2 and then publish them in the order 2, 1, and the "total order" would
    /// be a lie. The critical section holds no lock across an await and does no
    /// I/O — `broadcast::send` only pushes into ring buffers.
    seq: Mutex<u64>,
    /// Events published since start. Diagnostics only; never a correctness input.
    published: AtomicUsize,
    /// The most recent events, oldest first, for `resume` replay.
    ///
    /// Guarded by its own mutex rather than folded into `seq` so that a
    /// resuming connection reading the window cannot stall a publisher. Writes
    /// still happen inside the `seq` critical section, so the window's order is
    /// the total order by construction.
    replay: Mutex<VecDeque<DomainEvent>>,
    /// Fixed size of `replay`. Zero disables retention entirely.
    replay_capacity: usize,
}

impl EventBus {
    /// Creates a bus with [`DEFAULT_CAPACITY`] per subscriber.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Creates a bus with an explicit per-subscriber queue capacity.
    ///
    /// # Panics
    /// Never in practice: a zero capacity is raised to one, because a bus that
    /// can hold nothing would drop every event and silently look healthy.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacities(capacity, DEFAULT_REPLAY_CAPACITY)
    }

    /// Creates a bus with explicit per-subscriber and replay-window capacities.
    ///
    /// A `replay_capacity` of zero disables `resume` replay: the bus retains
    /// nothing and every resume request reports an unrecoverable gap. That is a
    /// supported configuration, not a degenerate one — it is the pre-ADR-100
    /// behaviour.
    ///
    /// # Panics
    /// Never in practice: a zero subscriber capacity is raised to one, for the
    /// reason given on [`EventBus::with_capacity`].
    #[must_use]
    pub fn with_capacities(capacity: usize, replay_capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self {
            sender,
            seq: Mutex::new(0),
            published: AtomicUsize::new(0),
            replay: Mutex::new(VecDeque::with_capacity(replay_capacity)),
            replay_capacity,
        }
    }

    /// Subscribes to every event published from now on.
    ///
    /// Events published *before* this call are not replayed: the bus is a live
    /// stream, not a log (`EVENT_MODEL.md` §5). Dropping the returned
    /// [`Subscriber`] releases the subscription and its queue.
    #[must_use]
    pub fn subscribe(&self) -> Subscriber {
        Subscriber {
            receiver: self.sender.subscribe(),
        }
    }

    /// How many subscribers are currently attached.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// How many events have been published since the bus was created.
    #[must_use]
    pub fn published_count(&self) -> usize {
        self.published.load(Ordering::Relaxed)
    }

    /// Publishes an event, stamping it with the next sequence number.
    ///
    /// Returns the `seq` assigned. Never blocks; having no subscribers is
    /// success, not an error — the core must not care whether anyone is
    /// listening.
    pub fn publish(&self, mut event: DomainEvent) -> u64 {
        // If the mutex is poisoned a previous holder panicked mid-stamp. The
        // guarded state is a single counter with no invariant beyond
        // monotonicity, so recovering preserves ordering; refusing to would
        // silently stop all events for the life of the process.
        let assigned = {
            let mut seq = match self.seq.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            *seq += 1;
            event.seq = *seq;
            // Retain before sending, inside the same critical section: a
            // resuming client must never find a gap between what the window
            // holds and what a live subscriber was handed.
            self.retain_for_replay(&event);
            // Send inside the critical section: stamping and publishing must be
            // atomic together, or subscribers could observe seq out of order.
            // `send` pushes into bounded ring buffers and never awaits.
            let _ = self.sender.send(event);
            *seq
        };

        self.published.fetch_add(1, Ordering::Relaxed);
        assigned
    }

    /// Appends to the replay window, evicting the oldest event when full.
    fn retain_for_replay(&self, event: &DomainEvent) {
        if self.replay_capacity == 0 {
            return;
        }
        // Poisoning carries no broken invariant here: the window is an ordered
        // cache, and refusing to use it would silently disable resume forever.
        let mut replay = match self.replay.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if replay.len() == self.replay_capacity {
            replay.pop_front();
        }
        replay.push_back(event.clone());
    }

    /// Returns every retained event with `seq` strictly greater than `after_seq`.
    ///
    /// `Ok` carries the replay, oldest first, and is empty when the caller is
    /// already current. `Err` carries [`ReplayGap`] when the window cannot
    /// prove it holds everything the caller missed — the caller must
    /// resynchronise by refetching instead of assuming it is up to date.
    ///
    /// # Errors
    /// Returns [`ReplayGap`] if retention is disabled, or if `after_seq` is
    /// older than the oldest event still retained.
    pub fn replay_since(&self, after_seq: u64) -> Result<Vec<DomainEvent>, ReplayGap> {
        if self.replay_capacity == 0 {
            return Err(ReplayGap { oldest_seq: None });
        }
        let replay = match self.replay.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let Some(oldest) = replay.front().map(|e| e.seq) else {
            // Nothing published yet. A caller at seq 0 is current; one claiming
            // a later seq is talking about a bus that no longer exists.
            return if after_seq == 0 {
                Ok(Vec::new())
            } else {
                Err(ReplayGap { oldest_seq: None })
            };
        };

        // The window must contain the event *after* `after_seq`. If the oldest
        // retained seq is further along than that, the events in between were
        // evicted and the caller cannot be made whole.
        if after_seq + 1 < oldest {
            return Err(ReplayGap {
                oldest_seq: Some(oldest),
            });
        }

        Ok(replay
            .iter()
            .filter(|e| e.seq > after_seq)
            .cloned()
            .collect())
    }
}

/// The replay window could not satisfy a `resume` request.
///
/// The caller missed events that are no longer retained, so it must
/// resynchronise from the REST API rather than continue from the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayGap {
    /// The oldest `seq` still retained, or `None` when nothing is retained.
    pub oldest_seq: Option<u64>,
}

impl std::fmt::Display for ReplayGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.oldest_seq {
            Some(oldest) => write!(
                f,
                "replay window starts at seq {oldest}; earlier events were evicted"
            ),
            None => write!(f, "no events are retained for replay"),
        }
    }
}

impl std::error::Error for ReplayGap {}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// The bus is the core's [`EventSink`].
///
/// `emit` discards the assigned sequence number: the core has no use for it,
/// and the port stays free of any bus concept (`EVENT_MODEL.md` §2).
impl EventSink for EventBus {
    fn emit(&self, event: DomainEvent) {
        self.publish(event);
    }
}

/// The bus is also the core's [`EventSource`].
///
/// This is what lets an adapter react to changes without depending on this
/// crate: IMAP `IDLE` holds an `Arc<dyn EventSource>`, and the composition root
/// is the only place that knows the implementation is this bus
/// (`EVENT_MODEL.md` §2).
impl EventSource for EventBus {
    fn subscribe(&self) -> Box<dyn EventStream> {
        Box::new(Self::subscribe(self))
    }
}

/// A [`Subscriber`] is the core's [`EventStream`].
///
/// [`RecvError::Lagged`] is deliberately *not* an end of stream: the subscriber
/// missed events but remains usable, so this resumes with the next one rather
/// than terminating the caller's `IDLE`. The count is logged, because a
/// consumer that silently skipped events is worth knowing about.
#[async_trait::async_trait]
impl EventStream for Subscriber {
    async fn next(&mut self) -> Option<DomainEvent> {
        loop {
            match self.recv().await {
                Ok(event) => return Some(event),
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "event subscriber lagged; events were dropped");
                }
                Err(RecvError::Closed) => return None,
            }
        }
    }
}

/// One subscription to the bus.
///
/// Holds a bounded queue. Dropping it unsubscribes and frees that queue, so a
/// disconnected `WebSocket` client leaks nothing.
#[derive(Debug)]
pub struct Subscriber {
    receiver: broadcast::Receiver<DomainEvent>,
}

impl Subscriber {
    /// Waits for the next event.
    ///
    /// # Errors
    /// [`RecvError::Lagged`] if the queue overflowed — the count says how many
    /// events were dropped, and the subscription remains usable.
    /// [`RecvError::Closed`] once the bus is gone.
    pub async fn recv(&mut self) -> Result<DomainEvent, RecvError> {
        match self.receiver.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(n)) => Err(RecvError::Lagged(n)),
            Err(broadcast::error::RecvError::Closed) => Err(RecvError::Closed),
        }
    }

    /// Takes an event if one is already queued, without waiting.
    ///
    /// # Errors
    /// As [`recv`](Self::recv); `Ok(None)` means the queue is merely empty.
    pub fn try_recv(&mut self) -> Result<Option<DomainEvent>, RecvError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(broadcast::error::TryRecvError::Lagged(n)) => Err(RecvError::Lagged(n)),
            Err(broadcast::error::TryRecvError::Closed) => Err(RecvError::Closed),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use sandesh_core::{AccountId, EntryId, EventPayload, MailboxId, MessageId, Uid};
    use std::sync::Arc;

    fn entry_created() -> EventPayload {
        EventPayload::EntryCreated {
            entry_id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: MailboxId::new(),
            uid: Uid::MIN,
        }
    }

    fn account_created(username: &str) -> EventPayload {
        EventPayload::AccountCreated {
            account_id: AccountId::new(),
            username: username.to_owned(),
        }
    }

    /// Usernames of a replay, in order, so assertions read as the sequence.
    fn replayed_names(events: &[DomainEvent]) -> Vec<String> {
        events
            .iter()
            .map(|e| match &e.payload {
                EventPayload::AccountCreated { username, .. } => username.clone(),
                other => panic!("unexpected payload: {other:?}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn replay_returns_only_events_after_the_requested_seq() {
        let bus = EventBus::new();
        for name in ["a", "b", "c"] {
            bus.publish(DomainEvent::new(account_created(name)));
        }

        // Resuming after the first event replays exactly the two it missed.
        let replayed = bus.replay_since(1).expect("within window");
        assert_eq!(replayed_names(&replayed), ["b", "c"]);
    }

    #[tokio::test]
    async fn replay_from_zero_returns_the_whole_window() {
        let bus = EventBus::new();
        for name in ["a", "b"] {
            bus.publish(DomainEvent::new(account_created(name)));
        }

        let replayed = bus.replay_since(0).expect("within window");
        assert_eq!(replayed_names(&replayed), ["a", "b"]);
    }

    #[tokio::test]
    async fn replay_is_empty_when_the_caller_is_already_current() {
        let bus = EventBus::new();
        bus.publish(DomainEvent::new(account_created("a")));

        // Current, not gapped: an up-to-date client must not be told to refetch.
        assert!(bus.replay_since(1).expect("within window").is_empty());
    }

    #[tokio::test]
    async fn replay_on_an_empty_bus_is_current_at_zero_and_gapped_beyond() {
        let bus = EventBus::new();
        assert!(bus.replay_since(0).expect("nothing missed").is_empty());
        // A seq this bus never issued: the client is talking about another run.
        assert_eq!(bus.replay_since(7), Err(ReplayGap { oldest_seq: None }));
    }

    #[tokio::test]
    async fn replay_reports_a_gap_once_the_requested_seq_is_evicted() {
        let bus = EventBus::with_capacities(DEFAULT_CAPACITY, 2);
        for name in ["a", "b", "c"] {
            bus.publish(DomainEvent::new(account_created(name)));
        }

        // The window holds b(2) and c(3). Resuming after 1 is still whole,
        // because the next event needed is 2 and 2 is retained.
        assert_eq!(
            replayed_names(&bus.replay_since(1).expect("boundary is recoverable")),
            ["b", "c"]
        );

        // Resuming after 0 needs event 1, which was evicted: unrecoverable.
        assert_eq!(
            bus.replay_since(0),
            Err(ReplayGap {
                oldest_seq: Some(2)
            })
        );
    }

    #[tokio::test]
    async fn the_replay_window_is_bounded() {
        let bus = EventBus::with_capacities(DEFAULT_CAPACITY, 3);
        for i in 0..50 {
            bus.publish(DomainEvent::new(account_created(&format!("n{i}"))));
        }

        // Memory is bounded regardless of how much was published.
        let replayed = bus.replay_since(47).expect("within window");
        assert_eq!(replayed_names(&replayed), ["n47", "n48", "n49"]);
    }

    #[tokio::test]
    async fn zero_replay_capacity_disables_resume() {
        let bus = EventBus::with_capacities(DEFAULT_CAPACITY, 0);
        bus.publish(DomainEvent::new(account_created("a")));

        assert_eq!(bus.replay_since(0), Err(ReplayGap { oldest_seq: None }));
    }

    #[tokio::test]
    async fn replay_order_matches_the_total_order() {
        let bus = EventBus::new();
        for i in 0..100 {
            bus.publish(DomainEvent::new(account_created(&format!("n{i}"))));
        }

        let replayed = bus.replay_since(0).expect("within window");
        let seqs: Vec<u64> = replayed.iter().map(|e| e.seq).collect();
        let expected: Vec<u64> = (1..=100).collect();
        assert_eq!(seqs, expected, "replay must be the gapless total order");
    }

    #[tokio::test]
    async fn publish_reaches_a_subscriber() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();

        bus.publish(DomainEvent::new(entry_created()));

        let event = sub.recv().await.expect("event delivered");
        assert_eq!(event.kind(), sandesh_core::EventKind::EntryCreated);
    }

    #[tokio::test]
    async fn every_subscriber_receives_every_event() {
        // The fan-out property the WebSocket layer depends on.
        let bus = EventBus::new();
        let mut subs: Vec<_> = (0..3).map(|_| bus.subscribe()).collect();

        bus.publish(DomainEvent::new(account_created("a")));
        bus.publish(DomainEvent::new(account_created("b")));

        for sub in &mut subs {
            for expected in ["a", "b"] {
                let event = sub.recv().await.expect("event delivered");
                match event.payload {
                    EventPayload::AccountCreated { username, .. } => {
                        assert_eq!(username, expected);
                    }
                    other => panic!("unexpected payload: {other:?}"),
                }
            }
        }
    }

    #[tokio::test]
    async fn seq_is_a_gapless_total_order() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();

        for _ in 0..10 {
            bus.publish(DomainEvent::new(entry_created()));
        }

        for expected in 1..=10 {
            let event = sub.recv().await.expect("event delivered");
            assert_eq!(event.seq, expected, "seq must be dense and increasing");
        }
    }

    #[tokio::test]
    async fn all_subscribers_observe_the_same_order() {
        let bus = EventBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();

        for _ in 0..5 {
            bus.publish(DomainEvent::new(entry_created()));
        }

        for _ in 0..5 {
            let from_a = a.recv().await.expect("event delivered");
            let from_b = b.recv().await.expect("event delivered");
            assert_eq!(from_a.seq, from_b.seq);
            assert_eq!(from_a.id, from_b.id);
        }
    }

    #[tokio::test]
    async fn concurrent_publishers_produce_no_duplicate_seq() {
        // Stamping and sending are one critical section; prove it under
        // contention rather than trusting the comment.
        let bus = Arc::new(EventBus::with_capacity(4096));
        let mut sub = bus.subscribe();

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let bus = Arc::clone(&bus);
            tasks.push(tokio::spawn(async move {
                for _ in 0..50 {
                    bus.publish(DomainEvent::new(entry_created()));
                }
            }));
        }
        for task in tasks {
            task.await.expect("publisher task");
        }

        let mut seen = Vec::new();
        while let Ok(Some(event)) = sub.try_recv() {
            seen.push(event.seq);
        }
        assert_eq!(seen.len(), 400, "no event may be lost within capacity");

        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 400, "seq must be unique");
        assert_eq!(seen, sorted, "delivery order must match seq order");
    }

    #[tokio::test]
    async fn dropping_a_subscriber_releases_it() {
        let bus = EventBus::new();
        let sub = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);

        drop(sub);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn one_subscriber_leaving_does_not_disturb_the_others() {
        let bus = EventBus::new();
        let leaving = bus.subscribe();
        let mut staying = bus.subscribe();

        drop(leaving);
        bus.publish(DomainEvent::new(account_created("after")));

        let event = staying.recv().await.expect("survivor still receives");
        match event.payload {
            EventPayload::AccountCreated { username, .. } => assert_eq!(username, "after"),
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn publishing_with_no_subscribers_succeeds() {
        // The core must not care whether anyone is listening.
        let bus = EventBus::new();
        assert_eq!(bus.publish(DomainEvent::new(entry_created())), 1);
        assert_eq!(bus.published_count(), 1);
    }

    #[tokio::test]
    async fn a_full_subscriber_is_lagged_not_blocking() {
        // The critical acceptance property: a subscriber that never reads must
        // not stop the producer. Overflow the queue by 10x and require every
        // publish to complete.
        let bus = EventBus::with_capacity(4);
        let mut slow = bus.subscribe();

        for _ in 0..40 {
            bus.publish(DomainEvent::new(entry_created()));
        }
        assert_eq!(bus.published_count(), 40, "producer was never blocked");

        let err = slow.recv().await.expect_err("must report lag");
        match err {
            RecvError::Lagged(dropped) => assert_eq!(dropped, 36),
            RecvError::Closed => panic!("bus is still alive"),
        }
    }

    #[tokio::test]
    async fn a_lagged_subscriber_recovers_with_the_newest_events() {
        // Oldest are dropped, so what survives is what just happened.
        let bus = EventBus::with_capacity(2);
        let mut slow = bus.subscribe();

        for name in ["1", "2", "3", "4"] {
            bus.publish(DomainEvent::new(account_created(name)));
        }

        assert!(matches!(slow.recv().await, Err(RecvError::Lagged(2))));
        let event = slow.recv().await.expect("still usable after lag");
        match event.payload {
            EventPayload::AccountCreated { username, .. } => {
                assert_eq!(username, "3", "the two oldest were dropped");
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_slow_subscriber_does_not_starve_a_fast_one() {
        let bus = EventBus::with_capacity(4);
        let _slow = bus.subscribe();
        let mut fast = bus.subscribe();

        for _ in 0..4 {
            bus.publish(DomainEvent::new(entry_created()));
        }

        for expected in 1..=4 {
            let event = fast.recv().await.expect("fast subscriber unaffected");
            assert_eq!(event.seq, expected);
        }
    }

    #[tokio::test]
    async fn closed_bus_reports_closed() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();
        drop(bus);
        assert_eq!(sub.recv().await, Err(RecvError::Closed));
    }

    #[tokio::test]
    async fn subscribing_late_does_not_replay() {
        // The bus is a live stream, not a log (EVENT_MODEL §5).
        let bus = EventBus::new();
        bus.publish(DomainEvent::new(account_created("before")));

        let mut late = bus.subscribe();
        bus.publish(DomainEvent::new(account_created("after")));

        let event = late.recv().await.expect("event delivered");
        match event.payload {
            EventPayload::AccountCreated { username, .. } => assert_eq!(username, "after"),
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bus_is_an_event_sink() {
        // The core holds it as a `dyn EventSink` and knows nothing more.
        let bus = Arc::new(EventBus::new());
        let mut sub = bus.subscribe();

        let sink: Arc<dyn EventSink> = Arc::clone(&bus) as Arc<dyn EventSink>;
        sink.emit(DomainEvent::new(entry_created()));

        let event = sub.recv().await.expect("emitted through the port");
        assert_eq!(event.seq, 1, "the bus stamps seq even via the port");
    }

    #[tokio::test]
    async fn zero_capacity_is_raised_rather_than_silently_dropping() {
        let bus = EventBus::with_capacity(0);
        let mut sub = bus.subscribe();
        bus.publish(DomainEvent::new(entry_created()));
        assert!(sub.recv().await.is_ok());
    }
}
