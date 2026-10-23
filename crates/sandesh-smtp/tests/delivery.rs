//! End-to-end SMTP delivery over real TCP.
//!
//! These are the Phase 2 acceptance tests: a client connects to a real socket,
//! speaks SMTP, and the message arrives in SQLite through the domain layer.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{TestServer, code_of, fixture};
use sandesh_core::{AccountStore, DeliveryStore, MessageStore};

#[tokio::test]
async fn full_transaction_stores_the_message() {
    // The §71 acceptance scenario, end to end.
    let server = TestServer::start().await;
    let bob = server.account("bob", "bob@example.test").await;

    let mut client = server.connect().await;
    assert_eq!(
        code_of(&client.greeting),
        220,
        "greeting: {}",
        client.greeting
    );

    let ehlo = client.send("EHLO localhost").await;
    assert_eq!(code_of(&ehlo), 250);

    assert_eq!(
        code_of(&client.send("MAIL FROM:<alice@example.test>").await),
        250
    );
    assert_eq!(
        code_of(&client.send("RCPT TO:<bob@example.test>").await),
        250
    );
    assert_eq!(code_of(&client.send("DATA").await), 354);

    let content = fixture("valid/plain-text.eml");
    let accepted = client.send_data(&content).await;
    assert_eq!(code_of(&accepted), 250, "delivery: {accepted}");

    assert_eq!(code_of(&client.send("QUIT").await), 221);

    // The message reached storage.
    assert_eq!(server.message_count().await, 1);
    let ids = server.stored_messages().await;
    let raw = server
        .messages
        .raw(ids[0])
        .await
        .expect("query")
        .expect("stored");

    assert_eq!(
        raw.bytes(),
        content.as_slice(),
        "stored bytes must be the message, not the SMTP framing"
    );

    // It landed in the recipient's INBOX.
    let inbox = server.inbox(bob).await;
    let entries = server.messages.list_entries(inbox).await.expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].uid.get(), 1);
}

