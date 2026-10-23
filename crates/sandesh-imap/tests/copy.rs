//! `COPY` and `UID COPY` over a real socket (RFC 9051 §6.4.7, RFC 4315 §3).
//!
//! Every test drives the production server over TCP against messages stored
//! through the production `MessageStore`. Assertions about what was *created*
//! read back **through storage**, not out of the response: a server that
//! reports a plausible `[COPYUID]` without creating the placements would pass a
//! response-only test and fail these.
//!
//! The recurring theme is that a copy creates a **new placement of a shared
//! message**. Three things must all hold at once, and they are easy to get
//! individually right and jointly wrong: the destination gets a new `EntryId`
//! and a new UID; the source keeps its UID, flags and membership; and the
//! underlying `Message` is reused rather than duplicated.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{
    AccountId, EntryId, EventKind, MailboxId, MessageFlags, MessageStore, SystemFlags,
};

/// A minimal well-formed message.
const SIMPLE: &[u8] = b"From: alice@example.test\r\n\
Subject: Copy test\r\n\
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
///
/// Returns `(uidvalidity, source set, destination set)` as written on the wire,
/// so a test can assert on the exact rendering rather than on a re-parse.
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

/// The numbers on a `* SEARCH` line.
fn search_numbers(response: &str) -> Vec<u32> {
    response
        .lines()
        .find(|line| line.starts_with("* SEARCH"))
        .map(|line| {
            line.trim_start_matches("* SEARCH")
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect()
        })
        .unwrap_or_default()
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

// --- COPY ------------------------------------------------------------------

#[tokio::test]
async fn copy_creates_a_placement_in_the_destination() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 Archive").await;

    assert!(is_ok(&response), "COPY failed: {response}");
    let copied = entries(&server, archive).await;
    assert_eq!(copied.len(), 1, "one placement in the destination");
    assert_ne!(
        copied[0].id, source.id,
        "a copy is a new placement, not the same one"
    );
    assert_eq!(
        copied[0].message_id, source.message_id,
        "the message is shared, never duplicated"
    );
}

#[tokio::test]
async fn copy_leaves_the_source_untouched() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let source = server
        .deliver_with_flags(inbox, SIMPLE, MessageFlags::from_system(SystemFlags::SEEN))
        .await;

    client.command("SELECT INBOX").await;
    client.command("COPY 1 Archive").await;

    let after = server
        .messages
        .get_entry(source.id)
        .await
        .expect("query")
        .expect("the source still exists");
    assert_eq!(after.uid, source.uid, "the source UID must not change");
    assert_eq!(after.mailbox_id, inbox, "the source stays where it was");
    assert!(after.flags.has(SystemFlags::SEEN), "source flags unchanged");
    assert_eq!(entries(&server, inbox).await.len(), 1, "source membership");
}

#[tokio::test]
async fn copy_moves_multiple_messages_in_one_command() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1:3 Archive").await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(entries(&server, archive).await.len(), 3);
    assert_eq!(entries(&server, inbox).await.len(), 3, "source intact");
}

#[tokio::test]
async fn copy_of_a_subset_copies_only_that_subset() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    let second = server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("COPY 2 Archive").await;

    let copied = entries(&server, archive).await;
    assert_eq!(copied.len(), 1);
    assert_eq!(
        copied[0].message_id, second.message_id,
        "sequence 2 is the second message"
    );
}

#[tokio::test]
async fn copy_preserves_flags() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let flags = MessageFlags::from_system(SystemFlags::SEEN.union(SystemFlags::FLAGGED));
    server.deliver_with_flags(inbox, SIMPLE, flags).await;

    client.command("SELECT INBOX").await;
    client.command("COPY 1 Archive").await;

    let copied = entries(&server, archive).await;
    assert!(copied[0].flags.has(SystemFlags::SEEN), "\\Seen preserved");
    assert!(
        copied[0].flags.has(SystemFlags::FLAGGED),
        "\\Flagged preserved"
    );
}

