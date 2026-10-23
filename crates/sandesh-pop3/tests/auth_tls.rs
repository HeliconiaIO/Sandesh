//! POP3 authentication (USER/PASS, SASL PLAIN/LOGIN) and TLS (STLS, implicit TLS) tests.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;
use common::{StreamClient, TestServer, sasl_field, sasl_plain, tls_client};

#[tokio::test]
async fn auth_required_rejects_wrong_password() {
    let config = TestServer::base_config();
    let config = sandesh_config::Pop3Config {
        auth_mode: sandesh_config::AuthMode::Required,
        ..config
    };
    let server = TestServer::with_config(config).await;
    server
        .account_with_password("alice", "correct-password")
        .await;

    let mut client = server.connect().await;
    assert!(client.cmd("USER alice").await.starts_with("+OK"));

    let bad_pass = client.cmd("PASS wrong-password").await;
    assert!(bad_pass.starts_with("-ERR authentication failed"));

    // A subsequent valid password succeeds
    assert!(client.cmd("USER alice").await.starts_with("+OK"));
    let good_pass = client.cmd("PASS correct-password").await;
    assert!(good_pass.starts_with("+OK"));

    client.cmd("QUIT").await;
}

#[tokio::test]
async fn auth_plain_with_initial_response() {
    let server = TestServer::start().await;
    server.create_account("bob").await;

    let mut client = server.connect().await;
    let auth_cmd = format!("AUTH PLAIN {}", sasl_plain("bob", "any-password"));
    let resp = client.cmd(&auth_cmd).await;
    assert!(resp.starts_with("+OK authenticated"));

    let stat = client.cmd("STAT").await;
    assert!(stat.starts_with("+OK 0 0"));

    client.cmd("QUIT").await;
}

#[tokio::test]
async fn auth_plain_with_challenge_response() {
    let server = TestServer::start().await;
    server.create_account("carol").await;

    let mut client = server.connect().await;
    let challenge = client.cmd("AUTH PLAIN").await;
    assert!(challenge.starts_with("+ "));

    let creds = sasl_plain("carol", "secret");
    let resp = client.cmd(&creds).await;
    assert!(resp.starts_with("+OK authenticated"));

    client.cmd("QUIT").await;
}

#[tokio::test]
async fn auth_login_flow() {
    let server = TestServer::start().await;
    server.create_account("dave").await;

    let mut client = server.connect().await;
    let prompt1 = client.cmd("AUTH LOGIN").await;
    assert!(prompt1.starts_with("+ "));

    // Send username
    let prompt2 = client.cmd(&sasl_field("dave")).await;
    assert!(prompt2.starts_with("+ "));

    // Send password
    let resp = client.cmd(&sasl_field("password123")).await;
    assert!(resp.starts_with("+OK authenticated"));

    client.cmd("QUIT").await;
}

#[tokio::test]
async fn auth_login_cancelled() {
    let server = TestServer::start().await;
    server.create_account("eve").await;

    let mut client = server.connect().await;
    let prompt = client.cmd("AUTH LOGIN").await;
    assert!(prompt.starts_with("+ "));

    // Send cancel token '*'
    let resp = client.cmd("*").await;
    assert!(resp.starts_with("-ERR authentication cancelled"));

    // Session remains in Authorization state
    assert!(client.cmd("USER eve").await.starts_with("+OK"));
    assert!(client.cmd("PASS pass").await.starts_with("+OK"));

    client.cmd("QUIT").await;
}

