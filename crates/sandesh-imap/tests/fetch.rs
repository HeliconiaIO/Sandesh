//! `FETCH` and `UID FETCH` over a real socket (RFC 9051 §6.4.5, §6.4.8).
//!
//! Every test here drives the production server over TCP, against messages
//! stored through the production `MessageStore`. Nothing is stubbed, so a test
//! cannot pass against a shortcut the real delivery path does not take.
//!
//! # What these tests assert
//!
//! Protocol *correctness*, not substring presence. A response is parsed into
//! its item/value pairs and literals are extracted by their declared length,
//! so a test fails if a literal's count disagrees with its payload — the
//! failure mode a `contains("BODY[]")` assertion would sail straight past.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{MessageFlags, MessageStore, SystemFlags, Timestamp};

/// A minimal well-formed message with known, hand-countable bytes.
const SIMPLE: &[u8] = b"From: alice@example.test\r\n\
Subject: Hello\r\n\
Date: Tue, 01 Sep 2026 12:00:00 +0000\r\n\
Message-ID: <simple-1@example.test>\r\n\
\r\n\
Line one.\r\n\
Line two.\r\n";

/// Logs in and selects INBOX, returning the client and the mailbox id.
async fn selected(
    server: &TestServer,
) -> (
    common::Client<tokio::net::TcpStream>,
    sandesh_core::MailboxId,
) {
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let mut client = server.connect().await;
    let login = client.command("LOGIN alice any").await;
    assert!(login.contains(" OK"), "login failed: {login}");
    (client, inbox)
}

/// Selects INBOX after messages have been delivered.
async fn select_inbox(client: &mut common::Client<tokio::net::TcpStream>) {
    let response = client.command("SELECT INBOX").await;
    assert!(response.contains(" OK"), "SELECT failed: {response}");
}

// ---------------------------------------------------------------------------
// Response parsing helpers
//
// These exist so the tests assert on structure rather than on substrings.
// ---------------------------------------------------------------------------

/// One untagged `* n FETCH (...)` response, with literals resolved.
#[derive(Debug, Clone)]
struct FetchReply {
    sequence: usize,
    /// The item list as raw octets, with literals still in `{n}CRLF…` form.
    body: Vec<u8>,
}

impl FetchReply {
    /// The value following `item`, as text, up to the next item boundary.
    ///
    /// Only for non-literal values: numbers, atoms, parenthesised lists.
    fn value(&self, item: &str) -> Option<String> {
        let text = String::from_utf8_lossy(&self.body).into_owned();
        let at = find_item(&text, item)?;
        let rest = text.get(at + item.len()..)?.trim_start();

        if rest.starts_with('(') {
            // A parenthesised value: take to the matching close paren.
            let mut depth = 0usize;
            for (index, ch) in rest.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return rest.get(..=index).map(str::to_owned);
                        }
                    }
                    _ => {}
                }
            }
            return None;
        }
        if rest.starts_with('"') {
            let end = rest.get(1..)?.find('"')? + 1;
            return rest.get(1..end).map(str::to_owned);
        }
        // A bare atom or number.
        let end = rest.find([' ', ')']).unwrap_or(rest.len());
        rest.get(..end).map(str::to_owned)
    }

    /// The literal payload following `item`, resolved by its declared length.
    ///
    /// This is the assertion that matters for body data: it reads the `{n}`
    /// count and returns exactly `n` octets, so a mis-sized literal produces a
    /// wrong-length payload rather than passing unnoticed.
    fn literal(&self, item: &str) -> Option<Vec<u8>> {
        let text = String::from_utf8_lossy(&self.body).into_owned();
        let at = find_item(&text, item)?;
        // Work in bytes from here: the payload may not be UTF-8.
        let after = at + item.len();
        let open = self.body.get(after..)?.iter().position(|b| *b == b'{')? + after;
        let close = self.body.get(open..)?.iter().position(|b| *b == b'}')? + open;
        let declared: usize = core::str::from_utf8(self.body.get(open + 1..close)?)
            .ok()?
            .parse()
            .ok()?;
        // `{n}` is followed by CRLF, then exactly n octets.
        let start = close + 3;
        let payload = self.body.get(start..start + declared)?;
        Some(payload.to_vec())
    }

    /// The declared length of the literal following `item`.
    fn literal_length(&self, item: &str) -> Option<usize> {
        let text = String::from_utf8_lossy(&self.body).into_owned();
        let at = find_item(&text, item)?;
        let after = at + item.len();
        let open = self.body.get(after..)?.iter().position(|b| *b == b'{')? + after;
        let close = self.body.get(open..)?.iter().position(|b| *b == b'}')? + open;
        core::str::from_utf8(self.body.get(open + 1..close)?)
            .ok()?
            .parse()
            .ok()
    }
}