#[tokio::test]
async fn copy_preserves_the_internal_date() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    // A date that is demonstrably not "now", so a copy that re-stamps it fails.
    let past = sandesh_core::Timestamp::from_micros(1_000_000_000_000_000);
    let source = server
        .deliver_at(inbox, SIMPLE, MessageFlags::empty(), past)
        .await;

    client.command("SELECT INBOX").await;
    client.command("COPY 1 Archive").await;

    let copied = entries(&server, archive).await;
    assert_eq!(
        copied[0].internal_date, source.internal_date,
        "INTERNALDATE must survive the copy (RFC 9051 §6.4.7)"
    );
}

#[tokio::test]
async fn the_copied_message_keeps_its_raw_bytes() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("COPY 1 Archive").await;

    let copied = entries(&server, archive).await;
    let raw = server
        .messages
        .raw(copied[0].message_id)
        .await
        .expect("query the bytes")
        .expect("the message has bytes");
    assert_eq!(
        raw.bytes(),
        SIMPLE,
        "the shared message's bytes must be byte-exact"
    );
}

#[tokio::test]
async fn an_empty_result_is_a_successful_copy_that_copied_nothing() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    // Sequence 99 names no message. RFC 9051 §6.4.8: not an error.
    let response = client.command("COPY 99 Archive").await;

    assert!(is_ok(&response), "out of range is not an error: {response}");
    assert!(
        copyuid(&response).is_none(),
        "nothing was copied, so there are no UIDs to report: {response}"
    );
    assert!(entries(&server, archive).await.is_empty());
}

#[tokio::test]
async fn copy_on_an_empty_mailbox_succeeds_quietly() {
    let server = TestServer::start().await;
    let (mut client, account, _inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1:* Archive").await;

    assert!(is_ok(&response), "{response}");
    assert!(copyuid(&response).is_none(), "{response}");
    assert!(entries(&server, archive).await.is_empty());
}

// --- new destination UIDs --------------------------------------------------

#[tokio::test]
async fn the_copy_gets_a_new_uid_from_the_destinations_sequence() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    // Put two messages in Archive first, so its UID sequence has advanced and
    // a copied UID cannot coincidentally equal the source UID.
    server.deliver(archive, SIMPLE).await;
    server.deliver(archive, SIMPLE).await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 Archive").await;

    let copied = entries(&server, archive).await;
    let new = copied.last().expect("the copy");
    assert!(
        new.uid.get() > 2,
        "the destination UID must continue Archive's sequence, got {}",
        new.uid.get()
    );
    assert_ne!(
        new.uid, source.uid,
        "the destination UID is Archive's, not INBOX's"
    );
    let (_, _, destinations) = copyuid(&response).expect("a COPYUID");
    assert_eq!(
        expand(&destinations),
        vec![new.uid.get()],
        "COPYUID must report the UID storage actually assigned: {response}"
    );
}

#[tokio::test]
async fn destination_uids_are_the_ones_storage_assigned() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1:2 Archive").await;

    let stored: Vec<u32> = entries(&server, archive)
        .await
        .iter()
        .map(|e| e.uid.get())
        .collect();
    let (_, _, destinations) = copyuid(&response).expect("a COPYUID");
    assert_eq!(
        expand(&destinations),
        stored,
        "reported UIDs must match storage exactly: {response}"
    );
}

#[tokio::test]
async fn two_different_messages_get_two_different_destination_uids() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    server
        .deliver(
            inbox,
            b"From: bob@example.test\r\nSubject: Second\r\n\r\nOther.\r\n",
        )
        .await;

    client.command("SELECT INBOX").await;
    let first = client.command("COPY 1 Archive").await;
    let second = client.command("COPY 2 Archive").await;

    let (_, _, first_dest) = copyuid(&first).expect("a COPYUID");
    let (_, _, second_dest) = copyuid(&second).expect("a COPYUID");
    assert_ne!(
        expand(&first_dest),
        expand(&second_dest),
        "each placement gets its own UID from the destination's sequence"
    );
    assert_eq!(entries(&server, archive).await.len(), 2);
}

#[tokio::test]
async fn recopying_the_same_message_to_the_same_destination_is_refused() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let first = client.command("COPY 1 Archive").await;
    assert!(is_ok(&first), "{first}");

    // The same idempotent-placement invariant as a same-mailbox copy, reached
    // by a different route: Archive already holds this message. The copy fails
    // in storage, contributes no COPYUID pair, and creates nothing.
    let second = client.command("COPY 1 Archive").await;
    assert!(
        copyuid(&second).is_none(),
        "a copy that created nothing must report no UIDs: {second}"
    );
    assert_eq!(
        entries(&server, archive).await.len(),
        1,
        "still exactly one placement"
    );
}

