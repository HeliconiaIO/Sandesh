//! `SEARCH` and `UID SEARCH` over a real socket (RFC 9051 §6.4.4, §6.4.8).
//!
//! Every test drives the production server over TCP against messages stored
//! through the production `MessageStore`, so a test cannot pass against a
//! shortcut the real delivery path does not take.
//!
//! The assertions are on the *set of matched messages*, resolved back to the
//! placements the test created. That is deliberate: asserting `* SEARCH 2`
//! would pass even if the server matched the wrong message and happened to
//! number it 2, whereas mapping the result back to a known UID cannot.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::{MailboxId, MessageEntry, MessageFlags, MessageStore, SystemFlags, Timestamp};

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

/// A message with the given headers and body.
fn message(from: &str, to: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\n\
To: {to}\r\n\
Subject: {subject}\r\n\
Date: Tue, 01 Sep 2026 12:00:00 +0000\r\n\
\r\n\
{body}\r\n"
    )
    .into_bytes()
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

/// The numbers in the untagged `* SEARCH` line.
///
/// Panics if the line is absent: RFC 9051 §7.3.4 requires it on every
/// successful `SEARCH`, including one that matched nothing, so a missing line
/// is a protocol failure rather than an empty result.
fn search_results(response: &str) -> Vec<u32> {
    let line = response
        .lines()
        .find(|line| *line == "* SEARCH" || line.starts_with("* SEARCH "))
        .unwrap_or_else(|| panic!("no untagged SEARCH line in: {response}"));

    line.strip_prefix("* SEARCH")
        .unwrap_or_default()
        .split_whitespace()
        .map(|token| {
            token
                .parse()
                .unwrap_or_else(|_| panic!("non-numeric result {token} in: {response}"))
        })
        .collect()
}

/// Runs `SEARCH <criteria>` and returns the matched numbers.
async fn search(client: &mut common::Client<tokio::net::TcpStream>, criteria: &str) -> Vec<u32> {
    let response = client.command(&format!("SEARCH {criteria}")).await;
    assert!(is_ok(&response), "SEARCH {criteria} failed: {response}");
    search_results(&response)
}

/// Runs `UID SEARCH <criteria>` and returns the matched UIDs.
async fn uid_search(
    client: &mut common::Client<tokio::net::TcpStream>,
    criteria: &str,
) -> Vec<u32> {
    let response = client.command(&format!("UID SEARCH {criteria}")).await;
    assert!(is_ok(&response), "UID SEARCH {criteria} failed: {response}");
    search_results(&response)
}

// ---------------------------------------------------------------------------
// Basic criteria
// ---------------------------------------------------------------------------

#[tokio::test]
async fn all_matches_every_message_in_the_mailbox() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for index in 0..4 {
        server
            .deliver(inbox, &message("a@b", "c@d", &format!("m{index}"), "body"))
            .await;
    }
    select_inbox(&mut client).await;

    assert_eq!(search(&mut client, "ALL").await, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn an_empty_mailbox_returns_the_bare_search_line() {
    // RFC 9051 §7.3.4: no matches is still a `* SEARCH` line, not silence.
    let server = TestServer::start().await;
    let (mut client, _inbox) = logged_in(&server).await;
    select_inbox(&mut client).await;

    let response = client.command("SEARCH ALL").await;
    assert!(is_ok(&response), "{response}");
    assert!(
        response.lines().any(|line| line == "* SEARCH"),
        "the bare line must be present: {response}"
    );
    assert!(search_results(&response).is_empty());
}

#[tokio::test]
async fn a_criterion_matching_nothing_returns_an_empty_result() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "hello", "body"))
        .await;
    select_inbox(&mut client).await;

    assert!(search(&mut client, "SUBJECT nonexistent").await.is_empty());
    assert!(search(&mut client, "FROM nobody").await.is_empty());
    assert!(search(&mut client, "SEEN").await.is_empty());
}

#[tokio::test]
async fn results_are_ascending_and_deduplicated() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..5 {
        server
            .deliver(inbox, &message("a@b", "c@d", "same", "body"))
            .await;
    }
    select_inbox(&mut client).await;

    let results = search(&mut client, "ALL").await;
    assert_eq!(results, vec![1, 2, 3, 4, 5], "ascending");

    let mut sorted = results.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(results, sorted, "no duplicates, already ordered");
}

// ---------------------------------------------------------------------------
// Flag criteria
// ---------------------------------------------------------------------------

/// Delivers three messages: unflagged, `\Seen`, and `\Flagged`.
async fn three_with_flags(server: &TestServer, inbox: MailboxId) -> Vec<MessageEntry> {
    let plain = server
        .deliver(inbox, &message("a@b", "c@d", "plain", "body"))
        .await;
    let seen = server
        .deliver_with_flags(
            inbox,
            &message("a@b", "c@d", "seen", "body"),
            MessageFlags::from_system(SystemFlags::SEEN),
        )
        .await;
    let flagged = server
        .deliver_with_flags(
            inbox,
            &message("a@b", "c@d", "flagged", "body"),
            MessageFlags::from_system(SystemFlags::FLAGGED),
        )
        .await;
    vec![plain, seen, flagged]
}