/// Finds an item name at a token boundary, so `BODY[]` does not match `BODY[]<0>`
/// and `UID` does not match a `UID` appearing inside a literal payload.
fn find_item(text: &str, item: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(offset) = text.get(from..)?.find(item) {
        let at = from + offset;
        let before_ok = at == 0
            || text
                .get(..at)
                .and_then(|s| s.chars().next_back())
                .is_some_and(|c| c == ' ' || c == '(');
        let after = at + item.len();
        let after_ok = text
            .get(after..)
            .and_then(|s| s.chars().next())
            .is_none_or(|c| c == ' ' || c == '{' || c == '"' || c == '(');
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// Splits a raw response into its untagged `FETCH` replies.
///
/// Literals are skipped over by their declared length, so a literal payload
/// that happens to contain `* 1 FETCH (` is not mistaken for a new reply —
/// which is exactly what a line-oriented parser would get wrong.
fn fetch_replies(raw: &[u8]) -> Vec<FetchReply> {
    let mut replies = Vec::new();
    let mut index = 0usize;

    while index < raw.len() {
        // Find the start of the next untagged FETCH at a line boundary.
        let Some(start) = next_fetch(raw, index) else {
            break;
        };
        // Parse `* <n> FETCH `.
        let after_star = start + 2;
        let digits_end = raw
            .get(after_star..)
            .map(|rest| {
                rest.iter()
                    .position(|b| !b.is_ascii_digit())
                    .unwrap_or(rest.len())
            })
            .unwrap_or(0)
            + after_star;
        let sequence: usize = core::str::from_utf8(&raw[after_star..digits_end])
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(0);

        // Walk to the end of the reply, stepping over literal payloads.
        let mut cursor = digits_end;
        let mut end = raw.len();
        while cursor < raw.len() {
            match raw[cursor] {
                b'{' => {
                    // A literal: read its length and jump the payload.
                    let close = match raw[cursor..].iter().position(|b| *b == b'}') {
                        Some(offset) => cursor + offset,
                        None => break,
                    };
                    let declared: usize = core::str::from_utf8(&raw[cursor + 1..close])
                        .ok()
                        .and_then(|text| text.parse().ok())
                        .unwrap_or(0);
                    cursor = close + 3 + declared;
                }
                b'\n' => {
                    end = cursor + 1;
                    break;
                }
                _ => cursor += 1,
            }
        }

        replies.push(FetchReply {
            sequence,
            body: raw[start..end.min(raw.len())].to_vec(),
        });
        index = end.max(start + 1);
    }

    replies
}

/// The offset of the next `* <digits> FETCH ` at a line start.
fn next_fetch(raw: &[u8], from: usize) -> Option<usize> {
    let mut index = from;
    while index < raw.len() {
        let at_line_start = index == 0 || raw.get(index - 1) == Some(&b'\n');
        if at_line_start && raw.get(index) == Some(&b'*') && raw.get(index + 1) == Some(&b' ') {
            let rest = raw.get(index + 2..)?;
            let digits = rest
                .iter()
                .position(|b| !b.is_ascii_digit())
                .unwrap_or(rest.len());
            if digits > 0 && rest.get(digits..digits + 7) == Some(b" FETCH ") {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

/// Whether the response ends in a tagged `OK`.
fn is_ok(raw: &[u8]) -> bool {
    String::from_utf8_lossy(raw)
        .lines()
        .any(|line| line.contains(" OK ") && !line.starts_with('*'))
}

/// Whether the response ends in a tagged `BAD`.
fn is_bad(raw: &[u8]) -> bool {
    String::from_utf8_lossy(raw)
        .lines()
        .any(|line| line.contains(" BAD ") && !line.starts_with('*'))
}

// ---------------------------------------------------------------------------
// Metadata items
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_returns_the_stored_uid_and_flags() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    let entry = server
        .deliver_with_flags(
            inbox,
            SIMPLE,
            MessageFlags::from_system(SystemFlags::FLAGGED),
        )
        .await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 (UID FLAGS)").await;
    assert!(is_ok(&raw), "{}", String::from_utf8_lossy(&raw));

    let replies = fetch_replies(&raw);
    assert_eq!(replies.len(), 1, "one message, one reply");
    assert_eq!(replies[0].sequence, 1);
    assert_eq!(
        replies[0].value("UID"),
        Some(entry.uid.get().to_string()),
        "the UID must be the placement's own"
    );
    assert_eq!(
        replies[0].value("FLAGS"),
        Some(String::from("(\\Flagged)")),
        "the stored flags, not inferred ones"
    );
}

#[tokio::test]
async fn a_uid_is_never_zero_and_is_stable_across_fetches() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let first = client.command_bytes("FETCH 1 UID").await;
    let second = client.command_bytes("FETCH 1 UID").await;

    let a = fetch_replies(&first)[0].value("UID").expect("a UID");
    let b = fetch_replies(&second)[0].value("UID").expect("a UID");
    assert_eq!(a, b, "UIDs must be stable across repeated FETCH");
    assert_ne!(a, "0", "UID 0 is not representable");
}

#[tokio::test]
async fn rfc822_size_is_the_exact_stored_octet_count() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 RFC822.SIZE").await;
    let size: usize = fetch_replies(&raw)[0]
        .value("RFC822.SIZE")
        .expect("a size")
        .parse()
        .expect("a number");

    assert_eq!(
        size,
        SIMPLE.len(),
        "the raw stored bytes are authoritative, not a decoded length"
    );
}

#[tokio::test]
async fn rfc822_size_counts_octets_not_characters() {
    // The mutation this catches: measuring UTF-8 as `chars().count()`.
    let utf8 = "From: a@b\r\nSubject: héllo — wörld\r\n\r\nBody with émojis 🎉\r\n"
        .as_bytes()
        .to_vec();
    assert!(utf8.len() > utf8.iter().filter(|b| b.is_ascii()).count());

    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, &utf8).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 RFC822.SIZE").await;
    let size: usize = fetch_replies(&raw)[0]
        .value("RFC822.SIZE")
        .expect("a size")
        .parse()
        .expect("a number");
    assert_eq!(size, utf8.len(), "octets, not characters");
}

#[tokio::test]
async fn rfc822_size_is_exact_for_every_fixture() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;

    let fixtures = [
        "valid/plain-text.eml",
        "valid/html.eml",
        "valid/multipart-alternative.eml",
        "valid/multipart-mixed.eml",
        "valid/attachment.eml",
        "valid/nested-multipart.eml",
        "encodings/unicode.eml",
        "encodings/base64-body.eml",
        "malformed/duplicate-headers.eml",
    ];

    let mut expected = Vec::new();
    for name in fixtures {
        let bytes = TestServer::fixture(name);
        expected.push(bytes.len());
        server.deliver(inbox, &bytes).await;
    }
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1:* RFC822.SIZE").await;
    let replies = fetch_replies(&raw);
    assert_eq!(replies.len(), fixtures.len());

    for (index, reply) in replies.iter().enumerate() {
        let size: usize = reply
            .value("RFC822.SIZE")
            .expect("a size")
            .parse()
            .expect("a number");
        assert_eq!(
            size, expected[index],
            "{} must report its exact stored size",
            fixtures[index]
        );
    }
}

#[tokio::test]
async fn internaldate_is_the_stored_value_not_the_current_time() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;

    // 2020-06-15T08:30:00Z — unmistakably not "now".
    let stored = Timestamp::from_micros(1_592_209_800_000_000);
    server
        .deliver_at(inbox, SIMPLE, MessageFlags::empty(), stored)
        .await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 INTERNALDATE").await;
    let date = fetch_replies(&raw)[0]
        .value("INTERNALDATE")
        .expect("a date");

    assert!(date.contains("2020"), "the stored year: {date}");
    assert!(date.contains("Jun"), "the stored month: {date}");
    assert!(date.ends_with("+0000"), "{date}");
}

// ---------------------------------------------------------------------------
// Raw message and headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn body_returns_the_raw_message_byte_for_byte() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY[]").await;
    let payload = fetch_replies(&raw)[0]
        .literal("BODY[]")
        .expect("a literal payload");

    assert_eq!(payload, SIMPLE, "the message must be byte-exact");
}

#[tokio::test]
async fn a_literal_length_always_matches_its_payload() {
    // Phase brief §15: the declared count is the actual octet count.
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    for name in [
        "valid/plain-text.eml",
        "valid/attachment.eml",
        "encodings/unicode.eml",
        "malformed/bare-lf.eml",
    ] {
        server.deliver(inbox, &TestServer::fixture(name)).await;
    }
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1:* BODY.PEEK[]").await;
    for reply in fetch_replies(&raw) {
        let declared = reply
            .literal_length("BODY[]")
            .expect("a declared literal length");
        let payload = reply.literal("BODY[]").expect("a payload");
        assert_eq!(
            declared,
            payload.len(),
            "message {} declared {declared} octets",
            reply.sequence
        );
    }
}

#[tokio::test]
async fn crlf_and_bare_lf_are_preserved_exactly() {
    // Phase brief §9: no line-ending normalisation, in either direction.
    let bare_lf = TestServer::fixture("malformed/bare-lf.eml");
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, &bare_lf).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY.PEEK[]").await;
    let payload = fetch_replies(&raw)[0].literal("BODY[]").expect("a payload");

    assert_eq!(payload, bare_lf, "bare LF must not become CRLF");
}

#[tokio::test]
async fn binary_attachment_content_survives_unchanged() {
    // A message carrying every byte value, so any transformation shows up.
    let mut message: Vec<u8> = b"From: a@b\r\n\
Subject: Binary\r\n\
MIME-Version: 1.0\r\n\
Content-Type: application/octet-stream\r\n\
Content-Transfer-Encoding: binary\r\n\
\r\n"
        .to_vec();
    let payload: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    message.extend_from_slice(&payload);

    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, &message).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY.PEEK[]").await;
    let fetched = fetch_replies(&raw)[0].literal("BODY[]").expect("a payload");

    assert_eq!(fetched.len(), message.len());
    assert_eq!(fetched, message, "every octet must survive");
}

