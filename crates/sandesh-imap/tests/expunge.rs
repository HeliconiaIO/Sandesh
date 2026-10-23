//! `EXPUNGE` and `UID EXPUNGE` over a real socket (RFC 9051 §6.4.3, RFC 4315 §2.1).
//!
//! Every test drives the production server over TCP against messages stored
//! through the production `MessageStore`. The assertions about what was
//! *removed* read back **through storage**, not out of the response: a server
//! that reports `* 2 EXPUNGE` without deleting the placement would pass a
//! response-only test and fail these.
//!
//! The other half of the coverage is the acting session's own view. After an
//! `EXPUNGE` this session's sequence numbers must have been renumbered, so a
//! following `FETCH`, `SEARCH` or `STORE` addresses the mailbox as it now is
//! (ADR-049). Those tests deliberately issue a second command on the *same*
//! connection rather than re-selecting, because re-selecting would rebuild the
//! snapshot and hide a stale one.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{EntryId, EventKind, MailboxId, MessageFlags, MessageStore, SystemFlags};

/// A minimal well-formed message.
const SIMPLE: &[u8] = b"From: alice@example.test\r\n\
Subject: Expunge test\r\n\
\r\n\
Body.\r\n";

/// Logs in, returning the client and INBOX id. Does not select.
async fn logged_in(server: &TestServer) -> (common::Client<tokio::net::TcpStream>, MailboxId) {
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let mut client = server.connect().await;
    let login = client.command("LOGIN alice any").await;
    assert!(login.contains(" OK"), "login failed: {login}");
    (client, inbox)
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

/// Whether the placement still exists in storage.
async fn exists(server: &TestServer, entry_id: EntryId) -> bool {
    server
        .messages
        .get_entry(entry_id)
        .await
        .expect("query the entry")
        .is_some()
}

/// The `* <n> EXPUNGE` numbers a response carries, in the order sent.
fn expunge_numbers(response: &str) -> Vec<u32> {
    response
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("* ")?;
            let (number, keyword) = rest.split_once(' ')?;
            keyword
                .trim()
                .eq_ignore_ascii_case("EXPUNGE")
                .then_some(())?;
            number.parse().ok()
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

/// Flags with `\Deleted` set.
fn deleted() -> MessageFlags {
    MessageFlags::from_system(SystemFlags::DELETED)
}

// --- EXPUNGE ---------------------------------------------------------------

#[tokio::test]
async fn expunge_removes_the_deleted_placement() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let kept = server.deliver(inbox, SIMPLE).await;
    let doomed = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let response = client.command("EXPUNGE").await;

    assert!(is_ok(&response), "EXPUNGE failed: {response}");
    assert!(
        !exists(&server, doomed.id).await,
        "the \\Deleted placement must be gone from storage"
    );
    assert!(
        exists(&server, kept.id).await,
        "a placement without \\Deleted must survive"
    );
}

#[tokio::test]
async fn expunge_leaves_undeleted_messages_alone() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let a = server.deliver(inbox, SIMPLE).await;
    let b = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client.command("EXPUNGE").await;

    assert!(is_ok(&response), "{response}");
    assert!(
        expunge_numbers(&response).is_empty(),
        "nothing was marked \\Deleted, so nothing may be reported: {response}"
    );
    assert!(exists(&server, a.id).await);
    assert!(exists(&server, b.id).await);
}

#[tokio::test]
async fn expunge_on_an_empty_mailbox_succeeds_quietly() {
    let server = TestServer::start().await;
    let (mut client, _inbox) = logged_in(&server).await;

    client.command("SELECT INBOX").await;
    let response = client.command("EXPUNGE").await;

    assert!(is_ok(&response), "{response}");
    assert!(expunge_numbers(&response).is_empty(), "{response}");
}

#[tokio::test]
async fn expunge_removes_every_deleted_message() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let middle = server.deliver(inbox, SIMPLE).await;
    let last = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let response = client.command("EXPUNGE").await;

    assert!(is_ok(&response), "{response}");
    assert!(!exists(&server, first.id).await);
    assert!(!exists(&server, last.id).await);
    assert!(exists(&server, middle.id).await, "the survivor");
}

#[tokio::test]
async fn expunge_reports_every_removal_as_an_untagged_line() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    // seq 1..4; \Deleted at seq 2 and 4.
    server.deliver(inbox, SIMPLE).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let response = client.command("EXPUNGE").await;

    assert_eq!(
        expunge_numbers(&response),
        vec![4, 2],
        "pre-removal sequence numbers, highest first (RFC 9051 §7.4.1): {response}"
    );
}

