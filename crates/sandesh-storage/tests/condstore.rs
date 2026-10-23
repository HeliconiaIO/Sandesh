//! CONDSTORE storage invariants (RFC 7162 §3).
//!
//! The whole extension rests on one property: a modification sequence is
//! monotonic **within a mailbox** and never re-issued. Everything a client does
//! with CONDSTORE — `CHANGEDSINCE`, `UNCHANGEDSINCE`, `HIGHESTMODSEQ` — is a
//! comparison against that number, so a duplicate or a rollback does not
//! degrade the feature, it silently desynchronises the client. These tests
//! guard the allocator and the conditional store rather than the wire syntax,
//! which `sandesh-imap` covers.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{FlagChangeStatus, Keyword, MailboxStore, MessageStore, ModSeq, SystemFlags};
use std::collections::BTreeSet;

/// `+FLAGS (\Seen)`, the most common real mutation.
fn add_seen() -> sandesh_core::FlagUpdate {
    sandesh_core::FlagUpdate::Add {
        system: SystemFlags::SEEN,
        keywords: BTreeSet::new(),
    }
}

/// How many flag-change events the harness has observed.
fn flag_change_events(h: &Harness) -> usize {
    h.sink
        .kinds()
        .iter()
        .filter(|kind| **kind == sandesh_core::EventKind::EntryFlagsChanged)
        .count()
}

// ------------------------------------------------------------------ migration

#[tokio::test]
async fn migration_adds_the_mailbox_high_water_mark() {
    // Migration 0002. `highest_mod_seq` must exist and be queryable, because
    // every allocation is an UPDATE against it; a missing column would fail at
    // the first flag change rather than at startup.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let (value,): (i64,) = sqlx::query_as("SELECT highest_mod_seq FROM mailboxes WHERE id = ?")
        .bind(&inbox.as_bytes()[..])
        .fetch_one(h.db.pool())
        .await
        .expect("column exists");

    assert_eq!(
        value, 1,
        "a mailbox with no changes starts at the sequence floor"
    );
}

#[tokio::test]
async fn migration_creates_the_modseq_lookup_index() {
    // `SEARCH MODSEQ`, `CHANGEDSINCE` and `UNCHANGEDSINCE` all filter by
    // (mailbox, mod_seq). Without the composite index each becomes a scan of
    // the mailbox, which is a performance cliff rather than a wrong answer —
    // asserted here so the index cannot be dropped silently.
    let h = Harness::new().await;

    let found: Option<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_entry_mod_seq'",
    )
    .fetch_optional(h.db.pool())
    .await
    .expect("query indexes");

    assert!(
        found.is_some(),
        "the (mailbox_id, mod_seq) index must exist"
    );
}

#[tokio::test]
async fn the_high_water_mark_is_never_below_any_entry_it_contains() {
    // The data-preservation guarantee of 0002, restated as an invariant that
    // must hold at all times rather than only just after migrating: if an
    // entry could sit above the mailbox's mark, the allocator would re-issue a
    // value that a client has already seen.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    for _ in 0..3 {
        let _ = h
            .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
            .await;
    }

    let (mark, highest_entry): (i64, Option<i64>) = sqlx::query_as(
        "SELECT
            (SELECT highest_mod_seq FROM mailboxes WHERE id = ?),
            (SELECT MAX(mod_seq) FROM message_entries WHERE mailbox_id = ?)",
    )
    .bind(&inbox.as_bytes()[..])
    .bind(&inbox.as_bytes()[..])
    .fetch_one(h.db.pool())
    .await
    .expect("query");

    assert!(
        mark >= highest_entry.expect("entries exist"),
        "mailbox mark {mark} must cover every entry it holds"
    );
}

// --------------------------------------------------------------- monotonicity

#[tokio::test]
async fn allocation_is_strictly_monotonic_within_a_mailbox() {
    // The core property. Each delivery is a change to the mailbox, so each
    // must take a strictly greater sequence than the last.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let mut seen: Vec<u64> = Vec::new();
    for _ in 0..5 {
        let inserted = h
            .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
            .await;
        seen.push(inserted.entries[0].mod_seq.get());
    }

    for pair in seen.windows(2) {
        assert!(
            pair[1] > pair[0],
            "sequence must strictly increase, got {seen:?}"
        );
    }
    let unique: BTreeSet<u64> = seen.iter().copied().collect();
    assert_eq!(unique.len(), seen.len(), "no value may be issued twice");
}

