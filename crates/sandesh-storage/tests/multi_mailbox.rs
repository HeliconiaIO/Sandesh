//! The defining test of the model: one message, several mailboxes, independent
//! UIDs and flags.
//!
//! This is what `messages.mailbox_id` could not express (ADR-009), and it is
//! the reason [`MessageEntry`] exists as its own entity. If these tests fail,
//! IMAP cannot be built on this storage layer.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{FlagUpdate, MessageStore, SystemFlags};
use std::collections::BTreeSet;

#[tokio::test]
async fn one_message_in_two_mailboxes_has_independent_flags() {
    // The mandated Phase 1 test: change flags on the INBOX placement and prove
    // the Archive placement is untouched.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let archive = h.mailbox(account, "Archive").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let inbox_entry = inserted.entries.first().expect("inbox placement");

    // IMAP COPY: place the same message into Archive.
    let archive_entry = h
        .messages
        .copy_entry(inbox_entry.id, archive)
        .await
        .expect("copy entry");

    assert_eq!(
        archive_entry.message_id, inbox_entry.message_id,
        "COPY shares the message; it must not duplicate it"
    );

    // Mutate flags on the INBOX placement only.
    h.messages
        .update_flags(
            inbox_entry.id,
            FlagUpdate::Add {
                system: SystemFlags::SEEN.union(SystemFlags::FLAGGED),
                keywords: BTreeSet::new(),
            },
        )
        .await
        .expect("set flags");

    let inbox_now = h
        .messages
        .get_entry(inbox_entry.id)
        .await
        .expect("query")
        .expect("exists");
    let archive_now = h
        .messages
        .get_entry(archive_entry.id)
        .await
        .expect("query")
        .expect("exists");

    assert!(inbox_now.flags.has(SystemFlags::SEEN));
    assert!(inbox_now.flags.has(SystemFlags::FLAGGED));
    assert!(
        !archive_now.flags.has(SystemFlags::SEEN),
        "flags must not leak between placements"
    );
    assert!(!archive_now.flags.has(SystemFlags::FLAGGED));
}

#[tokio::test]
async fn placements_in_different_mailboxes_get_independent_uids() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let archive = h.mailbox(account, "Archive").await;

    // Give Archive a head start so its UID sequence is genuinely separate.
    for _ in 0..5 {
        h.deliver(&common::fixture("valid/html.eml"), &[archive])
            .await;
    }

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let inbox_entry = inserted.entries.first().expect("placement");
    let archive_entry = h
        .messages
        .copy_entry(inbox_entry.id, archive)
        .await
        .expect("copy");

    assert_eq!(inbox_entry.uid.get(), 1, "INBOX sequence starts at 1");
    assert_eq!(
        archive_entry.uid.get(),
        6,
        "Archive continues its own sequence, independent of INBOX"
    );
}

#[tokio::test]
async fn one_delivery_to_several_mailboxes_stores_the_message_once() {
    // Multi-recipient delivery: one message, several placements, one copy of
    // the bytes (CORE_SPEC §7.2).
    let h = Harness::new().await;
    let alice = h.account("alice", "alice@example.test").await;
    let bob = h.account("bob", "bob@example.test").await;
    let alice_inbox = h.mailbox(alice, "INBOX").await;
    let bob_inbox = h.mailbox(bob, "INBOX").await;

    let raw = common::fixture("valid/multipart-mixed.eml");
    let inserted = h.deliver(&raw, &[alice_inbox, bob_inbox]).await;

    assert_eq!(inserted.entries.len(), 2, "one placement per recipient");

    let message_ids: BTreeSet<_> = inserted.entries.iter().map(|e| e.message_id).collect();
    assert_eq!(message_ids.len(), 1, "the bytes are stored exactly once");

    let stored_messages: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(h.db.pool())
        .await
        .expect("count");
    assert_eq!(stored_messages.0, 1);

    // Both recipients see identical bytes.
    for entry in &inserted.entries {
        let raw_back = h
            .messages
            .raw(entry.message_id)
            .await
            .expect("query")
            .expect("exists");
        assert_eq!(raw_back.bytes(), raw.as_slice());
    }
}

#[tokio::test]
async fn deleting_one_placement_leaves_the_others_intact() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let archive = h.mailbox(account, "Archive").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let inbox_entry = inserted.entries.first().expect("placement");
    let archive_entry = h
        .messages
        .copy_entry(inbox_entry.id, archive)
        .await
        .expect("copy");

    assert!(
        h.messages
            .delete_entry(inbox_entry.id)
            .await
            .expect("delete")
    );

    assert!(
        h.messages
            .get_entry(archive_entry.id)
            .await
            .expect("query")
            .is_some(),
        "the surviving placement must remain"
    );
    assert!(
        h.messages
            .get(inserted.message_id)
            .await
            .expect("query")
            .is_some(),
        "the message survives while any placement references it"
    );
}

#[tokio::test]
async fn message_is_collected_only_after_its_last_placement_goes() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let archive = h.mailbox(account, "Archive").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let inbox_entry = inserted.entries.first().expect("placement");
    let archive_entry = h
        .messages
        .copy_entry(inbox_entry.id, archive)
        .await
        .expect("copy");

    h.messages
        .delete_entry(inbox_entry.id)
        .await
        .expect("delete");
    assert!(
        h.messages.collect_orphans().await.expect("gc").is_empty(),
        "a message with a remaining placement is not garbage"
    );

    h.messages
        .delete_entry(archive_entry.id)
        .await
        .expect("delete");
    let collected = h.messages.collect_orphans().await.expect("gc");
    assert_eq!(collected, vec![inserted.message_id]);
    assert!(
        h.messages
            .get(inserted.message_id)
            .await
            .expect("query")
            .is_none()
    );
}

#[tokio::test]
async fn a_message_cannot_be_placed_twice_in_one_mailbox() {
    // Two envelope recipients resolving to the same mailbox produce one entry
    // (CORE_SPEC §7.3). The database enforces this rather than trusting callers.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = inserted.entries.first().expect("placement");

    let duplicate = h.messages.copy_entry(entry.id, inbox).await;
    assert!(duplicate.is_err(), "a repeat placement must be rejected");
}

#[tokio::test]
async fn entries_for_message_lists_every_placement() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let archive = h.mailbox(account, "Archive").await;
    let sent = h.mailbox(account, "Sent").await;

    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = inserted.entries.first().expect("placement");
    h.messages
        .copy_entry(entry.id, archive)
        .await
        .expect("copy");
    h.messages.copy_entry(entry.id, sent).await.expect("copy");

    let all = h
        .messages
        .entries_for_message(inserted.message_id)
        .await
        .expect("query");
    assert_eq!(all.len(), 3);

    let mailboxes: BTreeSet<_> = all.iter().map(|e| e.mailbox_id).collect();
    assert_eq!(mailboxes.len(), 3, "one placement per mailbox");
}
