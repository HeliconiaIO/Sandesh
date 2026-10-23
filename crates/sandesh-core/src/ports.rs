//! Storage ports — the traits the core needs the outside world to satisfy.
//!
//! The core **defines** these; `sandesh-storage` implements them. Not the
//! reverse. No SQL type, connection, transaction handle or database error ever
//! appears in a signature here, which is what keeps a `PostgreSQL` backend (or an
//! in-memory test double) a genuine drop-in substitution.
//!
//! # Why these are async
//!
//! `async_trait` is a proc macro that desugars to `Pin<Box<dyn Future>>`. It
//! introduces **no async runtime** — the core has no `tokio` dependency, which
//! the architecture test verifies mechanically. The methods are async because
//! any real storage backend performs I/O and `sqlx` is async; forcing a
//! synchronous port would push `block_on` into the storage layer and make it
//! runtime-hostile.
//!
//! # Granularity
//!
//! Five traits, split by aggregate rather than by entity. A separate
//! `MessageEntryRepository` was considered and rejected: entries are only ever
//! created or queried in the context of a mailbox's UID sequence, and splitting
//! them would put the UID-assignment transaction across a port boundary where
//! it could not be made atomic.

use crate::{
    Account, AccountId, Attachment, AttachmentId, ConnectionRecord, CoreResult, Delivery,
    EmailAddress, EntryId, Mailbox, MailboxId, MailboxName, Message, MessageEntry, MessageFlags,
    MessageId, RawMessage, SystemFlags, Timestamp, Uid,
};
use async_trait::async_trait;
use std::collections::BTreeSet;

/// A message plus its intended placements, submitted as one atomic unit.
///
/// Delivery is atomic per message (`CORE_SPEC.md` §7.5): either the message and
/// every placement are persisted, or nothing is. Passing them together is what
/// lets the storage layer honour that in a single transaction.
#[derive(Debug, Clone)]
pub struct NewMessage {
    /// The authoritative bytes.
    pub raw: RawMessage,
    /// The message envelope.
    pub envelope: crate::Envelope,
    /// How it arrived.
    pub source: crate::DeliverySource,
    /// Derived structure.
    pub parsed: crate::ParsedMessage,
    /// When it was received.
    pub received_at: Timestamp,
    /// The mailboxes to place it in, with the flags each placement starts with.
    pub placements: Vec<NewPlacement>,
}

/// One requested placement of a message into a mailbox.
///
/// The UID is deliberately absent: it is assigned by the storage layer inside
/// the same transaction that inserts the entry, because that serialisation is
/// the one place a race would corrupt client state (`CORE_SPEC.md` §11).
#[derive(Debug, Clone)]
pub struct NewPlacement {
    /// The target mailbox.
    pub mailbox_id: MailboxId,
    /// Flags this placement starts with.
    pub flags: MessageFlags,
    /// IMAP `INTERNALDATE`.
    pub internal_date: Timestamp,
}

/// Accounts and the addresses that route to them.
#[async_trait]
pub trait AccountStore: Send + Sync {
    /// Creates an account together with its default mailboxes.
    async fn create(&self, account: &Account) -> CoreResult<()>;

    /// Fetches an account by id.
    async fn get(&self, id: AccountId) -> CoreResult<Option<Account>>;

    /// Fetches an account by username.
    async fn get_by_username(&self, username: &str) -> CoreResult<Option<Account>>;

    /// Lists every account.
    async fn list(&self) -> CoreResult<Vec<Account>>;

    /// Deletes an account and everything it owns.
    async fn delete(&self, id: AccountId) -> CoreResult<bool>;

    /// Adds a delivery address to an account.
    async fn add_identity(
        &self,
        account_id: AccountId,
        address: &EmailAddress,
        is_primary: bool,
    ) -> CoreResult<()>;

    /// Removes a delivery address from an account.
    async fn remove_identity(
        &self,
        account_id: AccountId,
        address: &EmailAddress,
    ) -> CoreResult<bool>;

    /// Lists an account's addresses.
    async fn identities(&self, account_id: AccountId) -> CoreResult<Vec<crate::EmailIdentity>>;

