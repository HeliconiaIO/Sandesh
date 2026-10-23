//! Injected faults, observed from the client (ADR-084).
//!
//! The Phase 8 acceptance criterion is that each mode is "triggerable by
//! configuration and observable **by the client**", so every test here drives a
//! real socket and asserts what the client saw. Asserting server-side registry
//! state instead would pass without the feature working at all — the mistake
//! the Phase 5 audit found in three criteria that were checked only by
//! inspection.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{TestServer, code_of};
use sandesh_core::{FaultMatch, FaultMode, FaultRule};
use std::time::{Duration, Instant};

const MESSAGE: &[u8] = b"Subject: test\r\n\r\nbody\r\n";

#[tokio::test]
async fn with_no_faults_configured_delivery_is_unaffected() {
    // The inert-by-default claim (ADR-005, ADR-084). The wider SMTP suite
    // already asserts this by passing untouched; this states it directly so a
    // regression names the cause.
    let server = TestServer::start().await;
    server.catch_all().await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    assert!(!server.faults.is_active());

    let reply = client
        .deliver("sender@test.example", &["to@test.example"], MESSAGE)
        .await;
    assert_eq!(code_of(&reply), 250, "clean delivery expected: {reply}");
}

#[tokio::test]
async fn reject_at_rcpt_is_a_550_the_client_sees() {
    let server = TestServer::start().await;
    server.catch_all().await;
    server
        .faults
        .add(FaultRule::always(FaultMode::RejectAtRcpt));

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<sender@test.example>").await;
    let reply = client.send("RCPT TO:<anyone@test.example>").await;

    assert_eq!(code_of(&reply), 550, "injected RCPT rejection: {reply}");
    assert_eq!(
        server.message_count().await,
        0,
        "a rejected recipient must store nothing"
    );
}

#[tokio::test]
async fn a_rejected_recipient_leaves_the_transaction_open() {
    // RFC 5321 §3.3: a refused recipient does not end the transaction. The
    // fault must not be harsher than the protocol allows, or it would be
    // testing a server bug rather than the client's handling.
    let server = TestServer::start().await;
    server.catch_all().await;
    server.faults.add(FaultRule {
        mode: FaultMode::RejectAtRcpt,
        selector: FaultMatch {
            recipient: Some(String::from("blocked@test.example")),
            sender: None,
        },
    });

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<sender@test.example>").await;

    let rejected = client.send("RCPT TO:<blocked@test.example>").await;
    assert_eq!(code_of(&rejected), 550);

    // The same transaction continues with another address, which must work.
    let accepted = client.send("RCPT TO:<allowed@test.example>").await;
    assert_eq!(
        code_of(&accepted),
        250,
        "the transaction must survive a rejected recipient: {accepted}"
    );

    client.send("DATA").await;
    let delivered = client.send_data(MESSAGE).await;
    assert_eq!(code_of(&delivered), 250, "delivery expected: {delivered}");
}

#[tokio::test]
async fn reject_at_data_is_a_554_after_the_body_is_sent() {
    let server = TestServer::start().await;
    server.catch_all().await;
    server
        .faults
        .add(FaultRule::always(FaultMode::RejectAtData));

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<sender@test.example>").await;

    // The distinguishing property of this mode: RCPT is accepted, so the
    // client commits to transmitting before it learns the message is refused.
    let rcpt = client.send("RCPT TO:<to@test.example>").await;
    assert_eq!(code_of(&rcpt), 250, "RCPT must be accepted: {rcpt}");

    client.send("DATA").await;
    let reply = client.send_data(MESSAGE).await;
    assert_eq!(code_of(&reply), 554, "injected DATA rejection: {reply}");
    assert_eq!(
        server.message_count().await,
        0,
        "a refused message must not be stored"
    );
}

#[tokio::test]
async fn temporary_failure_is_a_4xx_so_a_client_retries() {
    // The mode exists to exercise retry logic, which only runs if the reply is
    // temporary. A 5xx here would silently make the fault untestable for its
    // primary purpose.
    let server = TestServer::start().await;
    server.catch_all().await;
    server
        .faults
        .add(FaultRule::always(FaultMode::TemporaryFailure));

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<sender@test.example>").await;
    client.send("RCPT TO:<to@test.example>").await;
    client.send("DATA").await;
    let reply = client.send_data(MESSAGE).await;

    let code = code_of(&reply);
    assert!(
        (400..500).contains(&code),
        "a temporary failure must be 4xx so the client retries, got: {reply}"
    );
    assert_eq!(server.message_count().await, 0);
}