#[tokio::test]
async fn header_and_text_sections_reconstruct_the_message() {
    // The property clients depend on: BODY[HEADER] + BODY[TEXT] == BODY[].
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client
        .command_bytes("FETCH 1 (BODY.PEEK[HEADER] BODY.PEEK[TEXT])")
        .await;
    let reply = &fetch_replies(&raw)[0];

    let header = reply.literal("BODY[HEADER]").expect("a header section");
    let text = reply.literal("BODY[TEXT]").expect("a text section");

    let mut rebuilt = header.clone();
    rebuilt.extend_from_slice(&text);
    assert_eq!(rebuilt, SIMPLE, "the two sections must tile the message");
    assert!(
        header.ends_with(b"\r\n\r\n"),
        "a header section includes its blank line"
    );
}

#[tokio::test]
async fn duplicate_headers_are_preserved_in_order() {
    // Phase brief §10: no reserialisation, so both copies survive.
    let fixture = TestServer::fixture("malformed/duplicate-headers.eml");
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, &fixture).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY.PEEK[HEADER]").await;
    let header = fetch_replies(&raw)[0]
        .literal("BODY[HEADER]")
        .expect("a header section");

    // Whatever field the fixture duplicates, the count must match the source.
    let source_headers = &fixture[..header.len().min(fixture.len())];
    for field in ["Subject:", "From:", "Received:", "To:"] {
        let in_source = count_occurrences(source_headers, field.as_bytes());
        let in_fetch = count_occurrences(&header, field.as_bytes());
        if in_source > 1 {
            assert_eq!(
                in_fetch, in_source,
                "{field} appears {in_source} times in the source"
            );
        }
    }
}

/// How many times `needle` occurs in `haystack`.
fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[tokio::test]
async fn header_fields_returns_only_the_named_fields_verbatim() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client
        .command_bytes("FETCH 1 BODY.PEEK[HEADER.FIELDS (SUBJECT)]")
        .await;
    let section = fetch_replies(&raw)[0]
        .literal("BODY[HEADER.FIELDS (SUBJECT)]")
        .expect("a field selection");

    let text = String::from_utf8_lossy(&section).into_owned();
    assert!(text.contains("Subject: Hello"), "{text:?}");
    assert!(
        !text.contains("From:"),
        "unnamed fields must be excluded: {text:?}"
    );
    assert!(section.ends_with(b"\r\n"), "a header section is terminated");
}

#[tokio::test]
async fn header_fields_not_excludes_only_the_named_fields() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client
        .command_bytes("FETCH 1 BODY.PEEK[HEADER.FIELDS.NOT (SUBJECT)]")
        .await;
    let section = fetch_replies(&raw)[0]
        .literal("BODY[HEADER.FIELDS.NOT (SUBJECT)]")
        .expect("a field exclusion");

    let text = String::from_utf8_lossy(&section).into_owned();
    assert!(!text.contains("Subject:"), "{text:?}");
    assert!(text.contains("From: alice@example.test"), "{text:?}");
}

#[tokio::test]
async fn the_rfc822_aliases_return_the_same_octets_as_their_sections() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let aliases = client
        .command_bytes("FETCH 1 (RFC822.HEADER RFC822.TEXT)")
        .await;
    let alias_reply = &fetch_replies(&aliases)[0];
    let alias_header = alias_reply.literal("RFC822.HEADER").expect("a header");
    let alias_text = alias_reply.literal("RFC822.TEXT").expect("a text");

    let sections = client
        .command_bytes("FETCH 1 (BODY.PEEK[HEADER] BODY.PEEK[TEXT])")
        .await;
    let section_reply = &fetch_replies(&sections)[0];

    assert_eq!(
        alias_header,
        section_reply.literal("BODY[HEADER]").expect("a header")
    );
    assert_eq!(
        alias_text,
        section_reply.literal("BODY[TEXT]").expect("a text")
    );

    // And RFC822 itself is the whole message.
    let whole = client.command_bytes("FETCH 1 RFC822").await;
    assert_eq!(
        fetch_replies(&whole)[0].literal("RFC822").expect("a body"),
        SIMPLE
    );
}

// ---------------------------------------------------------------------------
// MIME and BODYSTRUCTURE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bodystructure_describes_a_simple_text_message() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server
        .deliver(inbox, &TestServer::fixture("valid/plain-text.eml"))
        .await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODYSTRUCTURE").await;
    let structure = fetch_replies(&raw)[0]
        .value("BODYSTRUCTURE")
        .expect("a structure");

    assert!(structure.starts_with("(\"TEXT\" \"PLAIN\""), "{structure}");
    assert!(structure.contains("CHARSET"), "{structure}");
    // A text part carries a line count as its last non-extension field.
    assert!(
        structure.matches(char::is_numeric).count() >= 2,
        "size and line count: {structure}"
    );
}

#[tokio::test]
async fn bodystructure_describes_multipart_alternative() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server
        .deliver(
            inbox,
            &TestServer::fixture("valid/multipart-alternative.eml"),
        )
        .await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODYSTRUCTURE").await;
    let structure = fetch_replies(&raw)[0]
        .value("BODYSTRUCTURE")
        .expect("a structure");

    // A multipart names its subtype *after* the children.
    assert!(structure.contains("\"ALTERNATIVE\""), "{structure}");
    assert!(structure.contains("\"TEXT\" \"PLAIN\""), "{structure}");
    assert!(structure.contains("\"TEXT\" \"HTML\""), "{structure}");
    assert!(
        structure.starts_with("(("),
        "children come first: {structure}"
    );
}

#[tokio::test]
async fn bodystructure_describes_an_attachment() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server
        .deliver(inbox, &TestServer::fixture("valid/attachment.eml"))
        .await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODYSTRUCTURE").await;
    let structure = fetch_replies(&raw)[0]
        .value("BODYSTRUCTURE")
        .expect("a structure");

    assert!(structure.contains("\"MIXED\""), "{structure}");
    // The attachment's disposition and filename appear in extension data.
    assert!(
        structure.to_ascii_uppercase().contains("ATTACHMENT")
            || structure.to_ascii_uppercase().contains("FILENAME"),
        "{structure}"
    );
}

#[tokio::test]
async fn bodystructure_describes_nested_multipart() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server
        .deliver(inbox, &TestServer::fixture("valid/nested-multipart.eml"))
        .await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODYSTRUCTURE").await;
    let structure = fetch_replies(&raw)[0]
        .value("BODYSTRUCTURE")
        .expect("a structure");

    // Nesting shows as a multipart whose child is itself a multipart.
    let depth = structure
        .chars()
        .scan(0i32, |depth, ch| {
            match ch {
                '(' => *depth += 1,
                ')' => *depth -= 1,
                _ => {}
            }
            Some(*depth)
        })
        .max()
        .unwrap_or(0);
    assert!(depth >= 3, "nested structure should nest: {structure}");
}

#[tokio::test]
async fn body_and_bodystructure_differ_only_in_extension_data() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server
        .deliver(inbox, &TestServer::fixture("valid/attachment.eml"))
        .await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 (BODY BODYSTRUCTURE)").await;
    let reply = &fetch_replies(&raw)[0];
    let body = reply.value("BODY").expect("a body");
    let structure = reply.value("BODYSTRUCTURE").expect("a structure");

    assert!(
        structure.len() > body.len(),
        "BODYSTRUCTURE carries extension data BODY does not:\n{body}\n{structure}"
    );
    // Both must describe the same media types.
    assert!(body.contains("\"MIXED\"") && structure.contains("\"MIXED\""));
}

