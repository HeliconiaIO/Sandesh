//! The message-in-mailbox placement — the entity that makes IMAP possible.

use crate::{EntryId, MailboxId, MessageFlags, MessageId, ModSeq, Timestamp, Uid};

/// One placement of a [`Message`](crate::Message) in a
/// [`Mailbox`](crate::Mailbox).
///
/// This is the linchpin of the model (`CORE_SPEC.md` §4.5, ADR-009). A message
/// may be placed in several mailboxes, and each placement carries its **own**
/// UID and its **own** flags:
///
/// ```text
/// Message #100
///   ├── INBOX    uid = 42   flags = \Seen
///   └── Archive  uid = 18   flags = (none)
/// ```
///
/// That is exactly IMAP `COPY` semantics, and it is why `messages` has no
/// `mailbox_id` column: a single-owner field cannot express it, and working
/// around that would mean duplicating the message — breaking the guarantee that
/// raw bytes are stored exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageEntry {
    /// Identity of this placement.
    pub id: EntryId,
    /// The message being placed. Shared, immutable.
    pub message_id: MessageId,
    /// The mailbox it is placed in.
    pub mailbox_id: MailboxId,
    /// UID within this mailbox. Unique per mailbox, never reused.
    pub uid: Uid,
    /// Flags for **this placement only**.
    pub flags: MessageFlags,
    /// IMAP `INTERNALDATE`, set when the placement is made.
    pub internal_date: Timestamp,
    /// When the placement was created.
    pub added_at: Timestamp,
    /// The modification sequence this placement was last changed at.
    ///
    /// Drawn from the owning mailbox's sequence, not from a counter of its
    /// own: CONDSTORE (RFC 7162 §3) compares this against a client's saved
    /// value, which only works if the numbering is shared across the mailbox.
    /// Stamped at insert and re-stamped on every successful flag change.
    pub mod_seq: ModSeq,
}

impl MessageEntry {
    /// Whether this placement is marked `\Deleted`.
    ///
    /// `\Deleted` marks; it does not remove. Removal happens at `EXPUNGE`.
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        self.flags.has(crate::SystemFlags::DELETED)
    }

    /// Whether this placement is marked `\Seen`.
    #[must_use]
    pub fn is_seen(&self) -> bool {
        self.flags.has(crate::SystemFlags::SEEN)
    }
}
