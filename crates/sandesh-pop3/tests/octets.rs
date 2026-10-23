//! Octet accounting: `STAT`/`LIST` totals and `RETR` byte-exactness (Phase 6).
//!
//! The Phase 6 acceptance criteria make two claims about sizes that the rest of
//! the suite asserts only indirectly: that `STAT` octet counts match
//! `RFC822.SIZE`, and that `RETR` returns byte-identical content. Every prior
//! `STAT` assertion checked the message *count* and ignored the octet total, so
//! the number could have been anything at all.
//!
//! The fixtures below deliberately include the shapes where the wire encoding
//! and the stored bytes diverge — a bare-LF terminator, a leading-dot body line
//! and a message with no trailing newline — because those are exactly the cases
//! a CRLF-only fixture would hide.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
use common::TestServer;

/// A CRLF message with nothing needing an escape: the easy case.
const PLAIN: &[u8] = b"From: a@b.c\r\nSubject: plain\r\n\r\nHello world\r\n";

/// A body line starting with '.', which must be dot-stuffed on the wire and
/// un-stuffed by the client. Ordinary mail, not an edge case.
const LEADING_DOT: &[u8] = b"From: a@b.c\r\nSubject: dotted\r\n\r\n.hidden\r\nvisible\r\n";

/// Bare LF endings. SMTP preserves these verbatim (Phase 2), so they reach
/// storage and POP3 has to serve them.
const BARE_LF: &[u8] = b"From: a@b.c\nSubject: bare\n\nBody line\n";

/// No trailing newline at all: the terminator has to be synthesised.
const NO_TRAILING_NEWLINE: &[u8] = b"From: a@b.c\r\nSubject: abrupt\r\n\r\nNo newline here";

#[tokio::test]
async fn stat_octet_total_matches_stored_sizes() {
    let server = TestServer::start().await;
    let (_acc, inbox) = server.create_account("alice").await;

    for raw in [PLAIN, LEADING_DOT, BARE_LF] {
        server.insert_message(inbox, raw).await;
    }
    let expected: usize = [PLAIN, LEADING_DOT, BARE_LF].iter().map(|m| m.len()).sum();

    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    client.cmd("PASS pass").await;

    let stat = client.cmd("STAT").await;
    let reported: usize = stat
        .trim_end()
        .strip_prefix("+OK ")
        .and_then(|rest| rest.split_whitespace().nth(1))
        .expect("STAT reports a count and an octet total")
        .parse()
        .expect("the octet total is a number");

    assert_eq!(
        reported, expected,
        "STAT must report the summed RFC822.SIZE of the undeleted messages"
    );
}

#[tokio::test]
async fn stat_octet_total_drops_deleted_messages() {
    // The total is over *undeleted* messages, so a DELE has to move it. Without
    // this, a STAT that ignored `deleted` entirely would pass the test above.
    let server = TestServer::start().await;
    let (_acc, inbox) = server.create_account("alice").await;

    server.insert_message(inbox, PLAIN).await;
    server.insert_message(inbox, LEADING_DOT).await;

    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    client.cmd("PASS pass").await;

    client.cmd("DELE 1").await;

    let stat = client.cmd("STAT").await;
    let reported: usize = stat
        .trim_end()
        .strip_prefix("+OK ")
        .and_then(|rest| rest.split_whitespace().nth(1))
        .expect("STAT reports a count and an octet total")
        .parse()
        .expect("the octet total is a number");

    assert_eq!(
        reported,
        LEADING_DOT.len(),
        "a deleted message must not count toward the STAT octet total"
    );
}

#[tokio::test]
async fn list_sizes_match_stored_sizes_per_message() {
    let server = TestServer::start().await;
    let (_acc, inbox) = server.create_account("alice").await;

    let fixtures = [PLAIN, LEADING_DOT, BARE_LF, NO_TRAILING_NEWLINE];
    for raw in fixtures {
        server.insert_message(inbox, raw).await;
    }

    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    client.cmd("PASS pass").await;

    let (_status, lines) = client.cmd_multiline("LIST").await;
    assert_eq!(lines.len(), fixtures.len());

    for (index, raw) in fixtures.iter().enumerate() {
        let reported: usize = lines[index]
            .split_whitespace()
            .nth(1)
            .expect("LIST reports a size")
            .parse()
            .expect("the size is a number");
        assert_eq!(
            reported,
            raw.len(),
            "LIST size for message {} must equal its stored RFC822.SIZE",
            index + 1
        );
    }
}

