//! `MOVE` and `UID MOVE` over a real socket (RFC 6851).
//!
//! Every test drives the production server over TCP against messages stored
//! through the production `MessageStore`. Assertions about what was moved read
//! back **through storage**, not out of the response: a server that reports a
//! plausible `[COPYUID]` without moving anything would pass a response-only
//! test and fail these.
//!
//! `MOVE` is the only command that must get three things right at once, and
//! they are easy to get individually right and jointly wrong:
//!
//! * the destination gains a placement with a **new** UID, keeping the message,
//!   flags and internal date;
//! * the source placement is **gone**, not merely flagged;
//! * the client is told which sequence numbers vanished, in an order it can
//!   apply, and given a `[COPYUID]` whose two sets pair message-for-message.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{
    AccountId, EntryId, MailboxId, MessageFlags, MessageStore, SystemFlags, Timestamp,
};

/// A minimal well-formed message.
const SIMPLE: &[u8] = b"From: alice@example.test\r\n\
Subject: Move test\r\n\
\r\n\
Body.\r\n";

/// Logs in, returning the client, the account and INBOX id. Does not select.
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

/// The `COPYUID` code's three fields, if the response carries one.
fn copyuid(response: &str) -> Option<(String, String, String)> {
    let start = response.find("[COPYUID ")? + "[COPYUID ".len();
    let end = start + response[start..].find(']')?;
    let mut fields = response[start..end].split_whitespace();
    Some((
        fields.next()?.to_owned(),
        fields.next()?.to_owned(),
        fields.next()?.to_owned(),
    ))
}

/// The UIDs a rendered `uid-set` names, expanded.
fn expand(set: &str) -> Vec<u32> {
    set.split(',')
        .flat_map(|piece| match piece.split_once(':') {
            Some((low, high)) => {
                let low: u32 = low.parse().expect("a number");
                let high: u32 = high.parse().expect("a number");
                (low.min(high)..=low.max(high)).collect::<Vec<u32>>()
            }
            None => vec![piece.parse().expect("a number")],
        })
        .collect()
}

/// The sequence numbers on `* <n> EXPUNGE` lines, in the order sent.
fn expunged(response: &str) -> Vec<usize> {
    response
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("* ")?;
            let (number, keyword) = rest.split_once(' ')?;
            keyword
                .starts_with("EXPUNGE")
                .then(|| number.parse().ok())?
        })
        .collect()
}

/// The placements a mailbox holds, ordered by UID.
async fn entries(server: &TestServer, mailbox: MailboxId) -> Vec<sandesh_core::MessageEntry> {
    server
        .messages
        .list_entries(mailbox)
        .await
        .expect("list the placements")
}

/// Whether the placement still exists in storage.
async fn exists(server: &TestServer, entry_id: EntryId) -> bool {
    server
        .messages
        .get_entry(entry_id)
        .await
        .expect("query the entry")
        .is_some()
}

// --- the basic move --------------------------------------------------------

#[tokio::test]
async fn move_creates_the_destination_placement() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1 Archive").await;

    assert!(is_ok(&response), "MOVE failed: {response}");
    let moved = entries(&server, archive).await;
    assert_eq!(moved.len(), 1, "one placement in the destination");
    assert_ne!(
        moved[0].id, source.id,
        "a move creates a new placement, not the same one"
    );
    assert_eq!(
        moved[0].message_id, source.message_id,
        "the message itself is preserved, never re-created"
    );
}

#[tokio::test]
async fn move_removes_the_source_placement() {
    // The property that distinguishes MOVE from COPY. A server that copied and
    // forgot to remove would pass every destination assertion above.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("MOVE 1 Archive").await;

    assert!(
        !exists(&server, source.id).await,
        "the source placement must be gone, not merely flagged"
    );
    assert!(
        entries(&server, inbox).await.is_empty(),
        "the source mailbox is empty"
    );
}

