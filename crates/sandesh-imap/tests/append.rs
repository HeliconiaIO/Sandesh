//! `APPEND` over a real socket (RFC 9051 §6.3.12, `APPENDUID` per RFC 4315 §3).
//!
//! Every test drives the production server over TCP and reads results back
//! **through storage**, not out of the response.
//!
//! The recurring theme is that `APPEND` is the one command where the client
//! supplies the message bytes, so the guarantee under test throughout is that
//! what was submitted is what was stored — byte for byte, with the flags and
//! `INTERNALDATE` the client asked for, in exactly one new placement.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{
    AccountId, EventKind, EventPayload, MailboxId, MailboxStore, MessageStore, SystemFlags,
};

/// A minimal well-formed message.
const SIMPLE: &[u8] = b"From: alice@example.test\r\n\
Subject: Append test\r\n\
\r\n\
Body.\r\n";

/// Logs in, returning the client, the account and INBOX id.
async fn logged_in(
    server: &TestServer,
) -> (common::Client<tokio::net::TcpStream>, AccountId, MailboxId) {
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let mut client = server.connect().await;
    let login = client.command("LOGIN alice any").await;
    assert!(login.contains(" OK"), "login failed: {login}");
    (client, account, inbox)
}

/// Whether a response carries a tagged `OK`.
fn is_ok(response: &str) -> bool {
    response
        .lines()
        .any(|line| line.contains(" OK ") && !line.starts_with('*'))
}

/// Whether a response carries a tagged `BAD` or `NO`.
fn is_refused(response: &str) -> bool {
    response
        .lines()
        .any(|line| (line.contains(" BAD ") || line.contains(" NO ")) && !line.starts_with('*'))
}

/// The `APPENDUID` code's two fields, if the response carries one.
fn appenduid(response: &str) -> Option<(u32, u32)> {
    let start = response.find("[APPENDUID ")? + "[APPENDUID ".len();
    let end = start + response[start..].find(']')?;
    let mut fields = response[start..end].split_whitespace();
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

/// The placements a mailbox holds, ordered by UID.
async fn entries(server: &TestServer, mailbox: MailboxId) -> Vec<sandesh_core::MessageEntry> {
    server
        .messages
        .list_entries(mailbox)
        .await
        .expect("list the placements")
}

/// Sends a complete `APPEND` with a synchronising literal and reads the result.
///
/// The literal length is measured in octets, not characters, so a message with
/// non-ASCII bytes declares the length the server will actually read.
async fn append(
    client: &mut common::Client<tokio::net::TcpStream>,
    arguments: &str,
    message: &[u8],
) -> String {
    let tag = client.next_tag();
    client
        .send_line(&format!("{tag} APPEND {arguments}{{{}}}", message.len()))
        .await;
    // The server must ask before the octets are sent (RFC 9051 §4.3).
    let continuation = client.read_response().await;
    assert!(
        continuation.starts_with('+'),
        "expected a continuation request, got: {continuation}"
    );
    client.send_raw(message).await;
    client.send_raw(b"\r\n").await;
    client.read_until_tagged(&tag).await
}

// --- the basic append ------------------------------------------------------

#[tokio::test]
async fn append_creates_exactly_one_placement() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    let response = append(&mut client, "INBOX ", SIMPLE).await;

    assert!(is_ok(&response), "APPEND failed: {response}");
    let stored = entries(&server, inbox).await;
    assert_eq!(stored.len(), 1, "exactly one new placement");
}

#[tokio::test]
async fn the_stored_bytes_are_exactly_what_was_submitted() {
    // The product guarantee. A server that re-encoded, re-wrapped or
    // normalised line endings would fail here and nowhere else.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    append(&mut client, "INBOX ", SIMPLE).await;

    let stored = entries(&server, inbox).await;
    let raw = server
        .messages
        .raw(stored[0].message_id)
        .await
        .expect("query the raw bytes")
        .expect("the message exists");
    assert_eq!(
        raw.bytes(),
        SIMPLE,
        "the stored octets must equal the submitted octets"
    );
}

