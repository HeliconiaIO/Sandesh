//! CONDSTORE over a real socket (RFC 7162 §3).
//!
//! These are the raw-protocol counterparts to the Python `imaplib` tests in
//! `real_client.rs`. The split is deliberate and follows what a real client can
//! actually express: `imaplib` has no CONDSTORE API, so it can send the
//! commands as raw strings and parse the responses, but it cannot exercise
//! `EXAMINE`'s read-only refusal, per-session `ENABLE` gating, or the exact
//! wire spelling of a `MODIFIED` response code. Those are asserted here, byte
//! for byte, against the production server over TCP.
//!
//! As in `store.rs`, assertions about what *changed* read back through storage
//! rather than trusting the response: a server that reports a modseq it never
//! wrote would pass a response-only test and fail these.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{EntryId, MailboxId, MailboxStore, MessageStore, ModSeq};

/// A minimal well-formed message.
const SIMPLE: &[u8] = b"From: alice@example.test\r\n\
Subject: Condstore test\r\n\
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

/// The modification sequence a placement actually has in storage.
async fn stored_mod_seq(server: &TestServer, entry_id: EntryId) -> ModSeq {
    server
        .messages
        .get_entry(entry_id)
        .await
        .expect("query entry")
        .expect("entry exists")
        .mod_seq
}

/// A mailbox's stored high-water mark.
async fn stored_highest(server: &TestServer, mailbox: MailboxId) -> ModSeq {
    server
        .mailboxes
        .counts(mailbox)
        .await
        .expect("counts")
        .highest_mod_seq
}

// ------------------------------------------------------------------ HIGHESTMODSEQ

#[tokio::test]
async fn select_reports_highestmodseq() {
    // RFC 7162 §3.1.1. Sent unconditionally, because it is an `OK` response
    // code and RFC 9051 §7.1 requires a client to ignore codes it does not
    // know — so this cannot break an older client, while withholding it would
    // force a CONDSTORE client into a needless STATUS round-trip.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;

    let response = client.command("SELECT INBOX").await;

    let expected = stored_highest(&server, inbox).await;
    assert!(
        response.contains(&format!("[HIGHESTMODSEQ {}]", expected.get())),
        "SELECT must report the mailbox's own high-water mark: {response}"
    );
}

#[tokio::test]
async fn examine_reports_highestmodseq_too() {
    // RFC 9051 §6.3.3 defines EXAMINE as "identical to SELECT except" for
    // read-only-ness, so the synchronisation data must be identical.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;

    let response = client.command("EXAMINE INBOX").await;

    assert!(response.contains("[READ-ONLY]"), "{response}");
    assert!(
        response.contains("HIGHESTMODSEQ"),
        "EXAMINE must report HIGHESTMODSEQ as SELECT does: {response}"
    );
}

#[tokio::test]
async fn status_reports_highestmodseq() {
    // RFC 7162 §3.1.2.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;

    let response = client.command("STATUS INBOX (HIGHESTMODSEQ)").await;

    let expected = stored_highest(&server, inbox).await;
    assert!(
        response.contains(&format!("HIGHESTMODSEQ {}", expected.get())),
        "STATUS must report the high-water mark: {response}"
    );
}

#[tokio::test]
async fn highestmodseq_advances_when_flags_change() {
    // The value must track real changes, or a client polling STATUS would
    // never learn there was anything to fetch.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let before = stored_highest(&server, inbox).await;
    let stored = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(stored.contains(" OK"), "{stored}");
    let after = stored_highest(&server, inbox).await;

    assert!(
        after > before,
        "a flag change must advance HIGHESTMODSEQ: {before} -> {after}"
    );

    let status = client.command("STATUS INBOX (HIGHESTMODSEQ)").await;
    assert!(
        status.contains(&format!("HIGHESTMODSEQ {}", after.get())),
        "STATUS must report the advanced value: {status}"
    );
}

// -------------------------------------------------------------- FETCH MODSEQ

#[tokio::test]
async fn fetch_modseq_matches_storage() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let response = client.command("FETCH 1 (MODSEQ)").await;

    let expected = stored_mod_seq(&server, entry.id).await;
    assert!(
        response.contains(&format!("MODSEQ ({})", expected.get())),
        "MODSEQ must be a parenthesised value matching storage: {response}"
    );
}

#[tokio::test]
async fn fetch_modseq_reflects_this_sessions_own_store() {
    // The snapshot-consistency rule. A session that stores flags and then
    // fetches MODSEQ must see the value its own change produced — reporting
    // the pre-change value would tell the client its store did not happen.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let before = client.command("FETCH 1 (MODSEQ)").await;
    client.command(r"STORE 1 +FLAGS (\Seen)").await;
    let after = client.command("FETCH 1 (MODSEQ)").await;

    let stored = stored_mod_seq(&server, entry.id).await;
    assert_ne!(before, after, "the reported modseq must change: {after}");
    assert!(
        after.contains(&format!("MODSEQ ({})", stored.get())),
        "the session's own view must match storage: {after}"
    );
}

