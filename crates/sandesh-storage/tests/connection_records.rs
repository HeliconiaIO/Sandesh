//! Connection records and their transcripts (ADR-066, Phase 7 A2).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{
    ConnectionDisposition, ConnectionId, ConnectionRecord, ConnectionStore, Protocol, Timestamp,
};
use sandesh_storage::SqliteConnectionStore;

fn store(harness: &Harness) -> SqliteConnectionStore {
    SqliteConnectionStore::new(harness.db.pool().clone())
}

fn record_at(protocol: Protocol, opened_at: Timestamp) -> ConnectionRecord {
    ConnectionRecord {
        id: ConnectionId::new(),
        protocol,
        peer: Some(String::from("127.0.0.1:54321")),
        encrypted: false,
        opened_at,
        closed_at: Some(opened_at),
        disposition: Some(ConnectionDisposition::Closed),
        transcript: vec![
            String::from("C: EHLO localhost"),
            String::from("S: 250 sandesh.test"),
        ],
        truncated: false,
    }
}

#[tokio::test]
async fn a_connection_round_trips_with_its_transcript() {
    let harness = Harness::new().await;
    let store = store(&harness);

    let written = record_at(Protocol::Smtp, Timestamp::now());
    store.record(&written).await.expect("record");

    let read = store
        .get(written.id)
        .await
        .expect("get")
        .expect("the record exists");

    assert_eq!(read, written, "every field must survive the round trip");
}

#[tokio::test]
async fn an_empty_transcript_is_empty_not_a_blank_line() {
    // Joining and splitting on '\n' turns an empty Vec into "" and back into
    // vec![""] unless handled — a phantom line in every silent connection.
    let harness = Harness::new().await;
    let store = store(&harness);

    let mut written = record_at(Protocol::Pop3, Timestamp::now());
    written.transcript = Vec::new();
    store.record(&written).await.expect("record");

    let read = store.get(written.id).await.expect("get").expect("exists");
    assert!(
        read.transcript.is_empty(),
        "expected no lines, got {:?}",
        read.transcript
    );
}

#[tokio::test]
async fn recording_the_same_connection_twice_updates_it() {
    // A long-lived session may be written once while open and again on close.
    let harness = Harness::new().await;
    let store = store(&harness);

    let mut record = record_at(Protocol::Imap, Timestamp::now());
    record.closed_at = None;
    record.disposition = None;
    store.record(&record).await.expect("first write");

    record.closed_at = Some(Timestamp::now());
    record.disposition = Some(ConnectionDisposition::Disconnected);
    record.transcript.push(String::from("C: LOGOUT"));
    store.record(&record).await.expect("second write");

    let read = store.get(record.id).await.expect("get").expect("exists");
    assert_eq!(read.disposition, Some(ConnectionDisposition::Disconnected));
    assert_eq!(read.transcript.len(), 3);

    let all = store.recent(None, 10).await.expect("recent");
    assert_eq!(all.len(), 1, "an update must not create a second row");
}

#[tokio::test]
async fn recent_filters_by_protocol_and_orders_newest_first() {
    let harness = Harness::new().await;
    let store = store(&harness);

    let base = Timestamp::now().as_micros();
    for (index, protocol) in [Protocol::Smtp, Protocol::Imap, Protocol::Smtp]
        .into_iter()
        .enumerate()
    {
        store
            .record(&record_at(
                protocol,
                Timestamp::from_micros(base + index as i64),
            ))
            .await
            .expect("record");
    }

    let smtp = store
        .recent(Some(Protocol::Smtp), 10)
        .await
        .expect("recent");
    assert_eq!(smtp.len(), 2);
    assert!(smtp.iter().all(|c| c.protocol == Protocol::Smtp));
    assert!(
        smtp[0].opened_at.as_micros() > smtp[1].opened_at.as_micros(),
        "newest first"
    );

    let all = store.recent(None, 10).await.expect("recent");
    assert_eq!(all.len(), 3, "no filter returns every protocol");
}

#[tokio::test]
async fn prune_keeps_the_newest_and_removes_the_rest() {
    // Transcripts are unbounded growth that nothing else caps; STORAGE_DESIGN
    // §5 caps messages for the same reason.
    let harness = Harness::new().await;
    let store = store(&harness);

    let base = Timestamp::now().as_micros();
    let mut newest = None;
    for index in 0..20_i64 {
        let record = record_at(Protocol::Smtp, Timestamp::from_micros(base + index));
        if index == 19 {
            newest = Some(record.id);
        }
        store.record(&record).await.expect("record");
    }

    let removed = store.prune(5).await.expect("prune");
    assert_eq!(removed, 15);

    let surviving = store.recent(None, 100).await.expect("recent");
    assert_eq!(surviving.len(), 5);
    assert_eq!(
        surviving[0].id,
        newest.expect("the newest was recorded"),
        "pruning must keep the newest, not an arbitrary five"
    );
}

#[tokio::test]
async fn a_truncated_transcript_reports_itself_as_truncated() {
    // A reader has to be able to tell a quiet connection from a cut-off one.
    let harness = Harness::new().await;
    let store = store(&harness);

    let mut written = record_at(Protocol::Imap, Timestamp::now());
    written.truncated = true;
    store.record(&written).await.expect("record");

    let read = store.get(written.id).await.expect("get").expect("exists");
    assert!(read.truncated);
}