// --- COPYUID correctness ---------------------------------------------------

#[tokio::test]
async fn copyuid_reports_the_destination_uidvalidity() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    let destination = server
        .mailbox_by_name(account, "Archive")
        .await
        .expect("Archive exists");
    let source = server
        .mailbox_by_name(account, "INBOX")
        .await
        .expect("INBOX exists");

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 Archive").await;

    let (validity, _, _) = copyuid(&response).expect("a COPYUID");
    assert_eq!(
        validity,
        destination.uid_validity.get().to_string(),
        "COPYUID names the destination's epoch, not the source's: {response}"
    );
    // Only meaningful if the two actually differ, which the fixture arranges.
    if destination.uid_validity != source.uid_validity {
        assert_ne!(validity, source.uid_validity.get().to_string());
    }
    let _ = archive;
}

#[tokio::test]
async fn copyuid_pairs_source_and_destination_positionally() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let a = server.deliver(inbox, SIMPLE).await;
    let b = server.deliver(inbox, SIMPLE).await;
    let c = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1:3 Archive").await;

    let (_, sources, destinations) = copyuid(&response).expect("a COPYUID");
    let sources = expand(&sources);
    let destinations = expand(&destinations);
    assert_eq!(
        sources,
        vec![a.uid.get(), b.uid.get(), c.uid.get()],
        "{response}"
    );
    assert_eq!(
        sources.len(),
        destinations.len(),
        "the two sets must name the same number of messages: {response}"
    );

    // The nth of each set must be the same message (RFC 4315 §3).
    let copied = entries(&server, archive).await;
    for (index, source_uid) in sources.iter().enumerate() {
        let source_message = [&a, &b, &c]
            .iter()
            .find(|e| e.uid.get() == *source_uid)
            .expect("a source entry")
            .message_id;
        let destination_entry = copied
            .iter()
            .find(|e| e.uid.get() == destinations[index])
            .expect("the destination placement named by COPYUID");
        assert_eq!(
            destination_entry.message_id, source_message,
            "COPYUID pair {index} names two different messages"
        );
    }
}

#[tokio::test]
async fn copyuid_source_set_preserves_uid_gaps() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let first = server.deliver(inbox, SIMPLE).await;
    let doomed = server
        .deliver_with_flags(
            inbox,
            SIMPLE,
            MessageFlags::from_system(SystemFlags::DELETED),
        )
        .await;
    let third = server.deliver(inbox, SIMPLE).await;

    // Open a UID gap in the source.
    let mut prep = server.connect().await;
    prep.command("LOGIN alice any").await;
    prep.command("SELECT INBOX").await;
    prep.command("EXPUNGE").await;
    assert!(!exists(&server, doomed.id).await);

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1:2 Archive").await;

    let (_, sources, _) = copyuid(&response).expect("a COPYUID");
    assert_eq!(
        expand(&sources),
        vec![first.uid.get(), third.uid.get()],
        "the source set must name the real UIDs, gap included: {response}"
    );
}

#[tokio::test]
async fn a_single_message_copy_renders_single_uids() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 Archive").await;

    let (_, sources, destinations) = copyuid(&response).expect("a COPYUID");
    assert_eq!(sources, "1", "{response}");
    assert_eq!(destinations, "1", "{response}");
}

// --- sequence vs UID -------------------------------------------------------

#[tokio::test]
async fn uid_copy_resolves_uids_not_sequence_numbers() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    // Expunge the first two to leave UIDs 3 and 4 at sequence 1 and 2.
    server
        .deliver_with_flags(
            inbox,
            SIMPLE,
            MessageFlags::from_system(SystemFlags::DELETED),
        )
        .await;
    server
        .deliver_with_flags(
            inbox,
            SIMPLE,
            MessageFlags::from_system(SystemFlags::DELETED),
        )
        .await;
    let third = server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;

    let mut prep = server.connect().await;
    prep.command("LOGIN alice any").await;
    prep.command("SELECT INBOX").await;
    prep.command("EXPUNGE").await;

    client.command("SELECT INBOX").await;
    // `1:2` as UIDs names nothing that still exists; read as sequence numbers
    // it would copy both survivors.
    let response = client.command("UID COPY 1:2 Archive").await;

    assert!(is_ok(&response), "{response}");
    assert!(
        copyuid(&response).is_none(),
        "UIDs 1-2 are gone; sequence numbers must not leak in: {response}"
    );
    assert!(
        entries(&server, archive).await.is_empty(),
        "nothing should have been copied"
    );

    // And the same numbers as UIDs of live messages do copy.
    let response = client
        .command(&format!("UID COPY {} Archive", third.uid.get()))
        .await;
    assert!(is_ok(&response), "{response}");
    assert_eq!(entries(&server, archive).await.len(), 1);
}

