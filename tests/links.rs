//! Link and image extraction end to end: delivered by SMTP, extracted by the
//! API, never resolved (Phase 8 A6).
//!
//! The deliverable has two halves, and only one of them is a feature. The first
//! is that the URLs a message's HTML carries are enumerated and classified —
//! asserted here against a message delivered over a real socket, in the
//! encodings real mail uses. The second is the *absence* of network access, and
//! that half needs its own test: a bug that made extraction fetch would pass
//! every assertion about the list it returns. So one test points a message at a
//! listener this process owns and fails if a connection ever arrives.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sandesh_app::Application;
use sandesh_config::AppConfig;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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
            // Ephemeral too: these tests run in parallel, and the default
            // fixed ports collide between them.
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

    async fn get(&self, path: &str) -> String {
        let mut stream = TcpStream::connect(self.http).await.expect("connects");
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

    /// Delivers one message body verbatim over real SMTP.
    async fn deliver(&self, body: &str) {
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

        smtp.write_all(body.as_bytes()).await.expect("writes");
        smtp.write_all(b"\r\n.\r\n").await.expect("writes the dot");
        expect(&mut smtp, &mut buffer, "250").await;
        smtp.write_all(b"QUIT\r\n").await.expect("writes QUIT");
    }

    /// Delivers a message, then returns its `/links` response body.
    async fn deliver_and_extract(&self, message: &str) -> String {
        self.deliver(message).await;

        let list = self
            .eventually("/api/v1/messages", "the message to be queryable", |body| {
                body.contains(r#""id":"#)
            })
            .await;
        let id = list
            .split_once(r#""id":""#)
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(id, _)| id.to_owned())
            .expect("the list carries a message id");

        let response = self.get(&format!("/api/v1/messages/{id}/links")).await;
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "expected 200, got {response}"
        );
        response
    }

    async fn eventually(&self, path: &str, what: &str, check: impl Fn(&str) -> bool) -> String {
        for _ in 0..40 {
            let body = self.get(path).await;
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

/// An HTML message with `body` as its single part.
fn html_message(body: &str) -> String {
    format!(
        "From: sender@example.test\r\n\
         To: dev@example.test\r\n\
         Subject: links\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         \r\n\
         {body}"
    )
}

#[tokio::test]
async fn html_links_and_images_are_extracted_and_classified() {
    let app = Harness::start().await;

    let response = app
        .deliver_and_extract(&html_message(
            r#"<html><body>
               <a href="https://example.test/confirm">Confirm</a>
               <img src="https://cdn.example.test/logo.png" alt="Logo">
               <script src="https://tracker.example.test/t.js"></script>
               </body></html>"#,
        ))
        .await;

    // Classified by the role the markup gives each URL, not by what is behind it.
    assert!(response.contains(r#""url":"https://example.test/confirm""#));
    assert!(response.contains(r#""kind":"link""#));
    assert!(response.contains(r#""url":"https://cdn.example.test/logo.png""#));
    assert!(response.contains(r#""kind":"image""#));
    assert!(response.contains(r#""kind":"resource""#));

    assert!(response.contains(r#""link_count":1"#));
    assert!(response.contains(r#""image_count":1"#));
    assert!(response.contains(r#""resource_count":1"#));

    // The link text, which is where a deceptive link is visible as one: the URL
    // and the words the reader sees are different facts about the same anchor.
    assert!(response.contains(r#""text":"Confirm""#));
    assert!(response.contains(r#""alt":"Logo""#));

    // Hosts, deduplicated and sorted — the summary the panel leads with.
    assert!(
        response.contains(r#""hosts":["cdn.example.test","example.test","tracker.example.test"]"#),
        "expected the three hosts, sorted, in {response}"
    );
}

#[tokio::test]
async fn nothing_is_resolved_over_the_network() {
    // The half of the deliverable that is an absence. A listener this test owns
    // stands in for every host a message might name: if extraction ever
    // resolves a URL, a connection arrives here and the count is no longer zero.
    //
    // Asserted with a real socket rather than by reading the code, because the
    // property must survive a future change that adds an HTTP client for some
    // other reason.
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let trap = listener.local_addr().expect("has an address");

    let counter = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });

    let app = Harness::start().await;
    let response = app
        .deliver_and_extract(&html_message(&format!(
            r#"<a href="http://{trap}/click">click</a>
               <img src="http://{trap}/pixel.gif">
               <iframe src="http://{trap}/frame"></iframe>"#
        )))
        .await;

    // The URLs are reported...
    assert!(
        response.contains("/pixel.gif"),
        "expected the image in {response}"
    );
    // ...and the payload says plainly that none was fetched.
    assert!(
        response.contains(r#""disclaimer":"#) && response.contains("no network resolution"),
        "the response must state that nothing was resolved: {response}"
    );

    // Give any stray request time to arrive before concluding none did.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "extraction must never fetch a URL the message names"
    );
}

#[tokio::test]
async fn quoted_printable_urls_are_decoded_before_extraction() {
    // The defect this pins: scanning the stored bytes rather than the decoded
    // part reports `?a=3D1`, a URL the message never contained. Quoted-printable
    // is what a real mail client sends, so this is the common case, not an edge.
    let app = Harness::start().await;

    let response = app
        .deliver_and_extract(
            "From: sender@example.test\r\n\
             To: dev@example.test\r\n\
             Subject: qp\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Content-Transfer-Encoding: quoted-printable\r\n\
             \r\n\
             <a href=3D\"https://example.test/p?a=3D1&amp;b=3D2\">go</a>",
        )
        .await;

    assert!(
        response.contains(r#""url":"https://example.test/p?a=1&b=2""#),
        "expected the decoded URL, got {response}"
    );
}

#[tokio::test]
async fn embedded_and_insecure_urls_are_marked_as_facts_not_verdicts() {
    let app = Harness::start().await;

    let response = app
        .deliver_and_extract(&html_message(
            r#"<img src="cid:logo@example.test">
               <a href="http://plain.example.test/">plain</a>
               <a href="https://secure.example.test/">secure</a>"#,
        ))
        .await;

    assert!(
        response.contains(r#""embedded":true"#),
        "cid: is carried in the message"
    );
    assert!(
        response.contains(r#""insecure":true"#),
        "http: is not encrypted"
    );

    // A `cid:` URL has no host, so it must not appear among the network
    // destinations the panel summarises.
    assert!(
        response.contains(r#""hosts":["plain.example.test","secure.example.test"]"#),
        "cid: must not contribute a host: {response}"
    );
}

#[tokio::test]
async fn a_plain_text_message_reports_an_empty_extraction_not_an_error() {
    // "This message references nothing" is an answer, and a 404 would not be one.
    let app = Harness::start().await;

    let response = app
        .deliver_and_extract(
            "From: sender@example.test\r\n\
             To: dev@example.test\r\n\
             Subject: plain\r\n\
             \r\n\
             No HTML here, just https://example.test/ in the text.",
        )
        .await;

    assert!(
        response.contains(r#""links":[]"#),
        "expected an empty list in {response}"
    );
    assert!(response.contains(r#""hosts":[]"#));
}

#[tokio::test]
async fn multipart_alternative_attributes_links_to_the_html_part() {
    let app = Harness::start().await;

    let response = app
        .deliver_and_extract(
            "From: sender@example.test\r\n\
             To: dev@example.test\r\n\
             Subject: alt\r\n\
             Content-Type: multipart/alternative; boundary=\"b\"\r\n\
             \r\n\
             --b\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             See https://text.example.test/ — not HTML, so not extracted.\r\n\
             --b\r\n\
             Content-Type: text/html\r\n\
             \r\n\
             <a href=\"https://html.example.test/\">go</a>\r\n\
             --b--",
        )
        .await;

    // Only the HTML part contributes: a URL in plain text is not a reference
    // the client would act on, and reporting it would blur that distinction.
    assert!(response.contains("https://html.example.test/"));
    assert!(
        !response.contains("text.example.test"),
        "a plain-text URL is not an HTML reference: {response}"
    );
    assert!(
        response.contains(r#""part_path":"#),
        "every link names the part it came from: {response}"
    );
}