#[tokio::test]
async fn flag_criteria_select_by_stored_flags() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    three_with_flags(&server, inbox).await;
    select_inbox(&mut client).await;

    assert_eq!(search(&mut client, "SEEN").await, vec![2]);
    assert_eq!(search(&mut client, "UNSEEN").await, vec![1, 3]);
    assert_eq!(search(&mut client, "FLAGGED").await, vec![3]);
    assert_eq!(search(&mut client, "UNFLAGGED").await, vec![1, 2]);
}

#[tokio::test]
async fn every_flag_criterion_and_its_negation_partition_the_mailbox() {
    // For each pair, the two results must be disjoint and together cover
    // everything. This is the guard against a criterion being inverted.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver_with_flags(
            inbox,
            &message("a@b", "c@d", "all-flags", "body"),
            MessageFlags::from_system(SystemFlags::ALL),
        )
        .await;
    for _ in 0..2 {
        server
            .deliver(inbox, &message("a@b", "c@d", "no-flags", "body"))
            .await;
    }
    select_inbox(&mut client).await;

    let everything = search(&mut client, "ALL").await;

    for (positive, negative) in [
        ("SEEN", "UNSEEN"),
        ("ANSWERED", "UNANSWERED"),
        ("FLAGGED", "UNFLAGGED"),
        ("DELETED", "UNDELETED"),
        ("DRAFT", "UNDRAFT"),
    ] {
        let with = search(&mut client, positive).await;
        let without = search(&mut client, negative).await;

        for number in &with {
            assert!(
                !without.contains(number),
                "{number} is in both {positive} and {negative}"
            );
        }
        let mut union: Vec<u32> = with.iter().chain(without.iter()).copied().collect();
        union.sort_unstable();
        assert_eq!(
            union, everything,
            "{positive} + {negative} must cover the mailbox"
        );
    }
}

#[tokio::test]
async fn deleted_and_undeleted_reflect_a_store() {
    // The 4B.1 interaction: SEARCH must see what STORE wrote.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..3 {
        server
            .deliver(inbox, &message("a@b", "c@d", "m", "body"))
            .await;
    }
    select_inbox(&mut client).await;

    assert!(search(&mut client, "DELETED").await.is_empty());

    let stored = client.command(r"STORE 2 +FLAGS (\Deleted)").await;
    assert!(is_ok(&stored), "{stored}");

    assert_eq!(
        search(&mut client, "DELETED").await,
        vec![2],
        "SEARCH must see the flag STORE just set"
    );
    assert_eq!(search(&mut client, "UNDELETED").await, vec![1, 3]);
}

#[tokio::test]
async fn keyword_criteria_match_custom_keywords() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..2 {
        server
            .deliver(inbox, &message("a@b", "c@d", "m", "body"))
            .await;
    }
    select_inbox(&mut client).await;

    client.command(r"STORE 1 +FLAGS ($Junk)").await;

    assert_eq!(search(&mut client, "KEYWORD $Junk").await, vec![1]);
    assert_eq!(search(&mut client, "UNKEYWORD $Junk").await, vec![2]);
}

// ---------------------------------------------------------------------------
// Header criteria
// ---------------------------------------------------------------------------

#[tokio::test]
async fn header_criteria_match_the_right_header() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(
            inbox,
            &message(
                "alice@example.test",
                "bob@example.test",
                "Quarterly",
                "body",
            ),
        )
        .await;
    server
        .deliver(
            inbox,
            &message("carol@example.test", "dave@example.test", "Monthly", "body"),
        )
        .await;
    select_inbox(&mut client).await;

    assert_eq!(search(&mut client, "FROM alice").await, vec![1]);
    assert_eq!(search(&mut client, "FROM carol").await, vec![2]);
    assert_eq!(search(&mut client, "TO bob").await, vec![1]);
    assert_eq!(search(&mut client, "SUBJECT Quarterly").await, vec![1]);
    assert_eq!(search(&mut client, "SUBJECT Monthly").await, vec![2]);
}

#[tokio::test]
async fn a_header_criterion_does_not_leak_across_fields() {
    // `alice` is in From on message 1; a TO search must not find it.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(
            inbox,
            &message("alice@example.test", "bob@example.test", "s", "body"),
        )
        .await;
    select_inbox(&mut client).await;

    assert_eq!(search(&mut client, "FROM alice").await, vec![1]);
    assert!(
        search(&mut client, "TO alice").await.is_empty(),
        "FROM and TO must be distinct fields"
    );
}

#[tokio::test]
async fn header_matching_is_case_insensitive_and_substring() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "Quarterly Report", "body"))
        .await;
    select_inbox(&mut client).await;

    for criteria in [
        "SUBJECT Quarterly",
        "SUBJECT quarterly",
        "SUBJECT QUARTERLY",
        "SUBJECT arterl",
        r#"SUBJECT "Quarterly Report""#,
    ] {
        assert_eq!(
            search(&mut client, criteria).await,
            vec![1],
            "{criteria} should match"
        );
    }
}