#[tokio::test]
async fn crlf_inside_the_message_is_preserved() {
    // A message whose body contains blank lines and CRLF runs must survive
    // unchanged: rewriting line endings would change every hash and signature.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    let raw = b"From: a@b\r\nSubject: crlf\r\n\r\nline1\r\n\r\nline3\r\n";

    append(&mut client, "INBOX ", raw).await;

    let stored = entries(&server, inbox).await;
    let read_back = server
        .messages
        .raw(stored[0].message_id)
        .await
        .expect("query")
        .expect("exists");
    assert_eq!(read_back.bytes(), raw);
}

#[tokio::test]
async fn binary_octets_survive_the_round_trip() {
    // RFC 9051 §4.3 makes a literal arbitrary octets. Bytes that are not valid
    // UTF-8 must reach storage intact rather than being replaced.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    let mut raw = b"From: a@b\r\nSubject: binary\r\n\r\n".to_vec();
    raw.extend_from_slice(&[0xFF, 0x00, 0xC3, 0x28, 0xFE]);
    raw.extend_from_slice(b"\r\n");

    let response = append(&mut client, "INBOX ", &raw).await;

    assert!(is_ok(&response), "APPEND failed: {response}");
    let stored = entries(&server, inbox).await;
    let read_back = server
        .messages
        .raw(stored[0].message_id)
        .await
        .expect("query")
        .expect("exists");
    assert_eq!(
        read_back.bytes(),
        raw.as_slice(),
        "binary octets must not be re-encoded"
    );
}

#[tokio::test]
async fn an_appended_message_is_fetchable() {
    // End to end: what was appended reads back through the normal FETCH path.
    let server = TestServer::start().await;
    let (mut client, _account, _inbox) = logged_in(&server).await;

    append(&mut client, "INBOX ", SIMPLE).await;
    client.command("SELECT INBOX").await;
    let response = client.command("FETCH 1 (BODY[])").await;

    assert!(
        response.contains("Subject: Append test"),
        "the appended message reads back: {response}"
    );
}

#[tokio::test]
async fn an_appended_message_is_searchable() {
    let server = TestServer::start().await;
    let (mut client, _account, _inbox) = logged_in(&server).await;

    append(&mut client, "INBOX ", SIMPLE).await;
    client.command("SELECT INBOX").await;
    let response = client.command("SEARCH SUBJECT \"Append test\"").await;

    assert!(
        response.contains("* SEARCH 1"),
        "the appended message is searchable: {response}"
    );
}

// --- UID allocation --------------------------------------------------------

#[tokio::test]
async fn appenduid_reports_the_uid_storage_assigned() {
    // The UID must be the one storage allocated, never a value computed from
    // UIDNEXT: a guess would name a different message under concurrency.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;

    let response = append(&mut client, "INBOX ", SIMPLE).await;

    let (validity, uid) = appenduid(&response).expect("APPENDUID reported");
    let stored = entries(&server, inbox).await;
    assert_eq!(
        uid,
        stored[0].uid.get(),
        "APPENDUID names the UID actually assigned"
    );

    let mailbox = server
        .mailbox_by_name(account, "INBOX")
        .await
        .expect("INBOX exists");
    assert_eq!(
        validity,
        mailbox.uid_validity.get(),
        "APPENDUID names the destination's UIDVALIDITY"
    );
}