#[tokio::test]
async fn retr_returns_byte_identical_content_for_every_shape() {
    // The criterion is "byte-identical content, correctly dot-stuffed on the
    // wire". Un-stuffing the wire form must reproduce the stored bytes exactly,
    // including whichever line endings were stored.
    let server = TestServer::start().await;
    let (_acc, inbox) = server.create_account("alice").await;

    let fixtures = [
        ("plain", PLAIN),
        ("leading dot", LEADING_DOT),
        ("bare LF", BARE_LF),
        ("no trailing newline", NO_TRAILING_NEWLINE),
    ];
    for (_, raw) in fixtures {
        server.insert_message(inbox, raw).await;
    }

    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    client.cmd("PASS pass").await;

    for (index, (label, raw)) in fixtures.iter().enumerate() {
        let (status, payload) = client
            .cmd_multiline_bytes(&format!("RETR {}", index + 1))
            .await;
        assert!(status.starts_with("+OK"), "{label}: RETR failed: {status}");

        // A stored bare LF or a missing terminator is canonicalised to CRLF on
        // the wire, because RFC 1939 frames the response in CRLF-terminated
        // lines and a bare LF would not end one. Compare on that basis.
        let expected = canonical_crlf(raw);
        assert_eq!(
            payload,
            expected,
            "{label}: un-stuffed RETR payload must reproduce the message\n  \
             expected: {:?}\n  actual:   {:?}",
            String::from_utf8_lossy(&expected),
            String::from_utf8_lossy(&payload),
        );
    }
}

#[tokio::test]
async fn retr_greeting_does_not_announce_a_wrong_octet_count() {
    // If the greeting carries a count at all, it has to be the number of octets
    // the client actually receives. Announcing the stored size while sending
    // dot-stuffed, CRLF-canonicalised bytes is what this pins.
    let server = TestServer::start().await;
    let (_acc, inbox) = server.create_account("alice").await;

    for raw in [LEADING_DOT, BARE_LF, NO_TRAILING_NEWLINE] {
        server.insert_message(inbox, raw).await;
    }

    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    client.cmd("PASS pass").await;

    for index in 1..=3 {
        let (status, payload) = client.cmd_multiline_bytes(&format!("RETR {index}")).await;

        if let Some(announced) = parse_octets(&status) {
            assert_eq!(
                announced,
                payload.len(),
                "RETR {index} announced {announced} octets but delivered {}",
                payload.len()
            );
        }
    }
}

/// Rewrites bare LF endings as CRLF, matching how the wire must frame them.
fn canonical_crlf(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 2);
    let mut previous = None;
    for &byte in raw {
        if byte == b'\n' && previous != Some(b'\r') {
            out.push(b'\r');
        }
        out.push(byte);
        previous = Some(byte);
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// Extracts the octet count from a `+OK <n> octets` greeting, if it has one.
fn parse_octets(status: &str) -> Option<usize> {
    status
        .trim_end()
        .strip_prefix("+OK ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[tokio::test]
async fn uidl_is_stable_across_sessions_and_survives_deletion() {
    // RFC 1939 §7: a UIDL is the server's persistent handle for a message, and
    // clients use it to decide what they have already downloaded. If it were
    // derived from the session-local message number it would silently shift
    // whenever an earlier message was removed, and a leave-mail-on-server
    // client would re-download the tail of the mailbox.
    let server = TestServer::start().await;
    let (_acc, inbox) = server.create_account("alice").await;

    for raw in [PLAIN, LEADING_DOT, BARE_LF] {
        server.insert_message(inbox, raw).await;
    }

    let first: Vec<String> = {
        let mut client = server.connect().await;
        client.cmd("USER alice").await;
        client.cmd("PASS pass").await;
        let (_status, lines) = client.cmd_multiline("UIDL").await;
        client.cmd("QUIT").await;
        lines
    };
    assert_eq!(first.len(), 3);

    // Reconnecting must produce the same identifiers for the same messages.
    let second: Vec<String> = {
        let mut client = server.connect().await;
        client.cmd("USER alice").await;
        client.cmd("PASS pass").await;
        let (_status, lines) = client.cmd_multiline("UIDL").await;
        client.cmd("QUIT").await;
        lines
    };
    assert_eq!(
        first, second,
        "UIDL listings must be identical across sessions when nothing changed"
    );

    let uid_of = |lines: &[String], n: usize| -> String {
        lines
            .iter()
            .find(|l| l.starts_with(&format!("{n} ")))
            .and_then(|l| l.split_whitespace().nth(1))
            .expect("a UIDL entry")
            .to_string()
    };
    let second_uid = uid_of(&first, 2);
    let third_uid = uid_of(&first, 3);

    // Remove message 1 for real, then confirm the survivors kept their ids even
    // though their message *numbers* have shifted down.
    {
        let mut client = server.connect().await;
        client.cmd("USER alice").await;
        client.cmd("PASS pass").await;
        client.cmd("DELE 1").await;
        client.cmd("QUIT").await;
    }

    let mut client = server.connect().await;
    client.cmd("USER alice").await;
    client.cmd("PASS pass").await;
    let (_status, after) = client.cmd_multiline("UIDL").await;

    assert_eq!(after.len(), 2, "one message was expunged");
    assert_eq!(
        uid_of(&after, 1),
        second_uid,
        "the message now numbered 1 must keep the UIDL it had when numbered 2"
    );
    assert_eq!(
        uid_of(&after, 2),
        third_uid,
        "the message now numbered 2 must keep the UIDL it had when numbered 3"
    );
}
