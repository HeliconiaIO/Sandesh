//! The durable outbox: enqueue-in-transaction, and survival across a restart.
//!
//! The restart half is the reason this file exists. "Webhooks retry on failure
//! and survive a restart" is one criterion with two halves, and an in-memory
//! queue passes the first while silently failing the second -- so the test that
//! matters closes the database entirely and opens it again from the same file.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{EndpointId, OutboxStatus, OutboxStore, Timestamp, WebhookEndpoint};
use sandesh_storage::{Database, SqliteOutboxStore};

const MESSAGE: &[u8] = b"From: a@example.com\r\nTo: b@example.com\r\nSubject: hi\r\n\r\nbody\r\n";

fn endpoint(url: &str) -> WebhookEndpoint {
    WebhookEndpoint {
        id: EndpointId::new(),
        url: url.to_owned(),
        enabled: true,
        created_at: Timestamp::now(),
        secret: None,
    }
}

fn outbox(harness: &Harness) -> SqliteOutboxStore {
    SqliteOutboxStore::new(harness.db.pool().clone())
}

#[tokio::test]
async fn no_endpoint_means_no_outbox_rows() {
    // Webhooks are opt-in. A server with none configured must behave exactly as
    // it did before Phase 8, and the cheapest proof is that delivery writes
    // nothing at all here.
    let harness = Harness::new().await;
    let account = harness.account("alice", "alice@example.com").await;
    let inbox = harness.mailbox(account, "INBOX").await;

    harness.deliver(MESSAGE, &[inbox]).await;

    assert!(outbox(&harness).recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn delivery_enqueues_one_entry_per_placement() {
    let harness = Harness::new().await;
    let store = outbox(&harness);
    store
        .register_endpoint(&endpoint("http://localhost/hook"))
        .await
        .unwrap();

    let account = harness.account("alice", "alice@example.com").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    let archive = harness.mailbox(account, "Archive").await;

    harness.deliver(MESSAGE, &[inbox, archive]).await;

    // Two placements, two entries -- matching `MessageReceived`, which is
    // emitted per placement. Collapsing them would invent an aggregate the
    // event bus does not have.
    let entries = store.recent(10).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.status == OutboxStatus::Pending));
    assert!(entries.iter().all(|e| e.attempts == 0));
}

#[tokio::test]
async fn the_payload_names_the_message_and_its_placement() {
    let harness = Harness::new().await;
    let store = outbox(&harness);
    store
        .register_endpoint(&endpoint("http://localhost/hook"))
        .await
        .unwrap();

    let account = harness.account("alice", "alice@example.com").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    let inserted = harness.deliver(MESSAGE, &[inbox]).await;

    let entries = store.recent(10).await.unwrap();
    let payload = &entries[0].payload;

    // Asserting the ids are actually present, not merely that the payload is
    // JSON: a consumer's whole purpose is to fetch what it was told about.
    assert!(payload.contains(&inserted.message_id.to_string()));
    assert!(payload.contains(&inbox.to_string()));
    assert!(payload.contains("MessageReceived"));
}

#[tokio::test]
async fn a_disabled_endpoint_stops_new_entries() {
    let harness = Harness::new().await;
    let store = outbox(&harness);
    let mut hook = endpoint("http://localhost/hook");
    hook.enabled = false;
    store.register_endpoint(&hook).await.unwrap();

    let account = harness.account("alice", "alice@example.com").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    harness.deliver(MESSAGE, &[inbox]).await;

    assert!(store.recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn only_due_entries_are_returned() {
    let harness = Harness::new().await;
    let store = outbox(&harness);
    store
        .register_endpoint(&endpoint("http://localhost/hook"))
        .await
        .unwrap();

    let account = harness.account("alice", "alice@example.com").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    harness.deliver(MESSAGE, &[inbox]).await;

    let entry = store.recent(10).await.unwrap().remove(0);

    // Push it into the future, as a failed attempt would.
    let later = Timestamp::from_micros(Timestamp::now().as_micros() + 60_000_000);
    store
        .record_attempt(entry.id, OutboxStatus::Pending, later, Some("HTTP 500"))
        .await
        .unwrap();

    assert!(store.due(Timestamp::now(), 10).await.unwrap().is_empty());
    assert_eq!(store.due(later, 10).await.unwrap().len(), 1);

    // The attempt count came from the write, so the worker and the row cannot
    // disagree about how many attempts have happened.
    assert_eq!(store.recent(10).await.unwrap()[0].attempts, 1);
}

#[tokio::test]
async fn a_terminal_entry_is_never_due_again() {
    let harness = Harness::new().await;
    let store = outbox(&harness);
    store
        .register_endpoint(&endpoint("http://localhost/hook"))
        .await
        .unwrap();

    let account = harness.account("alice", "alice@example.com").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    harness.deliver(MESSAGE, &[inbox]).await;

    let entry = store.recent(10).await.unwrap().remove(0);
    store
        .record_attempt(entry.id, OutboxStatus::Delivered, Timestamp::now(), None)
        .await
        .unwrap();

    assert!(store.due(Timestamp::now(), 10).await.unwrap().is_empty());
    // Still listed, though: the outbox view shows what happened, not only what
    // is outstanding.
    assert_eq!(store.recent(10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn removing_an_endpoint_takes_its_pending_entries_with_it() {
    let harness = Harness::new().await;
    let store = outbox(&harness);
    let hook = endpoint("http://localhost/hook");
    store.register_endpoint(&hook).await.unwrap();

    let account = harness.account("alice", "alice@example.com").await;
    let inbox = harness.mailbox(account, "INBOX").await;
    harness.deliver(MESSAGE, &[inbox]).await;
    assert_eq!(store.recent(10).await.unwrap().len(), 1);

    assert!(store.remove_endpoint(hook.id).await.unwrap());

    // Cascaded, per the migration. A pending entry for a destination that no
    // longer exists is undeliverable by construction.
    assert!(store.recent(10).await.unwrap().is_empty());
    assert!(!store.remove_endpoint(hook.id).await.unwrap());
}

#[tokio::test]
async fn a_pending_entry_survives_a_restart() {
    // The half of the criterion an in-memory queue would silently fail. The
    // database is closed entirely and reopened from the same file, which is a
    // restart in every sense that matters to the outbox.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("sandesh.db");

    let (endpoint_id, entry_id, payload) = {
        let harness = Harness::on_disk(&path).await;
        let store = outbox(&harness);

        let hook = endpoint("http://localhost/hook");
        store.register_endpoint(&hook).await.unwrap();

        let account = harness.account("alice", "alice@example.com").await;
        let inbox = harness.mailbox(account, "INBOX").await;
        harness.deliver(MESSAGE, &[inbox]).await;

        let entry = store.recent(10).await.unwrap().remove(0);
        assert_eq!(entry.status, OutboxStatus::Pending);

        harness.db.close().await;
        (hook.id, entry.id, entry.payload)
    };

    // Second process, same file.
    let db = Database::open(&path).await.expect("reopen database");
    let store = SqliteOutboxStore::new(db.pool().clone());

    let due = store.due(Timestamp::now(), 10).await.unwrap();
    assert_eq!(due.len(), 1, "the pending entry must still be deliverable");
    assert_eq!(due[0].id, entry_id);
    assert_eq!(due[0].payload, payload);
    assert_eq!(due[0].status, OutboxStatus::Pending);

    // The endpoint survived too. An entry pointing at a destination that did
    // not survive would be durable in name only.
    let endpoints = store.endpoints().await.unwrap();
    assert_eq!(endpoints.len(), 1);
    assert_eq!(endpoints[0].id, endpoint_id);
}
