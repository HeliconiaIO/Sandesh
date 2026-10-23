//! Placement operations: where a message sits, and what moving it means.
//!
//! A [`MessageEntry`] is the association between a message and a mailbox, and
//! the operations over it — copy, move, discard — carry rules that hold
//! whoever is asking. Those rules were previously written separately in the
//! IMAP session and in the HTTP handlers, and the copies disagreed: HTTP would
//! place mail into another account's mailbox when the destination was named by
//! id, and would copy an entry into its own mailbox and surface the resulting
//! index violation as a `500` (ADR-067, ADR-069).
//!
//! ## What lives here, and what does not
//!
//! Here: that a destination must belong to the same account as the source, that
//! it must be able to hold messages, that it may not be the mailbox the entry
//! already occupies, and what "discard" means when a Trash mailbox exists.
//!
//! Not here, per `CORE_SPEC.md` §9: IMAP's `[COPYUID]` reporting and sequence
//! renumbering, POP3's session-scoped `DELE` staging, and the response codes
//! each protocol answers with. The planners in `sandesh-imap` (`copy.rs`,
//! `move_entry.rs`, `expunge.rs`) hold no ports and stay where they are.

use std::sync::Arc;

use crate::{
    AccountId, CoreError, CoreResult, EntityKind, EntryId, Mailbox, MailboxId, MailboxName,
    MailboxStore, MessageEntry, MessageStore,
};

use super::MailboxService;

/// The mailbox a discarded placement is moved to, when one exists.
///
/// Named rather than special-use-matched because the mailbox provisioned with
/// every account carries this name (`DEFAULT_MAILBOXES`), and a client is free
/// to have renamed or removed it — in which case discarding falls back to
/// removing the placement outright.
const TRASH: &str = "Trash";

/// Placement operations shared by every protocol.
pub struct MessageService {
    messages: Arc<dyn MessageStore>,
    mailboxes: Arc<dyn MailboxStore>,
}

impl std::fmt::Debug for MessageService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageService").finish_non_exhaustive()
    }
}

/// What [`MessageService::discard`] did with a placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discarded {
    /// Moved to the Trash mailbox, and can still be recovered.
    ToTrash(MailboxId),
    /// Removed outright: it was already in Trash, or there is no Trash.
    Removed,
}

impl MessageService {
    /// Builds the service over the placement and mailbox ports.
    #[must_use]
    pub fn new(messages: Arc<dyn MessageStore>, mailboxes: Arc<dyn MailboxStore>) -> Self {
        Self {
            messages,
            mailboxes,
        }
    }

    /// The placement with this id.
    ///
    /// # Errors
    /// [`CoreError::NotFound`] when no such placement exists, and
    /// [`CoreError::Storage`] when the store could not answer.
    pub async fn entry(&self, entry_id: EntryId) -> CoreResult<MessageEntry> {
        self.messages
            .get_entry(entry_id)
            .await?
            .ok_or_else(|| CoreError::not_found(EntityKind::Entry, entry_id))
    }

