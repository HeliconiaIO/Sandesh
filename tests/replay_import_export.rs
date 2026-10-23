//! Replay, and the export/import round trip (Phase 8 A5).
//!
//! The acceptance criterion is one sentence — "exported messages re-import
//! byte-identically" — and it is the only Phase 8 criterion that can fail
//! *silently*. A fault that does not fire is visible; a webhook that does not
//! retry is visible; a round trip that corrupts one line in one message of a
//! hundred looks exactly like a round trip that worked.
//!
//! So the corpus test below runs all 102 `testdata/` fixtures through the real
//! HTTP surface and compares bytes, and the corpus is enumerated **from disk**
//! (the rule ADR-063 established) so it grows with the fixtures rather than
//! against a number written here. The `>From `-quoting case is asserted
//! separately as well, because it is the one `mboxo` would fail and a corpus
//! that happened to contain no such line would let that bug through.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::path::PathBuf;

use sandesh_app::Application;
use sandesh_config::AppConfig;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

struct Harness {
    http: SocketAddr,
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
            // Every listener on an ephemeral port: these tests run in parallel
            // and the defaults are fixed ports, so two harnesses would collide.
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
        let shutdown = app.shutdown_token();

        tokio::spawn(async move {
            let _ = app.run().await;
        });

        Self {
            http,
            shutdown,
            _dir: dir,
        }
    }

    /// Sends a request with a raw byte body and returns the raw response.
    async fn request_bytes(
        &self,
        method: &str,
        path: &str,
        content_type: &str,
        body: &[u8],
    ) -> Vec<u8> {
        let mut stream = TcpStream::connect(self.http).await.expect("connects");
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: {content_type}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .await
            .expect("writes the head");
        stream.write_all(body).await.expect("writes the body");

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("reads the response");
        response
    }

    async fn get(&self, path: &str) -> Vec<u8> {
        self.request_bytes("GET", path, "application/octet-stream", b"")
            .await
    }

    async fn get_text(&self, path: &str) -> String {
        String::from_utf8_lossy(&self.get(path).await).into_owned()
    }
}

/// Splits an HTTP response into its status line and its body bytes.
///
/// The body is compared byte-for-byte, so it must not go through a lossy
/// string conversion: a fixture in `encodings/` is not valid UTF-8 and
/// `from_utf8_lossy` would replace exactly the bytes under test.
fn split_response(response: &[u8]) -> (String, Vec<u8>) {
    let separator = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a header/body separator");
    let head = String::from_utf8_lossy(&response[..separator]).into_owned();
    let status = head.lines().next().unwrap_or_default().to_owned();
    (status, response[separator + 4..].to_vec())
}

/// Every `.eml` fixture in `testdata/`, enumerated from disk (ADR-063).
fn corpus() -> Vec<(String, Vec<u8>)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut fixtures = Vec::new();

    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("testdata is readable") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "eml") {
                let name = path
                    .strip_prefix(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                fixtures.push((name, std::fs::read(&path).expect("a readable fixture")));
            }
        }
    }

    fixtures.sort_by(|a, b| a.0.cmp(&b.0));
    fixtures
}

#[tokio::test]
async fn corpus_round_trips_through_mbox_byte_identically() {
    let fixtures = corpus();
    assert!(
        fixtures.len() >= 102,
        "the corpus should hold at least 102 fixtures, found {}",
        fixtures.len(),
    );

    let harness = Harness::start().await;

    // Import the whole corpus as one mbox, exactly as a developer restoring a
    // saved corpus would. Framing it here rather than one message per request
    // is the point: the boundaries are what the round trip has to survive.
    let mut mbox = Vec::new();
    for (_, raw) in &fixtures {
        mbox.extend_from_slice(&sandesh_core::write_mbox_entry(
            raw,
            sandesh_core::Timestamp::now(),
        ));
    }

    let response = harness
        .request_bytes(
            "POST",
            "/api/v1/import?format=mbox",
            "application/mbox",
            &mbox,
        )
        .await;
    let (status, body) = split_response(&response);
    assert!(
        status.contains("200"),
        "import should succeed, got {status}: {}",
        String::from_utf8_lossy(&body),
    );

    let report = String::from_utf8_lossy(&body);
    assert!(
        report.contains(&format!("\"imported\":{}", fixtures.len())),
        "every fixture should import; got {report}",
    );

    // Export it all back out and compare, message for message.
    let exported = harness
        .get(&format!(
            "/api/v1/export?format=mbox&limit={}",
            fixtures.len()
        ))
        .await;
    let (status, mbox_out) = split_response(&exported);
    assert!(
        status.contains("200"),
        "export should succeed, got {status}"
    );

    let recovered = sandesh_core::parse_mbox(&mbox_out);
    assert_eq!(
        recovered.len(),
        fixtures.len(),
        "export should frame every imported message",
    );

    // Order is newest-first on export and the corpus went in oldest-first, so
    // compare as sets of bytes rather than pairwise by position: the criterion
    // is that every message survives, not that export preserves input order.
    let mut expected: Vec<Vec<u8>> = fixtures.iter().map(|(_, raw)| raw.clone()).collect();
    let mut actual = recovered;
    expected.sort();
    actual.sort();

    for (index, (want, got)) in expected.iter().zip(actual.iter()).enumerate() {
        assert_eq!(
            want,
            got,
            "fixture {index} did not survive the round trip\n  wrote {} bytes, read {} bytes",
            want.len(),
            got.len(),
        );
    }

    harness.shutdown.cancel();
}

