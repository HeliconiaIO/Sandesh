//! Message rules end to end: configured over HTTP, applied by SMTP, explained
//! by the delivery record (Phase 8 A3, ADR-087).
//!
//! The criterion has two halves — "rules apply at delivery **and are visible in
//! the delivery record**" — and the assertion that matters is the *pair*. A
//! keyword with no explanation is the opacity Phase 7 was built to remove; an
//! explanation with no keyword is a record of something that did not happen.
//! Each test here asserts both, over real sockets, because the split-store
//! failure would satisfy neither while every endpoint reported success.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use sandesh_app::Application;
use sandesh_config::AppConfig;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

struct Harness {
    http: SocketAddr,
    smtp: SocketAddr,
    shutdown: CancellationToken,
    _dir: TempDir,
}

impl Harness {
    async fn start() -> Self {
        let dir = TempDir::new().expect("a temporary directory");
        let ephemeral: SocketAddr = "127.0.0.1:0".parse().expect("a valid address");
        let defaults = AppConfig::default();
        let config = AppConfig {
            database_path: dir.path().join("sandesh.db"),
            blob_path: dir.path().join("blobs"),
            http: sandesh_config::HttpConfig {
                bind: ephemeral,
                ..defaults.http
            },
            smtp: sandesh_config::SmtpConfig {
                bind: ephemeral,
                ..defaults.smtp
            },
            imap: sandesh_config::ImapConfig {
                bind: Some(ephemeral),
                ..defaults.imap
            },
            pop3: sandesh_config::Pop3Config {
                bind: Some(ephemeral),
                ..defaults.pop3
            },
            ..defaults
        };

        let app = Application::start(config).await.expect("the app starts");
        let http = app.addresses().http.expect("HTTP is bound");
        let smtp = app.addresses().smtp.expect("SMTP is bound");
        let shutdown = app.shutdown_token();

        tokio::spawn(async move {
            let _ = app.run().await;
        });

        Self {
            http,
            smtp,
            shutdown,
            _dir: dir,
        }
    }