#[tokio::test]
async fn changedsince_filters_and_implies_modseq() {
    // RFC 7162 §3.1.4 and §3.1.4.1: the filter selects messages, and the
    // implied MODSEQ lets the client advance the bound it filters on.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..3 {
        server.deliver(inbox, SIMPLE).await;
    }
    client.command("SELECT INBOX").await;

    // Everything is at or below the mark, so nothing is newer than it.
    let mark = stored_highest(&server, inbox).await;
    let none = client
        .command(&format!("FETCH 1:* (FLAGS) (CHANGEDSINCE {})", mark.get()))
        .await;
    assert!(none.contains(" OK"), "{none}");
    assert!(
        !none.contains("* 1 FETCH"),
        "nothing changed after the mark, so nothing may be reported: {none}"
    );

    // Change one message; only it may come back.
    client.command(r"STORE 2 +FLAGS (\Seen)").await;
    let some = client
        .command(&format!("FETCH 1:* (FLAGS) (CHANGEDSINCE {})", mark.get()))
        .await;

    assert!(
        some.contains("* 2 FETCH"),
        "the changed message must be reported: {some}"
    );
    assert!(
        !some.contains("* 1 FETCH") && !some.contains("* 3 FETCH"),
        "unchanged messages must not be reported: {some}"
    );
    assert!(
        some.contains("MODSEQ"),
        "CHANGEDSINCE must imply MODSEQ (§3.1.4.1): {some}"
    );
}

#[tokio::test]
async fn changedsince_does_not_duplicate_an_explicit_modseq() {
    // The implied item must not be appended when the client already asked.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;
    client.command(r"STORE 1 +FLAGS (\Seen)").await;

    let response = client.command("FETCH 1:* (MODSEQ) (CHANGEDSINCE 1)").await;

    let line = response
        .lines()
        .find(|line| line.contains("FETCH ("))
        .unwrap_or_default();
    assert_eq!(
        line.matches("MODSEQ").count(),
        1,
        "MODSEQ must appear exactly once: {line}"
    );
}

// ------------------------------------------------------- STORE UNCHANGEDSINCE

#[tokio::test]
async fn conditional_store_succeeds_when_nothing_changed() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let current = stored_mod_seq(&server, entry.id).await;
    let response = client
        .command(&format!(
            r"STORE 1 (UNCHANGEDSINCE {}) +FLAGS (\Seen)",
            current.get()
        ))
        .await;

    assert!(response.contains(" OK"), "{response}");
    assert!(
        !response.contains("MODIFIED"),
        "a successful conditional store must not report MODIFIED: {response}"
    );
    assert!(
        response.contains("MODSEQ"),
        "RFC 7162 §3.2 requires MODSEQ in the untagged FETCH: {response}"
    );
    assert!(
        server
            .messages
            .get_entry(entry.id)
            .await
            .expect("query")
            .expect("entry")
            .is_seen(),
        "the change must actually be persisted"
    );
}

#[tokio::test]
async fn conditional_store_is_refused_against_a_stale_modseq() {
    // The whole purpose of the extension, over the wire. RFC 7162 §3.2.5: the
    // command still answers OK, and the refusal travels in `MODIFIED`.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let stale = stored_mod_seq(&server, entry.id).await;
    // Someone else moves the entry on.
    client.command(r"STORE 1 +FLAGS (\Seen)").await;

    let response = client
        .command(&format!(
            r"STORE 1 (UNCHANGEDSINCE {}) +FLAGS (\Flagged)",
            stale.get()
        ))
        .await;

    assert!(
        response.contains(" OK"),
        "a refused conditional store is a report, not an error: {response}"
    );
    assert!(
        response.contains(&format!("[MODIFIED {}]", entry.uid.get())),
        "the refused UID must travel in a MODIFIED code: {response}"
    );
    assert!(
        !response.contains("* 1 FETCH"),
        "a refused store must send no untagged FETCH: {response}"
    );

    let reloaded = server
        .messages
        .get_entry(entry.id)
        .await
        .expect("query")
        .expect("entry");
    assert!(
        !reloaded.flags.has(sandesh_core::SystemFlags::FLAGGED),
        "the refused mutation must not be persisted"
    );
}

