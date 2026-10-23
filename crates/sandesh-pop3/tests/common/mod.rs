//! Integration test harness for sandesh-pop3.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use sandesh_config::{AuthMode, Pop3Config};
use sandesh_core::{
    Account, AccountId, AccountStore, DeliverySource, EmailAddress, Envelope, InsertedMessage,
    MailboxId, MailboxName, MailboxStore, MessageFlags, MessageStore, NewMessage, NewPlacement,
    RawMessage, Timestamp,
};
use sandesh_pop3::Pop3Server;
use sandesh_proto::CancellationToken;
use sandesh_storage::{
    CollectingSink, Database, EventRecorder, NullBlobStore, SqliteAccountStore, SqliteMailboxStore,
    SqliteMessageStore,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// A running POP3 test server.
pub struct TestServer {
    pub addr: Option<SocketAddr>,
    pub tls_addr: Option<SocketAddr>,
    pub db: Database,
    pub accounts: Arc<SqliteAccountStore>,
    pub mailboxes: Arc<SqliteMailboxStore>,
    pub messages: Arc<SqliteMessageStore>,
    shutdown: CancellationToken,
    _tls_dir: Option<tempfile::TempDir>,
}

impl TestServer {
    pub fn base_config() -> Pop3Config {
        Pop3Config {
            bind: Some("127.0.0.1:0".parse().expect("valid address")),
            implicit_tls_bind: None,
            hostname: String::from("sandesh.test"),
            command_timeout: Duration::from_secs(5),
            ..Pop3Config::default()
        }
    }

    pub fn auth_required_with_stls() -> Pop3Config {
        Pop3Config {
            auth_mode: AuthMode::Required,
            start_tls: true,
            ..Self::base_config()
        }
    }

    pub fn implicit_tls_only() -> Pop3Config {
        Pop3Config {
            bind: None,
            implicit_tls_bind: Some("127.0.0.1:0".parse().expect("valid address")),
            auth_mode: AuthMode::Required,
            start_tls: false,
            ..Self::base_config()
        }
    }

    pub async fn start() -> Self {
        Self::with_config(Self::base_config()).await
    }

    pub async fn with_config(config: Pop3Config) -> Self {
        Self::build(config, false).await
    }

    pub async fn with_tls(config: Pop3Config) -> Self {
        Self::build(config, true).await
    }

    async fn build(config: Pop3Config, tls: bool) -> Self {
        let db = Database::open_in_memory().await.expect("open database");
        let events = EventRecorder::new(Arc::new(CollectingSink::new()));
        let accounts = Arc::new(SqliteAccountStore::new(db.pool().clone(), events.clone()));
        let mailboxes = Arc::new(SqliteMailboxStore::new(db.pool().clone(), events.clone()));
        let blobs = Arc::new(NullBlobStore);
        let messages = Arc::new(SqliteMessageStore::new(db.pool().clone(), blobs, events));

        let mut tls_dir = None;
        let server = Pop3Server::new(
            config,
            accounts.clone(),
            mailboxes.clone(),
            messages.clone(),
        );

        let server = if tls {
            let dir = tempfile::tempdir().expect("temporary directory");
            let identity =
                sandesh_proto::Identity::load_or_generate(dir.path()).expect("generate identity");
            let acceptor = identity.acceptor().expect("build acceptor");
            tls_dir = Some(dir);
            server.with_tls(acceptor)
        } else {
            server
        };

        let (bound, _warnings) = server.bind().await.expect("bind server");
        let status = bound.status();
        let addr = status.local_addr;
        let tls_addr = status.tls_addr;

        let shutdown = CancellationToken::new();
        let server_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = bound.serve(server_shutdown).await;
        });

        Self {
            addr,
            tls_addr,
            db,
            accounts,
            mailboxes,
            messages,
            shutdown,
            _tls_dir: tls_dir,
        }
    }

    pub fn cleartext(&self) -> SocketAddr {
        self.addr.expect("cleartext listener configured")
    }

    pub fn implicit(&self) -> SocketAddr {
        self.tls_addr.expect("implicit TLS listener configured")
    }

    pub async fn create_account(&self, username: &str) -> (AccountId, MailboxId) {
        let account = Account {
            id: AccountId::new(),
            username: username.to_owned(),
            credential: None,
            is_catch_all: false,
            created_at: Timestamp::now(),
        };
        self.accounts
            .create(&account)
            .await
            .expect("create account");
        self.accounts
            .add_identity(
                account.id,
                &EmailAddress::parse_lenient(&format!("{username}@sandesh.test")),
                true,
            )
            .await
            .expect("add identity");

        let inbox_name = MailboxName::new(sandesh_core::INBOX).expect("inbox name");
        let inbox = self
            .mailboxes
            .get_by_name(account.id, &inbox_name)
            .await
            .expect("fetch inbox")
            .expect("inbox exists");

        (account.id, inbox.id)
    }

    pub async fn account_with_password(
        &self,
        username: &str,
        password: &str,
    ) -> (AccountId, MailboxId) {
        let (id, inbox_id) = self.create_account(username).await;
        let hash = sandesh_proto::hash_password(&sandesh_proto::Secret::new(password))
            .expect("hash password");

        let affected = sqlx::query("UPDATE accounts SET credential_hash = ? WHERE id = ?")
            .bind(hash)
            .bind(&id.as_bytes()[..])
            .execute(self.db.pool())
            .await
            .expect("store credential")
            .rows_affected();
        assert_eq!(affected, 1);

        (id, inbox_id)
    }

    pub async fn insert_message(&self, mailbox_id: MailboxId, raw: &[u8]) -> InsertedMessage {
        let raw_msg = RawMessage::new(raw.to_vec());
        let parsed = sandesh_core::parse(raw_msg.bytes());
        let now = Timestamp::now();

        self.messages
            .insert(&NewMessage {
                raw: raw_msg,
                envelope: Envelope::default(),
                source: DeliverySource::Smtp,
                parsed,
                received_at: now,
                placements: vec![NewPlacement {
                    mailbox_id,
                    flags: MessageFlags::empty(),
                    internal_date: now,
                }],
            })
            .await
            .expect("insert message")
    }

    pub async fn connect(&self) -> TestClient {
        let stream = TcpStream::connect(self.cleartext()).await.expect("connect");
        let mut client = TestClient::new(stream);
        client.greeting = client.read_line().await;
        client
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// A line-buffered TCP client for testing POP3 sessions.
pub struct TestClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    pub greeting: String,
}

