//! POP3 deletion semantics (RFC 1939 §6) and snapshot isolation (ADR-006).

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
use common::TestServer;
use sandesh_core::MessageStore;

#[tokio::test]
async fn pop3_rset_unmarks_deleted_messages() {
    let server = TestServer::start().await;
    let (_acc_id, inbox_id) = server.create_account("alice").await;

    server
        .insert_message(inbox_id, b"From: a@b.c\r\nSubject: 1\r\n\r\nBody 1\r\n")
        .await;
    server
        .insert_message(inbox_id, b"From: a@b.c\r\nSubject: 2\r\n\r\nBody 2\r\n")
        .await;

    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    client.cmd("PASS pass").await;

    // Delete message 1
    let del1 = client.cmd("DELE 1").await;
    assert!(del1.starts_with("+OK"));

    // STAT should report 1 message left
    let stat = client.cmd("STAT").await;
    assert!(stat.starts_with("+OK 1 "));

    // LIST 1 should fail
    let list1 = client.cmd("LIST 1").await;
    assert!(list1.starts_with("-ERR"));

    // RSET restores all
    let rset = client.cmd("RSET").await;
    assert!(rset.starts_with("+OK 2 messages"));

    let stat_after = client.cmd("STAT").await;
    assert!(stat_after.starts_with("+OK 2 "));

    client.cmd("QUIT").await;

    // Both messages still in storage
    let entries = server.messages.list_entries(inbox_id).await.unwrap();
    assert_eq!(entries.len(), 2);
}

#[tokio::test]
async fn pop3_quit_commits_marked_deletions() {
    let server = TestServer::start().await;
    let (_acc_id, inbox_id) = server.create_account("alice").await;

    let msg1 = server
        .insert_message(inbox_id, b"From: a@b.c\r\nSubject: 1\r\n\r\nBody 1\r\n")
        .await;
    let msg2 = server
        .insert_message(inbox_id, b"From: a@b.c\r\nSubject: 2\r\n\r\nBody 2\r\n")
        .await;

    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    client.cmd("PASS pass").await;

    // Mark message 1 for deletion
    let del1 = client.cmd("DELE 1").await;
    assert!(del1.starts_with("+OK"));

    // QUIT commits deletion
    let quit = client.cmd("QUIT").await;
    assert!(quit.starts_with("+OK"));

    // Check storage: only message 2 should remain
    let entries = server.messages.list_entries(inbox_id).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, msg2.entries[0].id);

    // Entry 1 is gone
    let entry1 = server.messages.get_entry(msg1.entries[0].id).await.unwrap();
    assert!(entry1.is_none());
}

#[tokio::test]
async fn pop3_disconnect_without_quit_rolls_back_deletions() {
    let server = TestServer::start().await;
    let (_acc_id, inbox_id) = server.create_account("alice").await;

    let msg1 = server
        .insert_message(inbox_id, b"From: a@b.c\r\nSubject: 1\r\n\r\nBody 1\r\n")
        .await;
    let msg2 = server
        .insert_message(inbox_id, b"From: a@b.c\r\nSubject: 2\r\n\r\nBody 2\r\n")
        .await;

    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    client.cmd("PASS pass").await;

    // Mark both messages for deletion
    assert!(client.cmd("DELE 1").await.starts_with("+OK"));
    assert!(client.cmd("DELE 2").await.starts_with("+OK"));

    // Now drop client connection without sending QUIT
    drop(client);

    // Wait briefly for server session task to observe EOF
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Verify storage: both messages MUST still exist untouched!
    let entries = server.messages.list_entries(inbox_id).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, msg1.entries[0].id);
    assert_eq!(entries[1].id, msg2.entries[0].id);
}

#[tokio::test]
async fn pop3_snapshot_isolation_across_deliveries() {
    let server = TestServer::start().await;
    let (_acc_id, inbox_id) = server.create_account("alice").await;

    // Initially 2 messages
    server
        .insert_message(inbox_id, b"From: a@b.c\r\nSubject: 1\r\n\r\nBody 1\r\n")
        .await;
    server
        .insert_message(inbox_id, b"From: a@b.c\r\nSubject: 2\r\n\r\nBody 2\r\n")
        .await;

    // POP3 client connects and enters TRANSACTION state
    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    assert!(client.cmd("PASS pass").await.starts_with("+OK 2 messages"));

    // Mid-session, SMTP delivers a 3rd message into alice's INBOX
    server
        .insert_message(inbox_id, b"From: a@b.c\r\nSubject: 3\r\n\r\nBody 3\r\n")
        .await;

    // The active POP3 session MUST NOT see the 3rd message (ADR-006 snapshot isolation)
    let stat = client.cmd("STAT").await;
    assert!(
        stat.starts_with("+OK 2 "),
        "active session must still see 2 messages"
    );

    let (list_status, list_lines) = client.cmd_multiline("LIST").await;
    assert!(list_status.starts_with("+OK 2 messages"));
    assert_eq!(list_lines.len(), 2);
    assert_eq!(list_lines[0].split_whitespace().next().unwrap(), "1");
    assert_eq!(list_lines[1].split_whitespace().next().unwrap(), "2");

    // Attempting to access message 3 must fail
    assert!(client.cmd("RETR 3").await.starts_with("-ERR"));

    // Client quits cleanly
    client.cmd("QUIT").await;

    // A subsequent session sees all 3 messages!
    let mut client2 = server.connect().await;
    client2.cmd("USER alice").await;
    assert!(client2.cmd("PASS pass").await.starts_with("+OK 3 messages"));
    client2.cmd("QUIT").await;
}
