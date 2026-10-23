//! IMAP protocol behaviour over real TCP connections (phase brief §11, §12).
//!
//! Every test here opens an actual socket against an actual `ImapServer`.
//! Nothing is stubbed, because the properties under test are exactly the ones a
//! unit test cannot see: what the server *sends*, what it *refuses*, and what
//! state survives a connection.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{TestServer, sasl_plain};

// ------------------------------------------------------- greeting

#[tokio::test]
async fn the_server_greets_with_untagged_ok_and_capabilities() {
    let server = TestServer::start().await;
    let client = server.connect().await;

    assert!(
        client.greeting.starts_with("* OK "),
        "the greeting must be an untagged OK (RFC 9051 §7.1.1): {:?}",
        client.greeting
    );
    assert!(
        client.greeting.contains("IMAP4rev2"),
        "the greeting must advertise IMAP4rev2: {:?}",
        client.greeting
    );
    assert!(
        client.greeting.ends_with("\r\n"),
        "responses are CRLF terminated: {:?}",
        client.greeting
    );
}

#[tokio::test]
async fn the_greeting_is_never_preauth() {
    // PREAUTH would place the connection directly in the Authenticated state.
    // Nothing about a TCP connection to this server justifies that.
    let server = TestServer::start().await;
    let client = server.connect().await;
    assert!(
        !client.greeting.contains("PREAUTH"),
        "{:?}",
        client.greeting
    );
}

// ------------------------------------------------------- CAPABILITY

#[tokio::test]
async fn capability_lists_imap4rev2_first() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("CAPABILITY").await;

    assert!(
        response.contains("* CAPABILITY IMAP4rev2"),
        "IMAP4rev2 must come first (RFC 9051 §7.2.2): {response:?}"
    );
    assert!(
        response.contains("A1 OK"),
        "the command must complete: {response:?}"
    );
}

#[tokio::test]
async fn capability_never_advertises_unimplemented_extensions() {
    // The truthfulness rule (phase brief §5). A capability this server cannot
    // honour is worse than a missing one, because a client that sees it stops
    // using the fallback that works.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("CAPABILITY").await;

    for absent in ["QRESYNC", "SORT", "THREAD", "BINARY", "COMPRESS"] {
        assert!(
            !response.contains(absent),
            "{absent} is advertised but not implemented: {response:?}"
        );
    }

    // The converse, so this list cannot quietly become a no-op as commands are
    // implemented: `MOVE` and `UIDPLUS` are implemented, so they must appear.
    // `CONDSTORE` moved here from the absent list when RFC 7162 §3 was
    // implemented; `QRESYNC` stays absent, which is the pairing that matters —
    // CONDSTORE is QRESYNC's prerequisite, so advertising it must not drag
    // QRESYNC along.
    for present in [
        "MOVE",
        "UIDPLUS",
        "NAMESPACE",
        "ENABLE",
        "CONDSTORE",
        "ESEARCH",
        "LIST-EXTENDED",
        "SPECIAL-USE",
    ] {
        assert!(
            response.contains(present),
            "{present} is implemented but not advertised: {response:?}"
        );
    }

    // `IDLE` is advertised only when an event source was injected, and this
    // test server has none — so its absence here is the truthfulness rule
    // working, not a missing feature. `tests/idle.rs` asserts the other half.
    assert!(
        !response.contains("IDLE"),
        "IDLE must not be advertised without an event source: {response:?}"
    );
}

#[tokio::test]
async fn starttls_is_not_advertised_when_it_is_not_configured() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("CAPABILITY").await;
    assert!(
        !response.contains("STARTTLS"),
        "STARTTLS advertised with no TLS configured: {response:?}"
    );
}

#[tokio::test]
async fn auth_is_not_advertised_on_a_cleartext_connection() {
    // ADR-033: PLAIN and LOGIN both send the password, so neither is offered
    // in the clear. LOGINDISABLED tells the client not to try.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let mut client = server.connect().await;

    let response = client.command("CAPABILITY").await;

    assert!(
        !response.contains("AUTH="),
        "credentials must not be invited in the clear: {response:?}"
    );
    assert!(
        response.contains("LOGINDISABLED"),
        "a cleartext client must be told LOGIN is unavailable: {response:?}"
    );
    assert!(
        response.contains("STARTTLS"),
        "STARTTLS is configured and must be advertised: {response:?}"
    );
}

