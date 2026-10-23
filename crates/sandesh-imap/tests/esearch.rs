//! `ESEARCH` (RFC 9051 §6.4.4, §7.3.4) over a real socket.
//!
//! Every test drives the production server on a TCP connection against the
//! real store. The central invariant is that `ESEARCH` and `SEARCH` are one
//! search engine seen two ways: whenever both forms are asked the same
//! question, their answers must agree.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{MailboxId, MessageFlags, SystemFlags};

async fn authenticated(server: &TestServer) -> common::Client<tokio::net::TcpStream> {
    let mut client = server.connect().await;
    let response = client.command("LOGIN user secret").await;
    assert!(response.contains("OK"), "login must succeed: {response}");
    client
}

/// Delivers `count` plain messages.
async fn fill(server: &TestServer, mailbox: MailboxId, count: usize) {
    for index in 1..=count {
        let raw = format!(
            "From: sender{index}@example.test\r\n\
Subject: message {index}\r\n\
Date: Tue, 01 Sep 2026 12:00:00 +0000\r\n\
Message-ID: <m{index}@example.test>\r\n\
\r\n\
Body of message {index}.\r\n"
        );
        server.deliver(mailbox, raw.as_bytes()).await;
    }
}

/// The `* ESEARCH` line, or `None`.
fn esearch(response: &str) -> Option<String> {
    response
        .lines()
        .find(|line| line.starts_with("* ESEARCH"))
        .map(str::trim_end)
        .map(String::from)
}

// --- response form ------------------------------------------------------

#[tokio::test]
async fn a_plain_search_still_returns_the_classic_line() {
    // The regression guard: a client that asked for nothing extra must get
    // exactly what it always got.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH ALL").await;

    assert!(response.contains("* SEARCH 1 2 3"), "{response:?}");
    assert!(!response.contains("ESEARCH"), "{response:?}");
}

#[tokio::test]
async fn esearch_echoes_the_command_tag() {
    // The correlator (RFC 9051 §7.3.4): without it a client cannot tell which
    // command an untagged result answers.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 2).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (COUNT) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.starts_with("* ESEARCH (TAG \"A3\")"), "{line:?}");
}

#[tokio::test]
async fn a_sequence_search_omits_the_uid_marker() {
    // The `UID` marker is a claim about what the numbers *are*. Emitting it
    // for sequence numbers would make the client address the wrong messages.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (ALL) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(!line.contains(" UID "), "{line:?}");
    assert!(line.contains("ALL 1:3"), "{line:?}");
}

#[tokio::test]
async fn a_uid_search_includes_the_uid_marker() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("UID SEARCH RETURN (ALL) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("UID"), "{line:?}");
    assert!(line.contains("ALL 1:3"), "{line:?}");
}

// --- the return options ------------------------------------------------

#[tokio::test]
async fn count_reports_how_many_matched() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 5).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (COUNT) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("COUNT 5"), "{line:?}");
}

#[tokio::test]
async fn count_matches_the_classic_search_result_length() {
    // One engine, two views. If these disagree, one of them is wrong.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 4).await;
    server
        .deliver_with_flags(
            inbox,
            b"Subject: seen\r\n\r\nbody\r\n",
            MessageFlags::from_system(SystemFlags::SEEN),
        )
        .await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let classic = client.command("SEARCH SEEN").await;
    let extended = client.command("SEARCH RETURN (COUNT) SEEN").await;

    let matched = classic
        .lines()
        .find(|line| line.starts_with("* SEARCH"))
        .map(|line| line.split_whitespace().skip(2).count())
        .expect("a SEARCH line");
    let line = esearch(&extended).expect("an ESEARCH line");
    assert!(
        line.contains(&format!("COUNT {matched}")),
        "COUNT must equal the classic result length ({matched}): {line:?}"
    );
}

#[tokio::test]
async fn min_and_max_report_the_extremes() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 6).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (MIN MAX) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("MIN 1"), "{line:?}");
    assert!(line.contains("MAX 6"), "{line:?}");
}

#[tokio::test]
async fn min_and_max_are_equal_for_a_single_match() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (MIN MAX ALL COUNT) 2").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("MIN 2"), "{line:?}");
    assert!(line.contains("MAX 2"), "{line:?}");
    assert!(
        line.contains("ALL 2"),
        "a singleton must not render as 2:2: {line:?}"
    );
    assert!(line.contains("COUNT 1"), "{line:?}");
}

#[tokio::test]
async fn all_renders_contiguous_matches_as_a_range() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 4).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (ALL) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("ALL 1:4"), "{line:?}");
}

