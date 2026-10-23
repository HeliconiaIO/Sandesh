//! `IDLE` and unsolicited mailbox updates over real sockets (RFC 2177).
//!
//! These are the cross-session tests: two clients, two connections, one
//! mailbox. Session A idles; session B changes something; session A must learn
//! about it *without re-selecting*. That is the limitation ADR-046 recorded and
//! this increment closes.
//!
//! ```text
//!   Session A                    Session B
//!   ─────────                    ─────────
//!   SELECT INBOX
//!   IDLE
//!   + idling
//!                                APPEND INBOX {n}
//!   * 1 EXISTS          ◀────────  (via the event bus)
//!                                STORE 1 +FLAGS (\Seen)
//!   * 1 FETCH (…\Seen)  ◀────────
//!                                STORE 1 +FLAGS (\Deleted), EXPUNGE
//!   * 1 EXPUNGE         ◀────────
//!   DONE
//!   a1 OK IDLE terminated
//! ```
//!
//! Every wait here is bounded by a timeout, so a missing update fails as a
//! timeout rather than hanging the suite.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use std::time::Duration;

use common::TestServer;
use sandesh_core::{AccountId, MailboxId, MessageStore};

/// A minimal well-formed message.
const SIMPLE: &[u8] = b"From: alice@example.test\r\n\
Subject: Idle test\r\n\
\r\n\
Body.\r\n";

/// How long a test waits for an unsolicited response before failing.
///
/// Generous enough to be reliable on a loaded machine, short enough that a
/// genuine failure is reported rather than hanging the suite.
const WAIT: Duration = Duration::from_secs(5);

/// Logs in as `alice`, returning the client and INBOX id.
async fn logged_in(
    server: &TestServer,
    account: AccountId,
) -> common::Client<tokio::net::TcpStream> {
    let mut client = server.connect().await;
    let login = client.command("LOGIN alice any").await;
    assert!(login.contains(" OK"), "login failed: {login}");
    let _ = account;
    client
}

/// Starts a server with a live bus and returns it with alice's INBOX.
async fn idle_server() -> (TestServer, AccountId, MailboxId) {
    let server = TestServer::with_events().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    (server, account, inbox)
}

