//! `AUTH` and `STARTTLS` over a real socket (Phase 4.5).
//!
//! These drive an actual server with an actual `rustls` handshake. Nothing is
//! stubbed, because the properties under test are exactly the ones a unit test
//! cannot see: what the server *advertises*, what it *refuses*, and what state
//! survives a TLS upgrade.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use common::TestServer;
use sandesh_config::{AuthMode, SmtpConfig, TlsMode};
use tokio::net::TcpStream;

/// A configuration with authentication on and STARTTLS available.
fn auth_and_tls(mode: AuthMode) -> SmtpConfig {
    SmtpConfig {
        auth_mode: mode,
        tls_mode: TlsMode::StartTls,
        ..TestServer::base_config()
    }
}

/// Encodes a `PLAIN` credential the way a client would.
fn plain(username: &str, password: &str) -> String {
    BASE64.encode(format!("\0{username}\0{password}"))
}

// ---------------------------------------------------------------------------
// Advertisement: what the server admits to supporting
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn auth_is_not_advertised_by_default() {
    // ADR-005: authentication is off unless an operator turns it on, and a
    // capability that is off must not be advertised.
    let server = TestServer::start().await;
    let mut client = server.connect().await;

    let ehlo = client.send("EHLO test.local").await;

    assert!(
        !ehlo.contains("AUTH"),
        "AUTH must not be advertised: {ehlo}"
    );
    assert!(
        !ehlo.contains("STARTTLS"),
        "STARTTLS must not be advertised: {ehlo}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_is_not_advertised_on_a_cleartext_connection() {
    // ADR-033: PLAIN and LOGIN send the password, so they are withheld until
    // the channel is encrypted. A client that sees no AUTH will not try, which
    // is the whole point — the credential never reaches the wire.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    let mut client = server.connect().await;

    let ehlo = client.send("EHLO test.local").await;

    assert!(
        !ehlo.contains("AUTH"),
        "AUTH must not be offered in the clear: {ehlo}"
    );
    // STARTTLS, by contrast, is exactly what should be offered here.
    assert!(
        ehlo.contains("STARTTLS"),
        "STARTTLS must be advertised: {ehlo}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn starttls_is_not_advertised_when_tls_is_disabled() {
    // Advertising an upgrade that cannot happen is the worst failure mode an
    // SMTP server has.
    let (server, _dir) = TestServer::with_tls(SmtpConfig {
        tls_mode: TlsMode::Disabled,
        ..TestServer::base_config()
    })
    .await;
    let mut client = server.connect().await;

    let ehlo = client.send("EHLO test.local").await;
    assert!(!ehlo.contains("STARTTLS"), "{ehlo}");
}

// ---------------------------------------------------------------------------
// Refusal: credentials must never cross an unencrypted channel
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn auth_on_a_cleartext_connection_is_refused_with_538() {
    // The credential-exposure guard. Even a client that ignores the missing
    // advertisement and tries anyway must be refused *before* it is invited to
    // send anything.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    let mut client = server.connect().await;
    client.send("EHLO test.local").await;

    let reply = client.send("AUTH PLAIN").await;

    assert!(reply.starts_with("538"), "expected 538, got {reply}");
    // Critically, not a 334 challenge: the server must not ask for a password
    // it has already decided it will not accept in the clear.
    assert!(
        !reply.starts_with("334"),
        "the server invited a cleartext credential"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_inline_cleartext_credential_is_refused_without_being_checked() {
    // A client can put the credential in the AUTH command itself. That must
    // still be refused on encryption grounds rather than verified.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    server
        .account_with_password("dev", "dev@example.test", "hunter2")
        .await;

    let mut client = server.connect().await;
    client.send("EHLO test.local").await;

    let reply = client
        .send(&format!("AUTH PLAIN {}", plain("dev", "hunter2")))
        .await;

    // Correct credentials, but the wrong channel.
    assert!(reply.starts_with("538"), "expected 538, got {reply}");
    assert!(
        !reply.starts_with("235"),
        "a cleartext credential was accepted"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_is_not_implemented_when_disabled() {
    // Distinct from 538: the operator has not enabled authentication at all,
    // so the honest answer is 502, not "encrypt and try again".
    let (server, _dir) = TestServer::with_tls(SmtpConfig {
        auth_mode: AuthMode::Disabled,
        tls_mode: TlsMode::StartTls,
        ..TestServer::base_config()
    })
    .await;

    let mut client = server.connect().await;
    client.send("EHLO test.local").await;

    let reply = client.send("AUTH PLAIN").await;
    assert!(reply.starts_with("502"), "expected 502, got {reply}");
}

// ---------------------------------------------------------------------------
// Sequencing
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn auth_before_a_greeting_is_refused() {
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    let mut client = server.connect().await;

    let reply = client.send("AUTH PLAIN").await;
    assert!(reply.starts_with("503"), "expected 503, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn starttls_before_a_greeting_is_refused() {
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    let mut client = server.connect().await;

    let reply = client.send("STARTTLS").await;
    assert!(reply.starts_with("503"), "expected 503, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn starttls_during_a_transaction_is_refused() {
    // RFC 3207 §4: the upgrade discards session state, so permitting it
    // mid-transaction would silently drop an in-flight message.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    server.catch_all().await;

    let mut client = server.connect().await;
    client.send("EHLO test.local").await;
    client.send("MAIL FROM:<sender@example.test>").await;

    let reply = client.send("STARTTLS").await;
    assert!(reply.starts_with("503"), "expected 503, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn starttls_with_a_parameter_is_a_syntax_error() {
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    let mut client = server.connect().await;
    client.send("EHLO test.local").await;

    let reply = client.send("STARTTLS now").await;
    assert!(
        reply.starts_with("500") || reply.starts_with("501"),
        "expected a syntax error, got {reply}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_from_is_refused_without_authentication_when_required() {
    // AuthMode::Required must actually require it.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Required)).await;
    server.catch_all().await;

    let mut client = server.connect().await;
    client.send("EHLO test.local").await;

    let reply = client.send("MAIL FROM:<sender@example.test>").await;
    assert!(reply.starts_with("530"), "expected 530, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_from_is_accepted_without_authentication_when_disabled() {
    // The default must stay frictionless: a developer sends a test message
    // without configuring anything (ADR-005).
    let server = TestServer::start().await;
    server.catch_all().await;

    let mut client = server.connect().await;
    client.send("EHLO test.local").await;

    let reply = client.send("MAIL FROM:<sender@example.test>").await;
    assert!(reply.starts_with("250"), "expected 250, got {reply}");
}

// ---------------------------------------------------------------------------
// The upgrade itself
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn starttls_invites_a_handshake() {
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    let mut client = server.connect().await;
    client.send("EHLO test.local").await;

    let reply = client.send("STARTTLS").await;
    assert!(reply.starts_with("220"), "expected 220, got {reply}");
}

// ---------------------------------------------------------------------------
// The full flow: a real TLS handshake, then a real authentication
// ---------------------------------------------------------------------------

/// A TLS client that trusts whatever certificate the server presents.
///
/// Trusting blindly is correct *here* and nowhere else: the server generates a
/// fresh self-signed certificate per test, so there is no CA to validate
/// against, and what is under test is the SMTP behaviour after the handshake
/// rather than certificate validation itself.
mod tls_client {
    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};

    #[derive(Debug)]
    pub struct TrustAny;

    impl ServerCertVerifier for TrustAny {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    /// A client configuration that accepts the test server's certificate.
    pub fn config() -> Arc<ClientConfig> {
        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TrustAny))
            .with_no_client_auth();
        Arc::new(config)
    }
}

/// Drives a full `STARTTLS` upgrade and returns the encrypted stream.
async fn upgrade(addr: std::net::SocketAddr) -> tokio_rustls::client::TlsStream<TcpStream> {
    let mut stream = TcpStream::connect(addr).await.expect("connect");

    // Greeting, EHLO, then STARTTLS — all in cleartext.
    read_reply(&mut stream).await;
    write_line(&mut stream, "EHLO test.local").await;
    read_reply(&mut stream).await;
    write_line(&mut stream, "STARTTLS").await;
    let ready = read_reply(&mut stream).await;
    assert!(ready.starts_with("220"), "expected 220, got {ready}");

    let connector = tokio_rustls::TlsConnector::from(tls_client::config());
    let name = rustls::pki_types::ServerName::try_from("localhost").expect("valid name");

    connector
        .connect(name, stream)
        .await
        .expect("the TLS handshake must succeed")
}

async fn write_line<S>(stream: &mut S, line: &str)
where
    S: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    stream
        .write_all(format!("{line}\r\n").as_bytes())
        .await
        .expect("write");
    stream.flush().await.expect("flush");
}

/// Reads one SMTP reply, following multi-line continuations.
async fn read_reply<S>(stream: &mut S) -> String
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut reply = String::new();
    let mut byte = [0_u8; 1];

    loop {
        let mut line = String::new();
        loop {
            let read = stream.read(&mut byte).await.expect("read");
            assert!(read != 0, "the connection closed mid-reply: {reply}");
            if byte[0] == b'\n' {
                break;
            }
            if byte[0] != b'\r' {
                line.push(char::from(byte[0]));
            }
        }

        let more = line.as_bytes().get(3) == Some(&b'-');
        reply.push_str(&line);
        reply.push('\n');
        if !more {
            return reply;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_tls_handshake_succeeds_and_auth_is_then_offered() {
    // The acceptance criterion for STARTTLS: a real client completes a real
    // handshake, and the server then offers AUTH that it withheld in cleartext.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    let mut tls = upgrade(server.addr).await;

    // RFC 3207 §4.2: the client greets again inside TLS.
    write_line(&mut tls, "EHLO test.local").await;
    let ehlo = read_reply(&mut tls).await;

    assert!(
        ehlo.contains("AUTH"),
        "AUTH must be offered over TLS: {ehlo}"
    );
    assert!(ehlo.contains("PLAIN"), "{ehlo}");
    assert!(ehlo.contains("LOGIN"), "{ehlo}");
    // And STARTTLS must not be re-advertised inside TLS.
    assert!(
        !ehlo.contains("STARTTLS"),
        "STARTTLS must not be re-advertised: {ehlo}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_authentication_succeeds_over_tls() {
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Required)).await;
    server
        .account_with_password("dev", "dev@example.test", "hunter2")
        .await;

    let mut tls = upgrade(server.addr).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(&mut tls, &format!("AUTH PLAIN {}", plain("dev", "hunter2"))).await;
    let reply = read_reply(&mut tls).await;

    assert!(reply.starts_with("235"), "expected 235, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_password_is_rejected_over_tls() {
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Required)).await;
    server
        .account_with_password("dev", "dev@example.test", "hunter2")
        .await;

    let mut tls = upgrade(server.addr).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(&mut tls, &format!("AUTH PLAIN {}", plain("dev", "wrong"))).await;
    let reply = read_reply(&mut tls).await;

    assert!(reply.starts_with("535"), "expected 535, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_user_is_rejected_indistinguishably_from_a_wrong_password() {
    // The username oracle. The two failures must be byte-identical on the
    // wire, or the server tells an attacker which accounts exist.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Required)).await;
    server
        .account_with_password("dev", "dev@example.test", "hunter2")
        .await;

    let mut tls = upgrade(server.addr).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(&mut tls, &format!("AUTH PLAIN {}", plain("dev", "wrong"))).await;
    let wrong_password = read_reply(&mut tls).await;

    let mut tls = upgrade(server.addr).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(
        &mut tls,
        &format!("AUTH PLAIN {}", plain("nobody", "wrong")),
    )
    .await;
    let unknown_user = read_reply(&mut tls).await;

    assert_eq!(
        wrong_password, unknown_user,
        "the replies must be identical, or they leak which usernames exist"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn login_authentication_succeeds_over_tls() {
    // The two-prompt exchange, which is what older clients use.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Required)).await;
    server
        .account_with_password("dev", "dev@example.test", "hunter2")
        .await;

    let mut tls = upgrade(server.addr).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(&mut tls, "AUTH LOGIN").await;
    let username_prompt = read_reply(&mut tls).await;
    assert!(username_prompt.starts_with("334"), "got {username_prompt}");

    write_line(&mut tls, &BASE64.encode("dev")).await;
    let password_prompt = read_reply(&mut tls).await;
    assert!(password_prompt.starts_with("334"), "got {password_prompt}");

    write_line(&mut tls, &BASE64.encode("hunter2")).await;
    let reply = read_reply(&mut tls).await;

    assert!(reply.starts_with("235"), "expected 235, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn cram_md5_is_refused_by_name_over_tls() {
    // ADR-032: refused explicitly, never silently downgraded, so a developer
    // can see why their client failed.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    let mut tls = upgrade(server.addr).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(&mut tls, "AUTH CRAM-MD5").await;
    let reply = read_reply(&mut tls).await;

    assert!(reply.starts_with("504"), "expected 504, got {reply}");
    assert!(
        reply.contains("CRAM-MD5"),
        "the reply must name it: {reply}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_exchange_is_not_reported_as_a_bad_credential() {
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    let mut tls = upgrade(server.addr).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(&mut tls, "AUTH LOGIN").await;
    read_reply(&mut tls).await;
    write_line(&mut tls, "*").await;
    let reply = read_reply(&mut tls).await;

    assert!(reply.starts_with("501"), "expected 501, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_is_delivered_after_authenticating_over_tls() {
    // The end-to-end property: with authentication required, a client that
    // authenticates can actually deliver mail through the encrypted channel.
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Required)).await;
    server
        .account_with_password("dev", "dev@example.test", "hunter2")
        .await;

    let mut tls = upgrade(server.addr).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(&mut tls, &format!("AUTH PLAIN {}", plain("dev", "hunter2"))).await;
    let authenticated = read_reply(&mut tls).await;
    assert!(authenticated.starts_with("235"), "got {authenticated}");

    for (command, expected) in [
        ("MAIL FROM:<sender@example.test>", "250"),
        ("RCPT TO:<dev@example.test>", "250"),
        ("DATA", "354"),
    ] {
        write_line(&mut tls, command).await;
        let reply = read_reply(&mut tls).await;
        assert!(reply.starts_with(expected), "{command}: {reply}");
    }

    write_line(
        &mut tls,
        "Subject: Encrypted and authenticated\r\n\r\nBody.\r\n.",
    )
    .await;
    let accepted = read_reply(&mut tls).await;
    assert!(accepted.starts_with("250"), "expected 250, got {accepted}");

    assert_eq!(
        server.message_count().await,
        1,
        "the message must be stored"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plaintext_pipelined_after_starttls_is_discarded_not_executed() {
    // RFC 3207 §4, and the most serious failure mode of a STARTTLS upgrade.
    //
    // An active attacker can append commands to the cleartext STARTTLS line.
    // If the server keeps its read buffer across the handshake, those commands
    // execute *as though they had arrived inside the TLS session* — the client
    // never sent them, and TLS gives no protection because they were already
    // buffered before it started.
    //
    // Here the injected `MAIL FROM` names a sender the real client never uses.
    // After the upgrade the client greets and opens its own transaction; if the
    // injection had been executed, that `MAIL FROM` would be refused as a
    // nested transaction (503) instead of accepted (250).
    let (server, _dir) = TestServer::with_tls(auth_and_tls(AuthMode::Optional)).await;
    server.catch_all().await;

    let mut stream = TcpStream::connect(server.addr).await.expect("connect");
    read_reply(&mut stream).await;
    write_line(&mut stream, "EHLO test.local").await;
    read_reply(&mut stream).await;

    // One write: STARTTLS plus an injected command the client never intended.
    {
        use tokio::io::AsyncWriteExt;
        stream
            .write_all(b"STARTTLS\r\nMAIL FROM:<injected@attacker.test>\r\n")
            .await
            .expect("write");
        stream.flush().await.expect("flush");
    }

    let ready = read_reply(&mut stream).await;
    assert!(ready.starts_with("220"), "expected 220, got {ready}");

    let connector = tokio_rustls::TlsConnector::from(tls_client::config());
    let name = rustls::pki_types::ServerName::try_from("localhost").expect("valid name");
    let mut tls = connector
        .connect(name, stream)
        .await
        .expect("handshake succeeds");

    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    // The client's own transaction must open cleanly. A 503 here would mean
    // the injected MAIL FROM had been executed.
    write_line(&mut tls, "MAIL FROM:<real@example.test>").await;
    let reply = read_reply(&mut tls).await;

    assert!(
        reply.starts_with("250"),
        "the injected cleartext command was executed inside the TLS session: {reply}"
    );
}

// ---------------------------------------------------------------------------
// Cleartext Developer Authentication (Odoo, Python smtplib, local dev clients)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cleartext_auth_is_advertised_and_accepted_when_configured() {
    let server = TestServer::with_config(SmtpConfig {
        allow_cleartext_auth: true,
        ..TestServer::base_config()
    })
    .await;
    server
        .account_with_password("dev", "dev@example.test", "s3cret")
        .await;

    let mut client = server.connect().await;
    let ehlo = client.send("EHLO test.local").await;
    assert!(
        ehlo.contains("AUTH PLAIN LOGIN"),
        "AUTH PLAIN LOGIN must be advertised in cleartext: {ehlo}"
    );

    // Successful PLAIN authentication
    let reply = client
        .send(&format!("AUTH PLAIN {}", plain("dev", "s3cret")))
        .await;
    assert!(reply.starts_with("235"), "expected 235, got {reply}");

    // Invalid password returns 535
    let mut client2 = server.connect().await;
    client2.send("EHLO test.local").await;
    let reply2 = client2
        .send(&format!("AUTH PLAIN {}", plain("dev", "wrongpass")))
        .await;
    assert!(reply2.starts_with("535"), "expected 535, got {reply2}");
}

#[tokio::test(flavor = "multi_thread")]
async fn cleartext_login_mechanism_succeeds_over_unencrypted_channel() {
    let server = TestServer::with_config(SmtpConfig {
        allow_cleartext_auth: true,
        ..TestServer::base_config()
    })
    .await;
    server
        .account_with_password("dev", "dev@example.test", "s3cret")
        .await;

    let mut client = server.connect().await;
    client.send("EHLO test.local").await;

    let reply = client.send("AUTH LOGIN").await;
    assert!(
        reply.starts_with("334"),
        "expected 334 username prompt, got {reply}"
    );

    let reply = client.send(&BASE64.encode("dev")).await;
    assert!(
        reply.starts_with("334"),
        "expected 334 password prompt, got {reply}"
    );

    let reply = client.send(&BASE64.encode("s3cret")).await;
    assert!(reply.starts_with("235"), "expected 235, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn cleartext_permissive_auth_succeeds_for_account_without_password() {
    let server = TestServer::with_config(SmtpConfig {
        allow_cleartext_auth: true,
        ..TestServer::base_config()
    })
    .await;

    let mut client = server.connect().await;
    client.send("EHLO test.local").await;

    let reply = client
        .send(&format!("AUTH PLAIN {}", plain("anyuser", "anypass")))
        .await;
    assert!(
        reply.starts_with("235"),
        "expected 235 in permissive dev mode, got {reply}"
    );
}
