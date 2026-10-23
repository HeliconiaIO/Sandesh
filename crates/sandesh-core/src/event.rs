//! Domain events (`EVENT_MODEL.md`).
//!
//! Events are **domain facts**, not transport messages. They carry identifiers
//! and minimal context, never message bodies — a consumer that wants content
//! fetches it. That keeps frame sizes bounded and stops a large attachment from
//! starving a slow subscriber.
//!
//! Naming follows the model rather than convention: a `Message` is immutable
//! content and cannot be "updated" or "moved", so the events that describe
//! mutation are named for the *placement* that actually changes
//! ([`EntryFlagsChanged`](EventKind::EntryFlagsChanged),
//! [`EntryMoved`](EventKind::EntryMoved)).
//!
//! Nothing here references WebSocket, HTTP, or any UI concept. The same events
//! drive the live inbox, IMAP `IDLE`, logging and metrics.

use crate::{AccountId, DeliveryId, EntryId, EventId, MailboxId, MessageId, Timestamp, Uid};

/// The discriminant of a [`DomainEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// A message was delivered and is queryable.
    MessageReceived,
    /// A message's last placement was removed and the message was collected.
    MessageDeleted,
    /// A placement was created (delivery, `COPY`, `APPEND`).
    EntryCreated,
    /// A placement was removed (`EXPUNGE`, delete).
    EntryDeleted,
    /// A placement's flags changed.
    EntryFlagsChanged,
    /// A placement moved between mailboxes.
    EntryMoved,
    /// A mailbox was created.
    MailboxCreated,
    /// A mailbox was deleted.
    MailboxDeleted,
    /// A mailbox was renamed.
    MailboxRenamed,
    /// A mailbox's subscription state changed.
    MailboxSubscriptionChanged,
    /// An account was created.
    AccountCreated,
    /// An account was deleted.
    AccountDeleted,
    /// A delivery failed.
    DeliveryFailed,
    /// A workspace was created.
    WorkspaceCreated,
    /// A workspace was deleted.
    WorkspaceDeleted,
    /// The active workspace changed.
    WorkspaceSwitched,
    /// A workspace's data was purged.
    WorkspacePurged,
}

impl EventKind {
    /// The stable wire name, used by logging and later by the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MessageReceived => "MessageReceived",
            Self::MessageDeleted => "MessageDeleted",
            Self::EntryCreated => "EntryCreated",
            Self::EntryDeleted => "EntryDeleted",
            Self::EntryFlagsChanged => "EntryFlagsChanged",
            Self::EntryMoved => "EntryMoved",
            Self::MailboxCreated => "MailboxCreated",
            Self::MailboxDeleted => "MailboxDeleted",
            Self::MailboxRenamed => "MailboxRenamed",
            Self::MailboxSubscriptionChanged => "MailboxSubscriptionChanged",
            Self::AccountCreated => "AccountCreated",
            Self::AccountDeleted => "AccountDeleted",
            Self::DeliveryFailed => "DeliveryFailed",
            Self::WorkspaceCreated => "WorkspaceCreated",
            Self::WorkspaceDeleted => "WorkspaceDeleted",
            Self::WorkspaceSwitched => "WorkspaceSwitched",
            Self::WorkspacePurged => "WorkspacePurged",
        }
    }
}

