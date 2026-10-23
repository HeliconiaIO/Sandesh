//! `AUTHENTICATE`, `LOGIN` and TLS over real sockets (phase brief §11, §12).
//!
//! These drive an actual server with an actual `rustls` handshake. Nothing is
//! stubbed: credentials are Argon2id hashes written through the production code
//! path, so a test cannot pass against a comparison production does not make.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{Client, TestServer, sasl_field, sasl_plain, tls_client};
use sandesh_config::{AuthMode, ImapConfig};

/// Completes a `STARTTLS` upgrade and returns a client on the encrypted stream.
///
/// The full sequence a real client performs: greeting, `STARTTLS`, handshake.
async fn upgrade(
    server: &TestServer,
) -> Client<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let mut cleartext = server.connect().await;

    let response = cleartext.command("STARTTLS").await;
    assert!(
        response.contains("A1 OK"),
        "STARTTLS must be accepted when advertised: {response:?}"
    );

    let stream = tls_client::handshake(cleartext.into_inner()).await;
    Client::new(stream)
}

// ------------------------------------------------------- STARTTLS

#[tokio::test]
async fn starttls_upgrades_to_a_real_tls_connection() {
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let mut client = upgrade(&server).await;

    // The handshake succeeded, so the encrypted channel must now speak IMAP.
    let response = client.command("CAPABILITY").await;
    assert!(
        response.contains("* CAPABILITY IMAP4rev2"),
        "the encrypted channel must speak IMAP: {response:?}"
    );
}

#[tokio::test]
async fn capabilities_are_recalculated_after_starttls() {
    // The whole point of the upgrade: AUTH appears, STARTTLS disappears.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;

    // Before: no AUTH, STARTTLS offered.
    let mut cleartext = server.connect().await;
    let before = cleartext.command("CAPABILITY").await;
    assert!(!before.contains("AUTH="), "{before:?}");
    assert!(before.contains("STARTTLS"), "{before:?}");
    assert!(before.contains("LOGINDISABLED"), "{before:?}");
    drop(cleartext);

    // After: AUTH offered, STARTTLS gone.
    let mut client = upgrade(&server).await;
    let after = client.command("CAPABILITY").await;
    assert!(
        after.contains("AUTH=PLAIN") && after.contains("AUTH=LOGIN"),
        "AUTH must be offered once encrypted: {after:?}"
    );
    assert!(
        !after.contains("STARTTLS"),
        "STARTTLS must not be advertised inside TLS (RFC 9051 §6.2.1): {after:?}"
    );
    assert!(
        !after.contains("LOGINDISABLED"),
        "LOGIN is available now, so LOGINDISABLED would be a lie: {after:?}"
    );
}

#[tokio::test]
async fn the_server_sends_no_greeting_after_starttls() {
    // RFC 9051 §6.2.1: the server must not re-issue a greeting after the
    // handshake. A client that waits for one would hang.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let mut client = upgrade(&server).await;

    let response = client.command("NOOP").await;
    assert!(
        !response.contains("* OK ["),
        "no second greeting may follow STARTTLS: {response:?}"
    );
    assert!(response.contains("A1 OK"), "{response:?}");
}

#[tokio::test]
async fn starttls_is_refused_when_no_certificate_is_configured() {
    // Advertisement and behaviour must agree: with no TLS material the
    // capability is absent, and the command must be refused rather than
    // accepted into a handshake that cannot happen.
    let server = TestServer::with_config(ImapConfig {
        auth_mode: AuthMode::Required,
        start_tls: false,
        ..TestServer::base_config()
    })
    .await;
    let mut client = server.connect().await;

    let response = client.command("STARTTLS").await;
    assert!(
        response.contains("A1 BAD"),
        "STARTTLS must be refused when unavailable: {response:?}"
    );
}