    /// Resolves a recipient address to the account that owns it.
    ///
    /// The hot path of every `RCPT TO`.
    async fn resolve_address(&self, address: &EmailAddress) -> CoreResult<Option<AccountId>>;

    /// The catch-all account, when one is configured.
    async fn catch_all(&self) -> CoreResult<Option<Account>>;

    /// Updates an account's credential (password hash).
    async fn update_credential(
        &self,
        id: AccountId,
        credential: Option<&crate::Credential>,
    ) -> CoreResult<bool>;

    /// Updates whether an account is the catch-all account.
    async fn set_catch_all(&self, id: AccountId, is_catch_all: bool) -> CoreResult<bool>;
}

/// Mailboxes and their UID spaces.
#[async_trait]
pub trait MailboxStore: Send + Sync {
    /// Creates a mailbox, and any missing parents.
    async fn create(&self, mailbox: &Mailbox) -> CoreResult<()>;

    /// Fetches a mailbox by id.
    async fn get(&self, id: MailboxId) -> CoreResult<Option<Mailbox>>;

    /// Fetches a mailbox by account and name.
    async fn get_by_name(
        &self,
        account_id: AccountId,
        name: &MailboxName,
    ) -> CoreResult<Option<Mailbox>>;

    /// Lists an account's mailboxes.
    async fn list(&self, account_id: AccountId) -> CoreResult<Vec<Mailbox>>;

    /// Renames a mailbox and its descendants.
    async fn rename(&self, id: MailboxId, new_name: &MailboxName) -> CoreResult<()>;

    /// Sets a mailbox's subscription state.
    async fn set_subscribed(&self, id: MailboxId, subscribed: bool) -> CoreResult<()>;

    /// Deletes a mailbox and its placements.
    async fn delete(&self, id: MailboxId) -> CoreResult<bool>;

    /// Counts total and unseen placements — IMAP `STATUS`.
    async fn counts(&self, id: MailboxId) -> CoreResult<MailboxCounts>;
}

/// Summary counts for a mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxCounts {
    /// Total placements.
    pub total: u64,
    /// Placements without `\Seen`.
    pub unseen: u64,
    /// The next UID that will be assigned.
    pub uid_next: u32,
    /// The highest modification sequence issued in this mailbox.
    ///
    /// IMAP `STATUS ... (HIGHESTMODSEQ)` (RFC 7162 §3.1.2). Reported from the
    /// mailbox's own high-water mark rather than derived from the entries,
    /// because a mailbox whose only change was an `EXPUNGE` has a
    /// `HIGHESTMODSEQ` above every surviving entry's value.
    pub highest_mod_seq: crate::ModSeq,
}

impl Default for MailboxCounts {
    /// An empty mailbox: no messages, first UID unissued, sequence at its
    /// floor.
    ///
    /// Hand-written rather than derived because `ModSeq` has no zero — RFC
    /// 7162 §3 reserves 0 — so the derived default would not compile.
    fn default() -> Self {
        Self {
            total: 0,
            unseen: 0,
            uid_next: 1,
            highest_mod_seq: crate::ModSeq::MIN,
        }
    }
}

/// Messages, their raw bytes, and their placements.
#[async_trait]
pub trait MessageStore: Send + Sync {
    /// Persists a message and all of its placements atomically.
    ///
    /// Assigns each placement a UID from its mailbox's sequence within the same
    /// transaction. Returns the message id and the placements created.
    async fn insert(&self, message: &NewMessage) -> CoreResult<InsertedMessage>;

    /// Fetches a message with its derived structure.
    async fn get(&self, id: MessageId) -> CoreResult<Option<Message>>;

    /// Fetches only the authoritative raw bytes.
    ///
    /// Separate from [`get`](MessageStore::get) because IMAP `FETCH BODY[]` and
    /// POP3 `RETR` want the octets without the cost of rebuilding parsed
    /// structure.
    async fn raw(&self, id: MessageId) -> CoreResult<Option<RawMessage>>;

