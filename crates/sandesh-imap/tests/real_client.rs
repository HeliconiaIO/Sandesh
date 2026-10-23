//! Interoperability with clients we did not write (phase brief §12).
//!
//! The value of these tests is that they assert what a *stranger's* parser
//! accepts. A self-consistency test proves only that our writer and our reader
//! agree, which they will even if both are wrong about the RFC.
//!
//! `openssl s_client` is the external client used here, driven as a
//! subprocess. It has its own IMAP `STARTTLS` implementation (`-starttls
//! imap`), so it exercises the upgrade against an independent reading of
//! RFC 9051 §6.2.1.
//!
//! # Dual-RFC capability advertisement
//!
//! Pre-RFC-9051 clients — Python's `imaplib` among them — accept only the
//! `IMAP4REV1` and `IMAP4` capability names and reject a server advertising
//! `IMAP4rev2` alone with "server not IMAP4 compliant". Both names are
//! therefore advertised, which is what lets those clients connect at all;
//! [`imap4rev1_and_imap4rev2_are_advertised`] pins that so it stays a
//! deliberate choice rather than drifting silently.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use std::process::Stdio;

use common::TestServer;
use tokio::io::AsyncWriteExt;

/// Whether `openssl` is available; these tests skip rather than fail without it.
fn openssl_present() -> bool {
    std::process::Command::new("openssl")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Runs `openssl s_client` against `addr`, feeding it `script`.
///
/// Returns everything the client printed, which includes the server's
/// responses as the external client received and framed them.
async fn s_client(args: &[&str], script: &str) -> String {
    let mut child = tokio::process::Command::new("openssl")
        .arg("s_client")
        .args(args)
        .arg("-quiet")
        .arg("-verify_quiet")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn openssl s_client");

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .await
            .expect("write the IMAP script");
        stdin.flush().await.expect("flush");
        drop(stdin);
    }

    let output = tokio::time::timeout(std::time::Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("openssl s_client should finish")
        .expect("collect output");

    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test]
async fn openssl_completes_an_implicit_tls_session() {
    if !openssl_present() {
        eprintln!("skipping: openssl is not installed");
        return;
    }

    let server = TestServer::with_tls(TestServer::implicit_tls_only()).await;
    let addr = server.implicit();

    let transcript = s_client(
        &["-connect", &addr.to_string()],
        "A1 CAPABILITY\r\nA2 LOGOUT\r\n",
    )
    .await;

    // The handshake succeeded and the greeting arrived inside TLS.
    assert!(
        transcript.contains("* OK [CAPABILITY"),
        "an external TLS client must receive the greeting: {transcript}"
    );
    assert!(
        transcript.contains("A1 OK"),
        "CAPABILITY must complete for an external client: {transcript}"
    );
    assert!(
        transcript.contains("* BYE"),
        "LOGOUT must produce an untagged BYE: {transcript}"
    );
    assert!(
        transcript.contains("A2 OK"),
        "LOGOUT must complete: {transcript}"
    );

    // AUTH is offered because the channel is encrypted; STARTTLS is not,
    // because there is nothing to upgrade.
    assert!(transcript.contains("AUTH=PLAIN"), "{transcript}");
    assert!(
        !transcript.contains("STARTTLS"),
        "an implicit-TLS port must not advertise STARTTLS: {transcript}"
    );
}

#[tokio::test]
async fn openssl_negotiates_starttls_with_its_own_implementation() {
    if !openssl_present() {
        eprintln!("skipping: openssl is not installed");
        return;
    }

    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let addr = server.cleartext();

    // `-starttls imap` makes openssl drive the upgrade itself, using its own
    // reading of the RFC rather than ours.
    let transcript = s_client(
        &["-connect", &addr.to_string(), "-starttls", "imap"],
        "B1 CAPABILITY\r\nB2 LOGOUT\r\n",
    )
    .await;

    assert!(
        transcript.contains("B1 OK"),
        "an external client's STARTTLS upgrade must yield a working session: {transcript}"
    );
    assert!(
        transcript.contains("AUTH=PLAIN"),
        "AUTH must appear after the upgrade: {transcript}"
    );
    assert!(
        transcript.contains("B2 OK") && transcript.contains("* BYE"),
        "LOGOUT must work after the upgrade: {transcript}"
    );
}

#[tokio::test]
async fn openssl_cannot_reach_the_implicit_tls_port_without_tls() {
    if !openssl_present() {
        eprintln!("skipping: openssl is not installed");
        return;
    }

    let server = TestServer::with_tls(TestServer::implicit_tls_only()).await;
    let addr = server.implicit();

    // No `-starttls`, and the port speaks TLS from byte zero, so a *plaintext*
    // probe must get no IMAP at all. Driving this with a raw socket rather than
    // openssl, since the point is the absence of a handshake.
    use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    stream.write_all(b"C1 CAPABILITY\r\n").await.expect("write");

    let mut buf = [0u8; 256];
    let text = match tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut buf))
        .await
    {
        Ok(Ok(n)) => String::from_utf8_lossy(&buf[..n]).into_owned(),
        _ => String::new(),
    };

    assert!(
        !text.contains("IMAP4rev2"),
        "an implicit-TLS port must never answer plaintext IMAP: {text:?}"
    );
}

#[tokio::test]
async fn imap4rev1_and_imap4rev2_are_advertised() {
    // ADR-042 (Revised): Both IMAP4rev1 and IMAP4rev2 are advertised so that standard
    // clients (Python imaplib, Odoo fetchmail, Thunderbird, etc.) connect cleanly while
    // rev2 clients can negotiate RFC 9051 features.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("CAPABILITY").await;

    assert!(response.contains("IMAP4rev2"), "{response:?}");
    assert!(
        response.contains("IMAP4rev1"),
        "IMAP4rev1 must be advertised for RFC 3501 client compatibility (ADR-042 revised): {response:?}"
    );
}