#[tokio::test]
async fn expunge_response_numbers_are_sequence_numbers_not_uids() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    // Build a UID gap first, so UIDs and sequence numbers cannot coincide.
    let filler = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    server.deliver(inbox, SIMPLE).await;
    let target = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    let mut prep = server.connect().await;
    prep.command("LOGIN alice any").await;
    prep.command("SELECT INBOX").await;
    prep.command(&format!("UID EXPUNGE {}", filler.uid.get()))
        .await; // removes only UID 1
    assert!(!exists(&server, filler.id).await);

    // INBOX now holds UIDs 2 and 3 at sequence 1 and 2; UID 3 is \Deleted.
    client.command("SELECT INBOX").await;
    let response = client.command("EXPUNGE").await;

    assert_eq!(
        expunge_numbers(&response),
        vec![2],
        "UID {} sits at sequence 2; reporting its UID would be wrong: {response}",
        target.uid.get()
    );
}

// --- the acting session's snapshot (ADR-049) -------------------------------

#[tokio::test]
async fn the_acting_session_sees_the_new_message_count() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    // `SEARCH ALL` returns sequence numbers, so it reads the snapshot's
    // membership directly.
    let response = client.command("SEARCH ALL").await;
    assert_eq!(
        search_numbers(&response),
        vec![1, 2],
        "two messages remain and must be numbered 1 and 2: {response}"
    );
}

#[tokio::test]
async fn fetch_after_expunge_addresses_the_renumbered_mailbox() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let survivor = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    // The survivor was at sequence 2 and is now at sequence 1.
    let response = client.command("FETCH 1 (UID)").await;
    assert!(
        response.contains(&format!("UID {}", survivor.uid.get())),
        "sequence 1 must now be UID {}: {response}",
        survivor.uid.get()
    );

    let response = client.command("FETCH 2 (UID)").await;
    assert!(is_ok(&response), "{response}");
    assert!(
        !response.contains("FETCH ("),
        "sequence 2 no longer exists and must return no data: {response}"
    );
}

#[tokio::test]
async fn uid_fetch_after_expunge_still_finds_the_survivor_by_uid() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let survivor = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    // A UID is a stable identity: expunging another message must not move it.
    let response = client
        .command(&format!("UID FETCH {} (UID)", survivor.uid.get()))
        .await;
    assert!(
        response.contains(&format!("UID {}", survivor.uid.get())),
        "{response}"
    );
}

#[tokio::test]
async fn uid_fetch_of_an_expunged_uid_returns_nothing() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let doomed = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    let response = client
        .command(&format!("UID FETCH {} (UID)", doomed.uid.get()))
        .await;
    assert!(is_ok(&response), "{response}");
    assert!(
        !response.contains("FETCH ("),
        "an expunged UID must resolve to nothing: {response}"
    );
}

#[tokio::test]
async fn store_after_expunge_targets_the_renumbered_message() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let survivor = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    // A stale snapshot would apply this to the expunged placement — which no
    // longer exists — and leave the survivor unflagged.
    let response = client.command("STORE 1 +FLAGS (\\Flagged)").await;
    assert!(is_ok(&response), "{response}");

    let flags = server
        .messages
        .get_entry(survivor.id)
        .await
        .expect("query")
        .expect("the survivor exists")
        .flags;
    assert!(
        flags.has(SystemFlags::FLAGGED),
        "STORE 1 must have reached the survivor, not a ghost"
    );
}

#[tokio::test]
async fn search_after_expunge_never_names_a_removed_message() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let doomed = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let survivor = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    let response = client.command("UID SEARCH ALL").await;
    let uids = search_numbers(&response);
    assert!(uids.contains(&survivor.uid.get()), "{response}");
    assert!(
        !uids.contains(&doomed.uid.get()),
        "an expunged UID must not appear in a search result: {response}"
    );
}

#[tokio::test]
async fn a_second_expunge_finds_nothing_left_to_remove() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let first = client.command("EXPUNGE").await;
    assert_eq!(expunge_numbers(&first), vec![1], "{first}");

    // A stale snapshot would still hold the removed placement and try again,
    // reporting a second phantom line.
    let second = client.command("EXPUNGE").await;
    assert!(is_ok(&second), "{second}");
    assert!(
        expunge_numbers(&second).is_empty(),
        "the removal already happened: {second}"
    );
}

