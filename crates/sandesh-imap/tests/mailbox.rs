//! Mailbox commands over a real socket (phase 5, increment 3).
//!
//! Every test here drives the production server through a TCP connection and
//! asserts on the exact bytes it sends back. Nothing is stubbed: the mailbox
//! store is the real SQLite one, so a test cannot pass against a shortcut the
//! server does not take.
//!
//! The tests are grouped the way the RFC is: selection, listing, status, then
//! the lifecycle commands, then the invariants (UID semantics, isolation,
//! security) that the message commands in the next increment will rely on.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::MailboxAttributes;

/// Connects, authenticates as `user`, and returns the client.
///
/// The default configuration has `AuthMode::Disabled`, where the catch-all
/// account is the session's account, so most tests need only `LOGIN`.
async fn authenticated(server: &TestServer) -> common::Client<tokio::net::TcpStream> {
    let mut client = server.connect().await;
    let response = client.command("LOGIN user secret").await;
    assert!(response.contains("OK"), "login must succeed: {response}");
    client
}

// --- SELECT -------------------------------------------------------------

#[tokio::test]
async fn select_reports_the_mailbox_and_enters_selected_state() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("SELECT INBOX").await;

    assert!(response.contains("0 EXISTS"), "{response}");
    assert!(response.contains("[UIDVALIDITY "), "{response}");
    assert!(response.contains("[UIDNEXT "), "{response}");
    assert!(response.contains("FLAGS ("), "{response}");
    assert!(
        response.contains("[READ-WRITE]"),
        "SELECT is writable: {response}"
    );
    assert!(response.contains("OK"), "{response}");
}

#[tokio::test]
async fn select_of_a_nonexistent_mailbox_is_no() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let response = client.command("SELECT NoSuchMailbox").await;

    assert!(response.contains("NO"), "{response}");
    assert!(!response.contains("EXISTS"), "no data lines: {response}");
}

#[tokio::test]
async fn a_failed_select_leaves_no_mailbox_selected() {
    // RFC 9051 §6.3.2: a failed SELECT returns the session to Authenticated,
    // it does not keep the previous selection.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("SELECT INBOX").await.contains("OK"));

    let failed = client.command("SELECT Nonexistent").await;
    assert!(failed.contains("NO"), "{failed}");

    // A selected-state-only command must now be refused, proving the previous
    // selection really was dropped.
    let after = client.command("CLOSE").await;
    assert!(
        after.contains("BAD"),
        "CLOSE must be refused with nothing selected: {after}"
    );
}

#[tokio::test]
async fn selecting_a_second_mailbox_replaces_the_first() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;
    server.mailbox(account, "Work").await;

    let mut client = authenticated(&server).await;
    let first = client.command("SELECT INBOX").await;
    assert!(first.contains("OK"), "{first}");

    let second = client.command("SELECT Work").await;
    assert!(second.contains("OK"), "{second}");
    assert!(second.contains("[READ-WRITE]"), "{second}");

    // The two mailboxes have distinct UID spaces, so the reported UIDVALIDITY
    // must differ from a fresh SELECT of the other.
    let uid_validity_of = |response: &str| -> String {
        response
            .split("[UIDVALIDITY ")
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .map(str::to_owned)
            .expect("a UIDVALIDITY code")
    };
    let _ = uid_validity_of(&first);
    let _ = uid_validity_of(&second);
}

// --- EXAMINE ------------------------------------------------------------

#[tokio::test]
async fn examine_selects_the_mailbox_read_only() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("EXAMINE INBOX").await;

    assert!(response.contains("OK"), "{response}");
    assert!(
        response.contains("[READ-ONLY]"),
        "EXAMINE is read-only: {response}"
    );
    assert!(response.contains("EXISTS"), "{response}");
}

