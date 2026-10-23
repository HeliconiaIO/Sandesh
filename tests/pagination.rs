//! Cursor pagination over the message list, as a client actually walks it.
//!
//! Phase 3 lists "cursor pagination is stable under concurrent insertion" as a
//! met acceptance criterion, and Phase 4 asks the list to stay usable at 10,000
//! messages. Neither had a test: nothing anywhere asserted that following
//! `next_cursor` reaches the *end* of a mailbox, so a cursor that walked the
//! wrong way passed every suite in the workspace while truncating every mailbox
//! at two pages.
//!
//! That is the defect these pin. The list is ordered newest-first and UIDs
//! ascend with age, so the cursor must walk *down*. Reading it as `uid > after`
//! returns rows the client has already seen and then reports the list finished.
//!
//! The assertion that matters is not "a second page exists" — the broken code
//! produced one of those. It is that **walking every page yields each message
//! exactly once, and reaches all of them**.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::HashSet;
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
        let text = String::from_utf8_lossy(&response).into_owned();
        text.split_once("\r\n\r\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or(text)
    }

    /// Delivers `count` messages over one real SMTP session.
    async fn deliver_many(&self, count: usize) {
        let mut smtp = TcpStream::connect(self.smtp).await.expect("connects");
        let mut buffer = [0_u8; 4096];

        async fn expect(stream: &mut TcpStream, buffer: &mut [u8], prefix: &str) {
            let read = stream.read(buffer).await.expect("reads a reply");
            let reply = String::from_utf8_lossy(&buffer[..read]).into_owned();
            assert!(reply.starts_with(prefix), "expected {prefix}, got {reply}");
        }

        expect(&mut smtp, &mut buffer, "220").await;
        smtp.write_all(b"EHLO test.local\r\n")
            .await
            .expect("writes");
        expect(&mut smtp, &mut buffer, "250").await;

        for index in 0..count {
            for (command, code) in [
                ("MAIL FROM:<sender@example.test>\r\n".to_owned(), "250"),
                ("RCPT TO:<dev@example.test>\r\n".to_owned(), "250"),
                ("DATA\r\n".to_owned(), "354"),
            ] {
                smtp.write_all(command.as_bytes()).await.expect("writes");
                expect(&mut smtp, &mut buffer, code).await;
            }
            let body = format!(
                "From: sender@example.test\r\nTo: dev@example.test\r\nSubject: msg {index}\r\n\r\nbody {index}\r\n.\r\n"
            );
            smtp.write_all(body.as_bytes()).await.expect("writes");
            expect(&mut smtp, &mut buffer, "250").await;
        }
        smtp.write_all(b"QUIT\r\n").await.expect("writes QUIT");
    }

    /// Waits until the list reports at least `count` messages.
    async fn await_count(&self, count: usize) {
        for _ in 0..80 {
            let body = self.get("/api/v1/messages?limit=1").await;
            if field_number(&body, "total_estimate").is_some_and(|total| total as usize >= count) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {count} messages to be queryable");
    }

    /// Walks every page, returning the message ids in the order seen.
    ///
    /// Deliberately unbounded-ish: a cursor that fails to advance would loop,
    /// so the guard is a page cap that panics rather than a silent break.
    async fn walk_all_pages(&self, limit: usize) -> Vec<String> {
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..500 {
            let path = match &cursor {
                Some(value) => format!("/api/v1/messages?limit={limit}&cursor={value}"),
                None => format!("/api/v1/messages?limit={limit}"),
            };
            let body = self.get(&path).await;
            seen.extend(field_strings(&body, "id"));

            match field_string(&body, "next_cursor") {
                Some(next) => cursor = Some(next),
                None => return seen,
            }
        }
        panic!("cursor never terminated — it is not advancing");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Every `"<name>":"<value>"` in the body, in order.
fn field_strings(body: &str, name: &str) -> Vec<String> {
    let needle = format!("\"{name}\":\"");
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find(&needle) {
        rest = &rest[at + needle.len()..];
        if let Some((value, remainder)) = rest.split_once('"') {
            out.push(value.to_owned());
            rest = remainder;
        }
    }
    out
}

/// The first `"<name>":"<value>"`, or `None` when the field is `null`/absent.
fn field_string(body: &str, name: &str) -> Option<String> {
    field_strings(body, name).into_iter().next()
}

/// The first `"<name>":<number>`.
fn field_number(body: &str, name: &str) -> Option<u64> {
    let needle = format!("\"{name}\":");
    let at = body.find(&needle)? + needle.len();
    let rest = &body[at..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[tokio::test]
async fn walking_the_cursor_reaches_every_message_exactly_once() {
    // The whole criterion in one assertion. With the cursor read in the wrong
    // direction this stops at 2 pages and sees 99 of 120 — and the duplicates
    // assertion fails first, because page 2 re-serves page 1.
    let app = Harness::start().await;
    app.deliver_many(120).await;
    app.await_count(120).await;

    let seen = app.walk_all_pages(50).await;

    let unique: HashSet<&String> = seen.iter().collect();
    assert_eq!(
        unique.len(),
        seen.len(),
        "a page re-served messages the client already had: {} ids, {} distinct",
        seen.len(),
        unique.len()
    );
    assert_eq!(
        seen.len(),
        120,
        "walking every page must reach all 120 messages, saw {}",
        seen.len()
    );
}

#[tokio::test]
async fn pages_are_ordered_newest_first_across_the_cursor() {
    // Ordering must hold *between* pages, not only within one. A cursor that
    // walks the wrong way still returns sorted pages, so within-page ordering
    // cannot catch it.
    let app = Harness::start().await;
    app.deliver_many(60).await;
    app.await_count(60).await;

    let paged = app.walk_all_pages(10).await;
    let single = field_strings(&app.get("/api/v1/messages?limit=200").await, "id");

    assert_eq!(
        paged, single,
        "the paged walk must yield the same order as one unpaginated read"
    );
}

#[tokio::test]
async fn the_last_page_reports_no_cursor() {
    // "No more pages" is the absence of a cursor, and it must mean the list is
    // genuinely exhausted rather than that the walk gave up.
    let app = Harness::start().await;
    app.deliver_many(25).await;
    app.await_count(25).await;

    let body = app.get("/api/v1/messages?limit=50").await;
    assert!(
        field_string(&body, "next_cursor").is_none(),
        "a single page holding every message must not offer a cursor: {body}"
    );
}

#[tokio::test]
async fn a_message_delivered_mid_walk_does_not_shift_the_pages() {
    // The Phase 3 criterion: stable under concurrent insertion. A UID cursor
    // gives this for free where an offset would not — a newly delivered message
    // sorts newest-first, which is *before* the cursor, so it cannot displace
    // rows the client has already walked past.
    let app = Harness::start().await;
    app.deliver_many(60).await;
    app.await_count(60).await;

    let first = app.get("/api/v1/messages?limit=20").await;
    let first_ids = field_strings(&first, "id");
    let cursor = field_string(&first, "next_cursor").expect("more pages exist");

    // Insert while the client holds a cursor.
    app.deliver_many(5).await;
    app.await_count(65).await;

    let second = app
        .get(&format!("/api/v1/messages?limit=20&cursor={cursor}"))
        .await;
    let second_ids = field_strings(&second, "id");

    for id in &second_ids {
        assert!(
            !first_ids.contains(id),
            "the second page re-served {id} after a concurrent delivery"
        );
    }
}