/// Every fixture in the corpus, not a hand-picked handful.
///
/// The phase's acceptance criterion is that `BODYSTRUCTURE` is "correct for
/// every fixture in the corpus". The tests above assert *specific* shapes for
/// six well-formed fixtures; this asserts the invariants that must hold for
/// **all** of them, including the eleven — mostly malformed — that no IMAP test
/// touched before.
///
/// Malformed input is the point. A truncated base64 body, an unterminated
/// multipart boundary or a message with no headers at all must still produce a
/// parseable `BODYSTRUCTURE`: RFC 9051 §7.5.2 gives the server no way to say
/// "this message has no structure", and a client parses the response before it
/// ever looks at the content. A panic here would take down the connection, and
/// unbalanced parentheses would desynchronise the client's parser for every
/// response that follows — a fault that outlives the bad message.
///
/// The corpus is enumerated from disk rather than listed, so a fixture added
/// later is covered without anyone remembering to add it here.
#[tokio::test]
async fn bodystructure_is_well_formed_for_every_fixture_in_the_corpus() {
    let corpus = fixture_corpus();
    assert!(
        corpus.len() >= 20,
        "expected the full corpus, found {}: {corpus:?}",
        corpus.len()
    );

    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;

    // One mailbox, one message per fixture, so a single SELECT sees them all
    // in a known order.
    for relative in &corpus {
        server.deliver(inbox, &TestServer::fixture(relative)).await;
    }
    select_inbox(&mut client).await;

    for (index, relative) in corpus.iter().enumerate() {
        let sequence = index + 1;
        let raw = client
            .command_bytes(&format!("FETCH {sequence} BODYSTRUCTURE"))
            .await;

        // The server answered at all: no panic, no dropped connection. For a
        // fixture that is zero bytes long this is the whole assertion worth
        // making, and it is not a trivial one.
        let replies = fetch_replies(&raw);
        assert_eq!(
            replies.len(),
            1,
            "{relative}: expected one FETCH reply, got {}: {}",
            replies.len(),
            String::from_utf8_lossy(&raw)
        );

        // Check the *whole* untagged response first. `value()` walks parens to
        // find the item's end, so it returns `None` for a malformed structure
        // exactly as it does for a missing one — and in the single-item form
        // the enclosing `FETCH (...)` parens can mask an omission inside. The
        // raw line is the only place an imbalance is unambiguous, and it is
        // also what a real client's parser actually consumes.
        // The *whole* reply, not its first line. A `BODYSTRUCTURE` carrying a
        // value IMAP cannot quote — a non-ASCII filename, say — sends it as a
        // literal, whose payload follows a CRLF; splitting on lines would
        // discard everything after `{n}`, including the closing parens, and
        // report a correct response as unbalanced. `fetch_replies` already
        // steps over literal payloads to find the true end of the reply.
        let reply = String::from_utf8_lossy(&replies[0].body).into_owned();
        assert_balanced_parens(&reply, relative);

        let structure = replies[0].value("BODYSTRUCTURE").unwrap_or_else(|| {
            panic!(
                "{relative}: BODYSTRUCTURE missing or malformed in {}",
                String::from_utf8_lossy(&raw)
            )
        });

        assert_balanced_parens(&structure, relative);

        assert!(
            structure.starts_with('(') && structure.ends_with(')'),
            "{relative}: a body must be a parenthesised list: {structure}"
        );

        // Every body is either a multipart (children first, so it opens with a
        // nested list) or a single part naming its media type first. Both forms
        // must carry a quoted string in the first few tokens; a bare `NIL` body
        // or an empty list would be neither.
        assert!(
            structure.starts_with("((") || structure.contains('"'),
            "{relative}: neither multipart nor a typed single part: {structure}"
        );

        // The tagged OK must follow, which is what proves the response stream
        // is still in sync after this message.
        assert!(
            raw.windows(3).any(|w| w == b"OK "),
            "{relative}: no tagged OK after the fetch: {}",
            String::from_utf8_lossy(&raw)
        );
    }

    // The connection survived all 22, in order, on one session.
    let response = client.command("NOOP").await;
    assert!(
        response.contains(" OK"),
        "the session must still be usable after the whole corpus: {response}"
    );
}

/// `BODY` agrees with `BODYSTRUCTURE` across the whole corpus.
///
/// RFC 9051 §7.5.2 defines `BODYSTRUCTURE` as `BODY` plus extension data, so
/// the two can never disagree about a message's shape. Checking it per-fixture
/// catches an extension field leaking into the non-extension form — which a
/// rev1 client would reject as a parse error, and which no single-fixture test
/// would notice.
#[tokio::test]
async fn body_and_bodystructure_agree_across_the_corpus() {
    let corpus = fixture_corpus();
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    for relative in &corpus {
        server.deliver(inbox, &TestServer::fixture(relative)).await;
    }
    select_inbox(&mut client).await;

    for (index, relative) in corpus.iter().enumerate() {
        let raw = client
            .command_bytes(&format!("FETCH {} (BODY BODYSTRUCTURE)", index + 1))
            .await;
        let reply = &fetch_replies(&raw)[0];
        let body = reply
            .value("BODY")
            .unwrap_or_else(|| panic!("{relative}: no BODY"));
        let structure = reply
            .value("BODYSTRUCTURE")
            .unwrap_or_else(|| panic!("{relative}: no BODYSTRUCTURE"));

        assert_balanced_parens(&body, relative);

        // BODYSTRUCTURE is BODY plus extension data, so it can never be the
        // shorter of the two.
        assert!(
            structure.len() >= body.len(),
            "{relative}: BODYSTRUCTURE is shorter than BODY:\n  {body}\n  {structure}"
        );

        // Both describe the same top-level shape.
        assert_eq!(
            body.starts_with("(("),
            structure.starts_with("(("),
            "{relative}: BODY and BODYSTRUCTURE disagree on multipart-ness:\n  {body}\n  {structure}"
        );
    }
}

/// Every `.eml` under `testdata/`, as paths relative to it, sorted.
///
/// Sorted so the sequence numbers a test assigns are stable across runs and
/// across filesystems that enumerate in different orders.
fn fixture_corpus() -> Vec<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("testdata");
    let mut found = Vec::new();

    let mut directories = vec![root.clone()];
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                directories.push(path);
            } else if path.extension().is_some_and(|e| e == "eml") {
                let relative = path
                    .strip_prefix(&root)
                    .expect("a path under testdata")
                    .to_string_lossy()
                    .into_owned();
                found.push(relative);
            }
        }
    }

    found.sort();
    found
}

