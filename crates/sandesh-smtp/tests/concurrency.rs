//! Concurrency, session isolation, and TCP framing independence.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{TestServer, code_of};
use sandesh_config::SmtpConfig;
use sandesh_core::{AccountStore, MessageStore};
use std::collections::BTreeSet;

#[tokio::test]
async fn concurrent_sessions_do_not_interfere() {
    // Two sessions open at once; neither may see the other's transaction.
    let server = TestServer::start().await;
    server.account("alpha", "alpha@example.test").await;
    server.account("beta", "beta@example.test").await;

    let mut a = server.connect().await;
    let mut b = server.connect().await;

    a.send("EHLO session-a").await;
    b.send("EHLO session-b").await;

    // Interleave the two transactions deliberately.
    a.send("MAIL FROM:<from-a@example.test>").await;
    b.send("MAIL FROM:<from-b@example.test>").await;

    a.send("RCPT TO:<alpha@example.test>").await;
    b.send("RCPT TO:<beta@example.test>").await;

    a.send("DATA").await;
    b.send("DATA").await;

    let reply_a = a.send_data(b"Subject: from A\r\n\r\nbody A\r\n").await;
    let reply_b = b.send_data(b"Subject: from B\r\n\r\nbody B\r\n").await;

    assert_eq!(code_of(&reply_a), 250);
    assert_eq!(code_of(&reply_b), 250);

    // Each landed in exactly its own mailbox.
    for (username, subject) in [("alpha", "from A"), ("beta", "from B")] {
        let account = server
            .accounts
            .get_by_username(username)
            .await
            .expect("query")
            .expect("account");
        let inbox = server.inbox(account.id).await;
        let entries = server.messages.list_entries(inbox).await.expect("entries");

        assert_eq!(entries.len(), 1, "{username} received exactly one message");
        let message = server
            .messages
            .get(entries[0].message_id)
            .await
            .expect("query")
            .expect("stored");
        assert_eq!(
            message.subject(),
            Some(subject),
            "{username} received the wrong message — sessions leaked"
        );
    }
}

#[tokio::test]
async fn one_session_resetting_does_not_affect_another() {
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;

    let mut a = server.connect().await;
    let mut b = server.connect().await;
    a.send("EHLO a").await;
    b.send("EHLO b").await;

    // B builds a transaction; A resets its own.
    b.send("MAIL FROM:<x@example.test>").await;
    b.send("RCPT TO:<bob@example.test>").await;

    a.send("MAIL FROM:<y@example.test>").await;
    assert_eq!(code_of(&a.send("RSET").await), 250);

    // B's transaction must be untouched.
    assert_eq!(
        code_of(&b.send("DATA").await),
        354,
        "B's state survived A's RSET"
    );
    assert_eq!(code_of(&b.send_data(b"Subject: b\r\n\r\nx\r\n").await), 250);
}

#[tokio::test]
async fn many_concurrent_deliveries_produce_distinct_uids() {
    // The UID race is the one that would corrupt client state.
    let server = TestServer::start().await;
    let bob = server.account("bob", "bob@example.test").await;

    const SESSIONS: usize = 20;
    let mut tasks = Vec::with_capacity(SESSIONS);

    for n in 0..SESSIONS {
        let addr = server.addr;
        tasks.push(tokio::spawn(async move {
            let mut client = common::SmtpClient::connect(addr).await;
            client.send("EHLO concurrent").await;
            let content = format!("Subject: concurrent {n}\r\n\r\nbody {n}\r\n");
            let reply = client
                .deliver("a@example.test", &["bob@example.test"], content.as_bytes())
                .await;
            client.send("QUIT").await;
            reply
        }));
    }

    for task in tasks {
        let reply = task.await.expect("task");
        assert_eq!(code_of(&reply), 250, "concurrent delivery failed: {reply}");
    }

    let inbox = server.inbox(bob).await;
    let entries = server.messages.list_entries(inbox).await.expect("entries");
    assert_eq!(entries.len(), SESSIONS);

    let uids: BTreeSet<u32> = entries.iter().map(|e| e.uid.get()).collect();
    assert_eq!(uids.len(), SESSIONS, "UIDs must be unique");
    assert_eq!(
        uids.into_iter().collect::<Vec<_>>(),
        (1..=SESSIONS as u32).collect::<Vec<_>>(),
        "UIDs must be contiguous"
    );
}

#[tokio::test]
async fn recipients_do_not_mix_across_concurrent_sessions() {
    let server = TestServer::start().await;
    for name in ["one", "two", "three", "four"] {
        server.account(name, &format!("{name}@example.test")).await;
    }

    let addr = server.addr;
    let mut tasks = Vec::new();
    for name in ["one", "two", "three", "four"] {
        tasks.push(tokio::spawn(async move {
            let mut client = common::SmtpClient::connect(addr).await;
            client.send("EHLO isolation").await;
            let content = format!("Subject: for {name}\r\n\r\nbody\r\n");
            client
                .deliver(
                    "sender@example.test",
                    &[&format!("{name}@example.test")],
                    content.as_bytes(),
                )
                .await
        }));
    }
    for task in tasks {
        assert_eq!(code_of(&task.await.expect("task")), 250);
    }

    // Every account received exactly its own message.
    for name in ["one", "two", "three", "four"] {
        let account = server
            .accounts
            .get_by_username(name)
            .await
            .expect("query")
            .expect("account");
        let inbox = server.inbox(account.id).await;
        let entries = server.messages.list_entries(inbox).await.expect("entries");
        assert_eq!(entries.len(), 1, "{name} got the wrong number of messages");

        let message = server
            .messages
            .get(entries[0].message_id)
            .await
            .expect("query")
            .expect("stored");
        assert_eq!(message.subject(), Some(format!("for {name}").as_str()));
    }
}

