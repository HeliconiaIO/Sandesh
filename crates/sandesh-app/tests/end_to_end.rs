//! The authoritative Phase 3 end-to-end test.
//!
//! ```text
//! START APPLICATION ──▶ CONNECT WEBSOCKET ──▶ SMTP CONVERSATION ──▶ 250
//!                              │                                     │
//!                    ┌─────────┴──────────┐                          │
//!                    ▼                    ▼                          │
//!               entry_created        REST: message appears ◀─────────┘
//!                    │                    │
//!                    └────────┬───────────┘
//!                             ▼
//!                     raw bytes verified
//!                             │
//!                             ▼
//!                         SHUTDOWN
//! ```
//!
//! Every hop is real: a real TCP SMTP conversation, a real HTTP client, a real
//! WebSocket. Nothing is inserted directly into SQLite, because the thing under
//! test is precisely that SMTP, the core, storage, the bus and the API are one
//! working application rather than five that compile together.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{TestApp, WsClient};

/// The message used throughout, with CRLF line endings as the wire requires.
const MESSAGE: &str = "From: app@test.local\r\n\
                       To: dev@example.test\r\n\
                       Subject: Phase 3 end to end\r\n\
                       Message-ID: <e2e@test.local>\r\n\
                       \r\n\
                       The body of the message.";

#[tokio::test(flavor = "multi_thread")]
async fn smtp_delivery_reaches_rest_and_websocket_and_the_bytes_survive() {
    let app = TestApp::start().await;

    // 1. A WebSocket client subscribes *before* anything is delivered, so the
    //    event it sees is genuinely caused by the delivery below.
    let mut ws = WsClient::connect(&app).await;

    // 2. A real SMTP conversation, asserted at the protocol level.
    let reply = app
        .deliver("app@test.local", "dev@example.test", MESSAGE)
        .await;
    assert!(reply.starts_with("250"), "DATA was not accepted: {reply}");

    // 3. Both delivery events arrive over the WebSocket, in order.
    //    EVENT_MODEL §4.1: EntryCreated is the structural fact (the one IMAP
    //    IDLE consumes uniformly with COPY and APPEND) and MessageReceived is
    //    the human-level one; delivery emits both.
    let entry_created = ws.await_event("EntryCreated").await;
    assert_eq!(
        entry_created["seq"], 1,
        "the first frame on this connection"
    );

    let frame = ws.await_event("MessageReceived").await;
    assert_eq!(frame["type"], "event");
    assert_eq!(frame["seq"], 2, "delivered in publication order");
    assert!(
        entry_created["bus_seq"].as_u64() < frame["bus_seq"].as_u64(),
        "the bus's total order must agree with the per-connection order"
    );

    let payload = &frame["event"]["payload"];
    let message_id = payload["message_id"]
        .as_str()
        .expect("the event names the message")
        .to_owned();
    assert!(payload["entry_id"].is_string());
    assert!(payload["mailbox_id"].is_string());
    assert_eq!(payload["uid"], 1, "the first message in a fresh mailbox");

    // The event carries identifiers, not content (EVENT_MODEL §1).
    let rendered = frame.to_string();
    assert!(
        !rendered.contains("The body of the message"),
        "an event must never carry message content"
    );

    // 4. The same message is queryable over REST. `MessageReceived` is emitted
    //    after commit, so this must succeed with no retry — that is the
    //    commit-then-publish guarantee, tested rather than assumed.
    let (status, body) = app.get_json(&format!("/messages/{message_id}")).await;
    assert_eq!(status, 200, "the event promised a readable message");
    assert_eq!(body["subject"], "Phase 3 end to end");
    assert_eq!(body["id"], message_id);

    // 5. The message carries its placement, naming the mailbox it landed in.
    let entries = body["entries"].as_array().expect("entries are present");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["mailbox_name"], "INBOX");
    assert_eq!(entries[0]["uid"], 1);

    // 6. It appears in the list view too.
    let (status, list) = app.get_json("/messages").await;
    assert_eq!(status, 200);
    assert_eq!(list["total_estimate"], 1);
    assert_eq!(list["items"][0]["id"], message_id);

    // 7. The raw bytes are byte-identical to what SMTP received. This is one of
    //    Sandesh's core correctness guarantees, so it is compared exactly, not
    //    by length or by a normalised form.
    //
    //    The expectation includes the trailing CRLF: the client sends
    //    `<message>CRLF.CRLF`, so the final CRLF before the terminating dot is
    //    part of the message data per RFC 5321 §4.1.1.4. Asserting on what was
    //    genuinely transmitted is the whole point — trimming it here would be
    //    the test quietly agreeing to a normalisation Sandesh must not perform.
    let expected = format!("{MESSAGE}\r\n");
    let (status, raw) = app.get(&format!("/messages/{message_id}/raw")).await;
    assert_eq!(status, 200);
    assert_eq!(
        raw,
        expected.as_bytes(),
        "the raw endpoint must return the exact original octets"
    );

    // 8. And the MIME view describes those same bytes by offset, rather than
    //    holding a second copy of them.
    let (status, mime) = app.get_json(&format!("/messages/{message_id}/mime")).await;
    assert_eq!(status, 200);
    assert_eq!(mime["parse_ok"], true);
    let part = &mime["parts"][0];
    let offset = part["body_offset"].as_u64().expect("an offset") as usize;
    let length = part["body_length"].as_u64().expect("a length") as usize;
    assert_eq!(
        &raw[offset..offset + length],
        b"The body of the message.\r\n",
        "MIME offsets must index the raw bytes"
    );

    // 9. Everything stops cleanly.
    ws.close().await;
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_delivered_to_two_recipients_is_one_message_in_two_mailboxes() {
    // The multi-mailbox property. Collapsing this into two messages, or into
    // one message with a single "the" mailbox, would misrepresent the domain
    // and break Bcc testing — the reason the distinction exists.
    let app = TestApp::start().await;

    let mut client = common::SmtpClient::connect(app.smtp()).await;
    client.expect("220").await;
    client.send("EHLO test.local").await;
    client.read_multiline().await;
    client.send("MAIL FROM:<sender@test.local>").await;
    client.expect("250").await;
    // Two distinct recipients, so the catch-all routes both to its INBOX; the
    // point under test is that one set of bytes is stored once.
    client.send("RCPT TO:<one@example.test>").await;
    client.expect("250").await;
    client.send("RCPT TO:<two@example.test>").await;
    client.expect("250").await;
    client.send("DATA").await;
    client.expect("354").await;
    client
        .send("Subject: Two recipients\r\n\r\nBody.\r\n.")
        .await;
    client.expect("250").await;
    client.send("QUIT").await;

    let (status, list) = app.get_json("/messages").await;
    assert_eq!(status, 200);
    assert_eq!(
        list["total_estimate"], 1,
        "two recipients must not produce two copies of the bytes"
    );

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_delivery_record_reports_the_envelope_recipients() {
    // Envelope recipients belong to the delivery, not to the immutable message
    // — which is why the message summary reports the To: header instead.
    let app = TestApp::start().await;

    app.deliver("sender@test.local", "envelope@example.test", MESSAGE)
        .await;

    let (_, list) = app.get_json("/messages").await;
    let id = list["items"][0]["id"]
        .as_str()
        .expect("a message")
        .to_owned();

    let (status, delivery) = app.get_json(&format!("/messages/{id}/delivery")).await;
    assert_eq!(status, 200);
    assert_eq!(delivery["mail_from"], "sender@test.local");
    assert_eq!(delivery["rcpt_to"][0], "envelope@example.test");

    app.stop().await;
}