/// Asserts that `structure` has balanced, properly nested parentheses.
///
/// Two kinds of byte in a `BODYSTRUCTURE` are data rather than structure, and
/// both must be skipped or the count is meaningless:
///
/// * **Quoted strings.** A filename of `report(final).pdf` is legal, so the
///   scan tracks quoting and backslash escapes.
/// * **Literals.** `{n}CRLF` followed by exactly `n` octets is how IMAP sends a
///   value it cannot quote — a non-ASCII filename, for instance — and those
///   octets are arbitrary. A `(` inside one is a byte of a filename, not a
///   list. This is what `valid/attachment-filename-utf8.eml` exercises: its
///   RFC 2047 filename is emitted as a literal, and counting inside it made a
///   correct response look unbalanced.
fn assert_balanced_parens(structure: &str, relative: &str) {
    let mut depth = 0_i32;
    let mut in_quotes = false;
    let mut escaped = false;

    // Byte-oriented, because a literal's length prefix counts octets, not
    // characters: a multi-byte filename would otherwise skip the wrong amount.
    let bytes = structure.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        let character = bytes[index];

        if escaped {
            escaped = false;
            index += 1;
            continue;
        }

        // A literal opens with `{n}` at the end of a line. Skip the prefix, the
        // CRLF, and exactly `n` octets of payload.
        if character == b'{'
            && !in_quotes
            && let Some(close) = bytes[index..].iter().position(|b| *b == b'}')
            // `{n+}` is a non-synchronising literal; the trailing `+` is not
            // part of the count.
            && let Ok(length) = structure[index + 1..index + close]
                .trim_end_matches('+')
                .parse::<usize>()
        {
            let after_brace = index + close + 1;
            let payload_start = if structure[after_brace..].starts_with("\r\n") {
                after_brace + 2
            } else if structure[after_brace..].starts_with('\n') {
                after_brace + 1
            } else {
                after_brace
            };
            index = (payload_start + length).min(bytes.len());
            continue;
        }

        match character {
            b'\\' if in_quotes => escaped = true,
            b'"' => in_quotes = !in_quotes,
            b'(' if !in_quotes => depth += 1,
            b')' if !in_quotes => {
                depth -= 1;
                assert!(
                    depth >= 0,
                    "{relative}: unbalanced ')' — a client's parser would \
                     desynchronise here: {structure}"
                );
            }
            _ => {}
        }
        index += 1;
    }

    assert!(
        !in_quotes,
        "{relative}: unterminated quoted string: {structure}"
    );
    assert_eq!(
        depth, 0,
        "{relative}: {depth} unclosed parenthesis/es: {structure}"
    );
}

#[tokio::test]
async fn a_part_section_returns_that_part_exactly() {
    let fixture = TestServer::fixture("valid/multipart-alternative.eml");
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, &fixture).await;
    select_inbox(&mut client).await;

    let raw = client
        .command_bytes("FETCH 1 (BODY.PEEK[1] BODY.PEEK[2])")
        .await;
    let reply = &fetch_replies(&raw)[0];
    let first = reply.literal("BODY[1]").expect("part 1");
    let second = reply.literal("BODY[2]").expect("part 2");

    assert!(!first.is_empty(), "part 1 must have content");
    assert!(!second.is_empty(), "part 2 must have content");
    assert_ne!(first, second, "the two parts differ");

    // Each part's bytes must appear verbatim in the original message, and at a
    // boundary that is exactly where the part begins. Asserting only that the
    // bytes appear *somewhere* lets an off-by-one slice pass, because a
    // fragment of the part is still a substring of the message — so the check
    // is anchored: the octet before the match must be the end of the preceding
    // header block, and the part must be maximal.
    for (label, part) in [("1", &first), ("2", &second)] {
        let at = find_subslice(&fixture, part)
            .unwrap_or_else(|| panic!("part {label} is not a slice of the message"));

        // Maximality on the left: the byte before the part must not be one the
        // part itself could have started with, i.e. extending the slice left by
        // one must no longer match at that position.
        if at > 0 {
            let extended = &fixture[at - 1..at - 1 + part.len()];
            assert_ne!(
                extended,
                part.as_slice(),
                "part {label} slice is shifted: the same bytes match one octet earlier"
            );
        }

        // A part body begins immediately after its own header blank line.
        let preceding = &fixture[..at];
        assert!(
            preceding.ends_with(b"\r\n\r\n") || preceding.ends_with(b"\n\n"),
            "part {label} must start right after its MIME header block, \
             not one octet in; preceding bytes: {:?}",
            String::from_utf8_lossy(&preceding[preceding.len().saturating_sub(8)..])
        );
    }
}

/// The offset of `needle` within `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[tokio::test]
async fn a_part_mime_section_returns_that_parts_own_headers() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server
        .deliver(
            inbox,
            &TestServer::fixture("valid/multipart-alternative.eml"),
        )
        .await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY.PEEK[1.MIME]").await;
    let mime = fetch_replies(&raw)[0]
        .literal("BODY[1.MIME]")
        .expect("a MIME section");

    let text = String::from_utf8_lossy(&mime).into_owned();
    assert!(
        text.to_ascii_lowercase().contains("content-type"),
        "a part's MIME headers describe the part: {text:?}"
    );
}

#[tokio::test]
async fn a_nonexistent_part_returns_an_empty_literal_not_an_error() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY.PEEK[99]").await;
    assert!(is_ok(&raw), "{}", String::from_utf8_lossy(&raw));
    assert_eq!(
        fetch_replies(&raw)[0].literal_length("BODY[99]"),
        Some(0),
        "an absent part is {{0}}, not an error"
    );
}

// ---------------------------------------------------------------------------
// Partial fetch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_partial_fetch_returns_the_requested_window() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    // Offset 0.
    let raw = client.command_bytes("FETCH 1 BODY.PEEK[]<0.10>").await;
    let window = fetch_replies(&raw)[0]
        .literal("BODY[]<0>")
        .expect("a window");
    assert_eq!(window, &SIMPLE[..10], "the first ten octets");

    // A middle offset.
    let raw = client.command_bytes("FETCH 1 BODY.PEEK[]<20.15>").await;
    let window = fetch_replies(&raw)[0]
        .literal("BODY[]<20>")
        .expect("a window");
    assert_eq!(window, &SIMPLE[20..35], "octets 20..35");
}

#[tokio::test]
async fn a_partial_fetch_at_the_end_returns_only_what_remains() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let offset = SIMPLE.len() - 5;
    let raw = client
        .command_bytes(&format!("FETCH 1 BODY.PEEK[]<{offset}.1000>"))
        .await;
    let window = fetch_replies(&raw)[0]
        .literal(&format!("BODY[]<{offset}>"))
        .expect("a window");

    assert_eq!(window, &SIMPLE[offset..], "a count past the end is clamped");
    assert_eq!(window.len(), 5);
}

#[tokio::test]
async fn a_partial_fetch_beyond_the_end_returns_an_empty_literal() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    for offset in [SIMPLE.len(), SIMPLE.len() + 1, 1_000_000, u32::MAX as usize] {
        let raw = client
            .command_bytes(&format!("FETCH 1 BODY.PEEK[]<{offset}.10>"))
            .await;
        assert!(is_ok(&raw), "offset {offset} must succeed");
        assert_eq!(
            fetch_replies(&raw)[0].literal_length(&format!("BODY[]<{offset}>")),
            Some(0),
            "offset {offset} is past the end, so {{0}}"
        );
    }
}

#[tokio::test]
async fn a_partial_fetch_of_binary_data_is_byte_exact() {
    let mut message: Vec<u8> = b"From: a@b\r\nSubject: bin\r\n\r\n".to_vec();
    let payload: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
    message.extend_from_slice(&payload);

    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, &message).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY.PEEK[]<100.256>").await;
    let window = fetch_replies(&raw)[0]
        .literal("BODY[]<100>")
        .expect("a window");

    assert_eq!(window.len(), 256);
    assert_eq!(window, &message[100..356], "binary octets must be exact");
}

#[tokio::test]
async fn a_partial_response_reports_the_offset_without_the_count() {
    // RFC 9051 §7.4.2: the literal's own length states how much came back.
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY.PEEK[]<5.7>").await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    assert!(text.contains("BODY[]<5>"), "{text}");
    assert!(
        !text.contains("BODY[]<5.7>"),
        "the count is not echoed: {text}"
    );
}

// ---------------------------------------------------------------------------
// BODY vs BODY.PEEK and \Seen
// ---------------------------------------------------------------------------

