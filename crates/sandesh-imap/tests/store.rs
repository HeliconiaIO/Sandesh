//! `STORE` and `UID STORE` over a real socket (RFC 9051 §6.4.6, §6.4.8).
//!
//! Every test drives the production server over TCP against messages stored
//! through the production `MessageStore`. Crucially, the assertions about what
//! *changed* read the flags back **through storage**, not out of the response:
//! a server that reports the flags it intended to set but never wrote them
//! would pass a response-only test and fail these.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{Keyword, MailboxId, MessageFlags, MessageStore, SystemFlags};

/// A minimal well-formed message.
const SIMPLE: &[u8] = b"From: alice@example.test\r\n\
Subject: Store test\r\n\
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

/// Selects INBOX, asserting success.
async fn select_inbox(client: &mut common::Client<tokio::net::TcpStream>) {
    let response = client.command("SELECT INBOX").await;
    assert!(response.contains(" OK"), "SELECT failed: {response}");
}

/// The flags a placement actually has in storage.
async fn stored_flags(server: &TestServer, entry_id: sandesh_core::EntryId) -> MessageFlags {
    server
        .messages
        .get_entry(entry_id)
        .await
        .expect("query the entry")
        .expect("the entry exists")
        .flags
}

/// Whether a response contains a tagged `OK`.
fn is_ok(response: &str) -> bool {
    response
        .lines()
        .any(|line| line.contains(" OK ") && !line.starts_with('*'))
}

/// Whether a response contains a tagged `BAD` or `NO`.
fn is_refused(response: &str) -> bool {
    response
        .lines()
        .any(|line| (line.contains(" BAD ") || line.contains(" NO ")) && !line.starts_with('*'))
}

/// The untagged `FETCH` lines in a response.
fn fetch_lines(response: &str) -> Vec<&str> {
    response
        .lines()
        .filter(|line| line.starts_with("* ") && line.contains(" FETCH "))
        .collect()
}

/// A keyword, for building expectations.
fn keyword(value: &str) -> Keyword {
    Keyword::new(value).expect("a valid keyword")
}

// ---------------------------------------------------------------------------
// The three operations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn add_flags_sets_only_the_named_flags() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server
        .deliver_with_flags(inbox, SIMPLE, MessageFlags::from_system(SystemFlags::DRAFT))
        .await;
    select_inbox(&mut client).await;

    let response = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(is_ok(&response), "{response}");

    let flags = stored_flags(&server, entry.id).await;
    assert!(flags.has(SystemFlags::SEEN), "the named flag is added");
    assert!(
        flags.has(SystemFlags::DRAFT),
        "+FLAGS must leave existing flags alone"
    );
}

#[tokio::test]
async fn remove_flags_clears_only_the_named_flags() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server
        .deliver_with_flags(
            inbox,
            SIMPLE,
            MessageFlags::from_system(SystemFlags::SEEN.union(SystemFlags::FLAGGED)),
        )
        .await;
    select_inbox(&mut client).await;

    let response = client.command(r"STORE 1 -FLAGS (\Seen)").await;
    assert!(is_ok(&response), "{response}");

    let flags = stored_flags(&server, entry.id).await;
    assert!(!flags.has(SystemFlags::SEEN), "the named flag is removed");
    assert!(
        flags.has(SystemFlags::FLAGGED),
        "-FLAGS must leave other flags alone"
    );
}

#[tokio::test]
async fn replace_flags_discards_everything_not_named() {
    // The distinction that matters: FLAGS is not +FLAGS.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let mut initial = MessageFlags::from_system(SystemFlags::SEEN.union(SystemFlags::FLAGGED));
    initial.add(SystemFlags::NONE, [keyword("Existing")]);
    let entry = server.deliver_with_flags(inbox, SIMPLE, initial).await;
    select_inbox(&mut client).await;

    let response = client.command(r"STORE 1 FLAGS (\Draft)").await;
    assert!(is_ok(&response), "{response}");

    let flags = stored_flags(&server, entry.id).await;
    assert!(flags.has(SystemFlags::DRAFT), "the named flag is set");
    assert!(!flags.has(SystemFlags::SEEN), "unnamed flags are discarded");
    assert!(
        !flags.has(SystemFlags::FLAGGED),
        "unnamed flags are discarded"
    );
    assert!(
        flags.keywords().is_empty(),
        "FLAGS replaces keywords too: {:?}",
        flags.keywords()
    );
}