#[tokio::test]
async fn each_mailbox_has_its_own_independent_sequence() {
    // RFC 7162 §3 scopes the sequence to the mailbox. A global counter would
    // still be monotonic, but it would make one mailbox's HIGHESTMODSEQ jump
    // because an unrelated mailbox changed, forcing needless client resyncs.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let sent = h.mailbox(account, "Sent").await;

    for _ in 0..3 {
        let _ = h
            .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
            .await;
    }
    let into_sent = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[sent])
        .await;

    assert_eq!(
        into_sent.entries[0].mod_seq.get(),
        2,
        "Sent's own sequence must not be advanced by activity in INBOX"
    );
}

// ------------------------------------------------------------------- insertion

#[tokio::test]
async fn an_inserted_entry_is_stamped_above_the_mailbox_floor() {
    // The blocker this increment removes: inserts used to hardcode 1. A client
    // synced to HIGHESTMODSEQ 1 must see a new arrival as strictly greater, or
    // it will never fetch it.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;

    assert!(
        inserted.entries[0].mod_seq > ModSeq::MIN,
        "a new placement must be visible to a client synced at the floor"
    );
}

#[tokio::test]
async fn a_multi_mailbox_delivery_stamps_each_placement_from_its_own_mailbox() {
    // One message, two placements, two independent sequences. Sharing a value
    // across mailboxes would make a client's per-mailbox comparison wrong.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let sent = h.mailbox(account, "Sent").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox, sent])
        .await;

    assert_eq!(inserted.entries.len(), 2);
    for entry in &inserted.entries {
        let counts = h.mailboxes.counts(entry.mailbox_id).await.expect("counts");
        assert_eq!(
            counts.highest_mod_seq, entry.mod_seq,
            "each placement must match its own mailbox's mark"
        );
    }
}

#[tokio::test]
async fn copy_and_move_stamp_the_target_mailbox_sequence() {
    // COPY and MOVE create a placement, which is a change to the *target*
    // mailbox and must be reported to clients watching it.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let archive = h.mailbox(account, "Archive").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let before = h
        .mailboxes
        .counts(archive)
        .await
        .expect("counts")
        .highest_mod_seq;

    let copied = h
        .messages
        .copy_entry(inserted.entries[0].id, archive)
        .await
        .expect("copy");
    assert!(
        copied.mod_seq > before,
        "COPY must advance the target mailbox's sequence"
    );

    let second = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let moved = h
        .messages
        .move_entry(second.entries[0].id, archive)
        .await
        .expect("move");
    assert!(
        moved.mod_seq > copied.mod_seq,
        "MOVE must advance it again, not reuse the COPY value"
    );
}

// ---------------------------------------------------------------- flag change

#[tokio::test]
async fn a_successful_flag_change_allocates_a_new_sequence() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];

    let outcome = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), None)
        .await
        .expect("store");

    assert_eq!(outcome.status, FlagChangeStatus::Changed);
    assert!(
        outcome.mod_seq > entry.mod_seq,
        "a flag change must take a fresh sequence"
    );
    assert!(
        outcome.flags.has(SystemFlags::SEEN),
        "the reported flags must include the change"
    );
}

#[tokio::test]
async fn the_stored_entry_matches_the_reported_outcome() {
    // Internal consistency: what the caller is told and what the row holds must
    // agree, or an untagged FETCH would report a modseq the database disagrees
    // with.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];

    let outcome = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), None)
        .await
        .expect("store");

    let reloaded = h
        .messages
        .get_entry(entry.id)
        .await
        .expect("get")
        .expect("entry exists");

    assert_eq!(reloaded.mod_seq, outcome.mod_seq);
    assert_eq!(reloaded.flags, outcome.flags);

    let counts = h.mailboxes.counts(inbox).await.expect("counts");
    assert_eq!(
        counts.highest_mod_seq, outcome.mod_seq,
        "the mailbox mark must cover the change just made"
    );
}

#[tokio::test]
async fn a_keyword_only_change_also_allocates_a_sequence() {
    // Keywords live in a side table, so a modseq stamp driven only by the
    // system-flag bitmask would miss them entirely.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];

    let mut keywords = BTreeSet::new();
    keywords.insert(Keyword::new("$Important").expect("valid keyword"));

    let outcome = h
        .messages
        .update_flags_conditional(
            entry.id,
            sandesh_core::FlagUpdate::Add {
                system: SystemFlags::NONE,
                keywords,
            },
            None,
        )
        .await
        .expect("store");

    assert_eq!(outcome.status, FlagChangeStatus::Changed);
    assert!(outcome.mod_seq > entry.mod_seq);
}

// -------------------------------------------------------------------- no-op

