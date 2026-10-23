//! Event collection for the storage layer.
//!
//! Phase 1 has no event bus — that arrives with Phase 3. Repositories still
//! *emit* domain facts, because emission is part of the operation's contract
//! and retrofitting it later would mean revisiting every write path.
//!
//! [`EventRecorder`] is a thin indirection over an [`EventSink`]: by default it
//! discards, and tests substitute a recorder that keeps events so they can
//! assert on what was emitted.

use sandesh_core::{DomainEvent, EventPayload, EventSink, NullEventSink};
use std::sync::Arc;

/// Where a repository sends the events it produces.
#[derive(Clone)]
pub struct EventRecorder {
    sink: Arc<dyn EventSink>,
}

impl EventRecorder {
    /// A recorder that discards everything.
    #[must_use]
    pub fn null() -> Self {
        Self {
            sink: Arc::new(NullEventSink),
        }
    }

    /// A recorder that forwards to `sink`.
    #[must_use]
    pub fn new(sink: Arc<dyn EventSink>) -> Self {
        Self { sink }
    }

    /// Wraps a payload in an envelope and emits it.
    pub fn emit(&self, payload: EventPayload) {
        self.sink.emit(DomainEvent::new(payload));
    }
}

impl Default for EventRecorder {
    fn default() -> Self {
        Self::null()
    }
}

impl std::fmt::Debug for EventRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventRecorder").finish_non_exhaustive()
    }
}

/// An [`EventSink`] that keeps every event, for assertions in tests.
#[derive(Debug, Default)]
pub struct CollectingSink {
    events: std::sync::Mutex<Vec<DomainEvent>>,
}

impl CollectingSink {
    /// Creates an empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A snapshot of the events emitted so far.
    ///
    /// Returns empty if the lock was poisoned: an observation helper must never
    /// panic a test for a reason unrelated to what the test is checking.
    #[must_use]
    pub fn events(&self) -> Vec<DomainEvent> {
        self.events.lock().map(|e| e.clone()).unwrap_or_default()
    }

    /// The kinds emitted so far, in order.
    #[must_use]
    pub fn kinds(&self) -> Vec<sandesh_core::EventKind> {
        self.events().iter().map(DomainEvent::kind).collect()
    }
}

impl EventSink for CollectingSink {
    fn emit(&self, event: DomainEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}