#[tokio::test]
async fn replace_with_an_empty_list_clears_every_flag() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let mut initial = MessageFlags::from_system(SystemFlags::ALL);
    initial.add(SystemFlags::NONE, [keyword("Label")]);
    let entry = server.deliver_with_flags(inbox, SIMPLE, initial).await;
    select_inbox(&mut client).await;

    let response = client.command("STORE 1 FLAGS ()").await;
    assert!(is_ok(&response), "{response}");

    let flags = stored_flags(&server, entry.id).await;
    assert_eq!(flags.system(), SystemFlags::NONE, "everything cleared");
    assert!(flags.keywords().is_empty());
}

#[tokio::test]
async fn the_three_operations_are_not_interchangeable() {
    // Runs all three against identical starting state and asserts the results
    // differ in the way the RFC requires. This is the mutation guard against
    // add/remove/replace being wired to the wrong FlagUpdate.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let initial = MessageFlags::from_system(SystemFlags::SEEN.union(SystemFlags::FLAGGED));
    let a = server
        .deliver_with_flags(inbox, SIMPLE, initial.clone())
        .await;
    let b = server
        .deliver_with_flags(inbox, SIMPLE, initial.clone())
        .await;
    let c = server.deliver_with_flags(inbox, SIMPLE, initial).await;
    select_inbox(&mut client).await;

    client.command(r"STORE 1 +FLAGS (\Draft)").await;
    client.command(r"STORE 2 -FLAGS (\Seen)").await;
    client.command(r"STORE 3 FLAGS (\Draft)").await;

    let added = stored_flags(&server, a.id).await;
    let removed = stored_flags(&server, b.id).await;
    let replaced = stored_flags(&server, c.id).await;

    // +FLAGS: gained Draft, kept Seen and Flagged.
    assert!(added.has(SystemFlags::DRAFT) && added.has(SystemFlags::SEEN));
    // -FLAGS: lost Seen, kept Flagged, gained nothing.
    assert!(!removed.has(SystemFlags::SEEN) && removed.has(SystemFlags::FLAGGED));
    assert!(!removed.has(SystemFlags::DRAFT));
    // FLAGS: exactly Draft.
    assert_eq!(replaced.system(), SystemFlags::DRAFT);

    assert_ne!(added.system(), removed.system());
    assert_ne!(added.system(), replaced.system());
    assert_ne!(removed.system(), replaced.system());
}

// ---------------------------------------------------------------------------
// Keywords
// ---------------------------------------------------------------------------

#[tokio::test]
async fn keywords_can_be_added_and_removed() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let response = client.command(r"STORE 1 +FLAGS ($Junk Urgent)").await;
    assert!(is_ok(&response), "{response}");

    let flags = stored_flags(&server, entry.id).await;
    assert!(
        flags.has_keyword(&keyword("$Junk")),
        "{:?}",
        flags.keywords()
    );
    assert!(flags.has_keyword(&keyword("Urgent")));

    let response = client.command(r"STORE 1 -FLAGS ($Junk)").await;
    assert!(is_ok(&response), "{response}");

    let flags = stored_flags(&server, entry.id).await;
    assert!(!flags.has_keyword(&keyword("$Junk")), "removed");
    assert!(flags.has_keyword(&keyword("Urgent")), "the other survives");
}

#[tokio::test]
async fn system_flags_and_keywords_are_set_together() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    client.command(r"STORE 1 FLAGS (\Seen $Forwarded)").await;

    let flags = stored_flags(&server, entry.id).await;
    assert!(flags.has(SystemFlags::SEEN));
    assert!(flags.has_keyword(&keyword("$Forwarded")));
}

// ---------------------------------------------------------------------------
// SILENT
// ---------------------------------------------------------------------------