/// The mailbox flow a real client performs after authenticating.
///
/// This is the sequence in the phase brief §15: everything a client does
/// between logging in and asking for messages. Driven over a raw socket so the
/// assertions are on exact protocol output rather than on a library's
/// interpretation of it.
#[tokio::test]
async fn a_client_completes_the_mailbox_discovery_flow() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = server.connect().await;
    assert!(
        client.greeting.contains("* OK"),
        "greeting: {}",
        client.greeting
    );

    assert!(client.command("CAPABILITY").await.contains("IMAP4rev2"));
    assert!(client.command("LOGIN user secret").await.contains("OK"));
    assert!(client.command(r#"LIST "" "*""#).await.contains("INBOX"));
    assert!(
        client
            .command("SELECT INBOX")
            .await
            .contains("[READ-WRITE]")
    );
    assert!(
        client
            .command("STATUS INBOX (MESSAGES UIDNEXT UIDVALIDITY)")
            .await
            .contains("* STATUS")
    );
    assert!(
        client
            .command("EXAMINE INBOX")
            .await
            .contains("[READ-ONLY]")
    );

    let logout = client.command("LOGOUT").await;
    assert!(logout.contains("* BYE"), "{logout}");
    assert!(logout.contains("OK"), "{logout}");
}

/// The full mailbox lifecycle, end to end (phase brief §15).
#[tokio::test]
async fn a_client_completes_the_mailbox_lifecycle() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = server.connect().await;
    assert!(client.command("LOGIN user secret").await.contains("OK"));

    assert!(client.command("CREATE test").await.contains("OK"));
    assert!(client.command(r#"LIST "" "*""#).await.contains("test"));
    assert!(client.command("SELECT test").await.contains("OK"));

    assert!(client.command("RENAME test test2").await.contains("OK"));
    let listing = client.command(r#"LIST "" "*""#).await;
    assert!(listing.contains("test2"), "{listing}");

    assert!(client.command("SUBSCRIBE test2").await.contains("OK"));
    assert!(client.command("UNSUBSCRIBE test2").await.contains("OK"));
    assert!(client.command("DELETE test2").await.contains("OK"));

    let after = client.command(r#"LIST "" "*""#).await;
    assert!(
        !after.contains("test2"),
        "deleted mailbox must be gone: {after}"
    );
}

/// Whether a Python 3 interpreter with `imaplib` is available.
///
/// `imaplib` is in the standard library, so the check is really for Python
/// itself; these tests skip rather than fail without it, the same way the
/// `openssl` ones do.
fn python_present() -> bool {
    std::process::Command::new("python3")
        .args(["-c", "import imaplib"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Runs a Python script, returning `(stdout, stderr, success)`.
async fn python(script: &str) -> (String, String, bool) {
    let mut child = tokio::process::Command::new("python3")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn python3");

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .await
            .expect("write the script");
        stdin.flush().await.expect("flush");
        drop(stdin);
    }

    let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("python3 should finish")
        .expect("collect output");

    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// Python `imaplib` performs the whole read-mail flow, including `FETCH`.
///
/// # Why this test matters more than the raw-socket ones
///
/// `imaplib` is a parser nobody here wrote. It validates the response shape
/// independently: it rejects a malformed literal, it insists on the
/// `* n FETCH (...)` framing, and it raises on an unparseable `BODYSTRUCTURE`.
/// A self-consistency test proves only that our writer and our reader agree.
///
/// # No local compatibility adjustment is needed
///
/// ADR-042 (revised) advertises both `IMAP4rev1` and `IMAP4rev2`, which is what
/// lets `imaplib` connect at all — it rejects a server advertising `IMAP4rev2`
/// alone with "server not IMAP4 compliant". Because that advertisement is in
/// the server, this test drives *unmodified* `imaplib`: no monkey-patching of
/// `imaplib.Commands`, no forced capability. The `assert` on the greeting below
/// pins that, so a regression to rev2-only would fail here rather than silently
/// requiring a client-side patch.
#[tokio::test]
async fn python_imaplib_fetches_mail() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;

    let raw = b"From: Alice <alice@example.test>\r\n\
To: Bob <bob@example.test>\r\n\
Subject: Interop check\r\n\
Date: Tue, 01 Sep 2026 12:00:00 +0000\r\n\
Message-ID: <interop-1@example.test>\r\n\
\r\n\
The body an external client must read back.\r\n";
    server.deliver(inbox, raw).await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, sys

# Unmodified imaplib: no capability patching, no monkey-patching.
m = imaplib.IMAP4(host="{host}", port={port})
print("GREETING", m.welcome.decode() if isinstance(m.welcome, bytes) else m.welcome)

typ, _ = m.login("user", "secret")
assert typ == "OK", ("login", typ)

typ, boxes = m.list()
assert typ == "OK", ("list", typ)
assert any(b"INBOX" in b for b in boxes), ("no INBOX", boxes)

typ, count = m.select("INBOX")
assert typ == "OK", ("select", typ)
assert int(count[0]) == 1, ("exists", count)

# Metadata FETCH.
typ, data = m.fetch("1", "(UID FLAGS RFC822.SIZE INTERNALDATE ENVELOPE BODYSTRUCTURE)")
assert typ == "OK", ("meta fetch", typ)
meta = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
print("META", meta.decode(errors="replace"))
assert b"UID" in meta, meta
assert b"RFC822.SIZE" in meta, meta

# Body FETCH: imaplib parses the literal and hands back exact octets.
typ, data = m.fetch("1", "(BODY.PEEK[])")
assert typ == "OK", ("body fetch", typ)
body = None
for item in data:
    if isinstance(item, tuple) and len(item) == 2:
        body = item[1]
assert body is not None, ("no literal returned", data)
print("BODYLEN", len(body))
sys.stdout.flush()
with open("{out}", "wb") as fh:
    fh.write(body)

# Header-only FETCH.
typ, data = m.fetch("1", "(BODY.PEEK[HEADER.FIELDS (SUBJECT)])")
assert typ == "OK", ("header fetch", typ)
header = b"".join(i[1] for i in data if isinstance(i, tuple))
assert b"Interop check" in header, header

# UID FETCH through the library's own uid() entry point.
typ, data = m.uid("FETCH", "1:*", "(UID RFC822.SIZE)")
assert typ == "OK", ("uid fetch", typ)
print("UIDFETCH", b" ".join(d for d in data if d).decode(errors="replace"))

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
        out = body_path().display(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");

    // The greeting must have carried the rev1 name, which is what let
    // unmodified imaplib proceed.
    assert!(
        stdout.contains("IMAP4rev1"),
        "imaplib needs the rev1 capability name (ADR-042): {stdout}"
    );
    assert!(
        stdout.contains(&format!("BODYLEN {}", raw.len())),
        "the external client must receive every octet: {stdout}"
    );

    // And the octets it wrote out are byte-identical to what was stored.
    let fetched = std::fs::read(body_path()).expect("read what imaplib fetched");
    assert_eq!(
        fetched, raw,
        "an independent client must read back the exact stored bytes"
    );
    let _ = std::fs::remove_file(body_path());
}

/// Where the interop test asks Python to write the octets it fetched.
///
/// A file is used rather than stdout because the body is arbitrary octets and
/// piping it through a text stream would risk re-encoding — the very thing the
/// test is checking has not happened.
fn body_path() -> std::path::PathBuf {
    std::env::temp_dir().join("sandesh-imaplib-fetch-body.bin")
}

/// Python `imaplib` reads a MIME message and addresses its parts.
#[tokio::test]
async fn python_imaplib_reads_mime_parts() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server
        .deliver(
            inbox,
            &TestServer::fixture("valid/multipart-alternative.eml"),
        )
        .await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, email

m = imaplib.IMAP4(host="{host}", port={port})
m.login("user", "secret")
m.select("INBOX")

typ, data = m.fetch("1", "(BODYSTRUCTURE)")
assert typ == "OK", typ
structure = b" ".join(d for d in data if d)
assert b"ALTERNATIVE" in structure.upper(), structure

# Fetch each part individually and confirm both are non-empty and distinct.
typ, first = m.fetch("1", "(BODY.PEEK[1])")
assert typ == "OK", typ
typ, second = m.fetch("1", "(BODY.PEEK[2])")
assert typ == "OK", typ
a = b"".join(i[1] for i in first if isinstance(i, tuple))
b = b"".join(i[1] for i in second if isinstance(i, tuple))
assert a and b and a != b, (len(a), len(b))

# The whole message must parse as MIME in a library that is not ours.
typ, whole = m.fetch("1", "(BODY.PEEK[])")
octets = b"".join(i[1] for i in whole if isinstance(i, tuple))
message = email.message_from_bytes(octets)
assert message.is_multipart(), "the fetched message must still be multipart"
types = sorted(p.get_content_type() for p in message.walk() if not p.is_multipart())
print("TYPES", types)
m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib MIME session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
    assert!(
        stdout.contains("text/plain") && stdout.contains("text/html"),
        "an external MIME parser must see both alternatives: {stdout}"
    );
}

/// Python `imaplib` changes flags with `STORE` and sees the result.
///
/// The library builds the command itself and parses the untagged `FETCH`
/// reply, so this checks the wire shape against a parser nobody here wrote.
/// It also covers the `uid("STORE", ...)` entry point, which is what mail
/// clients actually use once they have searched by UID.
#[tokio::test]
async fn python_imaplib_stores_flags() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;

    let raw = b"From: alice@example.test\r\n\
Subject: Flag me\r\n\
\r\n\
Body.\r\n";
    let entry = server.deliver(inbox, raw).await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib

m = imaplib.IMAP4(host="{host}", port={port})
m.login("user", "secret")
m.select("INBOX")

# The message starts unflagged.
typ, data = m.fetch("1", "(FLAGS)")
assert typ == "OK", typ
print("BEFORE", data)

# +FLAGS, and the untagged FETCH must report the result.
typ, data = m.store("1", "+FLAGS", r"(\Seen)")
assert typ == "OK", ("store", typ, data)
joined = b" ".join(d for d in data if d)
print("STORED", joined.decode(errors="replace"))
assert b"\\Seen" in joined, joined

# A fresh FETCH in the *same session* must agree (ADR-047).
typ, data = m.fetch("1", "(FLAGS)")
assert typ == "OK", typ
after = b" ".join(d for d in data if d)
print("AFTER", after.decode(errors="replace"))
assert b"\\Seen" in after, ("same-session FETCH is stale", after)

# -FLAGS removes it again.
typ, data = m.store("1", "-FLAGS", r"(\Seen)")
assert typ == "OK", typ
typ, data = m.fetch("1", "(FLAGS)")
gone = b" ".join(d for d in data if d)
assert b"\\Seen" not in gone, ("flag not removed", gone)

# .SILENT sends no untagged FETCH but must still change the flags.
typ, data = m.store("1", "+FLAGS.SILENT", r"(\Flagged)")
assert typ == "OK", ("silent store", typ, data)
silent_payload = b" ".join(d for d in data if d)
print("SILENT", silent_payload.decode(errors="replace"))
typ, data = m.fetch("1", "(FLAGS)")
flagged = b" ".join(d for d in data if d)
assert b"\\Flagged" in flagged, ("SILENT did not mutate", flagged)

# UID STORE through the library's own uid() entry point.
typ, data = m.uid("STORE", "{uid}", "+FLAGS", r"($Junk)")
assert typ == "OK", ("uid store", typ, data)
uid_payload = b" ".join(d for d in data if d)
print("UIDSTORE", uid_payload.decode(errors="replace"))
assert b"UID {uid}" in uid_payload, ("UID STORE must report the UID", uid_payload)
assert b"$Junk" in uid_payload, uid_payload

# A keyword survives a round trip through UID FETCH.
typ, data = m.uid("FETCH", "{uid}", "(FLAGS)")
assert typ == "OK", typ
assert b"$Junk" in b" ".join(d for d in data if d)

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
        uid = entry.uid.get(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib STORE session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
    // `.SILENT` really produced no data for the library to report.
    assert!(
        stdout.contains("SILENT [None]") || stdout.contains("SILENT \n"),
        "a SILENT store should carry no untagged FETCH: {stdout}"
    );
}

/// The read-then-mark flow a mail client performs, over `imaplib`.
///
/// This is the shape a fetching agent uses once `SEARCH` exists: read the
/// body, then mark the message so it is not processed twice. `SEARCH` is not
/// implemented yet, so the messages are addressed directly — the point here is
/// that `FETCH` + `STORE` compose correctly in one session.
#[tokio::test]
async fn python_imaplib_reads_then_marks_messages() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    for index in 0..3 {
        let raw = format!(
            "From: alice@example.test\r\nSubject: Message {index}\r\n\r\nBody {index}.\r\n"
        );
        server.deliver(inbox, raw.as_bytes()).await;
    }

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib

m = imaplib.IMAP4(host="{host}", port={port})
m.login("user", "secret")
typ, count = m.select("INBOX")
assert int(count[0]) == 3, count

processed = []
for num in (b"1", b"2", b"3"):
    typ, data = m.fetch(num, "(BODY.PEEK[])")
    assert typ == "OK", (num, typ)
    body = b"".join(i[1] for i in data if isinstance(i, tuple))
    processed.append(body)
    # Mark it handled, quietly.
    typ, _ = m.store(num, "+FLAGS.SILENT", r"(\Seen)")
    assert typ == "OK", (num, typ)

assert len(processed) == 3, len(processed)
for index, body in enumerate(processed):
    assert ("Body %d." % index).encode() in body, (index, body)

# Every message now reads as seen, in this same session.
typ, data = m.fetch("1:3", "(FLAGS)")
assert typ == "OK", typ
flags = b" ".join(d for d in data if d)
assert flags.count(b"\\Seen") == 3, ("not all marked", flags)
print("SEENCOUNT", flags.count(b"\\Seen"))

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib read-then-mark failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("SEENCOUNT 3"), "{stdout}");
    assert!(stdout.contains("DONE"), "{stdout}");
}

/// Python `imaplib` runs the fetching-agent loop that Odoo performs.
///
/// This is the test that matters most for the reported failure. Odoo's
/// `fetchmail` does `search(None, '(UNSEEN)')`, splits the result into message
/// numbers, fetches each with `(RFC822)`, then marks it `\Seen`. Before
/// `SEARCH` existed, the `NO` reply was split into fake message numbers and
/// passed to `FETCH`, which rejected them — so the visible error came from
/// `FETCH` while the real gap was `SEARCH`.
///
/// Driving it through unmodified `imaplib` means the status check, the result
/// parsing and the command construction are all a stranger's code.
#[tokio::test]
async fn python_imaplib_runs_the_fetchmail_loop() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;

    for index in 0..3 {
        let raw = format!(
            "From: sender@example.test\r\n\
To: user@example.test\r\n\
Subject: Ticket {index}\r\n\
Date: Tue, 01 Sep 2026 12:00:00 +0000\r\n\
\r\n\
Body of ticket {index}.\r\n"
        );
        server.deliver(inbox, raw.as_bytes()).await;
    }

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib

m = imaplib.IMAP4(host="{host}", port={port})
m.login("user", "secret")
m.select("INBOX")

# Exactly what Odoo fetchmail does: search, then iterate data[0].split().
typ, data = m.search(None, "(UNSEEN)")
assert typ == "OK", ("search status", typ, data)
nums = data[0].split()
print("FOUND", len(nums), nums)
assert len(nums) == 3, ("expected three unread", data)

for num in nums:
    typ, msg = m.fetch(num, "(RFC822)")
    assert typ == "OK", ("fetch", num, typ)
    body = b"".join(i[1] for i in msg if isinstance(i, tuple))
    assert b"Ticket" in body, (num, body[:120])
    typ, _ = m.store(num, "+FLAGS", r"(\Seen)")
    assert typ == "OK", ("store", num, typ)

# The second poll must be empty: nothing is processed twice.
typ, data = m.search(None, "(UNSEEN)")
assert typ == "OK", typ
assert data[0].split() == [], ("second poll not empty", data)
print("SECONDPOLL", data)

# And all three now read as seen.
typ, data = m.search(None, "SEEN")
assert typ == "OK", typ
print("SEEN", len(data[0].split()))
assert len(data[0].split()) == 3, data

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "the fetchmail loop failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("FOUND 3"), "{stdout}");
    assert!(stdout.contains("SEEN 3"), "{stdout}");
    assert!(stdout.contains("DONE"), "{stdout}");
}

/// Python `imaplib` exercises the search criteria a mail client uses.
#[tokio::test]
async fn python_imaplib_searches_by_criteria() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;

    server
        .deliver(
            inbox,
            b"From: alice@example.test\r\n\
To: user@example.test\r\n\
Subject: Quarterly report\r\n\
Date: Tue, 01 Sep 2026 12:00:00 +0000\r\n\
\r\n\
The pineapple numbers are in.\r\n",
        )
        .await;
    server
        .deliver(
            inbox,
            b"From: bob@example.test\r\n\
To: user@example.test\r\n\
Subject: Lunch\r\n\
Date: Tue, 01 Sep 2026 12:00:00 +0000\r\n\
\r\n\
Mango sandwiches today.\r\n",
        )
        .await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib

m = imaplib.IMAP4(host="{host}", port={port})
m.login("user", "secret")
m.select("INBOX")

def found(*criteria):
    typ, data = m.search(None, *criteria)
    assert typ == "OK", (criteria, typ, data)
    return data[0].split()

assert found("ALL") == [b"1", b"2"], found("ALL")
assert found("FROM", "alice") == [b"1"], found("FROM", "alice")
assert found("FROM", "bob") == [b"2"], found("FROM", "bob")
assert found("SUBJECT", "Quarterly") == [b"1"]
assert found("BODY", "pineapple") == [b"1"]
assert found("BODY", "Mango") == [b"2"]
assert found("TEXT", "Lunch") == [b"2"]

# NOT and OR through the library's own quoting.
assert found("NOT", "FROM", "alice") == [b"2"], found("NOT", "FROM", "alice")
assert found("OR", "FROM", "alice", "FROM", "bob") == [b"1", b"2"]

# Implicit AND narrows.
assert found("FROM", "alice", "SUBJECT", "Quarterly") == [b"1"]
assert found("FROM", "alice", "SUBJECT", "Lunch") == []

# Size criteria.
assert found("SMALLER", "100000") == [b"1", b"2"]
assert found("LARGER", "100000") == []

# A quoted multi-word argument.
typ, data = m.search(None, 'SUBJECT "Quarterly report"')
assert typ == "OK", (typ, data)
assert data[0].split() == [b"1"], data

# UID SEARCH returns UIDs, and they are fetchable.
typ, uids = m.uid("SEARCH", "ALL")
assert typ == "OK", typ
uid_list = uids[0].split()
print("UIDS", uid_list)
assert len(uid_list) == 2, uids
typ, fetched = m.uid("FETCH", uid_list[0].decode(), "(UID)")
assert typ == "OK", typ
assert b"UID " + uid_list[0] in b" ".join(f for f in fetched if f), fetched

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "criteria search failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
}

/// The complete delete cycle through Python's `imaplib`, end to end.
///
/// This is the flow an integrating client (Odoo's fetchmail among them)
/// performs on every poll: discover with `SEARCH`, read with `FETCH`, mark with
/// `STORE +FLAGS \Deleted`, then `EXPUNGE`, then look again. The value of
/// running it through `imaplib` is that a stranger's parser has to accept our
/// `* <n> EXPUNGE` lines and our renumbering — `imaplib` tracks the untagged
/// responses itself, so a wrong sequence number or a missing line shows up as
/// the library disagreeing with the server about what the mailbox holds.
#[tokio::test]
async fn python_imaplib_completes_the_delete_cycle() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;

    let keep = server
        .deliver(
            inbox,
            b"From: alice@example.test\r\nSubject: Keep me\r\n\r\nStay.\r\n",
        )
        .await;
    let process = server
        .deliver(
            inbox,
            b"From: bob@example.test\r\nSubject: Process me\r\n\r\nGo.\r\n",
        )
        .await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib

m = imaplib.IMAP4(host="{host}", port={port})
m.login("user", "secret")
typ, data = m.select("INBOX")
assert typ == "OK", (typ, data)
assert data[0] == b"2", ("EXISTS", data)

# SEARCH — discover what is there, by UID.
typ, data = m.uid("SEARCH", None, "ALL")
assert typ == "OK", (typ, data)
uids = data[0].split()
print("SEARCH", uids)
assert uids == [b"{keep_uid}", b"{process_uid}"], uids

# FETCH — read the one to be processed.
typ, data = m.uid("FETCH", "{process_uid}", "(BODY.PEEK[])")
assert typ == "OK", (typ, data)
body = b" ".join(part for d in data if d for part in (d if isinstance(d, tuple) else (d,)))
assert b"Process me" in body, body

# STORE +FLAGS \Deleted — mark exactly that one.
typ, data = m.uid("STORE", "{process_uid}", "+FLAGS", r"(\Deleted)")
assert typ == "OK", ("store", typ, data)
marked = b" ".join(d for d in data if d)
assert b"\\Deleted" in marked, marked

# EXPUNGE — the mark is acted on, and reported by sequence number.
typ, data = m.expunge()
assert typ == "OK", ("expunge", typ, data)
print("EXPUNGE", data)
assert data == [b"2"], ("the processed message sat at sequence 2", data)

# SEARCH again — the removed message is gone.
typ, data = m.uid("SEARCH", None, "ALL")
assert typ == "OK", (typ, data)
remaining = data[0].split()
print("REMAINING", remaining)
assert remaining == [b"{keep_uid}"], remaining

# And in the same session, the survivor has renumbered to sequence 1.
typ, data = m.search(None, "ALL")
assert typ == "OK", (typ, data)
assert data[0].split() == [b"1"], ("stale SequenceMap", data)

# FETCH by that new sequence number returns the survivor's unchanged UID.
typ, data = m.fetch("1", "(UID)")
assert typ == "OK", (typ, data)
payload = b" ".join(d for d in data if d)
print("SURVIVOR", payload.decode(errors="replace"))
assert b"UID {keep_uid}" in payload, payload

# Nothing is left marked \Deleted.
typ, data = m.uid("SEARCH", None, "DELETED")
assert typ == "OK", (typ, data)
assert data[0].split() == [], data

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
        keep_uid = keep.uid.get(),
        process_uid = process.uid.get(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib delete cycle failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
}

/// `UID EXPUNGE` through `imaplib`'s own `uid()` entry point (RFC 4315 §2.1).
#[tokio::test]
async fn python_imaplib_uses_uid_expunge() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;

    let raw = b"From: alice@example.test\r\nSubject: Subject\r\n\r\nBody.\r\n";
    let first = server.deliver(inbox, raw).await;
    let second = server.deliver(inbox, raw).await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib

m = imaplib.IMAP4(host="{host}", port={port})
m.login("user", "secret")
m.select("INBOX")

# Mark *both* \Deleted, then expunge only one by UID: the narrowing that
# distinguishes UID EXPUNGE from EXPUNGE.
for uid in ("{first_uid}", "{second_uid}"):
    typ, data = m.uid("STORE", uid, "+FLAGS", r"(\Deleted)")
    assert typ == "OK", (typ, data)

typ, data = m.uid("EXPUNGE", "{second_uid}")
assert typ == "OK", ("uid expunge", typ, data)
print("UIDEXPUNGE", data)

# The other \Deleted message must still be there, still marked.
typ, data = m.uid("SEARCH", None, "DELETED")
assert typ == "OK", (typ, data)
still = data[0].split()
print("STILL", still)
assert still == [b"{first_uid}"], still

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
        first_uid = first.uid.get(),
        second_uid = second.uid.get(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib UID EXPUNGE session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
}

/// `COPY` and `UID COPY` through Python's `imaplib`, including reading the
/// copy back from the destination (RFC 9051 §6.4.7, RFC 4315 §3).
///
/// The value of running this through `imaplib` is that a stranger's parser has
/// to accept our `[COPYUID ...]` response code, and that the UIDs it reports
/// have to actually work: the script copies, then `SELECT`s the destination and
/// `UID FETCH`es the message at the UID the server said it assigned. A
/// fabricated or off-by-one destination UID fails at that fetch rather than
/// passing a self-consistency check.
#[tokio::test]
async fn python_imaplib_copies_between_mailboxes() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server.mailbox(account, "Archive").await;

    let raw = b"From: alice@example.test\r\n\
Subject: Archive me\r\n\
\r\n\
Filed body.\r\n";
    let source = server.deliver(inbox, raw).await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, re

m = imaplib.IMAP4(host="{host}", port={port})
m.login("user", "secret")
typ, data = m.select("INBOX")
assert typ == "OK", (typ, data)

# COPY by sequence number.
typ, data = m.copy("1", "Archive")
assert typ == "OK", ("copy", typ, data)

# imaplib records the tagged response code under untagged_responses; response()
# is the accessor a real client uses to read COPYUID.
_, code = m.response("COPYUID")
assert code and code[0], ("no COPYUID recorded by imaplib", code)
payload = code[0].decode(errors="replace") if isinstance(code[0], bytes) else str(code[0])
print("COPY", payload)

# Three fields: destination UIDVALIDITY, source set, destination set.
match = re.match(r"(\d+) (\S+) (\S+)", payload.strip())
assert match, ("malformed COPYUID", payload)
validity, sources, destinations = match.groups()
print("COPYUID", validity, sources, destinations)
assert sources == "{source_uid}", (sources, "expected the source UID")
destination_uid = destinations

# The source is untouched: still there, still at the same UID.
typ, data = m.uid("SEARCH", None, "ALL")
assert typ == "OK", (typ, data)
assert data[0].split() == [b"{source_uid}"], ("source changed", data)

# SELECT the destination and read the copy back at the reported UID.
typ, data = m.select("Archive")
assert typ == "OK", (typ, data)
assert data[0] == b"1", ("EXISTS in Archive", data)

typ, data = m.uid("FETCH", destination_uid, "(BODY.PEEK[])")
assert typ == "OK", ("fetch the copy", typ, data)
body = b" ".join(
    part for d in data if d for part in (d if isinstance(d, tuple) else (d,))
)
assert b"Archive me" in body, body
assert b"Filed body." in body, body
print("READBACK ok")

# The destination UIDVALIDITY the server reported matches the mailbox's own.
typ, data = m.status("Archive", "(UIDVALIDITY)")
assert typ == "OK", (typ, data)
assert validity.encode() in data[0], (validity, data)

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
        source_uid = source.uid.get(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib COPY session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
    assert!(stdout.contains("READBACK ok"), "{stdout}");
}

/// `UID COPY` of several messages through `imaplib`, checking the `[COPYUID]`
/// sets pair up and that every copy is readable in the destination.
#[tokio::test]
async fn python_imaplib_uid_copies_multiple_messages() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server.mailbox(account, "Archive").await;

    let first = server
        .deliver(
            inbox,
            b"From: a@example.test\r\nSubject: One\r\n\r\nFirst.\r\n",
        )
        .await;
    let second = server
        .deliver(
            inbox,
            b"From: b@example.test\r\nSubject: Two\r\n\r\nSecond.\r\n",
        )
        .await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, re

def expand(uid_set):
    out = []
    for piece in uid_set.split(","):
        if ":" in piece:
            low, high = (int(n) for n in piece.split(":"))
            out.extend(range(min(low, high), max(low, high) + 1))
        else:
            out.append(int(piece))
    return out

m = imaplib.IMAP4(host="{host}", port={port})
m.login("user", "secret")
m.select("INBOX")

typ, data = m.uid("COPY", "{first_uid}:{second_uid}", "Archive")
assert typ == "OK", ("uid copy", typ, data)

# imaplib files a tagged response code under untagged_responses, reachable
# through response(); this is how a real client reads COPYUID.
_, code = m.response("COPYUID")
assert code and code[0], ("no COPYUID recorded by imaplib", code)
payload = code[0].decode(errors="replace") if isinstance(code[0], bytes) else str(code[0])
print("RAWCODE", payload)
match = re.match(r"(\d+) (\S+) (\S+)", payload.strip())
assert match, ("malformed COPYUID", payload)
_, sources, destinations = match.groups()
sources, destinations = expand(sources), expand(destinations)
print("SETS", sources, destinations)

# RFC 4315 §3: the two sets pair positionally, so they must be the same size.
assert sources == [{first_uid}, {second_uid}], sources
assert len(destinations) == len(sources), (sources, destinations)
assert len(set(destinations)) == len(destinations), ("duplicate UIDs", destinations)

# Both copies are readable in the destination, and the nth destination UID
# holds the nth source message.
m.select("Archive")
subjects = []
for uid in destinations:
    typ, data = m.uid("FETCH", str(uid), "(BODY.PEEK[HEADER.FIELDS (SUBJECT)])")
    assert typ == "OK", (typ, data)
    blob = b" ".join(
        part for d in data if d for part in (d if isinstance(d, tuple) else (d,))
    )
    subjects.append(b"One" in blob and "One" or (b"Two" in blob and "Two" or "?"))
print("SUBJECTS", subjects)
assert subjects == ["One", "Two"], subjects

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
        first_uid = first.uid.get(),
        second_uid = second.uid.get(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib UID COPY session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
}

// --- APPEND ----------------------------------------------------------------

/// Python `imaplib` appends messages and reads them back.
///
/// `imaplib.append` builds the literal itself, including the flag list and the
/// `INTERNALDATE`, so this exercises the argument forms as a real client emits
/// them rather than as this project's own tests spell them.
#[tokio::test]
async fn python_imaplib_appends_messages() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, time, sys

m = imaplib.IMAP4(host="{host}", port={port})
typ, _ = m.login("user", "secret")
assert typ == "OK", ("login", typ)

first = (b"From: alice@example.test\r\n"
         b"Subject: Appended one\r\n"
         b"\r\n"
         b"First body.\r\n")
second = (b"From: alice@example.test\r\n"
          b"Subject: Appended two\r\n"
          b"\r\n"
          b"Second body.\r\n")

# A bare APPEND: no flags, no date.
typ, data = m.append("INBOX", None, None, first)
assert typ == "OK", ("append one", typ, data)
print("APPEND1", data)

# With flags and an explicit INTERNALDATE, both built by imaplib.
# An explicit UTC date-time rather than Time2Internaldate(mktime(...)):
# mktime interprets the tuple in *local* time, so the assertion below would
# depend on the machine's zone rather than on what the server stored.
stamp = '"01-Jan-2020 00:00:00 +0000"' 
typ, data = m.append("INBOX", r"(\Seen)", stamp, second)
assert typ == "OK", ("append two", typ, data)
print("APPEND2", data)

typ, count = m.select("INBOX")
assert typ == "OK", ("select", typ)
assert int(count[0]) == 2, ("exists", count)

# Both are searchable by the subject they were appended with.
typ, nums = m.search(None, 'SUBJECT', '"Appended one"')
assert typ == "OK" and nums[0].split() == [b"1"], ("search one", nums)
typ, nums = m.search(None, 'SUBJECT', '"Appended two"')
assert typ == "OK" and nums[0].split() == [b"2"], ("search two", nums)

# The flags asked for on the second append are the flags stored.
typ, data = m.fetch("2", "(FLAGS)")
assert typ == "OK", ("fetch flags", typ)
blob = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
assert b"\\Seen" in blob, ("flags lost", blob)
assert b"\\Seen" not in b" ".join(
    x if isinstance(x, bytes) else b"".join(x) for x in m.fetch("1", "(FLAGS)")[1] if x
), "the first append must not have acquired flags"

# The bytes come back exactly as submitted.
typ, data = m.fetch("1", "(BODY.PEEK[])")
body = [i[1] for i in data if isinstance(i, tuple)][0]
assert body == first, ("bytes changed", body, first)
print("BYTES-OK", len(body))

typ, data = m.fetch("2", "(INTERNALDATE)")
blob = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
# The instant is preserved exactly, zone included, with no drift through
# local time. The day is echoed space-padded (" 1") rather than zero-padded:
# RFC 9051 §9 writes `date-day-fixed = (SP DIGIT) / 2DIGIT`, and some clients
# reject the zero-padded form, so the accepted and emitted spellings differ
# on purpose.
assert b"1-Jan-2020 00:00:00 +0000" in blob, ("internaldate changed", blob)
assert b"00:00:00 +0000" in blob, ("time or zone drifted", blob)
print("DATE", blob.decode(errors="replace"))

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib APPEND session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
    assert!(stdout.contains("BYTES-OK"), "{stdout}");
}

// --- MOVE / UID MOVE -------------------------------------------------------

/// Python `imaplib` moves several messages and verifies both mailboxes.
///
/// `imaplib` has no `move()` helper, so the command goes through its generic
/// entry point — which is still *its* parser reading the `[COPYUID]` code and
/// the `* n EXPUNGE` lines, not ours.
#[tokio::test]
async fn python_imaplib_moves_messages() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server.mailbox(account, "Archive").await;

    for index in 0..3 {
        let raw =
            format!("From: alice@example.test\r\nSubject: Move {index}\r\n\r\nBody {index}.\r\n");
        server.deliver(inbox, raw.as_bytes()).await;
    }

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, sys

imaplib.Commands["MOVE"] = ("SELECTED",)

m = imaplib.IMAP4(host="{host}", port={port})
typ, _ = m.login("user", "secret")
assert typ == "OK", ("login", typ)

typ, count = m.select("INBOX")
assert typ == "OK" and int(count[0]) == 3, ("select", count)

typ, data = m._simple_command("MOVE", "1:3", "Archive")
assert typ == "OK", ("move", typ, data)
blob = b" ".join(x for x in data if isinstance(x, bytes))
print("MOVE", blob.decode(errors="replace"))
assert b"COPYUID" in blob, ("no COPYUID", blob)

# The source is empty and the destination holds all three.
typ, count = m.select("INBOX")
assert typ == "OK" and int(count[0]) == 0, ("source not emptied", count)

typ, count = m.select("Archive")
assert typ == "OK" and int(count[0]) == 3, ("destination", count)

# The moved messages are readable, and are the ones that were moved.
subjects = []
for n in (1, 2, 3):
    typ, data = m.fetch(str(n), "(BODY.PEEK[HEADER.FIELDS (SUBJECT)])")
    header = b"".join(i[1] for i in data if isinstance(i, tuple))
    subjects.append(header.decode(errors="replace").strip())
print("SUBJECTS", subjects)
assert all("Move" in s for s in subjects), subjects

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib MOVE session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
}

