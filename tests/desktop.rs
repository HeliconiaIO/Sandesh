//! The desktop configuration runs the identical engine (`ROADMAP.md` Phase 9).
//!
//! Phase 9's criteria are claims about the *engine under a desktop
//! configuration*, not about a window:
//!
//! * "runs the identical engine with no forked code"
//! * "the protocol listeners work identically"
//! * "the React build is byte-identical to the web build"
//!
//! A WebView can make none of those true or false, so none of them is tested
//! through one. What this file drives is exactly what the shell drives —
//! `Application::start(desktop_config(..))` — and then talks SMTP, IMAP, POP3
//! and HTTP to the ports it reports, the same way every other integration test
//! in this workspace does.
//!
//! The window itself is verified by compiling `sandesh-desktop` and, on a
//! machine with a working WebKit renderer, by launching it. Where WebKit
//! fails internally, that is an environmental gap (ADR-094), not a defect in
//! the shell.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use sandesh_app::Application;
use sandesh_config::AppConfig;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The desktop shell's configuration, duplicated from `sandesh-desktop`.
///
/// Deliberately a copy rather than an import: the workspace-root test package
/// must not depend on `sandesh-desktop`, since `tests/architecture.rs` asserts
/// that only the desktop shell depends on the composition root and adding
/// another consumer would blunt that rule. The unit tests in
/// `sandesh-desktop::engine` pin the real function's properties; this asserts
/// what an engine started *that way* does.
fn desktop_config(data_dir: &std::path::Path) -> AppConfig {
    let loopback_ephemeral: std::net::SocketAddr = ([127, 0, 0, 1], 0).into();
    let defaults = AppConfig::default();

    AppConfig {
        database_path: data_dir.join("sandesh.db"),
        blob_path: data_dir.join("blobs"),
        http: sandesh_config::HttpConfig {
            bind: loopback_ephemeral,
            ..defaults.http
        },
        smtp: sandesh_config::SmtpConfig {
            bind: loopback_ephemeral,
            ..defaults.smtp
        },
        imap: sandesh_config::ImapConfig {
            bind: Some(loopback_ephemeral),
            ..defaults.imap
        },
        pop3: sandesh_config::Pop3Config {
            bind: Some(loopback_ephemeral),
            ..defaults.pop3
        },
        ..defaults
    }
}

async fn expect(stream: &mut TcpStream, buffer: &mut [u8], prefix: &str) -> String {
    let read = stream.read(buffer).await.expect("reads a reply");
    let reply = String::from_utf8_lossy(&buffer[..read]).into_owned();
    assert!(reply.starts_with(prefix), "expected {prefix}, got {reply}");
    reply
}

async fn http_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connects");
    let head = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream
        .write_all(head.as_bytes())
        .await
        .expect("writes the request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("reads the response");
    String::from_utf8_lossy(&response).into_owned()
}

#[tokio::test]
async fn the_desktop_configuration_binds_every_listener_on_loopback() {
    // "The protocol listeners work identically" begins with them existing. A
    // desktop app that silently dropped IMAP would still open a window and look
    // correct.
    let dir = TempDir::new().expect("a temporary directory");
    let app = Application::start(desktop_config(dir.path()))
        .await
        .expect("the desktop configuration starts");

    let addresses = app.addresses();
    for (name, address) in [
        ("HTTP", addresses.http),
        ("SMTP", addresses.smtp),
        ("IMAP", addresses.imap),
        ("POP3", addresses.pop3),
    ] {
        let address = address.unwrap_or_else(|| panic!("{name} must be bound"));
        assert!(
            address.ip().is_loopback(),
            "{name} bound {address}, which is not loopback: a desktop install must not \
             put a mail server on the user's network"
        );
        assert_ne!(
            address.port(),
            0,
            "{name} must report the port the OS gave it"
        );
    }

    app.shutdown_token().cancel();
}