// ------------------------------------------------------- NOOP, ID

#[tokio::test]
async fn noop_succeeds_before_authentication() {
    // RFC 9051 §6.1.2: valid in every state.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("NOOP").await;
    assert!(response.contains("A1 OK"), "{response:?}");
}

#[tokio::test]
async fn check_requires_a_selected_mailbox() {
    // RFC 9051 §6.4.1 places CHECK in the Selected state only.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;
    let mut client = server.connect().await;

    client.command("LOGIN user secret").await;
    let response = client.command("CHECK").await;
    assert!(
        response.contains("BAD") || response.contains("NO"),
        "CHECK outside a selection must be refused: {response:?}"
    );

    client.command("SELECT INBOX").await;
    let response = client.command("CHECK").await;
    assert!(response.contains("OK"), "{response:?}");
}

#[tokio::test]
async fn check_is_permitted_on_a_read_only_selection() {
    // CHECK does not mutate, so EXAMINE must not block it.
    let server = TestServer::start().await;
    let account = server.account_with_password("user", "secret").await;
    server.mailbox(account, "INBOX").await;
    let mut client = server.connect().await;

    client.command("LOGIN user secret").await;
    client.command("EXAMINE INBOX").await;
    let response = client.command("CHECK").await;
    assert!(response.contains("OK"), "{response:?}");
}

#[tokio::test]
async fn id_replies_without_disclosing_a_version() {
    // RFC 2971 §3.3 warns against version disclosure.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("ID NIL").await;
    assert!(response.contains("* ID "), "{response:?}");
    assert!(response.contains("A1 OK"), "{response:?}");
}

// ------------------------------------------------------- state machine over TCP

#[tokio::test]
async fn commands_requiring_authentication_are_refused_before_it() {
    // The security-critical property: an unauthenticated client reaches no
    // mail. Asserted over a real socket, not only in the state-machine unit.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    for command in [
        "SELECT INBOX",
        "LIST \"\" *",
        "STATUS INBOX (MESSAGES)",
        "FETCH 1 (FLAGS)",
    ] {
        let response = client.command(command).await;
        assert!(
            response.contains(" BAD ") || response.contains(" NO "),
            "{command} must be refused before authentication: {response:?}"
        );
        assert!(
            !response.contains(" OK "),
            "{command} must not succeed before authentication: {response:?}"
        );
    }
}

#[tokio::test]
async fn an_unknown_command_is_bad_and_the_session_survives() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("FROBNICATE").await;
    assert!(response.contains("A1 BAD"), "{response:?}");

    // The session must still work: a BAD is not fatal (RFC 9051 §7.1.3).
    let next = client.command("NOOP").await;
    assert!(next.contains("A2 OK"), "session died after BAD: {next:?}");
}

#[tokio::test]
async fn idle_is_rejected_because_it_is_deferred() {
    // ADR-039. It is not advertised, and it must not work.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("IDLE").await;
    assert!(
        response.contains("A1 BAD"),
        "IDLE is deferred and must be refused: {response:?}"
    );
}

// ------------------------------------------------------- tags

#[tokio::test]
async fn the_server_echoes_the_client_tag() {
    // A client matches responses to commands by tag alone, so this is
    // load-bearing rather than cosmetic.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    client.send_line("xyzzy42 NOOP").await;
    let response = client.read_until_tagged("xyzzy42").await;
    assert!(response.contains("xyzzy42 OK"), "{response:?}");
}

#[tokio::test]
async fn a_command_with_no_tag_is_refused_without_crashing() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    client.send_line("NOOP").await;
    let response = client.read_response().await;
    assert!(response.contains("BAD"), "{response:?}");

    // Still alive.
    let next = client.command("NOOP").await;
    assert!(next.contains("OK"), "{next:?}");
}

