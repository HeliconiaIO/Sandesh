//! API authentication and HTTPS (Phase 4.5's outstanding deliverable, ADR-075).
//!
//! Driven over real sockets against a real [`Application`], because the thing
//! under test is a middleware layer and a TLS accept loop: a handler-level test
//! would exercise neither, and both are exactly where an auth bug hides.
//!
//! The assertions are deliberately about what an *unauthenticated* caller gets.
//! A test that only proves the right credential works would pass just as well
//! against a server that accepts everything.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sandesh_app::Application;
use sandesh_config::{ApiAuthMode, ApiToken, AppConfig};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "s3cr3t-token-value";

struct Harness {
    http: SocketAddr,
    shutdown: CancellationToken,
    _dir: TempDir,
}

impl Harness {
    /// Starts an application, letting the caller adjust the configuration.
    async fn start(adjust: impl FnOnce(&mut AppConfig)) -> Self {
        let dir = TempDir::new().expect("a temporary directory");
        let ephemeral: SocketAddr = "127.0.0.1:0".parse().expect("a valid address");
        let defaults = AppConfig::default();
        let mut config = AppConfig {
            database_path: dir.path().join("sandesh.db"),
            blob_path: dir.path().join("blobs"),
            http: sandesh_config::HttpConfig {
                bind: ephemeral,
                ..defaults.http
            },
            smtp: sandesh_config::SmtpConfig {
                bind: ephemeral,
                ..defaults.smtp
            },
            imap: sandesh_config::ImapConfig {
                bind: Some(ephemeral),
                ..defaults.imap
            },
            pop3: sandesh_config::Pop3Config {
                bind: Some(ephemeral),
                ..defaults.pop3
            },
            ..defaults
        };
        adjust(&mut config);

        let app = Application::start(config).await.expect("the app starts");
        let http = app.addresses().http.expect("HTTP is bound");
        let shutdown = app.shutdown_token();
        tokio::spawn(async move {
            let _ = app.run().await;
        });

        Self {
            http,
            shutdown,
            _dir: dir,
        }
    }

    /// An application requiring a bearer token.
    async fn with_token() -> Self {
        Self::start(|config| {
            config.http.auth_mode = ApiAuthMode::Required;
            config.http.auth_token = Some(ApiToken::new(TOKEN));
        })
        .await
    }

    /// Issues a GET with optional `Authorization`, returning the raw response.
    async fn get(&self, path: &str, authorization: Option<&str>) -> String {
        let mut stream = TcpStream::connect(self.http).await.expect("connects");
        let auth_line = authorization
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth_line}Connection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("writes the request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("reads the response");
        String::from_utf8_lossy(&response).into_owned()
    }