    /// Fetches one placement.
    async fn get_entry(&self, id: EntryId) -> CoreResult<Option<MessageEntry>>;

    /// Fetches a placement by mailbox and UID — the IMAP access pattern.
    async fn get_entry_by_uid(
        &self,
        mailbox_id: MailboxId,
        uid: Uid,
    ) -> CoreResult<Option<MessageEntry>>;

    /// Lists a mailbox's placements, ordered by UID.
    async fn list_entries(&self, mailbox_id: MailboxId) -> CoreResult<Vec<MessageEntry>>;

    /// Lists every placement of a message, across all mailboxes.
    async fn entries_for_message(&self, message_id: MessageId) -> CoreResult<Vec<MessageEntry>>;

    /// Places an existing message into another mailbox — IMAP `COPY`.
    ///
    /// Creates a new placement with a **new UID** and its own flags. The message
    /// is shared, never duplicated.
    async fn copy_entry(
        &self,
        entry_id: EntryId,
        target_mailbox: MailboxId,
    ) -> CoreResult<MessageEntry>;

    /// Moves an existing placement to another mailbox — IMAP `MOVE`.
    ///
    /// Atomically removes the source placement and creates a new placement in the
    /// target mailbox with a **new UID**, preserving flags and internal date.
    async fn move_entry(
        &self,
        entry_id: EntryId,
        target_mailbox: MailboxId,
    ) -> CoreResult<MessageEntry>;

    /// Updates a placement's flags — IMAP `STORE`.
    ///
    /// Affects only the named placement; other placements of the same message
    /// are untouched.
    async fn update_flags(&self, entry_id: EntryId, update: FlagUpdate)
    -> CoreResult<MessageFlags>;

    /// Updates a placement's flags, reporting the resulting modification
    /// sequence — IMAP `STORE` under CONDSTORE (RFC 7162 §3.2).
    ///
    /// `unchanged_since` implements `UNCHANGEDSINCE`: when `Some(n)`, the
    /// mutation is applied **only** if the entry's current modification
    /// sequence is `<= n`. A greater value means someone else changed the
    /// entry since the client last looked, and the store is refused rather
    /// than applied — which is the entire point of the extension, so the check
    /// and the write must happen in one transaction.
    ///
    /// Distinct from [`update_flags`](MessageStore::update_flags) rather than
    /// replacing it: the unconditional path has many callers that neither pass
    /// a precondition nor read a modseq, and widening their signature would
    /// gain nothing. This method is the one `STORE` uses.
    async fn update_flags_conditional(
        &self,
        entry_id: EntryId,
        update: FlagUpdate,
        unchanged_since: Option<crate::ModSeq>,
    ) -> CoreResult<FlagOutcome>;

    /// Removes a placement. The message survives if other placements remain.
    async fn delete_entry(&self, entry_id: EntryId) -> CoreResult<bool>;

    /// Removes messages that no placement references.
    ///
    /// Returns the ids collected.
    async fn collect_orphans(&self) -> CoreResult<Vec<MessageId>>;

    /// Lists a message's attachment metadata.
    async fn attachments(&self, message_id: MessageId) -> CoreResult<Vec<Attachment>>;

    /// Fetches an attachment by its id, returning the attachment metadata and its owning message id.
    async fn get_attachment(&self, id: AttachmentId)
    -> CoreResult<Option<(Attachment, MessageId)>>;
}

/// The result of inserting a message.
#[derive(Debug, Clone)]
pub struct InsertedMessage {
    /// The message that was stored.
    pub message_id: MessageId,
    /// The placements created, each with its assigned UID.
    pub entries: Vec<MessageEntry>,
}

/// The result of a conditional flag update.
///
/// Carries the three facts a CONDSTORE `STORE` response needs and keeps them
/// consistent with each other: what the flags are now, the sequence they are
/// stamped at, and whether the write actually happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagOutcome {
    /// The placement's flags after the operation.
    ///
    /// On a refused or no-op update these are the flags as they already stood,
    /// so a caller can report the truth without a second read.
    pub flags: MessageFlags,
    /// The modification sequence the entry now carries.
    ///
    /// A newly allocated value when the flags changed; the entry's existing
    /// value when nothing changed or the precondition failed.
    pub mod_seq: crate::ModSeq,
    /// What the store did.
    pub status: FlagChangeStatus,
}