#[tokio::test]
async fn silent_changes_the_flags_but_sends_no_untagged_fetch() {
    // Both halves matter: suppressing the response must not suppress the
    // mutation, and this is the test that catches it if it does.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let response = client.command(r"STORE 1 +FLAGS.SILENT (\Seen)").await;
    assert!(is_ok(&response), "{response}");
    assert!(
        fetch_lines(&response).is_empty(),
        "SILENT must send no untagged FETCH: {response}"
    );

    let flags = stored_flags(&server, entry.id).await;
    assert!(
        flags.has(SystemFlags::SEEN),
        "SILENT still changes the flags"
    );
}

#[tokio::test]
async fn the_non_silent_form_reports_the_resulting_flags() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let response = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(is_ok(&response), "{response}");

    let lines = fetch_lines(&response);
    assert_eq!(lines.len(), 1, "one message, one report: {response}");
    assert!(lines[0].starts_with("* 1 FETCH "), "{}", lines[0]);
    assert!(lines[0].contains("FLAGS "), "{}", lines[0]);
    assert!(
        lines[0].contains("\\Seen"),
        "the *resulting* flags: {}",
        lines[0]
    );
}

#[tokio::test]
async fn every_silent_variant_suppresses_the_response_and_still_mutates() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let initial = MessageFlags::from_system(SystemFlags::FLAGGED);
    let a = server
        .deliver_with_flags(inbox, SIMPLE, initial.clone())
        .await;
    let b = server
        .deliver_with_flags(inbox, SIMPLE, initial.clone())
        .await;
    let c = server.deliver_with_flags(inbox, SIMPLE, initial).await;
    select_inbox(&mut client).await;

    for (sequence, command, entry_id, check) in [
        (1, r"+FLAGS.SILENT (\Seen)", a.id, "add"),
        (2, r"-FLAGS.SILENT (\Flagged)", b.id, "remove"),
        (3, r"FLAGS.SILENT (\Draft)", c.id, "replace"),
    ] {
        let response = client.command(&format!("STORE {sequence} {command}")).await;
        assert!(is_ok(&response), "{command}: {response}");
        assert!(
            fetch_lines(&response).is_empty(),
            "{command} must be silent: {response}"
        );

        let flags = stored_flags(&server, entry_id).await;
        match check {
            "add" => assert!(flags.has(SystemFlags::SEEN), "{command} did not mutate"),
            "remove" => assert!(!flags.has(SystemFlags::FLAGGED), "{command} did not mutate"),
            _ => assert_eq!(
                flags.system(),
                SystemFlags::DRAFT,
                "{command} did not mutate"
            ),
        }
    }
}

#[tokio::test]
async fn silent_and_non_silent_reach_the_same_stored_state() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let loud = server.deliver(inbox, SIMPLE).await;
    let quiet = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    client.command(r"STORE 1 +FLAGS (\Seen \Flagged)").await;
    client
        .command(r"STORE 2 +FLAGS.SILENT (\Seen \Flagged)")
        .await;

    let a = stored_flags(&server, loud.id).await;
    let b = stored_flags(&server, quiet.id).await;
    assert_eq!(
        a.all_names(),
        b.all_names(),
        "silence changes the report, not the result"
    );
}

// ---------------------------------------------------------------------------
// Sequence numbers versus UIDs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_resolves_sequence_numbers_not_uids() {
    // The core mutation guard: `STORE 1` must hit the *first* message, even
    // once UIDs and positions disagree.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server.deliver(inbox, SIMPLE).await;
    let second = server.deliver(inbox, SIMPLE).await;
    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("delete the first placement");

    select_inbox(&mut client).await;
    // Now sequence 1 is the message whose UID is `second.uid` (> 1).
    let response = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(is_ok(&response), "{response}");

    let flags = stored_flags(&server, second.id).await;
    assert!(
        flags.has(SystemFlags::SEEN),
        "sequence 1 is the surviving message"
    );
    assert_ne!(second.uid.get(), 1, "UID and position really do differ");
}

#[tokio::test]
async fn uid_store_resolves_uids_not_positions() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server.deliver(inbox, SIMPLE).await;
    let second = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    // Address the second message by its UID.
    let response = client
        .command(&format!(r"UID STORE {} +FLAGS (\Seen)", second.uid.get()))
        .await;
    assert!(is_ok(&response), "{response}");

    assert!(
        stored_flags(&server, second.id)
            .await
            .has(SystemFlags::SEEN),
        "the UID-named message changed"
    );
    assert!(
        !stored_flags(&server, first.id).await.has(SystemFlags::SEEN),
        "the other message did not"
    );
}