    /// Issues a POST with a JSON body.
    async fn post_json(&self, path: &str, body: &str, authorization: Option<&str>) -> String {
        let mut stream = TcpStream::connect(self.http).await.expect("connects");
        let auth_line = authorization
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\n{auth_line}\
             Content-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("writes the request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("reads the response");
        String::from_utf8_lossy(&response).into_owned()
    }

    fn stop(&self) {
        self.shutdown.cancel();
    }
}

fn status_of(response: &str) -> &str {
    response.lines().next().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Disabled by default (ADR-005)

#[tokio::test]
async fn authentication_is_disabled_by_default() {
    // The default posture must stay open: Sandesh is a local development mail
    // server, and demanding a credential before `curl` can read a test message
    // is friction with no security benefit on loopback.
    let harness = Harness::start(|_| {}).await;

    let response = harness.get("/api/v1/messages", None).await;
    assert!(
        status_of(&response).contains("200"),
        "the default must not require a credential:\n{response}"
    );

    harness.stop();
}

// ---------------------------------------------------------------------------
// Bearer

#[tokio::test]
async fn a_protected_api_refuses_an_anonymous_request() {
    let harness = Harness::with_token().await;

    let response = harness.get("/api/v1/messages", None).await;
    assert!(
        status_of(&response).contains("401"),
        "expected 401 without a credential:\n{response}"
    );

    // RFC 9110 §11.6.1 requires this header on a 401, and it is what tells a
    // client what kind of credential to offer.
    assert!(
        response.to_lowercase().contains("www-authenticate:"),
        "a 401 must carry WWW-Authenticate:\n{response}"
    );

    // The response must not disclose mail. A 401 that still returned the body
    // would be the whole bug.
    assert!(
        !response.contains("\"items\""),
        "the body leaked past the 401:\n{response}"
    );

    harness.stop();
}

#[tokio::test]
async fn the_configured_bearer_token_is_accepted() {
    let harness = Harness::with_token().await;

    let response = harness
        .get("/api/v1/messages", Some(&format!("Bearer {TOKEN}")))
        .await;
    assert!(
        status_of(&response).contains("200"),
        "the configured token must be accepted:\n{response}"
    );

    harness.stop();
}

#[tokio::test]
async fn a_wrong_or_malformed_credential_is_refused() {
    let harness = Harness::with_token().await;

    for attempt in [
        "Bearer wrong-token",
        // A prefix of the real token must not pass: this is what a
        // short-circuiting comparison would eventually allow.
        "Bearer s3cr3t",
        "Bearer ",
        "Basic bm9uc2Vuc2U=",
        "Digest whatever",
        "not-even-a-scheme",
        // The token as a bare value, without the scheme.
        TOKEN,
    ] {
        let response = harness.get("/api/v1/messages", Some(attempt)).await;
        assert!(
            status_of(&response).contains("401"),
            "expected 401 for {attempt:?}:\n{response}"
        );
    }

    harness.stop();
}

// ---------------------------------------------------------------------------
// Exemptions

#[tokio::test]
async fn only_liveness_is_reachable_without_a_credential() {
    let harness = Harness::with_token().await;

    // A container orchestrator has nowhere to put a credential, and liveness
    // discloses only that Sandesh is running — which the open port already does.
    let health = harness.get("/api/v1/health", None).await;
    assert!(
        status_of(&health).contains("200"),
        "the healthcheck must stay reachable:\n{health}"
    );

    // Readiness reports storage state and `/info` reports bind addresses, the
    // database location and the auth posture. Neither is a healthcheck's
    // business, and neither is exempt.
    for path in ["/api/v1/health/ready", "/api/v1/info"] {
        let response = harness.get(path, None).await;
        assert!(
            status_of(&response).contains("401"),
            "{path} must require a credential:\n{response}"
        );
    }

    harness.stop();
}

#[tokio::test]
async fn the_web_ui_is_protected_too() {
    let harness = Harness::with_token().await;

    // The UI is a browser client like any other. Serving the shell
    // unauthenticated would not leak mail by itself, but it would be the one
    // path where "protected by default" quietly did not hold.
    let response = harness.get("/", None).await;
    assert!(
        status_of(&response).contains("401"),
        "the UI must require a credential when auth is on:\n{response}"
    );

    harness.stop();
}

// ---------------------------------------------------------------------------
// Basic auth against the accounts table

#[tokio::test]
async fn basic_auth_verifies_against_a_real_account() {
    let harness = Harness::with_token().await;

    // The catch-all account exists on a fresh instance but carries no
    // credential, so basic auth cannot succeed for it — and must not be
    // accepted merely because the account is found.
    let header = format!("Basic {}", BASE64.encode("dev:whatever"));
    let response = harness.get("/api/v1/messages", Some(&header)).await;
    assert!(
        status_of(&response).contains("401"),
        "an account with no credential must not authenticate:\n{response}"
    );

    // An account that does not exist must be refused identically — the
    // response must not distinguish the two.
    let unknown = format!("Basic {}", BASE64.encode("nobody:whatever"));
    let unknown_response = harness.get("/api/v1/messages", Some(&unknown)).await;
    assert_eq!(
        status_of(&unknown_response),
        status_of(&response),
        "an unknown user and a wrong password must be indistinguishable"
    );

    harness.stop();
}

// ---------------------------------------------------------------------------
// Credentials never leak

#[tokio::test]
async fn the_token_never_appears_in_a_response() {
    let harness = Harness::with_token().await;

    // `/info` reports the auth *posture*; it must never report the secret that
    // implements it (ADR-035).
    let info = harness
        .get("/api/v1/info", Some(&format!("Bearer {TOKEN}")))
        .await;
    assert!(
        !info.contains(TOKEN),
        "the token leaked through /info:\n{info}"
    );

    let rejected = harness.get("/api/v1/messages", Some("Bearer wrong")).await;
    assert!(
        !rejected.contains(TOKEN),
        "the token leaked through a 401:\n{rejected}"
    );

    harness.stop();
}

#[tokio::test]
async fn a_token_is_not_printed_by_debug() {
    // The configuration derives `Debug`, so a bare `String` token would be
    // printed by any `{:?}` of it — into a log, a panic message, a bug report.
    let token = ApiToken::new(TOKEN);
    let rendered = format!("{token:?}");
    assert!(
        !rendered.contains(TOKEN),
        "ApiToken's Debug leaked the value: {rendered}"
    );

    let config = sandesh_config::HttpConfig {
        auth_token: Some(ApiToken::new(TOKEN)),
        ..sandesh_config::HttpConfig::default()
    };
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains(TOKEN),
        "HttpConfig's Debug leaked the token: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// HTTPS

#[tokio::test]
async fn https_serves_the_api_over_tls() {
    let harness = Harness::start(|config| {
        config.http.tls_enabled = true;
    })
    .await;

    // A real TLS client against the self-signed certificate Sandesh generates
    // (ADR-034), verifying nothing — the point is that the handshake completes
    // and HTTP flows inside it, not that the certificate chains to a CA.
    let response = tls_get(harness.http, "/api/v1/health").await;
    assert!(
        response.contains("200"),
        "HTTPS did not serve the API:\n{response}"
    );

    harness.stop();
}

#[tokio::test]
async fn a_plaintext_request_to_the_tls_port_does_not_succeed() {
    let harness = Harness::start(|config| {
        config.http.tls_enabled = true;
    })
    .await;

    // Speaking HTTP at an HTTPS port must not be answered as if it were HTTP.
    // The handshake fails and the connection closes; what must never happen is
    // a 200 with mail in it.
    let mut stream = TcpStream::connect(harness.http).await.expect("connects");
    let _ = stream
        .write_all(b"GET /api/v1/messages HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    let text = String::from_utf8_lossy(&response);

    assert!(
        !text.contains("200 OK"),
        "plaintext was served on the TLS port:\n{text}"
    );

    harness.stop();
}

#[tokio::test]
async fn a_failed_handshake_does_not_stop_the_server() {
    let harness = Harness::start(|config| {
        config.http.tls_enabled = true;
    })
    .await;

    // Garbage first, twice: a port scanner is the ordinary case, and a listener
    // that dies on one is trivially deniable.
    for _ in 0..2 {
        let mut stream = TcpStream::connect(harness.http).await.expect("connects");
        let _ = stream
            .write_all(b"\x16\x03\x01garbage-not-a-handshake")
            .await;
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink).await;
    }

    // The server must still be serving.
    let response = tls_get(harness.http, "/api/v1/health").await;
    assert!(
        response.contains("200"),
        "the server stopped after a failed handshake:\n{response}"
    );

    harness.stop();
}

/// Issues a GET over TLS, accepting any certificate.
async fn tls_get(address: SocketAddr, path: &str) -> String {
    use tokio_rustls::TlsConnector;
    use tokio_rustls::rustls::client::danger::{
        HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
    };
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};

    /// Accepts any certificate.
    ///
    /// Correct for this test and nowhere else: the certificate is generated at
    /// startup and self-signed (ADR-034), so there is nothing to chain to. What
    /// is under test is that the handshake completes and HTTP flows inside it.
    #[derive(Debug)]
    struct AcceptAny;

    impl ServerCertVerifier for AcceptAny {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::RSA_PKCS1_SHA256,
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::ED25519,
            ]
        }
    }

    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(config));
    let stream = TcpStream::connect(address).await.expect("connects");
    let name = ServerName::try_from("localhost").expect("a valid name");
    let mut tls = connector
        .connect(name, stream)
        .await
        .expect("the TLS handshake completes");

    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes())
        .await
        .expect("writes the request");

    let mut response = Vec::new();
    let _ = tls.read_to_end(&mut response).await;
    String::from_utf8_lossy(&response).into_owned()
}