#[tokio::test]
async fn another_session_keeps_its_snapshot_until_it_reselects() {
    let server = TestServer::start().await;
    let (mut actor, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    server.deliver(inbox, SIMPLE).await;

    // A second session selects the mailbox as it stands, with two messages.
    let mut observer = server.connect().await;
    observer.command("LOGIN alice any").await;
    let selected = observer.command("SELECT INBOX").await;
    assert!(selected.contains("* 2 EXISTS"), "{selected}");

    actor.command("SELECT INBOX").await;
    actor.command("EXPUNGE").await;

    // No unsolicited synchronisation in this increment (ADR-046): the
    // observer's snapshot is unchanged until it selects again.
    let response = observer.command("SEARCH ALL").await;
    assert_eq!(
        search_numbers(&response),
        vec![1, 2],
        "the observer's snapshot must be untouched: {response}"
    );

    let reselected = observer.command("SELECT INBOX").await;
    assert!(
        reselected.contains("* 1 EXISTS"),
        "re-selecting must pick up the removal: {reselected}"
    );
}

// --- UID EXPUNGE (RFC 4315 §2.1) -------------------------------------------

#[tokio::test]
async fn uid_expunge_removes_only_the_named_uid() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let target = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let spared = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let response = client
        .command(&format!("UID EXPUNGE {}", target.uid.get()))
        .await;

    assert!(is_ok(&response), "{response}");
    assert!(!exists(&server, target.id).await, "the named UID");
    assert!(
        exists(&server, spared.id).await,
        "a \\Deleted message outside the set must survive"
    );
}

#[tokio::test]
async fn uid_expunge_ignores_a_uid_that_is_not_deleted() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let undeleted = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let response = client
        .command(&format!("UID EXPUNGE {}", undeleted.uid.get()))
        .await;

    assert!(is_ok(&response), "{response}");
    assert!(
        expunge_numbers(&response).is_empty(),
        "naming a UID is not the same as marking it \\Deleted: {response}"
    );
    assert!(exists(&server, undeleted.id).await);
}

#[tokio::test]
async fn uid_expunge_is_the_intersection_of_the_set_and_deleted() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let in_set_undeleted = server.deliver(inbox, SIMPLE).await;
    let in_set_deleted = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let outside_deleted = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let response = client
        .command(&format!(
            "UID EXPUNGE {}:{}",
            in_set_undeleted.uid.get(),
            in_set_deleted.uid.get()
        ))
        .await;

    assert!(is_ok(&response), "{response}");
    assert!(
        exists(&server, in_set_undeleted.id).await,
        "in set, not \\Deleted"
    );
    assert!(
        !exists(&server, in_set_deleted.id).await,
        "in set and \\Deleted"
    );
    assert!(
        exists(&server, outside_deleted.id).await,
        "\\Deleted, not in set"
    );
}

#[tokio::test]
async fn uid_expunge_uses_uids_not_sequence_numbers() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    // Expunge the first two to open a UID gap, leaving UIDs 3 and 4 at
    // sequence 1 and 2.
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let third = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let fourth = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    let mut prep = server.connect().await;
    prep.command("LOGIN alice any").await;
    prep.command("SELECT INBOX").await;
    prep.command("UID EXPUNGE 1:2").await;

    client.command("SELECT INBOX").await;
    // `1:2` as UIDs names nothing that still exists. Read as sequence numbers
    // it would wipe both survivors.
    let response = client.command("UID EXPUNGE 1:2").await;

    assert!(is_ok(&response), "{response}");
    assert!(
        expunge_numbers(&response).is_empty(),
        "UIDs 1-2 are gone; sequence numbers must not leak in: {response}"
    );
    assert!(exists(&server, third.id).await, "UID 3 must survive");
    assert!(exists(&server, fourth.id).await, "UID 4 must survive");
}

#[tokio::test]
async fn uid_expunge_skips_a_gap_rather_than_shifting_along() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let low = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let gap = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let high = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    // Open a hole in the middle of the UID space.
    let mut prep = server.connect().await;
    prep.command("LOGIN alice any").await;
    prep.command("SELECT INBOX").await;
    prep.command(&format!("UID EXPUNGE {}", gap.uid.get()))
        .await;

    client.command("SELECT INBOX").await;
    let response = client
        .command(&format!("UID EXPUNGE {}:{}", gap.uid.get(), gap.uid.get()))
        .await;

    assert!(is_ok(&response), "{response}");
    assert!(
        expunge_numbers(&response).is_empty(),
        "a set naming only the gap must select nothing: {response}"
    );
    assert!(exists(&server, low.id).await);
    assert!(exists(&server, high.id).await);
}

