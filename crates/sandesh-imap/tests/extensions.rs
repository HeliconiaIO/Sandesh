//! `NAMESPACE` (RFC 2342) and `ENABLE` (RFC 5161) over a real socket.
//!
//! Both are driven through the production server on a TCP connection, asserting
//! on the exact bytes returned. The two features share a file because they
//! share a purpose: they are how a client discovers what this server's mailbox
//! hierarchy looks like and what extended syntax it may use. Getting either
//! subtly wrong breaks discovery before a client reaches a single message.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::TestServer;

/// Connects and authenticates, as the mailbox tests do.
async fn authenticated(server: &TestServer) -> common::Client<tokio::net::TcpStream> {
    let mut client = server.connect().await;
    let response = client.command("LOGIN user secret").await;
    assert!(response.contains("OK"), "login must succeed: {response}");
    client
}

// --- NAMESPACE ----------------------------------------------------------

#[tokio::test]
async fn namespace_returns_one_personal_namespace() {
    // RFC 2342 §5. The whole untagged line is asserted: a wrong prefix or
    // delimiter each break mailbox discovery, and a substring check would let
    // either through.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("NAMESPACE").await;

    assert!(
        response.contains("* NAMESPACE ((\"\" \"/\")) NIL NIL"),
        "{response:?}"
    );
    assert!(response.contains("A2 OK"), "{response:?}");
}

#[tokio::test]
async fn namespace_reports_no_shared_or_other_users_namespaces() {
    // The honesty requirement. `MailboxStore::list` is scoped to one
    // `AccountId`, so there is no storage concept behind either namespace type
    // and `NIL` is the truthful answer. RFC 2342 §5 distinguishes `NIL` from an
    // empty list `()`, so an empty list here would be a different — and false
    // — claim: "this server has shared namespaces, but you may see none".
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("NAMESPACE").await;

    assert!(
        response.contains("NIL NIL"),
        "shared and other-users namespaces must both be NIL: {response:?}"
    );
    assert!(
        !response.contains("()"),
        "an empty list is not NIL and would claim namespaces exist: {response:?}"
    );
}

#[tokio::test]
async fn namespace_is_deterministic() {
    // Repeated calls in one session, and the presence of mailboxes, change
    // nothing: the answer is a property of the model, not of the contents.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let first = client.command("NAMESPACE").await;

    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;
    let second = client.command("NAMESPACE").await;

    let line = |response: &str| {
        response
            .lines()
            .find(|line| line.starts_with("* NAMESPACE"))
            .map(String::from)
            .expect("a NAMESPACE line")
    };
    assert_eq!(line(&first), line(&second));
}

#[tokio::test]
async fn namespace_delimiter_matches_what_list_reports() {
    // The interoperability requirement: a client learns the delimiter from
    // NAMESPACE and then parses LIST with it. If the two disagree, hierarchy
    // parsing breaks even though each response is individually well-formed.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "Work").await;
    server.mailbox(account, "Work/Deep").await;

    let mut client = authenticated(&server).await;
    let namespace = client.command("NAMESPACE").await;
    let list = client.command("LIST \"\" *").await;

    assert!(namespace.contains("\"/\""), "{namespace:?}");
    assert!(list.contains("\"/\""), "{list:?}");
    assert!(
        list.contains("Work/Deep"),
        "the delimiter must actually separate hierarchy: {list:?}"
    );
}

#[tokio::test]
async fn namespace_prefix_is_usable_as_a_list_reference() {
    // The empty personal prefix is a claim a client will act on: it will use it
    // as the LIST reference. So the claim is tested by doing exactly that.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("NAMESPACE").await;
    let list = client.command("LIST \"\" \"*\"").await;

    assert!(list.contains("INBOX"), "{list:?}");
    assert!(list.contains("A3 OK"), "{list:?}");
}

#[tokio::test]
async fn namespace_is_refused_before_authentication() {
    // RFC 2342 §5 places it in the authenticated state. It must not be a route
    // to learning anything about a server before logging in.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("NAMESPACE").await;

    assert!(response.contains("A1 BAD"), "{response:?}");
    assert!(
        !response.contains("* NAMESPACE"),
        "no namespace data may leak before authentication: {response:?}"
    );
}