#[tokio::test]
async fn cc_and_bcc_criteria_work() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let raw = b"From: a@b\r\n\
To: c@d\r\n\
Cc: carol@example.test\r\n\
Bcc: bcc-target@example.test\r\n\
Subject: s\r\n\
\r\n\
body\r\n";
    server.deliver(inbox, raw).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "other", "body"))
        .await;
    select_inbox(&mut client).await;

    assert_eq!(search(&mut client, "CC carol").await, vec![1]);
    assert_eq!(search(&mut client, "BCC bcc-target").await, vec![1]);
}

#[tokio::test]
async fn the_header_key_finds_arbitrary_headers() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let raw = b"From: a@b\r\n\
Subject: s\r\n\
X-Campaign: spring-sale\r\n\
\r\n\
body\r\n";
    server.deliver(inbox, raw).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "s", "body"))
        .await;
    select_inbox(&mut client).await;

    assert_eq!(
        search(&mut client, "HEADER X-Campaign spring").await,
        vec![1]
    );
    assert_eq!(
        search(&mut client, "HEADER x-campaign SPRING-SALE").await,
        vec![1],
        "both the name and the value are case-insensitive"
    );
    assert!(
        search(&mut client, "HEADER X-Absent anything")
            .await
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Body and text criteria
// ---------------------------------------------------------------------------

#[tokio::test]
async fn body_searches_the_body_not_the_headers() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "pineapple", "mango content"))
        .await;
    server
        .deliver(inbox, &message("a@b", "c@d", "other", "pineapple content"))
        .await;
    select_inbox(&mut client).await;

    assert_eq!(
        search(&mut client, "BODY pineapple").await,
        vec![2],
        "BODY must not match the Subject header"
    );
    assert_eq!(search(&mut client, "BODY mango").await, vec![1]);
}

#[tokio::test]
async fn text_searches_headers_and_body_together() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "pineapple", "mango content"))
        .await;
    server
        .deliver(inbox, &message("a@b", "c@d", "other", "pineapple content"))
        .await;
    select_inbox(&mut client).await;

    let mut results = search(&mut client, "TEXT pineapple").await;
    results.sort_unstable();
    assert_eq!(
        results,
        vec![1, 2],
        "TEXT covers the subject of one and the body of the other"
    );
}

#[tokio::test]
async fn body_search_sees_decoded_content() {
    // Quoted-printable on the wire; the search must match what a reader sees.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let raw = b"From: a@b\r\n\
Subject: qp\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
invoice=20number=20A123\r\n";
    server.deliver(inbox, raw).await;
    select_inbox(&mut client).await;

    assert_eq!(
        search(&mut client, "BODY \"invoice number\"").await,
        vec![1],
        "the decoded text must be searched, not the wire bytes"
    );
}

#[tokio::test]
async fn body_search_works_across_multipart_fixtures() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for name in [
        "valid/plain-text.eml",
        "valid/html.eml",
        "valid/multipart-alternative.eml",
        "valid/multipart-mixed.eml",
    ] {
        server.deliver(inbox, &TestServer::fixture(name)).await;
    }
    select_inbox(&mut client).await;

    // Every fixture is searchable without error, and ALL sees them all.
    assert_eq!(search(&mut client, "ALL").await.len(), 4);
    // A token that cannot appear matches nothing, across all four.
    assert!(
        search(&mut client, "BODY zzzznotpresentzzzz")
            .await
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Numeric criteria
// ---------------------------------------------------------------------------

#[tokio::test]
async fn size_criteria_use_the_stored_octet_count() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let small = message("a@b", "c@d", "s", "tiny");
    let large = message("a@b", "c@d", "s", &"x".repeat(5000));
    server.deliver(inbox, &small).await;
    server.deliver(inbox, &large).await;
    select_inbox(&mut client).await;

    let boundary = small.len();
    assert_eq!(
        search(&mut client, &format!("LARGER {boundary}")).await,
        vec![2]
    );
    assert_eq!(
        search(&mut client, &format!("SMALLER {}", boundary + 1)).await,
        vec![1]
    );
}

#[tokio::test]
async fn larger_and_smaller_are_not_interchangeable() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "s", "tiny"))
        .await;
    server
        .deliver(inbox, &message("a@b", "c@d", "s", &"x".repeat(5000)))
        .await;
    select_inbox(&mut client).await;

    let larger = search(&mut client, "LARGER 1000").await;
    let smaller = search(&mut client, "SMALLER 1000").await;
    assert_eq!(larger, vec![2]);
    assert_eq!(smaller, vec![1]);
    assert_ne!(larger, smaller);
}

#[tokio::test]
async fn size_criteria_are_strict_comparisons() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let raw = message("a@b", "c@d", "s", "body");
    server.deliver(inbox, &raw).await;
    select_inbox(&mut client).await;

    let exact = raw.len();
    assert!(
        search(&mut client, &format!("LARGER {exact}"))
            .await
            .is_empty(),
        "LARGER is strict"
    );
    assert!(
        search(&mut client, &format!("SMALLER {exact}"))
            .await
            .is_empty(),
        "SMALLER is strict"
    );
}

// ---------------------------------------------------------------------------
// Date criteria
// ---------------------------------------------------------------------------

