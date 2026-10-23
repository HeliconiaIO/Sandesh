//! Integration harness: a real SMTP server over a real TCP socket.
//!
//! Every instance binds an **ephemeral port**, so tests never collide and can
//! run in parallel. Each gets its own in-memory database, so no state is
//! shared. The server is cancelled on drop, leaving no orphaned tasks.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sandesh_config::SmtpConfig;
use sandesh_core::{Account, AccountId, AccountStore, EmailAddress, Timestamp};
use sandesh_proto::CancellationToken;
use sandesh_smtp::{DeliveryService, SmtpServer};
use sandesh_storage::{
    CollectingSink, Database, EventRecorder, NullBlobStore, SqliteAccountStore,
    SqliteConnectionStore, SqliteDeliveryStore, SqliteMailboxStore, SqliteMessageStore,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Converts the configured routing policy into the domain's.
///
/// `DeliveryService` takes the domain enum (ADR-071). The composition root has
/// the same conversion; a test that builds the service by hand needs it too,
/// and one copy here beats one per test file.
#[allow(dead_code)]
pub fn routing_policy(
    policy: sandesh_config::UnknownRecipientPolicy,
) -> sandesh_core::UnknownRecipient {
    match policy {
        sandesh_config::UnknownRecipientPolicy::CatchAll => {
            sandesh_core::UnknownRecipient::CatchAll
        }
        sandesh_config::UnknownRecipientPolicy::AutoCreate => {
            sandesh_core::UnknownRecipient::AutoCreate
        }
        sandesh_config::UnknownRecipientPolicy::Strict => sandesh_core::UnknownRecipient::Strict,
    }
}

/// A running SMTP server plus the storage behind it.
pub struct TestServer {
    pub addr: SocketAddr,
    pub db: Database,
    pub accounts: Arc<SqliteAccountStore>,
    pub mailboxes: Arc<SqliteMailboxStore>,
    pub messages: Arc<SqliteMessageStore>,
    pub deliveries: Arc<SqliteDeliveryStore>,
    pub connections: Arc<SqliteConnectionStore>,
    pub sink: Arc<CollectingSink>,
    /// The runtime fault registry this server consults (ADR-084).
    pub faults: Arc<sandesh_core::FaultRegistry>,
    shutdown: CancellationToken,
}

impl TestServer {
    /// Starts a server with default configuration on an ephemeral port.
    pub async fn start() -> Self {
        Self::with_config(Self::base_config()).await
    }

    /// A server with connection tracing switched off, which is the default a
    /// real deployment gets unless it opts in.
    pub async fn start_untraced() -> Self {
        Self::build_inner(Self::base_config(), None, false).await
    }

    /// Default test configuration: loopback, port 0, short timeouts.
    pub fn base_config() -> SmtpConfig {
        SmtpConfig {
            bind: "127.0.0.1:0".parse().expect("valid address"),
            hostname: String::from("sandesh.test"),
            // Short enough that timeout tests finish quickly, long enough that
            // ordinary tests never trip them.
            command_timeout: Duration::from_secs(5),
            data_timeout: Duration::from_secs(5),
            ..SmtpConfig::default()
        }
    }

    /// Starts a server with the given configuration and a TLS acceptor.
    ///
    /// The identity is generated into a temporary directory, so the handshake
    /// is real: a test drives an actual `rustls` server, not a stub.
    pub async fn with_tls(config: SmtpConfig) -> (Self, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temporary directory");
        let identity =
            sandesh_proto::Identity::load_or_generate(dir.path()).expect("generate identity");
        let acceptor = identity.acceptor().expect("build acceptor");
        (Self::build(config, Some(acceptor)).await, dir)
    }

    /// Starts a server with the given configuration.
    pub async fn with_config(config: SmtpConfig) -> Self {
        Self::build(config, None).await
    }

    /// Builds the server, optionally with TLS.
    async fn build(config: SmtpConfig, tls: Option<tokio_rustls::TlsAcceptor>) -> Self {
        Self::build_inner(config, tls, true).await
    }

    async fn build_inner(
        config: SmtpConfig,
        tls: Option<tokio_rustls::TlsAcceptor>,
        traced: bool,
    ) -> Self {
        let db = Database::open_in_memory().await.expect("open database");
        let sink = Arc::new(CollectingSink::new());
        let events = EventRecorder::new(sink.clone());

        let accounts = Arc::new(SqliteAccountStore::new(db.pool().clone(), events.clone()));
        let mailboxes = Arc::new(SqliteMailboxStore::new(db.pool().clone(), events.clone()));
        let messages = Arc::new(SqliteMessageStore::new(
            db.pool().clone(),
            Arc::new(NullBlobStore),
            events.clone(),
        ));
        let deliveries = Arc::new(SqliteDeliveryStore::new(db.pool().clone(), events));
        let connections = Arc::new(SqliteConnectionStore::new(db.pool().clone()));

        let policy = routing_policy(config.unknown_recipient);
        let delivery = Arc::new(DeliveryService::new(
            accounts.clone(),
            mailboxes.clone(),
            messages.clone(),
            deliveries.clone(),
            policy,
        ));

        let server = SmtpServer::new(config, delivery);
        let faults = server.faults();
        let server = if traced {
            server.with_tracing(sandesh_proto::ConnectionRecorder::new(
                sandesh_core::Protocol::Smtp,
                connections.clone(),
            ))
        } else {
            server
        };
        let server = match tls {
            Some(acceptor) => server.with_tls(acceptor),
            None => server,
        };
        let (bound, _warnings) = server.bind().await.expect("bind server");
        let addr = bound.local_addr();

        let shutdown = CancellationToken::new();
        let server_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = bound.serve(server_shutdown).await;
        });

        Self {
            addr,
            db,
            accounts,
            mailboxes,
            messages,
            deliveries,
            connections,
            sink,
            faults,
            shutdown,
        }
    }

    /// Creates an account with a password, for authentication tests.
    ///
    /// The credential is stored as an Argon2id hash through the same code path
    /// production uses, so a test cannot accidentally pass against a shortcut.
    pub async fn account_with_password(
        &self,
        username: &str,
        address: &str,
        password: &str,
    ) -> AccountId {
        let id = self.account(username, address).await;

        let hash = sandesh_proto::hash_password(&sandesh_proto::Secret::new(password))
            .expect("hash the password");

        // Ids are stored as raw bytes, not as a canonical string — binding a
        // string here silently matches no rows and the test then fails on the
        // authentication rather than on the setup.
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

    /// Creates an account with one address.
    pub async fn account(&self, username: &str, address: &str) -> AccountId {
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
            .add_identity(account.id, &EmailAddress::parse_lenient(address), true)
            .await
            .expect("add identity");
        account.id
    }

    /// Creates the catch-all account, which unmatched mail routes to.
    pub async fn catch_all(&self) -> AccountId {
        let account = Account {
            id: AccountId::new(),
            username: String::from("catch-all"),
            credential: None,
            is_catch_all: true,
            created_at: Timestamp::now(),
        };
        self.accounts
            .create(&account)
            .await
            .expect("create catch-all");
        account.id
    }

    /// Opens a client connection and reads the greeting.
    pub async fn connect(&self) -> SmtpClient {
        SmtpClient::connect(self.addr).await
    }

    /// The INBOX of an account.
    pub async fn inbox(&self, account: AccountId) -> sandesh_core::MailboxId {
        use sandesh_core::{MailboxName, MailboxStore};
        self.mailboxes
            .get_by_name(account, &MailboxName::new("INBOX").expect("name"))
            .await
            .expect("query")
            .expect("INBOX exists")
            .id
    }

    /// Every message stored, oldest first.
    pub async fn stored_messages(&self) -> Vec<sandesh_core::MessageId> {
        let rows: Vec<(Vec<u8>,)> = sqlx::query_as("SELECT id FROM messages ORDER BY received_at")
            .fetch_all(self.db.pool())
            .await
            .expect("query messages");
        rows.into_iter()
            .filter_map(|(id,)| {
                <[u8; 16]>::try_from(id.as_slice())
                    .ok()
                    .map(sandesh_core::MessageId::from_bytes)
            })
            .collect()
    }

    /// How many messages are stored.
    pub async fn message_count(&self) -> i64 {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
            .fetch_one(self.db.pool())
            .await
            .expect("count messages");
        count
    }

    /// How many placements exist.
    pub async fn entry_count(&self) -> i64 {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM message_entries")
            .fetch_one(self.db.pool())
            .await
            .expect("count entries");
        count
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Stop accepting so no task outlives the test.
        self.shutdown.cancel();
    }
}