#[tokio::test]
async fn a_partially_refused_conditional_store_reports_only_the_refused_uids() {
    // The realistic case: a client stores over a range and some members have
    // moved on. Every applicable message must still be changed, and only the
    // stale ones named — a server that refused the whole command would lose
    // the changes the client was entitled to make.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server.deliver(inbox, SIMPLE).await;
    let second = server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    // Move only the second message on, so its modseq exceeds the shared bound.
    let bound = stored_mod_seq(&server, second.id).await;
    client.command(r"STORE 2 +FLAGS (\Draft)").await;

    let response = client
        .command(&format!(
            r"STORE 1:2 (UNCHANGEDSINCE {}) +FLAGS (\Seen)",
            bound.get()
        ))
        .await;

    assert!(response.contains(" OK"), "{response}");
    assert!(
        response.contains(&format!("[MODIFIED {}]", second.uid.get())),
        "only the stale UID may be named: {response}"
    );

    let one = server
        .messages
        .get_entry(first.id)
        .await
        .expect("query")
        .expect("entry");
    let two = server
        .messages
        .get_entry(second.id)
        .await
        .expect("query")
        .expect("entry");
    assert!(one.is_seen(), "the applicable message must be changed");
    assert!(!two.is_seen(), "the stale message must not be changed");
}

#[tokio::test]
async fn a_noop_conditional_store_does_not_advance_the_mailbox() {
    // RFC 7162 §3.1.11, end to end: setting an already-set flag must not
    // advance HIGHESTMODSEQ, or every other client re-fetches for nothing.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    client.command(r"STORE 1 +FLAGS (\Seen)").await;
    let after_first = stored_highest(&server, inbox).await;

    let repeat = client.command(r"STORE 1 +FLAGS (\Seen)").await;
    assert!(repeat.contains(" OK"), "{repeat}");
    let after_repeat = stored_highest(&server, inbox).await;

    assert_eq!(
        after_first, after_repeat,
        "a no-op store must not consume a modification sequence"
    );
}

#[tokio::test]
async fn unchangedsince_accepts_the_uid_form() {
    // `UID STORE` takes the same modifier, and the MODIFIED code must name
    // UIDs — which in UID mode is what the client addressed by.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let stale = stored_mod_seq(&server, entry.id).await;
    client.command(r"STORE 1 +FLAGS (\Seen)").await;

    let response = client
        .command(&format!(
            r"UID STORE {} (UNCHANGEDSINCE {}) +FLAGS (\Flagged)",
            entry.uid.get(),
            stale.get()
        ))
        .await;

    assert!(response.contains(" OK"), "{response}");
    assert!(
        response.contains(&format!("[MODIFIED {}]", entry.uid.get())),
        "UID STORE must name the UID: {response}"
    );
}

#[tokio::test]
async fn a_malformed_unchangedsince_is_refused_and_changes_nothing() {
    // Nothing partially applied on a malformed command: the modifier is parsed
    // before any entry is touched.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    for bad in [
        r"STORE 1 (UNCHANGEDSINCE) +FLAGS (\Seen)",
        r"STORE 1 (UNCHANGEDSINCE abc) +FLAGS (\Seen)",
        r"STORE 1 (UNCHANGEDSINCE 0) +FLAGS (\Seen)",
        r"STORE 1 (NOTAMODIFIER 1) +FLAGS (\Seen)",
    ] {
        let response = client.command(bad).await;
        assert!(
            response.contains(" BAD") || response.contains(" NO"),
            "{bad:?} must be refused: {response}"
        );
    }

    assert!(
        !server
            .messages
            .get_entry(entry.id)
            .await
            .expect("query")
            .expect("entry")
            .is_seen(),
        "a refused command must mutate nothing"
    );
}

// ------------------------------------------------------------- SEARCH MODSEQ

#[tokio::test]
async fn search_modseq_selects_only_changed_messages() {
    // RFC 7162 §3.1.5, and the comparison is inclusive.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..3 {
        server.deliver(inbox, SIMPLE).await;
    }
    client.command("SELECT INBOX").await;

    client.command(r"STORE 2 +FLAGS (\Seen)").await;
    let changed = stored_highest(&server, inbox).await;

    let response = client
        .command(&format!("SEARCH MODSEQ {}", changed.get()))
        .await;

    assert!(response.contains(" OK"), "{response}");
    let line = response
        .lines()
        .find(|line| line.starts_with("* SEARCH"))
        .unwrap_or_default();
    assert_eq!(
        line.trim_end(),
        "* SEARCH 2",
        "only the changed message is at or above the new mark: {response}"
    );
}

#[tokio::test]
async fn search_modseq_at_the_floor_matches_everything() {
    // Every placement is stamped at or above 1, so the floor selects all of
    // them — the inclusive comparison, checked at its boundary.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..3 {
        server.deliver(inbox, SIMPLE).await;
    }
    client.command("SELECT INBOX").await;

    let response = client.command("SEARCH MODSEQ 1").await;

    let line = response
        .lines()
        .find(|line| line.starts_with("* SEARCH"))
        .unwrap_or_default();
    assert_eq!(line.trim_end(), "* SEARCH 1 2 3", "{response}");
}