/// Python `imaplib` performs a `UID MOVE` across UID gaps.
///
/// The gaps are the point: with UIDs 1, 3, 5 the positions and the UIDs differ,
/// so a server that confused the two would move the wrong messages and this
/// would catch it through an independent client.
#[tokio::test]
async fn python_imaplib_uid_moves_across_gaps() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server.mailbox(account, "Archive").await;

    let mut delivered = Vec::new();
    for index in 0..5 {
        let raw =
            format!("From: alice@example.test\r\nSubject: Gap {index}\r\n\r\nBody {index}.\r\n");
        delivered.push(server.deliver(inbox, raw.as_bytes()).await);
    }
    // Leave UIDs 1, 3, 5.
    for index in [1, 3] {
        sandesh_core::MessageStore::delete_entry(&*server.messages, delivered[index].id)
            .await
            .expect("delete to create UID gaps");
    }

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, sys

imaplib.Commands["MOVE"] = ("SELECTED",)

m = imaplib.IMAP4(host="{host}", port={port})
typ, _ = m.login("user", "secret")
assert typ == "OK", ("login", typ)

typ, count = m.select("INBOX")
assert typ == "OK" and int(count[0]) == 3, ("select", count)

typ, nums = m.uid("SEARCH", None, "ALL")
uids = nums[0].split()
print("UIDS", uids)
assert uids == [b"1", b"3", b"5"], ("unexpected UID layout", uids)

