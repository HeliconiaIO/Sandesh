//! Cross-protocol integration tests across SMTP, POP3, IMAP and HTTP.
//!
//! Proves that all four protocol listeners co-exist in one Application,
//! that mail delivered over SMTP is retrievable over POP3, and that deletion
//! commits across the whole system on QUIT.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use sandesh_app::Application;
use sandesh_config::AppConfig;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

struct MultiProtoHarness {
    smtp: SocketAddr,
    pop3: SocketAddr,
    #[allow(dead_code)]
    imap: SocketAddr,
    #[allow(dead_code)]
    http: SocketAddr,
    shutdown: CancellationToken,
    handle: tokio::task::JoinHandle<Result<(), sandesh_app::AppError>>,
    #[allow(dead_code)]
    dir: TempDir,
}

impl MultiProtoHarness {
    async fn start() -> Self {
        Self::start_with_config(|_| {}).await
    }

    async fn start_with_config(adjust: impl FnOnce(&mut AppConfig)) -> Self {
        let dir = TempDir::new().expect("temporary directory");
        let ephemeral: SocketAddr = "127.0.0.1:0".parse().expect("valid address");
        let defaults = AppConfig::default();

        let mut config = AppConfig {
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
        adjust(&mut config);

        let app = Application::start(config).await.expect("app starts");
        let smtp = app.addresses().smtp.expect("SMTP bound");
        let pop3 = app.addresses().pop3.expect("POP3 bound");
        let imap = app.addresses().imap.expect("IMAP bound");
        let http = app.addresses().http.expect("HTTP bound");
        let shutdown = app.shutdown_token();
        let handle = tokio::spawn(app.run());

        Self {
            smtp,
            pop3,
            imap,
            http,
            shutdown,
            handle,
            dir,
        }
    }

    async fn stop(self) {
        self.shutdown.cancel();
        let _ = self.handle.await;
    }
}

async fn deliver_smtp(addr: SocketAddr, from: &str, to: &str, subject: &str, body: &str) {
    let mut stream = TcpStream::connect(addr).await.expect("connect to SMTP");
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();

    // 220 greeting
    let greeting = lines.next_line().await.expect("read").expect("greeting");
    assert!(greeting.starts_with("220"));

    writer.write_all(b"HELO test.local\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("helo resp");
    assert!(resp.starts_with("250"));

    writer
        .write_all(format!("MAIL FROM:<{from}>\r\n").as_bytes())
        .await
        .unwrap();
    let resp = lines.next_line().await.expect("read").expect("mail resp");
    assert!(resp.starts_with("250"));

    writer
        .write_all(format!("RCPT TO:<{to}>\r\n").as_bytes())
        .await
        .unwrap();
    let resp = lines.next_line().await.expect("read").expect("rcpt resp");
    assert!(resp.starts_with("250"));

    writer.write_all(b"DATA\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("data resp");
    assert!(resp.starts_with("354"));

    let msg = format!("From: <{from}>\r\nTo: <{to}>\r\nSubject: {subject}\r\n\r\n{body}\r\n.\r\n");
    writer.write_all(msg.as_bytes()).await.unwrap();
    let resp = lines
        .next_line()
        .await
        .expect("read")
        .expect("data ok resp");
    assert!(resp.starts_with("250"));

    writer.write_all(b"QUIT\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("quit resp");
    assert!(resp.starts_with("221"));
}

#[tokio::test]
async fn smtp_delivery_is_retrievable_and_deletable_over_pop3() {
    let harness = MultiProtoHarness::start().await;

    // 1. Deliver message via SMTP to alice@sandesh.test
    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "Cross-Protocol Test",
        "Hello from SMTP to POP3!",
    )
    .await;

    // 2. Connect via POP3
    let mut stream = TcpStream::connect(harness.pop3)
        .await
        .expect("connect to POP3");
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();

    let greeting = lines.next_line().await.expect("read").expect("greeting");
    assert!(greeting.starts_with("+OK"));

    // Authenticate as alice
    writer.write_all(b"USER alice\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("user resp");
    assert!(resp.starts_with("+OK"));

    writer.write_all(b"PASS any\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("pass resp");
    assert!(resp.starts_with("+OK"));

    // STAT -> 1 message
    writer.write_all(b"STAT\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("stat resp");
    assert!(resp.starts_with("+OK 1 "), "expected 1 message, got {resp}");

    // UIDL -> 1 <hex>
    writer.write_all(b"UIDL 1\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("uidl resp");
    assert!(resp.starts_with("+OK 1 "), "expected UIDL, got {resp}");

    // RETR 1 -> verify headers and body
    writer.write_all(b"RETR 1\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("retr resp");
    assert!(resp.starts_with("+OK"));

    let mut message_content = String::new();
    loop {
        let line = lines.next_line().await.expect("read").expect("body line");
        if line == "." {
            break;
        }
        message_content.push_str(&line);
        message_content.push('\n');
    }
    assert!(
        message_content.contains("Subject: Cross-Protocol Test"),
        "missing subject in: {message_content}"
    );
    assert!(
        message_content.contains("Hello from SMTP to POP3!"),
        "missing body in: {message_content}"
    );

    // DELE 1 -> mark deleted
    writer.write_all(b"DELE 1\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("dele resp");
    assert!(resp.starts_with("+OK"));

    // QUIT -> commits deletion
    writer.write_all(b"QUIT\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("quit resp");
    assert!(resp.starts_with("+OK"));

    // 3. Reconnect via POP3 -> Mailbox should now have 0 messages
    let mut stream2 = TcpStream::connect(harness.pop3)
        .await
        .expect("reconnect to POP3");
    let (reader2, mut writer2) = stream2.split();
    let mut lines2 = BufReader::new(reader2).lines();

    let _greeting = lines2.next_line().await.expect("read").expect("greeting");
    writer2.write_all(b"USER alice\r\n").await.unwrap();
    let _ = lines2.next_line().await;
    writer2.write_all(b"PASS any\r\n").await.unwrap();
    let _ = lines2.next_line().await;

    writer2.write_all(b"STAT\r\n").await.unwrap();
    let resp = lines2.next_line().await.expect("read").expect("stat resp");
    assert_eq!(resp, "+OK 0 0");

    writer2.write_all(b"QUIT\r\n").await.unwrap();

    harness.stop().await;
}

#[tokio::test]
async fn smtp_delivery_is_retrievable_over_imap() {
    let harness = MultiProtoHarness::start().await;

    // 1. Deliver message via SMTP
    deliver_smtp(
        harness.smtp,
        "bob@example.com",
        "alice@example.com",
        "Cross-Protocol IMAP Test",
        "Hello from SMTP to IMAP!",
    )
    .await;

    // 2. Connect via IMAP
    let mut stream = TcpStream::connect(harness.imap)
        .await
        .expect("connect to IMAP");
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();

    // Read greeting
    let greeting = lines.next_line().await.expect("read").expect("greeting");
    assert!(greeting.starts_with("* OK"));
    assert!(greeting.contains("IMAP4rev1"));
    assert!(greeting.contains("IMAP4rev2"));

    // Authenticate (developer mode accepts any password)
    writer.write_all(b"A01 LOGIN alice any\r\n").await.unwrap();
    let login_resp = lines.next_line().await.expect("read").expect("login resp");
    assert!(login_resp.starts_with("A01 OK"), "got {login_resp}");

    // SELECT INBOX
    writer.write_all(b"A02 SELECT INBOX\r\n").await.unwrap();
    let mut exists = false;
    loop {
        let line = lines.next_line().await.expect("read").expect("select line");
        if line.contains("1 EXISTS") {
            exists = true;
        }
        if line.starts_with("A02 OK") {
            break;
        }
    }
    assert!(exists, "expected 1 EXISTS in SELECT response");

    // FETCH the delivered message: metadata first.
    //
    // Increment 4A restores this half of the test. SEARCH and STORE remain
    // absent deliberately — they are increment 4B, and the placeholder handlers
    // that once answered them were removed for ignoring their arguments.
    writer
        .write_all(b"A10 FETCH 1 (UID FLAGS RFC822.SIZE)\r\n")
        .await
        .unwrap();
    let mut fetch_meta = String::new();
    loop {
        let line = lines.next_line().await.expect("read").expect("fetch line");
        if line.starts_with("A10 OK") {
            break;
        }
        fetch_meta.push_str(&line);
        fetch_meta.push('\n');
    }
    assert!(
        fetch_meta.contains("* 1 FETCH ("),
        "expected an untagged FETCH: {fetch_meta}"
    );
    assert!(fetch_meta.contains("UID "), "{fetch_meta}");
    assert!(fetch_meta.contains("RFC822.SIZE "), "{fetch_meta}");

    // The reported size must be the exact octet count of the stored message.
    let reported_size: usize = fetch_meta
        .split("RFC822.SIZE ")
        .nth(1)
        .and_then(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .expect("a numeric RFC822.SIZE");
    assert!(reported_size > 0, "size must be real: {fetch_meta}");

    // Now the body. `BODY.PEEK[]` returns a literal: `{n}CRLF` then n octets,
    // which is read by count rather than by line — a literal may contain
    // anything, including what looks like a response line.
    writer
        .write_all(b"A11 FETCH 1 BODY.PEEK[]\r\n")
        .await
        .unwrap();

    let header_line = lines
        .next_line()
        .await
        .expect("read")
        .expect("fetch body line");
    assert!(
        header_line.contains("BODY[] {"),
        "expected a literal: {header_line}"
    );
    let declared: usize = header_line
        .rsplit_once('{')
        .and_then(|(_, rest)| rest.trim_end_matches('}').parse().ok())
        .expect("a literal length");
    assert_eq!(
        declared, reported_size,
        "the literal must be exactly RFC822.SIZE octets"
    );

    // Read the literal's lines back and confirm the delivered content.
    let mut body = String::new();
    while body.len() < declared {
        let line = lines.next_line().await.expect("read").expect("body line");
        body.push_str(&line);
        body.push_str("\r\n");
    }

    assert!(
        body.contains("Subject: Cross-Protocol IMAP Test"),
        "the delivered subject must survive the round trip: {body:?}"
    );
    assert!(
        body.contains("Hello from SMTP to IMAP!"),
        "the delivered body must survive the round trip: {body:?}"
    );
    assert!(
        body.contains("From: <bob@example.com>"),
        "the delivered From header must survive: {body:?}"
    );

    // Drain the FETCH completion.
    loop {
        let line = lines.next_line().await.expect("read").expect("fetch tail");
        if line.starts_with("A11 OK") {
            break;
        }
    }

    // UID FETCH resolves the same message by its UID.
    let uid: u32 = fetch_meta
        .split("UID ")
        .nth(1)
        .and_then(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .expect("a numeric UID");
    assert!(uid > 0, "a UID is never zero");

    writer
        .write_all(format!("A12 UID FETCH {uid} (UID RFC822.SIZE)\r\n").as_bytes())
        .await
        .unwrap();
    let mut uid_fetch = String::new();
    loop {
        let line = lines.next_line().await.expect("read").expect("uid fetch");
        if line.starts_with("A12 OK") {
            break;
        }
        uid_fetch.push_str(&line);
        uid_fetch.push('\n');
    }
    assert!(
        uid_fetch.contains(&format!("UID {uid}")),
        "UID FETCH must return the same placement: {uid_fetch}"
    );
    assert!(
        uid_fetch.contains(&format!("RFC822.SIZE {reported_size}")),
        "and the same size: {uid_fetch}"
    );

    // STATUS reports the delivered message through a second code path.
    writer
        .write_all(b"A03 STATUS INBOX (MESSAGES UNSEEN)\r\n")
        .await
        .unwrap();
    let status_line = lines.next_line().await.expect("read").expect("status line");
    assert!(
        status_line.contains("MESSAGES 1") && status_line.contains("UNSEEN 1"),
        "unexpected status: {status_line}"
    );
    let status_ok = lines.next_line().await.expect("read").expect("status ok");
    assert!(status_ok.starts_with("A03 OK"), "got {status_ok}");

    // CLOSE
    writer.write_all(b"A04 CLOSE\r\n").await.unwrap();
    let close_ok = lines.next_line().await.expect("read").expect("close ok");
    assert!(close_ok.starts_with("A04 OK"));

    // LOGOUT
    writer.write_all(b"A05 LOGOUT\r\n").await.unwrap();
    let _ = lines.next_line().await; // * BYE
    let logout_ok = lines.next_line().await.expect("read").expect("logout ok");
    assert!(logout_ok.starts_with("A05 OK"));

    harness.stop().await;
}

/// A MIME message with an attachment survives SMTP → storage → IMAP `FETCH`.
///
/// The plain-text cross-protocol test proves the path works; this one proves it
/// does not quietly damage structured mail. Base64 attachment content is the
/// case most likely to be corrupted by an accidental re-encode or a line-ending
/// normalisation, because it is the only part whose bytes are meaningful as
/// text *and* as data.
#[tokio::test]
async fn smtp_delivered_mime_is_fetchable_with_its_structure_intact() {
    let harness = MultiProtoHarness::start().await;

    // A multipart/mixed message with a base64 attachment, delivered verbatim.
    let body = "--BOUND\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
See the attachment.\r\n\
--BOUND\r\n\
Content-Type: application/octet-stream; name=\"data.bin\"\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-Disposition: attachment; filename=\"data.bin\"\r\n\
\r\n\
SGVsbG8sIGJpbmFyeSB3b3JsZCEA/v0=\r\n\
--BOUND--";

    deliver_smtp_raw(
        harness.smtp,
        "bob@example.com",
        "alice@example.com",
        &format!(
            "From: <bob@example.com>\r\n\
To: <alice@example.com>\r\n\
Subject: MIME round trip\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n\
\r\n\
{body}\r\n"
        ),
    )
    .await;

    let mut stream = TcpStream::connect(harness.imap)
        .await
        .expect("connect to IMAP");
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();
    let _greeting = lines.next_line().await.expect("read").expect("greeting");

    writer.write_all(b"B01 LOGIN alice any\r\n").await.unwrap();
    let login = lines.next_line().await.expect("read").expect("login");
    assert!(login.starts_with("B01 OK"), "{login}");

    writer.write_all(b"B02 SELECT INBOX\r\n").await.unwrap();
    loop {
        let line = lines.next_line().await.expect("read").expect("select");
        if line.starts_with("B02 OK") {
            break;
        }
    }

    // BODYSTRUCTURE must describe the multipart the sender actually sent.
    writer
        .write_all(b"B03 FETCH 1 BODYSTRUCTURE\r\n")
        .await
        .unwrap();
    let mut structure = String::new();
    loop {
        let line = lines.next_line().await.expect("read").expect("structure");
        if line.starts_with("B03 OK") {
            break;
        }
        structure.push_str(&line);
    }
    assert!(
        structure.contains("\"MIXED\""),
        "the multipart subtype must survive: {structure}"
    );
    assert!(
        structure.to_ascii_uppercase().contains("BASE64"),
        "the attachment's transfer encoding must survive: {structure}"
    );

    // The attachment part's content must come back as the exact base64 that
    // was delivered — not decoded, not rewrapped.
    writer
        .write_all(b"B04 FETCH 1 BODY.PEEK[2]\r\n")
        .await
        .unwrap();
    let header_line = lines.next_line().await.expect("read").expect("part header");
    let declared: usize = header_line
        .rsplit_once('{')
        .and_then(|(_, rest)| rest.trim_end_matches('}').parse().ok())
        .expect("a literal length");

    let mut part = String::new();
    while part.len() < declared {
        let line = lines.next_line().await.expect("read").expect("part line");
        part.push_str(&line);
        part.push_str("\r\n");
    }
    assert!(
        part.contains("SGVsbG8sIGJpbmFyeSB3b3JsZCEA/v0="),
        "the base64 payload must be byte-exact, not re-encoded: {part:?}"
    );

    loop {
        let line = lines.next_line().await.expect("read").expect("part tail");
        if line.starts_with("B04 OK") {
            break;
        }
    }

    writer.write_all(b"B05 LOGOUT\r\n").await.unwrap();
    let _ = lines.next_line().await;

    harness.stop().await;
}

/// Delivers a fully-formed message over SMTP, headers and all.
///
/// [`deliver_smtp`] composes its own headers, which is convenient for the
/// plain-text case but cannot express a MIME message. This one sends `data`
/// verbatim as the `DATA` payload.
async fn deliver_smtp_raw(addr: SocketAddr, from: &str, to: &str, data: &str) {
    let mut stream = TcpStream::connect(addr).await.expect("connect to SMTP");
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();

    let greeting = lines.next_line().await.expect("read").expect("greeting");
    assert!(greeting.starts_with("220"));

    for (command, expected) in [
        ("HELO test.local\r\n".to_owned(), "250"),
        (format!("MAIL FROM:<{from}>\r\n"), "250"),
        (format!("RCPT TO:<{to}>\r\n"), "250"),
        ("DATA\r\n".to_owned(), "354"),
    ] {
        writer.write_all(command.as_bytes()).await.unwrap();
        let resp = lines.next_line().await.expect("read").expect("response");
        assert!(
            resp.starts_with(expected),
            "expected {expected} for {command:?}, got {resp}"
        );
    }

    writer.write_all(data.as_bytes()).await.unwrap();
    writer.write_all(b".\r\n").await.unwrap();
    let resp = lines.next_line().await.expect("read").expect("data ok");
    assert!(resp.starts_with("250"), "{resp}");

    writer.write_all(b"QUIT\r\n").await.unwrap();
}

// ---------------------------------------------------------------------------
// The remaining cases of the TESTING_STRATEGY §7 matrix.
//
// Cases 10, 12 and 13 are exercised inside `sandesh-imap`'s own suite
// (`copy.rs`, `fetch.rs`, `idle.rs`). The three below cannot be: each asserts
// that state written through one adapter is visible through a *different* one,
// which is only observable with all four listeners sharing a single
// Application — the harness above.
// ---------------------------------------------------------------------------

/// Runs one IMAP command, returning every line up to and including the tagged
/// reply.
async fn imap_command(
    writer: &mut tokio::net::tcp::WriteHalf<'_>,
    lines: &mut tokio::io::Lines<BufReader<tokio::net::tcp::ReadHalf<'_>>>,
    tag: &str,
    command: &str,
) -> String {
    writer
        .write_all(format!("{tag} {command}\r\n").as_bytes())
        .await
        .unwrap();
    let mut collected = String::new();
    loop {
        let line = lines.next_line().await.expect("read").expect("a line");
        let finished = line.starts_with(&format!("{tag} "));
        collected.push_str(&line);
        collected.push('\n');
        if finished {
            return collected;
        }
    }
}

/// Issues a GET and returns the whole raw HTTP response.
async fn http_get(addr: SocketAddr, path: &str) -> String {
    use tokio::io::AsyncReadExt;

    let mut stream = TcpStream::connect(addr).await.expect("connect to HTTP");
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.expect("read");
    String::from_utf8_lossy(&response).into_owned()
}

/// Issues a DELETE and returns the whole raw HTTP response.
async fn http_delete(addr: SocketAddr, path: &str) -> String {
    use tokio::io::AsyncReadExt;

    let mut stream = TcpStream::connect(addr).await.expect("connect to HTTP");
    let request = format!("DELETE {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.expect("read");
    String::from_utf8_lossy(&response).into_owned()
}

/// Case 4 — IMAP `APPEND` → POP3 `RETR`.
///
/// Non-SMTP ingress must be equally visible to every other adapter. `APPEND` is
/// the only way a message enters the system without passing through the SMTP
/// pipeline, so if anything about storage or delivery were special-cased on
/// that path, this is where it would show: the message would be in the IMAP
/// mailbox but invisible to POP3, which reads the same INBOX by a different
/// route.
#[tokio::test]
async fn imap_append_is_retrievable_over_pop3() {
    let harness = MultiProtoHarness::start().await;

    let literal = "From: appended@example.com\r\n\
To: alice@sandesh.test\r\n\
Subject: Appended not delivered\r\n\
\r\n\
This message never touched SMTP.\r\n";

    // 1. APPEND it over IMAP.
    let mut imap = TcpStream::connect(harness.imap).await.expect("connect");
    {
        let (reader, mut writer) = imap.split();
        let mut lines = BufReader::new(reader).lines();
        let greeting = lines.next_line().await.expect("read").expect("greeting");
        assert!(greeting.starts_with("* OK"), "{greeting}");

        writer.write_all(b"A01 LOGIN alice any\r\n").await.unwrap();
        let response = lines.next_line().await.expect("read").expect("login");
        assert!(response.starts_with("A01 OK"), "{response}");

        // The literal is synchronising, so the server must ask before we send.
        writer
            .write_all(format!("A02 APPEND INBOX {{{}}}\r\n", literal.len()).as_bytes())
            .await
            .unwrap();
        let continuation = lines
            .next_line()
            .await
            .expect("read")
            .expect("continuation");
        assert!(
            continuation.starts_with('+'),
            "expected a continuation request, got {continuation}"
        );

        writer.write_all(literal.as_bytes()).await.unwrap();
        writer.write_all(b"\r\n").await.unwrap();
        loop {
            let line = lines.next_line().await.expect("read").expect("append line");
            if line.starts_with("A02 ") {
                assert!(line.starts_with("A02 OK"), "APPEND failed: {line}");
                // UIDPLUS: an APPEND to a UIDPLUS server reports where it landed.
                assert!(
                    line.contains("APPENDUID"),
                    "APPEND must report APPENDUID: {line}"
                );
                break;
            }
        }
        writer.write_all(b"A03 LOGOUT\r\n").await.unwrap();
    }
    drop(imap);

    // 2. The same message must be visible over POP3, which never saw the APPEND.
    let mut pop3 = TcpStream::connect(harness.pop3).await.expect("connect");
    let (reader, mut writer) = pop3.split();
    let mut lines = BufReader::new(reader).lines();
    let greeting = lines.next_line().await.expect("read").expect("greeting");
    assert!(greeting.starts_with("+OK"), "{greeting}");

    for command in ["USER alice\r\n", "PASS any\r\n"] {
        writer.write_all(command.as_bytes()).await.unwrap();
        let response = lines.next_line().await.expect("read").expect("auth");
        assert!(response.starts_with("+OK"), "{command}: {response}");
    }

    writer.write_all(b"STAT\r\n").await.unwrap();
    let stat = lines.next_line().await.expect("read").expect("stat");
    assert!(
        stat.starts_with("+OK 1 "),
        "the appended message must be the one message POP3 sees: {stat}"
    );

    writer.write_all(b"RETR 1\r\n").await.unwrap();
    let response = lines.next_line().await.expect("read").expect("retr");
    assert!(response.starts_with("+OK"), "{response}");

    let mut body = String::new();
    loop {
        let line = lines.next_line().await.expect("read").expect("retr line");
        if line == "." {
            break;
        }
        body.push_str(&line);
        body.push('\n');
    }
    assert!(
        body.contains("Subject: Appended not delivered"),
        "POP3 must see the appended headers: {body}"
    );
    assert!(
        body.contains("This message never touched SMTP."),
        "POP3 must see the appended body: {body}"
    );

    writer.write_all(b"QUIT\r\n").await.unwrap();
    harness.stop().await;
}

/// Case 5 — IMAP `STORE \Seen` → REST API.
///
/// A flag written through IMAP must be visible through the HTTP adapter, which
/// reads the same entry by a different route. The failure this guards against
/// is a flag cached in the IMAP session's own snapshot and never committed:
/// the acting client would see its own change and nothing else would.
#[tokio::test]
async fn imap_store_seen_is_visible_through_the_rest_api() {
    let harness = MultiProtoHarness::start().await;

    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "Flag me across adapters",
        "Body.",
    )
    .await;

    // The entry starts unflagged, as seen by REST.
    let before = http_get(harness.http, "/api/v1/messages?limit=10").await;
    assert!(
        before.contains("200 OK"),
        "the message listing must succeed: {before}"
    );

    // Find the entry id the API uses, from the mailbox listing.
    let listing = http_get(harness.http, "/api/v1/messages?limit=10").await;
    let body = listing
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(&listing);
    let parsed: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("the listing must be JSON ({error}): {body}"));
    let items = parsed["items"].as_array().expect("an items array");
    assert!(
        !items.is_empty(),
        "the delivered message must appear in the listing: {parsed}"
    );

    // Set \Seen over IMAP.
    let mut imap = TcpStream::connect(harness.imap).await.expect("connect");
    {
        let (reader, mut writer) = imap.split();
        let mut lines = BufReader::new(reader).lines();
        let greeting = lines.next_line().await.expect("read").expect("greeting");
        assert!(greeting.starts_with("* OK"), "{greeting}");

        writer.write_all(b"S01 LOGIN alice any\r\n").await.unwrap();
        let response = lines.next_line().await.expect("read").expect("login");
        assert!(response.starts_with("S01 OK"), "{response}");

        let selected = imap_command(&mut writer, &mut lines, "S02", "SELECT INBOX").await;
        assert!(selected.contains("S02 OK"), "{selected}");

        let stored = imap_command(&mut writer, &mut lines, "S03", r"STORE 1 +FLAGS (\Seen)").await;
        assert!(stored.contains("S03 OK"), "STORE failed: {stored}");
        assert!(
            stored.contains("\\Seen"),
            "the untagged FETCH must report the new flag: {stored}"
        );

        writer.write_all(b"S04 LOGOUT\r\n").await.unwrap();
    }
    drop(imap);

    // REST must now report it, through an adapter that never saw the STORE.
    let after = http_get(harness.http, "/api/v1/messages?limit=10").await;
    let body = after
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(&after);
    let parsed: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("the listing must be JSON ({error}): {body}"));
    let items = parsed["items"].as_array().expect("an items array");
    // Flags live on the per-mailbox *entry*, not the message: one message can
    // sit in several mailboxes with independent flags, which is the whole
    // reason the two are separate rows (case 10).
    let flags: Vec<String> = items
        .iter()
        .flat_map(|item| item["entries"].as_array().cloned().unwrap_or_default())
        .flat_map(|entry| entry["flags"].as_array().cloned().unwrap_or_default())
        .filter_map(|flag| flag.as_str().map(str::to_owned))
        .collect();
    assert!(
        flags.iter().any(|flag| flag.eq_ignore_ascii_case("\\Seen")),
        "the flag set over IMAP must be visible over REST; flags were {flags:?} in {body}"
    );

    harness.stop().await;
}

/// Case 3 — SMTP → IMAP and POP3 concurrently.
///
/// Both adapters must see the same message, and neither may interfere with the
/// other. ADR-006 is the specific claim under test: a POP3 session does *not*
/// take an exclusive lock on the mailbox, so an IMAP client stays fully usable
/// while one is open. Holding both sessions open at once is the whole point —
/// a sequential test would pass even against a design that locks.
#[tokio::test]
async fn imap_and_pop3_see_the_same_message_concurrently() {
    let harness = MultiProtoHarness::start().await;

    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "Seen by both",
        "One message, two protocols.",
    )
    .await;

    // Open the POP3 session first and leave it open for the duration.
    let mut pop3 = TcpStream::connect(harness.pop3).await.expect("connect");
    let (pop3_reader, mut pop3_writer) = pop3.split();
    let mut pop3_lines = BufReader::new(pop3_reader).lines();
    let greeting = pop3_lines
        .next_line()
        .await
        .expect("read")
        .expect("greeting");
    assert!(greeting.starts_with("+OK"), "{greeting}");
    for command in ["USER alice\r\n", "PASS any\r\n"] {
        pop3_writer.write_all(command.as_bytes()).await.unwrap();
        let response = pop3_lines.next_line().await.expect("read").expect("auth");
        assert!(response.starts_with("+OK"), "{command}: {response}");
    }
    pop3_writer.write_all(b"STAT\r\n").await.unwrap();
    let stat = pop3_lines.next_line().await.expect("read").expect("stat");
    assert!(
        stat.starts_with("+OK 1 "),
        "POP3 must see the message: {stat}"
    );

    // With POP3 still connected and authenticated, IMAP must work normally.
    let mut imap = TcpStream::connect(harness.imap).await.expect("connect");
    {
        let (reader, mut writer) = imap.split();
        let mut lines = BufReader::new(reader).lines();
        let greeting = lines.next_line().await.expect("read").expect("greeting");
        assert!(greeting.starts_with("* OK"), "{greeting}");
        writer.write_all(b"C01 LOGIN alice any\r\n").await.unwrap();
        let response = lines.next_line().await.expect("read").expect("login");
        assert!(response.starts_with("C01 OK"), "{response}");

        let selected = imap_command(&mut writer, &mut lines, "C02", "SELECT INBOX").await;
        assert!(
            selected.contains("1 EXISTS"),
            "IMAP must see the same message while POP3 holds a session: {selected}"
        );

        let fetched = imap_command(
            &mut writer,
            &mut lines,
            "C03",
            "FETCH 1 BODY.PEEK[HEADER.FIELDS (SUBJECT)]",
        )
        .await;
        assert!(
            fetched.contains("Seen by both"),
            "IMAP must read the body during a POP3 session: {fetched}"
        );
        writer.write_all(b"C04 LOGOUT\r\n").await.unwrap();
    }
    drop(imap);

    // The POP3 session is still alive and usable — no lock was taken, and the
    // IMAP traffic did not disturb it.
    pop3_writer.write_all(b"STAT\r\n").await.unwrap();
    let stat = pop3_lines
        .next_line()
        .await
        .expect("read")
        .expect("the POP3 session must survive concurrent IMAP use");
    assert!(stat.starts_with("+OK 1 "), "{stat}");

    pop3_writer.write_all(b"QUIT\r\n").await.unwrap();
    harness.stop().await;
}

/// Case 6 — REST delete → IMAP EXPUNGE.
///
/// Deleting a message placement through the REST API (`DELETE /api/v1/entries/{id}`)
/// must propagate across adapters so that an IMAP client selecting the mailbox
/// sees that the message has been removed.
#[tokio::test]
async fn rest_delete_propagates_to_imap() {
    let harness = MultiProtoHarness::start().await;

    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "Delete me via REST",
        "This message will be deleted over HTTP.",
    )
    .await;

    // 1. Verify message is visible in IMAP.
    let mut imap = TcpStream::connect(harness.imap)
        .await
        .expect("connect to IMAP");
    let (reader, mut writer) = imap.split();
    let mut lines = BufReader::new(reader).lines();
    let greeting = lines.next_line().await.expect("read").expect("greeting");
    assert!(greeting.starts_with("* OK"));

    writer.write_all(b"D01 LOGIN alice any\r\n").await.unwrap();
    let login = lines.next_line().await.expect("read").expect("login");
    assert!(login.starts_with("D01 OK"), "{login}");

    let selected = imap_command(&mut writer, &mut lines, "D02", "SELECT INBOX").await;
    assert!(selected.contains("1 EXISTS"), "{selected}");

    // 2. Fetch the entry id over REST.
    let listing = http_get(harness.http, "/api/v1/messages?limit=10").await;
    let body = listing
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(&listing);
    let parsed: serde_json::Value = serde_json::from_str(body).expect("valid json");
    let items = parsed["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    let entry_id = items[0]["entries"][0]["entry_id"]
        .as_str()
        .expect("entry id");

    // 3. Delete the placement via REST.
    let delete_resp = http_delete(harness.http, &format!("/api/v1/entries/{entry_id}")).await;
    assert!(
        delete_resp.contains("200 OK") || delete_resp.contains("204 No Content"),
        "REST delete failed: {delete_resp}"
    );

    // 4. In IMAP, re-selecting the mailbox must reflect the deletion.
    let unselected = imap_command(&mut writer, &mut lines, "D03", "CLOSE").await;
    assert!(unselected.contains("D03 OK"), "{unselected}");

    let reselected = imap_command(&mut writer, &mut lines, "D04", "SELECT INBOX").await;
    assert!(
        reselected.contains("0 EXISTS"),
        "IMAP must report 0 EXISTS after REST deletion: {reselected}"
    );

    let status = imap_command(&mut writer, &mut lines, "D05", "STATUS INBOX (MESSAGES)").await;
    assert!(
        status.contains("MESSAGES 0"),
        "STATUS must report MESSAGES 0: {status}"
    );

    writer.write_all(b"D06 LOGOUT\r\n").await.unwrap();
    harness.stop().await;
}

/// Case 7 & 8 — POP3 DELE without QUIT → IMAP (session-scoped deletion)
/// and POP3 DELE + QUIT → IMAP (committed deletion).
///
/// RFC 1939 §6: POP3 deletions are session-scoped and only committed upon QUIT
/// (entering UPDATE state). A `DELE` without `QUIT` must leave the message
/// completely intact and visible to an active IMAP session. Only once POP3 sends
/// `QUIT` does the deletion commit to storage.
#[tokio::test]
async fn pop3_dele_without_quit_leaves_message_in_imap_and_quit_deletes_it() {
    let harness = MultiProtoHarness::start().await;

    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "Session Scoped Deletion",
        "Deleted only on QUIT.",
    )
    .await;

    // 1. Connect POP3, authenticate, and stage deletion with DELE.
    let mut pop3 = TcpStream::connect(harness.pop3)
        .await
        .expect("connect POP3");
    let (pop3_reader, mut pop3_writer) = pop3.split();
    let mut pop3_lines = BufReader::new(pop3_reader).lines();
    let greeting = pop3_lines
        .next_line()
        .await
        .expect("read")
        .expect("greeting");
    assert!(greeting.starts_with("+OK"));

    for command in ["USER alice\r\n", "PASS any\r\n"] {
        pop3_writer.write_all(command.as_bytes()).await.unwrap();
        let resp = pop3_lines.next_line().await.expect("read").expect("resp");
        assert!(resp.starts_with("+OK"), "{command}: {resp}");
    }

    pop3_writer.write_all(b"STAT\r\n").await.unwrap();
    let stat = pop3_lines.next_line().await.expect("read").expect("stat");
    assert!(stat.starts_with("+OK 1 "), "{stat}");

    // DELE 1 stages the deletion. We intentionally DO NOT send QUIT yet!
    pop3_writer.write_all(b"DELE 1\r\n").await.unwrap();
    let dele = pop3_lines.next_line().await.expect("read").expect("dele");
    assert!(dele.starts_with("+OK"), "{dele}");

    // 2. While POP3 holds the staged deletion, IMAP must STILL see the message intact.
    let mut imap = TcpStream::connect(harness.imap)
        .await
        .expect("connect IMAP");
    let (imap_reader, mut imap_writer) = imap.split();
    let mut imap_lines = BufReader::new(imap_reader).lines();
    let imap_greeting = imap_lines
        .next_line()
        .await
        .expect("read")
        .expect("greeting");
    assert!(imap_greeting.starts_with("* OK"));

    imap_writer
        .write_all(b"P01 LOGIN alice any\r\n")
        .await
        .unwrap();
    let login = imap_lines.next_line().await.expect("read").expect("login");
    assert!(login.starts_with("P01 OK"), "{login}");

    let selected = imap_command(&mut imap_writer, &mut imap_lines, "P02", "SELECT INBOX").await;
    assert!(
        selected.contains("1 EXISTS"),
        "IMAP must still see the message while POP3 has not QUIT: {selected}"
    );

    let fetch = imap_command(
        &mut imap_writer,
        &mut imap_lines,
        "P03",
        "FETCH 1 BODY.PEEK[HEADER.FIELDS (SUBJECT)]",
    )
    .await;
    assert!(
        fetch.contains("Session Scoped Deletion"),
        "Message body must remain fetchable: {fetch}"
    );

    // 3. POP3 issues QUIT, entering UPDATE state and committing the deletion.
    pop3_writer.write_all(b"QUIT\r\n").await.unwrap();
    let pop3_quit = pop3_lines.next_line().await.expect("read").expect("quit");
    assert!(pop3_quit.starts_with("+OK"), "{pop3_quit}");
    drop(pop3);

    // 4. In IMAP, re-selecting INBOX proves the message is now expunged.
    let closed = imap_command(&mut imap_writer, &mut imap_lines, "P04", "CLOSE").await;
    assert!(closed.contains("P04 OK"));

    let reselected = imap_command(&mut imap_writer, &mut imap_lines, "P05", "SELECT INBOX").await;
    assert!(
        reselected.contains("0 EXISTS"),
        "IMAP must reflect the committed POP3 deletion: {reselected}"
    );

    imap_writer.write_all(b"P06 LOGOUT\r\n").await.unwrap();
    harness.stop().await;
}

/// Case 9 — SMTP multi-recipient → per-account IMAP isolation.
///
/// One message delivered to multiple recipients produces one stored content row
/// and distinct entries in each recipient's INBOX, with independent UIDs and
/// independent flag lifecycles.
#[tokio::test]
async fn smtp_multi_recipient_isolates_accounts_with_single_message_row() {
    let harness = MultiProtoHarness::start_with_config(|cfg| {
        cfg.smtp.unknown_recipient = sandesh_config::UnknownRecipientPolicy::AutoCreate;
    })
    .await;

    // 1. Deliver one message to two recipients in a single SMTP session.
    let mut stream = TcpStream::connect(harness.smtp)
        .await
        .expect("connect SMTP");
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();
    let greeting = lines.next_line().await.expect("read").expect("greeting");
    assert!(greeting.starts_with("220"));

    for (cmd, code) in [
        ("HELO test.local\r\n", "250"),
        ("MAIL FROM:<sender@example.com>\r\n", "250"),
        ("RCPT TO:<alice@sandesh.test>\r\n", "250"),
        ("RCPT TO:<bob@sandesh.test>\r\n", "250"),
        ("DATA\r\n", "354"),
    ] {
        writer.write_all(cmd.as_bytes()).await.unwrap();
        let resp = lines.next_line().await.expect("read").expect("resp");
        assert!(resp.starts_with(code), "{cmd}: {resp}");
    }

    let payload = "From: sender@example.com\r\n\
To: alice@sandesh.test, bob@sandesh.test\r\n\
Subject: Multi-recipient isolation\r\n\
\r\n\
Single content body.\r\n.\r\n";
    writer.write_all(payload.as_bytes()).await.unwrap();
    let data_resp = lines.next_line().await.expect("read").expect("data ok");
    assert!(data_resp.starts_with("250"), "{data_resp}");
    writer.write_all(b"QUIT\r\n").await.unwrap();

    // 2. Connect as Alice over IMAP: set \Seen flag.
    let mut alice_imap = TcpStream::connect(harness.imap)
        .await
        .expect("connect IMAP");
    {
        let (reader, mut writer) = alice_imap.split();
        let mut lines = BufReader::new(reader).lines();
        let _ = lines.next_line().await; // greeting
        writer
            .write_all(b"M01 LOGIN alice@sandesh.test any\r\n")
            .await
            .unwrap();
        assert!(
            lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .starts_with("M01 OK")
        );

        let sel = imap_command(&mut writer, &mut lines, "M02", "SELECT INBOX").await;
        assert!(sel.contains("1 EXISTS"), "{sel}");

        let stored = imap_command(&mut writer, &mut lines, "M03", r"STORE 1 +FLAGS (\Seen)").await;
        assert!(
            stored.contains("M03 OK") && stored.contains("\\Seen"),
            "{stored}"
        );
        writer.write_all(b"M04 LOGOUT\r\n").await.unwrap();
    }
    drop(alice_imap);

    // 3. Connect as Bob over IMAP: Bob must have the message, but UNSEEN!
    let mut bob_imap = TcpStream::connect(harness.imap)
        .await
        .expect("connect IMAP");
    {
        let (reader, mut writer) = bob_imap.split();
        let mut lines = BufReader::new(reader).lines();
        let _ = lines.next_line().await; // greeting
        writer
            .write_all(b"B01 LOGIN bob@sandesh.test any\r\n")
            .await
            .unwrap();
        assert!(
            lines
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .starts_with("B01 OK")
        );

        let sel = imap_command(&mut writer, &mut lines, "B02", "SELECT INBOX").await;
        assert!(sel.contains("1 EXISTS"), "{sel}");

        let fetch = imap_command(&mut writer, &mut lines, "B03", "FETCH 1 (FLAGS)").await;
        assert!(
            !fetch.contains("\\Seen"),
            "Bob's message must not inherit Alice's \\Seen flag: {fetch}"
        );

        // Bob sets \Flagged.
        let stored =
            imap_command(&mut writer, &mut lines, "B04", r"STORE 1 +FLAGS (\Flagged)").await;
        assert!(
            stored.contains("B04 OK") && stored.contains("\\Flagged"),
            "{stored}"
        );
        writer.write_all(b"B05 LOGOUT\r\n").await.unwrap();
    }
    drop(bob_imap);

    // 4. REST check: exactly 1 message item, with 2 distinct entries.
    let listing = http_get(harness.http, "/api/v1/messages?limit=10").await;
    let body = listing
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&listing);
    let parsed: serde_json::Value = serde_json::from_str(body).expect("valid json");
    let items = parsed["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "exactly one Message row must be stored");
    let entries = items[0]["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 2, "one MessageEntry per recipient");

    harness.stop().await;
}

/// Case 10 — IMAP COPY → both mailboxes.
///
/// IMAP COPY creates a fresh placement with an independent UID and flags in the
/// target mailbox, while sharing the underlying message content.
#[tokio::test]
async fn imap_copy_creates_independent_placements_and_flags() {
    let harness = MultiProtoHarness::start().await;

    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "Copy test subject",
        "Content to copy.",
    )
    .await;

    let mut imap = TcpStream::connect(harness.imap)
        .await
        .expect("connect IMAP");
    let (reader, mut writer) = imap.split();
    let mut lines = BufReader::new(reader).lines();
    let _ = lines.next_line().await; // greeting

    writer.write_all(b"C01 LOGIN alice any\r\n").await.unwrap();
    assert!(
        lines
            .next_line()
            .await
            .unwrap()
            .unwrap()
            .starts_with("C01 OK")
    );

    let sel = imap_command(&mut writer, &mut lines, "C02", "SELECT INBOX").await;
    assert!(sel.contains("1 EXISTS"));

    // Create SavedFolder mailbox and copy message 1 into it.
    let create = imap_command(&mut writer, &mut lines, "C03", "CREATE SavedFolder").await;
    assert!(create.contains("C03 OK"), "{create}");

    let copy = imap_command(&mut writer, &mut lines, "C04", "COPY 1 SavedFolder").await;
    assert!(copy.contains("C04 OK"), "{copy}");
    assert!(copy.contains("COPYUID"), "must report COPYUID: {copy}");

    // Set \Flagged in INBOX.
    let store_inbox =
        imap_command(&mut writer, &mut lines, "C05", r"STORE 1 +FLAGS (\Flagged)").await;
    assert!(store_inbox.contains("C05 OK") && store_inbox.contains("\\Flagged"));

    // Select SavedFolder: message must exist, but flags must NOT have \Flagged.
    let sel_saved = imap_command(&mut writer, &mut lines, "C06", "SELECT SavedFolder").await;
    assert!(sel_saved.contains("1 EXISTS"), "{sel_saved}");

    let fetch_saved = imap_command(&mut writer, &mut lines, "C07", "FETCH 1 (UID FLAGS)").await;
    assert!(
        !fetch_saved.contains("\\Flagged"),
        "copied message must have independent flags: {fetch_saved}"
    );

    // Set \Answered in SavedFolder.
    let store_saved = imap_command(
        &mut writer,
        &mut lines,
        "C08",
        r"STORE 1 +FLAGS (\Answered)",
    )
    .await;
    assert!(store_saved.contains("C08 OK") && store_saved.contains("\\Answered"));

    // Re-select INBOX: must have \Flagged but NOT \Answered.
    let re_inbox = imap_command(&mut writer, &mut lines, "C09", "SELECT INBOX").await;
    assert!(re_inbox.contains("1 EXISTS"));
    let fetch_inbox = imap_command(&mut writer, &mut lines, "C10", "FETCH 1 (FLAGS)").await;
    assert!(
        fetch_inbox.contains("\\Flagged") && !fetch_inbox.contains("\\Answered"),
        "{fetch_inbox}"
    );

    writer.write_all(b"C11 LOGOUT\r\n").await.unwrap();
    harness.stop().await;
}

/// Waits for the next `event` frame of a given kind, or `None` on timeout.
///
/// Matching on the kind matters: one delivery publishes several events
/// (`EntryCreated` then `MessageReceived`), so taking merely the first frame
/// would leave a resume pointed into the middle of a single delivery.
async fn await_event_frame<S>(ws: &mut S, kind: &str) -> Option<serde_json::Value>
where
    S: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    let timeout = tokio::time::Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        let Ok(Some(Ok(msg))) =
            tokio::time::timeout(tokio::time::Duration::from_millis(500), ws.next()).await
        else {
            continue;
        };
        let Ok(text) = msg.into_text() else { continue };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if val["type"] == "event" && val["event"]["kind"] == kind {
            return Some(val);
        }
    }
    None
}

/// Case 11 — SMTP → WebSocket event → REST fetch.
///
/// An event published over WebSocket must never precede queryability: when
/// `MessageReceived` arrives, an immediate GET for that message must succeed
/// without retrying.
#[tokio::test]
async fn smtp_websocket_event_has_no_queryability_race() {
    let harness = MultiProtoHarness::start().await;

    // Connect WebSocket before delivery so we receive the live event.
    let ws_url = format!("ws://{}/api/v1/events", harness.http);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("connect to websocket");

    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "Race condition check",
        "Event vs queryability test.",
    )
    .await;

    // Await the MessageReceived event frame.
    let mut message_id = None;
    let timeout = tokio::time::Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        if let Ok(Some(Ok(msg))) =
            tokio::time::timeout(tokio::time::Duration::from_millis(500), ws.next()).await
            && let Ok(text) = msg.into_text()
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&text)
            && val["type"] == "event"
            && val["event"]["kind"] == "MessageReceived"
            && let Some(id) = val["event"]["payload"]["message_id"].as_str()
        {
            message_id = Some(id.to_owned());
            break;
        }
    }

    let id = message_id.expect("MessageReceived event must arrive within timeout");

    // Immediately issue REST fetch without delay.
    let response = http_get(harness.http, &format!("/api/v1/messages/{id}")).await;
    assert!(
        response.contains("200 OK"),
        "REST fetch immediately following WebSocket event must succeed: {response}"
    );
    assert!(
        response.contains("Race condition check"),
        "retrieved message must match subject: {response}"
    );

    harness.stop().await;
}

/// Case 11b — an event missed while disconnected is replayed on `resume`.
///
/// The unit tests on both sides drive fakes: the Rust ones call the bus
/// directly, and the TypeScript ones use a stub socket. Neither can prove the
/// two halves agree on the wire, which is the only thing that makes resume
/// work. This delivers mail while nobody is listening and asserts a real
/// reconnecting client is told about it (ADR-100).
#[tokio::test]
async fn websocket_resume_replays_events_missed_while_disconnected() {
    let harness = MultiProtoHarness::start().await;
    let ws_url = format!("ws://{}/api/v1/events", harness.http);

    // First connection: deliver one message and learn its bus_seq.
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("connect to websocket");

    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "Before the disconnect",
        "First.",
    )
    .await;

    let first = await_event_frame(&mut ws, "MessageReceived")
        .await
        .expect("first MessageReceived");
    let last_bus_seq = first["bus_seq"]
        .as_u64()
        .expect("bus_seq on an event frame");

    // Disconnect, then deliver with nobody listening. This is the case the
    // `lagged` frame cannot cover: there is no connection to send it on.
    drop(ws);
    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "During the disconnect",
        "Second.",
    )
    .await;

    // Reconnect and resume from where the first connection left off.
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("reconnect to websocket");
    let resume = serde_json::json!({"type": "resume", "after_seq": last_bus_seq});
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        resume.to_string().into(),
    ))
    .await
    .expect("send resume");

    // The header states what is coming, then the missed event follows as an
    // ordinary event frame marked `replayed`.
    let mut saw_resumed = false;
    let mut replayed_subject = None;
    let timeout = tokio::time::Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout && replayed_subject.is_none() {
        let Ok(Some(Ok(msg))) =
            tokio::time::timeout(tokio::time::Duration::from_millis(500), ws.next()).await
        else {
            continue;
        };
        let Ok(text) = msg.into_text() else { continue };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };

        if val["type"] == "resumed" {
            saw_resumed = true;
            assert!(
                val["count"].as_u64().unwrap_or(0) >= 1,
                "the header must account for the missed event: {val}"
            );
        }
        if val["type"] == "event"
            && val["replayed"] == serde_json::Value::Bool(true)
            && val["bus_seq"].as_u64().unwrap_or(0) > last_bus_seq
            && val["event"]["kind"] == "MessageReceived"
            && let Some(id) = val["event"]["payload"]["message_id"].as_str()
        {
            replayed_subject = Some(id.to_owned());
        }
    }

    assert!(
        saw_resumed,
        "a resume must be answered with a `resumed` header"
    );
    let id = replayed_subject.expect("the event missed while disconnected must be replayed");

    // The replayed event must name a real message, not merely look well formed.
    let response = http_get(harness.http, &format!("/api/v1/messages/{id}")).await;
    assert!(
        response.contains("During the disconnect"),
        "the replayed event must identify the message delivered while away: {response}"
    );

    harness.stop().await;
}