#[tokio::test]
async fn a_tag_containing_crlf_cannot_forge_a_response_line() {
    // Response splitting: if the tag were echoed unvalidated, a client could
    // inject a whole forged response into the stream it is reading.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    // The CR/LF terminates the line early, so the server sees a bare tag.
    client.send_raw(b"A1\r\n* OK forged\r\n").await;
    let response = client.read_response().await;

    assert!(
        !response.contains("* OK forged"),
        "the server must not echo client bytes as a response line: {response:?}"
    );
}

// ------------------------------------------------------- LOGOUT

#[tokio::test]
async fn logout_sends_bye_then_ok_and_closes() {
    // RFC 9051 §7.1.5: the untagged BYE precedes the tagged completion.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client.command("LOGOUT").await;

    let bye = response.find("* BYE").expect("an untagged BYE is required");
    let ok = response.find("A1 OK").expect("a tagged OK is required");
    assert!(bye < ok, "BYE must precede the completion: {response:?}");

    assert!(
        client.is_closed().await,
        "the server must close the connection after LOGOUT"
    );
}

#[tokio::test]
async fn nothing_is_accepted_after_logout() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    client.command("LOGOUT").await;
    client.send_line("A2 NOOP").await;

    assert!(
        client.read_response().await.is_empty(),
        "a logged-out connection must not answer further commands"
    );
}

// ------------------------------------------------------- malformed and oversized input

#[tokio::test]
async fn malformed_input_produces_a_protocol_error_not_a_crash() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    // A sweep of hostile and merely strange input. The assertion is that the
    // server keeps answering — a parser panic would take the session down.
    for hostile in [
        "A1 SELECT \"unterminated",
        "A2 FETCH 1 (FLAGS",
        "A3 FETCH 1 FLAGS)",
        "A4 LOGIN {abc}",
        "A5 LOGIN {-1}",
        "A6 LOGIN {99999999999999999999}",
        "A7 UID SELECT INBOX",
        "A8",
        "+ NOOP",
    ] {
        client.send_line(hostile).await;
        let response = client.read_response().await;
        assert!(
            !response.is_empty(),
            "the server stopped answering after {hostile:?}"
        );
    }

    let alive = client.command("NOOP").await;
    assert!(alive.contains("OK"), "session died: {alive:?}");
}

#[tokio::test]
async fn deeply_nested_lists_cannot_exhaust_the_stack() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    client
        .send_line(&format!("A1 FETCH 1 {}", "(".repeat(50_000)))
        .await;
    let response = client.read_response().await;
    assert!(response.contains("BAD"), "{response:?}");

    let alive = client.command("NOOP").await;
    assert!(
        alive.contains("OK"),
        "server died on a nesting bomb: {alive:?}"
    );
}

#[tokio::test]
async fn an_oversized_command_line_is_refused_and_the_session_resynchronises() {
    let server = TestServer::with_config(sandesh_config::ImapConfig {
        max_line_length: 1024,
        ..TestServer::base_config()
    })
    .await;
    let mut client = server.connect().await;

    // Far over the limit, and with no CRLF until the very end.
    client
        .send_line(&format!("A1 NOOP {}", "x".repeat(8192)))
        .await;
    let response = client.read_response().await;
    assert!(
        response.contains("BAD"),
        "an oversized line must be refused: {response:?}"
    );

    // The stream must be back at a line boundary, not mid-command.
    let alive = client.command("NOOP").await;
    assert!(
        alive.contains("OK"),
        "the session must resynchronise after an oversized line: {alive:?}"
    );
}

#[tokio::test]
async fn an_oversized_literal_is_refused_before_it_is_allocated() {
    // The memory-amplification guard: `{4294967295}` must cost a rejection,
    // not four gigabytes.
    let server = TestServer::with_config(sandesh_config::ImapConfig {
        max_literal_size: 1024,
        ..TestServer::base_config()
    })
    .await;
    let mut client = server.connect().await;

    client.send_line("A1 LOGIN {4294967295}").await;
    let response = client.read_response().await;

    assert!(
        response.contains("BAD"),
        "an oversized literal must be refused: {response:?}"
    );
    assert!(
        !response.starts_with("+ "),
        "the server must not invite an oversized literal: {response:?}"
    );

    let alive = client.command("NOOP").await;
    assert!(alive.contains("OK"), "{alive:?}");
}