#[tokio::test]
async fn uid_store_and_store_disagree_when_uids_are_shifted() {
    // `STORE 2` and `UID STORE 2` must name different messages once a UID gap
    // exists — the sharpest form of the sequence/UID confusion check.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let a = server.deliver(inbox, SIMPLE).await; // UID 1
    let b = server.deliver(inbox, SIMPLE).await; // UID 2
    let c = server.deliver(inbox, SIMPLE).await; // UID 3
    server.messages.delete_entry(a.id).await.expect("delete");
    // Remaining: UID 2 at sequence 1, UID 3 at sequence 2.

    select_inbox(&mut client).await;

    client.command(r"STORE 2 +FLAGS (\Seen)").await; // → UID 3
    client.command(r"UID STORE 2 +FLAGS (\Flagged)").await; // → UID 2

    let b_flags = stored_flags(&server, b.id).await;
    let c_flags = stored_flags(&server, c.id).await;

    assert!(
        c_flags.has(SystemFlags::SEEN),
        "STORE 2 is sequence 2 = UID 3"
    );
    assert!(!c_flags.has(SystemFlags::FLAGGED));
    assert!(b_flags.has(SystemFlags::FLAGGED), "UID STORE 2 is UID 2");
    assert!(!b_flags.has(SystemFlags::SEEN));
}

#[tokio::test]
async fn uid_store_reports_the_uid_in_its_untagged_fetch() {
    // RFC 9051 §6.4.8: a UID command's responses carry the UID.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let response = client
        .command(&format!(r"UID STORE {} +FLAGS (\Seen)", entry.uid.get()))
        .await;
    let lines = fetch_lines(&response);
    assert_eq!(lines.len(), 1, "{response}");
    assert!(
        lines[0].contains(&format!("UID {}", entry.uid.get())),
        "UID STORE must report the UID: {}",
        lines[0]
    );
    assert!(lines[0].contains("FLAGS "), "{}", lines[0]);
}

#[tokio::test]
async fn a_plain_store_response_carries_no_uid() {
    // Symmetry check: the UID is added because the command was UID-mode, not
    // unconditionally.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let response = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    let lines = fetch_lines(&response);
    assert_eq!(lines.len(), 1, "{response}");
    assert!(
        !lines[0].contains("UID "),
        "a sequence-mode STORE need not report a UID: {}",
        lines[0]
    );
}

// ---------------------------------------------------------------------------
// Sequence sets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_range_stores_every_message_in_it() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entries: Vec<_> = {
        let mut v = Vec::new();
        for _ in 0..5 {
            v.push(server.deliver(inbox, SIMPLE).await);
        }
        v
    };
    select_inbox(&mut client).await;

    let response = client.command(r"STORE 2:4 +FLAGS (\Seen)").await;
    assert!(is_ok(&response), "{response}");
    assert_eq!(fetch_lines(&response).len(), 3, "three reports: {response}");

    for (index, entry) in entries.iter().enumerate() {
        let seen = stored_flags(&server, entry.id).await.has(SystemFlags::SEEN);
        let expected = (1..=3).contains(&index); // sequences 2,3,4 → indices 1,2,3
        assert_eq!(seen, expected, "message at index {index}");
    }
}

#[tokio::test]
async fn star_ranges_and_duplicates_behave() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..3 {
        server.deliver(inbox, SIMPLE).await;
    }
    select_inbox(&mut client).await;

    // `*` is the last message.
    let response = client.command(r"STORE * +FLAGS (\Seen)").await;
    assert_eq!(fetch_lines(&response).len(), 1, "{response}");
    assert!(fetch_lines(&response)[0].starts_with("* 3 FETCH "));

    // Duplicates and overlaps collapse to one report each.
    let response = client.command(r"STORE 1,1,1:2 +FLAGS (\Flagged)").await;
    let lines = fetch_lines(&response);
    assert_eq!(lines.len(), 2, "each message reported once: {response}");

    // `1:*` covers everything.
    let response = client.command(r"STORE 1:* +FLAGS (\Draft)").await;
    assert_eq!(fetch_lines(&response).len(), 3, "{response}");
}