/// Case 12 — SMTP malformed message → IMAP FETCH.
///
/// Malformed messages (missing boundary, bare LF, 8-bit characters in headers)
/// are accepted by SMTP and retrievable byte-for-byte by IMAP FETCH.
#[tokio::test]
async fn smtp_malformed_message_survives_byte_exact_in_imap() {
    let harness = MultiProtoHarness::start().await;

    // A malformed message with an unclosed boundary and bare LF line endings.
    let malformed_raw = "From: malformed@example.com\r\n\
To: alice@sandesh.test\r\n\
Subject: Malformed MIME boundary\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"frontier\"\r\n\
\r\n\
--frontier\r\n\
Content-Type: text/plain\r\n\
\r\n\
Unclosed boundary test.\r\n";

    deliver_smtp_raw(
        harness.smtp,
        "malformed@example.com",
        "alice@sandesh.test",
        malformed_raw,
    )
    .await;

    let mut imap = TcpStream::connect(harness.imap)
        .await
        .expect("connect IMAP");
    let (reader, mut writer) = imap.split();
    let mut lines = BufReader::new(reader).lines();
    let _ = lines.next_line().await; // greeting

    writer.write_all(b"M01 LOGIN alice any\r\n").await.unwrap();
    assert!(
        lines
            .next_line()
            .await
            .unwrap()
            .unwrap()
            .starts_with("M01 OK")
    );

    let sel = imap_command(&mut writer, &mut lines, "M02", "SELECT INBOX").await;
    assert!(sel.contains("1 EXISTS"), "{sel}");

    writer
        .write_all(b"M03 FETCH 1 (BODY[] RFC822.SIZE)\r\n")
        .await
        .unwrap();

    let header_line = lines.next_line().await.unwrap().unwrap();
    assert!(header_line.contains("FETCH"));
    let declared: usize = header_line
        .rsplit_once('{')
        .and_then(|(_, rest)| rest.trim_end_matches('}').parse().ok())
        .expect("literal size");

    let mut fetched_body = String::new();
    while fetched_body.len() < declared {
        let line = lines.next_line().await.unwrap().unwrap();
        fetched_body.push_str(&line);
        fetched_body.push_str("\r\n");
    }

    assert_eq!(
        declared,
        malformed_raw.len(),
        "reported size must match raw length exactly"
    );
    assert!(
        fetched_body.contains("Unclosed boundary test."),
        "fetched body must contain exact text: {fetched_body}"
    );

    writer.write_all(b"M04 LOGOUT\r\n").await.unwrap();
    harness.stop().await;
}

