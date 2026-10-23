//! `LIST-EXTENDED` (RFC 5258) and `SPECIAL-USE` (RFC 6154) over a real socket.
//!
//! Both extend one command, and both are driven through the production server
//! on a TCP connection against the real SQLite store — so a test cannot pass
//! against a shortcut the server does not take. Special-use attributes come
//! from persisted mailbox state, which is what makes the persistence tests here
//! meaningful rather than tautological.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;
use sandesh_core::MailboxAttributes;

async fn authenticated(server: &TestServer) -> common::Client<tokio::net::TcpStream> {
    let mut client = server.connect().await;
    let response = client.command("LOGIN user secret").await;
    assert!(response.contains("OK"), "login must succeed: {response}");
    client
}

/// The `* LIST` line naming `mailbox`, or `None`.
fn list_line(response: &str, mailbox: &str) -> Option<String> {
    response
        .lines()
        .find(|line| line.starts_with("* LIST") && line.trim_end().ends_with(mailbox))
        .map(String::from)
}

// --- plain LIST is preserved -------------------------------------------

#[tokio::test]
async fn plain_list_still_reports_mailboxes_and_the_delimiter() {
    // The regression guard: LIST-EXTENDED must not change what plain LIST
    // does for a client that asked for nothing extra.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" *").await;

    assert!(response.contains("* LIST ("), "{response:?}");
    assert!(response.contains("\"/\""), "{response:?}");
    assert!(response.contains("INBOX"), "{response:?}");
    assert!(response.contains("A2 OK"), "{response:?}");
}

#[tokio::test]
async fn an_empty_pattern_still_returns_the_delimiter_only() {
    // RFC 9051 §6.3.9, unchanged by the extensions: there is no mailbox to
    // report attributes or status for.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" \"\"").await;

    assert!(response.contains(r"\Noselect"), "{response:?}");
    assert!(!response.contains("INBOX"), "{response:?}");
}

#[tokio::test]
async fn a_nonexistent_pattern_is_ok_with_no_data_lines() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" NoSuchMailbox*").await;

    assert!(response.contains("A2 OK"), "{response:?}");
    assert!(!response.contains("* LIST"), "{response:?}");
}

#[tokio::test]
async fn listing_is_deterministically_ordered() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    for name in ["Zebra", "Alpha", "Middle"] {
        server.mailbox(account, name).await;
    }

    let mut client = authenticated(&server).await;
    let first = client.command("LIST \"\" *").await;
    let second = client.command("LIST \"\" *").await;

    let names = |response: &str| {
        response
            .lines()
            .filter(|line| line.starts_with("* LIST"))
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&first), names(&second));
    let alpha = first.find("Alpha").expect("Alpha listed");
    let middle = first.find("Middle").expect("Middle listed");
    let zebra = first.find("Zebra").expect("Zebra listed");
    assert!(alpha < middle && middle < zebra, "not sorted: {first:?}");
}

#[tokio::test]
async fn a_reference_name_scopes_the_listing() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;
    server.mailbox(account, "Personal").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST Work/ *").await;

    assert!(response.contains("Work/Deep"), "{response:?}");
    assert!(!response.contains("Personal"), "{response:?}");
}

// --- CHILDREN ----------------------------------------------------------

#[tokio::test]
async fn a_parent_is_reported_as_haschildren() {
    // RFC 5258 §3. Computed from the mailbox set, not stored — so it is
    // correct without any create/delete/rename bookkeeping.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" *").await;

    let work = list_line(&response, "Work").expect("Work listed");
    assert!(work.contains(r"\HasChildren"), "{work:?}");
    assert!(!work.contains(r"\HasNoChildren"), "{work:?}");
}

#[tokio::test]
async fn a_leaf_is_reported_as_hasnochildren() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" *").await;

    let deep = list_line(&response, "Work/Deep").expect("Work/Deep listed");
    assert!(deep.contains(r"\HasNoChildren"), "{deep:?}");
}