/// Whether a conditional flag update applied, was a no-op, or was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagChangeStatus {
    /// The flags changed and a new modification sequence was allocated.
    Changed,
    /// The requested flags were already in effect.
    ///
    /// Deliberately distinct from [`Self::Changed`]: RFC 7162 §3.1.11 warns
    /// that advancing the sequence for a no-op forces every other client to
    /// re-fetch a message that did not change. No sequence is allocated.
    Unchanged,
    /// `UNCHANGEDSINCE` was not satisfied; nothing was written.
    ///
    /// The entry's modification sequence exceeded the client's value, so the
    /// client's view is stale. `STORE` reports these UIDs in a `MODIFIED`
    /// response code (RFC 7162 §3.2.5).
    Failed,
}

/// How to change a placement's flags.
#[derive(Debug, Clone)]
pub enum FlagUpdate {
    /// `STORE +FLAGS`.
    Add {
        /// System flags to set.
        system: SystemFlags,
        /// Keywords to add.
        keywords: BTreeSet<crate::Keyword>,
    },
    /// `STORE -FLAGS`.
    Remove {
        /// System flags to clear.
        system: SystemFlags,
        /// Keywords to remove.
        keywords: BTreeSet<crate::Keyword>,
    },
    /// `STORE FLAGS` — replaces the entire set.
    Replace {
        /// The resulting system flags.
        system: SystemFlags,
        /// The resulting keywords.
        keywords: BTreeSet<crate::Keyword>,
    },
}

/// Delivery records, for the Diagnostics view.
#[async_trait]
pub trait DeliveryStore: Send + Sync {
    /// Records a delivery.
    async fn record(&self, delivery: &Delivery) -> CoreResult<()>;

    /// Fetches a delivery record.
    async fn get(&self, id: crate::DeliveryId) -> CoreResult<Option<Delivery>>;

    /// Fetches the delivery that produced a message.
    ///
    /// Distinct from scanning [`recent`](Self::recent) for a matching id: a
    /// message older than the scan window has a delivery record that a scan
    /// cannot reach, and reporting "no delivery" for a message that plainly
    /// has one is worse than reporting nothing at all.
    ///
    /// A message with no delivery record is `Ok(None)`, not an error: a message
    /// injected through the API before delivery recording existed is a real
    /// state, not a corruption.
    async fn by_message_id(&self, message_id: MessageId) -> CoreResult<Option<Delivery>>;

    /// Lists recent deliveries, newest first.
    ///
    /// Includes deliveries that stored no message — a transaction rejected
    /// outright has `message_id: None` and is reachable only here.
    async fn recent(&self, limit: u32) -> CoreResult<Vec<Delivery>>;
}

/// Connection records and their protocol transcripts (ADR-066).
///
/// Separate from [`DeliveryStore`] because a connection is not a delivery: one
/// connection can produce many deliveries or none at all, and IMAP and POP3
/// produce none ever.
#[async_trait]
pub trait ConnectionStore: Send + Sync {
    /// Records a finished connection and its transcript.
    async fn record(&self, connection: &ConnectionRecord) -> CoreResult<()>;

    /// Fetches one connection record.
    async fn get(&self, id: crate::ConnectionId) -> CoreResult<Option<ConnectionRecord>>;

    /// Lists recent connections, newest first, optionally for one protocol.
    async fn recent(
        &self,
        protocol: Option<crate::Protocol>,
        limit: u32,
    ) -> CoreResult<Vec<ConnectionRecord>>;

    /// Evicts the oldest records beyond `keep`, returning how many were removed.
    ///
    /// Traces are unbounded growth that nothing else caps: `STORAGE_DESIGN.md`
    /// §5 caps messages at 5000 by default precisely so a runaway test suite
    /// cannot fill a developer's disk, and transcripts are produced more often
    /// than messages.
    async fn prune(&self, keep: u32) -> CoreResult<u64>;
}