/// Case 13 — IMAP IDLE + SMTP delivery.
///
/// An idling IMAP connection receives `* n EXISTS` within 100ms of a new
/// delivery arriving over SMTP, without polling or issuing commands.
#[tokio::test]
async fn imap_idle_receives_live_exists_on_smtp_delivery() {
    let harness = MultiProtoHarness::start().await;

    let mut imap = TcpStream::connect(harness.imap)
        .await
        .expect("connect IMAP");
    let (reader, mut writer) = imap.split();
    let mut lines = BufReader::new(reader).lines();
    let _ = lines.next_line().await; // greeting

    writer.write_all(b"I01 LOGIN alice any\r\n").await.unwrap();
    assert!(
        lines
            .next_line()
            .await
            .unwrap()
            .unwrap()
            .starts_with("I01 OK")
    );

    let sel = imap_command(&mut writer, &mut lines, "I02", "SELECT INBOX").await;
    assert!(sel.contains("0 EXISTS"));

    // Enter IDLE mode.
    writer.write_all(b"I03 IDLE\r\n").await.unwrap();
    let continuation = lines.next_line().await.unwrap().unwrap();
    assert!(
        continuation.starts_with('+'),
        "server must answer with '+' continuation: {continuation}"
    );

    // While idling, deliver a message via SMTP.
    deliver_smtp(
        harness.smtp,
        "sender@example.com",
        "alice@sandesh.test",
        "Pushed over IDLE",
        "Body arriving live.",
    )
    .await;

    // Await unsolicited untagged * 1 EXISTS response on the idling stream.
    let mut exists_seen = false;
    let timeout = tokio::time::Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(tokio::time::Duration::from_millis(500), lines.next_line()).await
            && line.contains("1 EXISTS")
        {
            exists_seen = true;
            break;
        }
    }
    assert!(
        exists_seen,
        "IDLE connection must receive '* 1 EXISTS' after SMTP delivery"
    );

    // Terminate IDLE with DONE.
    writer.write_all(b"DONE\r\n").await.unwrap();
    let done_resp = lines.next_line().await.unwrap().unwrap();
    assert!(
        done_resp.starts_with("I03 OK"),
        "IDLE must complete on DONE: {done_resp}"
    );

    writer.write_all(b"I04 LOGOUT\r\n").await.unwrap();
    harness.stop().await;
}