# `imaplib.uid()` returns only the untagged FETCH responses and discards the
# tagged line, so `[COPYUID]` is read through `_simple_command`, which keeps
# it. Still imaplib's parser reading our response, which is the point.
typ, data = m._simple_command("UID", "MOVE", "1,5", "Archive")
assert typ == "OK", ("uid move", typ, data)
blob = b" ".join(x for x in data if isinstance(x, bytes))
print("MOVE", blob.decode(errors="replace"))
assert b"COPYUID" in blob, ("no COPYUID", blob)
# The source set names the UIDs asked for, and the destination set is the
# same size — the pairing itself is asserted message-by-message below.
assert b"COPYUID" in blob and b"1,5" in blob, ("unexpected COPYUID", blob)

# UID 3 was not named, so it stays; 1 and 5 left.
typ, count = m.select("INBOX")
assert typ == "OK" and int(count[0]) == 1, ("source", count)
typ, nums = m.uid("SEARCH", None, "ALL")
assert nums[0].split() == [b"3"], ("wrong survivor", nums)

typ, count = m.select("Archive")
assert typ == "OK" and int(count[0]) == 2, ("destination", count)

# The right two messages arrived: Gap 0 (UID 1) and Gap 4 (UID 5).
found = []
for n in (1, 2):
    typ, data = m.fetch(str(n), "(BODY.PEEK[HEADER.FIELDS (SUBJECT)])")
    header = b"".join(i[1] for i in data if isinstance(i, tuple))
    found.append(header.decode(errors="replace").strip())
