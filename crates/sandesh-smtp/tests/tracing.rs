//! Per-connection transcripts over a real SMTP conversation (ADR-066, A2).
//!
//! `transcript.rs` covers the recorder and `transcript_capture.rs` covers a
//! bare `Connection`. These drive the actual SMTP server over TCP, which is
//! what proves the listener opens a record, threads its transcript through the
//! session, and persists it when the socket closes.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{ConnectionDisposition, ConnectionStore, Protocol};

#[tokio::test]
async fn a_delivery_leaves_a_transcript_with_the_body_elided() {
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client
        .deliver(
            "alice@example.test",
            &["bob@example.test"],
            b"Subject: reset\r\n\r\nyour one-time code is 987654\r\n",
        )
        .await;
    client.send("QUIT").await;

    // Poll for the *closed* record specifically. An open connection's row
    // exists from the moment it is accepted and its transcript is flushed
    // while the socket is live (Phase 7 Pass B), so "the most recent record"
    // is not necessarily a finished one — taking the first row here caught
    // the in-flight version and failed on `closed_at`.
    let record = await_some(|| async {
        server
            .connections
            .recent(None, 10)
            .await
            .expect("recent")
            .into_iter()
            .find(|record| record.closed_at.is_some())
    })
    .await
    .expect("a closed connection record must be written");

    assert_eq!(record.protocol, Protocol::Smtp);
    assert!(record.peer.is_some(), "a TCP connection has a peer");
    assert!(record.closed_at.is_some(), "the record is closed");
    assert_eq!(record.disposition, Some(ConnectionDisposition::Closed));

    let text = record.transcript.join("\n");

    // The conversation is legible.
    assert!(text.contains("EHLO localhost"), "{text}");
    assert!(text.contains("MAIL FROM"), "{text}");
    assert!(text.contains("RCPT TO"), "{text}");
    assert!(text.contains("250"), "server replies are recorded: {text}");

    // The message is not.
    assert!(
        !text.contains("987654"),
        "a one-time code reached a persisted transcript: {text}"
    );
    assert!(
        !text.contains("Subject: reset"),
        "message headers are content too: {text}"
    );
    assert!(
        text.contains("DATA:") && text.contains("bytes not recorded"),
        "the transcript should say content passed, and how much: {text}"
    );
}

#[tokio::test]
async fn a_connection_that_delivers_nothing_is_still_recorded() {
    // The case `deliveries.trace` could never have covered, and one of the
    // reasons ADR-066 exists: a probe that greets and quits produces no
    // delivery row at all, yet is exactly what a developer wants to see when
    // asking "did anything even connect?".
    let server = TestServer::start().await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("QUIT").await;

    // The closed record, not merely the newest: an open connection's row is
    // written on accept and flushed while live (Phase 7 Pass B).
    let record = await_some(|| async {
        server
            .connections
            .recent(None, 10)
            .await
            .expect("recent")
            .into_iter()
            .find(|record| record.closed_at.is_some())
    })
    .await
    .expect("a closed connection record must be written");

    let text = record.transcript.join("\n");
    assert!(text.contains("EHLO"), "{text}");
    assert!(text.contains("QUIT"), "{text}");
}

#[tokio::test]
async fn tracing_is_off_unless_a_recorder_is_configured() {
    // `SmtpServer` records nothing by default. A server that captured
    // transcripts because someone forgot to opt out would be the wrong
    // default for a tool holding password-reset links.
    let server = TestServer::start_untraced().await;
    server.account("bob", "bob@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client
        .deliver(
            "alice@example.test",
            &["bob@example.test"],
            b"Subject: x\r\n\r\ny\r\n",
        )
        .await;
    client.send("QUIT").await;

    let recent = server.connections.recent(None, 10).await.expect("recent");
    assert!(
        recent.is_empty(),
        "no recorder was configured, so nothing should be persisted: {recent:?}"
    );
}

#[tokio::test]
async fn a_delivery_links_to_the_connection_that_produced_it() {
    // `deliveries.connection_id` is what lets the Delivery view offer the
    // transcript beside the routing outcome. Without it the two records exist
    // and cannot find each other.
    use sandesh_core::DeliveryStore;

    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    let reply = client
        .deliver(
            "alice@example.test",
            &["bob@example.test"],
            b"Subject: linked\r\n\r\nbody\r\n",
        )
        .await;
    assert!(
        reply.starts_with("250"),
        "the delivery itself must succeed, or the link means nothing: {reply}"
    );
    client.send("QUIT").await;

    let delivery = await_some(|| async {
        server
            .deliveries
            .recent(10)
            .await
            .expect("recent")
            .into_iter()
            .next()
    })
    .await
    .expect("a delivery must be recorded");

    let connection_id = delivery
        .connection_id
        .expect("a delivery over SMTP must name its connection");

    // And that id must resolve to a real record, not a dangling reference.
    let record = await_some(|| async {
        server
            .connections
            .get(connection_id)
            .await
            .expect("get connection")
    })
    .await
    .expect("the named connection record must exist");

    assert_eq!(record.protocol, sandesh_core::Protocol::Smtp);
    assert!(
        record
            .transcript
            .iter()
            .any(|line| line.contains("MAIL FROM")),
        "the linked transcript is the conversation that delivered it: {:?}",
        record.transcript
    );
}

/// Polls `probe` until it yields a value, giving up after a bounded wait.
///
/// Records are written as a socket closes, which races the client's own view
/// of the conversation ending. An unbounded spin would turn a genuine "this is
/// never recorded" bug into a hung test rather than a failing one — which is
/// exactly what happened while writing these.
async fn await_some<T, F, Fut>(mut probe: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..200 {
        if let Some(value) = probe().await {
            return Some(value);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    None
}