#[tokio::test]
async fn sequence_copy_and_uid_copy_select_different_messages() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    // Build a gap so sequence 1 holds UID 2.
    let doomed = server
        .deliver_with_flags(
            inbox,
            SIMPLE,
            MessageFlags::from_system(SystemFlags::DELETED),
        )
        .await;
    let survivor = server.deliver(inbox, SIMPLE).await;

    let mut prep = server.connect().await;
    prep.command("LOGIN alice any").await;
    prep.command("SELECT INBOX").await;
    prep.command("EXPUNGE").await;
    assert!(!exists(&server, doomed.id).await);

    client.command("SELECT INBOX").await;
    // `COPY 1` names sequence 1 → UID 2. `UID COPY 1` names UID 1 → nothing.
    let by_sequence = client.command("COPY 1 Archive").await;
    let (_, sources, _) = copyuid(&by_sequence).expect("a COPYUID");
    assert_eq!(expand(&sources), vec![survivor.uid.get()], "{by_sequence}");

    let by_uid = client.command("UID COPY 1 Archive").await;
    assert!(copyuid(&by_uid).is_none(), "UID 1 was expunged: {by_uid}");
    assert_eq!(entries(&server, archive).await.len(), 1, "only one copy");
}

#[tokio::test]
async fn a_star_in_a_uid_copy_means_the_largest_uid_in_use() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    let last = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("UID COPY * Archive").await;

    let (_, sources, _) = copyuid(&response).expect("a COPYUID");
    assert_eq!(
        expand(&sources),
        vec![last.uid.get()],
        "`*` is the largest UID in use: {response}"
    );
}

#[tokio::test]
async fn a_star_in_a_sequence_copy_means_the_last_message() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    let last = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY * Archive").await;

    let (_, sources, _) = copyuid(&response).expect("a COPYUID");
    assert_eq!(expand(&sources), vec![last.uid.get()], "{response}");
}

#[tokio::test]
async fn an_open_ended_range_reaches_the_end_of_the_mailbox() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 2:* Archive").await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(entries(&server, archive).await.len(), 2);
}

#[tokio::test]
async fn a_duplicated_member_copies_the_message_once() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1,1,1:1 Archive").await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(
        entries(&server, archive).await.len(),
        1,
        "naming a message three times copies it once: {response}"
    );
    let (_, sources, destinations) = copyuid(&response).expect("a COPYUID");
    assert_eq!(expand(&sources).len(), 1, "{response}");
    assert_eq!(expand(&destinations).len(), 1, "{response}");
}

#[tokio::test]
async fn a_duplicated_uid_copies_the_message_once() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let entry = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let uid = entry.uid.get();
    let response = client
        .command(&format!("UID COPY {uid},{uid},{uid}:{uid} Archive"))
        .await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(entries(&server, archive).await.len(), 1);
}

#[tokio::test]
async fn a_descending_range_names_the_same_messages() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let low = server.deliver(inbox, SIMPLE).await;
    let high = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    // RFC 9051 §9: `2:1` and `1:2` are the same set.
    let response = client
        .command(&format!(
            "UID COPY {}:{} Archive",
            high.uid.get(),
            low.uid.get()
        ))
        .await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(entries(&server, archive).await.len(), 2);
}

#[tokio::test]
async fn a_uid_naming_no_message_is_a_successful_no_op() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("UID COPY 9999 Archive").await;

    assert!(is_ok(&response), "{response}");
    assert!(copyuid(&response).is_none(), "{response}");
    assert!(entries(&server, archive).await.is_empty());
}