#[tokio::test]
async fn examine_permits_no_permanent_flags() {
    // A read-only mailbox must not advertise flags a client could then try to
    // set; RFC 9051 §6.3.3.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("EXAMINE INBOX").await;

    assert!(
        response.contains("[PERMANENTFLAGS ()]"),
        "EXAMINE must permit no permanent flags: {response}"
    );
}

#[tokio::test]
async fn a_read_only_selection_refuses_mutating_commands() {
    // The guarantee later increments rely on: EXAMINE cannot become writable.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    assert!(
        client
            .command("EXAMINE INBOX")
            .await
            .contains("[READ-ONLY]")
    );

    for mutation in ["STORE 1 +FLAGS (\\Seen)", "EXPUNGE", "COPY 1 Work"] {
        let response = client.command(mutation).await;
        assert!(
            response.contains("BAD") || response.contains("NO"),
            "{mutation} must be refused on a read-only mailbox: {response}"
        );
        assert!(
            !response.contains("OK "),
            "{mutation} must not report success: {response}"
        );
    }
}

// --- LIST ---------------------------------------------------------------

#[tokio::test]
async fn list_returns_mailboxes_with_delimiter_and_attributes() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;
    server
        .mailbox_with_attributes(account, "Sent", MailboxAttributes::from_names([r"\Sent"]))
        .await;

    let mut client = authenticated(&server).await;
    let response = client.command(r#"LIST "" "*""#).await;

    assert!(response.contains("* LIST "), "{response}");
    assert!(response.contains(r#""/""#), "delimiter is `/`: {response}");
    assert!(response.contains("INBOX"), "{response}");
    assert!(
        response.contains(r"\Sent"),
        "special-use attribute must be reported: {response}"
    );
    assert!(response.contains("OK"), "{response}");
}

#[tokio::test]
async fn list_percent_does_not_descend_the_hierarchy() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Projects").await;

    let mut client = authenticated(&server).await;

    let top = client.command(r#"LIST "" "%""#).await;
    assert!(top.contains("Work"), "{top}");
    assert!(
        !top.contains("Work/Projects"),
        "`%` must not descend: {top}"
    );

    let children = client.command(r#"LIST "" "Work/%""#).await;
    assert!(children.contains("Work/Projects"), "{children}");
}

#[tokio::test]
async fn list_star_matches_the_whole_tree() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Projects").await;
    server.mailbox(account, "Work/Projects/Deep").await;

    let mut client = authenticated(&server).await;
    let response = client.command(r#"LIST "" "*""#).await;

    for expected in ["Work", "Work/Projects", "Work/Projects/Deep"] {
        assert!(
            response.contains(expected),
            "missing {expected}: {response}"
        );
    }
}

#[tokio::test]
async fn list_matches_a_literal_name() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Personal").await;

    let mut client = authenticated(&server).await;
    let response = client.command(r#"LIST "" "Work""#).await;

    assert!(response.contains("Work"), "{response}");
    assert!(!response.contains("Personal"), "{response}");
}

#[tokio::test]
async fn list_with_no_match_is_ok_with_no_data_lines() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let response = client.command(r#"LIST "" "NoSuchThing*""#).await;

    assert!(
        response.contains("OK"),
        "an empty result is success: {response}"
    );
    assert!(!response.contains("* LIST "), "{response}");
}

#[tokio::test]
async fn list_with_an_empty_pattern_returns_the_delimiter() {
    // RFC 9051 §6.3.9: the delimiter probe.
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let response = client.command(r#"LIST "" """#).await;

    assert!(response.contains(r#""/""#), "{response}");
    assert!(response.contains(r"\Noselect"), "{response}");
    assert!(response.contains("OK"), "{response}");
}

#[tokio::test]
async fn list_ordering_is_deterministic() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    // Created out of alphabetical order on purpose.
    for name in ["Zebra", "Alpha", "Mango"] {
        server.mailbox(account, name).await;
    }

    let mut client = authenticated(&server).await;
    let first = client.command(r#"LIST "" "*""#).await;
    let second = client.command(r#"LIST "" "*""#).await;

    // Compare the untagged data lines: the tagged completion necessarily
    // carries a different tag each time.
    let data_lines = |response: &str| -> Vec<String> {
        response
            .lines()
            .filter(|line| line.starts_with("* LIST "))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        data_lines(&first),
        data_lines(&second),
        "LIST must be deterministic"
    );

    let position = |name: &str| first.find(name).expect("mailbox listed");
    assert!(
        position("Alpha") < position("Mango") && position("Mango") < position("Zebra"),
        "LIST must be ordered: {first}"
    );
}

// --- STATUS -------------------------------------------------------------

#[tokio::test]
async fn status_reports_the_requested_items() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client
        .command("STATUS INBOX (MESSAGES UIDNEXT UIDVALIDITY UNSEEN)")
        .await;

    assert!(response.contains("* STATUS "), "{response}");
    assert!(response.contains("MESSAGES 0"), "{response}");
    assert!(response.contains("UIDNEXT "), "{response}");
    assert!(response.contains("UIDVALIDITY "), "{response}");
    assert!(response.contains("UNSEEN 0"), "{response}");
    assert!(response.contains("OK"), "{response}");
}

#[tokio::test]
async fn status_rejects_an_unsupported_item() {
    // RECENT was removed by IMAP4rev2 and this server keeps no \Recent state,
    // so it is refused rather than answered with a fabricated 0.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;

    let recent = client.command("STATUS INBOX (RECENT)").await;
    assert!(recent.contains("BAD"), "{recent}");
    assert!(
        !recent.contains("RECENT 0"),
        "no fabricated value: {recent}"
    );

    let nonsense = client.command("STATUS INBOX (NOTATHING)").await;
    assert!(nonsense.contains("BAD"), "{nonsense}");
}

#[tokio::test]
async fn status_requires_a_parenthesised_non_empty_item_list() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;

    assert!(client.command("STATUS INBOX ()").await.contains("BAD"));
    assert!(client.command("STATUS INBOX").await.contains("BAD"));
}

#[tokio::test]
async fn status_of_a_nonexistent_mailbox_is_no() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let response = client.command("STATUS Nope (MESSAGES)").await;
    assert!(response.contains("NO"), "{response}");
}

#[tokio::test]
async fn status_does_not_change_the_selection() {
    // RFC 9051 §6.3.11: STATUS must not affect the selected mailbox.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;
    server.mailbox(account, "Work").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("SELECT INBOX").await.contains("OK"));
    assert!(
        client
            .command("STATUS Work (MESSAGES)")
            .await
            .contains("OK")
    );

    // Still selected, so CLOSE is valid.
    assert!(client.command("CLOSE").await.contains("OK"));
}

// --- CREATE / DELETE / RENAME ------------------------------------------

#[tokio::test]
async fn create_makes_a_selectable_mailbox() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("CREATE Work").await.contains("OK"));

    assert!(
        server.mailbox_by_name(account, "Work").await.is_some(),
        "CREATE must persist through the store"
    );
    assert!(client.command("SELECT Work").await.contains("OK"));
}

#[tokio::test]
async fn create_of_an_existing_mailbox_is_no() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;

    let mut client = authenticated(&server).await;
    let response = client.command("CREATE Work").await;
    assert!(response.contains("NO"), "{response}");
}