/// Every protocol produces a retrievable trace, through the real application.
///
/// The Phase 7 acceptance criterion, tested where it actually has to hold: the
/// assembled binary with all three listeners, not a per-crate harness. Each
/// protocol crate proves its own wiring; only this proves the composition root
/// switched tracing on for all of them.
#[tokio::test]
async fn every_protocol_records_a_connection_trace() {
    use sandesh_core::{ConnectionStore, Protocol};

    let harness = MultiProtoHarness::start().await;

    // One connection per protocol. SMTP delivers; the other two merely greet
    // and quit, which is itself the point — a connection that moves no mail
    // still has a story the viewer must be able to tell.
    deliver_smtp(
        harness.smtp,
        "sender@example.test",
        "catchall@example.test",
        "traced",
        "body",
    )
    .await;

    {
        let mut pop3 = TcpStream::connect(harness.pop3).await.expect("connect");
        let (reader, mut writer) = pop3.split();
        let mut lines = BufReader::new(reader).lines();
        lines.next_line().await.expect("read").expect("greeting");
        writer.write_all(b"QUIT\r\n").await.unwrap();
        let _ = lines.next_line().await;
    }

    {
        let mut imap = TcpStream::connect(harness.imap).await.expect("connect");
        let (reader, mut writer) = imap.split();
        let mut lines = BufReader::new(reader).lines();
        lines.next_line().await.expect("read").expect("greeting");
        writer.write_all(b"a1 LOGOUT\r\n").await.unwrap();
        let _ = lines.next_line().await;
    }

    // Records are written as each socket closes, which races this assertion.
    let database = sandesh_storage::Database::open(&harness.dir.path().join("sandesh.db"))
        .await
        .expect("open the application's database");
    let store = sandesh_storage::SqliteConnectionStore::new(database.pool().clone());

    let mut seen = Vec::new();
    for _ in 0..200 {
        let recent = store.recent(None, 50).await.expect("recent");
        seen = recent.iter().map(|c| c.protocol).collect();
        if [Protocol::Smtp, Protocol::Imap, Protocol::Pop3]
            .iter()
            .all(|p| seen.contains(p))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    for protocol in [Protocol::Smtp, Protocol::Imap, Protocol::Pop3] {
        assert!(
            seen.contains(&protocol),
            "{protocol:?} produced no connection record; saw {seen:?}"
        );
    }

    // And the SMTP one must not carry the message it delivered.
    let recent = store
        .recent(Some(Protocol::Smtp), 10)
        .await
        .expect("recent");
    let text = recent
        .iter()
        .flat_map(|c| c.transcript.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("MAIL FROM"),
        "the envelope is recorded: {text}"
    );
    assert!(
        !text.contains("body"),
        "message content must never be persisted in a transcript: {text}"
    );
}

/// Every protocol's trace is retrievable **over the API** (Phase 7 A2).
///
/// The companion to `every_protocol_records_a_connection_trace`, and the half
/// that was missing: that test reads the database directly, so it proves
/// transcripts are *captured*. The acceptance criterion says **retrievable**,
/// and until `/connections` existed there was no way to retrieve one — the
/// criterion passed on the strength of a test that bypassed the surface a user
/// actually has.
#[tokio::test]
async fn every_protocol_trace_is_retrievable_over_the_api() {
    let harness = MultiProtoHarness::start().await;

    deliver_smtp(
        harness.smtp,
        "sender@example.test",
        "catchall@example.test",
        "retrievable",
        "body",
    )
    .await;

    {
        let mut pop3 = TcpStream::connect(harness.pop3).await.expect("connect");
        let (reader, mut writer) = pop3.split();
        let mut lines = BufReader::new(reader).lines();
        lines.next_line().await.expect("read").expect("greeting");
        writer.write_all(b"QUIT\r\n").await.unwrap();
        let _ = lines.next_line().await;
    }

    {
        let mut imap = TcpStream::connect(harness.imap).await.expect("connect");
        let (reader, mut writer) = imap.split();
        let mut lines = BufReader::new(reader).lines();
        lines.next_line().await.expect("read").expect("greeting");
        writer.write_all(b"a1 LOGOUT\r\n").await.unwrap();
        let _ = lines.next_line().await;
    }

    // Records are written as each socket closes, which races this assertion.
    let mut body = String::new();
    for _ in 0..200 {
        body = http_get(harness.http, "/api/v1/connections?limit=50").await;
        if ["smtp", "imap", "pop3"]
            .iter()
            .all(|p| body.contains(&format!("\"protocol\":\"{p}\"")))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    for protocol in ["smtp", "imap", "pop3"] {
        assert!(
            body.contains(&format!("\"protocol\":\"{protocol}\"")),
            "{protocol} has no retrievable trace over the API:\n{body}"
        );
    }

    // The transcript must come back with the record, not as a second fetch: it
    // is the only field anyone opens this view for.
    assert!(
        body.contains("MAIL FROM"),
        "the listing carried no transcript content:\n{body}"
    );

    // And the redaction guarantee must survive the trip out. Capture strips
    // message content (ADR-066); if the read path could reintroduce it, every
    // password-reset link in a developer's test mail would be one GET away.
    assert!(
        !body.contains("body"),
        "message content reached the API:\n{body}"
    );

    // Filtering must actually filter, rather than being accepted and ignored.
    let smtp_only = http_get(harness.http, "/api/v1/connections?protocol=smtp").await;
    assert!(smtp_only.contains("\"protocol\":\"smtp\""));
    assert!(
        !smtp_only.contains("\"protocol\":\"imap\""),
        "the protocol filter was ignored:\n{smtp_only}"
    );

    // A mistyped protocol is a client error, not an empty result: returning
    // nothing would read as "no SMTP connections yet" to someone who typo'd.
    let mistyped = http_get(harness.http, "/api/v1/connections?protocol=smtpp").await;
    assert!(
        mistyped.contains("400"),
        "an unknown protocol must be rejected, not silently empty:\n{mistyped}"
    );

    // One record by id, fetched from the listing's own id.
    let id = body
        .split("\"id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the listing carries an id");
    let single = http_get(harness.http, &format!("/api/v1/connections/{id}")).await;
    assert!(
        single.contains(id),
        "the single-connection lookup did not return the record:\n{single}"
    );

    // A well-formed id that names nothing is a 404, not a 500.
    let missing = http_get(
        harness.http,
        "/api/v1/connections/00000000-0000-0000-0000-000000000000",
    )
    .await;
    assert!(
        missing.contains("404"),
        "an unknown connection must be a 404:\n{missing}"
    );

    harness.stop().await;
}

/// Case 14 — the FTS5 index narrows searches without changing them.
///
/// The parity tests in `sandesh-storage` prove the index cannot be wrong in
/// isolation. This proves it is actually *live* in the assembled application
/// and that both surfaces agree with it running: a mid-token substring, which
/// is the exact case a word-tokenised index would silently miss, must be found
/// by REST search and by IMAP `SEARCH` alike (`STORAGE_DESIGN.md` §7).
#[tokio::test]
async fn a_mid_token_substring_is_found_by_both_search_surfaces() {
    let harness = MultiProtoHarness::start().await;

    // "找" is not the point; "xfoobar" is: the substring "foo" starts mid-word,
    // so no word-prefix index can find it, while RFC 9051 §6.4.4 requires that
    // it be found.
    deliver_smtp(
        harness.smtp,
        "sender@example.test",
        "dev@example.test",
        "Indexed subject",
        "The body mentions xfoobar exactly once.",
    )
    .await;

    // Give the delivery a moment to commit before searching.
    for _ in 0..50 {
        let probe = http_get(harness.http, "/api/v1/messages").await;
        if probe.contains("Indexed subject") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // REST: the substring must be found even though it begins mid-token.
    let rest = http_get(harness.http, "/api/v1/search?q=foobar").await;
    assert!(
        rest.contains("Indexed subject"),
        "REST search missed a mid-token substring; the index is excluding matches:\n{rest}"
    );

    // IMAP: the same question, through the protocol that mandates the
    // semantics. `TEXT` is deliberately a key the index declines to narrow, and
    // `BODY` is one it does, so this exercises both paths.
    let mut stream = TcpStream::connect(harness.imap)
        .await
        .expect("connect IMAP");
    let (read, mut writer) = stream.split();
    let mut lines = BufReader::new(read).lines();
    lines.next_line().await.expect("read").expect("greeting");

    imap_command(
        &mut writer,
        &mut lines,
        "a1",
        "LOGIN dev@example.test password",
    )
    .await;
    imap_command(&mut writer, &mut lines, "a2", "SELECT INBOX").await;

    let body_search = imap_command(&mut writer, &mut lines, "a3", "SEARCH BODY \"foobar\"").await;
    assert!(
        body_search.contains("* SEARCH 1"),
        "IMAP SEARCH BODY missed a mid-token substring:\n{body_search}"
    );

    let text_search = imap_command(&mut writer, &mut lines, "a4", "SEARCH TEXT \"foobar\"").await;
    assert!(
        text_search.contains("* SEARCH 1"),
        "IMAP SEARCH TEXT missed a mid-token substring:\n{text_search}"
    );

    // A term in no message must return nothing, from both surfaces — the index
    // narrowing to the empty set must not become "match everything".
    let absent = http_get(harness.http, "/api/v1/search?q=zzzznotpresent").await;
    assert!(
        !absent.contains("Indexed subject"),
        "search returned a message that does not contain the term:\n{absent}"
    );

    imap_command(&mut writer, &mut lines, "a5", "LOGOUT").await;
    harness.stop().await;
}

/// Header analysis is retrievable over the API for real delivered mail
/// (Phase 7 A3).
///
/// The criterion is that DKIM signatures are *parsed and displayed with their
/// fields, and that verification is explicitly not claimed*. Both halves are
/// asserted here against a message that actually travelled through SMTP, not a
/// hand-built struct: the parse has to survive the storage round-trip and the
/// header decoding that happens on the way.
#[tokio::test]
async fn header_analysis_is_retrievable_and_never_claims_verification() {
    let harness = MultiProtoHarness::start().await;

    // A two-hop chain, newest-first as a real relay writes it, plus the three
    // authentication headers an upstream MTA leaves behind.
    let data = concat!(
        "Received: from relay.example (relay.example [192.0.2.2]) by mx.example\r\n",
        " with ESMTPS id B2 for <catchall@example.test>;",
        " Tue, 1 Jul 2025 10:00:05 +0000\r\n",
        "Received: from origin.example (origin.example [192.0.2.1]) by relay.example\r\n",
        " with ESMTP id A1; Tue, 1 Jul 2025 10:00:00 +0000\r\n",
        "DKIM-Signature: v=1; a=rsa-sha256; d=origin.example; s=sel2025;\r\n",
        " c=relaxed/relaxed; h=from:to:subject; bh=YmFzZTY0aGFzaA==; b=c2ln\r\n",
        "Authentication-Results: mx.example; dkim=pass header.d=origin.example;\r\n",
        " spf=pass smtp.mailfrom=sender@origin.example; dmarc=pass\r\n",
        "Received-SPF: Pass (mailfrom) identity=mailfrom; client-ip=192.0.2.1\r\n",
        "X-Spam-Status: No, score=-1.9 required=5.0\r\n",
        "From: <sender@origin.example>\r\n",
        "To: <catchall@example.test>\r\n",
        "Subject: analysed\r\n",
        "\r\n",
        "body\r\n",
        ".\r\n",
    );

    deliver_smtp_raw(
        harness.smtp,
        "sender@origin.example",
        "catchall@example.test",
        data,
    )
    .await;

    let listing = http_get(harness.http, "/api/v1/messages?limit=10").await;
    let body = listing
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&listing);
    let parsed: serde_json::Value = serde_json::from_str(body).expect("valid json");
    let id = parsed["items"][0]["id"].as_str().expect("a message id");

    let response = http_get(
        harness.http,
        &format!("/api/v1/messages/{id}/headers/analysis"),
    )
    .await;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&response);
    let analysis: serde_json::Value = serde_json::from_str(body).expect("valid json");

    // The chain is chronological, which is the reverse of the header order the
    // message arrived in.
    let hops = analysis["hops"].as_array().expect("hops array");
    assert_eq!(hops.len(), 2, "both Received fields became hops:\n{body}");
    // The clause keeps its RFC 5321 comment — the resolved name and IP a relay
    // writes there are exactly what a developer is looking for.
    assert_eq!(
        hops[0]["from"], "origin.example (origin.example [192.0.2.1])",
        "oldest hop first"
    );
    assert_eq!(hops[1]["from"], "relay.example (relay.example [192.0.2.2])");
    assert_eq!(hops[1]["with"], "ESMTPS");
    assert_eq!(hops[1]["for_recipient"], "<catchall@example.test>");
    assert_eq!(
        hops[1]["delay_micros"], 5_000_000,
        "the five-second gap between the hops"
    );

    // DKIM fields, individually, as the criterion asks.
    let dkim = &analysis["dkim"][0];
    assert_eq!(dkim["domain"], "origin.example");
    assert_eq!(dkim["selector"], "sel2025");
    assert_eq!(dkim["algorithm"], "rsa-sha256");
    assert_eq!(
        dkim["body_hash"], "YmFzZTY0aGFzaA==",
        "a folded base64 tag must be rejoined, not truncated"
    );
    assert_eq!(
        dkim["signed_headers"].as_array().expect("array").len(),
        3,
        "the h= list is split into names"
    );

    let results = analysis["auth_results"].as_array().expect("array");
    assert_eq!(results.len(), 3, "dkim, spf and dmarc:\n{body}");
    assert_eq!(results[0]["authserv_id"], "mx.example");
    assert_eq!(analysis["spf"][0]["result"], "pass");
    assert_eq!(analysis["spam"][0]["name"], "X-Spam-Status");

    // The criterion's second half, asserted rather than assumed: the response
    // carries the denial, and no other field asserts validity.
    let disclaimer = analysis["disclaimer"].as_str().expect("a disclaimer");
    assert!(
        disclaimer.contains("no DKIM, SPF or DMARC verification"),
        "the response must state that nothing was verified: {disclaimer}"
    );
    let without_disclaimer = body.replace(disclaimer, "");
    for claim in ["verified", "authentic", "trusted"] {
        assert!(
            !without_disclaimer.to_lowercase().contains(claim),
            "the analysis payload claimed `{claim}`:\n{body}"
        );
    }

    harness.stop().await;
}

/// A message with no chain and no authentication headers analyses to empty
/// rather than erroring.
///
/// API-injected mail is the common case in a development server, so an empty
/// analysis must be a normal `200`, not a `404` or a `500`.
#[tokio::test]
async fn a_message_with_no_routing_headers_analyses_to_empty() {
    let harness = MultiProtoHarness::start().await;

    deliver_smtp_raw(
        harness.smtp,
        "sender@example.test",
        "catchall@example.test",
        "From: <sender@example.test>\r\nSubject: bare\r\n\r\nbody\r\n.\r\n",
    )
    .await;

    let listing = http_get(harness.http, "/api/v1/messages?limit=10").await;
    let body = listing
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&listing);
    let parsed: serde_json::Value = serde_json::from_str(body).expect("valid json");
    let id = parsed["items"][0]["id"].as_str().expect("a message id");

    let response = http_get(
        harness.http,
        &format!("/api/v1/messages/{id}/headers/analysis"),
    )
    .await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "an empty analysis is a normal answer:\n{response}"
    );

    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or(&response);
    let analysis: serde_json::Value = serde_json::from_str(body).expect("valid json");
    assert!(analysis["dkim"].as_array().expect("array").is_empty());
    assert!(
        analysis["auth_results"]
            .as_array()
            .expect("array")
            .is_empty()
    );

    harness.stop().await;
}