#[tokio::test]
async fn a_star_in_a_uid_expunge_means_the_largest_uid_in_use() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let last = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let response = client.command("UID EXPUNGE *").await;

    assert!(is_ok(&response), "{response}");
    assert!(!exists(&server, last.id).await, "`*` is the largest UID");
    assert!(exists(&server, first.id).await, "and only that one");
}

#[tokio::test]
async fn an_open_ended_uid_range_reaches_the_end_of_the_mailbox() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server.deliver(inbox, SIMPLE).await;
    let second = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let third = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let response = client
        .command(&format!("UID EXPUNGE {}:*", second.uid.get()))
        .await;

    assert!(is_ok(&response), "{response}");
    assert!(exists(&server, first.id).await);
    assert!(!exists(&server, second.id).await);
    assert!(!exists(&server, third.id).await);
}

#[tokio::test]
async fn a_duplicated_uid_removes_the_message_once() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let target = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let uid = target.uid.get();
    let response = client
        .command(&format!("UID EXPUNGE {uid},{uid},{uid}:{uid}"))
        .await;

    assert!(is_ok(&response), "{response}");
    assert_eq!(
        expunge_numbers(&response),
        vec![1],
        "one message, one report: {response}"
    );
    assert!(!exists(&server, target.id).await);
}

#[tokio::test]
async fn a_uid_naming_no_message_is_a_successful_no_op() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let kept = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let response = client.command("UID EXPUNGE 9999").await;

    assert!(
        is_ok(&response),
        "a UID with no message is not an error: {response}"
    );
    assert!(expunge_numbers(&response).is_empty(), "{response}");
    assert!(exists(&server, kept.id).await);
}

#[tokio::test]
async fn a_descending_uid_range_names_the_same_messages() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let low = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let high = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    // RFC 9051 §9: `4:2` and `2:4` are the same set.
    let response = client
        .command(&format!("UID EXPUNGE {}:{}", high.uid.get(), low.uid.get()))
        .await;

    assert!(is_ok(&response), "{response}");
    assert!(!exists(&server, low.id).await);
    assert!(!exists(&server, high.id).await);
}

#[tokio::test]
async fn uid_expunge_renumbers_the_acting_snapshot_too() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let doomed = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let survivor = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client
        .command(&format!("UID EXPUNGE {}", doomed.uid.get()))
        .await;

    let response = client.command("FETCH 1 (UID)").await;
    assert!(
        response.contains(&format!("UID {}", survivor.uid.get())),
        "the survivor must have moved to sequence 1: {response}"
    );
}

// --- read-only protection --------------------------------------------------

#[tokio::test]
async fn examine_rejects_expunge_without_removing_anything() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let marked = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    let selected = client.command("EXAMINE INBOX").await;
    assert!(selected.contains("READ-ONLY"), "{selected}");

    let response = client.command("EXPUNGE").await;
    assert!(
        is_refused(&response),
        "EXAMINE must refuse EXPUNGE: {response}"
    );
    assert!(
        expunge_numbers(&response).is_empty(),
        "a refusal must report no removals: {response}"
    );
    assert!(
        exists(&server, marked.id).await,
        "a refused EXPUNGE must not remove anything"
    );
}

#[tokio::test]
async fn examine_rejects_uid_expunge_without_removing_anything() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let marked = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("EXAMINE INBOX").await;
    let response = client
        .command(&format!("UID EXPUNGE {}", marked.uid.get()))
        .await;

    assert!(is_refused(&response), "{response}");
    assert!(exists(&server, marked.id).await);
}

#[tokio::test]
async fn expunge_without_a_selected_mailbox_is_refused() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let marked = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    let response = client.command("EXPUNGE").await;
    assert!(is_refused(&response), "{response}");
    assert!(exists(&server, marked.id).await);
}

#[tokio::test]
async fn expunge_before_authentication_is_refused() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("EXPUNGE").await;
    assert!(is_refused(&response), "{response}");
}

// --- malformed input -------------------------------------------------------

#[tokio::test]
async fn uid_expunge_without_a_set_is_bad() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let marked = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let response = client.command("UID EXPUNGE").await;

    assert!(is_refused(&response), "{response}");
    assert!(
        exists(&server, marked.id).await,
        "a malformed command must remove nothing"
    );
}