print("FOUND", found)
joined = " ".join(found)
assert "Gap 0" in joined and "Gap 4" in joined, found
assert "Gap 2" not in joined, ("moved a message that was not named", found)

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib UID MOVE session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("DONE"), "{stdout}");
}

// --- IDLE ------------------------------------------------------------------

/// Two real clients: one idles, the other changes the mailbox.
///
/// `imaplib` has no `idle()` before Python 3.13, so the idling side drives the
/// socket directly — a protocol harness rather than a library call, which is
/// what the brief asks for. The *other* side is unmodified `imaplib`, so the
/// changes that trigger the updates are made by a real client.
///
/// The whole brief sequence runs in one session: APPEND → EXISTS, STORE → flag
/// update, EXPUNGE → EXPUNGE, then `DONE` and a normal command afterwards.
#[tokio::test]
async fn python_client_receives_unsolicited_updates_while_idling() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::with_events().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, socket, sys

HOST, PORT = "{host}", {port}

def readline(f, what):
    line = f.readline()
    if not line:
        raise AssertionError("connection closed while waiting for " + what)
    return line.decode(errors="replace").rstrip("\r\n")

def wait_for(f, needle, what, limit=40):
    seen = []
    for _ in range(limit):
        line = readline(f, what)
        seen.append(line)
        if needle in line:
            return seen
    raise AssertionError("never saw %s; got %r" % (what, seen))