#[tokio::test]
async fn move_preserves_flags_and_internal_date() {
    // RFC 6851 §3.3: the moved message keeps its flags and INTERNALDATE. Only
    // its UID and its mailbox change.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let when = Timestamp::from_micros(1_600_000_000_000_000);
    let source = server
        .deliver_at(
            inbox,
            SIMPLE,
            MessageFlags::from_system(SystemFlags::SEEN.union(SystemFlags::FLAGGED)),
            when,
        )
        .await;

    client.command("SELECT INBOX").await;
    client.command("MOVE 1 Archive").await;

    let moved = entries(&server, archive).await;
    assert_eq!(moved[0].flags, source.flags, "flags survive the move");
    assert_eq!(
        moved[0].internal_date, source.internal_date,
        "INTERNALDATE survives the move"
    );
}

#[tokio::test]
async fn the_destination_uid_comes_from_the_destination_mailbox() {
    // The moved placement is allocated a UID from Archive's own sequence, not
    // carried over from INBOX. With one message already in Archive, a server
    // that reused the source UID would collide with it.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(archive, b"From: a@b\r\n\r\nfirst\r\n").await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1 Archive").await;

    let moved = entries(&server, archive).await;
    assert_eq!(moved.len(), 2, "Archive holds both messages");
    let new = moved
        .iter()
        .find(|e| e.message_id == source.message_id)
        .expect("the moved placement");
    assert_ne!(
        new.uid, moved[0].uid,
        "the moved placement gets its own UID in the destination"
    );
    assert!(is_ok(&response), "MOVE failed: {response}");
}

// --- multiple messages -----------------------------------------------------

/// Delivers `count` distinct messages to `mailbox`, returning their placements.
async fn deliver_many(
    server: &TestServer,
    mailbox: MailboxId,
    count: usize,
) -> Vec<sandesh_core::MessageEntry> {
    let mut delivered = Vec::with_capacity(count);
    for index in 0..count {
        let raw = format!("From: alice@example.test\r\nSubject: m{index}\r\n\r\nBody {index}.\r\n");
        delivered.push(server.deliver(mailbox, raw.as_bytes()).await);
    }
    delivered
}

#[tokio::test]
async fn move_handles_multiple_messages_in_one_command() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let sources = deliver_many(&server, inbox, 3).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1:3 Archive").await;

    assert!(is_ok(&response), "MOVE failed: {response}");
    assert_eq!(entries(&server, archive).await.len(), 3, "all three moved");
    assert!(
        entries(&server, inbox).await.is_empty(),
        "none left in the source"
    );
    for source in &sources {
        assert!(
            !exists(&server, source.id).await,
            "every source placement is removed"
        );
    }
}

#[tokio::test]
async fn multi_message_copyuid_pairs_each_source_with_its_own_destination() {
    // The load-bearing assertion of this file. The nth source UID must name the
    // same *message* as the nth destination UID. A server that paired them by
    // position after independently sorting one set would pass a length check
    // and fail this.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let sources = deliver_many(&server, inbox, 3).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1:3 Archive").await;

    let (_, source_set, destination_set) =
        copyuid(&response).expect("a multi-message MOVE reports COPYUID");
    let source_uids = expand(&source_set);
    let destination_uids = expand(&destination_set);
    assert_eq!(
        source_uids.len(),
        destination_uids.len(),
        "the two sets must correspond message-for-message"
    );

    let moved = entries(&server, archive).await;
    for (source_uid, destination_uid) in source_uids.iter().zip(&destination_uids) {
        let expected_message = sources
            .iter()
            .find(|e| e.uid.get() == *source_uid)
            .map(|e| e.message_id)
            .expect("the source UID names a delivered message");
        let actual_message = moved
            .iter()
            .find(|e| e.uid.get() == *destination_uid)
            .map(|e| e.message_id)
            .expect("the destination UID names a placement in Archive");
        assert_eq!(
            actual_message, expected_message,
            "source UID {source_uid} must pair with destination UID {destination_uid}"
        );
    }
}