#[tokio::test]
async fn a_similarly_named_sibling_is_not_a_child() {
    // `Workshop` starts with `Work` but is not below it. The delimiter is part
    // of the prefix precisely so this cannot be confused.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Workshop").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" *").await;

    let work = list_line(&response, "Work").expect("Work listed");
    assert!(
        work.contains(r"\HasNoChildren"),
        "Workshop must not make Work a parent: {work:?}"
    );
}

#[tokio::test]
async fn children_excluded_by_the_pattern_still_count() {
    // The filtering trap. `LIST "" Work` returns only `Work`, but `Work` still
    // has a child — reporting `\HasNoChildren` here would tell the client the
    // hierarchy ends, and it would stop descending.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" Work").await;

    let work = list_line(&response, "Work").expect("Work listed");
    assert!(!response.contains("Work/Deep"), "{response:?}");
    assert!(work.contains(r"\HasChildren"), "{work:?}");
}

#[tokio::test]
async fn return_children_is_accepted_explicitly() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" * RETURN (CHILDREN)").await;

    assert!(response.contains("A2 OK"), "{response:?}");
    let work = list_line(&response, "Work").expect("Work listed");
    assert!(work.contains(r"\HasChildren"), "{work:?}");
}

// --- SUBSCRIBED --------------------------------------------------------

#[tokio::test]
async fn return_subscribed_marks_subscribed_mailboxes() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Kept").await;
    server.mailbox(account, "Ignored").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SUBSCRIBE Kept").await;
    let response = client.command("LIST \"\" * RETURN (SUBSCRIBED)").await;

    let kept = list_line(&response, "Kept").expect("Kept listed");
    let ignored = list_line(&response, "Ignored").expect("Ignored listed");
    assert!(kept.contains(r"\Subscribed"), "{kept:?}");
    assert!(!ignored.contains(r"\Subscribed"), "{ignored:?}");
}

#[tokio::test]
async fn subscribed_is_not_reported_unless_requested() {
    // A client that did not ask must not receive it (RFC 5258 §3.1).
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Kept").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SUBSCRIBE Kept").await;
    let response = client.command("LIST \"\" *").await;

    let kept = list_line(&response, "Kept").expect("Kept listed");
    assert!(!kept.contains(r"\Subscribed"), "{kept:?}");
}

#[tokio::test]
async fn the_subscribed_selection_option_filters_the_listing() {
    // RFC 5258 §3.1: the rev2 replacement for `LSUB`.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Kept").await;
    server.mailbox(account, "Ignored").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SUBSCRIBE Kept").await;
    let response = client.command("LIST (SUBSCRIBED) \"\" *").await;

    assert!(response.contains("Kept"), "{response:?}");
    assert!(!response.contains("Ignored"), "{response:?}");
    let kept = list_line(&response, "Kept").expect("Kept listed");
    assert!(
        kept.contains(r"\Subscribed"),
        "a subscribed-only listing implies the attribute: {kept:?}"
    );
}

#[tokio::test]
async fn unsubscribing_changes_the_listing() {
    // Subscription is read live, not cached at login. Asserted on the one
    // mailbox this test controls: account provisioning creates the default
    // mailboxes already subscribed, so the listing is never empty.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Kept").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SUBSCRIBE Kept").await;
    let before = client.command("LIST (SUBSCRIBED) \"\" *").await;
    let _ = client.command("UNSUBSCRIBE Kept").await;
    let after = client.command("LIST (SUBSCRIBED) \"\" *").await;

    assert!(list_line(&before, "Kept").is_some(), "{before:?}");
    assert!(
        list_line(&after, "Kept").is_none(),
        "an unsubscribed mailbox must leave the listing: {after:?}"
    );
    assert!(
        list_line(&after, "INBOX").is_some(),
        "other subscriptions are unaffected: {after:?}"
    );
}

// --- RETURN STATUS -----------------------------------------------------