#[tokio::test]
async fn body_peek_does_not_set_seen() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    assert!(!entry.flags.has(SystemFlags::SEEN), "delivered unseen");
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY.PEEK[]").await;
    assert!(is_ok(&raw));

    // Read the stored flags back through storage, not through the response.
    let stored = server
        .messages
        .get_entry(entry.id)
        .await
        .expect("query entry")
        .expect("the entry exists");
    assert!(
        !stored.flags.has(SystemFlags::SEEN),
        "BODY.PEEK must not set \\Seen"
    );
}

#[tokio::test]
async fn a_plain_body_fetch_sets_seen() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 BODY[]").await;
    assert!(is_ok(&raw));

    let stored = server
        .messages
        .get_entry(entry.id)
        .await
        .expect("query entry")
        .expect("the entry exists");
    assert!(
        stored.flags.has(SystemFlags::SEEN),
        "RFC 9051 §6.4.5: a non-peek body fetch sets \\Seen"
    );
}

#[tokio::test]
async fn a_metadata_fetch_never_sets_seen() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    for items in [
        "FLAGS",
        "UID",
        "RFC822.SIZE",
        "BODYSTRUCTURE",
        "ENVELOPE",
        "FAST",
    ] {
        let raw = client.command_bytes(&format!("FETCH 1 {items}")).await;
        assert!(is_ok(&raw), "{items}");
        let stored = server
            .messages
            .get_entry(entry.id)
            .await
            .expect("query entry")
            .expect("the entry exists");
        assert!(
            !stored.flags.has(SystemFlags::SEEN),
            "{items} must not set \\Seen"
        );
    }
}

#[tokio::test]
async fn rfc822_header_peeks_but_rfc822_does_not() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    let first = server.deliver(inbox, SIMPLE).await;
    let second = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    client.command_bytes("FETCH 1 RFC822.HEADER").await;
    let after_header = server
        .messages
        .get_entry(first.id)
        .await
        .expect("query")
        .expect("exists");
    assert!(
        !after_header.flags.has(SystemFlags::SEEN),
        "RFC822.HEADER is defined as BODY.PEEK[HEADER]"
    );

    client.command_bytes("FETCH 2 RFC822").await;
    let after_body = server
        .messages
        .get_entry(second.id)
        .await
        .expect("query")
        .expect("exists");
    assert!(
        after_body.flags.has(SystemFlags::SEEN),
        "RFC822 sets \\Seen"
    );
}

#[tokio::test]
async fn an_examined_mailbox_is_never_marked_seen() {
    // EXAMINE is read-only: RFC 9051 §6.3.3 forbids any state change.
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;

    let response = client.command("EXAMINE INBOX").await;
    assert!(response.contains(" OK"), "{response}");

    let raw = client.command_bytes("FETCH 1 BODY[]").await;
    assert!(is_ok(&raw), "the fetch itself still succeeds");

    let stored = server
        .messages
        .get_entry(entry.id)
        .await
        .expect("query")
        .expect("exists");
    assert!(
        !stored.flags.has(SystemFlags::SEEN),
        "an EXAMINE'd mailbox permits no flag change"
    );
}

#[tokio::test]
async fn the_flags_reported_match_what_the_fetch_stored() {
    // A client asking for both must not be told "unseen" by the very response
    // that made the message seen.
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 (FLAGS BODY[])").await;
    let flags = fetch_replies(&raw)[0].value("FLAGS").expect("flags");
    assert!(
        flags.contains("\\Seen"),
        "the response must reflect the flag it just set: {flags}"
    );
}

// ---------------------------------------------------------------------------
// Sequence-number semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sequence_numbers_run_from_one_and_are_contiguous_despite_uid_gaps() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;

    let a = server.deliver(inbox, SIMPLE).await;
    let b = server.deliver(inbox, SIMPLE).await;
    let c = server.deliver(inbox, SIMPLE).await;
    // Remove the middle placement, creating a UID gap.
    server
        .messages
        .delete_entry(b.id)
        .await
        .expect("delete the middle entry");

    select_inbox(&mut client).await;
    let raw = client.command_bytes("FETCH 1:* UID").await;
    let replies = fetch_replies(&raw);

    assert_eq!(replies.len(), 2, "two messages remain");
    assert_eq!(replies[0].sequence, 1, "numbering starts at 1");
    assert_eq!(replies[1].sequence, 2, "and is contiguous over the gap");
    assert_eq!(replies[0].value("UID"), Some(a.uid.get().to_string()));
    assert_eq!(replies[1].value("UID"), Some(c.uid.get().to_string()));
    assert!(
        c.uid.get() > a.uid.get() + 1,
        "the UIDs really are non-contiguous"
    );
}

#[tokio::test]
async fn a_sequence_number_is_a_position_not_a_uid() {
    // The single most consequential mutation: resolving `2` as UID 2.
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;

    let first = server.deliver(inbox, SIMPLE).await;
    let second = server.deliver(inbox, SIMPLE).await;
    // Make UIDs and positions clearly disagree.
    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("delete");

    select_inbox(&mut client).await;
    let raw = client.command_bytes("FETCH 1 UID").await;
    let replies = fetch_replies(&raw);

    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0].value("UID"),
        Some(second.uid.get().to_string()),
        "sequence 1 is now the message with the higher UID"
    );
    assert_ne!(second.uid.get(), 1, "UID 1 no longer exists");
}

#[tokio::test]
async fn a_star_range_selects_every_message() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    for _ in 0..5 {
        server.deliver(inbox, SIMPLE).await;
    }
    select_inbox(&mut client).await;

    for set in ["1:*", "*:1", "1:5", "1,2,3,4,5"] {
        let raw = client.command_bytes(&format!("FETCH {set} UID")).await;
        let replies = fetch_replies(&raw);
        assert_eq!(replies.len(), 5, "{set} must select all five");
        let sequences: Vec<usize> = replies.iter().map(|r| r.sequence).collect();
        assert_eq!(sequences, vec![1, 2, 3, 4, 5], "{set}");
    }
}

#[tokio::test]
async fn star_alone_is_the_last_message() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    for _ in 0..3 {
        server.deliver(inbox, SIMPLE).await;
    }
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH * UID").await;
    let replies = fetch_replies(&raw);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].sequence, 3, "* is the last message");
}

#[tokio::test]
async fn duplicates_and_overlapping_ranges_return_each_message_once() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    for _ in 0..3 {
        server.deliver(inbox, SIMPLE).await;
    }
    select_inbox(&mut client).await;

    for set in ["1,1,1", "1:3,2:3", "3,2,1", "1:2,2:3"] {
        let raw = client.command_bytes(&format!("FETCH {set} UID")).await;
        let replies = fetch_replies(&raw);
        let mut sequences: Vec<usize> = replies.iter().map(|r| r.sequence).collect();
        let before = sequences.len();
        sequences.dedup();
        assert_eq!(before, sequences.len(), "{set} returned a duplicate");
        assert!(
            sequences.windows(2).all(|w| w[0] < w[1]),
            "{set} must be ascending: {sequences:?}"
        );
    }
}

#[tokio::test]
async fn an_out_of_range_sequence_set_succeeds_with_no_data() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 99 UID").await;
    assert!(is_ok(&raw), "RFC 9051 §6.4.8: not an error");
    assert!(fetch_replies(&raw).is_empty(), "and no data");
}

#[tokio::test]
async fn fetching_an_empty_mailbox_succeeds_with_no_data() {
    let server = TestServer::start().await;
    let (mut client, _inbox) = selected(&server).await;
    select_inbox(&mut client).await;

    for set in ["1:*", "*", "1"] {
        let raw = client.command_bytes(&format!("FETCH {set} UID")).await;
        assert!(is_ok(&raw), "{set} on an empty mailbox must succeed");
        assert!(fetch_replies(&raw).is_empty(), "{set}");
    }
}