#[tokio::test]
async fn appenduid_is_the_uid_storage_assigned() {
    // What this *can* prove: the reported UID is the one the placement actually
    // holds, in a mailbox with a UID gap, where a naive "highest + 1" would be
    // wrong.
    //
    // What no single-threaded test can prove: that the value was *read from the
    // returned placement* rather than from `Mailbox::uid_next`. The handler
    // reads the mailbox immediately before the insert and nothing runs in
    // between, so the two are always equal here — substituting one for the
    // other is a mutation this suite does not kill, and saying so is more
    // useful than a test that appears to and does not. The guarantee rests on
    // the code reading `entry.uid` and on `docs/DECISIONS.md` (ADR-053)
    // recording why; under real concurrency the values diverge and the guess
    // would be wrong.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;

    // A UID gap: deliver two, remove the first.
    let first = server.deliver(inbox, b"From: a@b\r\n\r\none\r\n").await;
    server.deliver(inbox, b"From: a@b\r\n\r\ntwo\r\n").await;
    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("delete to create a gap");

    let response = append(&mut client, "INBOX ", SIMPLE).await;

    let (validity, reported) = appenduid(&response).expect("APPENDUID reported");
    let stored = entries(&server, inbox).await;
    let assigned = stored
        .iter()
        .map(|entry| entry.uid.get())
        .max()
        .expect("the appended placement");

    assert_eq!(
        reported, assigned,
        "APPENDUID must name the UID the placement holds: {response}"
    );
    let mailbox = server
        .mailbox_by_name(account, "INBOX")
        .await
        .expect("INBOX exists");
    assert_eq!(
        validity,
        mailbox.uid_validity.get(),
        "and the destination's UIDVALIDITY"
    );
    assert_eq!(
        mailbox.uid_next.get(),
        assigned + 1,
        "UIDNEXT advances past the assignment"
    );
}

#[tokio::test]
async fn successive_appends_get_distinct_ascending_uids() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    let first = append(&mut client, "INBOX ", b"From: a@b\r\n\r\none\r\n").await;
    let second = append(&mut client, "INBOX ", b"From: a@b\r\n\r\ntwo\r\n").await;
    let third = append(&mut client, "INBOX ", b"From: a@b\r\n\r\nthree\r\n").await;

    let uids: Vec<u32> = [&first, &second, &third]
        .iter()
        .map(|r| appenduid(r).expect("APPENDUID reported").1)
        .collect();
    assert!(
        uids[0] < uids[1] && uids[1] < uids[2],
        "UIDs must ascend: {uids:?}"
    );
    assert_eq!(entries(&server, inbox).await.len(), 3);
}

#[tokio::test]
async fn multiple_appends_store_distinct_messages() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    append(
        &mut client,
        "INBOX ",
        b"From: a@b\r\nSubject: one\r\n\r\n1\r\n",
    )
    .await;
    append(
        &mut client,
        "INBOX ",
        b"From: a@b\r\nSubject: two\r\n\r\n2\r\n",
    )
    .await;

    let stored = entries(&server, inbox).await;
    assert_eq!(stored.len(), 2);
    assert_ne!(
        stored[0].message_id, stored[1].message_id,
        "two different messages, not one shared placement"
    );
}

// --- flags and internal date ----------------------------------------------

#[tokio::test]
async fn appended_flags_are_stored() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    let response = append(&mut client, r"INBOX (\Seen \Flagged) ", SIMPLE).await;

    assert!(is_ok(&response), "APPEND failed: {response}");
    let stored = entries(&server, inbox).await;
    assert!(stored[0].flags.has(SystemFlags::SEEN), "\\Seen stored");
    assert!(
        stored[0].flags.has(SystemFlags::FLAGGED),
        "\\Flagged stored"
    );
}

#[tokio::test]
async fn an_append_without_flags_starts_unflagged() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    append(&mut client, "INBOX ", SIMPLE).await;

    let stored = entries(&server, inbox).await;
    assert!(
        !stored[0].flags.has(SystemFlags::SEEN),
        "no flags were asked for, so none are set"
    );
}

#[tokio::test]
async fn an_explicit_internal_date_is_stored() {
    // 1-Jan-2020 00:00:00 +0000 is 1577836800 seconds after the epoch.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    let response = append(&mut client, "INBOX \"1-Jan-2020 00:00:00 +0000\" ", SIMPLE).await;

    assert!(is_ok(&response), "APPEND failed: {response}");
    let stored = entries(&server, inbox).await;
    assert_eq!(
        stored[0].internal_date.as_micros(),
        1_577_836_800_000_000,
        "the client's INTERNALDATE is stored, not the time of receipt"
    );
}

