//! Looking a delivery up by the message it produced (Phase 7, A1).
//!
//! Before this existed, the API answered "how did this message get here?" by
//! fetching the 500 most recent delivery records and scanning them in memory.
//! That is not merely slow. A message older than the scan window has a delivery
//! record the scan cannot see, so the endpoint returned 404 for data sitting in
//! the database — and 404 is indistinguishable, to a caller, from "this message
//! was never delivered".
//!
//! The tests below are written around that window deliberately: one of them
//! would pass against the old scan, and the other would not.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{
    Delivery, DeliveryId, DeliveryOutcome, DeliverySource, DeliveryStore, EmailAddress, Envelope,
    MessageId, RecipientOutcome, Timestamp,
};
use sandesh_storage::{CollectingSink, EventRecorder, SqliteDeliveryStore};
use std::sync::Arc;

/// The scan window the old implementation used, from `routes::MAX_LIMIT`.
const OLD_SCAN_WINDOW: usize = 500;

fn store(harness: &Harness) -> SqliteDeliveryStore {
    let events = EventRecorder::new(Arc::new(CollectingSink::new()));
    SqliteDeliveryStore::new(harness.db.pool().clone(), events)
}

/// A delivery record naming `message_id`, received at `received_at`.
fn delivery_for(message_id: Option<MessageId>, received_at: Timestamp) -> Delivery {
    Delivery {
        id: DeliveryId::new(),
        message_id,
        source: DeliverySource::Smtp,
        envelope: Envelope {
            mail_from: Some(EmailAddress::parse_lenient("sender@example.test")),
            rcpt_to: vec![EmailAddress::parse_lenient("alice@example.test")],
            helo_domain: Some(String::from("client.example.test")),
            peer: Some(String::from("127.0.0.1:2525")),
            tls: None,
            auth_identity: None,
        },
        outcomes: vec![RecipientOutcome {
            rcpt_to: EmailAddress::parse_lenient("alice@example.test"),
            resolved_account: None,
            entry_id: None,
            outcome: DeliveryOutcome::Delivered,
            reason: None,
        }],
        received_at,
        duration_us: 1_234,
        connection_id: None,
        rules_applied: Vec::new(),
        replay_of: None,
    }
}

/// Stores a real message and returns its id.
///
/// `deliveries.message_id` is a foreign key onto `messages(id)`, so a delivery
/// naming an invented id is rejected by the database — correctly. Anything
/// asserting on a delivery that produced a message has to store one first.
async fn stored_message(harness: &Harness, mailbox: sandesh_core::MailboxId) -> MessageId {
    harness
        .deliver(b"Subject: delivered\r\n\r\nbody\r\n", &[mailbox])
        .await
        .message_id
}

#[tokio::test]
async fn a_delivery_is_found_by_the_message_it_produced() {
    let harness = Harness::new().await;
    let deliveries = store(&harness);
    let account = harness.account("alice", "alice@example.test").await;
    let mailbox = harness.mailbox(account, "INBOX").await;

    let message_id = stored_message(&harness, mailbox).await;
    let recorded = delivery_for(Some(message_id), Timestamp::now());
    deliveries.record(&recorded).await.expect("record");

    let found = deliveries
        .by_message_id(message_id)
        .await
        .expect("lookup")
        .expect("the delivery exists");

    assert_eq!(found.id, recorded.id);
    assert_eq!(found.message_id, Some(message_id));
    // The envelope has to survive the round trip: the view renders it.
    assert_eq!(found.envelope.peer.as_deref(), Some("127.0.0.1:2525"));
    assert_eq!(
        found.envelope.helo_domain.as_deref(),
        Some("client.example.test")
    );
    assert_eq!(found.duration_us, 1_234);
}

#[tokio::test]
async fn a_message_with_no_delivery_record_is_none_not_an_error() {
    // A message injected through the API before delivery recording existed is
    // a real state, and the caller has to be able to tell it apart from a
    // failure.
    let harness = Harness::new().await;
    let deliveries = store(&harness);

    let found = deliveries
        .by_message_id(MessageId::new())
        .await
        .expect("the lookup itself must succeed");

    assert!(found.is_none());
}

#[tokio::test]
async fn a_delivery_older_than_the_old_scan_window_is_still_found() {
    // The regression this whole change exists for. The target record is written
    // first and then buried under more than 500 newer ones, so a scan of the
    // 500 most recent cannot reach it. An index lookup does not care.
    let harness = Harness::new().await;
    let deliveries = store(&harness);
    let account = harness.account("alice", "alice@example.test").await;
    let mailbox = harness.mailbox(account, "INBOX").await;

    let buried = stored_message(&harness, mailbox).await;
    let base = Timestamp::now().as_micros();

    deliveries
        .record(&delivery_for(Some(buried), Timestamp::from_micros(base)))
        .await
        .expect("record the target");

    // The burying records need no message of their own: what matters is that
    // they are newer, so they fill the recency window ahead of the target.
    for offset in 1..=(OLD_SCAN_WINDOW as i64 + 50) {
        deliveries
            .record(&delivery_for(None, Timestamp::from_micros(base + offset)))
            .await
            .expect("record a newer delivery");
    }

    // Confirm the burial is real: the record is genuinely outside the window
    // the old implementation looked at. Without this the test could pass for
    // the wrong reason if `recent` ever changed shape.
    let window = deliveries
        .recent(OLD_SCAN_WINDOW as u32)
        .await
        .expect("recent");
    assert!(
        !window.iter().any(|d| d.message_id == Some(buried)),
        "the target must be outside the scan window, or this test proves nothing"
    );

    let found = deliveries
        .by_message_id(buried)
        .await
        .expect("lookup")
        .expect("a delivery outside the scan window is still a delivery");

    assert_eq!(found.message_id, Some(buried));
}

#[tokio::test]
async fn a_rejected_transaction_is_recorded_without_a_message() {
    // A refusal stores no message, so the record carries `message_id: None`.
    // It is reachable only through the recency listing, which is why the
    // `/deliveries` endpoint exists.
    let harness = Harness::new().await;
    let deliveries = store(&harness);

    let mut rejection = delivery_for(None, Timestamp::now());
    rejection.outcomes[0].outcome = DeliveryOutcome::Rejected;
    rejection.outcomes[0].reason = Some(String::from("message exceeds the 100-octet limit"));
    deliveries.record(&rejection).await.expect("record");

    let recent = deliveries.recent(10).await.expect("recent");
    let found = recent
        .iter()
        .find(|d| d.id == rejection.id)
        .expect("a rejection appears in the recency listing");

    assert_eq!(found.message_id, None);
    assert_eq!(found.outcomes[0].outcome, DeliveryOutcome::Rejected);
    assert_eq!(
        found.outcomes[0].reason.as_deref(),
        Some("message exceeds the 100-octet limit")
    );
}
