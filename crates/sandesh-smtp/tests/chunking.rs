//! `BDAT`/CHUNKING (RFC 3030), Phase 8 A4 — ADR-088.
//!
//! The test that matters is [`the_two_paths_store_identical_bytes`]: a second
//! ingestion path is a second chance to corrupt content, and the Phase 2
//! raw-fidelity guarantee (ADR-003) is what is at risk. Asserting that `BDAT`
//! stores *something plausible* would pass while silently dot-unstuffing, which
//! must never happen on this path; comparing the two paths byte for byte
//! cannot.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{TestServer, code_of};
use sandesh_core::{DeliveryStore, MessageStore};

/// A message with content that framing bugs mangle: a leading-dot line, a bare
/// dot line, CRLFs throughout, and 8-bit bytes.
const TRICKY: &[u8] = b"Subject: chunking\r\n\
                        \r\n\
                        .hidden starts with a dot\r\n\
                        ..two dots\r\n\
                        normal line\r\n";

/// Reads back the raw bytes of the one stored message.
async fn stored_bytes(server: &TestServer) -> Vec<u8> {
    let ids = server.stored_messages().await;
    assert_eq!(ids.len(), 1, "expected exactly one stored message");
    server
        .messages
        .get(ids[0])
        .await
        .expect("query message")
        .expect("message exists")
        .raw
        .bytes()
        .to_vec()
}

/// Opens a transaction ready for content.
async fn begin(client: &mut common::SmtpClient) {
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<sender@test.example>").await;
    client.send("RCPT TO:<to@test.example>").await;
}

#[tokio::test]
async fn ehlo_advertises_chunking() {
    // The inverted half of `protocol.rs`'s absent-capability assertion
    // (ADR-088). Advertising is only honest because BDAT is implemented.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let ehlo = client.send("EHLO localhost").await;
    assert!(ehlo.contains("CHUNKING"), "CHUNKING missing from: {ehlo}");
}

#[tokio::test]
async fn a_single_chunk_delivers_the_message() {
    let server = TestServer::start().await;
    server.catch_all().await;
    let mut client = server.connect().await;
    begin(&mut client).await;

    let reply = client.bdat(TRICKY, true).await;
    assert_eq!(code_of(&reply), 250, "expected acceptance: {reply}");
    assert_eq!(server.message_count().await, 1);
}

#[tokio::test]
async fn the_two_paths_store_identical_bytes() {
    // The guarantee ADR-088 exists to protect. The same message sent by DATA
    // and by BDAT must land as the same bytes — anything else means one of the
    // two framings is corrupting content.
    let via_data = {
        let server = TestServer::start().await;
        server.catch_all().await;
        let mut client = server.connect().await;
        client.send("EHLO localhost").await;
        let reply = client
            .deliver("sender@test.example", &["to@test.example"], TRICKY)
            .await;
        assert_eq!(code_of(&reply), 250, "DATA delivery failed: {reply}");
        stored_bytes(&server).await
    };

    let via_bdat = {
        let server = TestServer::start().await;
        server.catch_all().await;
        let mut client = server.connect().await;
        begin(&mut client).await;
        let reply = client.bdat(TRICKY, true).await;
        assert_eq!(code_of(&reply), 250, "BDAT delivery failed: {reply}");
        stored_bytes(&server).await
    };

    assert_eq!(
        via_bdat,
        via_data,
        "BDAT and DATA must store identical bytes;\n  BDAT: {:?}\n  DATA: {:?}",
        String::from_utf8_lossy(&via_bdat),
        String::from_utf8_lossy(&via_data)
    );
    // And specifically: the stored form keeps the single leading dot the sender
    // wrote, on both paths.
    assert!(
        via_bdat.windows(8).any(|w| w == b".hidden "),
        "the leading-dot line must survive"
    );
}

#[tokio::test]
async fn several_chunks_concatenate_in_order() {
    let server = TestServer::start().await;
    server.catch_all().await;
    let mut client = server.connect().await;
    begin(&mut client).await;

    // Split mid-header and mid-body, so a reordering or an off-by-one in the
    // counted read shows up as corrupted content rather than a clean failure.
    for part in [&b"Subject: split"[..], &b" across chunks\r\n\r\nfirst "[..]] {
        let reply = client.bdat(part, false).await;
        assert_eq!(code_of(&reply), 250, "chunk refused: {reply}");
    }
    let reply = client.bdat(b"and second\r\n", true).await;
    assert_eq!(code_of(&reply), 250);

    assert_eq!(
        stored_bytes(&server).await,
        b"Subject: split across chunks\r\n\r\nfirst and second\r\n".to_vec()
    );
}

#[tokio::test]
async fn every_chunk_is_acknowledged_with_its_octet_count() {
    // RFC 3030 §2: each chunk is answered, which is what lets a client stream
    // without waiting for the transaction to finish.
    let server = TestServer::start().await;
    server.catch_all().await;
    let mut client = server.connect().await;
    begin(&mut client).await;

    let reply = client.bdat(b"Subject: x\r\n", false).await;
    assert_eq!(code_of(&reply), 250);
    assert!(
        reply.contains("12"),
        "the reply should echo the octet count: {reply}"
    );

    client.bdat(b"\r\nbody\r\n", true).await;
}