#[tokio::test]
async fn a_zoned_internal_date_is_converted_to_utc() {
    // 05:30 at +0530 is midnight UTC. A server that dropped the zone would
    // store the message five and a half hours late.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    append(&mut client, "INBOX \"1-Jan-2020 05:30:00 +0530\" ", SIMPLE).await;

    let stored = entries(&server, inbox).await;
    assert_eq!(stored[0].internal_date.as_micros(), 1_577_836_800_000_000);
}

#[tokio::test]
async fn flags_and_internal_date_may_both_be_given() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    let response = append(
        &mut client,
        r#"INBOX (\Seen) "1-Jan-2020 00:00:00 +0000" "#,
        SIMPLE,
    )
    .await;

    assert!(is_ok(&response), "APPEND failed: {response}");
    let stored = entries(&server, inbox).await;
    assert!(stored[0].flags.has(SystemFlags::SEEN));
    assert_eq!(stored[0].internal_date.as_micros(), 1_577_836_800_000_000);
}

#[tokio::test]
async fn the_internal_date_is_visible_through_fetch() {
    let server = TestServer::start().await;
    let (mut client, _account, _inbox) = logged_in(&server).await;

    append(&mut client, "INBOX \"1-Jan-2020 00:00:00 +0000\" ", SIMPLE).await;
    client.command("SELECT INBOX").await;
    let response = client.command("FETCH 1 (INTERNALDATE)").await;

    assert!(
        response.contains("1-Jan-2020 00:00:00 +0000"),
        "FETCH reports the appended date: {response}"
    );
}

// --- refusals --------------------------------------------------------------

#[tokio::test]
async fn append_to_a_nonexistent_mailbox_is_refused_with_trycreate() {
    // RFC 9051 §6.3.12: the server must not create the mailbox itself.
    let server = TestServer::start().await;
    let (mut client, account, _inbox) = logged_in(&server).await;

    let response = append(&mut client, "NoSuchMailbox ", SIMPLE).await;

    assert!(is_refused(&response), "must be refused: {response}");
    assert!(
        response.contains("TRYCREATE"),
        "RFC 9051 §6.3.12 wants TRYCREATE: {response}"
    );
    assert!(
        server
            .mailbox_by_name(account, "NoSuchMailbox")
            .await
            .is_none(),
        "APPEND must not create the mailbox"
    );
}

#[tokio::test]
async fn append_to_a_noselect_mailbox_is_refused() {
    let server = TestServer::start().await;
    let (mut client, account, _inbox) = logged_in(&server).await;
    let holder = server
        .mailbox_with_attributes(
            account,
            "Holder",
            sandesh_core::MailboxAttributes::from_names([r"\Noselect"]),
        )
        .await;

    let response = append(&mut client, "Holder ", SIMPLE).await;

    assert!(is_refused(&response), "must be refused: {response}");
    assert!(
        entries(&server, holder).await.is_empty(),
        "nothing was stored"
    );
}

#[tokio::test]
async fn append_without_authentication_is_refused() {
    // The security boundary: an unauthenticated client cannot inject a message.
    //
    // The continuation request is *not* a leak and not an acceptance. A literal
    // is part of the command's syntax (RFC 9051 §4.3), so the octets must be
    // consumed before the command exists to be judged — a server that refused
    // at the `{n}` and then stopped reading would leave the message body in the
    // stream to be misparsed as commands. What matters is that the command is
    // refused before it reaches storage, and that the bounded-read limit
    // applies to the literal exactly as it does for an authenticated client.
    let server = TestServer::start().await;
    let mut client = server.connect().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;

    let response = append(&mut client, "INBOX ", SIMPLE).await;

    assert!(
        is_refused(&response),
        "an unauthenticated APPEND must be refused: {response}"
    );
    assert!(
        entries(&server, inbox).await.is_empty(),
        "an unauthenticated APPEND must store nothing"
    );
    assert!(
        !response.contains("APPENDUID"),
        "a refused APPEND reports no UID: {response}"
    );
}

