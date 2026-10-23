//! Startup, shutdown, and the independence of the two listeners.
//!
//! The recurring theme: a Sandesh that cannot do its job must **fail to start
//! and say why**, not start half-working. A developer whose SMTP port is taken
//! should be told that, not discover it when their test suite silently receives
//! no mail.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestApp;
use sandesh_app::{AppError, Application};

/// A configuration in a fresh temporary directory, on ephemeral ports.
fn config(dir: &tempfile::TempDir) -> sandesh_config::AppConfig {
    TestApp::config_in(dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_data_directory_that_does_not_exist_yet_is_created() {
    // The first thing anyone does with a relocatable data directory (OD-24) is
    // point it somewhere that does not exist yet. Before this was handled,
    // that failed with SQLite error 14 — "unable to open database file" — which
    // names neither the directory nor the cause.
    let dir = tempfile::tempdir().expect("temporary directory");
    let nested = dir.path().join("does").join("not").join("exist");

    let mut config = config(&dir);
    config.database_path = nested.join("sandesh.db");
    config.blob_path = nested.join("blobs");

    let app = Application::start(config).await.expect("starts");

    assert!(
        nested.join("sandesh.db").exists(),
        "the database was created"
    );
    assert!(nested.join("blobs").is_dir(), "the blob store was created");

    app.shutdown_token().cancel();
    app.run().await.expect("stops cleanly");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_started_application_reports_both_addresses() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let app = Application::start(config(&dir)).await.expect("starts");

    let addresses = app.addresses();
    let smtp = addresses.smtp.expect("SMTP bound");
    let http = addresses.http.expect("HTTP bound");

    // Port 0 was requested, so a real port must have been chosen and reported.
    assert_ne!(smtp.port(), 0, "the ephemeral SMTP port must be resolved");
    assert_ne!(http.port(), 0, "the ephemeral HTTP port must be resolved");
    assert!(smtp.ip().is_loopback() && http.ip().is_loopback());

    app.shutdown_token().cancel();
    app.run().await.expect("stops cleanly");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_banner_names_everything_a_developer_needs() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let app = Application::start(config(&dir)).await.expect("starts");

    let banner = app.banner();
    for expected in ["Sandesh", "SMTP:", "HTTP:", "WebSocket:", "Database:"] {
        assert!(
            banner.contains(expected),
            "banner is missing {expected}:\n{banner}"
        );
    }
    assert!(
        banner.contains(&app.addresses().http.expect("bound").to_string()),
        "the banner must name the real port"
    );

    app.shutdown_token().cancel();
    app.run().await.expect("stops cleanly");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_configuration_fails_before_anything_is_opened() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut invalid = config(&dir);
    invalid.smtp.max_recipients = 0;

    let error = Application::start(invalid).await.expect_err("must refuse");
    assert!(
        matches!(error, AppError::Config(_)),
        "expected a configuration error, got {error}"
    );

    // Nothing was created, because validation happens first.
    assert!(
        !dir.path().join("sandesh.db").exists(),
        "an invalid configuration must not leave a database behind"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cors_wildcard_is_refused_at_startup() {
    // Not a warning: `Allow-Origin: *` here would let any page a developer
    // visits read their mail.
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut wildcard = config(&dir);
    wildcard.http.cors_origins = vec![String::from("*")];

    let error = Application::start(wildcard).await.expect_err("must refuse");
    assert!(matches!(error, AppError::Config(_)), "got {error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unusable_database_path_fails_cleanly() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut broken = config(&dir);

    // A path whose parent is a *file*, so the directory genuinely cannot be
    // created. A merely-absent directory is no longer an error — it is created
    // (OD-24) — so this test needs a path that is unusable rather than one that
    // is only missing, which is what its comment always described.
    let blocker = dir.path().join("i-am-a-file");
    std::fs::write(&blocker, b"not a directory").expect("writes the blocking file");
    broken.database_path = blocker.join("nested").join("x.db");

    let error = Application::start(broken).await.expect_err("must refuse");
    assert!(
        matches!(error, AppError::Storage { .. } | AppError::Bootstrap(_)),
        "expected a storage or setup error, got {error}"
    );
    // The message names the path, so the failure is actionable.
    assert!(
        error.to_string().contains("i-am-a-file"),
        "the error must name the path: {error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_port_already_in_use_fails_startup_rather_than_starting_half_way() {
    let dir_a = tempfile::tempdir().expect("temporary directory");
    let first = Application::start(config(&dir_a)).await.expect("starts");
    let taken = first.addresses().http.expect("HTTP bound");

    let dir_b = tempfile::tempdir().expect("temporary directory");
    let mut clashing = config(&dir_b);
    clashing.http.bind = taken;

    let error = Application::start(clashing)
        .await
        .expect_err("the port is taken");
    assert!(
        matches!(error, AppError::Http(_)),
        "expected an HTTP bind error, got {error}"
    );
    // The message names the address, so a clash is diagnosable.
    assert!(
        error.to_string().contains(&taken.to_string()),
        "got {error}"
    );

    first.shutdown_token().cancel();
    first.run().await.expect("stops cleanly");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_smtp_port_clash_also_fails_startup() {
    let dir_a = tempfile::tempdir().expect("temporary directory");
    let first = Application::start(config(&dir_a)).await.expect("starts");
    let taken = first.addresses().smtp.expect("SMTP bound");

    let dir_b = tempfile::tempdir().expect("temporary directory");
    let mut clashing = config(&dir_b);
    clashing.smtp.bind = taken;

    let error = Application::start(clashing)
        .await
        .expect_err("the port is taken");
    assert!(
        matches!(error, AppError::Smtp(_)),
        "expected an SMTP bind error, got {error}"
    );

    first.shutdown_token().cancel();
    first.run().await.expect("stops cleanly");
}

#[tokio::test(flavor = "multi_thread")]
async fn smtp_runs_with_the_api_disabled() {
    // SMTP must not depend on the API existing.
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut smtp_only = config(&dir);
    smtp_only.http_enabled = false;

    let app = Application::start(smtp_only).await.expect("starts");
    assert!(app.addresses().smtp.is_some());
    assert!(app.addresses().http.is_none(), "HTTP must not be bound");

    let smtp = app.addresses().smtp.expect("bound");
    let shutdown = app.shutdown_token();
    let handle = tokio::spawn(app.run());

    // And it genuinely accepts mail with no HTTP server in the process.
    let mut client = common::SmtpClient::connect(smtp).await;
    client.expect("220").await;
    client.send("EHLO test.local").await;
    client.read_multiline().await;
    client.send("MAIL FROM:<s@test.local>").await;
    client.expect("250").await;
    client.send("RCPT TO:<dev@example.test>").await;
    client.expect("250").await;
    client.send("DATA").await;
    client.expect("354").await;
    client.send("Subject: no api\r\n\r\nBody.\r\n.").await;
    client.expect("250").await;
    client.send("QUIT").await;

    shutdown.cancel();
    handle.await.expect("task").expect("stops cleanly");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_api_runs_with_smtp_disabled() {
    // And the reverse: the API must not depend on SMTP existing.
    let dir = tempfile::tempdir().expect("temporary directory");
    let app = TestApp::with_config(|config| config.smtp_enabled = false).await;

    let (status, body) = app.get_json("/health").await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");

    let (_, info) = app.get_json("/info").await;
    assert!(
        info["smtp_address"].is_null(),
        "a disabled listener must be reported as absent, not invented"
    );

    app.stop().await;
    drop(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_every_server_and_releases_the_database() {
    let app = TestApp::start().await;
    let http = app.http;
    let smtp = app.smtp();

    // Both listeners are genuinely accepting before shutdown.
    assert!(tokio::net::TcpStream::connect(http).await.is_ok());
    assert!(tokio::net::TcpStream::connect(smtp).await.is_ok());

    // `stop` asserts the run task completed without panicking or erroring, so
    // an orphaned server task or a failed close would fail here.
    app.stop().await;

    // Nothing is accepting afterwards. Retried briefly because a listening
    // socket takes a moment to be torn down by the kernel.
    for address in [http, smtp] {
        let mut refused = false;
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(address).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(refused, "{address} was still accepting after shutdown");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_completes_with_websocket_clients_still_connected() {
    // A connected client must not be able to hold shutdown open.
    let app = TestApp::start().await;

    let _a = common::WsClient::connect(&app).await;
    let _b = common::WsClient::connect(&app).await;

    // `stop` has a bounded timeout, so a hang fails rather than blocking.
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_application_can_be_restarted_over_the_same_database() {
    // Restarting must find the existing catch-all rather than creating a
    // second one, and must find the mail already stored.
    let dir = tempfile::tempdir().expect("temporary directory");

    {
        let app = Application::start(config(&dir)).await.expect("starts");
        let smtp = app.addresses().smtp.expect("bound");
        let shutdown = app.shutdown_token();
        let handle = tokio::spawn(app.run());

        let mut client = common::SmtpClient::connect(smtp).await;
        client.expect("220").await;
        client.send("EHLO test.local").await;
        client.read_multiline().await;
        client.send("MAIL FROM:<s@test.local>").await;
        client.expect("250").await;
        client.send("RCPT TO:<dev@example.test>").await;
        client.expect("250").await;
        client.send("DATA").await;
        client.expect("354").await;
        client.send("Subject: persisted\r\n\r\nBody.\r\n.").await;
        client.expect("250").await;
        client.send("QUIT").await;

        shutdown.cancel();
        handle.await.expect("task").expect("stops cleanly");
    }

    // Second run, same directory.
    let app = Application::start(config(&dir)).await.expect("restarts");
    let http = app.addresses().http.expect("bound");
    let shutdown = app.shutdown_token();
    let handle = tokio::spawn(app.run());

    let probe = Probe { http };
    probe.await_ready().await;

    let accounts = probe.get_json("/accounts").await;
    assert_eq!(
        accounts["total_estimate"], 1,
        "a restart must not create a second catch-all account"
    );

    let messages = probe.get_json("/messages").await;
    assert_eq!(messages["total_estimate"], 1, "mail must survive a restart");
    assert_eq!(messages["items"][0]["subject"], "persisted");

    shutdown.cancel();
    handle.await.expect("task").expect("stops cleanly");
}

/// A minimal HTTP client for tests that build an `Application` directly.
struct Probe {
    http: std::net::SocketAddr,
}

impl Probe {
    async fn await_ready(&self) {
        for _ in 0..500 {
            if self.try_get("/health/ready").await.is_some() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the application never became ready");
    }

    async fn get_json(&self, path: &str) -> serde_json::Value {
        self.try_get(path)
            .await
            .unwrap_or_else(|| panic!("GET {path} failed"))
    }

    async fn try_get(&self, path: &str) -> Option<serde_json::Value> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(self.http).await.ok()?;
        let request = format!(
            "GET /api/v1{path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.http
        );
        stream.write_all(request.as_bytes()).await.ok()?;

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.ok()?;

        let index = raw.windows(4).position(|window| window == b"\r\n\r\n")?;
        serde_json::from_slice(&raw[index + 4..]).ok()
    }
}

// ---------------------------------------------------------------- IMAP (Phase 5)

#[tokio::test]
async fn the_imap_listener_starts_with_the_application() {
    // Phase 5 increment 2, §13: the IMAP server must run inside the real
    // application — same lifecycle, same shutdown token — not only under its
    // own crate's harness.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let app = TestApp::start().await;

    let mut stream = tokio::net::TcpStream::connect(app.imap())
        .await
        .expect("connect to the IMAP listener");

    let mut buf = [0u8; 512];
    let read = stream.read(&mut buf).await.expect("read the greeting");
    let greeting = String::from_utf8_lossy(&buf[..read]);

    assert!(
        greeting.starts_with("* OK "),
        "the application's IMAP listener must greet: {greeting:?}"
    );
    assert!(
        greeting.contains("IMAP4rev2"),
        "the greeting must advertise IMAP4rev2: {greeting:?}"
    );

    // And it must actually answer a command, not merely accept the socket.
    stream.write_all(b"A1 CAPABILITY\r\n").await.expect("write");
    let read = stream.read(&mut buf).await.expect("read the response");
    let response = String::from_utf8_lossy(&buf[..read]);
    assert!(response.contains("A1 OK"), "{response:?}");
}

#[tokio::test]
async fn smtp_http_and_imap_run_together() {
    // The three listeners must co-exist: none may depend on another being up,
    // and none may claim another's port.
    let app = TestApp::start().await;

    assert!(app.smtp_addr.is_some(), "SMTP should be bound");
    assert!(app.imap_addr.is_some(), "IMAP should be bound");

    let smtp = app.smtp();
    let imap = app.imap();
    let http = app.http;
    assert_ne!(smtp.port(), imap.port(), "listeners must not share a port");
    assert_ne!(imap.port(), http.port(), "listeners must not share a port");

    // Both accept connections.
    for addr in [smtp, imap] {
        tokio::net::TcpStream::connect(addr)
            .await
            .unwrap_or_else(|error| panic!("connect to {addr}: {error}"));
    }
}

#[tokio::test]
async fn the_imap_listener_stops_on_shutdown() {
    // Clean shutdown: one token stops every listener.
    let app = TestApp::start().await;
    let imap = app.imap();

    // Reachable while running.
    tokio::net::TcpStream::connect(imap)
        .await
        .expect("reachable before shutdown");

    app.stop().await;

    // The port must stop accepting. A short retry loop, because the OS
    // releases the listening socket asynchronously.
    let mut refused = false;
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(imap).await.is_err() {
            refused = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        refused,
        "the IMAP listener must stop accepting after shutdown"
    );
}