#[tokio::test]
async fn return_status_emits_a_status_line_per_mailbox() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    server.deliver(inbox, b"Subject: one\r\n\r\nbody\r\n").await;

    let mut client = authenticated(&server).await;
    let response = client
        .command("LIST \"\" INBOX RETURN (STATUS (MESSAGES UIDNEXT))")
        .await;

    assert!(response.contains("* STATUS"), "{response:?}");
    assert!(response.contains("MESSAGES 1"), "{response:?}");
    assert!(response.contains("UIDNEXT"), "{response:?}");
    assert!(response.contains("A2 OK"), "{response:?}");
}

#[tokio::test]
async fn return_status_agrees_with_the_status_command() {
    // The values must come from the same port. A second counting path here
    // would be free to drift, and a client comparing the two would see it.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    let inbox = server.mailbox(account, "INBOX").await;
    for subject in ["one", "two", "three"] {
        server
            .deliver(
                inbox,
                format!("Subject: {subject}\r\n\r\nbody\r\n").as_bytes(),
            )
            .await;
    }

    let mut client = authenticated(&server).await;
    let via_status = client.command("STATUS INBOX (MESSAGES UNSEEN)").await;
    let via_list = client
        .command("LIST \"\" INBOX RETURN (STATUS (MESSAGES UNSEEN))")
        .await;

    let status_line = |response: &str| {
        response
            .lines()
            .find(|line| line.starts_with("* STATUS"))
            .map(str::trim_end)
            .map(String::from)
            .expect("a STATUS line")
    };
    assert_eq!(status_line(&via_status), status_line(&via_list));
}

#[tokio::test]
async fn return_status_validates_items_with_the_status_commands_rules() {
    // `RECENT` is refused by `STATUS` because this server keeps no `\Recent`
    // state; it must be refused here for the same reason, not accepted and
    // answered with a fabricated 0.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client
        .command("LIST \"\" INBOX RETURN (STATUS (RECENT))")
        .await;

    assert!(response.contains("A2 BAD"), "{response:?}");
    assert!(!response.contains("* STATUS"), "{response:?}");
}

#[tokio::test]
async fn return_status_on_an_empty_result_emits_nothing() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client
        .command("LIST \"\" NoSuch* RETURN (STATUS (MESSAGES))")
        .await;

    assert!(response.contains("A2 OK"), "{response:?}");
    assert!(!response.contains("* STATUS"), "{response:?}");
}

#[tokio::test]
async fn return_options_combine() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SUBSCRIBE Work").await;
    let response = client
        .command("LIST \"\" * RETURN (SUBSCRIBED CHILDREN STATUS (MESSAGES))")
        .await;

    let work = list_line(&response, "Work").expect("Work listed");
    assert!(work.contains(r"\Subscribed"), "{work:?}");
    assert!(work.contains(r"\HasChildren"), "{work:?}");
    assert!(response.contains("* STATUS"), "{response:?}");
    assert!(response.contains("A3 OK"), "{response:?}");
}

#[tokio::test]
async fn an_unsupported_return_option_is_refused() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" * RETURN (MADEUP)").await;

    assert!(response.contains("A2 BAD"), "{response:?}");
    assert!(!response.contains("* LIST"), "{response:?}");
}

#[tokio::test]
async fn an_unsupported_selection_option_is_refused() {
    // `RECURSIVEMATCH` changes which mailboxes are listed, so ignoring it
    // would answer a different question with a well-formed response.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST (RECURSIVEMATCH) \"\" *").await;

    assert!(response.contains("A2 BAD"), "{response:?}");
    assert!(!response.contains("* LIST"), "{response:?}");
}

// --- SPECIAL-USE -------------------------------------------------------

#[tokio::test]
async fn list_reports_special_use_attributes() {
    // RFC 6154 §2. These come from persisted mailbox state.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server
        .mailbox_with_attributes(account, "Sent", MailboxAttributes::from_names([r"\Sent"]))
        .await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" *").await;

    let sent = list_line(&response, "Sent").expect("Sent listed");
    assert!(sent.contains(r"\Sent"), "{sent:?}");
}