#[tokio::test]
async fn uid_store_spans_uid_gaps() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let a = server.deliver(inbox, SIMPLE).await;
    let b = server.deliver(inbox, SIMPLE).await;
    let c = server.deliver(inbox, SIMPLE).await;
    server.messages.delete_entry(b.id).await.expect("delete");
    select_inbox(&mut client).await;

    let response = client
        .command(&format!(
            r"UID STORE {}:{} +FLAGS (\Seen)",
            a.uid.get(),
            c.uid.get()
        ))
        .await;
    assert!(is_ok(&response), "{response}");
    assert_eq!(
        fetch_lines(&response).len(),
        2,
        "the gap contributes nothing: {response}"
    );

    assert!(stored_flags(&server, a.id).await.has(SystemFlags::SEEN));
    assert!(stored_flags(&server, c.id).await.has(SystemFlags::SEEN));
}

#[tokio::test]
async fn an_out_of_range_set_succeeds_and_changes_nothing() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    for set in ["99", "50:60", "99999"] {
        let response = client
            .command(&format!(r"STORE {set} +FLAGS (\Seen)"))
            .await;
        assert!(is_ok(&response), "{set} must succeed: {response}");
        assert!(
            fetch_lines(&response).is_empty(),
            "{set} matched nothing: {response}"
        );
    }
    assert!(
        !stored_flags(&server, entry.id).await.has(SystemFlags::SEEN),
        "nothing may have changed"
    );
}

#[tokio::test]
async fn a_uid_that_does_not_exist_changes_nothing() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let response = client.command(r"UID STORE 99999 +FLAGS (\Seen)").await;
    assert!(is_ok(&response), "{response}");
    assert!(fetch_lines(&response).is_empty());
    assert!(!stored_flags(&server, entry.id).await.has(SystemFlags::SEEN));
}

// ---------------------------------------------------------------------------
// Read-only protection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_on_an_examined_mailbox_is_refused_without_mutating() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;

    let examine = client.command("EXAMINE INBOX").await;
    assert!(examine.contains("[READ-ONLY]"), "{examine}");

    for command in [
        r"STORE 1 +FLAGS (\Seen)",
        r"STORE 1 -FLAGS (\Seen)",
        r"STORE 1 FLAGS (\Seen)",
        r"STORE 1 +FLAGS.SILENT (\Seen)",
        r"UID STORE 1 +FLAGS (\Seen)",
    ] {
        let response = client.command(command).await;
        assert!(
            is_refused(&response),
            "{command} must be refused: {response}"
        );
        assert!(
            fetch_lines(&response).is_empty(),
            "{command} must report nothing: {response}"
        );

        let flags = stored_flags(&server, entry.id).await;
        assert_eq!(
            flags.system(),
            SystemFlags::NONE,
            "{command} must not have mutated storage"
        );
    }
}

#[tokio::test]
async fn store_before_select_is_refused() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    // Authenticated, nothing selected.

    let response = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(is_refused(&response), "{response}");
    assert_eq!(
        stored_flags(&server, entry.id).await.system(),
        SystemFlags::NONE
    );
}

#[tokio::test]
async fn store_before_authentication_is_refused() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(is_refused(&response), "{response}");
}

// ---------------------------------------------------------------------------
// Mailbox placement isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_affects_only_the_selected_mailboxs_placement() {
    // The same message in two mailboxes has two placements with independent
    // flags. A STORE in one must not touch the other.
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let archive = server.mailbox(account, "Archive").await;

    let in_inbox = server.deliver(inbox, SIMPLE).await;
    let in_archive = server
        .messages
        .copy_entry(in_inbox.id, archive)
        .await
        .expect("copy the placement");
    assert_eq!(
        in_inbox.message_id, in_archive.message_id,
        "one message, two placements"
    );

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;
    select_inbox(&mut client).await;

    let response = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(is_ok(&response), "{response}");

    assert!(
        stored_flags(&server, in_inbox.id)
            .await
            .has(SystemFlags::SEEN),
        "the INBOX placement changed"
    );
    assert!(
        !stored_flags(&server, in_archive.id)
            .await
            .has(SystemFlags::SEEN),
        "the Archive placement must be untouched"
    );
}