#[tokio::test]
async fn commands_split_across_packets_are_handled() {
    // TCP is a byte stream: packet boundaries must be irrelevant.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;

    // Split "EHLO localhost\r\n" mid-verb and mid-CRLF.
    client.write_raw(b"EH").await;
    client.write_raw(b"LO loc").await;
    client.write_raw(b"alhost\r").await;
    client.write_raw(b"\n").await;

    let ehlo = client.read_reply().await;
    assert_eq!(code_of(&ehlo), 250, "fragmented EHLO: {ehlo}");

    // Split the address too.
    client.write_raw(b"MAIL FROM:<ali").await;
    client.write_raw(b"ce@example.test>\r\n").await;
    assert_eq!(code_of(&client.read_reply().await), 250);

    client.write_raw(b"RCPT TO:<bob@example.test>\r\n").await;
    assert_eq!(code_of(&client.read_reply().await), 250);
    client.write_raw(b"DATA\r\n").await;
    assert_eq!(code_of(&client.read_reply().await), 354);

    // Content fragmented mid-line as well.
    client.write_raw(b"Subject: frag").await;
    client.write_raw(b"mented\r\n\r\nbo").await;
    client.write_raw(b"dy\r\n.\r\n").await;

    let accepted = client.read_reply().await;
    assert_eq!(code_of(&accepted), 250, "fragmented DATA: {accepted}");

    let ids = server.stored_messages().await;
    let raw = server
        .messages
        .raw(ids[0])
        .await
        .expect("query")
        .expect("stored");
    assert_eq!(
        raw.bytes(),
        b"Subject: fragmented\r\n\r\nbody\r\n",
        "fragmentation must not affect stored content"
    );
}

#[tokio::test]
async fn several_commands_in_one_packet_are_handled() {
    // Pipelining is advertised, so batched commands must work — and this is
    // also required regardless, since a client may batch without negotiating.
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let mut client = server.connect().await;

    // The whole transaction in a single write.
    client
        .write_raw(
            b"EHLO localhost\r\n\
MAIL FROM:<alice@example.test>\r\n\
RCPT TO:<bob@example.test>\r\n\
DATA\r\n",
        )
        .await;

    // Four replies, in order.
    let ehlo = client.read_reply().await;
    assert_eq!(code_of(&ehlo), 250, "EHLO: {ehlo}");
    assert_eq!(code_of(&client.read_reply().await), 250, "MAIL");
    assert_eq!(code_of(&client.read_reply().await), 250, "RCPT");
    assert_eq!(code_of(&client.read_reply().await), 354, "DATA");

    client
        .write_raw(b"Subject: pipelined\r\n\r\nbody\r\n.\r\n")
        .await;
    assert_eq!(code_of(&client.read_reply().await), 250);

    assert_eq!(server.message_count().await, 1);
}

#[tokio::test]
async fn the_connection_limit_is_enforced() {
    let config = SmtpConfig {
        max_connections: 2,
        ..TestServer::base_config()
    };
    let server = TestServer::with_config(config).await;

    // Two connections should be served.
    let mut first = server.connect().await;
    let mut second = server.connect().await;
    assert_eq!(code_of(&first.greeting), 220);
    assert_eq!(code_of(&second.greeting), 220);

    // A third connects at the TCP level but is not served until a permit frees:
    // no greeting arrives while both are held.
    let third = tokio::net::TcpStream::connect(server.addr)
        .await
        .expect("tcp connect");
    let mut buf = [0u8; 4];
    let greeting = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        tokio::io::AsyncReadExt::read(&mut { third }, &mut buf),
    )
    .await;
    assert!(
        greeting.is_err(),
        "a third session must wait for a permit rather than being served"
    );

    // Releasing one lets the next through.
    first.send("QUIT").await;
    drop(first);

    let mut fourth = server.connect().await;
    assert_eq!(
        code_of(&fourth.greeting),
        220,
        "a freed permit admits the next client"
    );
    second.send("QUIT").await;
    fourth.send("QUIT").await;
}

#[tokio::test]
async fn abrupt_disconnect_mid_transaction_is_harmless() {
    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;

    {
        let mut client = server.connect().await;
        client.send("EHLO localhost").await;
        client.send("MAIL FROM:<a@example.test>").await;
        client.send("RCPT TO:<bob@example.test>").await;
        client.send("DATA").await;
        client
            .write_raw(b"Subject: incomplete\r\n\r\npartial")
            .await;
        // Drop without the terminator.
    }

    // Nothing was stored, and the server is still serving.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        server.message_count().await,
        0,
        "an aborted DATA stores nothing"
    );

    let mut client = server.connect().await;
    client.send("EHLO localhost").await;
    let reply = client
        .deliver(
            "a@example.test",
            &["bob@example.test"],
            b"Subject: after\r\n\r\nx\r\n",
        )
        .await;
    assert_eq!(code_of(&reply), 250, "the server survived the abort");
}