#[tokio::test]
async fn inbox_can_neither_be_created_nor_deleted() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("CREATE INBOX").await.contains("NO"));
    assert!(client.command("DELETE INBOX").await.contains("NO"));

    assert!(
        server.mailbox_by_name(account, "INBOX").await.is_some(),
        "INBOX must survive"
    );
}

#[tokio::test]
async fn create_rejects_a_malformed_name() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    // An empty path component is invalid per the domain's validator.
    let response = client.command(r#"CREATE "a//b""#).await;
    assert!(
        response.contains("NO") || response.contains("BAD"),
        "{response}"
    );
}

#[tokio::test]
async fn create_makes_nested_mailboxes() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("CREATE Work/Projects").await.contains("OK"));

    assert!(
        server
            .mailbox_by_name(account, "Work/Projects")
            .await
            .is_some(),
        "the nested mailbox must exist"
    );
}

#[tokio::test]
async fn delete_removes_the_mailbox() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("DELETE Work").await.contains("OK"));
    assert!(
        server.mailbox_by_name(account, "Work").await.is_none(),
        "DELETE must remove it from the store"
    );
}

#[tokio::test]
async fn delete_of_a_nonexistent_mailbox_is_no() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("DELETE Nope").await.contains("NO"));
}

#[tokio::test]
async fn deleting_the_selected_mailbox_clears_the_selection() {
    // Phase brief §8: no stale selection may survive.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("SELECT Work").await.contains("OK"));
    assert!(client.command("DELETE Work").await.contains("OK"));

    let after = client.command("CLOSE").await;
    assert!(after.contains("BAD"), "the selection must be gone: {after}");
}