#[tokio::test]
async fn starttls_is_refused_once_authenticated() {
    // RFC 9051 §6.2.1 forbids renegotiating under an established identity.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut client = upgrade(&server).await;
    let auth = client
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("alice", "correct horse")
        ))
        .await;
    assert!(auth.contains("A1 OK"), "{auth:?}");

    let response = client.command("STARTTLS").await;
    assert!(
        response.contains("A2 BAD"),
        "STARTTLS must be refused after authentication: {response:?}"
    );
}

#[tokio::test]
async fn a_failed_tls_handshake_does_not_disturb_the_server() {
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;

    // Accept STARTTLS, then send garbage instead of a ClientHello.
    let mut client = server.connect().await;
    client.command("STARTTLS").await;
    client.send_raw(b"this is not a TLS ClientHello\r\n").await;
    drop(client);

    // A new client must still be served.
    let mut fresh = server.connect().await;
    let response = fresh.command("CAPABILITY").await;
    assert!(response.contains("IMAP4rev2"), "{response:?}");
}

// ------------------------------------------------------- implicit TLS

#[tokio::test]
async fn implicit_tls_serves_the_greeting_inside_tls() {
    let server = TestServer::with_tls(TestServer::implicit_tls_only()).await;

    let stream = tls_client::connect(server.implicit()).await;
    let mut client = Client::new(stream);
    let greeting = client.read_greeting().await;

    assert!(
        greeting.starts_with("* OK "),
        "the greeting must arrive inside TLS: {greeting:?}"
    );
    assert!(greeting.contains("IMAP4rev2"), "{greeting:?}");
}

#[tokio::test]
async fn implicit_tls_offers_auth_immediately_and_never_starttls() {
    let server = TestServer::with_tls(TestServer::implicit_tls_only()).await;

    let stream = tls_client::connect(server.implicit()).await;
    let mut client = Client::new(stream);
    client.read_greeting().await;

    let response = client.command("CAPABILITY").await;
    assert!(
        response.contains("AUTH=PLAIN"),
        "an encrypted connection offers AUTH from the outset: {response:?}"
    );
    assert!(
        !response.contains("STARTTLS"),
        "there is nothing to upgrade on an implicit-TLS port: {response:?}"
    );
}

#[tokio::test]
async fn implicit_tls_serves_no_plaintext_before_the_handshake() {
    // The property that makes the mode meaningful (ADR-037): a plaintext
    // client must get nothing at all, not a greeting it could then downgrade.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let server = TestServer::with_tls(TestServer::implicit_tls_only()).await;
    let mut stream = tokio::net::TcpStream::connect(server.implicit())
        .await
        .expect("connect");

    // Speak cleartext IMAP at a TLS-only port.
    stream.write_all(b"A1 CAPABILITY\r\n").await.expect("write");
    stream.flush().await.expect("flush");

    let mut buf = [0u8; 256];
    let read = tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut buf)).await;

    let bytes = match read {
        Ok(Ok(n)) => &buf[..n],
        // A closed or stalled connection is the correct outcome.
        _ => &[],
    };
    let text = String::from_utf8_lossy(bytes);

    assert!(
        !text.contains("IMAP4rev2") && !text.contains("* OK"),
        "an implicit-TLS port must never serve a plaintext greeting: {text:?}"
    );
}

#[tokio::test]
async fn authentication_succeeds_over_implicit_tls() {
    let server = TestServer::with_tls(TestServer::implicit_tls_only()).await;
    server.account_with_password("alice", "correct horse").await;

    let stream = tls_client::connect(server.implicit()).await;
    let mut client = Client::new(stream);
    client.read_greeting().await;

    let response = client
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("alice", "correct horse")
        ))
        .await;
    assert!(response.contains("A1 OK"), "{response:?}");
}

// ------------------------------------------------------- AUTHENTICATE PLAIN

#[tokio::test]
async fn plain_authentication_succeeds_with_an_initial_response() {
    // SASL-IR (RFC 4959): the credential rides along with the command.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut client = upgrade(&server).await;
    let response = client
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("alice", "correct horse")
        ))
        .await;

    assert!(response.contains("A1 OK"), "{response:?}");
    assert!(
        response.contains("CAPABILITY"),
        "the completion should carry the new capability list (RFC 9051 §6.2.2): {response:?}"
    );
}