    /// Checks that `target` is a usable destination for `entry`.
    ///
    /// The three rules, in the order a caller would want them reported:
    ///
    /// 1. The target must exist and be able to hold messages — a `\Noselect`
    ///    name holds children, not mail.
    /// 2. It must belong to the **same account** as the source. Nothing below
    ///    this layer enforces it: `copy_entry` and `move_entry` read the source
    ///    placement and allocate a UID on whatever target they are given, and
    ///    no schema constraint relates the two accounts. IMAP cannot express
    ///    the mistake because its destinations are names resolved against the
    ///    authenticated session's account; over HTTP the target is a request
    ///    parameter, so it has to be checked rather than trusted (ADR-067).
    /// 3. It must not be the mailbox the entry already occupies. Left to the
    ///    placement uniqueness index this would arrive as a storage failure,
    ///    which is the wrong answer to a client mistake.
    ///
    /// # Errors
    /// [`CoreError::NotFound`] when the target does not exist **or** belongs to
    /// another account — deliberately the same answer, so this cannot be used
    /// to discover which mailbox ids exist elsewhere; [`CoreError::Conflict`]
    /// when the target cannot hold mail or is already the entry's mailbox.
    async fn usable_destination(
        &self,
        entry: &MessageEntry,
        target: MailboxId,
    ) -> CoreResult<Mailbox> {
        let source = self
            .mailboxes
            .get(entry.mailbox_id)
            .await?
            .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, entry.mailbox_id))?;

        let destination = self
            .mailboxes
            .get(target)
            .await?
            .filter(|mailbox| mailbox.account_id == source.account_id)
            .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, target))?;

        if destination.attributes.is_noselect() {
            return Err(CoreError::Conflict {
                reason: format!("mailbox {} cannot hold messages", destination.name.as_str()),
            });
        }

        if destination.id == entry.mailbox_id {
            return Err(CoreError::Conflict {
                reason: String::from("the placement is already in that mailbox"),
            });
        }

        Ok(destination)
    }

    /// Checks a destination named by id, without performing any placement.
    ///
    /// Exposed so a caller can validate before acting — the HTTP handlers need
    /// the target id to pass to their own store call, and must not discover an
    /// unusable destination only after a write.
    ///
    /// # Errors
    /// As [`usable_destination`](Self::usable_destination).
    pub async fn resolve_destination_by_id(
        &self,
        entry: &MessageEntry,
        target: MailboxId,
    ) -> CoreResult<Mailbox> {
        self.usable_destination(entry, target).await
    }

    /// Resolves a destination named within the source placement's account.
    ///
    /// The name form cannot escape the account by construction, but it goes
    /// through the same checks so that both forms answer identically.
    ///
    /// # Errors
    /// As [`usable_destination`](Self::usable_destination).
    pub async fn resolve_destination_named(
        &self,
        entry: &MessageEntry,
        name: &MailboxName,
    ) -> CoreResult<Mailbox> {
        let source = self
            .mailboxes
            .get(entry.mailbox_id)
            .await?
            .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, entry.mailbox_id))?;

        let target = MailboxService::new(Arc::clone(&self.mailboxes))
            .resolve(source.account_id, name)
            .await?;

        self.usable_destination(entry, target.id).await
    }

    /// Copies a placement into another mailbox (`CORE_SPEC` §6).
    ///
    /// The message itself is shared, never duplicated; only a new placement
    /// with a new UID appears. Every check runs before anything is written, so
    /// a copy that is going to fail creates nothing.
    ///
    /// # Errors
    /// As [`usable_destination`](Self::usable_destination), plus
    /// [`CoreError::Storage`] if the copy itself fails.
    pub async fn copy(&self, entry_id: EntryId, target: MailboxId) -> CoreResult<MessageEntry> {
        let entry = self.entry(entry_id).await?;
        let destination = self.usable_destination(&entry, target).await?;
        self.messages.copy_entry(entry_id, destination.id).await
    }

    /// Moves a placement to another mailbox (`CORE_SPEC` §6).
    ///
    /// A new UID in the target; the source placement is removed.
    ///
    /// # Errors
    /// As [`copy`](Self::copy).
    pub async fn move_to(&self, entry_id: EntryId, target: MailboxId) -> CoreResult<MessageEntry> {
        let entry = self.entry(entry_id).await?;
        let destination = self.usable_destination(&entry, target).await?;
        self.messages.move_entry(entry_id, destination.id).await
    }

    /// Discards a placement: to Trash if there is one, otherwise removed.
    ///
    /// A placement already in Trash is removed rather than moved into itself,
    /// which is what makes a second delete permanent.
    ///
    /// This policy was written out three times in the HTTP handlers, and one
    /// copy fell back to a freshly generated [`AccountId`] when the source
    /// mailbox could not be read — an id that by construction owns no Trash, so
    /// the lookup could never succeed (ADR-069). Resolving Trash from the
    /// *source placement's own account* is the only correct reading, and it is
    /// the only one available here.
    ///
    /// # Errors
    /// [`CoreError::NotFound`] when the placement or its mailbox does not
    /// exist, and [`CoreError::Storage`] on a store failure.
    pub async fn discard(&self, entry_id: EntryId) -> CoreResult<Discarded> {
        let entry = self.entry(entry_id).await?;
        let mailbox = self
            .mailboxes
            .get(entry.mailbox_id)
            .await?
            .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, entry.mailbox_id))?;

        if mailbox.name.as_str().eq_ignore_ascii_case(TRASH) {
            self.remove(entry_id).await?;
            return Ok(Discarded::Removed);
        }

        let trash_name = MailboxName::new(TRASH)
            .map_err(|e| CoreError::invariant(format!("Trash is not a valid name: {e}")))?;

        // The account is the source placement's own, never a guess.
        if let Some(trash) = self
            .mailboxes
            .get_by_name(mailbox.account_id, &trash_name)
            .await?
        {
            self.messages.move_entry(entry_id, trash.id).await?;
            Ok(Discarded::ToTrash(trash.id))
        } else {
            // No Trash mailbox: a client may have renamed or removed it.
            self.remove(entry_id).await?;
            Ok(Discarded::Removed)
        }
    }

    /// Removes a placement outright. The message survives if others hold it.
    ///
    /// # Errors
    /// [`CoreError::NotFound`] when there was no such placement to remove, and
    /// [`CoreError::Storage`] on a store failure.
    pub async fn remove(&self, entry_id: EntryId) -> CoreResult<()> {
        if self.messages.delete_entry(entry_id).await? {
            Ok(())
        } else {
            Err(CoreError::not_found(EntityKind::Entry, entry_id))
        }
    }

    /// The account owning a placement, via its mailbox.
    ///
    /// # Errors
    /// [`CoreError::NotFound`] when the placement or its mailbox is missing.
    pub async fn account_of(&self, entry: &MessageEntry) -> CoreResult<AccountId> {
        self.mailboxes
            .get(entry.mailbox_id)
            .await?
            .map(|mailbox| mailbox.account_id)
            .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, entry.mailbox_id))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::{
        MailboxAttributes, MailboxCounts, MessageFlags, MessageId, ModSeq, Timestamp, Uid,
        UidValidity,
    };

    /// Drives a future to completion without an async runtime.
    ///
    /// The core declares no runtime and must not gain one even as a
    /// dev-dependency — `tests/architecture.rs` pins the declared dependency
    /// list, dev included. See the same helper in `service/mailbox.rs`.
    fn block_on<F: Future>(future: F) -> F::Output {
        use std::pin::pin;
        use std::task::{Context, Poll, Waker};

        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = pin!(future);
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
        }
    }

    #[derive(Default)]
    struct Fixture {
        mailboxes: Mutex<HashMap<MailboxId, Mailbox>>,
        entries: Mutex<HashMap<EntryId, MessageEntry>>,
    }

    impl std::fmt::Debug for Fixture {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Fixture").finish_non_exhaustive()
        }
    }

    impl Fixture {
        fn mailbox(&self, account: AccountId, name: &str) -> MailboxId {
            let mailbox = Mailbox {
                id: MailboxId::new(),
                account_id: account,
                parent_id: None,
                name: MailboxName::new(name).expect("a valid name"),
                uid_validity: UidValidity::next_epoch(),
                uid_next: Uid::MIN,
                attributes: MailboxAttributes::none(),
                subscribed: false,
                created_at: Timestamp::now(),
                highest_mod_seq: ModSeq::MIN,
            };
            let id = mailbox.id;
            self.mailboxes
                .lock()
                .expect("not poisoned")
                .insert(id, mailbox);
            id
        }

        fn noselect(&self, account: AccountId, name: &str) -> MailboxId {
            let id = self.mailbox(account, name);
            if let Some(mailbox) = self.mailboxes.lock().expect("not poisoned").get_mut(&id) {
                mailbox.attributes =
                    MailboxAttributes::from_names(vec![String::from("\\Noselect")]);
            }
            id
        }

        fn place(&self, mailbox: MailboxId) -> EntryId {
            let entry = MessageEntry {
                id: EntryId::new(),
                message_id: MessageId::new(),
                mailbox_id: mailbox,
                uid: Uid::MIN,
                flags: MessageFlags::empty(),
                internal_date: Timestamp::now(),
                added_at: Timestamp::now(),
                mod_seq: ModSeq::MIN,
            };
            let id = entry.id;
            self.entries.lock().expect("not poisoned").insert(id, entry);
            id
        }

        fn entries_in(&self, mailbox: MailboxId) -> usize {
            self.entries
                .lock()
                .expect("not poisoned")
                .values()
                .filter(|e| e.mailbox_id == mailbox)
                .count()
        }
    }

    #[async_trait::async_trait]
    impl MailboxStore for Fixture {
        async fn create(&self, mailbox: &Mailbox) -> CoreResult<()> {
            self.mailboxes
                .lock()
                .expect("not poisoned")
                .insert(mailbox.id, mailbox.clone());
            Ok(())
        }
        async fn get(&self, id: MailboxId) -> CoreResult<Option<Mailbox>> {
            Ok(self
                .mailboxes
                .lock()
                .expect("not poisoned")
                .get(&id)
                .cloned())
        }
        async fn get_by_name(
            &self,
            account_id: AccountId,
            name: &MailboxName,
        ) -> CoreResult<Option<Mailbox>> {
            Ok(self
                .mailboxes
                .lock()
                .expect("not poisoned")
                .values()
                .find(|m| m.account_id == account_id && m.name.as_str() == name.as_str())
                .cloned())
        }
        async fn list(&self, account_id: AccountId) -> CoreResult<Vec<Mailbox>> {
            Ok(self
                .mailboxes
                .lock()
                .expect("not poisoned")
                .values()
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
        async fn delete(&self, id: MailboxId) -> CoreResult<bool> {
            Ok(self
                .mailboxes
                .lock()
                .expect("not poisoned")
                .remove(&id)
                .is_some())
        }
        async fn counts(&self, _id: MailboxId) -> CoreResult<MailboxCounts> {
            Ok(MailboxCounts {
                total: 0,
                unseen: 0,
                uid_next: 1,
                highest_mod_seq: ModSeq::MIN,
            })
        }
    }

    #[async_trait::async_trait]
    impl MessageStore for Fixture {
        async fn insert(&self, _message: &crate::NewMessage) -> CoreResult<crate::InsertedMessage> {
            Err(CoreError::invariant("not needed by these tests"))
        }
        async fn get(&self, _id: MessageId) -> CoreResult<Option<crate::Message>> {
            Ok(None)
        }
        async fn raw(&self, _id: MessageId) -> CoreResult<Option<crate::RawMessage>> {
            Ok(None)
        }
        async fn get_entry(&self, id: EntryId) -> CoreResult<Option<MessageEntry>> {
            Ok(self.entries.lock().expect("not poisoned").get(&id).cloned())
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
                .entries
                .lock()
                .expect("not poisoned")
                .values()
                .filter(|e| e.mailbox_id == mailbox_id)
                .cloned()
                .collect())
        }
        async fn entries_for_message(
            &self,
            message_id: MessageId,
        ) -> CoreResult<Vec<MessageEntry>> {
            Ok(self
                .entries
                .lock()
                .expect("not poisoned")
                .values()
                .filter(|e| e.message_id == message_id)
                .cloned()
                .collect())
        }
        async fn copy_entry(
            &self,
            entry_id: EntryId,
            target_mailbox: MailboxId,
        ) -> CoreResult<MessageEntry> {
            let mut guard = self.entries.lock().expect("not poisoned");
            let source = guard
                .get(&entry_id)
                .cloned()
                .ok_or_else(|| CoreError::not_found(EntityKind::Entry, entry_id))?;
            let copy = MessageEntry {
                id: EntryId::new(),
                mailbox_id: target_mailbox,
                ..source
            };
            guard.insert(copy.id, copy.clone());
            Ok(copy)
        }
        async fn move_entry(
            &self,
            entry_id: EntryId,
            target_mailbox: MailboxId,
        ) -> CoreResult<MessageEntry> {
            let mut guard = self.entries.lock().expect("not poisoned");
            let mut entry = guard
                .get(&entry_id)
                .cloned()
                .ok_or_else(|| CoreError::not_found(EntityKind::Entry, entry_id))?;
            entry.mailbox_id = target_mailbox;
            guard.insert(entry.id, entry.clone());
            Ok(entry)
        }
        async fn update_flags(
            &self,
            _entry_id: EntryId,
            _update: crate::FlagUpdate,
        ) -> CoreResult<MessageFlags> {
            Ok(MessageFlags::empty())
        }
        async fn update_flags_conditional(
            &self,
            _entry_id: EntryId,
            _update: crate::FlagUpdate,
            _unchanged_since: Option<ModSeq>,
        ) -> CoreResult<crate::FlagOutcome> {
            Err(CoreError::invariant("not needed by these tests"))
        }
        async fn delete_entry(&self, entry_id: EntryId) -> CoreResult<bool> {
            Ok(self
                .entries
                .lock()
                .expect("not poisoned")
                .remove(&entry_id)
                .is_some())
        }
        async fn collect_orphans(&self) -> CoreResult<Vec<MessageId>> {
            Ok(Vec::new())
        }
        async fn attachments(&self, _message_id: MessageId) -> CoreResult<Vec<crate::Attachment>> {
            Ok(Vec::new())
        }
        async fn get_attachment(
            &self,
            _id: crate::AttachmentId,
        ) -> CoreResult<Option<(crate::Attachment, MessageId)>> {
            Ok(None)
        }
    }

    fn service(fixture: &Arc<Fixture>) -> MessageService {
        MessageService::new(
            Arc::clone(fixture) as Arc<dyn MessageStore>,
            Arc::clone(fixture) as Arc<dyn MailboxStore>,
        )
    }

    #[test]
    fn copying_into_another_accounts_mailbox_is_refused() {
        // The isolation hole ADR-067 closed at the HTTP boundary, now enforced
        // in the domain so every surface inherits it.
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let alice = AccountId::new();
            let bob = AccountId::new();
            let alice_inbox = fixture.mailbox(alice, "INBOX");
            let bob_inbox = fixture.mailbox(bob, "INBOX");
            let entry = fixture.place(alice_inbox);

            let error = service(&fixture)
                .copy(entry, bob_inbox)
                .await
                .expect_err("a cross-account copy is refused");

            assert!(matches!(error, CoreError::NotFound { .. }), "{error}");
            assert_eq!(
                fixture.entries_in(bob_inbox),
                0,
                "nothing may be placed in the other account"
            );
        });
    }

    #[test]
    fn moving_into_another_accounts_mailbox_is_refused_and_leaves_the_source() {
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let alice = AccountId::new();
            let bob = AccountId::new();
            let alice_inbox = fixture.mailbox(alice, "INBOX");
            let bob_inbox = fixture.mailbox(bob, "INBOX");
            let entry = fixture.place(alice_inbox);

            service(&fixture)
                .move_to(entry, bob_inbox)
                .await
                .expect_err("a cross-account move is refused");

            assert_eq!(fixture.entries_in(bob_inbox), 0);
            assert_eq!(
                fixture.entries_in(alice_inbox),
                1,
                "a refused move must not lose the mail"
            );
        });
    }

    #[test]
    fn a_copy_within_one_account_succeeds() {
        // The guard must refuse the other account, not every destination.
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let alice = AccountId::new();
            let inbox = fixture.mailbox(alice, "INBOX");
            let archive = fixture.mailbox(alice, "Archive");
            let entry = fixture.place(inbox);

            service(&fixture)
                .copy(entry, archive)
                .await
                .expect("a copy inside one account succeeds");

            assert_eq!(fixture.entries_in(archive), 1);
            assert_eq!(fixture.entries_in(inbox), 1, "a copy leaves the original");
        });
    }

    #[test]
    fn copying_into_the_source_mailbox_is_a_conflict() {
        // Left to the placement uniqueness index this would be a storage
        // failure, i.e. a 500 for a client mistake.
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let inbox = fixture.mailbox(AccountId::new(), "INBOX");
            let entry = fixture.place(inbox);

            let error = service(&fixture)
                .copy(entry, inbox)
                .await
                .expect_err("copying into its own mailbox is refused");
            assert!(matches!(error, CoreError::Conflict { .. }), "{error}");
        });
    }

    #[test]
    fn a_noselect_mailbox_cannot_receive_a_placement() {
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let account = AccountId::new();
            let inbox = fixture.mailbox(account, "INBOX");
            let holder = fixture.noselect(account, "Parent");
            let entry = fixture.place(inbox);

            let error = service(&fixture)
                .copy(entry, holder)
                .await
                .expect_err("a \\Noselect mailbox holds children, not mail");
            assert!(matches!(error, CoreError::Conflict { .. }), "{error}");
            assert_eq!(fixture.entries_in(holder), 0);
        });
    }

    #[test]
    fn discarding_moves_to_trash_in_the_placements_own_account() {
        // The bug this replaced resolved Trash from a *freshly generated*
        // AccountId when the source mailbox could not be read — an id that owns
        // no Trash, so the move could never happen (ADR-069). Two accounts each
        // with a Trash is what would catch resolving the wrong one.
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let alice = AccountId::new();
            let bob = AccountId::new();
            let alice_inbox = fixture.mailbox(alice, "INBOX");
            let alice_trash = fixture.mailbox(alice, "Trash");
            let bob_trash = fixture.mailbox(bob, "Trash");
            let entry = fixture.place(alice_inbox);

            let outcome = service(&fixture)
                .discard(entry)
                .await
                .expect("the placement is discarded");

            assert_eq!(outcome, Discarded::ToTrash(alice_trash));
            assert_eq!(fixture.entries_in(alice_trash), 1);
            assert_eq!(
                fixture.entries_in(bob_trash),
                0,
                "it must never land in another account's Trash"
            );
        });
    }

    #[test]
    fn discarding_from_trash_removes_the_placement() {
        // What makes a second delete permanent, rather than a move into itself.
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let account = AccountId::new();
            let trash = fixture.mailbox(account, "Trash");
            let entry = fixture.place(trash);

            let outcome = service(&fixture)
                .discard(entry)
                .await
                .expect("the placement is removed");

            assert_eq!(outcome, Discarded::Removed);
            assert_eq!(fixture.entries_in(trash), 0);
        });
    }

    #[test]
    fn discarding_without_a_trash_mailbox_removes_the_placement() {
        // A client may have renamed or deleted Trash; discarding must still do
        // something defined rather than failing.
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let inbox = fixture.mailbox(AccountId::new(), "INBOX");
            let entry = fixture.place(inbox);

            let outcome = service(&fixture)
                .discard(entry)
                .await
                .expect("the placement is removed");

            assert_eq!(outcome, Discarded::Removed);
            assert_eq!(fixture.entries_in(inbox), 0);
        });
    }

    #[test]
    fn an_unknown_destination_is_not_found() {
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let inbox = fixture.mailbox(AccountId::new(), "INBOX");
            let entry = fixture.place(inbox);

            let error = service(&fixture)
                .copy(entry, MailboxId::new())
                .await
                .expect_err("there is no such mailbox");
            assert!(matches!(error, CoreError::NotFound { .. }), "{error}");
        });
    }

    #[test]
    fn an_unknown_placement_is_not_found() {
        block_on(async {
            let fixture = Arc::new(Fixture::default());
            let error = service(&fixture)
                .discard(EntryId::new())
                .await
                .expect_err("there is no such placement");
            assert!(matches!(error, CoreError::NotFound { .. }), "{error}");
        });
    }
}
