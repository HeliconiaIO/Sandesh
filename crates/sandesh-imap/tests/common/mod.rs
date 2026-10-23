//! Integration harness: a real IMAP server over a real TCP socket.
//!
//! Every instance binds an **ephemeral port**, so tests never collide and run
//! in parallel. Each gets its own in-memory database, so no state is shared.
//! The server is cancelled on drop, leaving no orphaned tasks.
//!
//! Nothing here is stubbed. The server is the production `ImapServer`, the
//! storage is the production `SqliteAccountStore`, credentials are hashed by
//! the production Argon2id code path, and TLS is a real `rustls` handshake —
//! so a test cannot pass against a shortcut that production does not take.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use sandesh_config::{AuthMode, ImapConfig};
use sandesh_core::{
    Account, AccountId, AccountStore, DeliverySource, EmailAddress, Mailbox, MailboxAttributes,
    MailboxId, MailboxName, MailboxStore, MessageEntry, MessageFlags, MessageStore, NewMessage,
    NewPlacement, RawMessage, Timestamp, Uid, UidValidity,
};
use sandesh_imap::ImapServer;
use sandesh_proto::CancellationToken;
use sandesh_storage::{
    CollectingSink, Database, EventRecorder, NullBlobStore, SqliteAccountStore, SqliteMailboxStore,
    SqliteMessageStore,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A running IMAP server plus the storage behind it.
pub struct TestServer {
    /// The cleartext address, when a cleartext listener is configured.
    pub addr: Option<SocketAddr>,
    /// The implicit-TLS address, when one is configured.
    pub tls_addr: Option<SocketAddr>,
    pub db: Database,
    pub accounts: Arc<SqliteAccountStore>,
    pub mailboxes: Arc<SqliteMailboxStore>,
    pub messages: Arc<SqliteMessageStore>,
    /// Every domain event the stores emitted, for tests that assert on them.
    pub events: Arc<CollectingSink>,
    shutdown: CancellationToken,
    /// Kept alive so the generated certificate outlives the server.
    _tls_dir: Option<tempfile::TempDir>,
}

impl TestServer {
    /// Default test configuration: loopback, ephemeral port, short timeouts.
    ///
    /// `auth_mode` is `Disabled` and TLS is off, matching production defaults;
    /// tests opt in to what they exercise.
    pub fn base_config() -> ImapConfig {
        ImapConfig {
            bind: Some("127.0.0.1:0".parse().expect("valid address")),
            implicit_tls_bind: None,
            hostname: String::from("sandesh.test"),
            // Short enough that timeout tests finish quickly, long enough that
            // ordinary tests never trip them.
            command_timeout: Duration::from_secs(5),
            ..ImapConfig::default()
        }
    }

    /// A configuration with authentication required and STARTTLS available.
    pub fn auth_over_start_tls() -> ImapConfig {
        ImapConfig {
            auth_mode: AuthMode::Required,
            start_tls: true,
            ..Self::base_config()
        }
    }

    /// A configuration with an implicit-TLS listener and no cleartext port.
    pub fn implicit_tls_only() -> ImapConfig {
        ImapConfig {
            bind: None,
            implicit_tls_bind: Some("127.0.0.1:0".parse().expect("valid address")),
            auth_mode: AuthMode::Required,
            start_tls: false,
            ..Self::base_config()
        }
    }

    /// Starts a server with default configuration.
    pub async fn start() -> Self {
        Self::with_config(Self::base_config()).await
    }

    /// Starts a server without TLS material.
    pub async fn with_config(config: ImapConfig) -> Self {
        Self::build(config, false).await
    }

    /// Starts a server with a live bus and an explicit configuration.
    ///
    /// Used by the `IDLE` timeout test, which needs a ceiling it can actually
    /// reach; every other IDLE test wants the default.
    pub async fn with_events_and_config(config: ImapConfig) -> Self {
        Self::build_with(config, false, true).await
    }

    /// Starts a server with a live event bus, so `IDLE` has something to report.
    ///
    /// Separate from [`start`](Self::start) deliberately: a server without an
    /// event source does not advertise `IDLE`, and most tests want exactly that
    /// baseline. Only the tests that exercise unsolicited updates pay for a bus.
    pub async fn with_events() -> Self {
        Self::build_with(Self::base_config(), false, true).await
    }

    /// Starts a server with a real, freshly generated TLS identity.
    pub async fn with_tls(config: ImapConfig) -> Self {
        Self::build(config, true).await
    }

    async fn build(config: ImapConfig, tls: bool) -> Self {
        Self::build_with(config, tls, false).await
    }

    async fn build_with(config: ImapConfig, tls: bool, live_events: bool) -> Self {
        let db = Database::open_in_memory().await.expect("open database");
        let sink = Arc::new(CollectingSink::new());

        // With a live bus, repositories emit into *both* the bus (which IDLE
        // subscribes to) and the collecting sink (which assertions read). A fan
        // -out rather than a swap, so an IDLE test can still assert on the
        // events that were emitted.
        let bus = live_events.then(|| Arc::new(sandesh_events::EventBus::new()));
        let events = match &bus {
            Some(bus) => EventRecorder::new(Arc::new(FanOut {
                sinks: vec![
                    sink.clone(),
                    Arc::clone(bus) as Arc<dyn sandesh_core::EventSink>,
                ],
            })),
            None => EventRecorder::new(sink.clone()),
        };
        let accounts = Arc::new(SqliteAccountStore::new(db.pool().clone(), events.clone()));
        let mailboxes = Arc::new(SqliteMailboxStore::new(db.pool().clone(), events.clone()));
        let messages = Arc::new(SqliteMessageStore::new(
            db.pool().clone(),
            Arc::new(NullBlobStore),
            events,
        ));

        let mut tls_dir = None;
        let server = ImapServer::new(config, accounts.clone())
            .with_stores(mailboxes.clone(), messages.clone());
        let server = match &bus {
            Some(bus) => server.with_events(Arc::clone(bus) as Arc<dyn sandesh_core::EventSource>),
            None => server,
        };
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
        let addr = bound.local_addr();
        let tls_addr = bound.tls_addr();

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
            events: sink,
            shutdown,
            _tls_dir: tls_dir,
        }
    }

    /// The cleartext address, panicking if there is none.
    pub fn cleartext(&self) -> SocketAddr {
        self.addr.expect("a cleartext listener is configured")
    }

    /// The implicit-TLS address, panicking if there is none.
    pub fn implicit(&self) -> SocketAddr {
        self.tls_addr
            .expect("an implicit-TLS listener is configured")
    }

    /// Creates an account with an Argon2id-hashed password.
    ///
    /// Hashed through the same production code path, so a test cannot pass
    /// against a comparison production does not perform.
    pub async fn account_with_password(&self, username: &str, password: &str) -> AccountId {
        let id = self.account(username).await;

        let hash = sandesh_proto::hash_password(&sandesh_proto::Secret::new(password))
            .expect("hash the password");

        // Ids are stored as raw bytes, not a canonical string: binding a string
        // matches no rows, and the test then fails on authentication rather
        // than on its own setup.
        let affected = sqlx::query("UPDATE accounts SET credential_hash = ? WHERE id = ?")
            .bind(hash)
            .bind(&id.as_bytes()[..])
            .execute(self.db.pool())
            .await
            .expect("store the credential")
            .rows_affected();
        assert_eq!(affected, 1, "the credential must actually be stored");

        id
    }

    /// Creates an account with no password.
    pub async fn account(&self, username: &str) -> AccountId {
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
        account.id
    }

    /// Creates the catch-all account, which developer mode resolves sessions to.
    pub async fn catch_all_account(&self, username: &str) -> AccountId {
        let account = Account {
            id: AccountId::new(),
            username: username.to_owned(),
            credential: None,
            is_catch_all: true,
            created_at: Timestamp::now(),
        };
        self.accounts
            .create(&account)
            .await
            .expect("create catch-all account");
        account.id
    }

    /// Creates a mailbox for an account, returning its id.
    ///
    /// Goes through the production `MailboxStore`, so a test exercises the same
    /// creation path `CREATE` does.
    pub async fn mailbox(&self, account_id: AccountId, name: &str) -> MailboxId {
        self.mailbox_with_attributes(account_id, name, MailboxAttributes::none())
            .await
    }

    /// Creates a mailbox carrying the given name attributes.
    pub async fn mailbox_with_attributes(
        &self,
        account_id: AccountId,
        name: &str,
        attributes: MailboxAttributes,
    ) -> MailboxId {
        let mailbox = Mailbox {
            id: MailboxId::new(),
            account_id,
            name: MailboxName::new(name).expect("valid mailbox name"),
            parent_id: None,
            uid_validity: UidValidity::next_epoch(),
            uid_next: Uid::MIN,
            attributes,
            subscribed: false,
            created_at: Timestamp::now(),
            highest_mod_seq: sandesh_core::ModSeq::MIN,
        };
        // Account creation already provisions the default mailboxes (INBOX,
        // Sent, …), so asking for one of those returns what already exists
        // rather than failing the test's setup.
        if let Some(existing) = self
            .mailboxes
            .get_by_name(account_id, &mailbox.name)
            .await
            .expect("query mailbox")
        {
            return existing.id;
        }
        self.mailboxes
            .create(&mailbox)
            .await
            .expect("create mailbox");
        mailbox.id
    }

    /// Reads a mailbox back by name.
    pub async fn mailbox_by_name(&self, account_id: AccountId, name: &str) -> Option<Mailbox> {
        self.mailboxes
            .get_by_name(account_id, &MailboxName::new(name).expect("valid name"))
            .await
            .expect("query mailbox")
    }

    /// Stores a message into a mailbox, returning the placement created.
    ///
    /// Goes through the production `MessageStore::insert`, so the raw bytes,
    /// the derived MIME structure and the assigned UID all come from the same
    /// code path SMTP delivery uses. A test therefore cannot pass against a
    /// structure the real parser would not have produced.
    pub async fn deliver(&self, mailbox_id: MailboxId, raw: &[u8]) -> MessageEntry {
        self.deliver_with_flags(mailbox_id, raw, MessageFlags::empty())
            .await
    }

    /// Stores a message with the given starting flags.
    pub async fn deliver_with_flags(
        &self,
        mailbox_id: MailboxId,
        raw: &[u8],
        flags: MessageFlags,
    ) -> MessageEntry {
        self.deliver_at(mailbox_id, raw, flags, Timestamp::now())
            .await
    }

    /// Stores a message with explicit flags and internal date.
    ///
    /// The internal date is passed in rather than defaulted so that
    /// `INTERNALDATE` tests can assert a *stored* value that is demonstrably
    /// not the current time.
    pub async fn deliver_at(
        &self,
        mailbox_id: MailboxId,
        raw: &[u8],
        flags: MessageFlags,
        internal_date: Timestamp,
    ) -> MessageEntry {
        let new = NewMessage {
            raw: RawMessage::new(raw.to_vec()),
            envelope: sandesh_core::Envelope::default(),
            source: DeliverySource::Smtp,
            parsed: sandesh_core::parse(raw),
            received_at: Timestamp::now(),
            placements: vec![NewPlacement {
                mailbox_id,
                flags,
                internal_date,
            }],
        };
        let inserted = self.messages.insert(&new).await.expect("insert message");
        inserted
            .entries
            .into_iter()
            .next()
            .expect("one placement was requested")
    }

    /// Reads a fixture from `testdata/`, preserving its bytes exactly.
    pub fn fixture(relative: &str) -> Vec<u8> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata")
            .join(relative);
        std::fs::read(&root).unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
    }

    /// Connects over cleartext and reads the greeting.
    pub async fn connect(&self) -> Client<TcpStream> {
        let stream = TcpStream::connect(self.cleartext()).await.expect("connect");
        let mut client = Client::new(stream);
        client.greeting = client.read_response().await;
        client
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// A line-oriented IMAP client for tests.
///
/// Reads by draining whole responses rather than single lines, because an IMAP
/// command may be answered by several untagged lines before its tagged
/// completion — asserting on only the first line would miss the interesting
/// half.
pub struct Client<S> {
    stream: S,
    pub greeting: String,
    tag: u32,
}

impl<S> Client<S>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            greeting: String::new(),
            tag: 0,
        }
    }

    /// Reads the greeting into [`Self::greeting`].
    pub async fn read_greeting(&mut self) -> String {
        self.greeting = self.read_response().await;
        self.greeting.clone()
    }

    /// Sends a raw line with CRLF appended.
    pub async fn send_line(&mut self, line: &str) {
        self.stream
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .expect("write");
        self.stream.flush().await.expect("flush");
    }

    /// Sends raw bytes verbatim, for framing and literal tests.
    pub async fn send_raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.expect("write");
        self.stream.flush().await.expect("flush");
    }

    /// Issues a tagged command and returns everything up to its completion.
    ///
    /// The tag is generated and returned in the response text, so a test can
    /// assert the server echoed the right one.
    pub async fn command(&mut self, command: &str) -> String {
        let tag = self.next_tag();
        self.send_line(&format!("{tag} {command}")).await;
        self.read_until_tagged(&tag).await
    }

    /// The next tag in sequence, e.g. `A1`.
    pub fn next_tag(&mut self) -> String {
        self.tag += 1;
        format!("A{}", self.tag)
    }

    /// Reads until a line begins with `tag`, returning everything read.
    ///
    /// A continuation request (`+`) also stops the read: it means the server is
    /// waiting for the client, so reading on would deadlock.
    pub async fn read_until_tagged(&mut self, tag: &str) -> String {
        let mut out = String::new();
        loop {
            let chunk = self.read_response().await;
            if chunk.is_empty() {
                // Connection closed.
                return out;
            }
            out.push_str(&chunk);
            if out.lines().any(|line| line.starts_with(&format!("{tag} ")))
                || out.lines().any(|line| line.starts_with("+ "))
            {
                return out;
            }
        }
    }

    /// Reads until a line begins with `tag`, returning the **raw octets**.
    ///
    /// `FETCH` responses carry message content, which is not necessarily valid
    /// UTF-8 and is frequently larger than one socket read. This accumulates
    /// bytes until the tagged completion appears at the start of a line, so a
    /// literal spanning several reads is reassembled intact and binary octets
    /// are not replaced by U+FFFD the way a lossy string conversion would.
    pub async fn read_until_tagged_bytes(&mut self, tag: &str) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let needle = format!("{tag} ").into_bytes();
        loop {
            let mut buf = [0u8; 8192];
            let read =
                tokio::time::timeout(Duration::from_secs(10), self.stream.read(&mut buf)).await;
            match read {
                Ok(Ok(0)) | Err(_) => return out,
                Ok(Ok(n)) => out.extend_from_slice(&buf[..n]),
                Ok(Err(_)) => return out,
            }
            // The tagged line is the terminator, and it only counts at the
            // start of a line — the same octets inside a literal are data.
            if line_starts_with(&out, &needle) {
                return out;
            }
        }
    }

    /// Issues a tagged command and returns the raw response octets.
    pub async fn command_bytes(&mut self, command: &str) -> Vec<u8> {
        let tag = self.next_tag();
        self.send_line(&format!("{tag} {command}")).await;
        self.read_until_tagged_bytes(&tag).await
    }

    /// Reads whatever is currently available, as text.
    ///
    /// Bounded by a short timeout so a test asserting "nothing more arrives"
    /// terminates instead of hanging.
    pub async fn read_response(&mut self) -> String {
        let mut buf = [0u8; 8192];
        match tokio::time::timeout(Duration::from_secs(5), self.stream.read(&mut buf)).await {
            Ok(Ok(0)) | Err(_) => String::new(),
            Ok(Ok(n)) => String::from_utf8_lossy(&buf[..n]).into_owned(),
            Ok(Err(_)) => String::new(),
        }
    }

    /// Whether the peer has closed the connection.
    ///
    /// Reads with a timeout; an empty read means EOF.
    pub async fn is_closed(&mut self) -> bool {
        self.read_response().await.is_empty()
    }

    /// The underlying stream, for handing to a TLS handshake.
    pub fn into_inner(self) -> S {
        self.stream
    }
}