#[tokio::test]
async fn uid_store_cannot_reach_another_mailboxs_placement() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let archive = server.mailbox(account, "Archive").await;

    // Put a message only in Archive, and one in INBOX.
    let hidden = server.deliver(archive, SIMPLE).await;
    let visible = server.deliver(inbox, SIMPLE).await;

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;
    select_inbox(&mut client).await;

    // Sweep every UID: only the INBOX placement may change.
    let response = client.command(r"UID STORE 1:* +FLAGS (\Deleted)").await;
    assert!(is_ok(&response), "{response}");

    assert!(
        stored_flags(&server, visible.id)
            .await
            .has(SystemFlags::DELETED),
        "the selected mailbox's placement changed"
    );
    assert!(
        !stored_flags(&server, hidden.id)
            .await
            .has(SystemFlags::DELETED),
        "an Archive placement must be unreachable from an INBOX selection"
    );
}

// ---------------------------------------------------------------------------
// Same-session freshness (ADR-047)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fetch_after_a_store_sees_the_new_flags() {
    // ADR-047: the same session must not report the flags it captured at
    // SELECT after it has changed them itself.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    client.command(r"STORE 1 +FLAGS (\Seen \Flagged)").await;

    let response = client.command("FETCH 1 FLAGS").await;
    assert!(response.contains("\\Seen"), "stale snapshot: {response}");
    assert!(response.contains("\\Flagged"), "stale snapshot: {response}");
}

#[tokio::test]
async fn a_fetch_after_a_silent_store_also_sees_the_new_flags() {
    // The silent path updates the snapshot too — silence suppresses the
    // report, not the bookkeeping.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    client.command(r"STORE 1 +FLAGS.SILENT (\Seen)").await;

    let response = client.command("FETCH 1 FLAGS").await;
    assert!(
        response.contains("\\Seen"),
        "stale after SILENT: {response}"
    );
}

#[tokio::test]
async fn a_fetch_after_a_removal_no_longer_reports_the_flag() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver_with_flags(inbox, SIMPLE, MessageFlags::from_system(SystemFlags::SEEN))
        .await;
    select_inbox(&mut client).await;

    let before = client.command("FETCH 1 FLAGS").await;
    assert!(before.contains("\\Seen"), "{before}");

    client.command(r"STORE 1 -FLAGS (\Seen)").await;

    let after = client.command("FETCH 1 FLAGS").await;
    assert!(
        !after.contains("\\Seen"),
        "the snapshot must reflect the removal: {after}"
    );
}

#[tokio::test]
async fn uid_fetch_after_uid_store_sees_the_new_flags() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    client
        .command(&format!(r"UID STORE {} +FLAGS ($Junk)", entry.uid.get()))
        .await;

    let response = client
        .command(&format!("UID FETCH {} FLAGS", entry.uid.get()))
        .await;
    assert!(response.contains("$Junk"), "stale snapshot: {response}");
}

#[tokio::test]
async fn repeated_stores_accumulate_correctly_in_one_session() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    client.command(r"STORE 1 +FLAGS (\Seen)").await;
    client.command(r"STORE 1 +FLAGS (\Flagged)").await;
    client.command(r"STORE 1 -FLAGS (\Seen)").await;

    let response = client.command("FETCH 1 FLAGS").await;
    assert!(!response.contains("\\Seen"), "{response}");
    assert!(response.contains("\\Flagged"), "{response}");

    // And storage agrees with what the session reported.
    let flags = stored_flags(&server, entry.id).await;
    assert!(!flags.has(SystemFlags::SEEN));
    assert!(flags.has(SystemFlags::FLAGGED));
}