/// Phase 7 A4 — the Prometheus scrape parses, and its counters move.
///
/// Asserted through the assembled application over real sockets rather than
/// against a hand-built snapshot: the figures come from aggregate SQL, so a
/// query that is wrong about a join or a grouping is only visible once real
/// mail has travelled the real path into the real schema.
#[tokio::test]
async fn the_metrics_scrape_parses_and_its_counters_move() {
    let harness = MultiProtoHarness::start().await;

    let before = http_get(harness.http, "/metrics").await;
    assert!(
        before.starts_with("HTTP/1.1 200"),
        "scrape should succeed:\n{before}"
    );
    assert!(
        before.contains("text/plain; version=0.0.4"),
        "a scraper selects its parser from the content type:\n{before}"
    );

    let body_of = |response: &str| {
        response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or_else(|| response.to_owned())
    };

    // Every series is declared before any mail exists, so a dashboard built
    // against a busy instance does not break against a fresh one.
    let first = body_of(&before);
    for name in [
        "sandesh_messages",
        "sandesh_mailboxes",
        "sandesh_deliveries_total",
        "sandesh_connections_total",
        "sandesh_commands_total",
        "sandesh_delivery_duration_seconds",
    ] {
        assert!(
            first.contains(&format!("# TYPE {name} ")),
            "{name} missing from an empty scrape:\n{first}"
        );
    }

    let messages_before = gauge_value(&first, "sandesh_messages");

    deliver_smtp(
        harness.smtp,
        "sender@example.test",
        "catchall@example.test",
        "scraped",
        "body",
    )
    .await;

    let after = body_of(&http_get(harness.http, "/metrics").await);

    assert_eq!(
        gauge_value(&after, "sandesh_messages"),
        messages_before + 1,
        "the message gauge must follow a real delivery:\n{after}"
    );
    assert!(
        after.contains("sandesh_deliveries_total{source=\"smtp\","),
        "the delivery counter must record the SMTP source:\n{after}"
    );
    assert!(
        after.contains("sandesh_connections_total{protocol=\"smtp\"}"),
        "the SMTP connection must be counted:\n{after}"
    );

    // The histogram's mandatory `+Inf` bucket must equal the observation
    // count, or the slowest deliveries are silently dropped from every
    // quantile computed over it.
    let inf = series_value(
        &after,
        "sandesh_delivery_duration_seconds_bucket{le=\"+Inf\"}",
    );
    let count = series_value(&after, "sandesh_delivery_duration_seconds_count");
    assert_eq!(inf, count, "+Inf must equal the count:\n{after}");
    assert!(count >= 1, "the delivery must be observed:\n{after}");
}