#[tokio::test]
async fn a_partially_matching_range_copies_only_what_exists() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    // `1:99` names two live messages and 97 that do not exist.
    let response = client.command("COPY 1:99 Archive").await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(entries(&server, archive).await.len(), 2);
    let (_, sources, _) = copyuid(&response).expect("a COPYUID");
    assert_eq!(expand(&sources).len(), 2, "{response}");
}

// --- same-mailbox and cross-mailbox ----------------------------------------

#[tokio::test]
async fn copying_into_the_selected_mailbox_is_refused() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 INBOX").await;

    // ADR-050: the domain model places a message in a mailbox at most once
    // (`CORE_SPEC.md` §7, enforced by a unique index), so the second placement
    // RFC 9051 §6.4.7 would allow cannot exist. Refusing says so; reporting OK
    // would claim a copy that never happened.
    assert!(
        is_refused(&response),
        "a same-mailbox COPY must be refused, not silently ignored: {response}"
    );
    assert!(
        copyuid(&response).is_none(),
        "a refusal carries no COPYUID: {response}"
    );
    assert_eq!(
        entries(&server, inbox).await.len(),
        1,
        "the mailbox is unchanged"
    );
}

#[tokio::test]
async fn a_refused_same_mailbox_copy_leaves_the_snapshot_alone() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;

    let selected = client.command("SELECT INBOX").await;
    assert!(selected.contains("* 1 EXISTS"), "{selected}");
    client.command("COPY 1 INBOX").await;

    // Nothing was created, so nothing can have shifted.
    let response = client.command("SEARCH ALL").await;
    assert_eq!(search_numbers(&response), vec![1], "{response}");
    assert_eq!(entries(&server, inbox).await.len(), 1);
}

#[tokio::test]
async fn a_repeated_same_mailbox_copy_never_grows_the_mailbox() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    // Whatever a client does here, the mailbox cannot grow: there is no
    // doubling cascade to bound because the copy is refused outright.
    for _ in 0..3 {
        let response = client.command("COPY 1:* INBOX").await;
        assert!(is_refused(&response), "{response}");
    }

    assert_eq!(entries(&server, inbox).await.len(), 1);
}

#[tokio::test]
async fn copy_between_two_non_inbox_mailboxes() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    server.mailbox(account, "INBOX").await;
    let work = server.mailbox(account, "Work").await;
    let done = server.mailbox(account, "Done").await;
    server.deliver(work, SIMPLE).await;

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;
    client.command("SELECT Work").await;
    let response = client.command("COPY 1 Done").await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(entries(&server, done).await.len(), 1);
    assert_eq!(entries(&server, work).await.len(), 1, "source intact");
}

#[tokio::test]
async fn copy_into_a_hierarchical_mailbox_name() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let nested = server.mailbox(account, "Work/Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 Work/Archive").await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(entries(&server, nested).await.len(), 1);
}

#[tokio::test]
async fn copy_accepts_a_quoted_mailbox_name() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let sent = server.mailbox(account, "Sent Items").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 \"Sent Items\"").await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(entries(&server, sent).await.len(), 1);
}

// --- the copy is usable ----------------------------------------------------

#[tokio::test]
async fn the_copy_can_be_selected_and_fetched_from_the_destination() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let raw = b"From: alice@example.test\r\nSubject: Findable\r\n\r\nPayload.\r\n";
    server.deliver(inbox, raw).await;

    client.command("SELECT INBOX").await;
    let copied = client.command("COPY 1 Archive").await;
    let (_, _, destinations) = copyuid(&copied).expect("a COPYUID");
    let destination_uid = expand(&destinations)[0];

    // SELECT the destination and read the message back by the reported UID.
    let selected = client.command("SELECT Archive").await;
    assert!(selected.contains("* 1 EXISTS"), "{selected}");

    let fetched = client
        .command(&format!("UID FETCH {destination_uid} (BODY.PEEK[])"))
        .await;
    assert!(
        fetched.contains("Findable") && fetched.contains("Payload."),
        "the copy must be readable at the UID COPYUID reported: {fetched}"
    );
}