// ---------------------------------------------------------------------------
// Cross-session semantics (ADR-046 preserved)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn another_sessions_store_is_not_visible_until_reselect() {
    // ADR-046 is deliberately unchanged: only the *acting* session's snapshot
    // is refreshed. This pins that so a future change is a decision.
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server.deliver(inbox, SIMPLE).await;

    let mut a = server.connect().await;
    a.command("LOGIN alice any").await;
    select_inbox(&mut a).await;

    let mut b = server.connect().await;
    b.command("LOGIN alice any").await;
    select_inbox(&mut b).await;

    // B changes the flags.
    let response = b.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(is_ok(&response), "{response}");

    // A's snapshot predates that and is not refreshed.
    let stale = a.command("FETCH 1 FLAGS").await;
    assert!(
        !stale.contains("\\Seen"),
        "cross-session snapshots stay as they were (ADR-046): {stale}"
    );

    // Re-selecting takes a fresh snapshot.
    select_inbox(&mut a).await;
    let fresh = a.command("FETCH 1 FLAGS").await;
    assert!(fresh.contains("\\Seen"), "reselect refreshes: {fresh}");
}

#[tokio::test]
async fn two_sessions_storing_different_flags_both_persist() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let entry = server.deliver(inbox, SIMPLE).await;

    let mut a = server.connect().await;
    a.command("LOGIN alice any").await;
    select_inbox(&mut a).await;

    let mut b = server.connect().await;
    b.command("LOGIN alice any").await;
    select_inbox(&mut b).await;

    a.command(r"STORE 1 +FLAGS (\Seen)").await;
    b.command(r"STORE 1 +FLAGS (\Flagged)").await;

    // Both additive changes survive: each is a transaction over the same row.
    let flags = stored_flags(&server, entry.id).await;
    assert!(flags.has(SystemFlags::SEEN), "{:?}", flags.all_names());
    assert!(flags.has(SystemFlags::FLAGGED), "{:?}", flags.all_names());
}

#[tokio::test]
async fn a_store_on_a_concurrently_removed_entry_does_not_fail_the_command() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server.deliver(inbox, SIMPLE).await;
    let second = server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("concurrent delete");

    let response = client.command(r"STORE 1:2 +FLAGS (\Seen)").await;
    assert!(
        is_ok(&response),
        "a vanished entry must not fail the command: {response}"
    );
    assert!(
        stored_flags(&server, second.id)
            .await
            .has(SystemFlags::SEEN),
        "the surviving message still changed"
    );
}

// ---------------------------------------------------------------------------
// Malformed input
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_malformed_store_is_rejected_without_mutating() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server
        .deliver_with_flags(inbox, SIMPLE, MessageFlags::from_system(SystemFlags::DRAFT))
        .await;
    select_inbox(&mut client).await;

    for command in [
        "STORE",
        "STORE 1",
        r"STORE 1 +FLAGS",
        r"STORE 0 +FLAGS (\Seen)",
        r"STORE abc +FLAGS (\Seen)",
        r"STORE 1:0 +FLAGS (\Seen)",
        r"STORE 1,,2 +FLAGS (\Seen)",
        r"STORE 1 NOTFLAGS (\Seen)",
        r"STORE 1 +FLAGS (\Recent)",
        r"STORE 1 +FLAGS (\*)",
        r"STORE 1 +FLAGS (\Nonsense)",
        r"STORE 1 +FLAGS (\Seen) extra",
        r"UID STORE 0 +FLAGS (\Seen)",
    ] {
        let response = client.command(command).await;
        assert!(
            is_refused(&response),
            "{command} must be refused: {response}"
        );
        assert!(
            fetch_lines(&response).is_empty(),
            "{command} must report nothing: {response}"
        );

        // The starting state is exactly preserved: Draft only.
        let flags = stored_flags(&server, entry.id).await;
        assert_eq!(
            flags.system(),
            SystemFlags::DRAFT,
            "{command} mutated storage"
        );
        assert!(flags.keywords().is_empty(), "{command} added a keyword");
    }

    // And the session is still usable.
    let response = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(is_ok(&response), "the session must survive: {response}");
}