#[tokio::test]
async fn a_noop_flag_change_does_not_advance_the_sequence() {
    // RFC 7162 §3.1.11. Advancing the sequence for a store that changed
    // nothing would make every other client re-fetch an identical message.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];

    let first = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), None)
        .await
        .expect("first store");
    let repeat = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), None)
        .await
        .expect("repeat store");

    assert_eq!(repeat.status, FlagChangeStatus::Unchanged);
    assert_eq!(
        repeat.mod_seq, first.mod_seq,
        "setting an already-set flag must not allocate"
    );

    let counts = h.mailboxes.counts(inbox).await.expect("counts");
    assert_eq!(
        counts.highest_mod_seq, first.mod_seq,
        "the mailbox mark must not drift on a no-op"
    );
}

#[tokio::test]
async fn removing_a_flag_that_is_not_set_is_a_noop() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];

    let outcome = h
        .messages
        .update_flags_conditional(
            entry.id,
            sandesh_core::FlagUpdate::Remove {
                system: SystemFlags::SEEN,
                keywords: BTreeSet::new(),
            },
            None,
        )
        .await
        .expect("store");

    assert_eq!(outcome.status, FlagChangeStatus::Unchanged);
    assert_eq!(outcome.mod_seq, entry.mod_seq);
}

#[tokio::test]
async fn a_noop_store_emits_no_flag_change_event() {
    // The event system drives untagged FETCH updates to other sessions. An
    // event for a change that did not happen would wake every client for
    // nothing.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];

    let _ = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), None)
        .await
        .expect("first");
    let before = flag_change_events(&h);
    let _ = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), None)
        .await
        .expect("repeat");

    assert_eq!(
        flag_change_events(&h),
        before,
        "a no-op must not emit EntryFlagsChanged"
    );
}

// ------------------------------------------------------------ UNCHANGEDSINCE

#[tokio::test]
async fn unchanged_since_permits_a_store_on_an_unmodified_entry() {
    // The success path: the client's value still describes the entry.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];

    let outcome = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), Some(entry.mod_seq))
        .await
        .expect("store");

    assert_eq!(outcome.status, FlagChangeStatus::Changed);
    assert!(outcome.flags.has(SystemFlags::SEEN));
}

#[tokio::test]
async fn unchanged_since_is_inclusive_at_the_boundary() {
    // RFC 7162 §3.2: the failure condition is *greater than* the client's
    // value. Equality means the client holds exactly this revision and may
    // proceed — an off-by-one here would refuse every legitimate store.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];

    let exact = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), Some(entry.mod_seq))
        .await
        .expect("store");

    assert_eq!(
        exact.status,
        FlagChangeStatus::Changed,
        "UNCHANGEDSINCE equal to the entry's modseq must succeed"
    );
}

#[tokio::test]
async fn unchanged_since_refuses_a_store_against_a_stale_value() {
    // The failure path, and the reason the extension exists: the second client
    // must not clobber the first client's change.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];
    let stale = entry.mod_seq;

    // Someone else changes the entry first.
    let winner = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), None)
        .await
        .expect("first writer");

    let refused = h
        .messages
        .update_flags_conditional(
            entry.id,
            sandesh_core::FlagUpdate::Add {
                system: SystemFlags::FLAGGED,
                keywords: BTreeSet::new(),
            },
            Some(stale),
        )
        .await
        .expect("conditional store");

    assert_eq!(refused.status, FlagChangeStatus::Failed);
    assert_eq!(
        refused.mod_seq, winner.mod_seq,
        "the refusal must report the entry's current sequence"
    );
    assert!(
        !refused.flags.has(SystemFlags::FLAGGED),
        "the refused flag must not appear in the reported state"
    );
}

#[tokio::test]
async fn a_refused_store_writes_nothing_at_all() {
    // "Return the failure rather than applying the mutation." Verified against
    // storage, not just the returned value: a refusal that still wrote would be
    // silent corruption.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];
    let stale = entry.mod_seq;

    let winner = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), None)
        .await
        .expect("first writer");

    let _ = h
        .messages
        .update_flags_conditional(
            entry.id,
            sandesh_core::FlagUpdate::Add {
                system: SystemFlags::DELETED,
                keywords: BTreeSet::new(),
            },
            Some(stale),
        )
        .await
        .expect("conditional store");

    let reloaded = h
        .messages
        .get_entry(entry.id)
        .await
        .expect("get")
        .expect("entry");

    assert!(
        !reloaded.flags.has(SystemFlags::DELETED),
        "the refused mutation must not be persisted"
    );
    assert_eq!(
        reloaded.mod_seq, winner.mod_seq,
        "a refusal must not advance the entry's sequence"
    );

    let counts = h.mailboxes.counts(inbox).await.expect("counts");
    assert_eq!(
        counts.highest_mod_seq, winner.mod_seq,
        "a refusal must not consume a sequence from the mailbox"
    );
}