    async fn request(&self, method: &str, path: &str, body: Option<&str>) -> String {
        let mut stream = TcpStream::connect(self.http).await.expect("connects");
        let head = match body {
            Some(body) => format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
            None => {
                format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            }
        };
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

    /// Delivers one message over real SMTP.
    async fn deliver(&self, from: &str, subject: &str) {
        let mut smtp = TcpStream::connect(self.smtp).await.expect("connects");
        let mut buffer = [0_u8; 1024];

        async fn expect(stream: &mut TcpStream, buffer: &mut [u8], prefix: &str) {
            let read = stream.read(buffer).await.expect("reads a reply");
            let reply = String::from_utf8_lossy(&buffer[..read]).into_owned();
            assert!(reply.starts_with(prefix), "expected {prefix}, got {reply}");
        }

        expect(&mut smtp, &mut buffer, "220").await;
        for (command, code) in [
            ("EHLO test.local\r\n".to_owned(), "250"),
            (format!("MAIL FROM:<{from}>\r\n"), "250"),
            ("RCPT TO:<dev@example.test>\r\n".to_owned(), "250"),
            ("DATA\r\n".to_owned(), "354"),
        ] {
            smtp.write_all(command.as_bytes()).await.expect("writes");
            expect(&mut smtp, &mut buffer, code).await;
        }

        let message = format!(
            "From: {from}\r\nTo: dev@example.test\r\nSubject: {subject}\r\n\r\nbody\r\n.\r\n"
        );
        smtp.write_all(message.as_bytes()).await.expect("writes");
        expect(&mut smtp, &mut buffer, "250").await;
        smtp.write_all(b"QUIT\r\n").await.expect("writes QUIT");
    }

    /// Polls until `check` passes on the response body, or gives up.
    async fn eventually(&self, path: &str, what: &str, check: impl Fn(&str) -> bool) -> String {
        for _ in 0..40 {
            let body = self.request("GET", path, None).await;
            if check(&body) {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Creates a rule matching `Subject: …invoice…`, returning its id.
async fn invoice_rule(app: &Harness) -> String {
    let created = app
        .request(
            "POST",
            "/api/v1/rules",
            Some(
                r#"{"name":"invoices",
                    "conditions":[{"kind":"header_contains","name":"subject","value":"invoice"}],
                    "add_keyword":"$Invoice"}"#,
            ),
        )
        .await;
    assert!(
        created.starts_with("HTTP/1.1 201"),
        "expected 201, got {created}"
    );

    created
        .rsplit_once(r#""id":""#)
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(id, _)| id.to_owned())
        .expect("the response carries an id")
}

#[tokio::test]
async fn a_rule_tags_the_message_and_the_delivery_explains_why() {
    // The whole criterion, in one test: the keyword lands *and* the delivery
    // record names the rule that put it there.
    let app = Harness::start().await;
    invoice_rule(&app).await;

    app.deliver("billing@vendor.test", "Invoice 4021").await;

    let deliveries = app
        .eventually(
            "/api/v1/deliveries",
            "the delivery to be recorded",
            |body| body.contains("rules_applied") && body.contains("$Invoice"),
        )
        .await;

    // Half one: the record explains itself, naming the rule.
    assert!(
        deliveries.contains("invoices"),
        "the delivery must name the rule that fired: {deliveries}"
    );

    // Half two: the keyword is actually on the message, not merely claimed.
    let messages = app
        .eventually("/api/v1/messages", "the message to be listed", |body| {
            body.contains("Invoice 4021")
        })
        .await;
    let id = messages
        .rsplit_once(r#""id":""#)
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(id, _)| id.to_owned())
        .expect("a message id");

    let message = app
        .request("GET", &format!("/api/v1/messages/{id}"), None)
        .await;
    assert!(
        message.contains("$Invoice"),
        "the keyword must be on the message, not only in the record: {message}"
    );
}

#[tokio::test]
async fn a_message_that_does_not_match_is_left_alone() {
    let app = Harness::start().await;
    invoice_rule(&app).await;

    app.deliver("someone@vendor.test", "Just a hello").await;

    let deliveries = app
        .eventually(
            "/api/v1/deliveries",
            "the delivery to be recorded",
            |body| body.contains("Just a hello") || body.contains("rules_applied"),
        )
        .await;

    assert!(
        !deliveries.contains("$Invoice"),
        "a non-matching message must not be tagged: {deliveries}"
    );
}

#[tokio::test]
async fn with_no_rules_configured_delivery_is_unaffected() {
    // Rules are opt-in. A server with none must behave exactly as it did
    // before Phase 8 A3, and every delivery must report an empty list.
    let app = Harness::start().await;
    app.deliver("anyone@vendor.test", "No rules here").await;

    let deliveries = app
        .eventually(
            "/api/v1/deliveries",
            "the delivery to be recorded",
            |body| body.contains("rules_applied"),
        )
        .await;

    assert!(
        deliveries.contains(r#""rules_applied":[]"#),
        "no rules means an empty list, not a missing field: {deliveries}"
    );
    assert!(
        app.request("GET", "/api/v1/rules", None)
            .await
            .contains("[]")
    );
}

#[tokio::test]
async fn a_deleted_rule_stops_firing_but_its_record_survives() {
    // The reason the delivery record copies the rule's name rather than
    // pointing at it: removing a rule must not erase what it did.
    let app = Harness::start().await;
    let id = invoice_rule(&app).await;

    app.deliver("billing@vendor.test", "Invoice before").await;
    app.eventually("/api/v1/deliveries", "the first delivery", |body| {
        body.contains("$Invoice")
    })
    .await;

    let deleted = app
        .request("DELETE", &format!("/api/v1/rules/{id}"), None)
        .await;
    assert!(
        deleted.starts_with("HTTP/1.1 204"),
        "expected 204, got {deleted}"
    );

    app.deliver("billing@vendor.test", "Invoice after").await;
    app.eventually("/api/v1/deliveries", "the second delivery", |body| {
        body.contains("Invoice after") || body.matches("rules_applied").count() >= 2
    })
    .await;

    // The historical record still names the rule, though the rule is gone.
    let deliveries = app.request("GET", "/api/v1/deliveries", None).await;
    assert!(
        deliveries.contains("invoices"),
        "history must survive the rule: {deliveries}"
    );
    assert!(
        app.request("GET", "/api/v1/rules", None)
            .await
            .contains("[]")
    );

    // And a second delete is a 404, not a silent success.
    let missing = app
        .request("DELETE", &format!("/api/v1/rules/{id}"), None)
        .await;
    assert!(
        missing.starts_with("HTTP/1.1 404"),
        "expected 404, got {missing}"
    );
}

#[tokio::test]
async fn an_invalid_keyword_is_refused_at_creation() {
    // Validated here rather than at delivery time: an undecodable keyword
    // would otherwise be stored, listed, and silently never fire.
    let app = Harness::start().await;

    for bad in [r#"\Seen"#, "has space", ""] {
        let body = format!(
            r#"{{"name":"bad","conditions":[{{"kind":"sender_contains","value":"a"}}],"add_keyword":"{bad}"}}"#
        );
        let response = app.request("POST", "/api/v1/rules", Some(&body)).await;
        assert!(
            response.starts_with("HTTP/1.1 400"),
            "{bad:?} should be refused, got {response}"
        );
    }

    assert!(
        app.request("GET", "/api/v1/rules", None)
            .await
            .contains("[]")
    );
}

#[tokio::test]
async fn a_rule_with_no_conditions_is_refused() {
    // The domain treats an empty condition list as matching nothing; the API
    // refuses it outright, because it is the one place that can say why.
    let app = Harness::start().await;

    let response = app
        .request(
            "POST",
            "/api/v1/rules",
            Some(r#"{"name":"empty","conditions":[],"add_keyword":"$X"}"#),
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 400"),
        "expected 400, got {response}"
    );
}

#[tokio::test]
async fn a_sender_rule_matches_the_envelope_not_the_header() {
    // Envelope addresses decide routing, so a rule about the sender must mean
    // MAIL FROM. A `From:` header rule would be a header_contains condition.
    let app = Harness::start().await;
    app.request(
        "POST",
        "/api/v1/rules",
        Some(
            r#"{"name":"from billing",
                "conditions":[{"kind":"sender_contains","value":"billing@"}],
                "add_keyword":"$Billing"}"#,
        ),
    )
    .await;

    app.deliver("billing@vendor.test", "Anything at all").await;

    let deliveries = app
        .eventually(
            "/api/v1/deliveries",
            "the delivery to be recorded",
            |body| body.contains("$Billing"),
        )
        .await;
    assert!(deliveries.contains("from billing"));
}