/// Basic auth must work end to end against a password the API itself set.
///
/// This is the round trip that the duplicated Argon2 configuration threatened
/// (ADR-075): account creation hashed with one hand-copied set of parameters
/// and verification used another. They matched, so nothing was broken — but
/// nothing compared them either, and the first retune of the canonical
/// constants would have separated them silently. Now both go through
/// `CredentialVerifier`, and this proves the two ends agree.
#[tokio::test]
async fn a_password_set_through_the_api_authenticates_against_it() {
    let harness = Harness::with_token().await;

    let created = harness
        .post_json(
            "/api/v1/accounts",
            r#"{"username":"alice","password":"correct horse battery","addresses":["alice@example.test"]}"#,
            Some(&format!("Bearer {TOKEN}")),
        )
        .await;
    assert!(
        status_of(&created).contains("201"),
        "the account was not created:\n{created}"
    );

    let right = format!("Basic {}", BASE64.encode("alice:correct horse battery"));
    let response = harness.get("/api/v1/messages", Some(&right)).await;
    assert!(
        status_of(&response).contains("200"),
        "a password set through the API did not authenticate against it:\n{response}"
    );

    let wrong = format!("Basic {}", BASE64.encode("alice:wrong"));
    let refused = harness.get("/api/v1/messages", Some(&wrong)).await;
    assert!(
        status_of(&refused).contains("401"),
        "a wrong password was accepted:\n{refused}"
    );

    harness.stop();
}

