//! Implicit TLS (ADR-037).
//!
//! The property under test is that **no SMTP is spoken before the handshake**.
//! Before this mode was implemented, selecting it produced a listener that
//! served plaintext SMTP while the operator believed the port was encrypted —
//! a silent TLS downgrade. These tests exist so that cannot recur.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use common::TestServer;
use sandesh_config::{AuthMode, SmtpConfig, TlsMode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A configuration with implicit TLS and the given authentication mode.
fn implicit(auth: AuthMode) -> SmtpConfig {
    SmtpConfig {
        auth_mode: auth,
        tls_mode: TlsMode::Implicit,
        ..TestServer::base_config()
    }
}

/// A TLS client that accepts the test server's self-signed certificate.
///
/// Trusting blindly is correct here and nowhere else: each test generates a
/// fresh certificate, so there is no CA to validate against, and what is under
/// test is the SMTP behaviour rather than certificate validation.
mod trust {
    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};

    #[derive(Debug)]
    pub struct Any;

    impl ServerCertVerifier for Any {
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

    pub fn config() -> Arc<ClientConfig> {
        Arc::new(
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Any))
                .with_no_client_auth(),
        )
    }
}

/// Connects and completes the implicit-TLS handshake.
async fn connect(addr: std::net::SocketAddr) -> tokio_rustls::client::TlsStream<TcpStream> {
    let stream = TcpStream::connect(addr).await.expect("connect");
    let connector = tokio_rustls::TlsConnector::from(trust::config());
    let name = rustls::pki_types::ServerName::try_from("localhost").expect("valid name");

    connector
        .connect(name, stream)
        .await
        .expect("the implicit TLS handshake must succeed")
}

async fn write_line<S>(stream: &mut S, line: &str)
where
    S: tokio::io::AsyncWrite + Unpin,
{
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

// ---------------------------------------------------------------------------
// The mode is genuinely encrypted
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_real_handshake_precedes_any_smtp() {
    // The core property. A TLS client connects and receives the greeting
    // *inside* TLS — nothing was spoken in the clear.
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Disabled)).await;
    let mut tls = connect(server.addr).await;

    let greeting = read_reply(&mut tls).await;
    assert!(greeting.starts_with("220"), "expected 220, got {greeting}");
    assert!(greeting.contains("ESMTP"), "{greeting}");
}

#[tokio::test(flavor = "multi_thread")]
async fn plaintext_smtp_is_never_interpreted_on_an_implicit_listener() {
    // The regression guard for the original defect. A plaintext client sends
    // `EHLO` as its first bytes; the server must treat that as a failed
    // handshake and close, never as SMTP.
    //
    // Before the fix this test received `220 ... ESMTP ready` in cleartext.
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Required)).await;

    let mut stream = TcpStream::connect(server.addr).await.expect("connect");

    // A well-behaved SMTP client waits for the greeting first, so assert that
    // no cleartext greeting is ever sent.
    let mut buffer = [0_u8; 512];
    let first = tokio::time::timeout(
        std::time::Duration::from_millis(750),
        stream.read(&mut buffer),
    )
    .await;

    match first {
        // Closed or errored without sending anything: correct.
        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => {}
        Ok(Ok(read)) => {
            let text = String::from_utf8_lossy(&buffer[..read]);
            // A TLS alert is bytes, not SMTP. What must never appear is a
            // readable SMTP greeting.
            assert!(
                !text.starts_with("220"),
                "an implicit-TLS listener sent a cleartext SMTP greeting: {text:?}"
            );
        }
    }

    // And a plaintext command must not be answered as SMTP either.
    let _ = stream.write_all(b"EHLO plaintext.local\r\n").await;
    let mut after = [0_u8; 512];
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(750),
        stream.read(&mut after),
    )
    .await;

    if let Ok(Ok(read)) = response
        && read > 0
    {
        let text = String::from_utf8_lossy(&after[..read]);
        assert!(
            !text.contains("250"),
            "plaintext EHLO was answered as SMTP: {text:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Capabilities are truthful inside implicit TLS
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn starttls_is_not_advertised_inside_implicit_tls() {
    // RFC 3207 §4.2: already encrypted, so there is nothing to upgrade.
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Disabled)).await;
    let mut tls = connect(server.addr).await;
    read_reply(&mut tls).await;

    write_line(&mut tls, "EHLO test.local").await;
    let ehlo = read_reply(&mut tls).await;

    assert!(
        !ehlo.contains("STARTTLS"),
        "STARTTLS must not be advertised inside TLS: {ehlo}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_is_offered_immediately_because_the_channel_is_already_encrypted() {
    // ADR-033 is satisfied from the first EHLO: no upgrade is needed, so the
    // credential can be accepted straight away.
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Required)).await;
    let mut tls = connect(server.addr).await;
    read_reply(&mut tls).await;

    write_line(&mut tls, "EHLO test.local").await;
    let ehlo = read_reply(&mut tls).await;

    assert!(ehlo.contains("AUTH"), "AUTH must be offered: {ehlo}");
    assert!(ehlo.contains("PLAIN"), "{ehlo}");
    assert!(ehlo.contains("LOGIN"), "{ehlo}");
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_is_still_withheld_when_disabled() {
    // Encryption does not imply authentication. ADR-005 still governs.
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Disabled)).await;
    let mut tls = connect(server.addr).await;
    read_reply(&mut tls).await;

    write_line(&mut tls, "EHLO test.local").await;
    let ehlo = read_reply(&mut tls).await;

    assert!(!ehlo.contains("AUTH"), "{ehlo}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_starttls_inside_implicit_tls_is_refused() {
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Disabled)).await;
    let mut tls = connect(server.addr).await;
    read_reply(&mut tls).await;

    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(&mut tls, "STARTTLS").await;
    let reply = read_reply(&mut tls).await;

    // 503 (already active) or 502 (not offered) — either refuses; what must
    // not happen is a 220 inviting a nested handshake.
    assert!(
        reply.starts_with("503") || reply.starts_with("502"),
        "expected a refusal, got {reply}"
    );
}