/// Bulk storage for content too large to sit inline in a row.
///
/// Content-addressed: writing the same bytes twice is idempotent and costs one
/// object (`STORAGE_DESIGN.md` §4).
#[async_trait]
pub trait BlobStore: Send + Sync + std::fmt::Debug {
    /// Stores bytes, returning the reference to retrieve them by.
    async fn put(&self, bytes: &[u8]) -> CoreResult<String>;

    /// Retrieves bytes by reference.
    async fn get(&self, reference: &str) -> CoreResult<Option<Vec<u8>>>;

    /// Deletes an object. Returns whether it existed.
    async fn delete(&self, reference: &str) -> CoreResult<bool>;
}

/// What a [`SearchIndex`] could determine about a query, without ever being
/// allowed to be wrong.
///
/// The index is a **prefilter, not an oracle**. `SearchQuery::matches` stays the
/// authority on whether a message matches, because RFC 9051 §6.4.4 defines the
/// text keys as *substring* matches and FTS5 tokenises into words: an index can
/// prove a message is worth loading, but it cannot prove one matches.
///
/// So this type has exactly two shapes, and neither permits a false negative:
///
/// - [`Self::Unsupported`] — the index cannot narrow this query. The caller
///   scans, exactly as it did before the index existed.
/// - [`Self::Candidates`] — a **superset** of the matching messages. Anything
///   outside it provably cannot match; anything inside must still be confirmed
///   by `matches`.
///
/// Making "I don't know" a variant rather than an empty result is the whole
/// safety argument. An index that returned `Vec<MessageId>` would express
/// "nothing matched" and "I could not tell" identically, and the second silently
/// becomes the first — a search that quietly stops finding mail. That failure is
/// invisible in exactly the way the audit behind `ROADMAP.md` §Outstanding kept
/// finding: the tests still pass, because a search returning nothing is a valid
/// search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchCandidates {
    /// The index cannot narrow this query; the caller must scan.
    Unsupported,
    /// A superset of the messages that may match. Never a false negative.
    Candidates(BTreeSet<MessageId>),
}

impl SearchCandidates {
    /// Whether `id` still needs to be considered.
    ///
    /// `Unsupported` admits everything, which is what makes an unusable index
    /// degrade to the previous behaviour rather than to an empty result.
    #[must_use]
    pub fn admits(&self, id: MessageId) -> bool {
        match self {
            Self::Unsupported => true,
            Self::Candidates(ids) => ids.contains(&id),
        }
    }

    /// The narrowed set, or `None` when the index could not help.
    #[must_use]
    pub const fn narrowed(&self) -> Option<&BTreeSet<MessageId>> {
        match self {
            Self::Unsupported => None,
            Self::Candidates(ids) => Some(ids),
        }
    }
}

/// A full-text index over message content (`STORAGE_DESIGN.md` §7).
///
/// A **scaling optimisation with no semantics of its own.** Removing every
/// implementation must leave search working and correct, only slower — which is
/// why [`SearchCandidates::Unsupported`] exists and why `SearchQuery::matches`
/// still decides every result. `NullSearchIndex` is exactly that removal, and it
/// is what the in-memory test doubles use.
///
/// The trait is the one component a `PostgreSQL` backend would genuinely
/// rewrite: `tsvector` and FTS5 share no syntax.
#[async_trait]
pub trait SearchIndex: Send + Sync {
    /// Indexes a message's text. Idempotent: re-indexing replaces.
    async fn index(&self, message: &Message) -> CoreResult<()>;

    /// Removes a message from the index. Removing an absent message is not an
    /// error — the index is derived data, and a caller repairing it should not
    /// have to check first.
    async fn remove(&self, id: MessageId) -> CoreResult<()>;

    /// Narrows `query` to a superset of the messages that may match.
    ///
    /// Returning [`SearchCandidates::Unsupported`] is always correct and is the
    /// required answer whenever the index cannot guarantee a superset.
    async fn candidates(&self, query: &crate::SearchQuery) -> CoreResult<SearchCandidates>;
}