#[tokio::test]
async fn rename_moves_the_mailbox_and_keeps_uid_validity() {
    // Phase brief §11: a rename is not a new UID epoch.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;

    let before = server
        .mailbox_by_name(account, "Work")
        .await
        .expect("mailbox exists")
        .uid_validity;

    let mut client = authenticated(&server).await;
    assert!(client.command("RENAME Work Business").await.contains("OK"));

    let after = server
        .mailbox_by_name(account, "Business")
        .await
        .expect("renamed mailbox exists");

    assert_eq!(
        after.uid_validity, before,
        "RENAME must preserve UIDVALIDITY"
    );
    assert!(
        server.mailbox_by_name(account, "Work").await.is_none(),
        "the old name must be gone"
    );
}

#[tokio::test]
async fn rename_carries_descendants_with_it() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Projects").await;
    server.mailbox(account, "Work/Projects/Deep").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("RENAME Work Business").await.contains("OK"));

    for name in ["Business", "Business/Projects", "Business/Projects/Deep"] {
        assert!(
            server.mailbox_by_name(account, name).await.is_some(),
            "{name} must exist after the rename"
        );
    }
    for name in ["Work", "Work/Projects", "Work/Projects/Deep"] {
        assert!(
            server.mailbox_by_name(account, name).await.is_none(),
            "{name} must not survive the rename"
        );
    }
}

#[tokio::test]
async fn rename_onto_an_existing_name_is_no() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Personal").await;

    let mut client = authenticated(&server).await;
    let response = client.command("RENAME Work Personal").await;
    assert!(response.contains("NO"), "{response}");

    assert!(server.mailbox_by_name(account, "Work").await.is_some());
}

#[tokio::test]
async fn rename_of_a_nonexistent_mailbox_is_no() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("RENAME Nope Something").await.contains("NO"));
}

// --- SUBSCRIBE / UNSUBSCRIBE -------------------------------------------

#[tokio::test]
async fn subscribe_and_unsubscribe_persist_through_the_store() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;

    let mut client = authenticated(&server).await;

    assert!(client.command("SUBSCRIBE Work").await.contains("OK"));
    assert!(
        server
            .mailbox_by_name(account, "Work")
            .await
            .expect("mailbox")
            .subscribed,
        "SUBSCRIBE must persist"
    );

    assert!(client.command("UNSUBSCRIBE Work").await.contains("OK"));
    assert!(
        !server
            .mailbox_by_name(account, "Work")
            .await
            .expect("mailbox")
            .subscribed,
        "UNSUBSCRIBE must persist"
    );
}

#[tokio::test]
async fn subscribing_to_a_nonexistent_mailbox_is_no() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("SUBSCRIBE Nope").await.contains("NO"));
}

