//! Interoperability with an independent SMTP client implementation.
//!
//! Every other test drives the server with our own client code, so a shared
//! misreading of the specification would pass unnoticed. Python's `smtplib` is
//! a mature, independent implementation: if it can deliver mail to Sandesh, the
//! server speaks SMTP rather than merely speaking to itself.
//!
//! Skipped, not failed, when Python is unavailable — a missing interpreter is
//! an environment fact, not a defect in the server.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::MessageStore;
use std::process::Command;

/// Whether a usable Python 3 with `smtplib` is present.
fn python_available() -> bool {
    Command::new("python3")
        .args(["-c", "import smtplib, email"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Runs a Python script against the server, returning stdout.
///
/// `python3` blocks, so callers must invoke this from `spawn_blocking`: holding
/// a runtime worker while the client waits for a greeting would deadlock the
/// server that is meant to send it.
fn run_python(script: &str) -> Result<String, String> {
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .output()
        .map_err(|e| format!("failed to run python3: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_smtplib_can_deliver() {
    if !python_available() {
        eprintln!("skipping: python3 with smtplib is not available");
        return;
    }

    let server = TestServer::start().await;
    let bob = server.account("bob", "bob@example.test").await;
    let port = server.addr.port();

    // smtplib drives the whole conversation itself: EHLO, capability parsing,
    // MAIL/RCPT/DATA, dot-stuffing and the terminator.
    let script = format!(
        r#"
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"] = "alice@example.test"
msg["To"] = "bob@example.test"
msg["Subject"] = "Sent by smtplib"
msg.set_content("Body written by an independent client.\nSecond line.")

with smtplib.SMTP("127.0.0.1", {port}, timeout=10) as smtp:
    smtp.ehlo("python-client.test")
    print("CAPS:" + ",".join(sorted(smtp.esmtp_features.keys())))
    smtp.send_message(msg)
print("SENT")
"#
    );

    let stdout = match tokio::task::spawn_blocking(move || run_python(&script))
        .await
        .expect("python task")
    {
        Ok(out) => out,
        Err(e) => panic!("smtplib failed to deliver: {e}"),
    };
    assert!(
        stdout.contains("SENT"),
        "delivery did not complete: {stdout}"
    );

    // smtplib parsed our EHLO response and found the capabilities.
    assert!(
        stdout.contains("size") && stdout.contains("8bitmime"),
        "smtplib did not parse the capability list: {stdout}"
    );
    assert!(
        !stdout.contains("starttls") && !stdout.contains("auth"),
        "unimplemented capabilities must not be advertised: {stdout}"
    );

    // The message arrived intact.
    assert_eq!(server.message_count().await, 1);
    let inbox = server.inbox(bob).await;
    let entries = server.messages.list_entries(inbox).await.expect("entries");
    assert_eq!(entries.len(), 1);

    let message = server
        .messages
        .get(entries[0].message_id)
        .await
        .expect("query")
        .expect("stored");
    assert_eq!(message.subject(), Some("Sent by smtplib"));
    assert!(
        message.raw.bytes().windows(4).any(|w| w == b"Body"),
        "the body must be present in the stored bytes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_smtplib_handles_multiple_recipients() {
    if !python_available() {
        eprintln!("skipping: python3 with smtplib is not available");
        return;
    }

    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    server.account("carol", "carol@example.test").await;
    let port = server.addr.port();

    let script = format!(
        r#"
import smtplib

message = (
    "From: alice@example.test\r\n"
    "To: bob@example.test, carol@example.test\r\n"
    "Subject: Multi-recipient\r\n"
    "\r\n"
    "One message, two envelope recipients.\r\n"
)

with smtplib.SMTP("127.0.0.1", {port}, timeout=10) as smtp:
    refused = smtp.sendmail(
        "alice@example.test",
        ["bob@example.test", "carol@example.test"],
        message,
    )
    print("REFUSED:" + str(refused))
print("SENT")
"#
    );

    let stdout = match tokio::task::spawn_blocking(move || run_python(&script))
        .await
        .expect("python task")
    {
        Ok(out) => out,
        Err(e) => panic!("smtplib failed: {e}"),
    };
    assert!(stdout.contains("SENT"), "{stdout}");
    assert!(
        stdout.contains("REFUSED:{}"),
        "no recipient should be refused: {stdout}"
    );

    assert_eq!(server.message_count().await, 1, "one message");
    assert_eq!(server.entry_count().await, 2, "two placements");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_smtplib_sends_an_attachment_intact() {
    if !python_available() {
        eprintln!("skipping: python3 with smtplib is not available");
        return;
    }

    let server = TestServer::start().await;
    server.account("bob", "bob@example.test").await;
    let port = server.addr.port();

    // Binary content forces base64 and exercises dot-stuffing on real output.
    let script = format!(
        r#"
import smtplib
from email.message import EmailMessage

msg = EmailMessage()
msg["From"] = "alice@example.test"
msg["To"] = "bob@example.test"
msg["Subject"] = "With attachment"
msg.set_content("See attached.")
msg.add_attachment(
    bytes(range(256)),
    maintype="application",
    subtype="octet-stream",
    filename="data.bin",
)

with smtplib.SMTP("127.0.0.1", {port}, timeout=10) as smtp:
    smtp.send_message(msg)
print("SENT")
"#
    );

    let stdout = match tokio::task::spawn_blocking(move || run_python(&script))
        .await
        .expect("python task")
    {
        Ok(out) => out,
        Err(e) => panic!("smtplib failed: {e}"),
    };
    assert!(stdout.contains("SENT"), "{stdout}");

    let ids = server.stored_messages().await;
    let message = server
        .messages
        .get(ids[0])
        .await
        .expect("query")
        .expect("stored");

    let attachments = server
        .messages
        .attachments(ids[0])
        .await
        .expect("attachments");
    assert_eq!(attachments.len(), 1, "the attachment must be recognised");
    assert_eq!(attachments[0].filename.as_deref(), Some("data.bin"));
    assert!(message.raw.size() > 256, "the encoded payload is present");
}