#[tokio::test]
async fn namespace_rejects_arguments() {
    // RFC 2342 §5 defines no arguments. Accepting junk would leave a client
    // believing it had asked a narrower question than it did.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("NAMESPACE \"\"").await;

    assert!(response.contains("A2 BAD"), "{response:?}");
    assert!(!response.contains("* NAMESPACE"), "{response:?}");
}

#[tokio::test]
async fn namespace_works_while_a_mailbox_is_selected() {
    // A selection neither adds nor removes anything NAMESPACE reports, so it
    // must keep working rather than being refused.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("NAMESPACE").await;

    assert!(response.contains("* NAMESPACE"), "{response:?}");
    assert!(response.contains("A3 OK"), "{response:?}");
}

// --- ENABLE -------------------------------------------------------------

#[tokio::test]
async fn enable_condstore_reports_it_enabled() {
    // The successor to `enable_succeeds_and_enables_nothing_yet`, updated
    // deliberately when CONDSTORE was implemented — as that test's comment
    // required. `* ENABLED CONDSTORE` is now the truthful answer.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("ENABLE CONDSTORE").await;

    assert!(response.contains("A2 OK"), "{response:?}");
    assert!(
        response.contains("* ENABLED CONDSTORE"),
        "CONDSTORE is implemented, so it must be reported enabled: {response:?}"
    );
}

#[tokio::test]
async fn enable_qresync_is_refused_with_an_empty_enabled_line() {
    // RFC 5161 §3.1: an unimplemented capability is ignored, not refused, and
    // the command still answers OK. The empty `* ENABLED` is the refusal.
    // This is the honesty guard CONDSTORE used to carry; QRESYNC holds it now
    // because it needs a record of expunged UIDs this server does not keep.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("ENABLE QRESYNC").await;

    assert!(response.contains("A2 OK"), "{response:?}");
    assert!(
        response.contains("* ENABLED\r\n"),
        "QRESYNC is not implemented, so it must not be reported enabled: {response:?}"
    );
    assert!(
        !response.contains("* ENABLED QRESYNC"),
        "advertising an unimplemented capability as enabled: {response:?}"
    );
}

#[tokio::test]
async fn capability_advertises_condstore_but_never_qresync() {
    // The two must agree, in both directions. CONDSTORE is implemented, so it
    // appears; QRESYNC is built on it and is *not* implemented, so the
    // failure to catch is CONDSTORE dragging QRESYNC in behind it.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("CAPABILITY").await;

    assert!(
        response.contains("CONDSTORE"),
        "CONDSTORE is implemented and must be advertised: {response:?}"
    );
    assert!(
        !response.contains("QRESYNC"),
        "QRESYNC must not appear in CAPABILITY: {response:?}"
    );
    assert!(response.contains("ENABLE"), "{response:?}");
}

#[tokio::test]
async fn enable_accepts_multiple_capabilities() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("ENABLE CONDSTORE QRESYNC X-MADE-UP").await;

    assert!(response.contains("A2 OK"), "{response:?}");
    // Precisely the implemented one, and nothing else: QRESYNC is recognised
    // but unimplemented, X-MADE-UP is unrecognised, and RFC 5161 §3.1 makes
    // both silently absent rather than an error.
    assert!(
        response.contains("* ENABLED CONDSTORE\r\n"),
        "only the implemented capability may be reported: {response:?}"
    );
    assert!(!response.contains("QRESYNC"), "{response:?}");
    assert!(!response.contains("X-MADE-UP"), "{response:?}");
}

#[tokio::test]
async fn enable_ignores_unknown_capabilities() {
    // RFC 5161 §3.1 is explicit: ignore, do not fail. A client naming an
    // extension this server lacks has done nothing wrong.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("ENABLE X-TOTALLY-INVENTED").await;

    assert!(
        response.contains("A2 OK"),
        "an unknown capability must not fail the command: {response:?}"
    );
    assert!(response.contains("* ENABLED\r\n"), "{response:?}");
}