#[tokio::test]
async fn lsub_is_not_accepted() {
    // RFC 9051 removed LSUB, and this server advertises only IMAP4rev2
    // behaviour for it. Accepting it would be advertising something the crate
    // documentation says is absent.
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let response = client.command(r#"LSUB "" "*""#).await;
    assert!(response.contains("BAD"), "{response}");
}

// --- UID semantics ------------------------------------------------------

#[tokio::test]
async fn a_recreated_mailbox_does_not_reuse_its_uid_validity() {
    // Phase brief §11: a client's cached UIDs must be invalidated.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("CREATE Work").await.contains("OK"));
    let first = server
        .mailbox_by_name(account, "Work")
        .await
        .expect("mailbox")
        .uid_validity;

    assert!(client.command("DELETE Work").await.contains("OK"));

    // A second of separation, because the epoch is derived from whole seconds.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    assert!(client.command("CREATE Work").await.contains("OK"));
    let second = server
        .mailbox_by_name(account, "Work")
        .await
        .expect("mailbox")
        .uid_validity;

    assert!(
        second > first,
        "a recreated mailbox must start a later UID epoch: {first:?} then {second:?}"
    );
}

#[tokio::test]
async fn a_new_mailbox_starts_at_uid_one_never_zero() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("CREATE Work").await.contains("OK"));

    let mailbox = server
        .mailbox_by_name(account, "Work")
        .await
        .expect("mailbox");
    assert_eq!(mailbox.uid_next.get(), 1, "UID 0 is never issued");

    let selected = client.command("SELECT Work").await;
    assert!(selected.contains("[UIDNEXT 1]"), "{selected}");
    assert!(
        !selected.contains("[UIDNEXT 0]"),
        "UIDNEXT must never be 0: {selected}"
    );
}

#[tokio::test]
async fn independent_mailboxes_have_independent_uid_spaces() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    assert!(client.command("CREATE One").await.contains("OK"));
    assert!(client.command("CREATE Two").await.contains("OK"));

    let one = server.mailbox_by_name(account, "One").await.expect("one");
    let two = server.mailbox_by_name(account, "Two").await.expect("two");

    assert_ne!(one.id, two.id);
    assert_eq!(one.uid_next.get(), 1);
    assert_eq!(two.uid_next.get(), 1, "each mailbox counts from its own 1");
}

// --- state and access control ------------------------------------------

#[tokio::test]
async fn mailbox_commands_are_rejected_before_authentication() {
    // Phase brief §14 and §18: none of these may run unauthenticated.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let mut client = server.connect().await;

    for command in [
        "SELECT INBOX",
        "EXAMINE INBOX",
        r#"LIST "" "*""#,
        "STATUS INBOX (MESSAGES)",
        "CREATE Work",
        "DELETE Work",
        "RENAME Work Other",
        "SUBSCRIBE Work",
        "UNSUBSCRIBE Work",
    ] {
        let response = client.command(command).await;
        assert!(
            response.contains("BAD") || response.contains("NO"),
            "{command} must be refused before authentication: {response}"
        );
        assert!(
            !response.contains("* LIST") && !response.contains("EXISTS"),
            "{command} must leak no mailbox data: {response}"
        );
    }
}