#[tokio::test]
async fn stls_upgrade_and_authenticated_session() {
    let config = TestServer::auth_required_with_stls();
    let server = TestServer::with_tls(config).await;
    server.account_with_password("frank", "tls-password").await;

    let mut client = server.connect().await;

    // In cleartext CAPA, STLS is advertised
    let (_capa_status, capa_lines) = client.cmd_multiline("CAPA").await;
    assert!(capa_lines.iter().any(|l| l == "STLS"));

    // Request STLS
    let stls_resp = client.cmd("STLS").await;
    assert!(stls_resp.starts_with("+OK Begin TLS negotiation"));

    // Upgrade stream to TLS
    let tcp_stream = client.into_stream();
    let tls_stream = tls_client::handshake(tcp_stream).await;
    let mut tls_client = StreamClient::new(tls_stream);

    // RFC 2595: server does NOT re-issue greeting after STLS; client speaks directly
    let (_tls_capa_status, tls_capa_lines) = tls_client.cmd_multiline("CAPA").await;
    assert!(
        !tls_capa_lines.iter().any(|l| l == "STLS"),
        "STLS must not be advertised inside TLS"
    );
    assert!(
        tls_capa_lines.iter().any(|l| l.contains("SASL")),
        "SASL must be advertised once encrypted"
    );

    // Authenticate over TLS
    assert!(tls_client.cmd("USER frank").await.starts_with("+OK"));
    let pass_resp = tls_client.cmd("PASS tls-password").await;
    assert!(pass_resp.starts_with("+OK 0 messages"));

    let stat = tls_client.cmd("STAT").await;
    assert!(stat.starts_with("+OK 0 0"));

    tls_client.cmd("QUIT").await;
}

#[tokio::test]
async fn implicit_tls_listener() {
    let config = TestServer::implicit_tls_only();
    let server = TestServer::with_tls(config).await;
    server.account_with_password("grace", "pop3s-secret").await;

    let tls_stream = tls_client::connect(server.implicit()).await;
    let mut client = StreamClient::new(tls_stream);

    // Implicit TLS greets immediately after handshake
    client.greeting = client.read_line().await;
    assert!(client.greeting.starts_with("+OK Sandesh POP3 server ready"));

    assert!(client.cmd("USER grace").await.starts_with("+OK"));
    assert!(client.cmd("PASS pop3s-secret").await.starts_with("+OK"));

    client.cmd("QUIT").await;
}

// The failure paths below were uncovered when Phase 7's Definition-of-Done
// check measured coverage for the first time: `sandesh-pop3` sat at 84.15%
// against a documented ≥85% gate, and `handle_auth`'s error branches were the
// largest single gap. They are ordinary reachable behaviour — a client can ask
// for a mechanism that does not exist, or fail verification — not defensive
// code, so they are worth asserting rather than excluding.

#[tokio::test]
async fn an_unsupported_mechanism_is_refused_by_name() {
    let server = TestServer::start().await;

    let mut client = server.connect().await;
    // CRAM-MD5 is refused deliberately (ADR-032), so it is the honest example
    // of a mechanism a real client might still offer.
    let reply = client.cmd("AUTH CRAM-MD5").await;

    assert!(
        reply.starts_with("-ERR"),
        "an unknown mechanism must be refused: {reply}"
    );
    assert!(
        reply.to_lowercase().contains("mechanism"),
        "the refusal should say what was wrong: {reply}"
    );

    // The session survives: a refused mechanism is not a fatal error.
    assert!(client.cmd("NOOP").await.starts_with("+OK"));
    client.cmd("QUIT").await;
}

/// `AUTH` is refused outright on a cleartext connection when authentication
/// is enabled — before any credential is examined.
///
/// Worth its own test: writing the credential-failure tests below against a
/// cleartext socket produced this reply instead, which is the server being
/// right and the test being wrong. Pinning it keeps the ordering explicit —
/// transport first, credentials second.
#[tokio::test]
async fn auth_is_refused_in_the_clear_before_credentials_are_checked() {
    let config = TestServer::auth_required_with_stls();
    let server = TestServer::with_tls(config).await;
    server
        .account_with_password("alice", "correct-password")
        .await;

    let mut client = server.connect().await;
    let reply = client
        .cmd(&format!(
            "AUTH PLAIN {}",
            sasl_plain("alice", "correct-password")
        ))
        .await;

    assert!(
        reply.starts_with("-ERR"),
        "AUTH must not be honoured in the clear: {reply}"
    );
    assert!(
        reply.to_lowercase().contains("cleartext"),
        "the refusal should name the transport, not the credential: {reply}"
    );

    client.cmd("QUIT").await;
}

