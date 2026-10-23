//! The §73 architecture audit, as executable checks.
//!
//! The claim under test is that SMTP is a genuine *adapter*: it operates
//! against the core's ports, so a completely different storage implementation
//! substitutes without the protocol layer noticing. The proof is a full SMTP
//! delivery against in-memory doubles — no SQLite, no filesystem, no database
//! crate anywhere in the process.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use async_trait::async_trait;
use sandesh_config::SmtpConfig;
use sandesh_core::{
    Account, AccountId, AccountStore, Attachment, AttachmentId, CoreError, CoreResult, Delivery,
    DeliveryId, DeliveryStore, EmailAddress, EmailIdentity, EntityKind, EntryId, FlagUpdate,
    IdentityId, InsertedMessage, Mailbox, MailboxCounts, MailboxId, MailboxName, MailboxStore,
    Message, MessageEntry, MessageFlags, MessageId, MessageStore, NewMessage, RawMessage,
    Timestamp, Uid, UidValidity,
};
use sandesh_proto::CancellationToken;
use sandesh_smtp::{DeliveryService, SmtpServer};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Everything the fakes remember, so assertions can inspect it.
#[derive(Debug, Default)]
struct Recorded {
    accounts: Vec<Account>,
    identities: HashMap<String, AccountId>,
    mailboxes: Vec<Mailbox>,
    messages: Vec<(MessageId, Vec<u8>)>,
    entries: Vec<MessageEntry>,
    deliveries: Vec<Delivery>,
}

/// An in-memory implementation of every port SMTP touches.
#[derive(Debug, Default, Clone)]
struct FakeStore {
    state: Arc<Mutex<Recorded>>,
}

impl FakeStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, Recorded> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers an account with one address and an INBOX.
    fn add_account(&self, username: &str, address: &str) -> AccountId {
        let id = AccountId::new();
        let mut state = self.lock();
        state.accounts.push(Account {
            id,
            username: username.to_owned(),
            credential: None,
            is_catch_all: false,
            created_at: Timestamp::now(),
        });
        state
            .identities
            .insert(EmailAddress::parse_lenient(address).normalized(), id);
        state.mailboxes.push(Mailbox {
            id: MailboxId::new(),
            account_id: id,
            name: MailboxName::new("INBOX").expect("valid"),
            parent_id: None,
            uid_validity: UidValidity::new(1),
            uid_next: Uid::MIN,
            attributes: sandesh_core::MailboxAttributes::none(),
            subscribed: true,
            created_at: Timestamp::now(),
            highest_mod_seq: sandesh_core::ModSeq::MIN,
        });
        id
    }
}

#[async_trait]
impl AccountStore for FakeStore {
    async fn create(&self, _account: &Account) -> CoreResult<()> {
        Ok(())
    }
    async fn get(&self, id: AccountId) -> CoreResult<Option<Account>> {
        Ok(self.lock().accounts.iter().find(|a| a.id == id).cloned())
    }
    async fn get_by_username(&self, username: &str) -> CoreResult<Option<Account>> {
        Ok(self
            .lock()
            .accounts
            .iter()
            .find(|a| a.username == username)
            .cloned())
    }
    async fn list(&self) -> CoreResult<Vec<Account>> {
        Ok(self.lock().accounts.clone())
    }
    async fn delete(&self, _id: AccountId) -> CoreResult<bool> {
        Ok(false)
    }
    async fn add_identity(
        &self,
        account_id: AccountId,
        address: &EmailAddress,
        _is_primary: bool,
    ) -> CoreResult<()> {
        self.lock()
            .identities
            .insert(address.normalized(), account_id);
        Ok(())
    }
    async fn remove_identity(
        &self,
        _account_id: AccountId,
        address: &EmailAddress,
    ) -> CoreResult<bool> {
        let removed = self
            .lock()
            .identities
            .remove(&address.normalized())
            .is_some();
        Ok(removed)
    }
    async fn identities(&self, account_id: AccountId) -> CoreResult<Vec<EmailIdentity>> {
        Ok(self
            .lock()
            .identities
            .iter()
            .filter(|(_, id)| **id == account_id)
            .map(|(address, id)| EmailIdentity {
                id: IdentityId::new(),
                account_id: *id,
                address: EmailAddress::parse_lenient(address),
                is_primary: true,
            })
            .collect())
    }
    async fn resolve_address(&self, address: &EmailAddress) -> CoreResult<Option<AccountId>> {
        Ok(self.lock().identities.get(&address.normalized()).copied())
    }
    async fn catch_all(&self) -> CoreResult<Option<Account>> {
        Ok(None)
    }
    async fn update_credential(
        &self,
        _id: AccountId,
        _credential: Option<&sandesh_core::Credential>,
    ) -> CoreResult<bool> {
        Ok(true)
    }
    async fn set_catch_all(&self, _id: AccountId, _is_catch_all: bool) -> CoreResult<bool> {
        Ok(true)
    }
}