/// A minimal line-oriented SMTP client for driving the server over TCP.
pub struct SmtpClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    /// The greeting, captured at connect.
    pub greeting: String,
}

impl SmtpClient {
    /// Connects and reads the greeting.
    pub async fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).await.expect("connect");
        let (read, writer) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(read),
            writer,
            greeting: String::new(),
        };
        client.greeting = client.read_reply().await;
        client
    }

    /// Sends a line and returns the reply.
    pub async fn send(&mut self, line: &str) -> String {
        self.write_raw(format!("{line}\r\n").as_bytes()).await;
        self.read_reply().await
    }

    /// Writes raw bytes without reading a reply.
    ///
    /// Used by fragmentation tests, which need control over packet boundaries.
    pub async fn write_raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).await.expect("write");
        self.writer.flush().await.expect("flush");
    }

    /// Reads one complete reply, following multi-line continuations.
    pub async fn read_reply(&mut self) -> String {
        let mut reply = String::new();
        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .await
                .expect("read reply line");
            if read == 0 {
                break;
            }
            reply.push_str(&line);

            // A continuation has '-' as the fourth character; the final line
            // has a space.
            let continues = line.as_bytes().get(3) == Some(&b'-');
            if !continues {
                break;
            }
        }
        reply
    }

    /// Sends a complete message, applying dot-stuffing.
    ///
    /// Returns the reply to the terminator, i.e. the delivery result.
    pub async fn send_data(&mut self, content: &[u8]) -> String {
        let stuffed = sandesh_smtp::framing::stuff(content);
        self.write_raw(&stuffed).await;
        // The terminator needs a preceding CRLF unless the content already
        // ends with one.
        if !content.ends_with(b"\r\n") && !content.is_empty() {
            self.write_raw(b"\r\n").await;
        }
        self.write_raw(b".\r\n").await;
        self.read_reply().await
    }

    /// Sends one `BDAT` chunk and returns the reply (RFC 3030).
    ///
    /// Deliberately writes the payload verbatim: `BDAT` applies **no**
    /// dot-stuffing, so a helper that stuffed would hide exactly the bug this
    /// path must not have.
    pub async fn bdat(&mut self, content: &[u8], last: bool) -> String {
        let suffix = if last { " LAST" } else { "" };
        self.write_raw(format!("BDAT {}{suffix}\r\n", content.len()).as_bytes())
            .await;
        self.write_raw(content).await;
        self.read_reply().await
    }

    /// Runs a full transaction and returns the final reply.
    pub async fn deliver(&mut self, from: &str, to: &[&str], content: &[u8]) -> String {
        self.send(&format!("MAIL FROM:<{from}>")).await;
        for recipient in to {
            self.send(&format!("RCPT TO:<{recipient}>")).await;
        }
        self.send("DATA").await;
        self.send_data(content).await
    }
}

/// The reply code from a reply string.
pub fn code_of(reply: &str) -> u16 {
    reply
        .get(..3)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no reply code in {reply:?}"))
}

/// Loads a fixture from `testdata/`.
pub fn fixture(relative: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .join(relative);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}