#[tokio::test]
async fn plain_authentication_succeeds_through_a_continuation() {
    // No initial response: the server must send a bare `+` and wait.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut client = upgrade(&server).await;

    client.send_line("A1 AUTHENTICATE PLAIN").await;
    let continuation = client.read_response().await;
    assert!(
        continuation.starts_with("+ "),
        "a mechanism with no initial response needs a continuation: {continuation:?}"
    );

    client
        .send_line(&sasl_plain("alice", "correct horse"))
        .await;
    let response = client.read_until_tagged("A1").await;
    assert!(response.contains("A1 OK"), "{response:?}");
}

#[tokio::test]
async fn an_invalid_password_is_refused() {
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut client = upgrade(&server).await;
    let response = client
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("alice", "wrong password")
        ))
        .await;

    assert!(response.contains("A1 NO"), "{response:?}");
}

#[tokio::test]
async fn an_unknown_user_is_indistinguishable_from_a_wrong_password() {
    // Anti-enumeration (ADR-032): the two failures must look identical, or the
    // server becomes an oracle for which usernames exist.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut first = upgrade(&server).await;
    let wrong_password = first
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("alice", "wrong")
        ))
        .await;

    let mut second = upgrade(&server).await;
    let unknown_user = second
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("nobody", "wrong")
        ))
        .await;

    let strip = |text: &str| text.replace("A1 ", "");
    assert_eq!(
        strip(&wrong_password),
        strip(&unknown_user),
        "an unknown user must not be distinguishable from a bad password"
    );
}

#[tokio::test]
async fn an_account_with_no_password_cannot_authenticate() {
    // A credential-less account must not be a way in.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account("alice").await;

    let mut client = upgrade(&server).await;
    let response = client
        .command(&format!("AUTHENTICATE PLAIN {}", sasl_plain("alice", "")))
        .await;

    assert!(
        response.contains("A1 NO"),
        "an account with no credential must not authenticate: {response:?}"
    );
}

// ------------------------------------------------------- AUTHENTICATE LOGIN

#[tokio::test]
async fn login_mechanism_succeeds_through_two_prompts() {
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut client = upgrade(&server).await;

    client.send_line("A1 AUTHENTICATE LOGIN").await;
    let username_prompt = client.read_response().await;
    assert!(username_prompt.starts_with("+ "), "{username_prompt:?}");

    client.send_line(&sasl_field("alice")).await;
    let password_prompt = client.read_response().await;
    assert!(password_prompt.starts_with("+ "), "{password_prompt:?}");

    client.send_line(&sasl_field("correct horse")).await;
    let response = client.read_until_tagged("A1").await;
    assert!(response.contains("A1 OK"), "{response:?}");
}

// ------------------------------------------------------- malformed exchanges

#[tokio::test]
async fn an_unsupported_mechanism_is_refused_without_ending_the_session() {
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let mut client = upgrade(&server).await;

    let response = client.command("AUTHENTICATE CRAM-MD5").await;
    assert!(
        response.contains("A1 NO"),
        "an unimplemented mechanism is NO (RFC 9051 §6.2.2): {response:?}"
    );

    // The client must be able to try another mechanism.
    let alive = client.command("CAPABILITY").await;
    assert!(alive.contains("A2 OK"), "{alive:?}");
}

#[tokio::test]
async fn authenticate_with_no_mechanism_is_bad() {
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let mut client = upgrade(&server).await;

    let response = client.command("AUTHENTICATE").await;
    assert!(response.contains("A1 BAD"), "{response:?}");
}