/// The data carried by an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventPayload {
    /// A message became available in a mailbox.
    MessageReceived {
        /// The message.
        message_id: MessageId,
        /// The placement created.
        entry_id: EntryId,
        /// Where it landed.
        mailbox_id: MailboxId,
        /// Which account owns that mailbox.
        account_id: AccountId,
        /// The assigned UID.
        uid: Uid,
        /// `RFC822.SIZE`.
        size: u64,
    },
    /// A message was garbage-collected after its last placement was removed.
    MessageDeleted {
        /// The message.
        message_id: MessageId,
        /// The mailboxes it had been placed in.
        mailbox_ids: Vec<MailboxId>,
    },
    /// A placement was created.
    EntryCreated {
        /// The placement.
        entry_id: EntryId,
        /// The message placed.
        message_id: MessageId,
        /// The mailbox.
        mailbox_id: MailboxId,
        /// The assigned UID.
        uid: Uid,
    },
    /// A placement was removed.
    EntryDeleted {
        /// The placement.
        entry_id: EntryId,
        /// The message it referenced.
        message_id: MessageId,
        /// The mailbox.
        mailbox_id: MailboxId,
        /// The UID that was in use.
        uid: Uid,
    },
    /// A placement's flags changed.
    EntryFlagsChanged {
        /// The placement.
        entry_id: EntryId,
        /// The mailbox.
        mailbox_id: MailboxId,
        /// The UID.
        uid: Uid,
        /// Flag names added.
        added: Vec<String>,
        /// Flag names removed.
        removed: Vec<String>,
        /// The resulting flag names.
        flags_now: Vec<String>,
        /// The modification sequence the change was stamped with.
        ///
        /// Carried on the event so a CONDSTORE session can send the
        /// unsolicited `FETCH ... MODSEQ` that RFC 7162 §3.2 requires without
        /// a second read: the value is already known to the writer, and
        /// re-reading it could observe a *later* change and report a modseq
        /// that does not correspond to these flags.
        mod_seq: crate::ModSeq,
    },
    /// A placement moved between mailboxes.
    EntryMoved {
        /// The placement.
        entry_id: EntryId,
        /// The message placed.
        message_id: MessageId,
        /// The source mailbox.
        from_mailbox_id: MailboxId,
        /// The target mailbox.
        to_mailbox_id: MailboxId,
        /// The UID in the source mailbox.
        old_uid: Uid,
        /// The UID assigned in the target mailbox.
        new_uid: Uid,
    },
    /// A mailbox was created.
    MailboxCreated {
        /// The mailbox.
        mailbox_id: MailboxId,
        /// Which account owns it.
        account_id: AccountId,
        /// Its name.
        name: String,
        /// The mailbox's `UIDVALIDITY`.
        uid_validity: u32,
    },
    /// A mailbox was deleted.
    MailboxDeleted {
        /// The mailbox.
        mailbox_id: MailboxId,
        /// Which account owned it.
        account_id: AccountId,
        /// Its name.
        name: String,
    },
    /// A mailbox was renamed.
    MailboxRenamed {
        /// The mailbox.
        mailbox_id: MailboxId,
        /// The previous name.
        old_name: String,
        /// The new name.
        new_name: String,
    },
    /// A mailbox subscription changed.
    MailboxSubscriptionChanged {
        /// The mailbox.
        mailbox_id: MailboxId,
        /// The new subscription state.
        subscribed: bool,
    },
    /// An account was created.
    AccountCreated {
        /// The account.
        account_id: AccountId,
        /// Its username.
        username: String,
    },
    /// An account was deleted.
    AccountDeleted {
        /// The account.
        account_id: AccountId,
        /// Its username.
        username: String,
    },
    /// A delivery failed.
    DeliveryFailed {
        /// The delivery record.
        delivery_id: DeliveryId,
        /// Why it failed.
        reason: String,
    },
    /// A workspace was created.
    WorkspaceCreated {
        /// Workspace slug.
        workspace_id: String,
        /// Workspace display name.
        name: String,
    },
    /// A workspace was deleted.
    WorkspaceDeleted {
        /// Workspace slug.
        workspace_id: String,
    },
    /// The active workspace changed.
    WorkspaceSwitched {
        /// Previous active workspace slug.
        previous: String,
        /// Newly active workspace slug.
        current: String,
    },
    /// A workspace's data was purged.
    WorkspacePurged {
        /// Workspace slug.
        workspace_id: String,
        /// Number of messages deleted.
        deleted_messages: u64,
        /// Number of traces deleted.
        deleted_traces: u64,
        /// Reclaimed disk bytes.
        reclaimed_bytes: u64,
    },
}

impl EventPayload {
    /// The kind corresponding to this payload.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::MessageReceived { .. } => EventKind::MessageReceived,
            Self::MessageDeleted { .. } => EventKind::MessageDeleted,
            Self::EntryCreated { .. } => EventKind::EntryCreated,
            Self::EntryDeleted { .. } => EventKind::EntryDeleted,
            Self::EntryFlagsChanged { .. } => EventKind::EntryFlagsChanged,
            Self::EntryMoved { .. } => EventKind::EntryMoved,
            Self::MailboxCreated { .. } => EventKind::MailboxCreated,
            Self::MailboxDeleted { .. } => EventKind::MailboxDeleted,
            Self::MailboxRenamed { .. } => EventKind::MailboxRenamed,
            Self::MailboxSubscriptionChanged { .. } => EventKind::MailboxSubscriptionChanged,
            Self::AccountCreated { .. } => EventKind::AccountCreated,
            Self::AccountDeleted { .. } => EventKind::AccountDeleted,
            Self::DeliveryFailed { .. } => EventKind::DeliveryFailed,
            Self::WorkspaceCreated { .. } => EventKind::WorkspaceCreated,
            Self::WorkspaceDeleted { .. } => EventKind::WorkspaceDeleted,
            Self::WorkspaceSwitched { .. } => EventKind::WorkspaceSwitched,
            Self::WorkspacePurged { .. } => EventKind::WorkspacePurged,
        }
    }
}