#[tokio::test]
async fn internal_date_criteria_compare_days() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;

    // 2020-06-15T08:30:00Z and 2026-09-05T00:00:00Z.
    server
        .deliver_at(
            inbox,
            &message("a@b", "c@d", "old", "body"),
            MessageFlags::empty(),
            Timestamp::from_micros(1_592_209_800_000_000),
        )
        .await;
    server
        .deliver_at(
            inbox,
            &message("a@b", "c@d", "new", "body"),
            MessageFlags::empty(),
            Timestamp::from_micros(1_788_566_400_000_000),
        )
        .await;
    select_inbox(&mut client).await;

    assert_eq!(search(&mut client, "BEFORE 1-Jan-2021").await, vec![1]);
    assert_eq!(search(&mut client, "SINCE 1-Jan-2021").await, vec![2]);
    assert_eq!(search(&mut client, "ON 15-Jun-2020").await, vec![1]);
    assert!(search(&mut client, "ON 16-Jun-2020").await.is_empty());
}

#[tokio::test]
async fn sent_date_criteria_read_the_date_header() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    // Both fixtures carry `Date: Tue, 01 Sep 2026 12:00:00 +0000`.
    server
        .deliver(inbox, &message("a@b", "c@d", "s", "body"))
        .await;
    select_inbox(&mut client).await;

    assert_eq!(search(&mut client, "SENTON 1-Sep-2026").await, vec![1]);
    assert_eq!(search(&mut client, "SENTSINCE 1-Sep-2026").await, vec![1]);
    assert_eq!(search(&mut client, "SENTBEFORE 2-Sep-2026").await, vec![1]);
    assert!(
        search(&mut client, "SENTBEFORE 1-Sep-2026")
            .await
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// NOT, OR and combinations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn not_inverts_a_criterion() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    three_with_flags(&server, inbox).await;
    select_inbox(&mut client).await;

    let seen = search(&mut client, "SEEN").await;
    let not_seen = search(&mut client, "NOT SEEN").await;
    assert_eq!(seen, vec![2]);
    assert_eq!(not_seen, vec![1, 3]);
    assert_eq!(
        not_seen,
        search(&mut client, "UNSEEN").await,
        "NOT SEEN and UNSEEN agree"
    );
}

#[tokio::test]
async fn or_takes_either_branch() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    three_with_flags(&server, inbox).await;
    select_inbox(&mut client).await;

    assert_eq!(
        search(&mut client, "OR SEEN FLAGGED").await,
        vec![2, 3],
        "either flag qualifies"
    );
}

#[tokio::test]
async fn implicit_and_narrows_where_or_widens() {
    // The semantic distinction that matters most.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    three_with_flags(&server, inbox).await;
    select_inbox(&mut client).await;

    let anded = search(&mut client, "SEEN FLAGGED").await;
    let ored = search(&mut client, "OR SEEN FLAGGED").await;

    assert!(
        anded.is_empty(),
        "no message is both \\Seen and \\Flagged: {anded:?}"
    );
    assert_eq!(ored, vec![2, 3]);
    assert_ne!(anded, ored, "AND must not behave as OR");
}

#[tokio::test]
async fn nested_or_and_not_compose() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    three_with_flags(&server, inbox).await;
    select_inbox(&mut client).await;

    // NOT (OR SEEN FLAGGED) → only the plain message.
    assert_eq!(search(&mut client, "NOT (OR SEEN FLAGGED)").await, vec![1]);

    // OR of two composite keys.
    assert_eq!(search(&mut client, "OR (SEEN) (FLAGGED)").await, vec![2, 3]);

    // A parenthesised AND.
    assert!(search(&mut client, "(SEEN FLAGGED)").await.is_empty());
}

#[tokio::test]
async fn criteria_combine_across_categories() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(
            inbox,
            &message("alice@example.test", "c@d", "Report", "body"),
        )
        .await;
    server
        .deliver_with_flags(
            inbox,
            &message("alice@example.test", "c@d", "Report", "body"),
            MessageFlags::from_system(SystemFlags::SEEN),
        )
        .await;
    server
        .deliver(inbox, &message("bob@example.test", "c@d", "Report", "body"))
        .await;
    select_inbox(&mut client).await;

    assert_eq!(
        search(&mut client, "UNSEEN FROM alice").await,
        vec![1],
        "flag and header together"
    );
    assert_eq!(
        search(&mut client, "FROM alice SUBJECT Report").await,
        vec![1, 2]
    );
    assert_eq!(
        search(&mut client, "UNSEEN FROM alice SUBJECT Report").await,
        vec![1]
    );
}

#[tokio::test]
async fn duplicate_criteria_are_idempotent() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    three_with_flags(&server, inbox).await;
    select_inbox(&mut client).await;

    assert_eq!(
        search(&mut client, "UNSEEN UNSEEN").await,
        search(&mut client, "UNSEEN").await
    );
    assert_eq!(
        search(&mut client, "UNSEEN UNSEEN UNSEEN").await,
        vec![1, 3]
    );
}