/// A [`SearchIndex`] that indexes nothing and narrows nothing.
///
/// Not a stub awaiting an implementation: it is the proof that the index is
/// optional. Every search must return identical results with this in place, and
/// the test suite asserts that against the real index.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSearchIndex;

#[async_trait]
impl SearchIndex for NullSearchIndex {
    async fn index(&self, _message: &Message) -> CoreResult<()> {
        Ok(())
    }

    async fn remove(&self, _id: MessageId) -> CoreResult<()> {
        Ok(())
    }

    async fn candidates(&self, _query: &crate::SearchQuery) -> CoreResult<SearchCandidates> {
        Ok(SearchCandidates::Unsupported)
    }
}

/// Verifies a password against an account's stored credential.
///
/// A port for one reason: `tests/architecture.rs` forbids `sandesh-api` from
/// depending on `sandesh-proto`, where the Argon2id verification lives (§59 —
/// the API and the mail protocols are siblings and must not see each other).
/// The API needs that exact verification for HTTP basic auth, so the capability
/// is declared here and supplied by `sandesh-app`, the one crate that may see
/// both.
///
/// The alternative — reimplementing verification inside `sandesh-api` — would
/// have put a second password-checking path in the codebase, which is precisely
/// how one of them ends up missing the timing-safety property the other has.
#[async_trait]
pub trait CredentialVerifier: Send + Sync {
    /// Whether `password` authenticates `username`.
    ///
    /// Implementations **must** take the same time whether or not the account
    /// exists: a fast "no such user" is an account-enumeration oracle. The
    /// existing `sandesh_proto::verify_optional` already owns that property
    /// (ADR-032), which is the other reason this is a port rather than a
    /// reimplementation.
    async fn verify(&self, username: &str, password: &str) -> bool;

    /// Hashes a password for storage, in PHC format.
    ///
    /// Here rather than in a separate port because hashing and verification
    /// must agree on algorithm and parameters, and splitting them across two
    /// traits is how they drift apart — which is not hypothetical: the API
    /// carried its own Argon2 call with the parameters copied by hand, so
    /// retuning the canonical ones would have left account creation behind
    /// (ADR-075).
    ///
    /// # Errors
    /// Returns a message suitable for logging when hashing fails, which in
    /// practice means the platform has no entropy source.
    fn hash(&self, password: &str) -> Result<String, String>;
}

/// A point-in-time reading of the counters and gauges `/metrics` reports
/// (Phase 7 A4).
///
/// One port rather than aggregate methods spread across [`MessageStore`],
/// [`MailboxStore`], [`DeliveryStore`] and [`ConnectionStore`]. Those four
/// traits describe how a *single* entity is read and written; a scrape asks a
/// different question — how many of everything there are — and answering it
/// through them would mean either a count method on each (four traits, and
/// every test double that implements them, changed to serve one endpoint) or
/// the API iterating mailboxes to sum them, which makes a scrape O(mailboxes)
/// and repeats the linear scan A1 removed from the delivery lookup.
///
/// Gathering it in one call also makes the reading *coherent*: four separate
/// queries can interleave with a delivery in progress and report a message
/// count that never existed at any instant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// Messages stored, across every account.
    pub messages: u64,
    /// Mailboxes that exist, across every account.
    pub mailboxes: u64,
    /// Placements of a message into a mailbox.
    pub entries: u64,
    /// Accounts that exist.
    pub accounts: u64,
    /// Delivery records grouped by how the mail arrived and how it ended.
    ///
    /// A rejected transaction stored no message, so it appears here and in no
    /// message count — which is the point of recording it (A1).
    pub deliveries: Vec<DeliveryTally>,
    /// Connection records grouped by protocol.
    pub connections: Vec<ProtocolTally>,
    /// Delivery durations, for the scrape's histogram.
    pub delivery_durations: DurationHistogram,
}