#[tokio::test]
async fn an_empty_message_is_refused() {
    // `{0}` is legal IMAP but is not a message: it has no headers at all.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    let tag = client.next_tag();
    client.send_line(&format!("{tag} APPEND INBOX {{0}}")).await;
    let continuation = client.read_response().await;
    assert!(continuation.starts_with('+'), "got: {continuation}");
    client.send_raw(b"\r\n").await;
    let response = client.read_until_tagged(&tag).await;

    assert!(is_refused(&response), "must be refused: {response}");
    assert!(
        entries(&server, inbox).await.is_empty(),
        "nothing was stored"
    );
}

#[tokio::test]
async fn an_invalid_flag_is_refused_and_stores_nothing() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    let response = append(&mut client, r"INBOX (\Nonsense) ", SIMPLE).await;

    assert!(is_refused(&response), "must be refused: {response}");
    assert!(
        entries(&server, inbox).await.is_empty(),
        "a refused APPEND stores nothing"
    );
}

#[tokio::test]
async fn a_malformed_date_is_refused_and_stores_nothing() {
    // Not silently replaced with "now": the client would never know.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    let response = append(&mut client, "INBOX \"not-a-date\" ", SIMPLE).await;

    assert!(is_refused(&response), "must be refused: {response}");
    assert!(
        entries(&server, inbox).await.is_empty(),
        "a refused APPEND stores nothing"
    );
}

#[tokio::test]
async fn an_oversized_literal_is_refused_before_it_is_read() {
    // ADR-040: the declared length is checked against `max_literal_size` before
    // any octet is read, so APPEND is no route around the message-size limit
    // and no route to unbounded memory growth.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    let oversized = common::TestServer::base_config().max_literal_size + 1;

    let tag = client.next_tag();
    client
        .send_line(&format!("{tag} APPEND INBOX {{{oversized}}}"))
        .await;
    let response = client.read_response().await;

    assert!(
        !response.starts_with('+'),
        "an oversized literal must not be accepted: {response}"
    );
    assert!(
        entries(&server, inbox).await.is_empty(),
        "nothing was stored"
    );
}

// --- events ----------------------------------------------------------------

#[tokio::test]
async fn a_successful_append_emits_one_entry_created_event() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    let before = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryCreated)
        .count();

    append(&mut client, "INBOX ", SIMPLE).await;

    let after = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryCreated)
        .count();
    assert_eq!(after, before + 1, "one append, one EntryCreated event");
    assert_eq!(entries(&server, inbox).await.len(), 1);
}

#[tokio::test]
async fn the_event_names_the_mailbox_and_uid_that_were_created() {
    // The event must carry enough for a selected session to build an
    // unsolicited update, and must name the mailbox that actually changed.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    append(&mut client, "INBOX ", SIMPLE).await;

    let stored = entries(&server, inbox).await;
    let matched = server.events.events().iter().any(|event| {
        matches!(
            &event.payload,
            EventPayload::EntryCreated { mailbox_id, uid, .. }
                if *mailbox_id == inbox && *uid == stored[0].uid
        ) || matches!(
            &event.payload,
            EventPayload::MessageReceived { mailbox_id, uid, .. }
                if *mailbox_id == inbox && *uid == stored[0].uid
        )
    });
    assert!(
        matched,
        "an event must name the mailbox and the assigned UID: {:?}",
        server.events.kinds()
    );
}

#[tokio::test]
async fn a_failed_append_emits_no_event() {
    // No phantom events: a refused APPEND changed nothing, so it must announce
    // nothing. A subscriber that acted on such an event would show a message
    // that does not exist.
    let server = TestServer::start().await;
    let (mut client, _account, _inbox) = logged_in(&server).await;
    let before = server.events.kinds().len();

    let response = append(&mut client, "NoSuchMailbox ", SIMPLE).await;
    assert!(is_refused(&response), "must be refused: {response}");

    assert_eq!(
        server.events.kinds().len(),
        before,
        "a refused APPEND emits nothing"
    );
}