#[tokio::test]
async fn from_quoting_survives_the_round_trip() {
    // The `mboxo` failure case, asserted directly. A body line beginning
    // `From ` must come back unchanged, and one already beginning `>From `
    // must not gain or lose a `>`.
    let harness = Harness::start().await;

    let message = b"From: sender@example.test\r\nTo: dev@example.test\r\n\
                    Subject: quoting\r\n\r\n\
                    From the desk of Alice\r\n\
                    >From an already quoted line\r\n\
                    >>From a doubly quoted line\r\n\
                    Fromage is not a separator\r\n"
        .as_slice();

    let mbox = sandesh_core::write_mbox_entry(message, sandesh_core::Timestamp::now());

    // The written form must quote, or the export is ambiguous to any reader.
    assert!(
        find(&mbox, b"\n>From the desk of Alice").is_some(),
        "a bare `From ` body line must be quoted on write",
    );
    assert!(
        find(&mbox, b"\n>>From an already quoted line").is_some(),
        "an already-quoted line must gain a level — this is the mboxrd rule",
    );

    let response = harness
        .request_bytes(
            "POST",
            "/api/v1/import?format=mbox",
            "application/mbox",
            &mbox,
        )
        .await;
    let (status, _) = split_response(&response);
    assert!(
        status.contains("200"),
        "import should succeed, got {status}"
    );

    let exported = harness.get("/api/v1/export?format=mbox").await;
    let (_, mbox_out) = split_response(&exported);
    let recovered = sandesh_core::parse_mbox(&mbox_out);

    assert_eq!(recovered.len(), 1, "one message in, one message out");
    assert_eq!(
        recovered[0], message,
        "the `From `-quoted lines must come back exactly as written",
    );

    harness.shutdown.cancel();
}

#[tokio::test]
async fn eml_import_and_export_round_trip() {
    let harness = Harness::start().await;

    let message = b"From: sender@example.test\r\nTo: dev@example.test\r\n\
                    Subject: single\r\n\r\nbody\r\n"
        .as_slice();

    let response = harness
        .request_bytes(
            "POST",
            "/api/v1/import?format=eml",
            "message/rfc822",
            message,
        )
        .await;
    let (status, body) = split_response(&response);
    assert!(
        status.contains("200"),
        "import should succeed, got {status}"
    );

    let report = String::from_utf8_lossy(&body);
    assert!(report.contains("\"imported\":1"), "got {report}");

    let exported = harness.get("/api/v1/export?format=eml").await;
    let (status, raw) = split_response(&exported);
    assert!(
        status.contains("200"),
        "export should succeed, got {status}"
    );
    assert_eq!(raw, message, "a single-message .eml export is the bytes");

    harness.shutdown.cancel();
}