// ---------------------------------------------------------------------------
// Authentication and delivery over implicit TLS
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn plain_authentication_succeeds_over_implicit_tls() {
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Required)).await;
    server
        .account_with_password("dev", "dev@example.test", "hunter2")
        .await;

    let mut tls = connect(server.addr).await;
    read_reply(&mut tls).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    let credential = BASE64.encode("\0dev\0hunter2");
    write_line(&mut tls, &format!("AUTH PLAIN {credential}")).await;
    let reply = read_reply(&mut tls).await;

    assert!(reply.starts_with("235"), "expected 235, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_password_is_rejected_over_implicit_tls() {
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Required)).await;
    server
        .account_with_password("dev", "dev@example.test", "hunter2")
        .await;

    let mut tls = connect(server.addr).await;
    read_reply(&mut tls).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    let credential = BASE64.encode("\0dev\0wrong");
    write_line(&mut tls, &format!("AUTH PLAIN {credential}")).await;
    let reply = read_reply(&mut tls).await;

    assert!(reply.starts_with("535"), "expected 535, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_from_is_refused_without_authentication_when_required() {
    // Encryption is not authentication: `Required` must still be enforced.
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Required)).await;
    server.catch_all().await;

    let mut tls = connect(server.addr).await;
    read_reply(&mut tls).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    write_line(&mut tls, "MAIL FROM:<sender@example.test>").await;
    let reply = read_reply(&mut tls).await;

    assert!(reply.starts_with("530"), "expected 530, got {reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_is_delivered_after_authenticating_over_implicit_tls() {
    // The end-to-end property: the whole pipe works inside implicit TLS.
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Required)).await;
    server
        .account_with_password("dev", "dev@example.test", "hunter2")
        .await;

    let mut tls = connect(server.addr).await;
    read_reply(&mut tls).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    let credential = BASE64.encode("\0dev\0hunter2");
    write_line(&mut tls, &format!("AUTH PLAIN {credential}")).await;
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

    write_line(&mut tls, "Subject: Implicit TLS\r\n\r\nBody.\r\n.").await;
    let accepted = read_reply(&mut tls).await;
    assert!(accepted.starts_with("250"), "expected 250, got {accepted}");

    assert_eq!(
        server.message_count().await,
        1,
        "the message must be stored"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_is_delivered_without_authentication_when_auth_is_disabled() {
    // Implicit TLS must not require authentication by implication.
    let (server, _dir) = TestServer::with_tls(implicit(AuthMode::Disabled)).await;
    server.catch_all().await;

    let mut tls = connect(server.addr).await;
    read_reply(&mut tls).await;
    write_line(&mut tls, "EHLO test.local").await;
    read_reply(&mut tls).await;

    for (command, expected) in [
        ("MAIL FROM:<sender@example.test>", "250"),
        ("RCPT TO:<anyone@example.test>", "250"),
        ("DATA", "354"),
    ] {
        write_line(&mut tls, command).await;
        let reply = read_reply(&mut tls).await;
        assert!(reply.starts_with(expected), "{command}: {reply}");
    }

    write_line(&mut tls, "Subject: Anonymous\r\n\r\nBody.\r\n.").await;
    let accepted = read_reply(&mut tls).await;
    assert!(accepted.starts_with("250"), "got {accepted}");
}

// ---------------------------------------------------------------------------
// Invalid configuration must not start
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn implicit_tls_without_a_certificate_fails_startup() {
    // The alternative would be a listener that silently serves plaintext on a
    // port the operator believes is encrypted. Failing to bind is the only
    // safe answer (ADR-037).
    use sandesh_smtp::{ServerError, SmtpServer};

    let (bound, _warnings) = build_server(implicit(AuthMode::Required)).await;

    let error = bound.expect_err("binding must fail with no certificate");
    assert!(
        matches!(error, ServerError::MissingTlsAcceptor { mode: "implicit" }),
        "expected MissingTlsAcceptor, got {error}"
    );
    // The message must name both the setting and the fix.
    let text = error.to_string();
    assert!(text.contains("implicit"), "{text}");
    assert!(text.contains("tls_mode"), "{text}");
    assert!(text.contains("disabled"), "the fix must be named: {text}");

    // Keep the type in scope so the helper's signature stays honest.
    let _ = std::any::type_name::<SmtpServer>();
}

#[tokio::test(flavor = "multi_thread")]
async fn starttls_without_a_certificate_also_fails_startup() {
    // Same reasoning: STARTTLS would be advertised and then fail every
    // handshake, which is worse than refusing to start.
    use sandesh_smtp::ServerError;

    let bound = build_server(SmtpConfig {
        tls_mode: TlsMode::StartTls,
        ..TestServer::base_config()
    })
    .await
    .0;

    let error = bound.expect_err("binding must fail with no certificate");
    assert!(
        matches!(error, ServerError::MissingTlsAcceptor { mode: "start_tls" }),
        "got {error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plaintext_mode_still_binds_without_a_certificate() {
    // The default path must be untouched: no certificate is needed, and none
    // is demanded.
    let bound = build_server(TestServer::base_config()).await.0;
    assert!(bound.is_ok(), "plaintext mode must still bind");
}

/// Builds and binds a server with no TLS acceptor, returning the result.
///
/// Deliberately does not supply an acceptor: these tests are about what
/// happens when a TLS mode is selected without a certificate.
async fn build_server(
    config: SmtpConfig,
) -> (
    Result<sandesh_smtp::BoundServer, sandesh_smtp::ServerError>,
    Vec<String>,
) {
    use std::sync::Arc;

    use sandesh_smtp::{DeliveryService, SmtpServer};
    use sandesh_storage::{
        CollectingSink, Database, EventRecorder, NullBlobStore, SqliteAccountStore,
        SqliteDeliveryStore, SqliteMailboxStore, SqliteMessageStore,
    };

    let db = Database::open_in_memory().await.expect("open database");
    let sink = Arc::new(CollectingSink::new());
    let events = EventRecorder::new(sink);

    let accounts = Arc::new(SqliteAccountStore::new(db.pool().clone(), events.clone()));
    let mailboxes = Arc::new(SqliteMailboxStore::new(db.pool().clone(), events.clone()));
    let messages = Arc::new(SqliteMessageStore::new(
        db.pool().clone(),
        Arc::new(NullBlobStore),
        events.clone(),
    ));
    let deliveries = Arc::new(SqliteDeliveryStore::new(db.pool().clone(), events));

    let policy = common::routing_policy(config.unknown_recipient);
    let delivery = Arc::new(DeliveryService::new(
        accounts, mailboxes, messages, deliveries, policy,
    ));

    match SmtpServer::new(config, delivery).bind().await {
        Ok((bound, warnings)) => (Ok(bound), warnings),
        Err(error) => (Err(error), Vec::new()),
    }
}