#[tokio::test]
async fn the_copy_is_searchable_in_the_destination() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    server
        .deliver(
            inbox,
            b"From: alice@example.test\r\nSubject: Unique marker\r\n\r\nBody.\r\n",
        )
        .await;

    client.command("SELECT INBOX").await;
    client.command("COPY 1 Archive").await;
    client.command("SELECT Archive").await;

    let response = client.command("SEARCH SUBJECT \"Unique marker\"").await;
    assert_eq!(search_numbers(&response), vec![1], "{response}");
}

#[tokio::test]
async fn the_copys_flags_are_independent_of_the_source() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("COPY 1 Archive").await;

    // Flag the copy, then confirm the source did not change: the placements own
    // their flags separately even though the message is shared.
    client.command("SELECT Archive").await;
    let stored = client.command("STORE 1 +FLAGS (\\Flagged)").await;
    assert!(is_ok(&stored), "{stored}");

    let source_after = server
        .messages
        .get_entry(source.id)
        .await
        .expect("query")
        .expect("the source exists");
    assert!(
        !source_after.flags.has(SystemFlags::FLAGGED),
        "flagging the copy must not flag the source"
    );
}

// --- destination errors ----------------------------------------------------

#[tokio::test]
async fn copy_to_a_nonexistent_mailbox_is_refused_with_trycreate() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 NoSuchMailbox").await;

    assert!(is_refused(&response), "{response}");
    assert!(
        response.contains("[TRYCREATE]"),
        "RFC 9051 §6.4.7 requires TRYCREATE: {response}"
    );
    assert!(
        exists(&server, source.id).await,
        "a failed COPY must leave the source alone"
    );
}

#[tokio::test]
async fn a_failed_copy_creates_no_placement() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 NoSuchMailbox").await;
    assert!(is_refused(&response), "{response}");

    assert!(
        entries(&server, archive).await.is_empty(),
        "no placement anywhere, including in an unrelated mailbox"
    );
    assert_eq!(entries(&server, inbox).await.len(), 1);
}

#[tokio::test]
async fn copy_to_a_noselect_mailbox_is_refused() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let parent = server
        .mailbox_with_attributes(
            account,
            "Holder",
            sandesh_core::MailboxAttributes::from_names([r"\Noselect"]),
        )
        .await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 Holder").await;

    assert!(is_refused(&response), "{response}");
    assert!(entries(&server, parent).await.is_empty());
}

#[tokio::test]
async fn copy_from_a_read_only_selection_is_allowed() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    // EXAMINE makes the *source* read-only. COPY does not mutate the source,
    // so RFC 9051 §6.4.7 permits it: the write lands in the destination.
    let selected = client.command("EXAMINE INBOX").await;
    assert!(selected.contains("READ-ONLY"), "{selected}");
    let response = client.command("COPY 1 Archive").await;

    assert!(
        is_ok(&response),
        "COPY out of an EXAMINEd mailbox is legal: {response}"
    );
    assert_eq!(entries(&server, archive).await.len(), 1);
    assert_eq!(entries(&server, inbox).await.len(), 1, "source intact");
}

#[tokio::test]
async fn copy_without_a_selected_mailbox_is_refused() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    let response = client.command("COPY 1 Archive").await;

    assert!(is_refused(&response), "{response}");
    assert!(entries(&server, archive).await.is_empty());
}

#[tokio::test]
async fn copy_before_authentication_is_refused() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("COPY 1 Archive").await;
    assert!(is_refused(&response), "{response}");
}

// --- placement isolation ---------------------------------------------------

#[tokio::test]
async fn a_copy_cannot_reach_a_message_in_another_mailbox() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let other = server.mailbox(account, "Other").await;
    let archive = server.mailbox(account, "Archive").await;

    server.deliver(inbox, SIMPLE).await;
    // A fresh mailbox starts its own UID sequence, so this placement's UID
    // collides with INBOX's — the case a mailbox-blind lookup gets wrong.
    let elsewhere = server
        .deliver(
            other,
            b"From: bob@example.test\r\nSubject: Not yours\r\n\r\nX.\r\n",
        )
        .await;

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;
    client.command("SELECT INBOX").await;
    let response = client
        .command(&format!("UID COPY {} Archive", elsewhere.uid.get()))
        .await;

    assert!(is_ok(&response), "{response}");
    let copied = entries(&server, archive).await;
    assert_eq!(copied.len(), 1, "the colliding UID resolves within INBOX");
    assert_ne!(
        copied[0].message_id, elsewhere.message_id,
        "a UID resolves only within the selected mailbox"
    );
    assert_eq!(entries(&server, other).await.len(), 1, "Other untouched");
}