#[tokio::test]
async fn a_malformed_sasl_payload_is_refused() {
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let mut client = upgrade(&server).await;

    for payload in ["not base64 at all!!", "", &sasl_field("no-nul-separators")] {
        let tag = client.next_tag();
        client
            .send_line(&format!("{tag} AUTHENTICATE PLAIN {payload}"))
            .await;
        let response = client.read_until_tagged(&tag).await;
        assert!(
            response.contains(" NO ") || response.contains(" BAD ") || response.starts_with("+ "),
            "a malformed payload must not authenticate: {response:?}"
        );
        assert!(
            !response.contains(" OK "),
            "a malformed payload must never succeed: {response:?}"
        );
    }
}

#[tokio::test]
async fn a_cancelled_exchange_is_refused() {
    // RFC 9051 §6.2.2: a lone `*` cancels.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let mut client = upgrade(&server).await;

    client.send_line("A1 AUTHENTICATE PLAIN").await;
    let continuation = client.read_response().await;
    assert!(continuation.starts_with("+ "), "{continuation:?}");

    client.send_line("*").await;
    let response = client.read_until_tagged("A1").await;
    assert!(
        response.contains("A1 NO"),
        "a cancelled exchange must not authenticate: {response:?}"
    );
}

#[tokio::test]
async fn repeated_authentication_is_refused() {
    // RFC 9051 §6.2: AUTHENTICATE is a Not Authenticated command. A second one
    // would let a client change identity mid-session.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;
    server.account_with_password("bob", "hunter2").await;

    let mut client = upgrade(&server).await;

    let first = client
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("alice", "correct horse")
        ))
        .await;
    assert!(first.contains("A1 OK"), "{first:?}");

    let second = client
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("bob", "hunter2")
        ))
        .await;
    assert!(
        second.contains("A2 BAD"),
        "re-authentication must be refused: {second:?}"
    );
}

#[tokio::test]
async fn logout_works_after_authentication() {
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut client = upgrade(&server).await;
    client
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("alice", "correct horse")
        ))
        .await;

    let response = client.command("LOGOUT").await;
    let bye = response.find("* BYE").expect("BYE required");
    let ok = response.find("A2 OK").expect("tagged OK required");
    assert!(bye < ok, "{response:?}");
}

// ------------------------------------------------------- the TLS policy

#[tokio::test]
async fn plaintext_authentication_is_refused_when_tls_is_required() {
    // The central security property of this increment: a credential is never
    // accepted in the clear (ADR-033). Not merely unadvertised — refused.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut client = server.connect().await;
    let response = client
        .command(&format!(
            "AUTHENTICATE PLAIN {}",
            sasl_plain("alice", "correct horse")
        ))
        .await;

    assert!(
        response.contains("A1 NO"),
        "correct credentials must still be refused in the clear: {response:?}"
    );
    assert!(
        !response.contains("A1 OK"),
        "a plaintext credential must never be accepted: {response:?}"
    );
}

#[tokio::test]
async fn the_login_command_is_refused_in_the_clear_too() {
    // LOGIN sends the password as a plain argument, so it falls under the same
    // policy as AUTHENTICATE. Refusing one but not the other would be a bypass.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut client = server.connect().await;
    let response = client.command("LOGIN alice \"correct horse\"").await;

    assert!(
        response.contains("A1 NO"),
        "LOGIN must be refused in the clear: {response:?}"
    );
}

#[tokio::test]
async fn the_login_command_succeeds_over_tls() {
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    server.account_with_password("alice", "correct horse").await;

    let mut client = upgrade(&server).await;
    let response = client.command("LOGIN alice \"correct horse\"").await;
    assert!(response.contains("A1 OK"), "{response:?}");
}

#[tokio::test]
async fn a_plaintext_client_cannot_reach_mail_commands_at_all() {
    // Defence in depth: even if authentication were somehow bypassed, the
    // state machine must still refuse mailbox commands.
    let server = TestServer::with_tls(TestServer::auth_over_start_tls()).await;
    let mut client = server.connect().await;

    for command in ["SELECT INBOX", "FETCH 1 (BODY[])", "LIST \"\" *"] {
        let response = client.command(command).await;
        assert!(
            !response.contains(" OK "),
            "{command} must not succeed unauthenticated: {response:?}"
        );
    }
}