// ------------------------------------------------------------------ ordering

#[tokio::test]
async fn a_conditional_store_cannot_overwrite_a_newer_change() {
    // The lost-update scenario stated end to end: two clients read the same
    // revision, both try to store, and only the first may win.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = &inserted.entries[0];
    let shared_view = entry.mod_seq;

    let first = h
        .messages
        .update_flags_conditional(entry.id, add_seen(), Some(shared_view))
        .await
        .expect("client one");
    let second = h
        .messages
        .update_flags_conditional(
            entry.id,
            sandesh_core::FlagUpdate::Replace {
                system: SystemFlags::NONE,
                keywords: BTreeSet::new(),
            },
            Some(shared_view),
        )
        .await
        .expect("client two");

    assert_eq!(first.status, FlagChangeStatus::Changed);
    assert_eq!(
        second.status,
        FlagChangeStatus::Failed,
        "the second writer held a stale view and must be refused"
    );

    let reloaded = h
        .messages
        .get_entry(entry.id)
        .await
        .expect("get")
        .expect("entry");
    assert!(
        reloaded.flags.has(SystemFlags::SEEN),
        "the first writer's change must survive"
    );
}

// --------------------------------------------------------------- concurrency

/// A file-backed stack, so several pool connections genuinely contend.
///
/// The in-memory harness is capped at one connection (each `:memory:`
/// connection would otherwise be a separate database), which serialises
/// everything and would make a concurrency test prove nothing.
async fn contended_stack() -> (
    tempfile::TempDir,
    std::sync::Arc<sandesh_storage::SqliteMessageStore>,
    sandesh_storage::SqliteMailboxStore,
    sandesh_core::MailboxId,
) {
    use sandesh_core::{AccountStore, MailboxName};

    let dir = tempfile::tempdir().expect("tempdir");
    let db = sandesh_storage::Database::open(dir.path().join("condstore.db"))
        .await
        .expect("open");

    let events = sandesh_storage::EventRecorder::null();
    let accounts = sandesh_storage::SqliteAccountStore::new(db.pool().clone(), events.clone());
    let mailboxes = sandesh_storage::SqliteMailboxStore::new(db.pool().clone(), events.clone());
    let messages = std::sync::Arc::new(sandesh_storage::SqliteMessageStore::new(
        db.pool().clone(),
        std::sync::Arc::new(sandesh_storage::NullBlobStore),
        events,
    ));

    let account = sandesh_core::Account {
        id: sandesh_core::AccountId::new(),
        username: String::from("dev"),
        credential: None,
        is_catch_all: false,
        created_at: sandesh_core::Timestamp::now(),
    };
    accounts.create(&account).await.expect("create account");
    let inbox = mailboxes
        .get_by_name(account.id, &MailboxName::new("INBOX").expect("name"))
        .await
        .expect("query")
        .expect("inbox")
        .id;

    (dir, messages, mailboxes, inbox)
}