// ---------------------------------------------------------------------------
// UID FETCH
// ---------------------------------------------------------------------------

#[tokio::test]
async fn uid_fetch_resolves_by_uid_not_by_position() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;

    let first = server.deliver(inbox, SIMPLE).await;
    let second = server.deliver(inbox, SIMPLE).await;
    let third = server.deliver(inbox, SIMPLE).await;
    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("delete");

    select_inbox(&mut client).await;

    // UID FETCH of the third message's UID must return it, at sequence 2.
    let raw = client
        .command_bytes(&format!("UID FETCH {} UID", third.uid.get()))
        .await;
    let replies = fetch_replies(&raw);
    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0].value("UID"),
        Some(third.uid.get().to_string()),
        "the requested UID"
    );
    assert_eq!(
        replies[0].sequence, 2,
        "reported by sequence number, which is now 2"
    );
    let _ = second;
}

#[tokio::test]
async fn uid_fetch_always_reports_the_uid_even_when_unasked() {
    // RFC 9051 §6.4.8 requires it.
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client
        .command_bytes(&format!("UID FETCH {} FLAGS", entry.uid.get()))
        .await;
    let replies = fetch_replies(&raw);
    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0].value("UID"),
        Some(entry.uid.get().to_string()),
        "UID FETCH must include the UID unasked"
    );
}

#[tokio::test]
async fn a_uid_fetch_star_is_the_largest_uid_in_use() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    let last = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("UID FETCH * UID").await;
    let replies = fetch_replies(&raw);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].value("UID"), Some(last.uid.get().to_string()));
}

#[tokio::test]
async fn a_uid_range_spans_gaps_without_error() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    let a = server.deliver(inbox, SIMPLE).await;
    let b = server.deliver(inbox, SIMPLE).await;
    let c = server.deliver(inbox, SIMPLE).await;
    server.messages.delete_entry(b.id).await.expect("delete");
    select_inbox(&mut client).await;

    let raw = client
        .command_bytes(&format!("UID FETCH {}:{} UID", a.uid.get(), c.uid.get()))
        .await;
    let uids: Vec<String> = fetch_replies(&raw)
        .iter()
        .filter_map(|reply| reply.value("UID"))
        .collect();
    assert_eq!(
        uids,
        vec![a.uid.get().to_string(), c.uid.get().to_string()],
        "the gap is simply absent"
    );
}

#[tokio::test]
async fn a_uid_that_does_not_exist_returns_no_data_not_an_error() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("UID FETCH 99999 UID").await;
    assert!(is_ok(&raw), "{}", String::from_utf8_lossy(&raw));
    assert!(fetch_replies(&raw).is_empty());
}

#[tokio::test]
async fn uid_fetch_cannot_reach_a_message_in_another_mailbox() {
    // The security property in phase brief §18 and §22.
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let other = server.mailbox(account, "Archive").await;

    let hidden = server.deliver(other, SIMPLE).await;
    let visible = server.deliver(inbox, SIMPLE).await;

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;
    select_inbox(&mut client).await;

    // Ask for every UID that could exist.
    let raw = client.command_bytes("UID FETCH 1:* UID").await;
    let uids: Vec<String> = fetch_replies(&raw)
        .iter()
        .filter_map(|reply| reply.value("UID"))
        .collect();

    assert_eq!(
        uids,
        vec![visible.uid.get().to_string()],
        "only the selected mailbox's placement is reachable"
    );

    // And naming the other mailbox's UID directly returns nothing.
    let direct = client
        .command_bytes(&format!("UID FETCH {} BODY.PEEK[]", hidden.uid.get()))
        .await;
    let replies = fetch_replies(&direct);
    if let Some(reply) = replies.first() {
        // If a UID collision put a *different* message at that number, it must
        // be the INBOX one, never the Archive one.
        assert_eq!(
            reply.value("UID"),
            Some(visible.uid.get().to_string()),
            "an Archive placement must never surface in an INBOX selection"
        );
    }
}

#[tokio::test]
async fn the_same_message_in_two_mailboxes_has_two_independent_uids() {
    // Phase brief §18: test the same message placed in multiple mailboxes.
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let archive = server.mailbox(account, "Archive").await;

    let in_inbox = server.deliver(inbox, SIMPLE).await;
    let copied = server
        .messages
        .copy_entry(in_inbox.id, archive)
        .await
        .expect("copy the placement");

    assert_eq!(
        in_inbox.message_id, copied.message_id,
        "the message is shared, not duplicated"
    );

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;

    // The INBOX selection sees the INBOX UID.
    select_inbox(&mut client).await;
    let raw = client.command_bytes("FETCH 1 (UID BODY.PEEK[])").await;
    let inbox_reply = &fetch_replies(&raw)[0];
    assert_eq!(
        inbox_reply.value("UID"),
        Some(in_inbox.uid.get().to_string())
    );
    let inbox_body = inbox_reply.literal("BODY[]").expect("a body");

    // The Archive selection sees the Archive UID — same content.
    let response = client.command("SELECT Archive").await;
    assert!(response.contains(" OK"), "{response}");
    let raw = client.command_bytes("FETCH 1 (UID BODY.PEEK[])").await;
    let archive_reply = &fetch_replies(&raw)[0];
    assert_eq!(
        archive_reply.value("UID"),
        Some(copied.uid.get().to_string())
    );

    assert_eq!(
        archive_reply.literal("BODY[]").expect("a body"),
        inbox_body,
        "the same message, byte-identical, under a different UID"
    );
}

// ---------------------------------------------------------------------------
// ENVELOPE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn envelope_reports_the_message_headers() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 ENVELOPE").await;
    let envelope = fetch_replies(&raw)[0]
        .value("ENVELOPE")
        .expect("an envelope");

    assert!(envelope.contains("Hello"), "the subject: {envelope}");
    assert!(envelope.contains("alice"), "the from address: {envelope}");
    assert!(
        envelope.contains("example.test"),
        "the host, split from the mailbox: {envelope}"
    );
}

#[tokio::test]
async fn envelope_uses_nil_for_absent_headers() {
    let minimal = b"Subject: only a subject\r\n\r\nbody\r\n";
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, minimal).await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 ENVELOPE").await;
    let envelope = fetch_replies(&raw)[0]
        .value("ENVELOPE")
        .expect("an envelope");
    assert!(
        envelope.contains("NIL"),
        "absent fields are NIL: {envelope}"
    );
}

#[tokio::test]
async fn envelope_handles_utf8_headers() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server
        .deliver(inbox, &TestServer::fixture("encodings/encoded-headers.eml"))
        .await;
    select_inbox(&mut client).await;

    let raw = client.command_bytes("FETCH 1 ENVELOPE").await;
    assert!(is_ok(&raw), "an encoded-word header must not break FETCH");
    assert_eq!(fetch_replies(&raw).len(), 1);
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_before_authentication_is_refused() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let raw = client.command_bytes("FETCH 1 UID").await;
    assert!(is_bad(&raw), "{}", String::from_utf8_lossy(&raw));
    assert!(fetch_replies(&raw).is_empty(), "no data may leak");
}

#[tokio::test]
async fn uid_fetch_before_authentication_is_refused() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let raw = client.command_bytes("UID FETCH 1 UID").await;
    assert!(is_bad(&raw), "{}", String::from_utf8_lossy(&raw));
    assert!(fetch_replies(&raw).is_empty());
}