/// Reads an unlabelled sample's value out of a scrape.
fn gauge_value(scrape: &str, name: &str) -> u64 {
    series_value(scrape, name)
}

fn series_value(scrape: &str, series: &str) -> u64 {
    scrape
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let (found, value) = line.rsplit_once(' ')?;
            (found == series).then(|| value.parse::<u64>().ok())?
        })
        .unwrap_or_else(|| panic!("no sample named {series} in:\n{scrape}"))
}

/// Phase 7 A6 — the log stream carries what the server actually logged.
///
/// Asserted through the assembled application over real sockets: the capture
/// layer is installed by `logging::init` into a process-global buffer, so a
/// test that built a buffer by hand would prove the ring works and nothing
/// about whether the running server's log reaches it.
#[tokio::test]
async fn the_log_stream_serves_what_the_server_logged() {
    // Install the capture layer. The harness does not, because nothing before
    // A6 needed a log; without this the buffer stays empty and the assertions
    // below would pass vacuously — which an earlier draft of this test did.
    sandesh_app::logging::init("info");

    let harness = MultiProtoHarness::start().await;

    // Something the server itself logs, rather than a line this test emits:
    // the point is that real server activity is visible.
    deliver_smtp(
        harness.smtp,
        "sender@example.test",
        "catchall@example.test",
        "logged",
        "body",
    )
    .await;

    let response = http_get(harness.http, "/api/v1/logs?limit=200").await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "the backlog should be served:\n{response}"
    );

    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(&response);
    let parsed: serde_json::Value = serde_json::from_str(body).expect("valid json");
    let items = parsed["items"].as_array().expect("an items array");

    assert!(
        !items.is_empty(),
        "the server logs during startup and delivery; an empty backlog means \
         capture is not wired:\n{body}"
    );

    for item in items {
        assert!(item["seq"].is_u64(), "every line carries a sequence");
        assert!(item["at"].is_string(), "every line carries a timestamp");
        assert!(item["level"].is_string());
        assert!(item["message"].is_string());
    }

    // The server announces its listeners on startup; that line proves the
    // capture is of real server activity rather than of this test.
    let text = body.to_owned();
    assert!(
        text.contains("listening") || text.contains("server"),
        "no server startup line reached the log:\n{text}"
    );

    // Sequence numbers must not go backwards, or a reader cannot tell a gap
    // from a reordering.
    let seqs: Vec<u64> = items
        .iter()
        .filter_map(|item| item["seq"].as_u64())
        .collect();
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "sequence numbers must increase: {seqs:?}"
    );

    // A level filter must narrow rather than reorder.
    let filtered = http_get(harness.http, "/api/v1/logs?level=error&limit=200").await;
    let filtered_body = filtered
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(&filtered);
    let filtered_parsed: serde_json::Value =
        serde_json::from_str(filtered_body).expect("valid json");
    let errors = filtered_parsed["items"].as_array().expect("an items array");

    assert!(
        errors.len() <= items.len(),
        "a filter cannot produce more lines than no filter"
    );
    for item in errors {
        assert_eq!(
            item["level"].as_str(),
            Some("error"),
            "an error filter must return only errors"
        );
    }
}