#[tokio::test]
async fn all_splits_around_gaps() {
    // A gap must break the range, or the client is told a message matched that
    // did not.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let seen = MessageFlags::from_system(SystemFlags::SEEN);
    for index in 1..=5u32 {
        let raw = format!("Subject: m{index}\r\n\r\nbody\r\n");
        if index == 3 {
            server.deliver(inbox, raw.as_bytes()).await;
        } else {
            server
                .deliver_with_flags(inbox, raw.as_bytes(), seen.clone())
                .await;
        }
    }

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (ALL) SEEN").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("ALL 1:2,4:5"), "{line:?}");
}

#[tokio::test]
async fn all_options_can_be_combined() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 9).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client
        .command("UID SEARCH RETURN (MIN MAX ALL COUNT) ALL")
        .await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("UID"), "{line:?}");
    assert!(line.contains("MIN 1"), "{line:?}");
    assert!(line.contains("MAX 9"), "{line:?}");
    assert!(line.contains("ALL 1:9"), "{line:?}");
    assert!(line.contains("COUNT 9"), "{line:?}");
}

#[tokio::test]
async fn return_data_is_in_a_fixed_order() {
    // Determinism: whatever order the client asked in.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let scrambled = client
        .command("SEARCH RETURN (COUNT ALL MAX MIN) ALL")
        .await;

    let line = esearch(&scrambled).expect("an ESEARCH line");
    let min = line.find("MIN").expect("MIN present");
    let max = line.find("MAX").expect("MAX present");
    let all = line.find("ALL").expect("ALL present");
    let count = line.find("COUNT").expect("COUNT present");
    assert!(min < max && max < all && all < count, "{line:?}");
}

// --- empty results -----------------------------------------------------

#[tokio::test]
async fn an_empty_result_omits_min_max_and_all() {
    // RFC 9051 §7.3.4: there is no minimum of nothing, and no set naming no
    // messages. `MIN 0` would name a message, since 0 is not a valid number.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (MIN MAX ALL) SEEN").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert_eq!(line, "* ESEARCH (TAG \"A3\")", "{line:?}");
    assert!(!line.contains("MIN"), "{line:?}");
    assert!(!line.contains("MAX"), "{line:?}");
    assert!(!line.contains("ALL"), "{line:?}");
}

#[tokio::test]
async fn an_empty_result_still_reports_count_zero() {
    // Zero *is* a correct count, so it is sent.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client
        .command("SEARCH RETURN (MIN MAX ALL COUNT) SEEN")
        .await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("COUNT 0"), "{line:?}");
    assert!(!line.contains("MIN"), "{line:?}");
}

#[tokio::test]
async fn an_empty_mailbox_searches_cleanly() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (COUNT) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("COUNT 0"), "{line:?}");
    assert!(response.contains("A3 OK"), "{response:?}");
}

// --- UID semantics -----------------------------------------------------

#[tokio::test]
async fn uid_search_reports_uids_not_sequence_numbers() {
    // The confusion that matters most. After expunging the first two
    // messages, the survivor is sequence 1 but UID 3 — so the two forms must
    // return different numbers, and each must return its own.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let _ = client.command("STORE 1:2 +FLAGS (\\Deleted)").await;
    let _ = client.command("EXPUNGE").await;

    let sequence = client.command("SEARCH RETURN (ALL MIN MAX) ALL").await;
    let uid = client.command("UID SEARCH RETURN (ALL MIN MAX) ALL").await;

    let sequence_line = esearch(&sequence).expect("an ESEARCH line");
    let uid_line = esearch(&uid).expect("an ESEARCH line");
    assert!(sequence_line.contains("MIN 1"), "{sequence_line:?}");
    assert!(sequence_line.contains("ALL 1"), "{sequence_line:?}");
    assert!(uid_line.contains("MIN 3"), "{uid_line:?}");
    assert!(uid_line.contains("ALL 3"), "{uid_line:?}");
}

#[tokio::test]
async fn uid_gaps_are_reported_as_gaps() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 5).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let _ = client.command("STORE 3 +FLAGS (\\Deleted)").await;
    let _ = client.command("EXPUNGE").await;

    let response = client.command("UID SEARCH RETURN (ALL COUNT) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("ALL 1:2,4:5"), "{line:?}");
    assert!(line.contains("COUNT 4"), "{line:?}");
}

// --- criteria and scoping ----------------------------------------------

#[tokio::test]
async fn criteria_are_applied_exactly_as_in_a_plain_search() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;
    server
        .deliver_with_flags(
            inbox,
            b"Subject: flagged\r\n\r\nbody\r\n",
            MessageFlags::from_system(SystemFlags::FLAGGED),
        )
        .await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (ALL COUNT) FLAGGED").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("ALL 4"), "{line:?}");
    assert!(line.contains("COUNT 1"), "{line:?}");
}