#[tokio::test]
async fn fetch_before_select_is_refused() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    // Authenticated but with no selection.

    let raw = client.command_bytes("FETCH 1 UID").await;
    assert!(is_bad(&raw), "{}", String::from_utf8_lossy(&raw));
    assert!(fetch_replies(&raw).is_empty(), "no data before SELECT");
}

#[tokio::test]
async fn fetch_after_close_is_refused() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;
    client.command("CLOSE").await;

    let raw = client.command_bytes("FETCH 1 UID").await;
    assert!(is_bad(&raw), "{}", String::from_utf8_lossy(&raw));
}

#[tokio::test]
async fn an_invalid_sequence_set_is_rejected_without_partial_execution() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    for _ in 0..3 {
        server.deliver(inbox, SIMPLE).await;
    }
    select_inbox(&mut client).await;

    for set in ["0", "0:5", "abc", "1:", ",1", "1,,2", "-1", "1:2:3", "*:0"] {
        let raw = client.command_bytes(&format!("FETCH {set} UID")).await;
        assert!(
            is_bad(&raw),
            "{set} must be BAD: {}",
            String::from_utf8_lossy(&raw)
        );
        assert!(
            fetch_replies(&raw).is_empty(),
            "{set} must return no data at all"
        );
    }
}

#[tokio::test]
async fn an_invalid_data_item_is_rejected_without_partial_execution() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    for items in [
        "(NOTANITEM)",
        "(UID NOTANITEM)",
        "BODY[NOPE]",
        "BODY[0]",
        "BODY[MIME]",
        "BODY[]<>",
        "BODY[]<1>",
        "BODY[]<a.b>",
        "(FAST UID)",
        "()",
    ] {
        let raw = client.command_bytes(&format!("FETCH 1 {items}")).await;
        assert!(
            is_bad(&raw),
            "{items} must be BAD: {}",
            String::from_utf8_lossy(&raw)
        );
        assert!(
            fetch_replies(&raw).is_empty(),
            "{items} must return no data"
        );
    }
}

#[tokio::test]
async fn a_malformed_fetch_never_panics_and_the_session_survives() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    for hostile in [
        "FETCH",
        "FETCH 1",
        "FETCH (((((",
        "FETCH 1 BODY[[[[",
        "FETCH 1 BODY[]]]]",
        "FETCH * *",
        "FETCH 4294967295 UID",
        "FETCH 1 BODY[99999999999999999999]",
        "FETCH 1 BODY[]<4294967295.4294967295>",
        "FETCH 1,,,,,, UID",
        &format!("FETCH {} UID", "1,".repeat(2000)),
    ] {
        let raw = client.command_bytes(hostile).await;
        assert!(!raw.is_empty(), "{hostile} produced no response");
        // Either rejected or answered — never a panic, never a dropped session.
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert!(
            text.contains(" BAD") || text.contains(" OK") || text.contains(" NO"),
            "{hostile} => {text}"
        );
    }

    // The session is still usable afterwards.
    let raw = client.command_bytes("FETCH 1 UID").await;
    assert!(is_ok(&raw), "the session must survive every rejection");
}

// ---------------------------------------------------------------------------
// Snapshot semantics (ADR-046)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_new_message_is_invisible_until_the_mailbox_is_reselected() {
    // ADR-046: a selection is a snapshot. This pins the *actual* behaviour so
    // it is a documented decision rather than an accident.
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    // A second session delivers a new message after the selection was taken.
    server.deliver(inbox, SIMPLE).await;

    let raw = client.command_bytes("FETCH 1:* UID").await;
    assert_eq!(
        fetch_replies(&raw).len(),
        1,
        "the snapshot is fixed at SELECT: the arrival is not yet visible"
    );

    // Reselecting takes a fresh snapshot, and both are visible.
    select_inbox(&mut client).await;
    let raw = client.command_bytes("FETCH 1:* UID").await;
    assert_eq!(
        fetch_replies(&raw).len(),
        2,
        "reselecting refreshes the snapshot"
    );
}

#[tokio::test]
async fn a_concurrently_deleted_entry_is_reported_as_absent_not_as_an_error() {
    // Session A holds a snapshot; session B deletes a placement. A's FETCH of
    // that sequence number must degrade cleanly.
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    let first = server.deliver(inbox, SIMPLE).await;
    let second = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("concurrent delete");

    // Metadata comes from the snapshot and still answers.
    let raw = client.command_bytes("FETCH 1:2 UID").await;
    assert!(is_ok(&raw), "{}", String::from_utf8_lossy(&raw));

    // The surviving message is still fully fetchable.
    let raw = client
        .command_bytes(&format!("UID FETCH {} BODY.PEEK[]", second.uid.get()))
        .await;
    assert!(is_ok(&raw));
    assert_eq!(
        fetch_replies(&raw)[0].literal("BODY[]").expect("a body"),
        SIMPLE
    );
}

#[tokio::test]
async fn two_sessions_hold_independent_snapshots() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server.deliver(inbox, SIMPLE).await;

    let mut a = server.connect().await;
    a.command("LOGIN alice any").await;
    select_inbox(&mut a).await;

    server.deliver(inbox, SIMPLE).await;

    let mut b = server.connect().await;
    b.command("LOGIN alice any").await;
    select_inbox(&mut b).await;

    // B selected later, so it sees both; A's snapshot predates the arrival.
    let a_raw = a.command_bytes("FETCH 1:* UID").await;
    let b_raw = b.command_bytes("FETCH 1:* UID").await;
    assert_eq!(fetch_replies(&a_raw).len(), 1, "A's snapshot is older");
    assert_eq!(fetch_replies(&b_raw).len(), 2, "B sees the arrival");
}

// ---------------------------------------------------------------------------
// Multiple messages and items in one command
// ---------------------------------------------------------------------------

#[tokio::test]
async fn many_items_and_many_messages_in_one_command() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    for name in [
        "valid/plain-text.eml",
        "valid/html.eml",
        "valid/multipart-mixed.eml",
    ] {
        server.deliver(inbox, &TestServer::fixture(name)).await;
    }
    select_inbox(&mut client).await;

    let raw = client
        .command_bytes(
            "FETCH 1:* (UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE BODY.PEEK[HEADER])",
        )
        .await;
    assert!(is_ok(&raw), "{}", String::from_utf8_lossy(&raw));

    let replies = fetch_replies(&raw);
    assert_eq!(replies.len(), 3);
    for reply in &replies {
        assert!(reply.value("UID").is_some(), "UID missing");
        assert!(reply.value("FLAGS").is_some(), "FLAGS missing");
        assert!(reply.value("RFC822.SIZE").is_some(), "size missing");
        assert!(reply.value("INTERNALDATE").is_some(), "date missing");
        assert!(
            reply.literal("BODY[HEADER]").is_some(),
            "header section missing"
        );
    }
}

#[tokio::test]
async fn multiple_literals_in_one_response_are_each_correctly_framed() {
    let server = TestServer::start().await;
    let (mut client, inbox) = selected(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let raw = client
        .command_bytes("FETCH 1 (BODY.PEEK[HEADER] BODY.PEEK[TEXT] BODY.PEEK[])")
        .await;
    let reply = &fetch_replies(&raw)[0];

    let header = reply.literal("BODY[HEADER]").expect("header");
    let text = reply.literal("BODY[TEXT]").expect("text");
    let whole = reply.literal("BODY[]").expect("whole");

    assert_eq!(header.len() + text.len(), whole.len());
    assert_eq!(whole, SIMPLE);
}