#[tokio::test]
async fn uid_search_modseq_reports_uids() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let response = client.command("UID SEARCH MODSEQ 1").await;

    let line = response
        .lines()
        .find(|line| line.starts_with("* SEARCH"))
        .unwrap_or_default();
    assert!(
        line.contains(&entry.uid.get().to_string()),
        "UID SEARCH must report UIDs: {response}"
    );
}

// --------------------------------------------------------- state and read-only

#[tokio::test]
async fn condstore_commands_are_refused_before_a_mailbox_is_selected() {
    // The state machine owns this: RFC 9051 §6.4 puts FETCH, STORE and SEARCH
    // in the selected state only, and CONDSTORE adds no exemption.
    let server = TestServer::start().await;
    let (mut client, _inbox) = logged_in(&server).await;

    for command in [
        "FETCH 1 (MODSEQ)",
        r"STORE 1 (UNCHANGEDSINCE 1) +FLAGS (\Seen)",
        "SEARCH MODSEQ 1",
    ] {
        let response = client.command(command).await;
        assert!(
            response.contains(" BAD") || response.contains(" NO"),
            "{command:?} must be refused outside the selected state: {response}"
        );
    }
}

#[tokio::test]
async fn a_conditional_store_is_refused_on_an_examined_mailbox() {
    // EXAMINE permits no flag change at all, so `UNCHANGEDSINCE` cannot make
    // one permissible — the read-only refusal comes first.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;

    let examined = client.command("EXAMINE INBOX").await;
    assert!(examined.contains("[READ-ONLY]"), "{examined}");

    let response = client
        .command(r"STORE 1 (UNCHANGEDSINCE 1) +FLAGS (\Seen)")
        .await;

    assert!(
        response.contains(" BAD") || response.contains(" NO"),
        "a conditional store must be refused on a read-only selection: {response}"
    );
    assert!(
        !server
            .messages
            .get_entry(entry.id)
            .await
            .expect("query")
            .expect("entry")
            .is_seen(),
        "nothing may be written on a read-only selection"
    );
}

#[tokio::test]
async fn fetch_modseq_works_on_an_examined_mailbox() {
    // Reading a modification sequence is a read. EXAMINE must still answer it,
    // or a client could not synchronise a mailbox it opened read-only.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server.deliver(inbox, SIMPLE).await;
    client.command("EXAMINE INBOX").await;

    let response = client.command("FETCH 1 (MODSEQ)").await;

    let expected = stored_mod_seq(&server, entry.id).await;
    assert!(
        response.contains(&format!("MODSEQ ({})", expected.get())),
        "EXAMINE must answer FETCH MODSEQ: {response}"
    );
}

#[tokio::test]
async fn an_examined_fetch_does_not_advance_the_sequence() {
    // A non-peek BODY[] fetch sets \Seen — but not on a read-only selection,
    // so it must not allocate a modification sequence either.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    client.command("EXAMINE INBOX").await;

    let before = stored_highest(&server, inbox).await;
    let response = client.command("FETCH 1 (BODY[])").await;
    assert!(response.contains(" OK"), "{response}");
    let after = stored_highest(&server, inbox).await;

    assert_eq!(
        before, after,
        "a read-only fetch must not consume a modification sequence"
    );
}

#[tokio::test]
async fn condstore_needs_no_enable_for_explicitly_requested_data() {
    // RFC 7162 §3.1: `ENABLE CONDSTORE` governs *unsolicited* CONDSTORE data.
    // A client that explicitly asks for MODSEQ has asked for the syntax and
    // must get it — refusing would make the item unusable for the many clients
    // that never send ENABLE.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let response = client.command("FETCH 1 (MODSEQ)").await;

    assert!(
        response.contains("MODSEQ ("),
        "an explicit MODSEQ request must be answered without ENABLE: {response}"
    );
}

#[tokio::test]
async fn enable_condstore_is_refused_before_authentication() {
    // ENABLE is an authenticated-state command (RFC 5161 §3.1); CONDSTORE
    // changes nothing about that.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("ENABLE CONDSTORE").await;

    assert!(
        response.contains(" BAD") || response.contains(" NO"),
        "ENABLE must be refused before authentication: {response}"
    );
}

#[tokio::test]
async fn vanished_is_refused_because_qresync_is_not_implemented() {
    // `VANISHED` shares the FETCH modifier slot with `CHANGEDSINCE` but
    // belongs to QRESYNC. Accepting it would promise a `VANISHED` response
    // this server never sends, leaving the client waiting for data.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let response = client
        .command("FETCH 1:* (FLAGS) (CHANGEDSINCE 1 VANISHED)")
        .await;

    assert!(
        response.contains(" BAD") || response.contains(" NO"),
        "VANISHED must be refused while QRESYNC is unimplemented: {response}"
    );
}