/// Deliveries counted by source and outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryTally {
    /// How the mail arrived.
    pub source: crate::DeliverySource,
    /// How it ended for the recipients counted here.
    pub outcome: crate::DeliveryOutcome,
    /// How many.
    pub count: u64,
}

/// Connections and their commands, counted per protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolTally {
    /// The protocol.
    pub protocol: crate::Protocol,
    /// Connections accepted.
    pub connections: u64,
    /// Commands exchanged across those connections.
    ///
    /// Read from the recorded transcripts rather than counted live in the
    /// protocol crates: the API cannot depend on `sandesh-proto`
    /// (`tests/architecture.rs`), and a counter living in three protocol
    /// crates is three counters that drift.
    pub commands: u64,
}

/// Delivery durations as cumulative bucket counts, in microseconds.
///
/// Prometheus histogram semantics: each bucket holds every observation less
/// than or equal to its bound, so the counts are non-decreasing and the final
/// bucket equals the total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DurationHistogram {
    /// `(upper bound in microseconds, cumulative count)`, ascending.
    pub buckets: Vec<(u64, u64)>,
    /// Every observation.
    pub count: u64,
    /// The sum of all observations, in microseconds.
    pub sum_us: u64,
}

/// The bucket bounds, in microseconds: 1ms to ~16s, doubling.
///
/// A local delivery completes in well under a millisecond (the IMAP `EXISTS`
/// latency measured in Phase 5 was ~640µs), so the interesting resolution is
/// at the fast end; the upper buckets exist to make a pathological delivery
/// visible rather than to measure it precisely.
pub const DELIVERY_DURATION_BUCKETS_US: &[u64] = &[
    1_000, 2_000, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 2_500_000,
    5_000_000, 10_000_000,
];

impl DurationHistogram {
    /// Builds a histogram over [`DELIVERY_DURATION_BUCKETS_US`].
    ///
    /// Negative durations are ignored rather than clamped to zero: a negative
    /// elapsed time means the clock moved, and folding it into the fastest
    /// bucket would report a delivery that was never observed.
    #[must_use]
    pub fn from_micros(observations: impl IntoIterator<Item = i64>) -> Self {
        let mut counts = vec![0_u64; DELIVERY_DURATION_BUCKETS_US.len()];
        let mut count = 0_u64;
        let mut sum_us = 0_u64;

        for observation in observations {
            let Ok(value) = u64::try_from(observation) else {
                continue;
            };
            count += 1;
            sum_us += value;
            for (index, bound) in DELIVERY_DURATION_BUCKETS_US.iter().enumerate() {
                if value <= *bound {
                    counts[index] += 1;
                }
            }
        }

        Self {
            buckets: DELIVERY_DURATION_BUCKETS_US
                .iter()
                .copied()
                .zip(counts)
                .collect(),
            count,
            sum_us,
        }
    }
}

/// Supplies the readings behind `/metrics`.
#[async_trait]
pub trait MetricsSource: Send + Sync {
    /// Gathers one coherent snapshot.
    async fn snapshot(&self) -> CoreResult<MetricsSnapshot>;
}

/// Webhook endpoints and the durable outbox that feeds them (ADR-085, ADR-086).
///
/// The outbox is a store rather than a queue because the acceptance criterion
/// names restart survival: the worker keeps no state between ticks, so
/// everything needed to resume a pending delivery has to be readable back from
/// here. That is also what makes a restart indistinguishable from a slow tick.
///
/// Nothing in this trait mentions HTTP. The core defines *what* is owed to whom
/// and when to try again; `sandesh-webhook` owns the client that actually dials
/// (ADR-085), and `tests/architecture.rs` enforces that the two stay apart.
#[async_trait]
pub trait OutboxStore: Send + Sync + std::fmt::Debug {
    /// Registers a destination.
    async fn register_endpoint(&self, endpoint: &crate::WebhookEndpoint) -> CoreResult<()>;

    /// Lists every registered endpoint, newest first.
    async fn endpoints(&self) -> CoreResult<Vec<crate::WebhookEndpoint>>;