#[tokio::test]
async fn multiple_recipients_share_one_stored_message() {
    // CORE_SPEC §7.2: one message, several placements, bytes stored once.
    let server = TestServer::start().await;
    let bob = server.account("bob", "bob@example.test").await;
    let carol = server.account("carol", "carol@example.test").await;
    let dave = server.account("dave", "dave@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    let reply = client
        .deliver(
            "alice@example.test",
            &[
                "bob@example.test",
                "carol@example.test",
                "dave@example.test",
            ],
            &fixture("valid/plain-text.eml"),
        )
        .await;
    assert_eq!(code_of(&reply), 250);

    assert_eq!(
        server.message_count().await,
        1,
        "three recipients must not duplicate the message"
    );
    assert_eq!(server.entry_count().await, 3, "one placement per recipient");

    for account in [bob, carol, dave] {
        let inbox = server.inbox(account).await;
        let entries = server.messages.list_entries(inbox).await.expect("entries");
        assert_eq!(entries.len(), 1, "each recipient gets a placement");
    }
}

#[tokio::test]
async fn all_recipients_are_preserved_on_the_delivery_record() {
    // Phase 1's OD-16: the envelope's full recipient list lives here.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    server.account("carol", "carol@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client
        .deliver(
            "alice@example.test",
            &["bob@example.test", "carol@example.test"],
            b"Subject: multi\r\n\r\nbody\r\n",
        )
        .await;

    let recent = server.deliveries.recent(10).await.expect("recent");
    assert_eq!(recent.len(), 1);

    let delivery = &recent[0];
    assert_eq!(
        delivery.envelope.rcpt_to.len(),
        2,
        "every RCPT TO must be retained"
    );
    assert_eq!(delivery.outcomes.len(), 2, "one outcome per recipient");
    assert_eq!(
        delivery
            .envelope
            .mail_from
            .as_ref()
            .map(sandesh_core::EmailAddress::normalized),
        Some(String::from("alice@example.test"))
    );
    // Each outcome names the placement it produced — the "why did it land
    // here?" answer.
    assert!(delivery.outcomes.iter().all(|o| o.entry_id.is_some()));
}

#[tokio::test]
async fn envelope_is_kept_separate_from_message_headers() {
    // The envelope and the headers legitimately disagree; both must survive.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO relay.example.test").await;

    // Envelope sender is a bounce address; the From: header says otherwise.
    let content = b"From: marketing@example.test\r\n\
To: someone-else@example.test\r\n\
Subject: divergent\r\n\
\r\n\
body\r\n";
    client
        .deliver("bounce@example.test", &["bob@example.test"], content)
        .await;

    let delivery = &server.deliveries.recent(1).await.expect("recent")[0];
    assert_eq!(
        delivery
            .envelope
            .mail_from
            .as_ref()
            .map(sandesh_core::EmailAddress::normalized),
        Some(String::from("bounce@example.test")),
        "the envelope sender is not the From: header"
    );
    assert_eq!(
        delivery.envelope.helo_domain.as_deref(),
        Some("relay.example.test")
    );

    let ids = server.stored_messages().await;
    let message = server
        .messages
        .get(ids[0])
        .await
        .expect("query")
        .expect("stored");
    assert_eq!(
        message
            .parsed
            .header("from")
            .map(|h| h.decoded_value.as_str()),
        Some("marketing@example.test"),
        "the header is preserved independently"
    );
}

#[tokio::test]
async fn two_recipients_in_one_mailbox_produce_one_placement() {
    // CORE_SPEC §7.3: idempotent placement.
    let server = TestServer::start().await;
    let bob = server.account("bob", "bob@example.test").await;
    // A second address routing to the same account.
    server
        .accounts
        .add_identity(
            bob,
            &sandesh_core::EmailAddress::parse_lenient("bob+tag@example.test"),
            false,
        )
        .await
        .expect("add alias");

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    let reply = client
        .deliver(
            "alice@example.test",
            &["bob@example.test", "bob+tag@example.test"],
            b"Subject: aliased\r\n\r\nbody\r\n",
        )
        .await;
    assert_eq!(code_of(&reply), 250);

    assert_eq!(server.entry_count().await, 1, "one mailbox, one placement");

    // Both recipients still appear on the delivery record.
    let delivery = &server.deliveries.recent(1).await.expect("recent")[0];
    assert_eq!(delivery.outcomes.len(), 2, "both RCPT TOs are recorded");
}

#[tokio::test]
async fn unknown_recipient_routes_to_the_catch_all() {
    // The default policy: a development server should not bounce.
    let server = TestServer::start().await;
    let catch_all = server.catch_all().await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    let reply = client
        .deliver(
            "alice@example.test",
            &["nobody@example.test"],
            b"Subject: stray\r\n\r\nbody\r\n",
        )
        .await;
    assert_eq!(code_of(&reply), 250, "unknown recipients are accepted");

    let inbox = server.inbox(catch_all).await;
    let entries = server.messages.list_entries(inbox).await.expect("entries");
    assert_eq!(entries.len(), 1, "the message lands in the catch-all");

    let delivery = &server.deliveries.recent(1).await.expect("recent")[0];
    assert_eq!(
        delivery.outcomes[0].outcome,
        sandesh_core::DeliveryOutcome::CatchAll,
        "the record explains why it landed there"
    );
}

#[tokio::test]
async fn null_reverse_path_is_accepted() {
    // Bounce testing depends on MAIL FROM:<> working.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    assert_eq!(code_of(&client.send("MAIL FROM:<>").await), 250);
    assert_eq!(
        code_of(&client.send("RCPT TO:<bob@example.test>").await),
        250
    );
    assert_eq!(code_of(&client.send("DATA").await), 354);
    assert_eq!(
        code_of(&client.send_data(b"Subject: bounce\r\n\r\nx\r\n").await),
        250
    );

    let delivery = &server.deliveries.recent(1).await.expect("recent")[0];
    assert!(
        delivery.envelope.mail_from.is_none(),
        "the null sender must round-trip as None"
    );
}

#[tokio::test]
async fn one_connection_carries_several_transactions() {
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;

    for n in 1..=3 {
        let content = format!("Subject: message {n}\r\n\r\nbody {n}\r\n");
        let reply = client
            .deliver(
                "alice@example.test",
                &["bob@example.test"],
                content.as_bytes(),
            )
            .await;
        assert_eq!(code_of(&reply), 250, "transaction {n}: {reply}");
    }

    assert_eq!(code_of(&client.send("QUIT").await), 221);
    assert_eq!(server.message_count().await, 3);

    // Each got its own UID, in order.
    let bob_inbox = server
        .inbox(
            server
                .accounts
                .get_by_username("bob")
                .await
                .expect("query")
                .expect("account")
                .id,
        )
        .await;
    let uids: Vec<u32> = server
        .messages
        .list_entries(bob_inbox)
        .await
        .expect("entries")
        .iter()
        .map(|e| e.uid.get())
        .collect();
    assert_eq!(uids, vec![1, 2, 3]);
}

#[tokio::test]
async fn transaction_state_resets_after_delivery() {
    // After DATA the session returns to Ready: a bare RCPT must now fail.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client
        .deliver(
            "alice@example.test",
            &["bob@example.test"],
            b"Subject: one\r\n\r\nx\r\n",
        )
        .await;

    let stray = client.send("RCPT TO:<bob@example.test>").await;
    assert_eq!(
        code_of(&stray),
        503,
        "the previous transaction must not persist: {stray}"
    );
}

#[tokio::test]
async fn rset_discards_the_transaction() {
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    client.send("MAIL FROM:<alice@example.test>").await;
    client.send("RCPT TO:<bob@example.test>").await;

    assert_eq!(code_of(&client.send("RSET").await), 250);

    // DATA must now be refused: the transaction is gone.
    let data = client.send("DATA").await;
    assert_eq!(code_of(&data), 503, "RSET must discard recipients: {data}");

    // And a fresh transaction still works.
    let reply = client
        .deliver(
            "alice@example.test",
            &["bob@example.test"],
            b"Subject: after\r\n\r\nx\r\n",
        )
        .await;
    assert_eq!(code_of(&reply), 250);
    assert_eq!(
        server.message_count().await,
        1,
        "only the second was stored"
    );
}

#[tokio::test]
async fn a_message_arriving_for_no_valid_mailbox_is_still_accepted() {
    // No catch-all configured: the message is stored with no placement rather
    // than silently vanishing after a 250.
    let server = TestServer::start().await;

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    let reply = client
        .deliver(
            "alice@example.test",
            &["nobody@example.test"],
            b"Subject: orphan\r\n\r\nx\r\n",
        )
        .await;

    assert_eq!(code_of(&reply), 250);
    assert_eq!(server.message_count().await, 1, "the bytes are kept");
    assert_eq!(server.entry_count().await, 0, "but nothing is placed");

    let delivery = &server.deliveries.recent(1).await.expect("recent")[0];
    assert_eq!(
        delivery.outcomes[0].outcome,
        sandesh_core::DeliveryOutcome::Rejected,
        "the record says why"
    );
}