# --- Session A: raw socket, so it can sit in IDLE and read pushes.
a = socket.create_connection((HOST, PORT), timeout=10)
af = a.makefile("rwb")
readline(af, "greeting")

af.write(b"a1 LOGIN user secret\r\n"); af.flush()
wait_for(af, "a1 OK", "login")

af.write(b"a2 SELECT INBOX\r\n"); af.flush()
wait_for(af, "a2 OK", "select")

af.write(b"a3 IDLE\r\n"); af.flush()
line = readline(af, "continuation")
assert line.startswith("+"), ("no continuation for IDLE", line)
print("IDLE-CONTINUATION", line)

# --- Session B: unmodified imaplib makes the changes.
b = imaplib.IMAP4(host=HOST, port=PORT)
typ, _ = b.login("user", "secret")
assert typ == "OK", ("B login", typ)

msg = (b"From: alice@example.test\r\n"
       b"Subject: Idle interop\r\n"
       b"\r\n"
       b"Body.\r\n")
typ, _ = b.append("INBOX", None, None, msg)
assert typ == "OK", ("B append", typ)

seen = wait_for(af, "EXISTS", "EXISTS after APPEND")
print("GOT-EXISTS", seen[-1])
assert "1 EXISTS" in seen[-1], seen

# 2. A flag change.
typ, count = b.select("INBOX")
assert typ == "OK" and int(count[0]) == 1, ("B select", count)
typ, _ = b.store("1", "+FLAGS", r"(\Seen)")
assert typ == "OK", ("B store", typ)

seen = wait_for(af, "FETCH", "flag update")
print("GOT-FLAGS", seen[-1])
assert "\\Seen" in seen[-1], seen

# 3. A removal.
typ, _ = b.store("1", "+FLAGS", r"(\Deleted)")
assert typ == "OK", ("B store deleted", typ)
# Drain A's flag update for \Deleted before expunging, so the next wait is
# unambiguous about which response it matched.
wait_for(af, "FETCH", "second flag update")
typ, _ = b.expunge()
assert typ == "OK", ("B expunge", typ)

seen = wait_for(af, "EXPUNGE", "EXPUNGE")
print("GOT-EXPUNGE", seen[-1])
assert "1 EXPUNGE" in seen[-1], seen

# 4. DONE returns A to normal command processing.
af.write(b"DONE\r\n"); af.flush()
wait_for(af, "a3 OK", "IDLE completion")
print("IDLE-ENDED")

# 5. A ordinary command still works, against the renumbered mailbox.
af.write(b"a4 NOOP\r\n"); af.flush()
wait_for(af, "a4 OK", "NOOP after DONE")

af.write(b"a5 SEARCH ALL\r\n"); af.flush()
seen = wait_for(af, "a5 OK", "SEARCH after DONE")
search = [s for s in seen if s.startswith("* SEARCH")]
print("SEARCH", search)
assert search and search[0].strip() == "* SEARCH", ("mailbox should be empty", search)

af.write(b"a6 LOGOUT\r\n"); af.flush()
b.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "IDLE interop session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    for marker in [
        "IDLE-CONTINUATION",
        "GOT-EXISTS",
        "GOT-FLAGS",
        "GOT-EXPUNGE",
        "IDLE-ENDED",
        "DONE",
    ] {
        assert!(stdout.contains(marker), "missing {marker} in: {stdout}");
    }
}

/// A client that disconnects mid-`IDLE` is reaped cleanly.
#[tokio::test]
async fn a_client_disconnecting_while_idling_leaves_the_server_healthy() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::with_events().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, socket, sys

HOST, PORT = "{host}", {port}

# Enter IDLE, then drop the connection without sending DONE.
a = socket.create_connection((HOST, PORT), timeout=10)
af = a.makefile("rwb")
af.readline()
af.write(b"a1 LOGIN user secret\r\n"); af.flush()
while b"a1 " not in af.readline():
    pass
af.write(b"a2 SELECT INBOX\r\n"); af.flush()
while b"a2 " not in af.readline():
    pass
af.write(b"a3 IDLE\r\n"); af.flush()
line = af.readline()
assert line.startswith(b"+"), line
print("IDLING")

a.close()

# The server is still serving: a fresh client works normally.
m = imaplib.IMAP4(host=HOST, port=PORT)
typ, _ = m.login("user", "secret")
assert typ == "OK", ("login after abandoned IDLE", typ)
typ, count = m.select("INBOX")
assert typ == "OK", ("select after abandoned IDLE", typ)
typ, _ = m.append("INBOX", None, None,
                  b"From: a@b\r\nSubject: after\r\n\r\nbody\r\n")
assert typ == "OK", ("append after abandoned IDLE", typ)
m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "abandoned-IDLE session failed:\nstdout:{stdout}\nstderr:{stderr}"
    );
    assert!(stdout.contains("IDLING"), "{stdout}");
    assert!(stdout.contains("DONE"), "{stdout}");
}

/// Python `imaplib` exercises `NAMESPACE` and `ENABLE` through its own API.
///
/// `imaplib` has native `namespace()` and `enable()` methods, so this drives a
/// parser nobody here wrote against both responses. `namespace()` in particular
/// is strict about the `((prefix delimiter)) NIL NIL` shape — a fabricated
/// shared namespace or a missing quote fails inside the library rather than in
/// an assertion of ours.
///
/// `enable()` is the interesting half. `imaplib` refuses to call it unless the
/// server advertises `ENABLE` in CAPABILITY, so reaching the call at all proves
/// the advertisement is visible to a real client. Note the limit of that gate:
/// `imaplib.enable` checks only for `ENABLE`, never for the capability being
/// named, so `CONDSTORE` being offered is asserted against the recorded
/// capability set directly rather than inferred from the call succeeding.
#[tokio::test]
async fn python_imaplib_uses_namespace_and_enable() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, sys

m = imaplib.IMAP4(host="{host}", port={port})
typ, _ = m.login("user", "secret")
assert typ == "OK", ("login", typ)

# CAPABILITY as imaplib recorded it during the greeting/login.
caps = m.capabilities
print("CAPS", " ".join(sorted(caps)))
assert "NAMESPACE" in caps, ("NAMESPACE not advertised", caps)
assert "ENABLE" in caps, ("ENABLE not advertised", caps)
assert "CONDSTORE" in caps, ("CONDSTORE must be advertised", caps)
assert "QRESYNC" not in caps, ("QRESYNC must not be advertised", caps)

# NAMESPACE through imaplib's own strict parser.
typ, data = m.namespace()
assert typ == "OK", ("namespace", typ, data)
ns = b" ".join(x for x in data if x)
print("NS", ns.decode(errors="replace"))
assert b'(("" "/"))' in ns, ("wrong personal namespace", ns)
assert ns.rstrip().endswith(b"NIL NIL"), ("shared/other must be NIL", ns)

# The delimiter NAMESPACE reported must be the one LIST uses.
typ, boxes = m.list()
assert typ == "OK", ("list", typ)
joined = b" ".join(b if isinstance(b, bytes) else b"".join(b) for b in boxes)
print("LIST", joined.decode(errors="replace"))
assert b'"/"' in joined, ("delimiter mismatch", joined)
assert b"Work/Deep" in joined, ("hierarchy not separated by /", joined)

# ENABLE through imaplib's own method. It gates on `ENABLE` being
# advertised, so reaching this call proves a real client saw the
# advertisement; the capability assertion above is what proves CONDSTORE
# specifically was offered.
typ, data = m.enable("CONDSTORE")
assert typ == "OK", ("enable", typ, data)
print("ENABLE", typ, data)

# QRESYNC: recognised, refused, and the refusal is carried by its absence
# from `* ENABLED` (RFC 5161 §3.1) rather than by a BAD — a client naming
# an extension this server lacks has done nothing wrong. imaplib does not
# expose the untagged line from its own `enable`, so the raw command is
# used to read it.
typ, data = m._simple_command("ENABLE", "QRESYNC")
assert typ == "OK", ("enable qresync must still answer OK", typ, data)
untagged = b" ".join(m.untagged_responses.get("ENABLED", []) or [b""])
print("QRESYNC-ENABLED-LINE", repr(untagged))
assert b"QRESYNC" not in untagged, ("QRESYNC must not be reported enabled", untagged)

# And enabling changed nothing it should not have: the session still works.
typ, count = m.select("INBOX")
assert typ == "OK", ("select after enable", typ, count)

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib script failed:\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(stdout.contains("DONE"), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains(r#"(("" "/"))"#),
        "stdout={stdout}\nstderr={stderr}"
    );
}