#[tokio::test]
async fn the_default_mailboxes_carry_their_standard_uses() {
    // Account provisioning assigns these, and clients rely on finding them
    // rather than creating duplicates.
    let server = TestServer::start().await;
    let _ = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" *").await;

    for (mailbox, attribute) in [
        ("Sent", r"\Sent"),
        ("Drafts", r"\Drafts"),
        ("Trash", r"\Trash"),
        ("Junk", r"\Junk"),
        ("Archive", r"\Archive"),
    ] {
        let line = list_line(&response, mailbox).unwrap_or_else(|| panic!("{mailbox} listed"));
        assert!(line.contains(attribute), "{mailbox}: {line:?}");
    }
}

#[tokio::test]
async fn inbox_carries_no_special_use_attribute() {
    // INBOX is required, not special-use. Assigning it one would be a
    // fabricated semantic — RFC 6154 defines no `\Inbox`.
    let server = TestServer::start().await;
    let _ = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST \"\" INBOX").await;

    let inbox = list_line(&response, "INBOX").expect("INBOX listed");
    for attribute in [
        r"\Sent",
        r"\Drafts",
        r"\Trash",
        r"\Junk",
        r"\Archive",
        r"\All",
    ] {
        assert!(
            !inbox.contains(attribute),
            "{attribute} on INBOX: {inbox:?}"
        );
    }
}

#[tokio::test]
async fn the_special_use_selection_option_filters_the_listing() {
    // RFC 6154 §3.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Ordinary").await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST (SPECIAL-USE) \"\" *").await;

    assert!(response.contains("Sent"), "{response:?}");
    assert!(
        !response.contains("Ordinary"),
        "a mailbox with no special use must be excluded: {response:?}"
    );
}

#[tokio::test]
async fn noselect_is_not_treated_as_a_special_use() {
    // The leakage trap. `\Noselect` is a name attribute, not an RFC 6154
    // special use, so it must not satisfy a `(SPECIAL-USE)` filter.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server
        .mailbox_with_attributes(
            account,
            "Container",
            MailboxAttributes::from_names([r"\Noselect"]),
        )
        .await;

    let mut client = authenticated(&server).await;
    let response = client.command("LIST (SPECIAL-USE) \"\" *").await;

    assert!(
        !response.contains("Container"),
        r"\Noselect must not pass a SPECIAL-USE filter: {response:?}"
    );
}

#[tokio::test]
async fn create_can_assign_a_special_use() {
    let server = TestServer::start().await;
    let _ = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let created = client.command(r"CREATE Keep (USE (\Archive))").await;
    assert!(created.contains("A2 OK"), "{created:?}");

    let response = client.command("LIST \"\" Keep").await;
    let keep = list_line(&response, "Keep").expect("Keep listed");
    assert!(keep.contains(r"\Archive"), "{keep:?}");
}

#[tokio::test]
async fn a_created_special_use_survives_a_reconnect() {
    // Persistence: the attribute is written to storage, not held in the
    // session. A new connection must see it.
    let server = TestServer::start().await;
    let _ = server.account_with_password("user", "secret").await;

    let mut first = authenticated(&server).await;
    let created = first.command(r"CREATE Keep (USE (\Archive))").await;
    assert!(created.contains("OK"), "{created:?}");
    let _ = first.command("LOGOUT").await;

    let mut second = authenticated(&server).await;
    let response = second.command("LIST \"\" Keep").await;

    let keep = list_line(&response, "Keep").expect("Keep listed after reconnect");
    assert!(keep.contains(r"\Archive"), "{keep:?}");
}