// ---------------------------------------------------------------------------
// SEARCH versus UID SEARCH
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_returns_sequence_numbers_and_uid_search_returns_uids() {
    // The single most consequential distinction in this increment. The two are
    // made to disagree by deleting the first placement, so a mix-up shows.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server
        .deliver(inbox, &message("a@b", "c@d", "m1", "body"))
        .await;
    let second = server
        .deliver(inbox, &message("a@b", "c@d", "m2", "body"))
        .await;
    let third = server
        .deliver(inbox, &message("a@b", "c@d", "m3", "body"))
        .await;
    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("delete the first placement");

    select_inbox(&mut client).await;

    // Two messages remain: sequence 1,2 with UIDs second,third.
    assert_eq!(
        search(&mut client, "ALL").await,
        vec![1, 2],
        "SEARCH returns positions"
    );
    assert_eq!(
        uid_search(&mut client, "ALL").await,
        vec![second.uid.get(), third.uid.get()],
        "UID SEARCH returns UIDs"
    );
    // And the two genuinely differ.
    assert_ne!(
        search(&mut client, "ALL").await,
        uid_search(&mut client, "ALL").await,
        "positions and UIDs must not coincide in this fixture"
    );
    assert!(second.uid.get() > 1, "UIDs really are shifted");
}

#[tokio::test]
async fn uid_search_results_are_usable_as_uid_fetch_arguments() {
    // The round trip a real client performs.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server
        .deliver(inbox, &message("a@b", "c@d", "m1", "body"))
        .await;
    server
        .deliver(inbox, &message("a@b", "c@d", "m2", "body"))
        .await;
    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("delete");
    select_inbox(&mut client).await;

    let uids = uid_search(&mut client, "ALL").await;
    assert_eq!(uids.len(), 1);

    let fetched = client
        .command(&format!("UID FETCH {} (UID ENVELOPE)", uids[0]))
        .await;
    assert!(is_ok(&fetched), "{fetched}");
    assert!(
        fetched.contains(&format!("UID {}", uids[0])),
        "the searched UID must be fetchable: {fetched}"
    );
}

#[tokio::test]
async fn search_results_are_usable_as_fetch_arguments() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    three_with_flags(&server, inbox).await;
    select_inbox(&mut client).await;

    let sequences = search(&mut client, "UNSEEN").await;
    assert_eq!(sequences, vec![1, 3]);

    for sequence in sequences {
        let fetched = client.command(&format!("FETCH {sequence} FLAGS")).await;
        assert!(is_ok(&fetched), "{fetched}");
        assert!(
            !fetched.contains("\\Seen"),
            "an UNSEEN result must really be unseen: {fetched}"
        );
    }
}

#[tokio::test]
async fn uid_search_spans_uid_gaps() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let a = server
        .deliver(inbox, &message("a@b", "c@d", "m1", "body"))
        .await;
    let b = server
        .deliver(inbox, &message("a@b", "c@d", "m2", "body"))
        .await;
    let c = server
        .deliver(inbox, &message("a@b", "c@d", "m3", "body"))
        .await;
    server.messages.delete_entry(b.id).await.expect("delete");
    select_inbox(&mut client).await;

    assert_eq!(
        uid_search(&mut client, "ALL").await,
        vec![a.uid.get(), c.uid.get()],
        "the gap is simply absent"
    );
    // Sequence numbers stay contiguous despite the UID gap.
    assert_eq!(search(&mut client, "ALL").await, vec![1, 2]);
}

#[tokio::test]
async fn a_uid_criterion_selects_by_uid_in_both_modes() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let a = server
        .deliver(inbox, &message("a@b", "c@d", "m1", "body"))
        .await;
    let b = server
        .deliver(inbox, &message("a@b", "c@d", "m2", "body"))
        .await;
    select_inbox(&mut client).await;

    // `UID <set>` names UIDs regardless of the command's mode; only the
    // *result* numbering differs.
    assert_eq!(
        search(&mut client, &format!("UID {}", b.uid.get())).await,
        vec![2],
        "SEARCH UID returns the position of the UID-named message"
    );
    assert_eq!(
        uid_search(&mut client, &format!("UID {}", b.uid.get())).await,
        vec![b.uid.get()]
    );
    assert_eq!(
        uid_search(&mut client, &format!("UID {}:{}", a.uid.get(), b.uid.get())).await,
        vec![a.uid.get(), b.uid.get()]
    );
}

#[tokio::test]
async fn a_bare_set_means_sequence_numbers_in_search_and_uids_in_uid_search() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server
        .deliver(inbox, &message("a@b", "c@d", "m1", "body"))
        .await;
    let second = server
        .deliver(inbox, &message("a@b", "c@d", "m2", "body"))
        .await;
    let third = server
        .deliver(inbox, &message("a@b", "c@d", "m3", "body"))
        .await;
    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("delete");
    select_inbox(&mut client).await;

    // Remaining: sequence 1 → second.uid, sequence 2 → third.uid.
    assert_eq!(
        search(&mut client, "1").await,
        vec![1],
        "a bare set in SEARCH is a sequence number"
    );
    assert_eq!(
        uid_search(&mut client, &format!("{}", second.uid.get())).await,
        vec![second.uid.get()],
        "a bare set in UID SEARCH is a UID"
    );
    // The distinction: sequence 1 is not UID 1.
    assert!(
        uid_search(&mut client, "1").await.is_empty(),
        "UID 1 was deleted, so UID SEARCH 1 matches nothing"
    );
    let _ = third;
}