#[tokio::test]
async fn a_hostile_store_never_panics() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    for hostile in [
        r"STORE 1 +FLAGS ((((",
        r"STORE 1 FLAGS.SILENT.SILENT (\Seen)",
        r"STORE 4294967295 +FLAGS (\Seen)",
        r"STORE 1:* +FLAGS (\Seen \Seen \Seen \Seen)",
        &format!(r"STORE 1 +FLAGS ({})", "kw ".repeat(500)),
        &format!("STORE {} +FLAGS (\\Seen)", "1,".repeat(2000)),
    ] {
        let response = client.command(hostile).await;
        assert!(!response.is_empty(), "{hostile} produced no response");
        assert!(
            response.contains(" OK") || response.contains(" BAD") || response.contains(" NO"),
            "{hostile} => {response}"
        );
    }

    let response = client.command("FETCH 1 FLAGS").await;
    assert!(is_ok(&response), "the session must survive");
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// The `EntryFlagsChanged` events emitted so far, as `(added, removed, now)`.
fn flag_change_events(server: &TestServer) -> Vec<(Vec<String>, Vec<String>, Vec<String>)> {
    server
        .events
        .events()
        .into_iter()
        .filter_map(|event| match event.payload {
            sandesh_core::EventPayload::EntryFlagsChanged {
                added,
                removed,
                flags_now,
                ..
            } => Some((added, removed, flags_now)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_store_emits_entry_flags_changed() {
    // The event architecture is the storage layer's, reached through the port.
    // This asserts the IMAP path actually goes through it rather than around.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let before = flag_change_events(&server).len();
    let response = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(is_ok(&response), "{response}");

    let events = flag_change_events(&server);
    assert_eq!(
        events.len(),
        before + 1,
        "exactly one flag-change event: {events:?}"
    );
    let (added, removed, now) = events.last().expect("an event");
    assert_eq!(added, &vec![String::from(r"\Seen")], "added");
    assert!(removed.is_empty(), "nothing removed: {removed:?}");
    assert_eq!(now, &vec![String::from(r"\Seen")], "resulting flags");
}

#[tokio::test]
async fn a_silent_store_still_emits_its_event() {
    // Silence is an IMAP response concern; the domain event is unaffected.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let before = flag_change_events(&server).len();
    client.command(r"STORE 1 +FLAGS.SILENT (\Flagged)").await;

    let events = flag_change_events(&server);
    assert_eq!(events.len(), before + 1, "SILENT must still emit");
    assert_eq!(
        events.last().map(|e| e.0.clone()),
        Some(vec![String::from(r"\Flagged")])
    );
}

#[tokio::test]
async fn a_removal_is_reported_as_removed_in_the_event() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver_with_flags(inbox, SIMPLE, MessageFlags::from_system(SystemFlags::SEEN))
        .await;
    select_inbox(&mut client).await;

    let before = flag_change_events(&server).len();
    client.command(r"STORE 1 -FLAGS (\Seen)").await;

    let events = flag_change_events(&server);
    assert_eq!(events.len(), before + 1);
    let (added, removed, now) = events.last().expect("an event");
    assert!(added.is_empty(), "nothing added: {added:?}");
    assert_eq!(removed, &vec![String::from(r"\Seen")], "removed");
    assert!(now.is_empty(), "no flags remain: {now:?}");
}

#[tokio::test]
async fn a_store_over_several_messages_emits_one_event_each() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..3 {
        server.deliver(inbox, SIMPLE).await;
    }
    select_inbox(&mut client).await;

    let before = flag_change_events(&server).len();
    client.command(r"STORE 1:3 +FLAGS (\Seen)").await;

    assert_eq!(
        flag_change_events(&server).len(),
        before + 3,
        "one event per placement changed"
    );
}

#[tokio::test]
async fn a_rejected_store_emits_no_event() {
    // No mutation, no event — the check that a refusal is total.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    select_inbox(&mut client).await;

    let before = flag_change_events(&server).len();
    for command in [
        r"STORE 0 +FLAGS (\Seen)",
        r"STORE 1 NOTFLAGS (\Seen)",
        r"STORE 1 +FLAGS (\Recent)",
        r"STORE 99 +FLAGS (\Seen)",
    ] {
        client.command(command).await;
    }
    assert_eq!(
        flag_change_events(&server).len(),
        before,
        "a refused or empty STORE changes nothing and emits nothing"
    );
}