#[tokio::test]
async fn a_destination_deleted_mid_command_is_refused_and_stores_nothing() {
    // The destination is resolved *after* the literal is read — it has to be,
    // because the literal is part of the command's syntax and must be consumed
    // before the command exists to run. So a mailbox that disappears while the
    // octets are in flight is caught by the lookup, not by the insert, and the
    // client gets `NO [TRYCREATE]`.
    //
    // Asserted explicitly because it is the one ordering in this handler that
    // is easy to get backwards: resolving the destination first would leave the
    // unread literal in the stream to be misparsed as commands.
    let server = TestServer::start().await;
    let (mut client, account, _inbox) = logged_in(&server).await;
    let doomed = server.mailbox(account, "Doomed").await;
    let before = server.events.kinds().len();

    // Send the command up to the literal, so the server is waiting for octets.
    let tag = client.next_tag();
    client
        .send_line(&format!("{tag} APPEND Doomed {{{}}}", SIMPLE.len()))
        .await;
    let continuation = client.read_response().await;
    assert!(continuation.starts_with('+'), "got: {continuation}");

    // Remove the mailbox out from under the append.
    server
        .mailboxes
        .delete(doomed)
        .await
        .expect("delete the destination");

    client.send_raw(SIMPLE).await;
    client.send_raw(b"\r\n").await;
    let response = client.read_until_tagged(&tag).await;

    assert!(
        is_refused(&response),
        "a vanished destination must be reported as failure: {response}"
    );
    assert!(
        !response.contains("APPENDUID"),
        "a failed APPEND reports no UID: {response}"
    );
    assert!(
        entries(&server, doomed).await.is_empty(),
        "nothing was stored"
    );
    // And no message-level event: the only events are the mailbox's own removal.
    assert!(
        !server
            .events
            .kinds()
            .iter()
            .skip(before)
            .any(|kind| *kind == EventKind::EntryCreated),
        "a failed APPEND emits no EntryCreated: {:?}",
        server.events.kinds()
    );
}

#[tokio::test]
async fn a_refused_flag_emits_no_event() {
    let server = TestServer::start().await;
    let (mut client, _account, _inbox) = logged_in(&server).await;
    let before = server.events.kinds().len();

    append(&mut client, r"INBOX (\Nonsense) ", SIMPLE).await;

    assert_eq!(
        server.events.kinds().len(),
        before,
        "a refused APPEND emits nothing"
    );
}

// --- mailbox isolation -----------------------------------------------------

#[tokio::test]
async fn appending_to_one_mailbox_does_not_touch_another() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;

    append(&mut client, "Archive ", SIMPLE).await;

    assert_eq!(
        entries(&server, archive).await.len(),
        1,
        "Archive received it"
    );
    assert!(
        entries(&server, inbox).await.is_empty(),
        "INBOX is untouched"
    );
}

#[tokio::test]
async fn append_works_while_a_different_mailbox_is_selected() {
    // APPEND is an Authenticated-state command: it does not require, and is not
    // constrained by, the current selection.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;

    client.command("SELECT INBOX").await;
    let response = append(&mut client, "Archive ", SIMPLE).await;

    assert!(is_ok(&response), "APPEND failed: {response}");
    assert_eq!(entries(&server, archive).await.len(), 1);
    assert!(entries(&server, inbox).await.is_empty());
}

#[tokio::test]
async fn append_into_the_selected_mailbox_is_permitted() {
    // Unlike COPY and MOVE, APPEND into the selected mailbox creates a genuinely
    // new message, so the placement invariant is not in play and it succeeds.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;

    client.command("SELECT INBOX").await;
    let response = append(&mut client, "INBOX ", SIMPLE).await;

    assert!(is_ok(&response), "APPEND failed: {response}");
    assert_eq!(entries(&server, inbox).await.len(), 1);
}