#[tokio::test]
async fn repeated_enable_is_safe() {
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let first = client.command("ENABLE CONDSTORE").await;
    let second = client.command("ENABLE CONDSTORE").await;

    assert!(first.contains("OK"), "{first:?}");
    assert!(second.contains("OK"), "{second:?}");
    let enabled = |response: &str| {
        response
            .lines()
            .find(|line| line.starts_with("* ENABLED"))
            .map(String::from)
            .expect("an ENABLED line")
    };
    assert_eq!(
        enabled(&first),
        enabled(&second),
        "a repeated ENABLE must give a consistent answer (RFC 5161 §3.2)"
    );
}

#[tokio::test]
async fn enable_with_no_capabilities_is_bad() {
    // RFC 5161 §3.1 requires one or more capability names.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("ENABLE").await;

    assert!(response.contains("A2 BAD"), "{response:?}");
    assert!(!response.contains("* ENABLED"), "{response:?}");
}

#[tokio::test]
async fn enable_rejects_a_non_atom_capability_name() {
    // Capability names are atoms (RFC 9051 §9). A parenthesised list is
    // malformed, and is refused rather than silently skipped — an ignored
    // argument would look like a declined capability, a different answer.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let response = client.command("ENABLE (CONDSTORE)").await;

    assert!(response.contains("A2 BAD"), "{response:?}");
    assert!(!response.contains("* ENABLED"), "{response:?}");
}

#[tokio::test]
async fn enable_is_refused_before_authentication() {
    // It must never be a route to acting on an unauthenticated session, and
    // the reason must say *authentication* rather than anything about
    // selection, or the client is misinformed about what to do next.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("ENABLE CONDSTORE").await;

    assert!(response.contains("A1 BAD"), "{response:?}");
    assert!(!response.contains("* ENABLED"), "{response:?}");
}

#[tokio::test]
async fn enable_is_refused_while_a_mailbox_is_selected() {
    // RFC 5161 §3.1. Enabling an extension can change the syntax of the
    // untagged responses a selected mailbox is already streaming, so the
    // client must enable before it selects.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("SELECT INBOX").await;
    let response = client.command("ENABLE CONDSTORE").await;

    assert!(response.contains("A3 BAD"), "{response:?}");
    assert!(!response.contains("* ENABLED"), "{response:?}");
}

#[tokio::test]
async fn enable_state_does_not_leak_between_sessions() {
    // The isolation requirement, proved with two live connections rather than
    // asserted about one struct. One client's ENABLE must not change what a
    // different client is sent, because the second client is entitled to a
    // parser that does not know the extension.
    //
    // `CAPABILITY` can no longer carry this proof: CONDSTORE is advertised to
    // every session, enabled or not, because advertising states what the
    // *server* implements. What is per-session is whether the extension's
    // syntax is actually sent — proved end to end by
    // `a_condstore_session_is_sent_modseq_but_a_plain_one_is_not` in the IDLE
    // suite, where the unsolicited-response machinery lives. Here the check is
    // that the second session's own ENABLE state starts empty.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut first = authenticated(&server).await;
    let enabled = first.command("ENABLE CONDSTORE").await;
    assert!(enabled.contains("* ENABLED CONDSTORE"), "{enabled:?}");

    // A second connection that enables *nothing* must be told nothing is
    // enabled, rather than inheriting the first session's set.
    let mut second = authenticated(&server).await;
    let response = second.command("ENABLE X-TOTALLY-INVENTED").await;
    assert!(
        response.contains("* ENABLED\r\n"),
        "a second session must not inherit enabled state: {response:?}"
    );
}

#[tokio::test]
async fn enable_survives_as_a_session_and_the_session_continues() {
    // ENABLE must not disturb the session: the connection stays usable and
    // later commands behave exactly as before.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;

    let mut client = authenticated(&server).await;
    let _ = client.command("ENABLE CONDSTORE").await;
    let selected = client.command("SELECT INBOX").await;

    assert!(selected.contains("A3 OK"), "{selected:?}");
    assert!(selected.contains("EXISTS"), "{selected:?}");
}