#[async_trait]
impl MailboxStore for FakeStore {
    async fn create(&self, _mailbox: &Mailbox) -> CoreResult<()> {
        Ok(())
    }
    async fn get(&self, id: MailboxId) -> CoreResult<Option<Mailbox>> {
        Ok(self.lock().mailboxes.iter().find(|m| m.id == id).cloned())
    }
    async fn get_by_name(
        &self,
        account_id: AccountId,
        name: &MailboxName,
    ) -> CoreResult<Option<Mailbox>> {
        Ok(self
            .lock()
            .mailboxes
            .iter()
            .find(|m| m.account_id == account_id && &m.name == name)
            .cloned())
    }
    async fn list(&self, account_id: AccountId) -> CoreResult<Vec<Mailbox>> {
        Ok(self
            .lock()
            .mailboxes
            .iter()
            .filter(|m| m.account_id == account_id)
            .cloned()
            .collect())
    }
    async fn rename(&self, _id: MailboxId, _new_name: &MailboxName) -> CoreResult<()> {
        Ok(())
    }
    async fn set_subscribed(&self, _id: MailboxId, _subscribed: bool) -> CoreResult<()> {
        Ok(())
    }
    async fn delete(&self, _id: MailboxId) -> CoreResult<bool> {
        Ok(false)
    }
    async fn counts(&self, id: MailboxId) -> CoreResult<MailboxCounts> {
        let state = self.lock();
        Ok(MailboxCounts {
            total: state.entries.iter().filter(|e| e.mailbox_id == id).count() as u64,
            unseen: 0,
            uid_next: 1,
            highest_mod_seq: sandesh_core::ModSeq::MIN,
        })
    }
}

