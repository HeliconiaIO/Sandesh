//! Shared harness for storage integration tests.
//!
//! Every test gets an isolated in-memory database, so tests never share state
//! and are safe to run in parallel.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sandesh_core::{
    Account, AccountId, AccountStore, DeliverySource, Envelope, MailboxId, MailboxName,
    MailboxStore, Message, MessageFlags, MessageStore, NewMessage, NewPlacement, RawMessage,
    Timestamp,
};
use sandesh_storage::{
    CollectingSink, Database, EventRecorder, NullBlobStore, SqliteAccountStore, SqliteMailboxStore,
    SqliteMessageStore,
};
use std::sync::Arc;

/// A fully wired storage stack over an isolated database.
pub struct Harness {
    pub db: Database,
    pub accounts: SqliteAccountStore,
    pub mailboxes: SqliteMailboxStore,
    pub messages: SqliteMessageStore,
    pub sink: Arc<CollectingSink>,
}

impl Harness {
    /// Builds a harness over a private in-memory database.
    pub async fn new() -> Self {
        let db = Database::open_in_memory().await.expect("open database");
        Self::over(db)
    }

    /// Builds a harness over a database file, so a test can close it and open
    /// it again -- which is what "survives a restart" means for the outbox.
    pub async fn on_disk(path: impl AsRef<std::path::Path>) -> Self {
        let db = Database::open(path).await.expect("open database");
        Self::over(db)
    }

    /// Wires the stores over an already-open database.
    fn over(db: Database) -> Self {
        let sink = Arc::new(CollectingSink::new());
        let events = EventRecorder::new(sink.clone());

        Self {
            accounts: SqliteAccountStore::new(db.pool().clone(), events.clone()),
            mailboxes: SqliteMailboxStore::new(db.pool().clone(), events.clone()),
            messages: SqliteMessageStore::new(db.pool().clone(), Arc::new(NullBlobStore), events),
            sink,
            db,
        }
    }

    /// Creates an account with one address, returning its id.
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
            .add_identity(
                account.id,
                &sandesh_core::EmailAddress::parse_lenient(address),
                true,
            )
            .await
            .expect("add identity");
        account.id
    }

    /// Looks up a mailbox by name, which must exist.
    pub async fn mailbox(&self, account: AccountId, name: &str) -> MailboxId {
        self.mailboxes
            .get_by_name(account, &MailboxName::new(name).expect("valid name"))
            .await
            .expect("query mailbox")
            .expect("mailbox exists")
            .id
    }

    /// Delivers `raw` into each named mailbox, returning the insert result.
    pub async fn deliver(
        &self,
        raw: &[u8],
        mailboxes: &[MailboxId],
    ) -> sandesh_core::InsertedMessage {
        let raw = RawMessage::new(raw.to_vec());
        let parsed = sandesh_core::parse(raw.bytes());
        let now = Timestamp::now();

        self.messages
            .insert(&NewMessage {
                raw,
                envelope: Envelope::default(),
                source: DeliverySource::Smtp,
                parsed,
                received_at: now,
                placements: mailboxes
                    .iter()
                    .map(|id| NewPlacement {
                        mailbox_id: *id,
                        flags: MessageFlags::empty(),
                        internal_date: now,
                    })
                    .collect(),
            })
            .await
            .expect("insert message")
    }

    /// Fetches a stored message.
    pub async fn get_message(&self, id: sandesh_core::MessageId) -> Message {
        self.messages
            .get(id)
            .await
            .expect("query message")
            .expect("message exists")
    }
}

/// Loads a fixture from `testdata/`.
pub fn fixture(relative: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .join(relative);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// The corpus categories, as `TESTING_STRATEGY.md` §4 defines them.
///
/// Named explicitly rather than discovered, so a category that is deleted or
/// renamed fails the build instead of quietly shrinking the corpus every
/// "every fixture" assertion runs over. Adding a category means adding it here,
/// which is the point: the list is the contract.
pub const CATEGORIES: [&str; 5] = [
    "valid",
    "encodings",
    "malformed",
    "adversarial",
    "real_world",
];

/// Every fixture in the corpus, as `(name, bytes)`, sorted by name.
///
/// Sorted so a failure names the same fixture on every machine, regardless of
/// how the filesystem happens to enumerate a directory.
pub fn all_fixtures() -> Vec<(String, Vec<u8>)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
    let mut found = Vec::new();

    for category in CATEGORIES {
        let dir = root.join(category);
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read dir {}: {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "eml") {
                let name = format!(
                    "{category}/{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                found.push((name, std::fs::read(&path).expect("read fixture")));
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}