    /// Removes an endpoint and everything queued for it.
    ///
    /// Cascading is deliberate: a pending entry for a destination that no
    /// longer exists is undeliverable by construction, and leaving it to retry
    /// its way to terminal failure would fill the outbox view with noise about
    /// a decision the developer already made.
    async fn remove_endpoint(&self, id: crate::EndpointId) -> CoreResult<bool>;

    /// Claims the entries whose next attempt is due, oldest first.
    ///
    /// The worker's only read. `limit` bounds a tick so a large backlog is
    /// drained across several passes rather than loaded at once.
    async fn due(&self, now: Timestamp, limit: u32) -> CoreResult<Vec<crate::OutboxEntry>>;

    /// Records the outcome of an attempt.
    ///
    /// One method for success and failure alike, because they are the same
    /// write with different arguments, and splitting them invites a caller to
    /// update the status without the attempt count.
    async fn record_attempt(
        &self,
        id: crate::OutboxId,
        status: crate::OutboxStatus,
        next_attempt_at: Timestamp,
        error: Option<&str>,
    ) -> CoreResult<()>;

    /// Lists recent entries, newest first, for the outbox view.
    async fn recent(&self, limit: u32) -> CoreResult<Vec<crate::OutboxEntry>>;
}

/// Auto-tagging rules (ADR-087).
///
/// Persisted, unlike the fault registry: a tagging rule is configuration a
/// developer curates over a session, not a hazard that restart must clear. The
/// worst a rule does is put a keyword on a message, which is visible,
/// reversible through `STORE`, and explained by the delivery record.
#[async_trait]
pub trait RuleStore: Send + Sync + std::fmt::Debug {
    /// Stores a rule, replacing one with the same id.
    async fn save(&self, rule: &crate::Rule) -> CoreResult<()>;

    /// Every configured rule, in evaluation order.
    ///
    /// Read once per delivery. Ordering is stable so a delivery record's list
    /// of applied rules can be compared against the configured set.
    async fn all(&self) -> CoreResult<Vec<crate::Rule>>;

    /// Removes a rule, reporting whether one was there.
    ///
    /// Deliveries that already recorded it keep their record: the match stored
    /// the rule's name, not a reference to it, so history survives the rule.
    async fn remove(&self, id: crate::RuleId) -> CoreResult<bool>;
}

/// Workspaces and isolated project database management.
#[async_trait]
pub trait WorkspaceStore: Send + Sync {
    /// Lists all workspaces.
    async fn list(&self) -> CoreResult<Vec<crate::workspace::Workspace>>;

    /// Fetches a workspace by id/slug.
    async fn get(&self, id: &str) -> CoreResult<Option<crate::workspace::Workspace>>;

    /// Returns the currently active workspace identifier.
    ///
    /// Cheaper than `list()` when only the active ID is needed — a single
    /// registry read rather than a full enumeration with disk-size stats.
    async fn active_id(&self) -> CoreResult<String>;

    /// Creates a new workspace with given id, name, optional description, optional source workspace to clone accounts/routing from,
    /// optional initial account setup when creating from scratch, and whether authentication is required.
    async fn create(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        clone_from: Option<&str>,
        initial_account: Option<crate::workspace::InitialAccountSetup>,
        auth_required: bool,
    ) -> CoreResult<crate::workspace::Workspace>;

    /// Updates an existing workspace's settings (name, description, or auth requirement).
    async fn update(
        &self,
        id: &str,
        update: crate::workspace::WorkspaceUpdate,
    ) -> CoreResult<crate::workspace::Workspace>;

    /// Sets the active workspace.
    async fn set_active(&self, id: &str) -> CoreResult<()>;

    /// Deletes a workspace and drops its database from disk.
    async fn delete(&self, id: &str) -> CoreResult<bool>;

    /// Purges mail, traces, or resets data for a workspace.
    async fn purge(
        &self,
        id: &str,
        options: crate::workspace::PurgeOptions,
    ) -> CoreResult<crate::workspace::PurgeResult>;

    /// Flushes uncheckpointed WAL frames into the database file for a workspace.
    async fn checkpoint(&self, id: &str) -> CoreResult<()>;
}