impl TestClient {
    pub fn new(stream: TcpStream) -> Self {
        let (read, write) = stream.into_split();
        Self {
            reader: BufReader::new(read),
            writer: write,
            greeting: String::new(),
        }
    }

    pub async fn send_line(&mut self, line: &str) {
        let line_to_send = if line.ends_with("\r\n") {
            line.to_string()
        } else {
            format!("{line}\r\n")
        };
        self.writer
            .write_all(line_to_send.as_bytes())
            .await
            .expect("write line");
    }

    pub async fn read_line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).await.expect("read line");
        line
    }

    pub async fn cmd(&mut self, line: &str) -> String {
        self.send_line(line).await;
        self.read_line().await
    }

    pub async fn read_multiline(&mut self) -> (String, Vec<String>) {
        let status_line = self.read_line().await;
        let mut lines = Vec::new();

        if status_line.starts_with("+OK") {
            loop {
                let line = self.read_line().await;
                if line == ".\r\n" || line == ".\n" {
                    break;
                }
                // Strip terminating CRLF
                let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
                lines.push(trimmed);
            }
        }

        (status_line, lines)
    }

    pub async fn cmd_multiline(&mut self, line: &str) -> (String, Vec<String>) {
        self.send_line(line).await;
        self.read_multiline().await
    }

    /// Reads a multi-line response and reverses the dot-stuffing, returning the
    /// status line and the exact payload bytes a real client would reconstruct.
    ///
    /// `read_multiline` trims line endings and returns `String`s, which is
    /// precisely the information needed to check byte-exactness, so anything
    /// asserting on octets has to read the socket this way instead.
    pub async fn read_multiline_bytes(&mut self) -> (String, Vec<u8>) {
        let status_line = self.read_line().await;
        let mut payload = Vec::new();

        if status_line.starts_with("+OK") {
            loop {
                let line = self.read_line().await;
                if line == ".\r\n" || line == ".\n" {
                    break;
                }
                // RFC 1939 §3: a leading '.' on the wire is an escape, and the
                // client strips exactly one before storing the line.
                let unstuffed = line.strip_prefix('.').unwrap_or(&line);
                payload.extend_from_slice(unstuffed.as_bytes());
            }
        }

        (status_line, payload)
    }

    pub async fn cmd_multiline_bytes(&mut self, line: &str) -> (String, Vec<u8>) {
        self.send_line(line).await;
        self.read_multiline_bytes().await
    }

    pub fn into_stream(self) -> TcpStream {
        self.reader
            .into_inner()
            .reunite(self.writer)
            .expect("reunite TCP stream")
    }
}

/// A line-buffered client over any async read/write stream.
pub struct StreamClient<S> {
    reader: BufReader<tokio::io::ReadHalf<S>>,
    writer: tokio::io::WriteHalf<S>,
    pub greeting: String,
}

impl<S> StreamClient<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    pub fn new(stream: S) -> Self {
        let (read, write) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(read),
            writer: write,
            greeting: String::new(),
        }
    }

    pub async fn send_line(&mut self, line: &str) {
        let line_to_send = if line.ends_with("\r\n") {
            line.to_string()
        } else {
            format!("{line}\r\n")
        };
        self.writer
            .write_all(line_to_send.as_bytes())
            .await
            .expect("write line");
    }

    pub async fn read_line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).await.expect("read line");
        line
    }

    pub async fn cmd(&mut self, line: &str) -> String {
        self.send_line(line).await;
        self.read_line().await
    }

    pub async fn read_multiline(&mut self) -> (String, Vec<String>) {
        let status_line = self.read_line().await;
        let mut lines = Vec::new();

        if status_line.starts_with("+OK") {
            loop {
                let line = self.read_line().await;
                if line == ".\r\n" || line == ".\n" {
                    break;
                }
                let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
                lines.push(trimmed);
            }
        }

        (status_line, lines)
    }

    pub async fn cmd_multiline(&mut self, line: &str) -> (String, Vec<String>) {
        self.send_line(line).await;
        self.read_multiline().await
    }
}

pub mod tls_client {
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

    pub fn config() -> Arc<ClientConfig> {
        Arc::new(
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(TrustAny))
                .with_no_client_auth(),
        )
    }

    pub async fn connect(
        addr: std::net::SocketAddr,
    ) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
        let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        handshake(stream).await
    }

    pub async fn handshake(
        stream: tokio::net::TcpStream,
    ) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
        let connector = tokio_rustls::TlsConnector::from(config());
        let name = rustls::pki_types::ServerName::try_from("localhost").expect("server name");
        connector
            .connect(name, stream)
            .await
            .expect("TLS handshake")
    }
}

pub fn sasl_plain(username: &str, password: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(format!("\0{username}\0{password}"))
}

pub fn sasl_field(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(value)
}