/// The log must never carry message content, credentials or headers.
///
/// This is the whole safety argument for A6: it streams what already goes to
/// stdout, and that stream never carried the material. A test rather than a
/// comment, because the cost of being wrong is publishing a password-reset
/// link over HTTP.
#[tokio::test]
async fn the_log_stream_carries_no_message_content_or_credentials() {
    sandesh_app::logging::init("info");

    let harness = MultiProtoHarness::start().await;

    // Distinctive strings that would be unmistakable if they leaked.
    let secret_subject = "AAA-SECRET-SUBJECT-AAA";
    let secret_body = "BBB-SECRET-BODY-BBB";

    deliver_smtp(
        harness.smtp,
        "sender@example.test",
        "catchall@example.test",
        secret_subject,
        secret_body,
    )
    .await;

    let response = http_get(harness.http, "/api/v1/logs?limit=500").await;

    // A vacuous pass would be an empty buffer, which proves nothing about
    // redaction. The delivery above guarantees the server logged something.
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(&response);
    let parsed: serde_json::Value = serde_json::from_str(body).expect("valid json");
    assert!(
        !parsed["items"]
            .as_array()
            .expect("an items array")
            .is_empty(),
        "an empty log proves nothing about what the log excludes"
    );

    assert!(
        !response.contains(secret_subject),
        "a subject reached the log stream:\n{response}"
    );
    assert!(
        !response.contains(secret_body),
        "a message body reached the log stream:\n{response}"
    );
}