#[tokio::test]
async fn slow_response_delays_but_still_delivers() {
    // The mode tests a client's patience, not its error handling: the reply
    // that follows must be the ordinary one. An implementation that delayed
    // and then swallowed the reply would be a protocol violation, and this
    // test is what distinguishes the two.
    let delay = Duration::from_millis(300);
    let server = TestServer::start().await;
    server.catch_all().await;
    server
        .faults
        .add(FaultRule::always(FaultMode::SlowResponse(delay)));

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<sender@test.example>").await;
    client.send("RCPT TO:<to@test.example>").await;
    client.send("DATA").await;

    let started = Instant::now();
    let reply = client.send_data(MESSAGE).await;
    let elapsed = started.elapsed();

    assert_eq!(
        code_of(&reply),
        250,
        "a slow response is still a successful one: {reply}"
    );
    assert!(
        elapsed >= delay,
        "the reply arrived in {elapsed:?}, faster than the configured {delay:?}"
    );
    assert_eq!(
        server.message_count().await,
        1,
        "a delayed delivery is still a delivery"
    );
}

#[tokio::test]
async fn connection_drop_closes_without_a_reply() {
    let server = TestServer::start().await;
    server.catch_all().await;
    server
        .faults
        .add(FaultRule::always(FaultMode::ConnectionDrop));

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<sender@test.example>").await;
    client.send("RCPT TO:<to@test.example>").await;

    client.send("DATA").await;
    // The client sees the socket close mid-conversation: no reply at all,
    // which a read returns as empty rather than as a status line.
    let reply = client.send_data(MESSAGE).await;
    assert!(
        reply.trim().is_empty(),
        "a dropped connection must send no reply, got: {reply}"
    );
    assert_eq!(server.message_count().await, 0);
}

#[tokio::test]
async fn a_sender_selector_spares_other_senders() {
    let server = TestServer::start().await;
    server.catch_all().await;
    server.faults.add(FaultRule {
        mode: FaultMode::RejectAtData,
        selector: FaultMatch {
            recipient: None,
            sender: Some(String::from("blocked@test.example")),
        },
    });

    // The named sender is refused.
    let mut blocked = server.connect().await;
    blocked.send("EHLO localhost").await;
    let refused = blocked
        .deliver("blocked@test.example", &["to@test.example"], MESSAGE)
        .await;
    assert_eq!(code_of(&refused), 554, "named sender refused: {refused}");

    // Anyone else is unaffected, on the same running server.
    let mut allowed = server.connect().await;
    allowed.send("EHLO localhost").await;
    let delivered = allowed
        .deliver("other@test.example", &["to@test.example"], MESSAGE)
        .await;
    assert_eq!(
        code_of(&delivered),
        250,
        "an unnamed sender must be unaffected: {delivered}"
    );
}

#[tokio::test]
async fn clearing_faults_restores_delivery_without_a_restart() {
    // The whole reason faults are runtime state rather than environment
    // configuration (ADR-084): the same process, the same listener, a
    // different answer.
    let server = TestServer::start().await;
    server.catch_all().await;
    server
        .faults
        .add(FaultRule::always(FaultMode::RejectAtData));

    let mut first = server.connect().await;
    first.send("EHLO localhost").await;
    let refused = first
        .deliver("sender@test.example", &["to@test.example"], MESSAGE)
        .await;
    assert_eq!(code_of(&refused), 554);

    assert_eq!(server.faults.clear(), 1);

    let mut second = server.connect().await;
    second.send("EHLO localhost").await;
    let delivered = second
        .deliver("sender@test.example", &["to@test.example"], MESSAGE)
        .await;
    assert_eq!(
        code_of(&delivered),
        250,
        "clearing must take effect with no restart: {delivered}"
    );
    assert_eq!(server.message_count().await, 1);
}

#[tokio::test]
async fn a_fault_added_mid_session_applies_to_the_next_command() {
    // The registry is shared, not snapshotted at session start.
    let server = TestServer::start().await;
    server.catch_all().await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<sender@test.example>").await;

    let before = client.send("RCPT TO:<first@test.example>").await;
    assert_eq!(code_of(&before), 250, "no fault configured yet: {before}");

    server
        .faults
        .add(FaultRule::always(FaultMode::RejectAtRcpt));

    let after = client.send("RCPT TO:<second@test.example>").await;
    assert_eq!(
        code_of(&after),
        550,
        "a rule added mid-session must apply immediately: {after}"
    );
}