#[tokio::test]
async fn a_malformed_uid_set_removes_nothing() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let marked = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    for set in ["0", "abc", "1:2:3", "1,,2", "1,", ""] {
        let response = client.command(&format!("UID EXPUNGE {set}")).await;
        assert!(
            is_refused(&response),
            "`UID EXPUNGE {set}` must be refused: {response}"
        );
    }

    assert!(
        exists(&server, marked.id).await,
        "no malformed command may have removed the placement"
    );
}

#[tokio::test]
async fn the_whole_command_is_validated_before_anything_is_removed() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let second = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    // A set whose first member is valid and whose last is not: nothing may be
    // removed, not even the valid prefix.
    let response = client
        .command(&format!("UID EXPUNGE {},bogus", first.uid.get()))
        .await;

    assert!(is_refused(&response), "{response}");
    assert!(exists(&server, first.id).await, "the valid prefix too");
    assert!(exists(&server, second.id).await);
}

// --- placement isolation ---------------------------------------------------

#[tokio::test]
async fn expunge_removes_only_the_selected_mailboxs_placement() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let archive = server.mailbox(account, "Archive").await;

    let in_inbox = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let in_archive = server.deliver_with_flags(archive, SIMPLE, deleted()).await;

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;
    client.command("SELECT INBOX").await;
    let response = client.command("EXPUNGE").await;

    assert!(is_ok(&response), "{response}");
    assert!(!exists(&server, in_inbox.id).await);
    assert!(
        exists(&server, in_archive.id).await,
        "another mailbox's \\Deleted placement must be untouched"
    );
}

#[tokio::test]
async fn a_uid_expunge_cannot_reach_another_mailbox() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let archive = server.mailbox(account, "Archive").await;

    server.deliver(inbox, SIMPLE).await;
    // A fresh mailbox starts its own UID sequence, so this placement's UID
    // collides with one in INBOX — the case a mailbox-blind lookup gets wrong.
    let elsewhere = server.deliver_with_flags(archive, SIMPLE, deleted()).await;

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;
    client.command("SELECT INBOX").await;
    let response = client
        .command(&format!("UID EXPUNGE {}", elsewhere.uid.get()))
        .await;

    assert!(is_ok(&response), "{response}");
    assert!(
        exists(&server, elsewhere.id).await,
        "a UID resolves only within the selected mailbox"
    );
}

#[tokio::test]
async fn a_message_survives_while_another_placement_references_it() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let archive = server.mailbox(account, "Archive").await;

    // One message, two placements: the shared-message model (ADR-046).
    let entry = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    let copy = server
        .messages
        .copy_entry(entry.id, archive)
        .await
        .expect("copy the placement");

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;
    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    assert!(!exists(&server, entry.id).await, "the expunged placement");
    assert!(exists(&server, copy.id).await, "the surviving placement");
    assert!(
        server
            .messages
            .get(entry.message_id)
            .await
            .expect("query the message")
            .is_some(),
        "the message must survive while a placement references it"
    );
}

// --- orphan cleanup --------------------------------------------------------

#[tokio::test]
async fn the_message_is_collected_when_its_last_placement_goes() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let only = server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    assert!(
        server
            .messages
            .get(only.message_id)
            .await
            .expect("query the message")
            .is_none(),
        "a message no placement references must be collected"
    );
}

#[tokio::test]
async fn an_expunge_that_removes_nothing_collects_nothing() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let kept = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    assert!(
        server
            .messages
            .get(kept.message_id)
            .await
            .expect("query the message")
            .is_some()
    );
}

// --- events ----------------------------------------------------------------

#[tokio::test]
async fn a_removal_emits_entry_deleted() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let before = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryDeleted)
        .count();

    client.command("EXPUNGE").await;

    let after = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryDeleted)
        .count();
    assert_eq!(after, before + 1, "one removal, one EntryDeleted event");
}

#[tokio::test]
async fn collecting_the_last_placement_emits_message_deleted() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    client.command("EXPUNGE").await;

    assert!(
        server.events.kinds().contains(&EventKind::MessageDeleted),
        "the orphan sweep must report the collected message"
    );
}

#[tokio::test]
async fn each_removal_emits_exactly_one_event() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;
    server.deliver(inbox, SIMPLE).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let before = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryDeleted)
        .count();

    client.command("EXPUNGE").await;

    let after = server
        .events
        .kinds()
        .iter()
        .filter(|k| **k == EventKind::EntryDeleted)
        .count();
    assert_eq!(after, before + 2, "two removals, two events — not three");
}