/// Reads lines until one satisfies `wanted`, or the wait elapses.
///
/// Returns everything read, so a failure message shows what *did* arrive rather
/// than only that something did not.
async fn wait_for(
    client: &mut common::Client<tokio::net::TcpStream>,
    wanted: impl Fn(&str) -> bool,
) -> String {
    let mut seen = String::new();
    let result = tokio::time::timeout(WAIT, async {
        loop {
            let line = client.read_response().await;
            seen.push_str(&line);
            if line.lines().any(&wanted) {
                return;
            }
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "timed out waiting for an unsolicited response; saw: {seen:?}"
    );
    seen
}

/// Enters IDLE and asserts the continuation arrived.
async fn begin_idle(client: &mut common::Client<tokio::net::TcpStream>) -> String {
    let tag = client.next_tag();
    client.send_line(&format!("{tag} IDLE")).await;
    let continuation = client.read_response().await;
    assert!(
        continuation.starts_with('+'),
        "IDLE must be answered with a continuation request, got: {continuation:?}"
    );
    tag
}

// --- the command itself ----------------------------------------------------

#[tokio::test]
async fn idle_is_advertised_when_an_event_source_exists() {
    let (server, _account, _inbox) = idle_server().await;
    let mut client = server.connect().await;

    let response = client.command("CAPABILITY").await;

    assert!(
        response.contains("IDLE"),
        "IDLE must be advertised when it can be honoured: {response}"
    );
}

#[tokio::test]
async fn idle_answers_with_a_continuation_and_done_ends_it() {
    let (server, account, _inbox) = idle_server().await;
    let mut client = logged_in(&server, account).await;
    client.command("SELECT INBOX").await;

    let tag = begin_idle(&mut client).await;
    client.send_line("DONE").await;
    let response = client.read_until_tagged(&tag).await;

    assert!(
        response.contains(&format!("{tag} OK")),
        "DONE must complete the IDLE: {response}"
    );
}

#[tokio::test]
async fn done_is_case_insensitive() {
    // RFC 2177 §3 writes DONE as an atom, and atoms are case-insensitive.
    let (server, account, _inbox) = idle_server().await;
    let mut client = logged_in(&server, account).await;
    client.command("SELECT INBOX").await;

    let tag = begin_idle(&mut client).await;
    client.send_line("done").await;
    let response = client.read_until_tagged(&tag).await;

    assert!(response.contains(&format!("{tag} OK")), "got: {response}");
}

#[tokio::test]
async fn a_command_other_than_done_ends_the_idle_with_bad() {
    // While idling the client is not sending commands, so anything but DONE is
    // a protocol violation. Treating it as a command would let a client run one
    // under a tag it never sent.
    let (server, account, _inbox) = idle_server().await;
    let mut client = logged_in(&server, account).await;
    client.command("SELECT INBOX").await;

    let tag = begin_idle(&mut client).await;
    client.send_line("A99 NOOP").await;
    let response = client.read_until_tagged(&tag).await;

    assert!(
        response.contains(&format!("{tag} BAD")),
        "a non-DONE line must end the IDLE with BAD: {response}"
    );
}

#[tokio::test]
async fn normal_commands_work_after_done() {
    // The session returns to ordinary command processing, with its selection
    // and its sequence numbering intact.
    let (server, account, inbox) = idle_server().await;
    let mut client = logged_in(&server, account).await;
    server.deliver(inbox, SIMPLE).await;
    client.command("SELECT INBOX").await;

    let tag = begin_idle(&mut client).await;
    client.send_line("DONE").await;
    client.read_until_tagged(&tag).await;

    let response = client.command("FETCH 1 (UID)").await;
    assert!(
        response.contains("UID 1"),
        "the session works normally after DONE: {response}"
    );
}

#[tokio::test]
async fn idle_requires_authentication() {
    let (server, _account, _inbox) = idle_server().await;
    let mut client = server.connect().await;

    let response = client.command("IDLE").await;

    assert!(
        response.contains(" NO ") || response.contains(" BAD "),
        "an unauthenticated IDLE must be refused: {response}"
    );
}

#[tokio::test]
async fn an_idle_the_client_never_renews_is_ended_by_the_server() {
    // RFC 2177 §3 caps an IDLE at 29 minutes and lets the server end one the
    // client did not renew. Ended with a tagged `OK` rather than a dropped
    // connection, so a client that was simply slow can just IDLE again.
    //
    // Configured down to a reachable value here: with the 29-minute default
    // this path is untestable, and a mutation disabling the timeout survives.
    let mut config = TestServer::base_config();
    config.idle_timeout = Duration::from_millis(300);
    let server = TestServer::with_events_and_config(config).await;
    let account = server.catch_all_account("alice").await;
    server.mailbox(account, "INBOX").await;

    let mut client = logged_in(&server, account).await;
    client.command("SELECT INBOX").await;
    let tag = begin_idle(&mut client).await;

    // No DONE is ever sent: the server must end it on its own.
    let response = tokio::time::timeout(WAIT, client.read_until_tagged(&tag))
        .await
        .expect("the server must end an un-renewed IDLE rather than waiting forever");

    assert!(
        response.contains(&format!("{tag} OK")),
        "an expired IDLE completes rather than failing: {response}"
    );

    // And the session is still usable afterwards.
    let noop = client.command("NOOP").await;
    assert!(
        noop.contains(" OK"),
        "the session survives an IDLE timeout: {noop}"
    );
}

#[tokio::test]
async fn a_disconnect_while_idling_is_clean() {
    // The connection is dropped mid-IDLE. The server must reap the session and
    // its subscription rather than leaving a task parked forever.
    let (server, account, inbox) = idle_server().await;
    let mut client = logged_in(&server, account).await;
    client.command("SELECT INBOX").await;
    begin_idle(&mut client).await;

    drop(client);

    // The server is still healthy: a new session works, and a change still
    // flows through the bus rather than being blocked by the abandoned one.
    let mut next = logged_in(&server, account).await;
    server.deliver(inbox, SIMPLE).await;
    let response = next.command("SELECT INBOX").await;
    assert!(
        response.contains("1 EXISTS"),
        "the server survives a disconnect while idling: {response}"
    );
}

// --- cross-session: APPEND -------------------------------------------------

#[tokio::test]
async fn an_append_from_another_session_produces_exists() {
    // Scenario 1 from the brief, and the headline of this increment: session A
    // learns about session B's APPEND without re-selecting.
    let (server, account, _inbox) = idle_server().await;
    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    let mut bob = logged_in(&server, account).await;
    let bob_tag = bob.next_tag();
    bob.send_line(&format!("{bob_tag} APPEND INBOX {{{}}}", SIMPLE.len()))
        .await;
    bob.read_response().await;
    bob.send_raw(SIMPLE).await;
    bob.send_raw(b"\r\n").await;
    bob.read_until_tagged(&bob_tag).await;

    let seen = wait_for(&mut alice, |line| line.contains("EXISTS")).await;
    assert!(
        seen.contains("* 1 EXISTS"),
        "the idling session must be told the mailbox grew: {seen:?}"
    );

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;
}

#[tokio::test]
async fn a_delivery_produces_exists_while_idling() {
    // The same path from the other producer: SMTP delivery, not APPEND.
    let (server, account, inbox) = idle_server().await;
    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    server.deliver(inbox, SIMPLE).await;

    let seen = wait_for(&mut alice, |line| line.contains("EXISTS")).await;
    assert!(seen.contains("* 1 EXISTS"), "got: {seen:?}");

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;
}

/// The phase's latency criterion: `EXISTS` within 100ms of SMTP delivery.
///
/// The tests above prove the notification *arrives*; this one proves it arrives
/// in time. The bound matters because IDLE exists to replace polling — a client
/// that waits seconds for an `EXISTS` gains nothing over a `NOOP` loop, and the
/// regression that would cause it (a notification parked behind a timeout or a
/// poll interval rather than driven by the event bus) is invisible to a test
/// that only waits five seconds for the line to show up.
///
/// Measured from just before the delivery commits to the moment the line is
/// read, so it includes the bus hop, the session wake-up and the socket write —
/// everything between the two events the criterion names.
#[tokio::test]
async fn exists_arrives_within_the_latency_budget() {
    const BUDGET: Duration = Duration::from_millis(100);

    let (server, account, inbox) = idle_server().await;
    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    let started = std::time::Instant::now();
    server.deliver(inbox, SIMPLE).await;
    let seen = wait_for(&mut alice, |line| line.contains("EXISTS")).await;
    let elapsed = started.elapsed();

    assert!(seen.contains("* 1 EXISTS"), "got: {seen:?}");
    assert!(
        elapsed <= BUDGET,
        "EXISTS took {elapsed:?}, over the {BUDGET:?} budget — IDLE has \
         fallen back to polling or is waiting on a timeout"
    );

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;
}

#[tokio::test]
async fn the_appended_message_is_fetchable_after_done() {
    // The EXISTS was not merely a number: the session's snapshot grew, so the
    // new message is addressable by the sequence number just announced.
    let (server, account, inbox) = idle_server().await;
    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    server.deliver(inbox, SIMPLE).await;
    wait_for(&mut alice, |line| line.contains("EXISTS")).await;

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;

    let response = alice.command("FETCH 1 (UID)").await;
    assert!(
        response.contains("UID 1"),
        "the announced message must be addressable: {response}"
    );
}

#[tokio::test]
async fn successive_arrivals_report_an_increasing_count() {
    let (server, account, inbox) = idle_server().await;
    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    server.deliver(inbox, SIMPLE).await;
    let first = wait_for(&mut alice, |line| line.contains("EXISTS")).await;
    assert!(first.contains("* 1 EXISTS"), "got: {first:?}");

    server.deliver(inbox, SIMPLE).await;
    let second = wait_for(&mut alice, |line| line.contains("2 EXISTS")).await;
    assert!(
        second.contains("* 2 EXISTS"),
        "the count is the mailbox size, not a running total of events: {second:?}"
    );

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;
}

// --- cross-session: STORE --------------------------------------------------

#[tokio::test]
async fn a_flag_change_from_another_session_is_reported() {
    // Scenario 2 from the brief.
    let (server, account, inbox) = idle_server().await;
    server.deliver(inbox, SIMPLE).await;

    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    let mut bob = logged_in(&server, account).await;
    bob.command("SELECT INBOX").await;
    bob.command(r"STORE 1 +FLAGS (\Seen)").await;

    let seen = wait_for(&mut alice, |line| line.contains("FETCH")).await;
    assert!(
        seen.contains(r"\Seen"),
        "the flag change must reach the idling session: {seen:?}"
    );
    assert!(
        seen.contains("* 1 FETCH"),
        "reported as an untagged FETCH against the right sequence number: {seen:?}"
    );

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;
}

#[tokio::test]
async fn a_flag_change_updates_the_idling_sessions_own_view() {
    // Not just announced — recorded. A following FETCH in session A must report
    // the new flags rather than the ones captured at SELECT.
    let (server, account, inbox) = idle_server().await;
    server.deliver(inbox, SIMPLE).await;

    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    let mut bob = logged_in(&server, account).await;
    bob.command("SELECT INBOX").await;
    bob.command(r"STORE 1 +FLAGS (\Seen)").await;
    wait_for(&mut alice, |line| line.contains("FETCH")).await;

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;

    let response = alice.command("FETCH 1 (FLAGS)").await;
    assert!(
        response.contains(r"\Seen"),
        "the idling session's own view must be current: {response}"
    );
}

#[tokio::test]
async fn a_condstore_session_is_sent_modseq_but_a_plain_one_is_not() {
    // RFC 7162 §3.2 requires the unsolicited flag report of a
    // CONDSTORE-enabled session to carry `MODSEQ`, so the client can keep its
    // own high-water mark current without polling.
    //
    // RFC 5161 §3 makes that per *session*: a client that did not enable the
    // extension is entitled to a parser that does not know its syntax, and an
    // unsolicited response is exactly where an unexpected item would break
    // one. Both halves are asserted here against one shared change, so the
    // gate cannot be satisfied by never sending MODSEQ at all.
    let (server, account, inbox) = idle_server().await;
    server.deliver(inbox, SIMPLE).await;

    let mut alice = logged_in(&server, account).await;
    let enabled = alice.command("ENABLE CONDSTORE").await;
    assert!(
        enabled.contains("* ENABLED CONDSTORE"),
        "CONDSTORE must be enableable: {enabled}"
    );
    alice.command("SELECT INBOX").await;
    let alice_tag = begin_idle(&mut alice).await;

    let mut carol = logged_in(&server, account).await;
    carol.command("SELECT INBOX").await;
    let carol_tag = begin_idle(&mut carol).await;

    // One change, observed by both idling sessions.
    let mut bob = logged_in(&server, account).await;
    bob.command("SELECT INBOX").await;
    bob.command(r"STORE 1 +FLAGS (\Seen)").await;

    let alice_saw = wait_for(&mut alice, |line| line.contains("FETCH")).await;
    let carol_saw = wait_for(&mut carol, |line| line.contains("FETCH")).await;

    assert!(
        alice_saw.contains("MODSEQ"),
        "a CONDSTORE session must be sent MODSEQ: {alice_saw}"
    );
    assert!(
        !carol_saw.contains("MODSEQ"),
        "a session that did not enable CONDSTORE must not be sent MODSEQ: {carol_saw}"
    );
    assert!(
        carol_saw.contains(r"\Seen"),
        "it must still be told the flags changed: {carol_saw}"
    );

    alice.send_line("DONE").await;
    alice.read_until_tagged(&alice_tag).await;
    carol.send_line("DONE").await;
    carol.read_until_tagged(&carol_tag).await;
}

// --- cross-session: EXPUNGE ------------------------------------------------

#[tokio::test]
async fn an_expunge_from_another_session_is_reported() {
    // Scenario 3 from the brief.
    let (server, account, inbox) = idle_server().await;
    server.deliver(inbox, SIMPLE).await;

    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    let mut bob = logged_in(&server, account).await;
    bob.command("SELECT INBOX").await;
    bob.command(r"STORE 1 +FLAGS (\Deleted)").await;
    bob.command("EXPUNGE").await;

    let seen = wait_for(&mut alice, |line| line.contains("EXPUNGE")).await;
    assert!(
        seen.contains("* 1 EXPUNGE"),
        "the removal must reach the idling session: {seen:?}"
    );

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;
}

#[tokio::test]
async fn an_expunge_renumbers_the_idling_sessions_sequence_map() {
    // The correctness property that matters most: after `* 1 EXPUNGE`, session
    // A's `FETCH 1` must address what was message 2. A session that announced
    // the expunge but did not renumber would then return the wrong message.
    let (server, account, inbox) = idle_server().await;
    let first = server.deliver(inbox, SIMPLE).await;
    let second = server
        .deliver(inbox, b"From: a@b\r\nSubject: second\r\n\r\ntwo\r\n")
        .await;

    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    let mut bob = logged_in(&server, account).await;
    bob.command("SELECT INBOX").await;
    bob.command(r"STORE 1 +FLAGS (\Deleted)").await;
    bob.command("EXPUNGE").await;
    wait_for(&mut alice, |line| line.contains("EXPUNGE")).await;

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;

    let response = alice.command("FETCH 1 (UID)").await;
    assert!(
        response.contains(&format!("UID {}", second.uid.get())),
        "sequence 1 must now be the surviving message (UID {}), not UID {}: {response}",
        second.uid.get(),
        first.uid.get()
    );
}

#[tokio::test]
async fn uids_are_stable_across_an_unsolicited_expunge() {
    let (server, account, inbox) = idle_server().await;
    server.deliver(inbox, SIMPLE).await;
    let second = server
        .deliver(inbox, b"From: a@b\r\nSubject: second\r\n\r\ntwo\r\n")
        .await;

    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    let mut bob = logged_in(&server, account).await;
    bob.command("SELECT INBOX").await;
    bob.command(r"STORE 1 +FLAGS (\Deleted)").await;
    bob.command("EXPUNGE").await;
    wait_for(&mut alice, |line| line.contains("EXPUNGE")).await;

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;

    // The UID is unchanged, and UID FETCH still finds it — sequence numbers
    // moved, identities did not.
    let response = alice
        .command(&format!("UID FETCH {} (UID)", second.uid.get()))
        .await;
    assert!(
        response.contains(&format!("UID {}", second.uid.get())),
        "UID addressing is unaffected by renumbering: {response}"
    );
}

#[tokio::test]
async fn a_move_from_another_session_reports_an_expunge() {
    // MOVE removes from the source, so an idling session on the source mailbox
    // must see the message leave.
    let (server, account, inbox) = idle_server().await;
    server.mailbox(account, "Archive").await;
    server.deliver(inbox, SIMPLE).await;

    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    let mut bob = logged_in(&server, account).await;
    bob.command("SELECT INBOX").await;
    bob.command("MOVE 1 Archive").await;

    let seen = wait_for(&mut alice, |line| line.contains("EXPUNGE")).await;
    assert!(seen.contains("* 1 EXPUNGE"), "got: {seen:?}");

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;
}

// --- mailbox isolation -----------------------------------------------------

#[tokio::test]
async fn a_change_in_another_mailbox_is_not_reported() {
    // The isolation rule. A session idling on INBOX must never be told that
    // Archive changed: the sequence numbers in such a response would address
    // the wrong mailbox entirely.
    let (server, account, _inbox) = idle_server().await;
    let archive = server.mailbox(account, "Archive").await;

    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    // A change in Archive, then one in INBOX. Only the second may be reported,
    // and the first must not appear *before* it — which is what makes this a
    // real assertion rather than a race that usually passes.
    server.deliver(archive, SIMPLE).await;
    server.deliver(_inbox, SIMPLE).await;

    let seen = wait_for(&mut alice, |line| line.contains("EXISTS")).await;
    assert_eq!(
        seen.matches("EXISTS").count(),
        1,
        "exactly one EXISTS, for INBOX only: {seen:?}"
    );
    assert!(
        seen.contains("* 1 EXISTS"),
        "the INBOX arrival is reported as the first message: {seen:?}"
    );

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;
}

#[tokio::test]
async fn two_idling_sessions_on_different_mailboxes_each_see_only_their_own() {
    let (server, account, inbox) = idle_server().await;
    let archive = server.mailbox(account, "Archive").await;

    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let alice_tag = begin_idle(&mut alice).await;

    let mut bob = logged_in(&server, account).await;
    bob.command("SELECT Archive").await;
    let bob_tag = begin_idle(&mut bob).await;

    server.deliver(inbox, SIMPLE).await;
    let alice_seen = wait_for(&mut alice, |line| line.contains("EXISTS")).await;
    assert!(alice_seen.contains("* 1 EXISTS"), "got: {alice_seen:?}");

    server.deliver(archive, SIMPLE).await;
    let bob_seen = wait_for(&mut bob, |line| line.contains("EXISTS")).await;
    assert!(bob_seen.contains("* 1 EXISTS"), "got: {bob_seen:?}");

    // Neither saw two arrivals: each mailbox produced exactly one.
    assert_eq!(alice_seen.matches("EXISTS").count(), 1, "{alice_seen:?}");
    assert_eq!(bob_seen.matches("EXISTS").count(), 1, "{bob_seen:?}");

    alice.send_line("DONE").await;
    alice.read_until_tagged(&alice_tag).await;
    bob.send_line("DONE").await;
    bob.read_until_tagged(&bob_tag).await;
}

#[tokio::test]
async fn two_idling_sessions_on_the_same_mailbox_both_see_the_change() {
    // Independent snapshots, one event: both sessions are told, and neither
    // starves the other.
    let (server, account, inbox) = idle_server().await;

    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let alice_tag = begin_idle(&mut alice).await;

    let mut bob = logged_in(&server, account).await;
    bob.command("SELECT INBOX").await;
    let bob_tag = begin_idle(&mut bob).await;

    server.deliver(inbox, SIMPLE).await;

    let alice_seen = wait_for(&mut alice, |line| line.contains("EXISTS")).await;
    let bob_seen = wait_for(&mut bob, |line| line.contains("EXISTS")).await;
    assert!(alice_seen.contains("* 1 EXISTS"), "got: {alice_seen:?}");
    assert!(bob_seen.contains("* 1 EXISTS"), "got: {bob_seen:?}");

    alice.send_line("DONE").await;
    alice.read_until_tagged(&alice_tag).await;
    bob.send_line("DONE").await;
    bob.read_until_tagged(&bob_tag).await;
}

#[tokio::test]
async fn a_session_that_is_not_idling_is_not_disturbed() {
    // Unsolicited responses belong to IDLE. A session sitting at the command
    // prompt must not receive lines it did not ask for, which would desynchronise
    // its response parsing.
    let (server, account, inbox) = idle_server().await;
    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;

    server.deliver(inbox, SIMPLE).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The very next thing read must be this command's own response.
    let response = alice.command("NOOP").await;
    assert!(
        response.contains(" OK"),
        "a non-idling session sees only its own responses: {response}"
    );
    assert!(
        !response.contains("EXISTS"),
        "no unsolicited line may precede it: {response}"
    );
}

// --- concurrency -----------------------------------------------------------

#[tokio::test]
async fn many_concurrent_changes_are_all_reported_in_order() {
    // Event ordering: the bus assigns a total order, and the session applies
    // updates in that order, so the counts a client sees ascend by one.
    let (server, account, inbox) = idle_server().await;
    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    let tag = begin_idle(&mut alice).await;

    for _ in 0..5 {
        server.deliver(inbox, SIMPLE).await;
    }

    let seen = wait_for(&mut alice, |line| line.contains("5 EXISTS")).await;
    let counts: Vec<&str> = seen
        .lines()
        .filter(|line| line.contains("EXISTS"))
        .map(|line| line.trim_start_matches("* ").trim_end_matches(" EXISTS"))
        .collect();
    assert_eq!(
        counts,
        vec!["1", "2", "3", "4", "5"],
        "counts must ascend by one, in order: {seen:?}"
    );

    alice.send_line("DONE").await;
    alice.read_until_tagged(&tag).await;

    assert_eq!(
        server
            .messages
            .list_entries(inbox)
            .await
            .expect("list")
            .len(),
        5,
        "the reported count matches committed storage"
    );
}

#[tokio::test]
async fn an_idle_session_does_not_block_other_work() {
    // The bus never blocks its producer, so an idling session cannot throttle
    // delivery. A test that hangs here would mean it can.
    let (server, account, inbox) = idle_server().await;
    let mut alice = logged_in(&server, account).await;
    alice.command("SELECT INBOX").await;
    begin_idle(&mut alice).await;

    // Deliveries proceed while alice idles and never reads.
    let delivered = tokio::time::timeout(WAIT, async {
        for _ in 0..20 {
            server.deliver(inbox, SIMPLE).await;
        }
    })
    .await;

    assert!(
        delivered.is_ok(),
        "an idling session must not block delivery"
    );
}