#[tokio::test]
async fn copying_a_message_twice_leaves_three_independent_placements() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let a = server.mailbox(account, "A").await;
    let b = server.mailbox(account, "B").await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("COPY 1 A").await;
    client.command("COPY 1 B").await;

    let placements = server
        .messages
        .entries_for_message(source.message_id)
        .await
        .expect("list the placements");
    assert_eq!(placements.len(), 3, "one original and two copies");
    let mailboxes: std::collections::BTreeSet<MailboxId> =
        placements.iter().map(|e| e.mailbox_id).collect();
    assert_eq!(mailboxes.len(), 3, "one placement per mailbox");
    assert!(mailboxes.contains(&inbox) && mailboxes.contains(&a) && mailboxes.contains(&b));
}

#[tokio::test]
async fn expunging_the_source_leaves_the_copy_intact() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let source = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("COPY 1 Archive").await;
    client.command("STORE 1 +FLAGS (\\Deleted)").await;
    client.command("EXPUNGE").await;

    assert!(!exists(&server, source.id).await, "the source is gone");
    let copied = entries(&server, archive).await;
    assert_eq!(copied.len(), 1, "the copy survives");
    assert!(
        server
            .messages
            .get(source.message_id)
            .await
            .expect("query the message")
            .is_some(),
        "the shared message survives while the copy references it"
    );
}

// --- snapshot behavior -----------------------------------------------------

#[tokio::test]
async fn the_source_snapshot_is_unchanged_by_a_copy() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    let a = server.deliver(inbox, SIMPLE).await;
    let b = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("COPY 1:2 Archive").await;

    // Sequence numbers, UIDs and flags in the source are all as they were.
    let sequences = client.command("SEARCH ALL").await;
    assert_eq!(search_numbers(&sequences), vec![1, 2], "{sequences}");
    let uids = client.command("UID SEARCH ALL").await;
    assert_eq!(
        search_numbers(&uids),
        vec![a.uid.get(), b.uid.get()],
        "{uids}"
    );
}

#[tokio::test]
async fn another_session_with_the_destination_selected_keeps_its_snapshot() {
    let server = TestServer::start().await;
    let (mut actor, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(archive, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;

    // A second session has the destination open, holding one message.
    let mut observer = server.connect().await;
    observer.command("LOGIN alice any").await;
    let selected = observer.command("SELECT Archive").await;
    assert!(selected.contains("* 1 EXISTS"), "{selected}");

    actor.command("SELECT INBOX").await;
    actor.command("COPY 1 Archive").await;

    // No unsolicited synchronisation (ADR-046): the observer sees the copy
    // only when it re-selects.
    let response = observer.command("SEARCH ALL").await;
    assert_eq!(
        search_numbers(&response),
        vec![1],
        "the observer's snapshot must be untouched: {response}"
    );

    let reselected = observer.command("SELECT Archive").await;
    assert!(
        reselected.contains("* 2 EXISTS"),
        "re-selecting picks up the copy: {reselected}"
    );
}

// --- events ----------------------------------------------------------------

#[tokio::test]
async fn a_copy_emits_entry_created() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let before = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryCreated)
        .count();

    client.command("COPY 1 Archive").await;

    let after = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryCreated)
        .count();
    assert_eq!(after, before + 1, "one copy, one EntryCreated event");
}

#[tokio::test]
async fn each_copied_message_emits_exactly_one_event() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let before = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryCreated)
        .count();

    client.command("COPY 1:3 Archive").await;

    let after = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryCreated)
        .count();
    assert_eq!(after, before + 3, "three copies, three events");
}

#[tokio::test]
async fn a_copy_emits_no_deletion_event() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let before = server.events.kinds().len();
    client.command("COPY 1 Archive").await;

    let new: Vec<EventKind> = server.events.kinds().into_iter().skip(before).collect();
    assert!(
        !new.contains(&EventKind::EntryDeleted) && !new.contains(&EventKind::EntryMoved),
        "COPY removes nothing and moves nothing: {new:?}"
    );
}

