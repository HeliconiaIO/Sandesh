//! Webhooks end to end: registered over HTTP, fired by SMTP, observed at a real
//! endpoint (Phase 8 A2, ADR-085, ADR-086).
//!
//! The unit tests in `sandesh-webhook` drive the worker directly and the
//! storage tests drive the table directly. Neither can catch the failure that
//! matters most here: an API that registers endpoints into one store while the
//! worker drains another. Every endpoint would report success and nothing would
//! ever be sent — the split-registry hazard ADR-084 named for faults, which is
//! why these tests go in through the front door and wait at the far end.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::time::Duration;

use sandesh_app::Application;
use sandesh_config::AppConfig;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A running application on ephemeral ports, torn down when dropped.
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
    async fn deliver(&self, subject: &str) {
        let mut smtp = TcpStream::connect(self.smtp).await.expect("connects");
        let mut buffer = [0_u8; 1024];

        async fn expect(stream: &mut TcpStream, buffer: &mut [u8], prefix: &str) {
            let read = stream.read(buffer).await.expect("reads a reply");
            let reply = String::from_utf8_lossy(&buffer[..read]).into_owned();
            assert!(reply.starts_with(prefix), "expected {prefix}, got {reply}");
        }

        expect(&mut smtp, &mut buffer, "220").await;
        for (command, code) in [
            ("EHLO test.local\r\n", "250"),
            ("MAIL FROM:<sender@example.test>\r\n", "250"),
            ("RCPT TO:<dev@example.test>\r\n", "250"),
            ("DATA\r\n", "354"),
        ] {
            smtp.write_all(command.as_bytes()).await.expect("writes");
            expect(&mut smtp, &mut buffer, code).await;
        }

        let message = format!(
            "From: sender@example.test\r\nTo: dev@example.test\r\nSubject: {subject}\r\n\r\nbody\r\n.\r\n"
        );
        smtp.write_all(message.as_bytes()).await.expect("writes");
        expect(&mut smtp, &mut buffer, "250").await;
        smtp.write_all(b"QUIT\r\n").await.expect("writes QUIT");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Polls until `check` passes, or gives up.
///
/// The worker ticks on a timer, so the arrival is genuinely asynchronous; a
/// bare assertion after delivery would be a race rather than a test.
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..60 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn a_delivery_fires_a_webhook_at_a_registered_endpoint() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&endpoint)
        .await;

    let app = Harness::start().await;

    // Registered through the API, exactly as a developer would.
    let response = app
        .request(
            "POST",
            "/api/v1/webhooks",
            Some(&format!(r#"{{"url":"{}/hook"}}"#, endpoint.uri())),
        )
        .await;
    assert!(
        response.starts_with("HTTP/1.1 201"),
        "expected 201, got {response}"
    );

    app.deliver("webhook end to end").await;

    // Waiting at the far end. This is the assertion the split-store bug would
    // fail: registration would still return 201 and the outbox would stay
    // empty forever.
    eventually("the endpoint to receive a POST", || async {
        !endpoint
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    })
    .await;

    let requests = endpoint.received_requests().await.expect("requests");
    let body = String::from_utf8_lossy(&requests[0].body).into_owned();
    assert!(
        body.contains("MessageReceived"),
        "the payload must name the event, got {body}"
    );
}

#[tokio::test]
async fn the_outbox_view_reports_the_delivered_entry() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&endpoint)
        .await;

    let app = Harness::start().await;
    app.request(
        "POST",
        "/api/v1/webhooks",
        Some(&format!(r#"{{"url":"{}"}}"#, endpoint.uri())),
    )
    .await;
    app.deliver("outbox view").await;

    // The view answers "did my webhook fire?", so a delivered entry has to be
    // visible in it — not only pending work.
    eventually("the outbox to report a delivery", || async {
        app.request("GET", "/api/v1/webhooks/outbox", None)
            .await
            .contains("\"delivered\"")
    })
    .await;
}

#[tokio::test]
async fn a_failing_endpoint_is_retried_and_stays_visible() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&endpoint)
        .await;

    let app = Harness::start().await;
    app.request(
        "POST",
        "/api/v1/webhooks",
        Some(&format!(r#"{{"url":"{}"}}"#, endpoint.uri())),
    )
    .await;
    app.deliver("retry").await;

    // More than one attempt against an endpoint that always fails is the retry
    // behaviour the criterion names, observed from the endpoint's side.
    eventually("the endpoint to be retried", || async {
        endpoint.received_requests().await.unwrap_or_default().len() >= 2
    })
    .await;

    // And the entry explains itself rather than vanishing.
    let outbox = app.request("GET", "/api/v1/webhooks/outbox", None).await;
    assert!(
        outbox.contains("HTTP 500"),
        "the outbox must record why, got {outbox}"
    );
}

#[tokio::test]
async fn with_no_endpoint_registered_delivery_is_unaffected() {
    // Webhooks are opt-in. A server with none configured must behave exactly as
    // it did before Phase 8, and the outbox must stay empty.
    let app = Harness::start().await;
    app.deliver("no webhooks here").await;

    let mut seen = false;
    for _ in 0..20 {
        if app
            .request("GET", "/api/v1/messages", None)
            .await
            .contains("no webhooks here")
        {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "delivery must still work with no webhook configured");

    let outbox = app.request("GET", "/api/v1/webhooks/outbox", None).await;
    assert!(
        outbox.contains("[]"),
        "the outbox must be empty, got {outbox}"
    );
}

#[tokio::test]
async fn a_malformed_url_is_refused_at_registration() {
    // Validated at registration rather than at send time: a typo would
    // otherwise become a backlog of terminal failures instead of a 400.
    let app = Harness::start().await;

    let response = app
        .request("POST", "/api/v1/webhooks", Some(r#"{"url":"not-a-url"}"#))
        .await;
    assert!(
        response.starts_with("HTTP/1.1 400"),
        "expected 400, got {response}"
    );

    let listed = app.request("GET", "/api/v1/webhooks", None).await;
    assert!(listed.contains("[]"), "nothing must be registered");
}

#[tokio::test]
async fn removing_an_endpoint_stops_its_notifications() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&endpoint)
        .await;

    let app = Harness::start().await;
    let created = app
        .request(
            "POST",
            "/api/v1/webhooks",
            Some(&format!(r#"{{"url":"{}"}}"#, endpoint.uri())),
        )
        .await;

    let id = created
        .rsplit_once(r#""id":""#)
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(id, _)| id.to_owned())
        .expect("the response carries an id");

    let deleted = app
        .request("DELETE", &format!("/api/v1/webhooks/{id}"), None)
        .await;
    assert!(
        deleted.starts_with("HTTP/1.1 204"),
        "expected 204, got {deleted}"
    );

    app.deliver("after removal").await;

    // Give the worker several ticks to prove a negative.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        endpoint
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a removed endpoint must receive nothing"
    );

    let missing = app
        .request("DELETE", &format!("/api/v1/webhooks/{id}"), None)
        .await;
    assert!(
        missing.starts_with("HTTP/1.1 404"),
        "a second delete is a 404, got {missing}"
    );
}