#[async_trait]
impl MessageStore for FakeStore {
    async fn insert(&self, message: &NewMessage) -> CoreResult<InsertedMessage> {
        let mut state = self.lock();
        let message_id = MessageId::new();
        state
            .messages
            .push((message_id, message.raw.bytes().to_vec()));

        let mut entries = Vec::new();
        for (index, placement) in message.placements.iter().enumerate() {
            let entry = MessageEntry {
                id: EntryId::new(),
                message_id,
                mailbox_id: placement.mailbox_id,
                uid: Uid::new(index as u32 + 1).unwrap_or(Uid::MIN),
                flags: MessageFlags::empty(),
                internal_date: placement.internal_date,
                added_at: placement.internal_date,
                mod_seq: sandesh_core::ModSeq::MIN,
            };
            state.entries.push(entry.clone());
            entries.push(entry);
        }

        Ok(InsertedMessage {
            message_id,
            entries,
        })
    }
    async fn get(&self, _id: MessageId) -> CoreResult<Option<Message>> {
        Ok(None)
    }
    async fn raw(&self, id: MessageId) -> CoreResult<Option<RawMessage>> {
        Ok(self
            .lock()
            .messages
            .iter()
            .find(|(m, _)| *m == id)
            .map(|(_, bytes)| RawMessage::new(bytes.clone())))
    }
    async fn get_entry(&self, _id: EntryId) -> CoreResult<Option<MessageEntry>> {
        Ok(None)
    }
    async fn get_entry_by_uid(
        &self,
        _mailbox_id: MailboxId,
        _uid: Uid,
    ) -> CoreResult<Option<MessageEntry>> {
        Ok(None)
    }
    async fn list_entries(&self, mailbox_id: MailboxId) -> CoreResult<Vec<MessageEntry>> {
        Ok(self
            .lock()
            .entries
            .iter()
            .filter(|e| e.mailbox_id == mailbox_id)
            .cloned()
            .collect())
    }
    async fn entries_for_message(&self, message_id: MessageId) -> CoreResult<Vec<MessageEntry>> {
        Ok(self
            .lock()
            .entries
            .iter()
            .filter(|e| e.message_id == message_id)
            .cloned()
            .collect())
    }
    async fn copy_entry(&self, _entry_id: EntryId, _target: MailboxId) -> CoreResult<MessageEntry> {
        Err(CoreError::not_found(
            EntityKind::Entry,
            "unsupported in fake",
        ))
    }
    async fn move_entry(&self, _entry_id: EntryId, _target: MailboxId) -> CoreResult<MessageEntry> {
        Err(CoreError::not_found(
            EntityKind::Entry,
            "unsupported in fake",
        ))
    }
    async fn update_flags(
        &self,
        _entry_id: EntryId,
        _update: FlagUpdate,
    ) -> CoreResult<MessageFlags> {
        Ok(MessageFlags::empty())
    }
    async fn update_flags_conditional(
        &self,
        _entry_id: EntryId,
        _update: FlagUpdate,
        _unchanged_since: Option<sandesh_core::ModSeq>,
    ) -> CoreResult<sandesh_core::FlagOutcome> {
        // This fake exists to prove SMTP delivery touches no IMAP flag state;
        // it never stores flags, so the honest answer is "nothing changed".
        Ok(sandesh_core::FlagOutcome {
            flags: MessageFlags::empty(),
            mod_seq: sandesh_core::ModSeq::MIN,
            status: sandesh_core::FlagChangeStatus::Unchanged,
        })
    }
    async fn delete_entry(&self, _entry_id: EntryId) -> CoreResult<bool> {
        Ok(false)
    }
    async fn collect_orphans(&self) -> CoreResult<Vec<MessageId>> {
        Ok(Vec::new())
    }
    async fn attachments(&self, _message_id: MessageId) -> CoreResult<Vec<Attachment>> {
        Ok(Vec::new())
    }
    async fn get_attachment(
        &self,
        _id: AttachmentId,
    ) -> CoreResult<Option<(Attachment, MessageId)>> {
        Ok(None)
    }
}

#[async_trait]
impl DeliveryStore for FakeStore {
    async fn record(&self, delivery: &Delivery) -> CoreResult<()> {
        self.lock().deliveries.push(delivery.clone());
        Ok(())
    }
    async fn get(&self, id: DeliveryId) -> CoreResult<Option<Delivery>> {
        Ok(self.lock().deliveries.iter().find(|d| d.id == id).cloned())
    }
    async fn by_message_id(&self, message_id: MessageId) -> CoreResult<Option<Delivery>> {
        // Newest last in this Vec, so search from the back to match the
        // SQLite store's "most recent record wins".
        Ok(self
            .lock()
            .deliveries
            .iter()
            .rev()
            .find(|d| d.message_id == Some(message_id))
            .cloned())
    }
    async fn recent(&self, _limit: u32) -> CoreResult<Vec<Delivery>> {
        Ok(self.lock().deliveries.clone())
    }
}