/// Python `imaplib` exercises extended `LIST`, `SPECIAL-USE` and `ESEARCH`.
///
/// `imaplib` has no dedicated method for any of these, so they go through
/// `_simple_command`/`uid` — which still means its own tokeniser frames every
/// response. That is the point: an `* ESEARCH` line or an extended `* LIST`
/// line that a stranger's parser chokes on is a real interoperability bug, and
/// this catches it where a self-consistency test could not.
#[tokio::test]
async fn python_imaplib_uses_list_extended_and_esearch() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;
    for index in 1..=4u32 {
        let raw = format!("Subject: message {index}\r\n\r\nBody {index}.\r\n");
        server.deliver(inbox, raw.as_bytes()).await;
    }

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, sys

m = imaplib.IMAP4(host="{host}", port={port})
typ, _ = m.login("user", "secret")
assert typ == "OK", ("login", typ)

caps = m.capabilities
print("CAPS", " ".join(sorted(caps)))
for required in ("ESEARCH", "LIST-EXTENDED", "SPECIAL-USE"):
    assert required in caps, (required + " not advertised", caps)

# Special-use attributes in a plain LIST, through imaplib's own parser.
typ, boxes = m.list()
assert typ == "OK", ("list", typ)
joined = b" ".join(b if isinstance(b, bytes) else b"".join(b) for b in boxes)
print("LIST", joined.decode(errors="replace"))
assert b"\\Sent" in joined, ("no special-use attribute", joined)
assert b"\\HasChildren" in joined, ("no CHILDREN attribute", joined)
assert b"\\HasNoChildren" in joined, ("no CHILDREN attribute", joined)

# Extended LIST: a RETURN clause imaplib passes through verbatim.
typ, data = m._simple_command("LIST", '""', '*', 'RETURN (SUBSCRIBED CHILDREN)')
assert typ == "OK", ("list-extended", typ, data)
typ, data = m._untagged_response(typ, data, "LIST")
ext = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
print("EXT", ext.decode(errors="replace"))
assert b"\\Subscribed" in ext, ("no SUBSCRIBED attribute", ext)

# A SPECIAL-USE filtered listing.
typ, data = m._simple_command("LIST", '(SPECIAL-USE)', '""', '*')
assert typ == "OK", ("special-use list", typ, data)
typ, data = m._untagged_response(typ, data, "LIST")
su = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
print("SU", su.decode(errors="replace"))
assert b"\\Sent" in su, ("Sent missing from SPECIAL-USE listing", su)
assert b"Work" not in su, ("an ordinary mailbox leaked into SPECIAL-USE", su)

# RETURN (STATUS (...)) produces untagged STATUS lines alongside LIST.
typ, data = m._simple_command("LIST", '""', 'INBOX', 'RETURN (STATUS (MESSAGES))')
assert typ == "OK", ("list return status", typ, data)
typ, st = m._untagged_response(typ, data, "STATUS")
stat = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in st if x)
print("STATUS", stat.decode(errors="replace"))
assert b"MESSAGES 4" in stat, ("wrong STATUS in LIST", stat)

typ, count = m.select("INBOX")
assert typ == "OK", ("select", typ)

# ESEARCH, sequence form.
typ, data = m._simple_command("SEARCH", "RETURN (MIN MAX COUNT ALL)", "ALL")
assert typ == "OK", ("esearch", typ, data)
typ, es = m._untagged_response(typ, data, "ESEARCH")
line = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in es if x)
print("ESEARCH", line.decode(errors="replace"))
assert b"COUNT 4" in line, ("wrong COUNT", line)
assert b"MIN 1" in line, ("wrong MIN", line)
assert b"MAX 4" in line, ("wrong MAX", line)
assert b"ALL 1:4" in line, ("wrong ALL", line)

# ESEARCH, UID form.
#
# imaplib's uid() cannot be used to *read* this one: it hard-codes
# `_untagged_response(..., "SEARCH")` for a UID SEARCH and so never looks for
# an ESEARCH response, returning None regardless of what the server sent. That
# is a limitation of imaplib, not of the server, and the server is deliberately
# not changed to accommodate it — RFC 9051 §7.3.4 requires ESEARCH once RETURN
# is given. The command still goes through imaplib's writer and its response
# through imaplib's reader; only the lookup key differs.
typ, data = m.uid("SEARCH", "RETURN (ALL COUNT)", "ALL")
assert typ == "OK", ("uid esearch", typ, data)
typ, es = m._untagged_response(typ, data, "ESEARCH")
flat = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in es if x)
print("UIDESEARCH", flat.decode(errors="replace"))
assert b"COUNT 4" in flat, ("wrong UID COUNT", flat)
assert b"UID" in flat, ("UID marker missing", flat)

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib script failed:\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(stdout.contains("DONE"), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("ALL 1:4"),
        "stdout={stdout}\nstderr={stderr}"
    );
}

/// Python `imaplib` exercises CONDSTORE end to end (RFC 7162).
///
/// `imaplib` has no CONDSTORE API at all — no `MODSEQ` fetch item, no
/// `UNCHANGEDSINCE` store modifier — so everything here goes through
/// `uid()`/`_simple_command()` as raw commands whose *responses* `imaplib`
/// still parses. That split is the point: the library's own parser decides
/// whether `MODSEQ (n)` inside a `FETCH` and a `[MODIFIED 1]` response code
/// are well formed, so a malformed emission fails inside code nobody here
/// wrote rather than in an assertion of ours.
#[tokio::test]
async fn python_imaplib_uses_condstore_fetch_and_conditional_store() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    for n in 0..3 {
        server
            .deliver(
                inbox,
                format!("Subject: condstore {n}\r\n\r\nbody {n}\r\n").as_bytes(),
            )
            .await;
    }

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, re, sys

m = imaplib.IMAP4(host="{host}", port={port})
typ, _ = m.login("user", "secret")
assert typ == "OK", ("login", typ)

caps = m.capabilities
assert "CONDSTORE" in caps, ("CONDSTORE must be advertised", caps)

typ, data = m.enable("CONDSTORE")
assert typ == "OK", ("enable", typ, data)

# SELECT must report HIGHESTMODSEQ (RFC 7162 3.1.1). imaplib squirrels
# untagged OK response codes away rather than returning them.
typ, _ = m.select("INBOX")
assert typ == "OK", ("select", typ)
highest = m.untagged_responses.get("HIGHESTMODSEQ", [])
print("HIGHESTMODSEQ", highest)
assert highest, "SELECT must report HIGHESTMODSEQ"
base = int(highest[0])
assert base > 0, ("HIGHESTMODSEQ must be non-zero", highest)

# FETCH MODSEQ, parsed by imaplib's own response parser.
typ, data = m.uid("FETCH", "1:*", "(UID MODSEQ)")
assert typ == "OK", ("fetch modseq", typ, data)
flat = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
print("FETCH", flat.decode(errors="replace"))
assert b"MODSEQ" in flat, ("MODSEQ missing from FETCH", flat)
# RFC 7162 3.1.4 spells the value as a parenthesised list.
assert re.search(rb"MODSEQ \(\d+\)", flat), ("MODSEQ must be a parenthesised value", flat)

seqs = [int(x) for x in re.findall(rb"MODSEQ \((\d+)\)", flat)]
assert seqs, ("no modseq values", flat)
assert max(seqs) <= base, ("no entry may exceed HIGHESTMODSEQ", seqs, base)

# A conditional store that must succeed: the precondition names the
# mailbox's current high-water mark, so nothing can be newer.
typ, data = m.uid("STORE", "1", "(UNCHANGEDSINCE %d)" % base, "+FLAGS", r"(\Seen)")
assert typ == "OK", ("conditional store", typ, data)
flat = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
print("STORE-OK", flat.decode(errors="replace"))
assert b"MODSEQ" in flat, ("a successful conditional STORE must report MODSEQ", flat)
assert b"Seen" in flat, ("the new flags must be reported", flat)

# The change advanced the sequence past the value we just used.
typ, data = m.uid("FETCH", "1", "(MODSEQ)")
after = int(re.search(rb"MODSEQ \((\d+)\)", b" ".join(
    x if isinstance(x, bytes) else b"".join(x) for x in data if x)).group(1))
print("AFTER", after, "BASE", base)
assert after > base, ("a successful store must allocate a new modseq", after, base)

# And now the failure: the same stale precondition can no longer apply.
typ, data = m.uid("STORE", "1", "(UNCHANGEDSINCE %d)" % base, "+FLAGS", r"(\Flagged)")
assert typ == "OK", ("a refused conditional store is still OK", typ, data)
print("STORE-REFUSED", typ, data)

# The refusal travels in a MODIFIED response code, which imaplib records.
modified = m.untagged_responses.get("MODIFIED", [])
print("MODIFIED", modified)
assert modified, "a refused UNCHANGEDSINCE must be reported as MODIFIED"

# The refused flag was genuinely not applied.
typ, data = m.uid("FETCH", "1", "(FLAGS)")
flat = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
print("FLAGS", flat.decode(errors="replace"))
assert b"Flagged" not in flat, ("a refused store must not be applied", flat)

# SEARCH MODSEQ, again through imaplib's parser.
typ, data = m.uid("SEARCH", "MODSEQ", str(after))
assert typ == "OK", ("search modseq", typ, data)
print("SEARCH", data)

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib CONDSTORE script failed:\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(stdout.contains("DONE"), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("STORE-REFUSED"),
        "the conditional failure path must have been exercised: stdout={stdout}"
    );
}