#[tokio::test]
async fn create_use_accepts_a_lowercase_attribute_and_stores_it_canonically() {
    // Attribute names are case-insensitive atoms, but the stored and reported
    // spelling must be the RFC's — a client matching on `\Archive` exactly
    // would otherwise miss it.
    let server = TestServer::start().await;
    let _ = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let created = client.command(r"CREATE Keep (USE (\archive))").await;
    assert!(created.contains("A2 OK"), "{created:?}");

    let response = client.command("LIST \"\" Keep").await;
    let keep = list_line(&response, "Keep").expect("Keep listed");
    assert!(keep.contains(r"\Archive"), "{keep:?}");
}

#[tokio::test]
async fn a_duplicated_use_is_assigned_once() {
    let server = TestServer::start().await;
    let _ = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let created = client
        .command(r"CREATE Keep (USE (\Archive \Archive))")
        .await;
    assert!(created.contains("A2 OK"), "{created:?}");

    let response = client.command("LIST \"\" Keep").await;
    let keep = list_line(&response, "Keep").expect("Keep listed");
    assert_eq!(
        keep.matches(r"\Archive").count(),
        1,
        "duplicate attribute emitted: {keep:?}"
    );
}

#[tokio::test]
async fn an_unknown_use_is_refused_with_useattr_and_creates_nothing() {
    // RFC 6154 §3 `NO [USEATTR]`. Validated before creation, so a refused
    // request leaves no mailbox behind.
    let server = TestServer::start().await;
    let _ = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let created = client.command(r"CREATE Keep (USE (\Nonsense))").await;

    assert!(created.contains("A2 NO"), "{created:?}");
    assert!(created.contains("USEATTR"), "{created:?}");

    let response = client.command("LIST \"\" Keep").await;
    assert!(
        !response.contains("* LIST"),
        "a refused CREATE must leave no mailbox: {response:?}"
    );
}

#[tokio::test]
async fn create_without_use_assigns_no_special_use() {
    let server = TestServer::start().await;
    let _ = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("CREATE Plain").await;
    let response = client.command("LIST \"\" Plain").await;

    let plain = list_line(&response, "Plain").expect("Plain listed");
    for attribute in [
        r"\Sent",
        r"\Drafts",
        r"\Trash",
        r"\Junk",
        r"\Archive",
        r"\All",
    ] {
        assert!(!plain.contains(attribute), "{attribute}: {plain:?}");
    }
}

#[tokio::test]
async fn special_use_and_subscription_are_independent() {
    // A special-use mailbox is not implicitly subscribed, and subscribing an
    // ordinary one does not give it a use.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Ordinary").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SUBSCRIBE Ordinary").await;
    let response = client.command("LIST \"\" * RETURN (SUBSCRIBED)").await;

    let ordinary = list_line(&response, "Ordinary").expect("Ordinary listed");
    assert!(ordinary.contains(r"\Subscribed"), "{ordinary:?}");
    assert!(!ordinary.contains(r"\Sent"), "{ordinary:?}");

    // `Sent` is provisioned subscribed *and* special-use, so it carries both.
    // The independence being asserted is that neither attribute implies the
    // other: `Ordinary` above is subscribed with no use, and unsubscribing
    // `Sent` here leaves its use intact.
    let _ = client.command("UNSUBSCRIBE Sent").await;
    let after = client.command("LIST \"\" * RETURN (SUBSCRIBED)").await;
    let sent = list_line(&after, "Sent").expect("Sent listed");
    assert!(
        sent.contains(r"\Sent"),
        "unsubscribing must not remove a special use: {sent:?}"
    );
    assert!(!sent.contains(r"\Subscribed"), "{sent:?}");
}

#[tokio::test]
async fn special_use_filter_combines_with_return_status() {
    let server = TestServer::start().await;
    let _ = server.account_with_password("user", "secret").await;

    let mut client = authenticated(&server).await;
    let response = client
        .command("LIST (SPECIAL-USE) \"\" * RETURN (STATUS (MESSAGES))")
        .await;

    assert!(response.contains("A2 OK"), "{response:?}");
    assert!(response.contains(r"\Sent"), "{response:?}");
    assert!(response.contains("* STATUS"), "{response:?}");
}