#[tokio::test]
async fn expunge_lines_are_sent_highest_sequence_first() {
    // RFC 9051 §7.4.1. Descending order is what makes each number still valid
    // when the client applies it; ascending order would corrupt the client's
    // view from the second line onwards.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    deliver_many(&server, inbox, 3).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1:3 Archive").await;

    assert_eq!(
        expunged(&response),
        vec![3, 2, 1],
        "each reported number must still be valid when applied: {response}"
    );
}

#[tokio::test]
async fn a_move_of_a_middle_message_reports_its_sequence_number() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    deliver_many(&server, inbox, 3).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 2 Archive").await;

    assert_eq!(expunged(&response), vec![2], "response was: {response}");
}

// --- set forms -------------------------------------------------------------

#[tokio::test]
async fn move_star_names_the_last_message() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let sources = deliver_many(&server, inbox, 3).await;

    client.command("SELECT INBOX").await;
    client.command("MOVE * Archive").await;

    let moved = entries(&server, archive).await;
    assert_eq!(moved.len(), 1);
    assert_eq!(
        moved[0].message_id, sources[2].message_id,
        "`*` is the last message"
    );
}

#[tokio::test]
async fn a_descending_range_moves_the_same_messages_as_its_ascending_twin() {
    // RFC 9051 §9: `3:1` and `1:3` name the same set.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    deliver_many(&server, inbox, 3).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 3:1 Archive").await;

    assert!(is_ok(&response), "MOVE failed: {response}");
    assert_eq!(entries(&server, archive).await.len(), 3);
}

#[tokio::test]
async fn duplicate_members_move_the_message_once() {
    // `1,1,1` names one message. Attempting it three times would move it once
    // and then fail twice against a placement that no longer exists — which
    // would make an otherwise complete MOVE report itself partial.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1,1,1 Archive").await;

    assert!(is_ok(&response), "MOVE must not report partial: {response}");
    assert_eq!(entries(&server, archive).await.len(), 1);
    assert_eq!(
        expunged(&response),
        vec![1],
        "reported once, not three times"
    );
}

#[tokio::test]
async fn a_set_naming_no_message_is_a_success_that_moves_nothing() {
    // RFC 9051 §6.4.8: an out-of-range member is not an error.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 99 Archive").await;

    assert!(is_ok(&response), "must not be an error: {response}");
    assert!(
        copyuid(&response).is_none(),
        "nothing moved, so there is no COPYUID to report: {response}"
    );
    assert!(expunged(&response).is_empty(), "nothing was expunged");
    assert_eq!(entries(&server, archive).await.len(), 0);
    assert_eq!(entries(&server, inbox).await.len(), 1, "the source stays");
}

#[tokio::test]
async fn a_mixed_set_moves_the_members_that_exist() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    deliver_many(&server, inbox, 2).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1,99 Archive").await;

    assert!(
        is_ok(&response),
        "a nonexistent member is not a partial failure: {response}"
    );
    assert_eq!(entries(&server, archive).await.len(), 1);
    assert_eq!(entries(&server, inbox).await.len(), 1);
}

// --- UID MOVE --------------------------------------------------------------

#[tokio::test]
async fn uid_move_reads_the_set_as_uids_not_positions() {
    // With a UID gap, position and UID differ. `UID MOVE 3` must name the
    // message whose UID is 3, not the third message.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let delivered = deliver_many(&server, inbox, 4).await;

    // Remove UID 2, leaving UIDs 1, 3, 4 at positions 1, 2, 3.
    server
        .messages
        .delete_entry(delivered[1].id)
        .await
        .expect("delete to create a UID gap");

    client.command("SELECT INBOX").await;
    let response = client.command("UID MOVE 3 Archive").await;

    assert!(is_ok(&response), "UID MOVE failed: {response}");
    let moved = entries(&server, archive).await;
    assert_eq!(moved.len(), 1);
    assert_eq!(
        moved[0].message_id, delivered[2].message_id,
        "UID 3 names the third delivered message, which sits at position 2"
    );
}