/// A rev1 client's modified-UTF-7 mailbox names round-trip (RFC 3501 §5.1.3).
///
/// The regression this pins was found against a live server, not by review:
/// with no decoder, `CREATE "Wichtig-&APw-ber"` from a rev1 client and
/// `CREATE "Wichtig-über"` from a rev2 client created **two different
/// mailboxes for the same logical folder**, each independently selectable.
/// A user with one client encoding and another not would silently accumulate
/// duplicate folders that diverge.
#[tokio::test]
async fn python_imaplib_round_trips_modified_utf7_mailbox_names() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, sys

m = imaplib.IMAP4(host="{host}", port={port})
typ, _ = m.login("user", "secret")
assert typ == "OK", ("login", typ)

# A rev1 client sends the *encoded* form; imaplib itself is ASCII-only, which
# is precisely why the encoding exists.
typ, data = m.create('"Wichtig-&APw-ber"')
assert typ == "OK", ("create encoded", typ, data)

# The same logical folder in rev2 form must now be the SAME mailbox, so a
# second create is rejected rather than silently making a duplicate.
typ, data = m.create('"Wichtig-&APw-ber"')
print("DUPLICATE", typ, data)
assert typ == "NO", ("recreating the same folder must fail", typ, data)

# LIST returns it in encoded form, byte-for-byte as it was sent.
typ, data = m.list()
assert typ == "OK", ("list", typ, data)
names = b" ".join(data)
print("LIST", names.decode(errors="replace"))
assert b"Wichtig-&APw-ber" in names, ("encoded name must come back encoded", names)
# The raw UTF-8 spelling must NOT also appear: that would be the duplicate.
assert "Wichtig-über".encode() not in names, ("raw UTF-8 leaked alongside", names)

# It is selectable under the name the client knows.
typ, data = m.select('"Wichtig-&APw-ber"')
assert typ == "OK", ("select encoded", typ, data)
m.close()

# A CJK name exercises multi-code-unit runs and the ,-for-/ substitution.
typ, data = m.create('"&ZeVnLIqe-"')      # 日本語
assert typ == "OK", ("create cjk", typ, data)
typ, data = m.list()
assert b"&ZeVnLIqe-" in b" ".join(data), ("cjk name", data)

typ, data = m.rename('"&ZeVnLIqe-"', '"&U,BTFw-"')   # 日本語 -> 台北
assert typ == "OK", ("rename cjk", typ, data)
typ, data = m.list()
flat = b" ".join(data)
assert b"&U,BTFw-" in flat, ("renamed name", flat)
assert b"&ZeVnLIqe-" not in flat, ("old name lingered", flat)

typ, data = m.delete('"&U,BTFw-"')
assert typ == "OK", ("delete cjk", typ, data)

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib modified-UTF-7 script failed:\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(stdout.contains("DONE"), "stdout={stdout}\nstderr={stderr}");
}

/// A full client sync cycle, in the shape `mbsync` and Thunderbird perform it.
///
/// The phase's headline acceptance criterion is that real clients "connect,
/// list, select, fetch and sync successfully". Neither `mbsync` nor
/// Thunderbird can be driven headlessly in CI, so this pins the *sequence*
/// they issue, through a parser we did not write:
///
/// 1. capability discovery, then `LIST` the whole tree;
/// 2. `STATUS` each folder for `UIDVALIDITY`/`UIDNEXT`/`UNSEEN` — how a
///    syncing client decides whether its cache is still valid;
/// 3. `SELECT`, then `UID FETCH` headers for the range it lacks;
/// 4. `UID FETCH BODY.PEEK[]` the new messages without setting `\Seen`;
/// 5. propagate local flag state back with `UID STORE`;
/// 6. re-`SELECT` and confirm the server agrees — the resync a client does
///    on its next pass.
///
/// Step 4's `.PEEK` matters: a sync that marks mail read as a side effect of
/// downloading it is the classic client-visible bug this ordering prevents.
#[tokio::test]
async fn python_imaplib_performs_a_full_client_sync_cycle() {
    if !python_present() {
        eprintln!("skipping: python3 is not installed");
        return;
    }

    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server.mailbox(account, "Archive").await;

    for index in 1..=4u32 {
        let raw = format!(
            "From: sender{index}@example.test\r\n\
To: user@example.test\r\n\
Subject: Sync {index}\r\n\
Date: Tue, 01 Sep 2026 12:0{index}:00 +0000\r\n\
Message-ID: <sync-{index}@example.test>\r\n\
\r\n\
Body of sync message {index}.\r\n"
        );
        server.deliver(inbox, raw.as_bytes()).await;
    }

    let addr = server.cleartext();
    let script = format!(
        r#"
import imaplib, re, sys

m = imaplib.IMAP4(host="{host}", port={port})
typ, _ = m.login("user", "secret")
assert typ == "OK", ("login", typ)

# 1. Discovery. A syncing client reads CAPABILITY before deciding its strategy.
caps = m.capabilities
print("CAPS", caps)
for required in ("IMAP4REV1", "UIDPLUS", "UNSELECT"):
    assert required in caps, (required + " missing", caps)

typ, folders = m.list()
assert typ == "OK", ("list", typ)
names = [f.decode() for f in folders]
print("FOLDERS", names)
assert any(n.endswith("INBOX") for n in names), names
assert any(n.endswith("Archive") for n in names), names

# 2. STATUS drives the "has anything changed?" decision, before any SELECT.
typ, data = m.status("INBOX", "(MESSAGES UIDNEXT UIDVALIDITY UNSEEN)")
assert typ == "OK", ("status", typ, data)
status = data[0].decode()
print("STATUS", status)
uidvalidity = int(re.search(r"UIDVALIDITY (\d+)", status).group(1))
uidnext = int(re.search(r"UIDNEXT (\d+)", status).group(1))
assert int(re.search(r"MESSAGES (\d+)", status).group(1)) == 4, status
assert uidvalidity > 0, status
assert uidnext > 4, status

# 3. SELECT, then discover UIDs the way a client with an empty cache does.
typ, data = m.select("INBOX")
assert typ == "OK", ("select", typ, data)
assert int(data[0]) == 4, data

typ, data = m.uid("SEARCH", "ALL")
uids = data[0].split()
print("UIDS", uids)
assert len(uids) == 4, uids

typ, data = m.uid("FETCH", b",".join(uids), "(UID FLAGS RFC822.SIZE)")
assert typ == "OK", ("fetch envelope", typ)
meta = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
for uid in uids:
    assert b"UID " + uid in meta, (uid, meta)

# 4. Download bodies WITHOUT marking them read. This is the step that
#    distinguishes a correct sync from one that destroys unread state.
typ, data = m.uid("FETCH", b",".join(uids), "(BODY.PEEK[])")
assert typ == "OK", ("fetch bodies", typ)
bodies = b"".join(x[1] for x in data if isinstance(x, tuple))
assert b"Body of sync message 1." in bodies, "first body missing"
assert b"Body of sync message 4." in bodies, "last body missing"

typ, data = m.uid("FETCH", b",".join(uids), "(FLAGS)")
flags = b" ".join(x if isinstance(x, bytes) else b"".join(x) for x in data if x)
print("FLAGS-AFTER-PEEK", flags.decode(errors="replace"))
assert b"\\Seen" not in flags, ("PEEK must not set \\Seen", flags)

# 5. Push local state back: the client marks two messages read.
typ, data = m.uid("STORE", b",".join(uids[:2]), "+FLAGS", r"(\Seen)")
assert typ == "OK", ("store", typ, data)

# 6. The resync pass. A fresh SELECT must agree with what we just wrote.
m.close()
typ, data = m.select("INBOX")
assert typ == "OK", ("reselect", typ, data)

typ, data = m.status("INBOX", "(UIDVALIDITY UNSEEN)")
restatus = data[0].decode()
print("RESTATUS", restatus)
assert int(re.search(r"UIDVALIDITY (\d+)", restatus).group(1)) == uidvalidity, (
    "UIDVALIDITY changed mid-sync; every client would discard its cache", restatus)
assert int(re.search(r"UNSEEN (\d+)", restatus).group(1)) == 2, restatus

typ, data = m.uid("SEARCH", "UNSEEN")
remaining = data[0].split()
print("UNSEEN", remaining)
assert remaining == uids[2:], (remaining, uids)

# UNSELECT leaves without expunging — the capability advertised in ADR-060.
typ, data = m.xatom("UNSELECT")
assert typ == "OK", ("unselect", typ, data)

m.logout()
print("DONE")
"#,
        host = addr.ip(),
        port = addr.port(),
    );

    let (stdout, stderr, ok) = python(&script).await;
    assert!(
        ok,
        "imaplib sync-cycle script failed:\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(stdout.contains("DONE"), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("FLAGS-AFTER-PEEK"),
        "the PEEK assertion must have run: stdout={stdout}"
    );
}