#[tokio::test]
async fn a_sequence_set_combines_with_other_criteria() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    three_with_flags(&server, inbox).await;
    select_inbox(&mut client).await;

    // Messages 1 and 3 are unseen; restrict to the first two positions.
    assert_eq!(search(&mut client, "1:2 UNSEEN").await, vec![1]);
    assert_eq!(search(&mut client, "1:3 UNSEEN").await, vec![1, 3]);
    assert!(search(&mut client, "2 UNSEEN").await.is_empty());
}

#[tokio::test]
async fn an_out_of_range_uid_matches_nothing_without_error() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "m", "body"))
        .await;
    select_inbox(&mut client).await;

    assert!(uid_search(&mut client, "UID 99999").await.is_empty());
    assert!(search(&mut client, "UID 99999").await.is_empty());
    assert!(search(&mut client, "99999").await.is_empty());
    assert!(uid_search(&mut client, "50:60").await.is_empty());
}

// ---------------------------------------------------------------------------
// Mailbox scoping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_is_scoped_to_the_selected_mailbox() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let archive = server.mailbox(account, "Archive").await;

    server
        .deliver(inbox, &message("a@b", "c@d", "inbox-only", "body"))
        .await;
    server
        .deliver(
            archive,
            &message("a@b", "c@d", "archive-only", "unique-archive-token"),
        )
        .await;

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;
    select_inbox(&mut client).await;

    assert_eq!(
        search(&mut client, "ALL").await.len(),
        1,
        "one message here"
    );
    assert_eq!(search(&mut client, "SUBJECT inbox-only").await, vec![1]);
    assert!(
        search(&mut client, "SUBJECT archive-only").await.is_empty(),
        "another mailbox's message must be invisible"
    );
    assert!(
        search(&mut client, "BODY unique-archive-token")
            .await
            .is_empty(),
        "not even by body content"
    );
}

#[tokio::test]
async fn selecting_another_mailbox_changes_the_scope() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let archive = server.mailbox(account, "Archive").await;

    server
        .deliver(inbox, &message("a@b", "c@d", "inbox-only", "body"))
        .await;
    server
        .deliver(archive, &message("a@b", "c@d", "archive-only", "body"))
        .await;

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;

    select_inbox(&mut client).await;
    assert_eq!(search(&mut client, "SUBJECT inbox-only").await, vec![1]);

    let selected = client.command("SELECT Archive").await;
    assert!(selected.contains(" OK"), "{selected}");
    assert_eq!(search(&mut client, "SUBJECT archive-only").await, vec![1]);
    assert!(search(&mut client, "SUBJECT inbox-only").await.is_empty());
}

#[tokio::test]
async fn the_same_message_in_two_mailboxes_is_found_under_each_uid() {
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    let archive = server.mailbox(account, "Archive").await;

    let in_inbox = server
        .deliver(inbox, &message("a@b", "c@d", "shared", "body"))
        .await;
    let in_archive = server
        .messages
        .copy_entry(in_inbox.id, archive)
        .await
        .expect("copy the placement");

    let mut client = server.connect().await;
    client.command("LOGIN alice any").await;

    select_inbox(&mut client).await;
    assert_eq!(
        uid_search(&mut client, "SUBJECT shared").await,
        vec![in_inbox.uid.get()]
    );

    client.command("SELECT Archive").await;
    assert_eq!(
        uid_search(&mut client, "SUBJECT shared").await,
        vec![in_archive.uid.get()],
        "the same message, found under the Archive placement's UID"
    );
}

// ---------------------------------------------------------------------------
// Snapshot behaviour (ADR-046 / ADR-047)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_uses_the_sessions_snapshot() {
    // ADR-046: a message delivered after SELECT is not in this session's view.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "first", "body"))
        .await;
    select_inbox(&mut client).await;

    server
        .deliver(inbox, &message("a@b", "c@d", "second", "body"))
        .await;

    assert_eq!(
        search(&mut client, "ALL").await,
        vec![1],
        "the arrival is not in the snapshot"
    );
    assert!(search(&mut client, "SUBJECT second").await.is_empty());

    select_inbox(&mut client).await;
    assert_eq!(
        search(&mut client, "ALL").await,
        vec![1, 2],
        "reselecting refreshes the snapshot"
    );
}

#[tokio::test]
async fn search_sees_this_sessions_own_store() {
    // ADR-047: the acting session's flag changes are visible immediately.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..3 {
        server
            .deliver(inbox, &message("a@b", "c@d", "m", "body"))
            .await;
    }
    select_inbox(&mut client).await;

    assert_eq!(search(&mut client, "UNSEEN").await, vec![1, 2, 3]);

    client.command(r"STORE 2 +FLAGS (\Seen)").await;

    assert_eq!(
        search(&mut client, "UNSEEN").await,
        vec![1, 3],
        "SEARCH must see the flag this session just set"
    );
    assert_eq!(search(&mut client, "SEEN").await, vec![2]);
}