#[tokio::test]
async fn a_duplicated_criterion_does_not_duplicate_results() {
    // Adjacent keys are ANDed, so repeating one is a no-op — and must not
    // double any count.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let once = client.command("SEARCH RETURN (ALL COUNT) UNSEEN").await;
    let twice = client
        .command("SEARCH RETURN (ALL COUNT) UNSEEN UNSEEN")
        .await;

    let first = esearch(&once).expect("an ESEARCH line");
    let second = esearch(&twice).expect("an ESEARCH line");
    assert!(first.contains("COUNT 3"), "{first:?}");
    assert!(second.contains("COUNT 3"), "{second:?}");
    assert!(second.contains("ALL 1:3"), "{second:?}");
}

#[tokio::test]
async fn results_are_scoped_to_the_selected_mailbox() {
    // The scope violation to guard: a search must never see another mailbox's
    // messages, however many it holds.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let other = server.mailbox(account, "Other").await;
    fill(&server, inbox, 2).await;
    fill(&server, other, 7).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (COUNT MAX) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("COUNT 2"), "{line:?}");
    assert!(line.contains("MAX 2"), "{line:?}");
}

#[tokio::test]
async fn selecting_another_mailbox_changes_the_scope() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let other = server.mailbox(account, "Other").await;
    fill(&server, inbox, 2).await;
    fill(&server, other, 5).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let first = client.command("SEARCH RETURN (COUNT) ALL").await;
    let _ = client.command("SELECT Other").await;
    let second = client.command("SEARCH RETURN (COUNT) ALL").await;

    assert!(esearch(&first).expect("line").contains("COUNT 2"));
    assert!(esearch(&second).expect("line").contains("COUNT 5"));
}

// --- interaction with mutation -----------------------------------------

#[tokio::test]
async fn a_store_changes_what_a_later_esearch_matches() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 4).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let before = client.command("SEARCH RETURN (COUNT) SEEN").await;
    let _ = client.command("STORE 2:3 +FLAGS (\\Seen)").await;
    let after = client.command("SEARCH RETURN (ALL COUNT) SEEN").await;

    assert!(esearch(&before).expect("line").contains("COUNT 0"));
    let line = esearch(&after).expect("an ESEARCH line");
    assert!(line.contains("COUNT 2"), "{line:?}");
    assert!(line.contains("ALL 2:3"), "{line:?}");
}

#[tokio::test]
async fn an_expunge_renumbers_a_later_sequence_esearch() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 4).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let _ = client.command("STORE 1 +FLAGS (\\Deleted)").await;
    let _ = client.command("EXPUNGE").await;
    let response = client.command("SEARCH RETURN (ALL MAX COUNT) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("COUNT 3"), "{line:?}");
    assert!(line.contains("ALL 1:3"), "{line:?}");
    assert!(line.contains("MAX 3"), "{line:?}");
}

// --- refusals ----------------------------------------------------------

#[tokio::test]
async fn an_unsupported_return_option_is_refused() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 2).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (RELEVANCY) ALL").await;

    assert!(response.contains("A3 BAD"), "{response:?}");
    assert!(!response.contains("ESEARCH"), "{response:?}");
}

#[tokio::test]
async fn save_is_refused_rather_than_silently_ignored() {
    // ADR-057: there is no search-result variable, so a later `FETCH $` would
    // address nothing. Accepting and ignoring `SAVE` is the failure mode.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 2).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("SEARCH RETURN (SAVE) ALL").await;

    assert!(response.contains("A3 BAD"), "{response:?}");
    assert!(!response.contains("ESEARCH"), "{response:?}");
}

#[tokio::test]
async fn a_malformed_return_clause_is_refused() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 2).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;

    for command in ["SEARCH RETURN ALL", "SEARCH RETURN (COUNT)"] {
        let response = client.command(command).await;
        assert!(
            response.contains("BAD"),
            "{command} must be refused: {response:?}"
        );
    }
}

#[tokio::test]
async fn esearch_requires_a_selected_mailbox() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("SEARCH RETURN (COUNT) ALL").await;

    assert!(response.contains("A2 BAD"), "{response:?}");
    assert!(!response.contains("ESEARCH"), "{response:?}");
}

#[tokio::test]
async fn esearch_works_on_an_examined_mailbox() {
    // Searching mutates nothing, so a read-only selection must permit it.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    fill(&server, inbox, 3).await;

    let mut client = authenticated(&server).await;
    let _ = client.command("EXAMINE INBOX").await;
    let response = client.command("SEARCH RETURN (COUNT) ALL").await;

    let line = esearch(&response).expect("an ESEARCH line");
    assert!(line.contains("COUNT 3"), "{line:?}");
}