#[tokio::test]
async fn uid_move_reports_the_sequence_number_not_the_uid() {
    // RFC 9051 §7.4.1: `* <n> EXPUNGE` is always a *sequence* number, even when
    // the command addressed messages by UID. UID 3 sits at position 2.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let delivered = deliver_many(&server, inbox, 4).await;
    server
        .messages
        .delete_entry(delivered[1].id)
        .await
        .expect("delete to create a UID gap");

    client.command("SELECT INBOX").await;
    let response = client.command("UID MOVE 3 Archive").await;

    assert_eq!(
        expunged(&response),
        vec![2],
        "UID 3 is at sequence 2: {response}"
    );
}

#[tokio::test]
async fn uid_move_across_gaps_maps_every_source_to_its_destination() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let delivered = deliver_many(&server, inbox, 5).await;
    // Leave UIDs 1, 3, 5.
    for index in [1, 3] {
        server
            .messages
            .delete_entry(delivered[index].id)
            .await
            .expect("delete to create UID gaps");
    }

    client.command("SELECT INBOX").await;
    let response = client.command("UID MOVE 1:5 Archive").await;

    assert!(is_ok(&response), "UID MOVE failed: {response}");
    let (_, source_set, destination_set) = copyuid(&response).expect("COPYUID reported");
    assert_eq!(
        expand(&source_set),
        vec![1, 3, 5],
        "the source set names the UIDs that existed, not the range asked for"
    );

    let moved = entries(&server, archive).await;
    for (source_uid, destination_uid) in expand(&source_set)
        .iter()
        .zip(expand(&destination_set).iter())
    {
        let expected = delivered
            .iter()
            .find(|e| e.uid.get() == *source_uid)
            .map(|e| e.message_id)
            .expect("source UID names a delivered message");
        let actual = moved
            .iter()
            .find(|e| e.uid.get() == *destination_uid)
            .map(|e| e.message_id)
            .expect("destination UID names a moved placement");
        assert_eq!(actual, expected, "UID {source_uid} mapped incorrectly");
    }
}

// --- refusals --------------------------------------------------------------

#[tokio::test]
async fn move_into_the_selected_mailbox_is_refused() {
    // ADR-051. The domain places a message in a mailbox at most once, so this
    // move has nowhere to go. Reporting OK would claim a move that did not
    // happen and would expunge a message that is still there.
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1 INBOX").await;

    assert!(is_refused(&response), "must be refused: {response}");
    assert!(
        exists(&server, source.id).await,
        "the message must still be in INBOX"
    );
    assert!(
        expunged(&response).is_empty(),
        "nothing left the mailbox, so nothing may be reported as expunged"
    );
}

#[tokio::test]
async fn move_to_a_nonexistent_mailbox_is_refused_with_trycreate() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1 NoSuchMailbox").await;

    assert!(is_refused(&response), "must be refused: {response}");
    assert!(
        response.contains("TRYCREATE"),
        "RFC 9051 §6.4.7 wants TRYCREATE: {response}"
    );
    assert!(
        exists(&server, source.id).await,
        "a refused MOVE moves nothing"
    );
}

#[tokio::test]
async fn move_to_a_noselect_mailbox_is_refused() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server
        .mailbox_with_attributes(
            account,
            "Holder",
            sandesh_core::MailboxAttributes::from_names([r"\Noselect"]),
        )
        .await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1 Holder").await;

    assert!(is_refused(&response), "must be refused: {response}");
    assert!(exists(&server, source.id).await, "nothing moved");
}

#[tokio::test]
async fn move_is_refused_on_an_examined_mailbox() {
    // MOVE removes from the source, so unlike COPY it needs a writable
    // selection (RFC 9051 §6.3.3).
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("EXAMINE INBOX").await;
    let response = client.command("MOVE 1 Archive").await;

    assert!(
        is_refused(&response),
        "an EXAMINEd mailbox must refuse MOVE: {response}"
    );
    assert!(exists(&server, source.id).await, "nothing moved");
}