/// A `rustls` client that accepts the test server's self-signed certificate.
///
/// Certificate *validation* is not what these tests exercise: the server
/// generates a fresh self-signed certificate per test, so there is no CA to
/// validate against. What is under test is IMAP behaviour after the handshake.
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

    /// A client configuration that trusts the test server.
    pub fn config() -> Arc<ClientConfig> {
        Arc::new(
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(TrustAny))
                .with_no_client_auth(),
        )
    }

    /// Connects and completes a TLS handshake against `addr`.
    pub async fn connect(
        addr: std::net::SocketAddr,
    ) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
        let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        handshake(stream).await
    }

    /// Completes a TLS handshake over an existing stream.
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

/// Whether any line in `haystack` begins with `needle`.
///
/// Used to find a tagged completion without mistaking the same bytes appearing
/// inside a literal payload for the end of the response.
fn line_starts_with(haystack: &[u8], needle: &[u8]) -> bool {
    if haystack.starts_with(needle) {
        return true;
    }
    haystack
        .windows(needle.len() + 1)
        .any(|window| window.first() == Some(&b'\n') && &window[1..] == needle)
}

/// Base64-encodes a SASL PLAIN payload: `\0user\0password`.
pub fn sasl_plain(username: &str, password: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(format!("\0{username}\0{password}"))
}

/// Base64-encodes one SASL LOGIN field.
pub fn sasl_field(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(value)
}

/// An [`EventSink`](sandesh_core::EventSink) that forwards to several others.
///
/// Lets an IDLE test hold both a live bus, for the session to subscribe to, and
/// a collecting sink, for assertions — without either one hiding the other.
struct FanOut {
    sinks: Vec<Arc<dyn sandesh_core::EventSink>>,
}

impl sandesh_core::EventSink for FanOut {
    fn emit(&self, event: sandesh_core::DomainEvent) {
        for sink in &self.sinks {
            sink.emit(event.clone());
        }
    }
}