#[tokio::test]
async fn a_refused_copy_emits_no_event() {
    let server = TestServer::start().await;
    let (mut client, _account, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let before = server.events.kinds().len();

    let response = client.command("COPY 1 NoSuchMailbox").await;
    assert!(is_refused(&response), "{response}");

    assert_eq!(
        server.events.kinds().len(),
        before,
        "a failed operation must emit no false event"
    );
}

#[tokio::test]
async fn a_copy_that_matches_nothing_emits_no_event() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let before = server.events.kinds().len();

    client.command("COPY 99 Archive").await;

    assert_eq!(server.events.kinds().len(), before);
}

// --- malformed input -------------------------------------------------------

#[tokio::test]
async fn copy_without_arguments_is_bad() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY").await;

    assert!(is_refused(&response), "{response}");
    assert!(entries(&server, archive).await.is_empty());
}

#[tokio::test]
async fn copy_without_a_destination_is_bad() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1").await;

    assert!(is_refused(&response), "{response}");
    assert!(entries(&server, archive).await.is_empty());
}

#[tokio::test]
async fn a_malformed_set_copies_nothing() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    for set in ["0", "abc", "1:2:3", "1,,2", "1,"] {
        let response = client.command(&format!("COPY {set} Archive")).await;
        assert!(
            is_refused(&response),
            "`COPY {set} Archive` must be refused: {response}"
        );
    }

    assert!(
        entries(&server, archive).await.is_empty(),
        "no malformed command may have copied anything"
    );
}

#[tokio::test]
async fn a_malformed_uid_set_copies_nothing() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    for set in ["0", "xyz", "1:2:3"] {
        let response = client.command(&format!("UID COPY {set} Archive")).await;
        assert!(is_refused(&response), "{response}");
    }
    assert!(entries(&server, archive).await.is_empty());
}

#[tokio::test]
async fn the_whole_command_is_validated_before_anything_is_copied() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    // A set whose first member is valid and whose last is not: nothing may be
    // copied, not even the valid prefix.
    let response = client.command("COPY 1,bogus Archive").await;

    assert!(is_refused(&response), "{response}");
    assert!(
        entries(&server, archive).await.is_empty(),
        "not even the valid prefix"
    );
}

#[tokio::test]
async fn an_invalid_mailbox_name_copies_nothing() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("COPY 1 \"\"").await;

    assert!(
        is_refused(&response),
        "an empty name is not a mailbox: {response}"
    );
    assert!(entries(&server, archive).await.is_empty());
}

// --- the copy-then-clean flow ----------------------------------------------

#[tokio::test]
async fn the_archive_then_delete_flow_completes() {
    let server = TestServer::start().await;
    let (mut client, account, inbox) = logged_in(&server).await;
    let archive = server.mailbox(account, "Archive").await;
    let keep = server.deliver(inbox, SIMPLE).await;
    let filed = server
        .deliver(
            inbox,
            b"From: bob@example.test\r\nSubject: File me\r\n\r\nBody.\r\n",
        )
        .await;

    client.command("SELECT INBOX").await;

    // COPY to Archive, then mark and expunge from INBOX — the flow a client
    // uses to "move" a message before MOVE exists.
    let copied = client
        .command(&format!("UID COPY {} Archive", filed.uid.get()))
        .await;
    assert!(is_ok(&copied), "{copied}");
    let (_, sources, destinations) = copyuid(&copied).expect("a COPYUID");
    assert_eq!(expand(&sources), vec![filed.uid.get()], "{copied}");
    let archived_uid = expand(&destinations)[0];

    client
        .command(&format!("UID STORE {} +FLAGS (\\Deleted)", filed.uid.get()))
        .await;
    let expunged = client.command("EXPUNGE").await;
    assert!(is_ok(&expunged), "{expunged}");

    // INBOX keeps only the kept message; Archive holds the filed one, readable
    // at the UID COPYUID reported.
    assert!(exists(&server, keep.id).await);
    assert!(!exists(&server, filed.id).await);
    assert_eq!(entries(&server, archive).await.len(), 1);

    client.command("SELECT Archive").await;
    let fetched = client
        .command(&format!("UID FETCH {archived_uid} (BODY.PEEK[])"))
        .await;
    assert!(fetched.contains("File me"), "{fetched}");
}