#[tokio::test]
async fn smtp_delivers_against_a_storage_implementation_that_is_not_sqlite() {
    // If this passes, SQLite can be replaced without touching the SMTP crate.
    let store = FakeStore::default();
    let bob = store.add_account("bob", "bob@example.test");

    let backing: Arc<FakeStore> = Arc::new(store.clone());
    let delivery = Arc::new(DeliveryService::new(
        backing.clone(),
        backing.clone(),
        backing.clone(),
        backing.clone(),
        sandesh_core::UnknownRecipient::CatchAll,
    ));

    let config = SmtpConfig {
        bind: "127.0.0.1:0".parse().expect("valid address"),
        hostname: String::from("fake.test"),
        ..SmtpConfig::default()
    };

    let (bound, _warnings) = SmtpServer::new(config, delivery)
        .bind()
        .await
        .expect("bind");
    let addr = bound.local_addr();
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = bound.serve(server_shutdown).await;
    });

    // A full transaction over TCP.
    let stream = TcpStream::connect(addr).await.expect("connect");
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    async fn reply(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> String {
        let mut out = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            out.push_str(&line);
            if line.as_bytes().get(3) != Some(&b'-') {
                break;
            }
        }
        out
    }

    assert!(reply(&mut reader).await.starts_with("220"));

    let content = b"Subject: no database here\r\n\r\nbody\r\n";
    for command in [
        "EHLO fake-client\r\n",
        "MAIL FROM:<alice@example.test>\r\n",
        "RCPT TO:<bob@example.test>\r\n",
        "DATA\r\n",
    ] {
        write.write_all(command.as_bytes()).await.expect("write");
        let response = reply(&mut reader).await;
        assert!(
            response.starts_with("250") || response.starts_with("354"),
            "{command:?} → {response}"
        );
    }

    write.write_all(content).await.expect("write content");
    write.write_all(b".\r\n").await.expect("write terminator");
    let accepted = reply(&mut reader).await;
    assert!(accepted.starts_with("250"), "delivery: {accepted}");

    shutdown.cancel();

    // The message reached the fake store, byte-exact.
    let state = store.lock();
    assert_eq!(state.messages.len(), 1, "one message stored");
    assert_eq!(
        state.messages[0].1,
        content.to_vec(),
        "raw fidelity holds regardless of the storage implementation"
    );

    assert_eq!(state.entries.len(), 1, "one placement");
    let inbox = state
        .mailboxes
        .iter()
        .find(|m| m.account_id == bob)
        .expect("INBOX");
    assert_eq!(state.entries[0].mailbox_id, inbox.id);

    assert_eq!(state.deliveries.len(), 1, "the delivery was recorded");
    assert_eq!(state.deliveries[0].envelope.rcpt_to.len(), 1);
}

#[tokio::test]
async fn a_session_can_be_driven_without_any_socket() {
    // The session is generic over its transport, so protocol behaviour is
    // testable with no networking at all — which is what will make the IMAP and
    // POP3 state machines cheap to test in later phases.
    use sandesh_proto::Connection;
    use tokio::io::duplex;

    let store = FakeStore::default();
    store.add_account("bob", "bob@example.test");
    let backing: Arc<FakeStore> = Arc::new(store.clone());

    let delivery = Arc::new(DeliveryService::new(
        backing.clone(),
        backing.clone(),
        backing.clone(),
        backing.clone(),
        sandesh_core::UnknownRecipient::CatchAll,
    ));

    let (mut client, server) = duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server);
    let connection = Connection::new(server_read, server_write, 4096, None);
    let session = sandesh_smtp::Session::new(connection, Arc::new(SmtpConfig::default()), delivery);

    let driver = tokio::spawn(async move { session.run().await });

    let script = b"EHLO in-memory\r\n\
MAIL FROM:<alice@example.test>\r\n\
RCPT TO:<bob@example.test>\r\n\
DATA\r\n\
Subject: no socket\r\n\
\r\n\
body\r\n\
.\r\n\
QUIT\r\n";
    client.write_all(script).await.expect("write script");

    let mut transcript = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut client, &mut transcript)
        .await
        .expect("read transcript");
    driver.await.expect("session task").expect("session ran");

    let text = String::from_utf8_lossy(&transcript);
    assert!(text.contains("220"), "greeting: {text}");
    assert!(text.contains("354"), "DATA invitation: {text}");
    assert!(text.contains("221"), "QUIT: {text}");

    let state = store.lock();
    assert_eq!(state.messages.len(), 1);
    assert_eq!(state.messages[0].1, b"Subject: no socket\r\n\r\nbody\r\n");
}