/// Opens an authenticated-capable TLS session, returning the client.
async fn tls_session(
    server: &TestServer,
) -> StreamClient<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let mut client = server.connect().await;
    assert!(client.cmd("STLS").await.starts_with("+OK"));
    let tls_stream = tls_client::handshake(client.into_stream()).await;
    StreamClient::new(tls_stream)
}

#[tokio::test]
async fn auth_plain_with_a_wrong_password_fails_without_ending_the_session() {
    let config = TestServer::auth_required_with_stls();
    let server = TestServer::with_tls(config).await;
    server
        .account_with_password("alice", "correct-password")
        .await;

    let mut client = tls_session(&server).await;

    let reply = client
        .cmd(&format!(
            "AUTH PLAIN {}",
            sasl_plain("alice", "wrong-password")
        ))
        .await;
    assert!(
        reply.starts_with("-ERR authentication failed"),
        "a wrong password must fail: {reply}"
    );

    // The same connection can then authenticate correctly — a failed attempt
    // must not poison the session.
    let good = client
        .cmd(&format!(
            "AUTH PLAIN {}",
            sasl_plain("alice", "correct-password")
        ))
        .await;
    assert!(good.starts_with("+OK"), "a retry should succeed: {good}");

    client.cmd("QUIT").await;
}

#[tokio::test]
async fn auth_plain_for_an_unknown_account_fails_the_same_way() {
    let config = TestServer::auth_required_with_stls();
    let server = TestServer::with_tls(config).await;
    server
        .account_with_password("alice", "correct-password")
        .await;

    let mut client = tls_session(&server).await;
    let reply = client
        .cmd(&format!(
            "AUTH PLAIN {}",
            sasl_plain("nobody", "any-password")
        ))
        .await;

    // Identical wording to a wrong password: a different message here would
    // be an account-enumeration oracle (ADR-032).
    assert!(
        reply.starts_with("-ERR authentication failed"),
        "an unknown account must fail indistinguishably: {reply}"
    );

    client.cmd("QUIT").await;
}

/// Cancelling `AUTH PLAIN` mid-exchange is distinct from failing it, and the
/// server must say so rather than reporting a credential problem.
#[tokio::test]
async fn auth_plain_can_be_cancelled_at_the_challenge() {
    let server = TestServer::start().await;

    let mut client = server.connect().await;
    let challenge = client.cmd("AUTH PLAIN").await;
    assert!(
        challenge.starts_with('+'),
        "expected a challenge: {challenge}"
    );

    let reply = client.cmd("*").await;
    assert!(
        reply.starts_with("-ERR"),
        "cancellation must be refused, not accepted: {reply}"
    );

    assert!(client.cmd("NOOP").await.starts_with("+OK"));
    client.cmd("QUIT").await;
}

/// A base64 payload that is not valid SASL must fail as authentication, not
/// crash the session or be mistaken for a cancellation.
#[tokio::test]
async fn a_malformed_sasl_payload_fails_cleanly() {
    let server = TestServer::start().await;

    let mut client = server.connect().await;
    let reply = client.cmd("AUTH PLAIN !!!not-base64!!!").await;

    assert!(
        reply.starts_with("-ERR"),
        "a malformed payload must be refused: {reply}"
    );

    assert!(
        client.cmd("NOOP").await.starts_with("+OK"),
        "the session must survive a malformed payload"
    );
    client.cmd("QUIT").await;
}

#[tokio::test]
async fn auth_login_rejects_a_wrong_password_after_both_prompts() {
    let config = TestServer::auth_required_with_stls();
    let server = TestServer::with_tls(config).await;
    server
        .account_with_password("alice", "correct-password")
        .await;

    let mut client = tls_session(&server).await;
    let prompt = client.cmd("AUTH LOGIN").await;
    assert!(
        prompt.starts_with('+'),
        "expected a username prompt: {prompt}"
    );

    let password_prompt = client.cmd(&sasl_field("alice")).await;
    assert!(
        password_prompt.starts_with('+'),
        "expected a password prompt: {password_prompt}"
    );

    let reply = client.cmd(&sasl_field("wrong-password")).await;
    assert!(
        reply.starts_with("-ERR authentication failed"),
        "a wrong password must fail: {reply}"
    );

    client.cmd("QUIT").await;
}
