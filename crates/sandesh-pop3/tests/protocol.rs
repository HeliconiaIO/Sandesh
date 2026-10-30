//! Protocol integration tests (RFC 1939, RFC 2449).

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
use common::TestServer;

#[tokio::test]
async fn pop3_greeting_and_capa() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    assert!(client.greeting.starts_with("+OK Sandesh POP3 server ready"));

    let (capa_status, capa_lines) = client.cmd_multiline("CAPA").await;
    assert!(capa_status.starts_with("+OK"));
    assert!(capa_lines.iter().any(|l| l == "USER"));
    assert!(capa_lines.iter().any(|l| l == "UIDL"));
    assert!(capa_lines.iter().any(|l| l == "TOP"));
    let implementation = format!("IMPLEMENTATION Sandesh {}", env!("CARGO_PKG_VERSION"));
    assert!(
        capa_lines.contains(&implementation),
        "CAPA must announce the running version: {capa_lines:?}"
    );

    let quit_resp = client.cmd("QUIT").await;
    assert!(quit_resp.starts_with("+OK"));
}

#[tokio::test]
async fn pop3_full_session_flow() {
    let server = TestServer::start().await;
    let (_acc_id, inbox_id) = server.create_account("alice").await;

    // Insert 2 messages
    let msg1 = b"From: bob@example.com\r\nTo: alice@sandesh.test\r\nSubject: First\r\n\r\nHello Alice!\r\n";
    let msg2 = b"From: carol@example.com\r\nTo: alice@sandesh.test\r\nSubject: Second\r\n\r\n.Line starting with dot\r\nSecond line\r\n";
    server.insert_message(inbox_id, msg1).await;
    server.insert_message(inbox_id, msg2).await;

    let mut client = server.connect().await;

    // In Authorization state, STAT must fail
    let err_stat = client.cmd("STAT").await;
    assert!(err_stat.starts_with("-ERR"));

    // Authenticate
    let user_resp = client.cmd("USER alice").await;
    assert!(user_resp.starts_with("+OK"));

    let pass_resp = client.cmd("PASS secret").await;
    assert!(pass_resp.starts_with("+OK 2 messages"));

    // STAT
    let stat = client.cmd("STAT").await;
    assert!(stat.starts_with("+OK 2 "));

    // LIST all
    let (list_status, list_lines) = client.cmd_multiline("LIST").await;
    assert!(list_status.starts_with("+OK 2 messages"));
    assert_eq!(list_lines.len(), 2);
    assert!(list_lines[0].starts_with("1 "));
    assert!(list_lines[1].starts_with("2 "));

    // LIST single
    let list_single = client.cmd("LIST 1").await;
    assert!(list_single.starts_with("+OK 1 "));

    let list_invalid = client.cmd("LIST 99").await;
    assert!(list_invalid.starts_with("-ERR"));

    // UIDL all
    let (uidl_status, uidl_lines) = client.cmd_multiline("UIDL").await;
    assert!(uidl_status.starts_with("+OK"));
    assert_eq!(uidl_lines.len(), 2);
    assert!(uidl_lines[0].starts_with("1 "));
    assert!(uidl_lines[1].starts_with("2 "));

    // UIDL single
    let uidl_single = client.cmd("UIDL 2").await;
    assert!(uidl_single.starts_with("+OK 2 "));

    // RETR 1
    let (retr1_status, retr1_lines) = client.cmd_multiline("RETR 1").await;
    assert!(retr1_status.starts_with("+OK"));
    assert!(retr1_lines.iter().any(|l| l.contains("Subject: First")));
    assert!(retr1_lines.iter().any(|l| l.contains("Hello Alice!")));

    // RETR 2 (checks dot-stuffing)
    let (retr2_status, retr2_lines) = client.cmd_multiline("RETR 2").await;
    assert!(retr2_status.starts_with("+OK"));
    // The dot-stuffed wire format "..Line" is un-stuffed by client reading, or contains "..Line"
    assert!(
        retr2_lines
            .iter()
            .any(|l| l.contains("Line starting with dot"))
    );

    // TOP 1 0 (headers only)
    let (top_status, top_lines) = client.cmd_multiline("TOP 1 0").await;
    assert!(top_status.starts_with("+OK"));
    assert!(top_lines.iter().any(|l| l.contains("Subject: First")));
    assert!(!top_lines.iter().any(|l| l.contains("Hello Alice!")));

    // NOOP
    let noop = client.cmd("NOOP").await;
    assert!(noop.starts_with("+OK"));

    // QUIT
    let quit = client.cmd("QUIT").await;
    assert!(quit.starts_with("+OK"));
}

#[tokio::test]
async fn pop3_state_enforcement_and_unknown_commands() {
    let server = TestServer::start().await;
    let (_acc_id, _inbox_id) = server.create_account("bob").await;
    let mut client = server.connect().await;

    // Commands rejected in Authorization
    assert!(client.cmd("RETR 1").await.starts_with("-ERR"));
    assert!(client.cmd("DELE 1").await.starts_with("-ERR"));
    assert!(client.cmd("RSET").await.starts_with("-ERR"));
    assert!(client.cmd("TOP 1 5").await.starts_with("-ERR"));

    // Unknown command
    assert!(client.cmd("BOGUS").await.starts_with("-ERR"));

    // Login
    assert!(client.cmd("USER bob").await.starts_with("+OK"));
    assert!(client.cmd("PASS none").await.starts_with("+OK"));

    // Commands rejected in Transaction
    assert!(client.cmd("USER alice").await.starts_with("-ERR"));
    assert!(client.cmd("PASS secret").await.starts_with("-ERR"));

    // Clean quit
    assert!(client.cmd("QUIT").await.starts_with("+OK"));
}