#[tokio::test]
async fn a_zero_length_last_chunk_terminates_the_message() {
    // `BDAT 0 LAST` is the idiomatic way to end a sequence whose content
    // already finished, and a real client will send it.
    let server = TestServer::start().await;
    server.catch_all().await;
    let mut client = server.connect().await;
    begin(&mut client).await;

    client.bdat(b"Subject: done\r\n\r\nbody\r\n", false).await;
    let reply = client.bdat(b"", true).await;

    assert_eq!(code_of(&reply), 250, "expected acceptance: {reply}");
    assert_eq!(
        stored_bytes(&server).await,
        b"Subject: done\r\n\r\nbody\r\n".to_vec()
    );
}

#[tokio::test]
async fn binary_content_survives_unchanged() {
    // The reason CHUNKING exists: no line scanning, so arbitrary bytes pass
    // through. Includes a bare LF and a lone CR, which DATA's framing treats
    // specially and this path must not.
    let server = TestServer::start().await;
    server.catch_all().await;
    let mut client = server.connect().await;
    begin(&mut client).await;

    let mut payload = b"Subject: binary\r\n\r\n".to_vec();
    payload.extend_from_slice(&[0x00, 0x01, 0xFF, 0xFE, b'\n', b'\r', 0x80]);

    let reply = client.bdat(&payload, true).await;
    assert_eq!(code_of(&reply), 250, "expected acceptance: {reply}");
    assert_eq!(stored_bytes(&server).await, payload);
}

#[tokio::test]
async fn data_after_bdat_is_refused() {
    // RFC 3030 §3: a transaction commits to one content mechanism. Merging the
    // framings would produce a message neither client meant to send.
    let server = TestServer::start().await;
    server.catch_all().await;
    let mut client = server.connect().await;
    begin(&mut client).await;

    client.bdat(b"Subject: x\r\n", false).await;
    let reply = client.send("DATA").await;

    assert_eq!(code_of(&reply), 503, "expected a sequence error: {reply}");
    assert_eq!(server.message_count().await, 0);
}

#[tokio::test]
async fn bdat_before_a_recipient_is_refused() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    // No MAIL FROM at all.
    let reply = client.send("BDAT 5").await;
    assert_eq!(code_of(&reply), 503, "expected a sequence error: {reply}");

    // A sender but no recipient.
    client.send("MAIL FROM:<sender@test.example>").await;
    let reply = client.send("BDAT 5").await;
    assert_eq!(code_of(&reply), 503, "expected a sequence error: {reply}");
}

#[tokio::test]
async fn a_malformed_bdat_is_a_syntax_error_not_a_read() {
    // The dangerous failure: treating an unparseable size as a default would
    // mean reading an unknown number of octets as commands.
    let server = TestServer::start().await;
    server.catch_all().await;
    let mut client = server.connect().await;
    begin(&mut client).await;

    for bad in [
        "BDAT",
        "BDAT abc",
        "BDAT -1",
        "BDAT 5 SOON",
        "BDAT 5 LAST EXTRA",
    ] {
        let reply = client.send(bad).await;
        assert_eq!(
            code_of(&reply),
            501,
            "{bad:?} should be a syntax error: {reply}"
        );
    }

    // The session is still usable: a syntax error must not desynchronise it.
    let reply = client.bdat(b"Subject: fine\r\n", true).await;
    assert_eq!(
        code_of(&reply),
        250,
        "session unusable after a bad BDAT: {reply}"
    );
}

#[tokio::test]
async fn an_oversize_chunked_message_is_refused_and_recorded() {
    // Matches what DATA already does for 552: read to the end so the
    // connection stays synchronised, then answer.
    let config = sandesh_config::SmtpConfig {
        max_message_size: 64,
        ..TestServer::base_config()
    };
    let server = TestServer::with_config(config).await;
    server.catch_all().await;
    let mut client = server.connect().await;
    begin(&mut client).await;

    let payload = vec![b'x'; 200];
    let reply = client.bdat(&payload, true).await;

    assert_eq!(code_of(&reply), 552, "expected a size refusal: {reply}");
    assert_eq!(server.message_count().await, 0, "nothing may be stored");

    // The refusal is explainable: "the message never arrived" is the question
    // the Delivery view exists to answer.
    let deliveries = server
        .deliveries
        .recent(10)
        .await
        .expect("query deliveries");
    assert_eq!(deliveries.len(), 1, "the rejection must be recorded");
    assert!(deliveries[0].message_id.is_none());

    // And the session survives it.
    let reply = client.send("NOOP").await;
    assert_eq!(code_of(&reply), 250, "session unusable after 552: {reply}");
}

#[tokio::test]
async fn rset_discards_accumulated_chunks() {
    let server = TestServer::start().await;
    server.catch_all().await;
    let mut client = server.connect().await;
    begin(&mut client).await;

    client.bdat(b"Subject: abandoned\r\n", false).await;
    let reply = client.send("RSET").await;
    assert_eq!(code_of(&reply), 250);

    // A fresh transaction must not inherit the abandoned chunk.
    client.send("MAIL FROM:<sender@test.example>").await;
    client.send("RCPT TO:<to@test.example>").await;
    client.bdat(b"Subject: kept\r\n", true).await;

    assert_eq!(
        stored_bytes(&server).await,
        b"Subject: kept\r\n".to_vec(),
        "the discarded chunk must not appear"
    );
}