#[tokio::test]
async fn another_sessions_store_is_not_visible_until_reselect() {
    // ADR-046 preserved: cross-session staleness is unchanged.
    let server = TestServer::start().await;
    let account = server.catch_all_account("alice").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server
        .deliver(inbox, &message("a@b", "c@d", "m", "body"))
        .await;

    let mut a = server.connect().await;
    a.command("LOGIN alice any").await;
    select_inbox(&mut a).await;

    let mut b = server.connect().await;
    b.command("LOGIN alice any").await;
    select_inbox(&mut b).await;

    b.command(r"STORE 1 +FLAGS (\Seen)").await;

    assert_eq!(
        search(&mut a, "UNSEEN").await,
        vec![1],
        "A's snapshot predates B's change"
    );

    select_inbox(&mut a).await;
    assert!(
        search(&mut a, "UNSEEN").await.is_empty(),
        "reselecting picks up B's change"
    );
}

#[tokio::test]
async fn search_is_permitted_on_an_examined_mailbox() {
    // SEARCH mutates nothing, so a read-only selection is fine.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let entry = server
        .deliver(inbox, &message("a@b", "c@d", "m", "body"))
        .await;

    let examined = client.command("EXAMINE INBOX").await;
    assert!(examined.contains("[READ-ONLY]"), "{examined}");

    assert_eq!(search(&mut client, "ALL").await, vec![1]);
    assert_eq!(search(&mut client, "UNSEEN").await, vec![1]);

    // And nothing was marked seen by searching.
    let flags = server
        .messages
        .get_entry(entry.id)
        .await
        .expect("query")
        .expect("exists")
        .flags;
    assert!(!flags.has(SystemFlags::SEEN), "SEARCH must not set \\Seen");
}

#[tokio::test]
async fn a_concurrently_deleted_message_does_not_fail_the_search() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    let first = server
        .deliver(inbox, &message("a@b", "c@d", "gone", "body"))
        .await;
    server
        .deliver(inbox, &message("a@b", "c@d", "stays", "body"))
        .await;
    select_inbox(&mut client).await;

    server
        .messages
        .delete_entry(first.id)
        .await
        .expect("concurrent delete");

    // Flag criteria answer from the snapshot and still succeed.
    let response = client.command("SEARCH ALL").await;
    assert!(is_ok(&response), "{response}");

    // A content criterion cannot load the removed message, so it does not
    // match — but the command still completes.
    let response = client.command("SEARCH SUBJECT stays").await;
    assert!(is_ok(&response), "{response}");
    assert_eq!(search_results(&response), vec![2]);
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_before_authentication_is_refused() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("SEARCH ALL").await;
    assert!(is_refused(&response), "{response}");
    assert!(
        !response.contains("* SEARCH"),
        "no result may leak: {response}"
    );
}

#[tokio::test]
async fn search_before_select_is_refused() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "m", "body"))
        .await;
    // Authenticated, nothing selected.

    let response = client.command("SEARCH ALL").await;
    assert!(is_refused(&response), "{response}");
    assert!(!response.contains("* SEARCH"), "{response}");
}

#[tokio::test]
async fn uid_search_before_select_is_refused() {
    let server = TestServer::start().await;
    let (mut client, _inbox) = logged_in(&server).await;

    let response = client.command("UID SEARCH ALL").await;
    assert!(is_refused(&response), "{response}");
}

#[tokio::test]
async fn malformed_criteria_are_refused_without_a_result_line() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "m", "body"))
        .await;
    select_inbox(&mut client).await;

    for criteria in [
        "",
        "NOTAKEY",
        "SEEN NOTAKEY",
        "FROM",
        "HEADER",
        "HEADER X-Only",
        "LARGER",
        "LARGER abc",
        "SMALLER -1",
        "BEFORE notadate",
        "ON 2026-09-05",
        "UID 0",
        "UID abc",
        "0",
        "OR SEEN",
        "OR",
        "NOT",
        "()",
        "NEW",
        "RECENT",
        // `MODSEQ 1` is valid now that CONDSTORE is implemented; these are
        // the malformed forms — a missing argument, a non-number, and the
        // reserved 0 (RFC 7162 §3).
        "MODSEQ",
        "MODSEQ abc",
        "MODSEQ 0",
    ] {
        let response = client.command(&format!("SEARCH {criteria}")).await;
        assert!(
            is_refused(&response),
            "SEARCH {criteria:?} must be refused: {response}"
        );
        assert!(
            !response.lines().any(|line| line.starts_with("* SEARCH")),
            "SEARCH {criteria:?} must emit no result line: {response}"
        );
    }

    // The session survives every rejection.
    assert_eq!(search(&mut client, "ALL").await, vec![1]);
}