/// A domain event with its envelope.
///
/// `seq` is assigned by the bus at publication, not here: it is a property of
/// the total order, not of the fact. Phase 1 leaves it at zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainEvent {
    /// Identity.
    pub id: EventId,
    /// Position in the bus's total order; assigned at publication.
    pub seq: u64,
    /// When the fact occurred.
    pub occurred_at: Timestamp,
    /// The payload, which determines the kind.
    pub payload: EventPayload,
}

impl DomainEvent {
    /// Creates an event, stamping it with a fresh id and the current time.
    #[must_use]
    pub fn new(payload: EventPayload) -> Self {
        Self {
            id: EventId::new(),
            seq: 0,
            occurred_at: Timestamp::now(),
            payload,
        }
    }

    /// The event kind.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        self.payload.kind()
    }
}

/// Where the core sends domain events.
///
/// `emit` takes `&self`, returns nothing, and cannot fail. A subscriber problem
/// must never fail a mail delivery: by the time an event is emitted the mail is
/// already committed, and the event is a notification about that fact, not part
/// of establishing it.
///
/// This is intentionally **not** async and carries no channel type. Phase 3
/// supplies a real broadcast bus; a synchronous sink keeps the core free of any
/// runtime dependency.
pub trait EventSink: Send + Sync {
    /// Publishes an event. Never blocks, never fails.
    fn emit(&self, event: DomainEvent);
}

/// A live subscription to the event stream.
///
/// The read side of [`EventSink`], and the reason IMAP `IDLE` needs no
/// dependency on the bus implementation: an adapter that wants to react to
/// changes depends on this port, exactly as one that wants to announce them
/// depends on `EventSink`. The bus crate supplies the implementation, the
/// composition root injects it, and neither adapter knows the other exists
/// (`EVENT_MODEL.md` §2, ARCHITECTURE §3.2).
///
/// Dropping the subscription must release whatever queue backs it, so a
/// disconnected client leaks nothing.
#[async_trait::async_trait]
pub trait EventStream: Send + Sync {
    /// Waits for the next event.
    ///
    /// Returns `None` once no further event can arrive — the source is gone,
    /// or the subscription has ended. A subscriber that fell behind and lost
    /// events still receives the ones that follow: lag is not termination, and
    /// the implementation is expected to keep the subscription usable.
    async fn next(&mut self) -> Option<DomainEvent>;
}

/// A source that hands out [`EventStream`] subscriptions.
///
/// Separate from the stream itself so that one shared source can be held for
/// the life of the process while each session takes its own subscription and
/// drops it when it ends.
pub trait EventSource: Send + Sync {
    /// Subscribes to every event published from *now* on.
    ///
    /// Events published earlier are not replayed: this is a live stream, not a
    /// log. A consumer that needs the earlier state fetches it (`EVENT_MODEL.md`
    /// §5).
    fn subscribe(&self) -> Box<dyn EventStream>;
}

/// An [`EventSink`] that discards everything.
///
/// The default for contexts with no interested consumer, and for tests that
/// exercise behaviour rather than notification.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullEventSink;

impl EventSink for NullEventSink {
    fn emit(&self, _event: DomainEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_determines_kind() {
        let payload = EventPayload::EntryCreated {
            entry_id: EntryId::new(),
            message_id: MessageId::new(),
            mailbox_id: MailboxId::new(),
            uid: Uid::MIN,
        };
        assert_eq!(DomainEvent::new(payload).kind(), EventKind::EntryCreated);
    }

    #[test]
    fn event_names_are_stable() {
        // These strings become the wire contract in Phase 3.
        assert_eq!(EventKind::MessageReceived.as_str(), "MessageReceived");
        assert_eq!(EventKind::EntryFlagsChanged.as_str(), "EntryFlagsChanged");
    }

    #[test]
    fn no_message_updated_or_moved_event_exists() {
        // A Message is immutable; only placements change. Named events must not
        // suggest otherwise.
        for kind in [
            EventKind::MessageReceived,
            EventKind::MessageDeleted,
            EventKind::EntryCreated,
            EventKind::EntryDeleted,
            EventKind::EntryFlagsChanged,
            EventKind::EntryMoved,
        ] {
            let name = kind.as_str();
            assert!(name != "MessageUpdated" && name != "MessageMoved");
        }
    }

    #[test]
    fn null_sink_accepts_events() {
        let sink = NullEventSink;
        sink.emit(DomainEvent::new(EventPayload::AccountDeleted {
            account_id: AccountId::new(),
            username: String::from("dev"),
        }));
    }
}