#[tokio::test]
async fn move_without_a_selected_mailbox_is_refused() {
    let server = TestServer::start().await;
    let (mut client, account, _inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;

    let response = client.command("MOVE 1 Archive").await;

    assert!(is_refused(&response), "must be refused: {response}");
}

#[tokio::test]
async fn move_without_authentication_is_refused() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("MOVE 1 Archive").await;

    assert!(is_refused(&response), "must be refused: {response}");
}

#[tokio::test]
async fn move_without_a_destination_is_a_syntax_error() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE 1").await;

    assert!(is_refused(&response), "must be refused: {response}");
}

#[tokio::test]
async fn a_malformed_set_is_refused_and_moves_nothing() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("MOVE not-a-set Archive").await;

    assert!(is_refused(&response), "must be refused: {response}");
    assert!(
        exists(&server, source.id).await,
        "a malformed command mutates nothing"
    );
}

// --- the session's own view ------------------------------------------------

#[tokio::test]
async fn the_acting_session_renumbers_after_a_move() {
    // ADR-049 applied to MOVE: the client was just told `* 1 EXPUNGE`, so its
    // next `FETCH 1` must address what is now the first message.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let delivered = deliver_many(&server, inbox, 3).await;

    client.command("SELECT INBOX").await;
    client.command("MOVE 1 Archive").await;
    let response = client.command("UID FETCH 1:* (UID)").await;

    // UID 1 moved away; the survivors are UIDs 2 and 3, now at positions 1 and 2.
    assert!(
        !response.contains("UID 1)"),
        "the moved message must be gone from this session's view: {response}"
    );
    let fetched = response.matches("* ").count();
    assert_eq!(fetched, 2, "two messages remain: {response}");
    assert_eq!(delivered.len(), 3);
}

#[tokio::test]
async fn uids_are_stable_across_a_move() {
    // A move renumbers positions but must never rewrite the UIDs of the
    // messages that stayed.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let delivered = deliver_many(&server, inbox, 3).await;

    client.command("SELECT INBOX").await;
    client.command("MOVE 1 Archive").await;

    let remaining = entries(&server, inbox).await;
    let uids: Vec<u32> = remaining.iter().map(|e| e.uid.get()).collect();
    assert_eq!(
        uids,
        vec![delivered[1].uid.get(), delivered[2].uid.get()],
        "surviving UIDs are unchanged"
    );
}

#[tokio::test]
async fn a_fetch_after_a_move_addresses_the_renumbered_mailbox() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let delivered = deliver_many(&server, inbox, 3).await;

    client.command("SELECT INBOX").await;
    client.command("MOVE 1 Archive").await;
    let response = client.command("FETCH 1 (UID)").await;

    assert!(
        response.contains(&format!("UID {}", delivered[1].uid.get())),
        "sequence 1 must now be the former sequence 2: {response}"
    );
}

// --- the message is shared, not duplicated ---------------------------------

#[tokio::test]
async fn moving_one_placement_leaves_other_placements_alone() {
    // The same message placed in two mailboxes has two independent placements.
    // Moving one must not disturb the other — the isolation the domain model
    // guarantees and that a mailbox-scoped MOVE must preserve.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let other = server.mailbox(account, "Other").await;
    let source = server.deliver(inbox, SIMPLE).await;

    // Place the same message in Other as well.
    let shared = server
        .messages
        .copy_entry(source.id, other)
        .await
        .expect("copy into Other");

    client.command("SELECT INBOX").await;
    client.command("MOVE 1 Archive").await;

    assert!(
        exists(&server, shared.id).await,
        "the other mailbox's placement of the same message is untouched"
    );
    assert_eq!(
        entries(&server, other).await.len(),
        1,
        "Other still holds its placement"
    );
    assert_eq!(entries(&server, archive).await.len(), 1);
}

#[tokio::test]
async fn a_moved_message_is_fetchable_from_the_destination() {
    // End to end: the bytes survive the move and are readable through the
    // destination mailbox.
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("MOVE 1 Archive").await;
    client.command("SELECT Archive").await;
    let response = client.command("FETCH 1 (BODY[])").await;

    assert!(
        response.contains("Subject: Move test"),
        "the moved message reads back intact: {response}"
    );
}