#[tokio::test]
async fn an_unsupported_charset_is_refused_with_badcharset() {
    // RFC 9051 §6.4.4: NO [BADCHARSET], so the client can retry.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "m", "body"))
        .await;
    select_inbox(&mut client).await;

    let response = client.command("SEARCH CHARSET ISO-8859-1 ALL").await;
    assert!(is_refused(&response), "{response}");
    assert!(
        response.contains("BADCHARSET"),
        "must carry the BADCHARSET code: {response}"
    );
    assert!(
        response.lines().any(|line| line.contains(" NO ")),
        "BADCHARSET is NO, not BAD: {response}"
    );
}

#[tokio::test]
async fn the_supported_charsets_are_accepted() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "m", "body"))
        .await;
    select_inbox(&mut client).await;

    for charset in ["UTF-8", "utf-8", "US-ASCII"] {
        assert_eq!(
            search(&mut client, &format!("CHARSET {charset} ALL")).await,
            vec![1],
            "{charset} should be accepted"
        );
    }
}

#[tokio::test]
async fn a_hostile_search_never_panics() {
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for _ in 0..3 {
        server
            .deliver(inbox, &message("a@b", "c@d", "m", "body"))
            .await;
    }
    select_inbox(&mut client).await;

    for hostile in [
        "SEARCH ((((",
        "SEARCH ))))",
        &format!("SEARCH {}SEEN", "NOT ".repeat(200)),
        "SEARCH OR OR OR SEEN SEEN SEEN",
        "SEARCH 4294967295",
        "SEARCH UID 4294967295",
        "SEARCH LARGER 99999999999999999999",
        &format!("SEARCH SUBJECT {}", "x".repeat(2000)),
        &format!("SEARCH {} ALL", "1,".repeat(2000)),
        "SEARCH CHARSET",
    ] {
        let response = client.command(hostile).await;
        assert!(!response.is_empty(), "{hostile} produced no response");
        assert!(
            response.contains(" OK") || response.contains(" BAD") || response.contains(" NO"),
            "{hostile} => {response}"
        );
    }

    assert_eq!(
        search(&mut client, "ALL").await,
        vec![1, 2, 3],
        "the session must survive"
    );
}

// ---------------------------------------------------------------------------
// The Odoo workflow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_odoo_fetch_cycle_completes() {
    // SELECT → SEARCH UNSEEN → FETCH → STORE +FLAGS \Seen, then confirm the
    // next poll finds nothing. This is the exact loop that was failing.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for index in 0..3 {
        server
            .deliver(
                inbox,
                &message(
                    "sender@example.test",
                    "alice@example.test",
                    &format!("Ticket {index}"),
                    &format!("Body of message {index}."),
                ),
            )
            .await;
    }
    select_inbox(&mut client).await;

    // 1. Find the unread messages.
    let unseen = search(&mut client, "UNSEEN").await;
    assert_eq!(unseen, vec![1, 2, 3], "all three are unread");

    // 2. Fetch and 3. mark each one, exactly as a fetching agent does.
    for sequence in &unseen {
        let fetched = client.command(&format!("FETCH {sequence} (RFC822)")).await;
        assert!(is_ok(&fetched), "FETCH {sequence} failed: {fetched}");
        assert!(
            fetched.contains("Ticket"),
            "the body must come back: {fetched}"
        );

        let stored = client
            .command(&format!(r"STORE {sequence} +FLAGS \Seen"))
            .await;
        assert!(is_ok(&stored), "STORE {sequence} failed: {stored}");
    }

    // 4. The next poll finds nothing — no message is processed twice.
    assert!(
        search(&mut client, "UNSEEN").await.is_empty(),
        "the second poll must be empty"
    );
    assert_eq!(search(&mut client, "SEEN").await, vec![1, 2, 3]);
}

#[tokio::test]
async fn the_odoo_cycle_works_in_uid_mode() {
    // The same loop a client using UIDs performs.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    for index in 0..2 {
        server
            .deliver(
                inbox,
                &message(
                    "s@example.test",
                    "a@example.test",
                    &format!("T{index}"),
                    "body",
                ),
            )
            .await;
    }
    select_inbox(&mut client).await;

    let uids = uid_search(&mut client, "UNSEEN").await;
    assert_eq!(uids.len(), 2);

    for uid in &uids {
        let fetched = client.command(&format!("UID FETCH {uid} (RFC822)")).await;
        assert!(is_ok(&fetched), "{fetched}");

        let stored = client
            .command(&format!(r"UID STORE {uid} +FLAGS.SILENT (\Seen)"))
            .await;
        assert!(is_ok(&stored), "{stored}");
    }

    assert!(uid_search(&mut client, "UNSEEN").await.is_empty());
}

#[tokio::test]
async fn a_parenthesised_unseen_also_works() {
    // Some clients send `SEARCH (UNSEEN)`.
    let server = TestServer::start().await;
    let (mut client, inbox) = logged_in(&server).await;
    server
        .deliver(inbox, &message("a@b", "c@d", "m", "body"))
        .await;
    select_inbox(&mut client).await;

    assert_eq!(search(&mut client, "(UNSEEN)").await, vec![1]);
    assert_eq!(search(&mut client, "UNSEEN UNDELETED").await, vec![1]);
    assert_eq!(search(&mut client, "(UNSEEN UNDELETED)").await, vec![1]);
}
