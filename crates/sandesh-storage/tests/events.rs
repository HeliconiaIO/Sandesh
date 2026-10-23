//! Domain events emitted by storage operations.
//!
//! Phase 1 has no bus; these tests verify that the *facts* are produced with
//! the right shape, so Phase 3 only has to route them.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{EventKind, EventPayload, FlagUpdate, MessageStore, SystemFlags};
use std::collections::BTreeSet;

#[tokio::test]
async fn creating_an_account_emits_account_created() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;

    let found = h.sink.events().into_iter().find_map(|e| match e.payload {
        EventPayload::AccountCreated {
            account_id,
            username,
        } => Some((account_id, username)),
        _ => None,
    });
    assert_eq!(found, Some((account, String::from("dev"))));
}

#[tokio::test]
async fn delivery_emits_entry_created_and_message_received() {
    // Both fire: EntryCreated is the structural fact IMAP IDLE consumes,
    // MessageReceived is the message-level fact the UI shows.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let raw = common::fixture("valid/plain-text.eml");
    let inserted = h.deliver(&raw, &[inbox]).await;

    let kinds = h.sink.kinds();
    assert!(kinds.contains(&EventKind::EntryCreated));
    assert!(kinds.contains(&EventKind::MessageReceived));

    let received = h
        .sink
        .events()
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::MessageReceived {
                message_id,
                mailbox_id,
                account_id,
                uid,
                size,
                ..
            } => Some((message_id, mailbox_id, account_id, uid, size)),
            _ => None,
        })
        .expect("MessageReceived emitted");

    assert_eq!(received.0, inserted.message_id);
    assert_eq!(received.1, inbox);
    assert_eq!(
        received.2, account,
        "the owning account must be named, not a placeholder"
    );
    assert_eq!(received.3, inserted.entries[0].uid);
    assert_eq!(received.4, raw.len() as u64, "size is RFC822.SIZE");
}

#[tokio::test]
async fn multi_recipient_delivery_emits_one_event_pair_per_placement() {
    let h = Harness::new().await;
    let alice = h.account("alice", "alice@example.test").await;
    let bob = h.account("bob", "bob@example.test").await;
    let alice_inbox = h.mailbox(alice, "INBOX").await;
    let bob_inbox = h.mailbox(bob, "INBOX").await;

    h.deliver(
        &common::fixture("valid/plain-text.eml"),
        &[alice_inbox, bob_inbox],
    )
    .await;

    let received: Vec<_> = h
        .sink
        .kinds()
        .into_iter()
        .filter(|k| *k == EventKind::MessageReceived)
        .collect();
    assert_eq!(received.len(), 2, "one per placement, not one per message");
}

#[tokio::test]
async fn flag_changes_report_what_actually_changed() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;

    h.messages
        .update_flags(
            inserted.entries[0].id,
            FlagUpdate::Add {
                system: SystemFlags::SEEN,
                keywords: BTreeSet::new(),
            },
        )
        .await
        .expect("flag");

    let change = h
        .sink
        .events()
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::EntryFlagsChanged {
                added,
                removed,
                flags_now,
                ..
            } => Some((added, removed, flags_now)),
            _ => None,
        })
        .expect("EntryFlagsChanged emitted");

    assert_eq!(change.0, vec![r"\Seen"], "added");
    assert!(change.1.is_empty(), "nothing was removed");
    assert_eq!(change.2, vec![r"\Seen"], "resulting flags");
}

#[tokio::test]
async fn removing_a_flag_is_reported_as_a_removal() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;
    let entry = inserted.entries[0].id;

    h.messages
        .update_flags(
            entry,
            FlagUpdate::Add {
                system: SystemFlags::SEEN,
                keywords: BTreeSet::new(),
            },
        )
        .await
        .expect("add");
    h.messages
        .update_flags(
            entry,
            FlagUpdate::Remove {
                system: SystemFlags::SEEN,
                keywords: BTreeSet::new(),
            },
        )
        .await
        .expect("remove");

    let last = h
        .sink
        .events()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::EntryFlagsChanged { added, removed, .. } => Some((added, removed)),
            _ => None,
        })
        .next_back()
        .expect("a second flag event");

    assert!(last.0.is_empty(), "nothing added");
    assert_eq!(last.1, vec![r"\Seen"], "removed");
}

#[tokio::test]
async fn deleting_a_placement_emits_entry_deleted() {
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;

    h.messages
        .delete_entry(inserted.entries[0].id)
        .await
        .expect("delete");

    assert!(h.sink.kinds().contains(&EventKind::EntryDeleted));
}

#[tokio::test]
async fn events_carry_identifiers_not_message_bodies() {
    // Events are notifications about committed facts. Carrying bodies would let
    // one large attachment starve a slow subscriber (EVENT_MODEL §1).
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;

    let mut raw = b"Subject: large\r\n\r\n".to_vec();
    raw.extend(std::iter::repeat_n(b'X', 200_000));
    h.deliver(&raw, &[inbox]).await;

    // A rough proxy for "no body was copied in": the debug rendering of every
    // event stays small regardless of message size.
    for event in h.sink.events() {
        let rendered = format!("{event:?}");
        assert!(
            rendered.len() < 2_000,
            "event payload looks like it carries content: {} bytes",
            rendered.len()
        );
    }
}

#[tokio::test]
async fn events_are_emitted_only_after_the_data_is_queryable() {
    // A consumer woken by MessageReceived must never race an empty read
    // (EVENT_MODEL §5): events are emitted post-commit.
    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let inserted = h
        .deliver(&common::fixture("valid/plain-text.eml"), &[inbox])
        .await;

    for event in h.sink.events() {
        if let EventPayload::MessageReceived {
            message_id,
            entry_id,
            ..
        } = event.payload
        {
            assert!(
                h.messages.get(message_id).await.expect("query").is_some(),
                "the message must be readable when the event is observed"
            );
            assert!(
                h.messages
                    .get_entry(entry_id)
                    .await
                    .expect("query")
                    .is_some(),
                "the placement must be readable too"
            );
        }
    }
    assert_eq!(inserted.entries.len(), 1);
}

#[tokio::test]
async fn a_rolled_back_delivery_emits_nothing() {
    use sandesh_core::{
        DeliverySource, Envelope, MailboxId, MessageFlags, NewMessage, NewPlacement, RawMessage,
        Timestamp,
    };

    let h = Harness::new().await;
    let account = h.account("dev", "dev@example.test").await;
    let inbox = h.mailbox(account, "INBOX").await;
    let before = h.sink.events().len();

    let raw = RawMessage::new(common::fixture("valid/plain-text.eml"));
    let parsed = sandesh_core::parse(raw.bytes());
    let now = Timestamp::now();

    let result = h
        .messages
        .insert(&NewMessage {
            raw,
            envelope: Envelope::default(),
            source: DeliverySource::Smtp,
            parsed,
            received_at: now,
            placements: vec![
                NewPlacement {
                    mailbox_id: inbox,
                    flags: MessageFlags::empty(),
                    internal_date: now,
                },
                NewPlacement {
                    mailbox_id: MailboxId::new(), // does not exist
                    flags: MessageFlags::empty(),
                    internal_date: now,
                },
            ],
        })
        .await;

    assert!(result.is_err());
    assert_eq!(
        h.sink.events().len(),
        before,
        "a failed delivery must not announce a fact that did not happen"
    );
}