/// Delivers one message into `inbox`, returning its single placement.
async fn deliver_one(
    messages: &sandesh_storage::SqliteMessageStore,
    inbox: sandesh_core::MailboxId,
    subject: &str,
) -> sandesh_core::MessageEntry {
    use sandesh_core::{
        DeliverySource, Envelope, MessageFlags, NewMessage, NewPlacement, RawMessage, Timestamp,
    };

    let raw = RawMessage::new(format!("Subject: {subject}\r\n\r\nbody\r\n").into_bytes());
    let parsed = sandesh_core::parse(raw.bytes());
    let now = Timestamp::now();
    let inserted = messages
        .insert(&NewMessage {
            raw,
            envelope: Envelope::default(),
            source: DeliverySource::Smtp,
            parsed,
            received_at: now,
            placements: vec![NewPlacement {
                mailbox_id: inbox,
                flags: MessageFlags::empty(),
                internal_date: now,
            }],
        })
        .await
        .expect("insert");
    inserted.entries.into_iter().next().expect("one placement")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_flag_changes_allocate_distinct_sequences() {
    // The race that would silently desynchronise clients. If two flag changes
    // can be handed the same modseq, a client asking "what changed since n"
    // gets one of them and never learns about the other.
    let (_dir, messages, mailboxes, inbox) = contended_stack().await;

    const WRITERS: usize = 24;
    let mut entries = Vec::with_capacity(WRITERS);
    for n in 0..WRITERS {
        entries.push(deliver_one(&messages, inbox, &format!("msg {n}")).await);
    }

    let mut tasks = Vec::with_capacity(WRITERS);
    for entry in entries {
        let messages = messages.clone();
        tasks.push(tokio::spawn(async move {
            messages
                .update_flags_conditional(entry.id, add_seen(), None)
                .await
        }));
    }

    let mut allocated = Vec::with_capacity(WRITERS);
    for task in tasks {
        let outcome = task.await.expect("task").expect("store");
        assert_eq!(outcome.status, FlagChangeStatus::Changed);
        allocated.push(outcome.mod_seq.get());
    }

    let unique: BTreeSet<u64> = allocated.iter().copied().collect();
    assert_eq!(
        unique.len(),
        allocated.len(),
        "every concurrent change must get its own sequence; got {allocated:?}"
    );

    let mark = mailboxes
        .counts(inbox)
        .await
        .expect("counts")
        .highest_mod_seq;
    let highest = allocated.iter().copied().max().expect("non-empty");
    assert_eq!(
        mark.get(),
        highest,
        "the mailbox mark must equal the highest value issued"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_stores_on_one_entry_leave_it_consistent() {
    // Many writers, one entry. Whatever the interleaving, the entry's final
    // sequence must be one that was actually issued and must not exceed the
    // mailbox's mark.
    let (_dir, messages, mailboxes, inbox) = contended_stack().await;
    let entry = deliver_one(&messages, inbox, "contended").await;

    const WRITERS: usize = 16;
    let mut tasks = Vec::with_capacity(WRITERS);
    for n in 0..WRITERS {
        let messages = messages.clone();
        let entry_id = entry.id;
        tasks.push(tokio::spawn(async move {
            // Alternate between two distinct mutations so roughly half are real
            // changes rather than no-ops.
            let update = if n % 2 == 0 {
                sandesh_core::FlagUpdate::Add {
                    system: SystemFlags::FLAGGED,
                    keywords: BTreeSet::new(),
                }
            } else {
                sandesh_core::FlagUpdate::Remove {
                    system: SystemFlags::FLAGGED,
                    keywords: BTreeSet::new(),
                }
            };
            messages
                .update_flags_conditional(entry_id, update, None)
                .await
        }));
    }
    for task in tasks {
        let _ = task.await.expect("task").expect("store");
    }

    let reloaded = messages
        .get_entry(entry.id)
        .await
        .expect("get")
        .expect("entry");
    let mark = mailboxes
        .counts(inbox)
        .await
        .expect("counts")
        .highest_mod_seq;

    assert!(
        reloaded.mod_seq <= mark,
        "an entry may never sit above its mailbox's high-water mark"
    );
    assert!(
        reloaded.mod_seq > ModSeq::MIN,
        "the entry was changed, so it must be stamped above the floor"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exactly_one_concurrent_conditional_store_wins() {
    // UNCHANGEDSINCE under real contention. All writers hold the same view, so
    // precisely one may apply — more than one would be a lost update, none
    // would be a livelock.
    let (_dir, messages, mailboxes, inbox) = contended_stack().await;
    let entry = deliver_one(&messages, inbox, "conditional").await;
    let shared_view = entry.mod_seq;

    const WRITERS: usize = 12;
    let mut tasks = Vec::with_capacity(WRITERS);
    for _ in 0..WRITERS {
        let messages = messages.clone();
        let entry_id = entry.id;
        tasks.push(tokio::spawn(async move {
            messages
                .update_flags_conditional(entry_id, add_seen(), Some(shared_view))
                .await
        }));
    }

    let mut changed = 0usize;
    let mut refused = 0usize;
    for task in tasks {
        match task.await.expect("task").expect("store").status {
            FlagChangeStatus::Changed => changed += 1,
            FlagChangeStatus::Failed => refused += 1,
            // A writer that arrives after the winner sees \Seen already set and
            // its own precondition already violated, so it is refused rather
            // than reported as a no-op; counting it separately would hide a bug.
            FlagChangeStatus::Unchanged => {
                panic!("a stale writer must be refused, not treated as a no-op")
            }
        }
    }

    assert_eq!(changed, 1, "exactly one writer may apply the change");
    assert_eq!(refused, WRITERS - 1, "every stale writer must be refused");

    let mark = mailboxes
        .counts(inbox)
        .await
        .expect("counts")
        .highest_mod_seq;
    assert_eq!(
        mark.get(),
        entry.mod_seq.get() + 1,
        "only the winning store may consume a sequence"
    );
}