#[tokio::test]
async fn replay_creates_a_new_message_with_recorded_provenance() {
    let harness = Harness::start().await;

    let original = b"From: sender@example.test\r\nTo: dev@example.test\r\n\
                     Message-ID: <original@example.test>\r\n\
                     Subject: replay me\r\n\r\nbody\r\n"
        .as_slice();

    let response = harness
        .request_bytes("POST", "/api/v1/messages", "message/rfc822", original)
        .await;
    let (status, body) = split_response(&response);
    assert!(status.contains("201"), "injection should succeed: {status}");

    let injected = String::from_utf8_lossy(&body);
    let source_id = json_string(&injected, "id").expect("the injected message id");

    // Replay it.
    let response = harness
        .request_bytes(
            "POST",
            &format!("/api/v1/messages/{source_id}/replay"),
            "application/json",
            b"",
        )
        .await;
    let (status, body) = split_response(&response);
    assert!(status.contains("201"), "replay should succeed: {status}");

    let replayed = String::from_utf8_lossy(&body);
    let replay_id = json_string(&replayed, "id").expect("the replayed message id");

    assert_ne!(
        replay_id, source_id,
        "a replay is a new message, not a second reference to the old one",
    );

    // Half one of the decision: the provenance travels in the bytes.
    let raw = harness
        .get(&format!("/api/v1/messages/{replay_id}/raw"))
        .await;
    let (_, raw_body) = split_response(&raw);
    let raw_text = String::from_utf8_lossy(&raw_body);

    assert!(
        raw_text.contains("X-Sandesh-Replay-Of: <original@example.test>"),
        "the replayed bytes should name their source; got:\n{raw_text}",
    );
    assert!(
        !raw_text.contains("Message-ID: <original@example.test>"),
        "the replay must not reuse the original Message-ID:\n{raw_text}",
    );
    assert_eq!(
        raw_text.matches("Message-ID:").count(),
        1,
        "exactly one Message-ID, per RFC 5322:\n{raw_text}",
    );
    assert!(
        raw_text.contains("Subject: replay me") && raw_text.contains("body"),
        "the replay must carry the original content:\n{raw_text}",
    );

    // Half two: the same link is queryable on the delivery record.
    let delivery = harness
        .get_text(&format!("/api/v1/messages/{replay_id}/delivery"))
        .await;
    assert!(
        delivery.contains(&source_id),
        "the delivery record should name the replayed-from message; got:\n{delivery}",
    );

    harness.shutdown.cancel();
}

#[tokio::test]
async fn replaying_a_message_without_a_message_id_still_works() {
    // A malformed fixture may carry no Message-ID at all. Replay must give the
    // new message one and simply record no provenance header, rather than
    // failing or writing an empty one.
    let harness = Harness::start().await;

    let original =
        b"From: sender@example.test\r\nTo: dev@example.test\r\nSubject: no id\r\n\r\nbody\r\n"
            .as_slice();

    let response = harness
        .request_bytes("POST", "/api/v1/messages", "message/rfc822", original)
        .await;
    let (_, body) = split_response(&response);
    let source_id =
        json_string(&String::from_utf8_lossy(&body), "id").expect("the injected message id");

    let response = harness
        .request_bytes(
            "POST",
            &format!("/api/v1/messages/{source_id}/replay"),
            "application/json",
            b"",
        )
        .await;
    let (status, body) = split_response(&response);
    assert!(status.contains("201"), "replay should succeed: {status}");

    let replay_id =
        json_string(&String::from_utf8_lossy(&body), "id").expect("the replayed message id");
    let raw = harness
        .get(&format!("/api/v1/messages/{replay_id}/raw"))
        .await;
    let (_, raw_body) = split_response(&raw);
    let raw_text = String::from_utf8_lossy(&raw_body);

    assert!(
        raw_text.contains("Message-ID:"),
        "the replay gets a fresh Message-ID even when the source had none",
    );
    assert!(
        !raw_text.contains("X-Sandesh-Replay-Of:"),
        "no source Message-ID means no provenance header, not an empty one",
    );

    // The delivery record still carries the link, which is why recording it in
    // two places rather than one is worth the column.
    let delivery = harness
        .get_text(&format!("/api/v1/messages/{replay_id}/delivery"))
        .await;
    assert!(
        delivery.contains(&source_id),
        "the queryable half holds even when the header cannot:\n{delivery}",
    );

    harness.shutdown.cancel();
}

#[tokio::test]
async fn import_rejects_a_payload_that_frames_into_nothing() {
    let harness = Harness::start().await;

    let (status, _) = split_response(
        &harness
            .request_bytes(
                "POST",
                "/api/v1/import?format=mbox",
                "application/mbox",
                b"this is not an mbox\r\n",
            )
            .await,
    );
    assert!(
        status.contains("400"),
        "a payload with no `From ` separator is a client error, got {status}",
    );

    let (status, _) = split_response(
        &harness
            .request_bytes(
                "POST",
                "/api/v1/import?format=mbox",
                "application/mbox",
                b"",
            )
            .await,
    );
    assert!(
        status.contains("400"),
        "an empty body is rejected: {status}"
    );

    let (status, _) = split_response(
        &harness
            .request_bytes(
                "POST",
                "/api/v1/import?format=maildir",
                "application/mbox",
                b"anything",
            )
            .await,
    );
    assert!(
        status.contains("400"),
        "an unsupported format is refused rather than guessed at, got {status}",
    );

    harness.shutdown.cancel();
}