/// `/metrics` is protected, and liveness remains the only exemption.
///
/// A scrape discloses message counts, mailbox counts and delivery volumes —
/// strictly more than `/info`, which ADR-075 already declined to exempt on the
/// grounds that it reports deployment state. The Prometheus convention of a
/// bare, open `/metrics` is not a reason to publish that to an unauthenticated
/// caller: a scraper can carry a bearer token in its scrape configuration,
/// where a container orchestrator's liveness probe cannot.
///
/// This test is what makes that a property rather than a comment. Adding
/// `/metrics` to `EXEMPT_PATHS` fails it.
#[tokio::test]
async fn the_metrics_scrape_requires_a_credential() {
    let app = Harness::with_token().await;

    let anonymous = app.get("/metrics", None).await;
    assert!(
        anonymous.starts_with("HTTP/1.1 401"),
        "an unauthenticated scrape must be refused:\n{anonymous}"
    );
    // The refusal must not leak the very figures it is withholding.
    assert!(
        !anonymous.contains("sandesh_messages"),
        "a 401 body must not carry metric samples:\n{anonymous}"
    );

    let authorised = app.get("/metrics", Some(&format!("Bearer {TOKEN}"))).await;
    assert!(
        authorised.starts_with("HTTP/1.1 200"),
        "a scraper presenting the token must be served:\n{authorised}"
    );
    assert!(
        authorised.contains("sandesh_messages"),
        "the scrape body must carry the samples:\n{authorised}"
    );

    app.shutdown.cancel();
}