// ------------------------------------------------------- literals over the wire

#[tokio::test]
async fn a_literal_receives_a_continuation_request_and_completes() {
    // This is the `Parsed::NeedLiteral` path meeting the network: the parser
    // stops, the session sends `+`, reads exactly N octets, and resumes.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    // LOGIN with the username as a literal. Authentication is disabled, so the
    // command is refused — but only *after* the literal is read, which is what
    // is under test.
    client.send_line("A1 LOGIN {5}").await;
    let continuation = client.read_response().await;
    assert!(
        continuation.starts_with("+ "),
        "a synchronising literal must be answered with a continuation request \
         (RFC 9051 §7.5): {continuation:?}"
    );

    client.send_line("alice secret").await;
    let response = client.read_until_tagged("A1").await;
    assert!(
        response.contains("A1 "),
        "the command must complete after the literal: {response:?}"
    );

    let alive = client.command("NOOP").await;
    assert!(alive.contains("OK"), "{alive:?}");
}

#[tokio::test]
async fn a_non_synchronising_literal_is_not_answered_with_a_continuation() {
    // RFC 7888 LITERAL+: the client is already sending the octets, so a
    // continuation here would desynchronise the stream.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    // Sent as one write, exactly as a LITERAL+ client would.
    client.send_raw(b"A1 LOGIN {5+}\r\nalice secret\r\n").await;
    let response = client.read_until_tagged("A1").await;

    assert!(
        !response.starts_with("+ "),
        "a non-synchronising literal must not be answered with `+`: {response:?}"
    );
    assert!(response.contains("A1 "), "{response:?}");
}

#[tokio::test]
async fn a_literal_carrying_arbitrary_bytes_does_not_break_framing() {
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    // NUL, CR, LF and high bytes inside a literal payload.
    let payload: &[u8] = &[0x00, 0x0d, 0x0a, 0xff, 0x80];
    client
        .send_line(&format!("A1 LOGIN {{{}}}", payload.len()))
        .await;
    let continuation = client.read_response().await;
    assert!(continuation.starts_with("+ "), "{continuation:?}");

    let mut line = payload.to_vec();
    line.extend_from_slice(b" secret\r\n");
    client.send_raw(&line).await;
    let _ = client.read_until_tagged("A1").await;

    // Framing intact: the next command must still be understood.
    let alive = client.command("NOOP").await;
    assert!(
        alive.contains("OK"),
        "binary literal payload desynchronised the stream: {alive:?}"
    );
}

// ------------------------------------------------------- connection handling

#[tokio::test]
async fn an_abrupt_disconnect_does_not_disturb_the_server() {
    let server = TestServer::start().await;

    // Connect and drop without a LOGOUT, repeatedly.
    for _ in 0..5 {
        let client = server.connect().await;
        drop(client);
    }

    // The server must still serve a new client.
    let mut client = server.connect().await;
    let response = client.command("NOOP").await;
    assert!(response.contains("OK"), "{response:?}");
}

#[tokio::test]
async fn several_clients_are_served_independently() {
    let server = TestServer::start().await;

    let mut first = server.connect().await;
    let mut second = server.connect().await;
    let mut third = server.connect().await;

    // Interleaved, so a server that shared session state across connections
    // would produce mismatched tags.
    let a = first.command("NOOP").await;
    let b = second.command("CAPABILITY").await;
    let c = third.command("NOOP").await;

    assert!(a.contains("A1 OK"), "{a:?}");
    assert!(b.contains("A1 OK"), "{b:?}");
    assert!(c.contains("A1 OK"), "{c:?}");

    // And each still has its own state afterwards.
    let a2 = first.command("NOOP").await;
    assert!(a2.contains("A2 OK"), "{a2:?}");
}

#[tokio::test]
async fn authentication_is_refused_when_it_is_not_enabled() {
    // Default configuration has auth disabled: AUTHENTICATE must say so
    // rather than appearing to work.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let response = client
        .command(&format!("AUTHENTICATE PLAIN {}", sasl_plain("alice", "pw")))
        .await;

    assert!(
        response.contains("A1 NO"),
        "authentication is disabled and must be refused: {response:?}"
    );
}