#[tokio::test]
async fn a_refused_expunge_emits_no_event() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("EXAMINE INBOX").await;
    let before = server.events.kinds().len();

    let response = client.command("EXPUNGE").await;
    assert!(is_refused(&response), "{response}");

    assert_eq!(
        server.events.kinds().len(),
        before,
        "a failed operation must emit no false event"
    );
}

#[tokio::test]
async fn a_malformed_uid_expunge_emits_no_event() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver_with_flags(inbox, SIMPLE, deleted()).await;

    client.command("SELECT INBOX").await;
    let before = server.events.kinds().len();

    client.command("UID EXPUNGE bogus").await;

    assert_eq!(
        server.events.kinds().len(),
        before,
        "no event for a BAD command"
    );
}

#[tokio::test]
async fn an_expunge_with_nothing_deleted_emits_no_event() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    let before = server.events.kinds().len();

    client.command("EXPUNGE").await;

    assert_eq!(server.events.kinds().len(), before);
}

// --- the Odoo-relevant delete cycle ----------------------------------------

#[tokio::test]
async fn the_full_select_search_fetch_store_expunge_cycle() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;

    let first = server
        .deliver(
            inbox,
            b"From: a@example.test\r\nSubject: Keep\r\n\r\nOne.\r\n",
        )
        .await;
    let second = server
        .deliver(
            inbox,
            b"From: b@example.test\r\nSubject: Process\r\n\r\nTwo.\r\n",
        )
        .await;

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;

    // SELECT
    let selected = client.command("SELECT INBOX").await;
    assert!(selected.contains("* 2 EXISTS"), "{selected}");

    // SEARCH — the client discovers what is there, by UID.
    let searched = client.command("UID SEARCH ALL").await;
    let uids = search_numbers(&searched);
    assert_eq!(uids, vec![first.uid.get(), second.uid.get()], "{searched}");

    // FETCH — it reads the one it means to process.
    let fetched = client
        .command(&format!(
            "UID FETCH {} (BODY.PEEK[HEADER.FIELDS (SUBJECT)])",
            second.uid.get()
        ))
        .await;
    assert!(fetched.contains("Process"), "{fetched}");

    // STORE +FLAGS \Deleted — it marks that one, and only that one.
    let stored = client
        .command(&format!(
            "UID STORE {} +FLAGS (\\Deleted)",
            second.uid.get()
        ))
        .await;
    assert!(is_ok(&stored), "{stored}");

    // EXPUNGE — the mark is acted on.
    let expunged = client.command("EXPUNGE").await;
    assert_eq!(
        expunge_numbers(&expunged),
        vec![2],
        "the processed message sat at sequence 2: {expunged}"
    );

    // SEARCH again — the removed message is gone and the survivor renumbered.
    let searched = client.command("UID SEARCH ALL").await;
    assert_eq!(
        search_numbers(&searched),
        vec![first.uid.get()],
        "only the kept message remains: {searched}"
    );

    let searched = client.command("SEARCH ALL").await;
    assert_eq!(
        search_numbers(&searched),
        vec![1],
        "and it is now sequence 1: {searched}"
    );

    // FETCH again — the survivor is reachable at its new position and its
    // unchanged UID.
    let fetched = client.command("FETCH 1 (UID)").await;
    assert!(
        fetched.contains(&format!("UID {}", first.uid.get())),
        "{fetched}"
    );

    // And storage agrees with every one of those answers.
    assert!(exists(&server, first.id).await);
    assert!(!exists(&server, second.id).await);
}

#[tokio::test]
async fn the_delete_cycle_works_through_uid_expunge() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let keep = server.deliver(inbox, SIMPLE).await;
    let process = server.deliver(inbox, SIMPLE).await;

    client.command("SELECT INBOX").await;
    client
        .command(&format!(
            "UID STORE {} +FLAGS (\\Deleted)",
            process.uid.get()
        ))
        .await;
    let response = client
        .command(&format!("UID EXPUNGE {}", process.uid.get()))
        .await;

    assert_eq!(expunge_numbers(&response), vec![2], "{response}");
    assert!(exists(&server, keep.id).await);
    assert!(!exists(&server, process.id).await);

    let searched = client.command("UID SEARCH DELETED").await;
    assert!(
        search_numbers(&searched).is_empty(),
        "nothing is left marked \\Deleted: {searched}"
    );
}