#[tokio::test]
async fn replaying_a_missing_message_is_a_404() {
    let harness = Harness::start().await;

    // A well-formed UUID that names nothing.
    let missing = "019a0000-0000-7000-8000-000000000000";
    let (status, _) = split_response(
        &harness
            .request_bytes(
                "POST",
                &format!("/api/v1/messages/{missing}/replay"),
                "application/json",
                b"",
            )
            .await,
    );
    assert!(status.contains("404"), "expected 404, got {status}");

    let (status, _) = split_response(
        &harness
            .request_bytes(
                "POST",
                "/api/v1/messages/not-a-uuid/replay",
                "application/json",
                b"",
            )
            .await,
    );
    assert!(status.contains("400"), "expected 400, got {status}");

    harness.shutdown.cancel();
}

#[tokio::test]
async fn import_into_target_mailbox_directly() {
    let harness = Harness::start().await;

    // Fetch mailboxes to locate Drafts
    let response = harness.get("/api/v1/mailboxes").await;
    let (status, body) = split_response(&response);
    assert!(status.contains("200"), "get mailboxes failed: {status}");
    let body_str = String::from_utf8_lossy(&body);

    let drafts_pos = body_str
        .find("\"name\":\"Drafts\"")
        .expect("Drafts mailbox exists");
    let slice_before = &body_str[..drafts_pos];
    let id_marker = "\"id\":\"";
    let last_id_pos = slice_before.rfind(id_marker).expect("id before Drafts");
    let id_start = last_id_pos + id_marker.len();
    let id_end = body_str[id_start..].find('"').expect("id ends") + id_start;
    let drafts_id = &body_str[id_start..id_end];

    let message = b"From sender@example.test Sun Sep 17 00:00:00 2026\r\n\
                    From: sender@example.test\r\n\
                    To: dev@example.test\r\n\
                    Subject: A draft message\r\n\r\n\
                    Draft content\r\n"
        .as_slice();

    let import_url = format!("/api/v1/import?format=mbox&mailbox_id={drafts_id}");
    let import_resp = harness
        .request_bytes("POST", &import_url, "application/mbox", message)
        .await;
    let (import_status, import_body) = split_response(&import_resp);
    assert!(
        import_status.contains("200"),
        "import into Drafts failed: {import_status}"
    );
    let report = String::from_utf8_lossy(&import_body);
    assert!(
        report.contains("\"imported\":1"),
        "expected 1 imported: {report}"
    );

    // Verify the message landed in Drafts and has \Draft and \Seen flags
    let entries_url = format!("/api/v1/mailboxes/{drafts_id}/entries");
    let entries_resp = harness.get(&entries_url).await;
    let (entries_status, entries_body) = split_response(&entries_resp);
    assert!(
        entries_status.contains("200"),
        "get entries failed: {entries_status}"
    );
    let entries_str = String::from_utf8_lossy(&entries_body);
    assert!(entries_str.contains("\\\\Draft"));
    assert!(entries_str.contains("\\\\Seen"));

    let msg_id = json_string(&entries_str, "message_id").expect("message_id in entry");
    let msg_resp = harness.get(&format!("/api/v1/messages/{msg_id}")).await;
    let (msg_status, msg_body) = split_response(&msg_resp);
    assert!(
        msg_status.contains("200"),
        "get message failed: {msg_status}"
    );
    let msg_str = String::from_utf8_lossy(&msg_body);
    assert!(msg_str.contains("A draft message"));

    // Verify invalid mailbox_id returns 400
    let bad_url = "/api/v1/import?format=mbox&mailbox_id=not-a-uuid";
    let bad_resp = harness
        .request_bytes("POST", bad_url, "application/mbox", message)
        .await;
    let (bad_status, _) = split_response(&bad_resp);
    assert!(
        bad_status.contains("400"),
        "expected 400 for bad mailbox_id, got {bad_status}"
    );

    harness.shutdown.cancel();
}

/// The first index at which `needle` occurs in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Pulls a string field out of a JSON body without a parser dependency.
fn json_string(body: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\":\"");
    let start = body.find(&key)? + key.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}