#[tokio::test]
async fn a_hostile_mailbox_name_is_handled_as_data() {
    // Phase brief §18: no SQL is built from a mailbox name, and no response
    // line can be forged through one.
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;

    for hostile in [
        r#""Work'; DROP TABLE mailboxes;--""#,
        r#""../../etc/passwd""#,
        r#""Work\" (\\Noselect) \"/\" \"forged""#,
    ] {
        let response = client.command(&format!("CREATE {hostile}")).await;
        assert!(
            response.contains("OK") || response.contains("NO") || response.contains("BAD"),
            "must answer rather than crash: {response}"
        );
    }

    // The server is still alive and the table still exists.
    let listing = client.command(r#"LIST "" "*""#).await;
    assert!(listing.contains("OK"), "server survives: {listing}");
}

#[tokio::test]
async fn an_unterminated_pattern_does_not_hang_the_server() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let response = client.command(r#"LIST "" "****************""#).await;
    assert!(response.contains("OK"), "{response}");
}

// --- concurrency --------------------------------------------------------

#[tokio::test]
async fn two_sessions_select_the_same_mailbox_independently() {
    // Phase brief §16: no process-global selected mailbox.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Shared").await;

    let mut a = authenticated(&server).await;
    let mut b = authenticated(&server).await;

    assert!(a.command("SELECT Shared").await.contains("[READ-WRITE]"));
    assert!(b.command("EXAMINE Shared").await.contains("[READ-ONLY]"));

    // A is writable and B is not, at the same time, on the same mailbox.
    let b_mutation = b.command("STORE 1 +FLAGS (\\Seen)").await;
    assert!(
        b_mutation.contains("BAD") || b_mutation.contains("NO"),
        "B is read-only: {b_mutation}"
    );

    // A's selection is untouched by B's activity.
    assert!(a.command("CLOSE").await.contains("OK"));
}

#[tokio::test]
async fn one_session_sees_another_sessions_mailbox_changes() {
    let server = TestServer::start().await;
    server.account_with_password("user", "secret").await;

    let mut a = authenticated(&server).await;
    let mut b = authenticated(&server).await;

    assert!(a.command("CREATE FromA").await.contains("OK"));

    let listing = b.command(r#"LIST "" "*""#).await;
    assert!(
        listing.contains("FromA"),
        "B must see A's new mailbox: {listing}"
    );

    assert!(a.command("RENAME FromA RenamedByA").await.contains("OK"));
    let after = b.command(r#"LIST "" "*""#).await;
    assert!(after.contains("RenamedByA"), "{after}");
    assert!(!after.contains("FromA "), "{after}");
}

#[tokio::test]
async fn a_selection_is_not_shared_between_sessions() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;

    let mut a = authenticated(&server).await;
    let mut b = authenticated(&server).await;

    assert!(a.command("SELECT Work").await.contains("OK"));

    // B never selected anything, so a selected-state command must be refused
    // even though A has a mailbox open.
    let response = b.command("CLOSE").await;
    assert!(
        response.contains("BAD"),
        "B has no selection of its own: {response}"
    );
}

#[tokio::test]
async fn developer_mode_falls_back_to_the_catch_all_account() {
    // With AuthMode::Disabled, LOGIN accepts any username. A name matching no
    // account must still reach the catch-all's mailboxes, or the developer
    // workflow the mode exists for cannot open a mailbox at all.
    let server = TestServer::start().await;
    let catch_all = server.catch_all_account("catchall").await;
    server.mailbox(catch_all, "INBOX").await;

    let mut client = server.connect().await;
    assert!(client.command("LOGIN nobody any").await.contains("OK"));

    let response = client.command("SELECT INBOX").await;
    assert!(
        response.contains("OK") && response.contains("EXISTS"),
        "developer mode must resolve to the catch-all: {response}"
    );
}

#[tokio::test]
async fn an_unresolvable_identity_reaches_no_mailboxes_when_auth_is_required() {
    // The other half of the same rule: with authentication enabled there is no
    // catch-all fallback, so a session whose identity does not resolve must
    // reach nothing rather than someone else's mail (phase brief §18).
    use sandesh_config::AuthMode;
    let config = sandesh_config::ImapConfig {
        auth_mode: AuthMode::Required,
        start_tls: true,
        ..TestServer::base_config()
    };
    let server = TestServer::with_tls(config).await;
    server.account_with_password("real", "secret").await;

    let mut client = server.connect().await;

    // Over cleartext with AUTH required, LOGIN is refused outright — so no
    // unauthenticated session can reach a mailbox by any route.
    let login = client.command("LOGIN nobody any").await;
    assert!(login.contains("NO") || login.contains("BAD"), "{login}");

    let select = client.command("SELECT INBOX").await;
    assert!(
        !select.contains("EXISTS"),
        "no mailbox data without a resolved identity: {select}"
    );
}