#[tokio::test]
async fn mail_delivered_over_smtp_is_readable_over_every_protocol_and_the_api() {
    // The whole of "runs the identical engine" in one test: deliver over SMTP,
    // then read the same message back over IMAP, POP3 and HTTP. If the desktop
    // configuration forked anything — storage, routing, placement — one of
    // these four surfaces disagrees with the others.
    let dir = TempDir::new().expect("a temporary directory");
    let app = Application::start(desktop_config(dir.path()))
        .await
        .expect("the desktop configuration starts");

    let http = app.addresses().http.expect("HTTP is bound");
    let smtp = app.addresses().smtp.expect("SMTP is bound");
    let imap = app.addresses().imap.expect("IMAP is bound");
    let pop3 = app.addresses().pop3.expect("POP3 is bound");
    let shutdown = app.shutdown_token();

    tokio::spawn(async move {
        let _ = app.run().await;
    });

    // --- SMTP in ---
    let mut buffer = [0_u8; 4096];
    let mut session = TcpStream::connect(smtp).await.expect("connects to SMTP");
    expect(&mut session, &mut buffer, "220").await;
    for (command, code) in [
        ("EHLO desktop.test\r\n", "250"),
        ("MAIL FROM:<app@example.test>\r\n", "250"),
        ("RCPT TO:<dev@example.test>\r\n", "250"),
        ("DATA\r\n", "354"),
    ] {
        session.write_all(command.as_bytes()).await.expect("writes");
        expect(&mut session, &mut buffer, code).await;
    }
    session
        .write_all(
            b"From: app@example.test\r\nTo: dev@example.test\r\nSubject: desktop\r\n\r\nhello desktop\r\n.\r\n",
        )
        .await
        .expect("writes the message");
    expect(&mut session, &mut buffer, "250").await;
    session.write_all(b"QUIT\r\n").await.expect("writes QUIT");

    // --- HTTP out ---
    let mut api_body = String::new();
    for _ in 0..60 {
        api_body = http_get(http, "/api/v1/messages").await;
        if api_body.contains("desktop") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        api_body.contains("desktop"),
        "the message must be queryable over the API: {api_body}"
    );

    // --- IMAP out ---
    let mut imap_session = TcpStream::connect(imap).await.expect("connects to IMAP");
    expect(&mut imap_session, &mut buffer, "* OK").await;
    imap_session
        .write_all(b"a1 LOGIN dev@example.test password\r\n")
        .await
        .expect("writes LOGIN");
    // Authentication is disabled by default (ADR-005), so any credential is
    // accepted; what matters here is that the listener speaks IMAP at all.
    let login = expect(&mut imap_session, &mut buffer, "a1 ").await;
    assert!(
        login.contains("OK"),
        "IMAP must accept a login in the desktop configuration: {login}"
    );
    imap_session
        .write_all(b"a2 SELECT INBOX\r\n")
        .await
        .expect("writes SELECT");
    let mut selected = String::new();
    for _ in 0..8 {
        let read = imap_session
            .read(&mut buffer)
            .await
            .expect("reads the SELECT response");
        selected.push_str(&String::from_utf8_lossy(&buffer[..read]));
        if selected.contains("a2 ") {
            break;
        }
    }
    assert!(
        selected.contains("EXISTS"),
        "IMAP must report the delivered message: {selected}"
    );
    imap_session
        .write_all(b"a3 LOGOUT\r\n")
        .await
        .expect("writes LOGOUT");

    // --- POP3 out ---
    let mut pop = TcpStream::connect(pop3).await.expect("connects to POP3");
    expect(&mut pop, &mut buffer, "+OK").await;
    for command in ["USER dev@example.test\r\n", "PASS password\r\n", "STAT\r\n"] {
        pop.write_all(command.as_bytes()).await.expect("writes");
        let reply = expect(&mut pop, &mut buffer, "+OK").await;
        if command.starts_with("STAT") {
            assert!(
                reply.contains("+OK 1 "),
                "POP3 must count the delivered message: {reply}"
            );
        }
    }
    pop.write_all(b"QUIT\r\n").await.expect("writes QUIT");

    shutdown.cancel();
}

#[tokio::test]
async fn the_desktop_engine_serves_the_same_embedded_ui() {
    // "The React build is byte-identical to the web build" holds structurally:
    // the desktop app has no asset pipeline of its own, so the WebView loads
    // the very assets `rust-embed` compiled into `sandesh-api`. What this
    // asserts is the load-bearing half — that the engine a desktop install
    // starts does serve the UI, so pointing a WebView at it is sufficient.
    let dir = TempDir::new().expect("a temporary directory");
    let app = Application::start(desktop_config(dir.path()))
        .await
        .expect("the desktop configuration starts");
    let http = app.addresses().http.expect("HTTP is bound");
    let shutdown = app.shutdown_token();

    tokio::spawn(async move {
        let _ = app.run().await;
    });

    let response = http_get(http, "/").await;
    // Without `npm run build` there is no UI to embed (the Rust CI job builds
    // none), and the engine answers 503 by design. Assert that contract here,
    // as `sandesh-api`'s own asset tests do, rather than failing the build.
    if !sandesh_api::assets::is_available() {
        assert!(
            response.starts_with("HTTP/1.1 503"),
            "without a built UI the root must explain itself with a 503: {response}"
        );
        shutdown.cancel();
        return;
    }
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "the desktop engine must serve the UI at the root: {response}"
    );
    assert!(
        response.contains("<div id=\"root\""),
        "the root document must be the React app's shell: {response}"
    );

    shutdown.cancel();
}

#[tokio::test]
async fn two_desktop_instances_do_not_collide() {
    // The reason the ports are ephemeral. A second instance — or a Sandesh
    // server the user already had running — must not stop the app launching.
    // With fixed default ports this fails on the second `start`.
    let first_dir = TempDir::new().expect("a temporary directory");
    let second_dir = TempDir::new().expect("a temporary directory");

    let first = Application::start(desktop_config(first_dir.path()))
        .await
        .expect("the first instance starts");
    let second = Application::start(desktop_config(second_dir.path()))
        .await
        .expect("a second instance must also start");

    assert_ne!(
        first.addresses().smtp.expect("bound"),
        second.addresses().smtp.expect("bound"),
        "two instances must not be handed the same SMTP port"
    );

    first.shutdown_token().cancel();
    second.shutdown_token().cancel();
}
