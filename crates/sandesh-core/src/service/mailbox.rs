//! Mailbox operations, as `CORE_SPEC.md` §6 defines them.
//!
//! Every protocol surface reaches mailboxes through this type, so
//! `CREATE`/`DELETE`/`RENAME` mean the same thing whether they arrive over IMAP
//! or over HTTP. They previously did not: the two paths were written
//! independently and disagreed about whether `INBOX` could be created, whether
//! a mailbox could be renamed *onto* `INBOX`, and whether a new mailbox starts
//! subscribed (ADR-068).
//!
//! ## What lives here, and what does not
//!
//! Here: the rules that hold regardless of who is asking — `INBOX` is not
//! creatable, deletable or renameable; a name may not be taken twice; a
//! `\Noselect` name holds children rather than messages, so nothing can be
//! placed in it.
//!
//! Not here, per `CORE_SPEC.md` §9: anything protocol-shaped. Modified UTF-7
//! decoding is a wire encoding and stays in the IMAP codec. So do the
//! `[TRYCREATE]` response code, dropping a failed `SELECT` back to
//! Authenticated, and fixing up a selected mailbox's name after a rename —
//! those are session state, not domain facts.
//!
//! ## Why the return type matters
//!
//! Every method returns [`CoreResult`], which keeps "this does not exist"
//! ([`CoreError::NotFound`]) distinct from "the store could not answer"
//! ([`CoreError::Storage`]). The call sites this replaces collapsed the two:
//! several matched `Ok(Some(_))` and treated every other case as absence, so a
//! database failure during a duplicate check read as *the name is free* and the
//! caller went on to create or rename over it. A service that cannot express
//! the difference is how that bug happened, so this one insists on it.

use std::sync::Arc;

use crate::{
    AccountId, CoreError, CoreResult, EntityKind, Mailbox, MailboxAttributes, MailboxId,
    MailboxName, MailboxStore, ModSeq, Timestamp, Uid, UidValidity,
};

/// Mailbox operations shared by every protocol.
pub struct MailboxService {
    mailboxes: Arc<dyn MailboxStore>,
}

impl std::fmt::Debug for MailboxService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MailboxService").finish_non_exhaustive()
    }
}

impl MailboxService {
    /// Builds the service over the mailbox port.
    #[must_use]
    pub fn new(mailboxes: Arc<dyn MailboxStore>) -> Self {
        Self { mailboxes }
    }

    /// A fresh mailbox in `account`, named `name`.
    ///
    /// The one constructor for a mailbox created by a user action. A new
    /// mailbox starts a new UID epoch and its first UID is 1 — never 0, which
    /// [`Uid`] cannot represent — and `HIGHESTMODSEQ` starts at its floor,
    /// since a mailbox that has never changed has nothing above 1 to report
    /// (RFC 7162 §3).
    ///
    /// `subscribed` is **false**: RFC 9051 treats subscription as a separate
    /// act, and a client that wants the mailbox in `LSUB` subscribes to it.
    /// This is the IMAP behaviour, now applied to both surfaces (ADR-068).
    fn new_mailbox(
        account: AccountId,
        name: MailboxName,
        attributes: MailboxAttributes,
    ) -> Mailbox {
        Mailbox {
            id: MailboxId::new(),
            account_id: account,
            parent_id: None,
            uid_validity: UidValidity::next_epoch(),
            uid_next: Uid::MIN,
            attributes,
            subscribed: false,
            created_at: Timestamp::now(),
            highest_mod_seq: ModSeq::MIN,
            name,
        }
    }

    /// The mailbox called `name` in `account`.
    ///
    /// # Errors
    /// [`CoreError::NotFound`] when no such mailbox exists, and
    /// [`CoreError::Storage`] when the store could not answer — the two are
    /// deliberately distinguishable.
    pub async fn resolve(&self, account: AccountId, name: &MailboxName) -> CoreResult<Mailbox> {
        self.mailboxes
            .get_by_name(account, name)
            .await?
            .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, name.as_str()))
    }

    /// The mailbox called `name`, if it can hold messages.
    ///
    /// A `\Noselect` name exists only to hold children, so selecting it or
    /// placing mail in it is refused. Used for IMAP `SELECT`/`EXAMINE` and for
    /// any `COPY`/`MOVE`/`APPEND` destination.
    ///
    /// # Errors
    /// As [`resolve`](Self::resolve), plus [`CoreError::Conflict`] when the
    /// mailbox exists but is not selectable.
    pub async fn resolve_selectable(
        &self,
        account: AccountId,
        name: &MailboxName,
    ) -> CoreResult<Mailbox> {
        let mailbox = self.resolve(account, name).await?;
        if mailbox.attributes.is_noselect() {
            return Err(CoreError::Conflict {
                reason: format!("mailbox {} is not selectable", name.as_str()),
            });
        }
        Ok(mailbox)
    }

    /// Whether a mailbox of this name exists in `account`.
    ///
    /// Separate from [`resolve`](Self::resolve) because absence is the
    /// *expected* answer here rather than a failure, while a store error must
    /// still propagate rather than being read as "no".
    ///
    /// # Errors
    /// [`CoreError::Storage`] if the store could not answer.
    pub async fn exists(&self, account: AccountId, name: &MailboxName) -> CoreResult<bool> {
        Ok(self.mailboxes.get_by_name(account, name).await?.is_some())
    }

    /// Creates a mailbox, with its parents (`CORE_SPEC` §6).
    ///
    /// Intermediate names are created by the store as part of the same
    /// operation, so `CREATE "a/b/c"` in an empty account yields `a`, `a/b`
    /// and `a/b/c`.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] for `INBOX`, which always exists and may
    /// not be created (RFC 9051 §6.3.4); [`CoreError::AlreadyExists`] when the
    /// name is taken; [`CoreError::Storage`] on a store failure — including one
    /// during the duplicate check, which is never treated as "the name is
    /// free".
    pub async fn create(
        &self,
        account: AccountId,
        name: &MailboxName,
        attributes: MailboxAttributes,
    ) -> CoreResult<Mailbox> {
        if name.is_inbox() {
            return Err(CoreError::InvalidArgument {
                field: String::from("name"),
                reason: String::from("INBOX always exists and cannot be created"),
            });
        }

        if self.exists(account, name).await? {
            return Err(CoreError::already_exists(
                EntityKind::Mailbox,
                name.as_str(),
            ));
        }

        let mailbox = Self::new_mailbox(account, name.clone(), attributes);
        self.mailboxes.create(&mailbox).await?;
        Ok(mailbox)
    }

    /// Deletes a mailbox and its entries (`CORE_SPEC` §6).
    ///
    /// Messages left unreferenced are collected later, not here.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] for `INBOX`, which may not be deleted
    /// (RFC 9051 §6.3.5); [`CoreError::NotFound`] when the mailbox does not
    /// exist; [`CoreError::Conflict`] when the store declines the delete.
    pub async fn delete(&self, account: AccountId, name: &MailboxName) -> CoreResult<MailboxId> {
        if name.is_inbox() {
            return Err(CoreError::InvalidArgument {
                field: String::from("name"),
                reason: String::from("INBOX cannot be deleted"),
            });
        }

        let mailbox = self.resolve(account, name).await?;
        if self.mailboxes.delete(mailbox.id).await? {
            Ok(mailbox.id)
        } else {
            Err(CoreError::Conflict {
                reason: format!("mailbox {} could not be deleted", name.as_str()),
            })
        }
    }

    /// Renames a mailbox; descendants follow (`CORE_SPEC` §6, RFC 9051).
    ///
    /// UIDs are preserved — it is the same mailbox under a new name — so a
    /// client's cached state stays valid.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] when either name is `INBOX`: RFC 9051
    /// §6.3.6 gives `INBOX` a special meaning here (create a new mailbox and
    /// move the contents) that this server does not implement, so it is refused
    /// rather than half-honoured. [`CoreError::NotFound`] when the source does
    /// not exist, [`CoreError::AlreadyExists`] when the target name is taken,
    /// and [`CoreError::Storage`] on a store failure.
    pub async fn rename(
        &self,
        account: AccountId,
        from: &MailboxName,
        to: &MailboxName,
    ) -> CoreResult<MailboxId> {
        if from.is_inbox() || to.is_inbox() {
            return Err(CoreError::InvalidArgument {
                field: String::from("name"),
                reason: String::from("INBOX cannot be renamed"),
            });
        }

        let mailbox = self.resolve(account, from).await?;
        if self.exists(account, to).await? {
            return Err(CoreError::already_exists(EntityKind::Mailbox, to.as_str()));
        }

        self.mailboxes.rename(mailbox.id, to).await?;
        Ok(mailbox.id)
    }

    /// Subscribes to or unsubscribes from a mailbox.
    ///
    /// # Errors
    /// [`CoreError::NotFound`] when the mailbox does not exist, and
    /// [`CoreError::Storage`] on a store failure.
    pub async fn set_subscribed(
        &self,
        account: AccountId,
        name: &MailboxName,
        subscribed: bool,
    ) -> CoreResult<()> {
        let mailbox = self.resolve(account, name).await?;
        self.mailboxes.set_subscribed(mailbox.id, subscribed).await
    }

    /// The account's `INBOX`.
    ///
    /// Every account is created with one (`CORE_SPEC` §4.4), so its absence is
    /// a broken invariant rather than an ordinary miss — which is why this
    /// reports [`CoreError::InvariantViolated`] and not `NotFound`.
    ///
    /// # Errors
    /// [`CoreError::InvariantViolated`] if the account has no `INBOX`, and
    /// [`CoreError::Storage`] on a store failure.
    pub async fn inbox_of(&self, account: AccountId) -> CoreResult<MailboxId> {
        let name = MailboxName::new(crate::INBOX)
            .map_err(|e| CoreError::invariant(format!("INBOX is not a valid name: {e}")))?;

        self.mailboxes
            .get_by_name(account, &name)
            .await?
            .map(|mailbox| mailbox.id)
            .ok_or_else(|| CoreError::invariant(format!("account {account} has no INBOX")))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::{MailboxCounts, StoreError};

    /// A mailbox store that can be told to fail.
    ///
    /// The failure mode is the point: the bug this service exists to prevent
    /// was a duplicate check that read a store error as "the name is free", and
    /// only a double that can fail on demand can assert it is gone. A real
    /// `SQLite` database cannot be asked to fail a single query.
    #[derive(Default)]
    struct FakeMailboxes {
        mailboxes: Mutex<HashMap<MailboxId, Mailbox>>,
        /// When set, every lookup reports a store failure.
        failing: bool,
        /// When set, `delete` reports "nothing deleted" rather than an error.
        refuse_delete: bool,
    }

    impl FakeMailboxes {
        fn with(mailboxes: Vec<Mailbox>) -> Self {
            let map = mailboxes.into_iter().map(|m| (m.id, m)).collect();
            Self {
                mailboxes: Mutex::new(map),
                ..Self::default()
            }
        }

        fn failing() -> Self {
            Self {
                failing: true,
                ..Self::default()
            }
        }

        fn count(&self) -> usize {
            self.mailboxes.lock().map(|m| m.len()).unwrap_or_default()
        }
    }

    impl std::fmt::Debug for FakeMailboxes {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FakeMailboxes").finish_non_exhaustive()
        }
    }

    #[async_trait::async_trait]
    impl MailboxStore for FakeMailboxes {
        async fn create(&self, mailbox: &Mailbox) -> CoreResult<()> {
            if self.failing {
                return Err(StoreError::new("store is down").into());
            }
            self.mailboxes
                .lock()
                .map_err(|_| CoreError::invariant("poisoned"))?
                .insert(mailbox.id, mailbox.clone());
            Ok(())
        }

        async fn get(&self, id: MailboxId) -> CoreResult<Option<Mailbox>> {
            if self.failing {
                return Err(StoreError::new("store is down").into());
            }
            Ok(self
                .mailboxes
                .lock()
                .map_err(|_| CoreError::invariant("poisoned"))?
                .get(&id)
                .cloned())
        }

        async fn get_by_name(
            &self,
            account_id: AccountId,
            name: &MailboxName,
        ) -> CoreResult<Option<Mailbox>> {
            if self.failing {
                return Err(StoreError::new("store is down").into());
            }
            Ok(self
                .mailboxes
                .lock()
                .map_err(|_| CoreError::invariant("poisoned"))?
                .values()
                .find(|m| m.account_id == account_id && m.name.as_str() == name.as_str())
                .cloned())
        }

        async fn list(&self, account_id: AccountId) -> CoreResult<Vec<Mailbox>> {
            Ok(self
                .mailboxes
                .lock()
                .map_err(|_| CoreError::invariant("poisoned"))?
                .values()
                .filter(|m| m.account_id == account_id)
                .cloned()
                .collect())
        }

        async fn rename(&self, id: MailboxId, new_name: &MailboxName) -> CoreResult<()> {
            let mut guard = self
                .mailboxes
                .lock()
                .map_err(|_| CoreError::invariant("poisoned"))?;
            if let Some(mailbox) = guard.get_mut(&id) {
                mailbox.name = new_name.clone();
            }
            Ok(())
        }

        async fn set_subscribed(&self, id: MailboxId, subscribed: bool) -> CoreResult<()> {
            let mut guard = self
                .mailboxes
                .lock()
                .map_err(|_| CoreError::invariant("poisoned"))?;
            if let Some(mailbox) = guard.get_mut(&id) {
                mailbox.subscribed = subscribed;
            }
            Ok(())
        }

        async fn delete(&self, id: MailboxId) -> CoreResult<bool> {
            if self.refuse_delete {
                return Ok(false);
            }
            Ok(self
                .mailboxes
                .lock()
                .map_err(|_| CoreError::invariant("poisoned"))?
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

    /// Drives a future to completion on this thread.
    ///
    /// The core carries no async runtime and must not gain one, even as a
    /// dev-dependency: `tests/architecture.rs` pins the crate's *declared*
    /// dependencies (dev included) to the list `ARCHITECTURE.md` §3.2 fixes,
    /// and adding `tokio` here to run a handful of tests would weaken the very
    /// check that keeps infrastructure out of the domain.
    ///
    /// These services await only in-memory doubles, so nothing ever yields
    /// pending in practice — a waker that does nothing is sufficient, and if a
    /// future ever did park this loop would spin rather than hang silently on a
    /// missing wake.
    fn block_on<F: Future>(future: F) -> F::Output {
        use std::pin::pin;
        use std::task::{Context, Poll, Waker};

        // `Waker::noop` is the safe constructor for exactly this case, which
        // matters because `unsafe` is forbidden workspace-wide.
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = pin!(future);

        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
        }
    }

    fn name(text: &str) -> MailboxName {
        MailboxName::new(text).expect("a valid mailbox name")
    }

    fn service(store: FakeMailboxes) -> (MailboxService, Arc<FakeMailboxes>) {
        let store = Arc::new(store);
        (MailboxService::new(store.clone()), store)
    }

    #[test]
    fn inbox_cannot_be_created() {
        block_on(async {
            // RFC 9051 §6.3.4. IMAP already refused this; HTTP did not, and a
            // second mailbox named INBOX breaks the invariant delivery relies on.
            let (mailboxes, store) = service(FakeMailboxes::default());
            let account = AccountId::new();

            let error = mailboxes
                .create(account, &name("INBOX"), MailboxAttributes::none())
                .await
                .expect_err("INBOX is not creatable");

            assert!(
                matches!(error, CoreError::InvalidArgument { .. }),
                "{error}"
            );
            assert_eq!(store.count(), 0, "nothing may be written");
        });
    }

    #[test]
    fn inbox_is_refused_whatever_its_case() {
        block_on(async {
            // `is_inbox` is case-insensitive, so the guard must not be defeated by
            // spelling it differently — the storage index is on the exact name, so
            // "Inbox" would otherwise create a second one.
            let (mailboxes, _) = service(FakeMailboxes::default());
            let account = AccountId::new();

            for spelling in ["inbox", "Inbox", "InBoX"] {
                let error = mailboxes
                    .create(account, &name(spelling), MailboxAttributes::none())
                    .await
                    .expect_err("INBOX is not creatable under any spelling");
                assert!(
                    matches!(error, CoreError::InvalidArgument { .. }),
                    "{spelling}"
                );
            }
        });
    }

    #[test]
    fn a_new_mailbox_starts_unsubscribed() {
        block_on(async {
            // ADR-068: IMAP's default is canonical, so both surfaces agree.
            let (mailboxes, _) = service(FakeMailboxes::default());
            let created = mailboxes
                .create(
                    AccountId::new(),
                    &name("Projects"),
                    MailboxAttributes::none(),
                )
                .await
                .expect("the mailbox is created");

            assert!(!created.subscribed);
        });
    }

    #[test]
    fn a_duplicate_name_is_refused() {
        block_on(async {
            let account = AccountId::new();
            let (mailboxes, _) = service(FakeMailboxes::default());

            mailboxes
                .create(account, &name("Archive"), MailboxAttributes::none())
                .await
                .expect("the first create succeeds");

            let error = mailboxes
                .create(account, &name("Archive"), MailboxAttributes::none())
                .await
                .expect_err("the second is refused");
            assert!(matches!(error, CoreError::AlreadyExists { .. }), "{error}");
        });
    }

    #[test]
    fn a_store_failure_during_the_duplicate_check_never_reads_as_free() {
        block_on(async {
            // The bug this service exists to prevent. The old call sites matched
            // `Ok(Some(_))` and treated every other outcome as absence, so a store
            // error meant "the name is free" and CREATE proceeded anyway.
            let (mailboxes, store) = service(FakeMailboxes::failing());

            let error = mailboxes
                .create(
                    AccountId::new(),
                    &name("Archive"),
                    MailboxAttributes::none(),
                )
                .await
                .expect_err("a store failure must not be read as absence");

            assert!(
                matches!(error, CoreError::Storage(_)),
                "expected a storage error, got {error}"
            );
            assert_eq!(store.count(), 0, "nothing may be created on a failed check");
        });
    }

    #[test]
    fn renaming_to_inbox_is_refused() {
        block_on(async {
            // The gap that existed only on the HTTP side: the source was checked,
            // the target was not.
            let account = AccountId::new();
            let existing =
                MailboxService::new_mailbox(account, name("Notes"), MailboxAttributes::none());
            let (mailboxes, _) = service(FakeMailboxes::with(vec![existing]));

            let error = mailboxes
                .rename(account, &name("Notes"), &name("INBOX"))
                .await
                .expect_err("renaming onto INBOX is refused");
            assert!(
                matches!(error, CoreError::InvalidArgument { .. }),
                "{error}"
            );
        });
    }

    #[test]
    fn renaming_inbox_itself_is_refused() {
        block_on(async {
            let account = AccountId::new();
            let inbox =
                MailboxService::new_mailbox(account, name("INBOX"), MailboxAttributes::none());
            let (mailboxes, _) = service(FakeMailboxes::with(vec![inbox]));

            let error = mailboxes
                .rename(account, &name("INBOX"), &name("Elsewhere"))
                .await
                .expect_err("INBOX is not renameable");
            assert!(
                matches!(error, CoreError::InvalidArgument { .. }),
                "{error}"
            );
        });
    }

    #[test]
    fn renaming_onto_a_taken_name_is_refused() {
        block_on(async {
            let account = AccountId::new();
            let (mailboxes, _) = service(FakeMailboxes::with(vec![
                MailboxService::new_mailbox(account, name("Notes"), MailboxAttributes::none()),
                MailboxService::new_mailbox(account, name("Archive"), MailboxAttributes::none()),
            ]));

            let error = mailboxes
                .rename(account, &name("Notes"), &name("Archive"))
                .await
                .expect_err("the target name is taken");
            assert!(matches!(error, CoreError::AlreadyExists { .. }), "{error}");
        });
    }

    #[test]
    fn inbox_cannot_be_deleted() {
        block_on(async {
            let account = AccountId::new();
            let inbox =
                MailboxService::new_mailbox(account, name("INBOX"), MailboxAttributes::none());
            let (mailboxes, store) = service(FakeMailboxes::with(vec![inbox]));

            let error = mailboxes
                .delete(account, &name("INBOX"))
                .await
                .expect_err("INBOX is not deletable");
            assert!(
                matches!(error, CoreError::InvalidArgument { .. }),
                "{error}"
            );
            assert_eq!(store.count(), 1, "INBOX must still be there");
        });
    }

    #[test]
    fn deleting_an_absent_mailbox_reports_not_found() {
        block_on(async {
            let (mailboxes, _) = service(FakeMailboxes::default());
            let error = mailboxes
                .delete(AccountId::new(), &name("Ghost"))
                .await
                .expect_err("there is nothing to delete");
            assert!(matches!(error, CoreError::NotFound { .. }), "{error}");
        });
    }

    #[test]
    fn a_noselect_mailbox_cannot_be_selected() {
        block_on(async {
            // A `\Noselect` name holds children, not messages, so it is not a
            // usable SELECT target nor a usable COPY/MOVE destination.
            let account = AccountId::new();
            let mut holder =
                MailboxService::new_mailbox(account, name("Parent"), MailboxAttributes::none());
            holder.attributes = MailboxAttributes::from_names(vec![String::from("\\Noselect")]);
            let (mailboxes, _) = service(FakeMailboxes::with(vec![holder]));

            let error = mailboxes
                .resolve_selectable(account, &name("Parent"))
                .await
                .expect_err("a \\Noselect mailbox is not selectable");
            assert!(matches!(error, CoreError::Conflict { .. }), "{error}");

            // Plain resolution still finds it: it exists, it just cannot hold mail.
            mailboxes
                .resolve(account, &name("Parent"))
                .await
                .expect("the mailbox exists");
        });
    }

    #[test]
    fn one_accounts_mailbox_is_invisible_to_another() {
        block_on(async {
            // Every lookup is scoped by account, which is what makes the service a
            // safe place to resolve a destination (ADR-067).
            let alice = AccountId::new();
            let bob = AccountId::new();
            let (mailboxes, _) = service(FakeMailboxes::with(vec![MailboxService::new_mailbox(
                alice,
                name("Archive"),
                MailboxAttributes::none(),
            )]));

            mailboxes
                .resolve(alice, &name("Archive"))
                .await
                .expect("Alice sees her own mailbox");

            let error = mailboxes
                .resolve(bob, &name("Archive"))
                .await
                .expect_err("Bob does not see Alice's mailbox");
            assert!(matches!(error, CoreError::NotFound { .. }), "{error}");
        });
    }

    #[test]
    fn a_missing_inbox_is_an_invariant_violation_not_a_miss() {
        block_on(async {
            // Every account is created with an INBOX, so its absence is a bug in
            // the system rather than a legitimate "not found" a caller should
            // handle by creating one.
            let (mailboxes, _) = service(FakeMailboxes::default());
            let error = mailboxes
                .inbox_of(AccountId::new())
                .await
                .expect_err("the account has no INBOX");
            assert!(
                matches!(error, CoreError::InvariantViolated { .. }),
                "{error}"
            );
        });
    }

    #[test]
    fn a_refused_delete_is_a_conflict_not_a_silent_success() {
        block_on(async {
            let account = AccountId::new();
            let target =
                MailboxService::new_mailbox(account, name("Stuck"), MailboxAttributes::none());
            let store = FakeMailboxes {
                refuse_delete: true,
                ..FakeMailboxes::with(vec![target])
            };
            let (mailboxes, _) = service(store);

            let error = mailboxes
                .delete(account, &name("Stuck"))
                .await
                .expect_err("the store declined the delete");
            assert!(matches!(error, CoreError::Conflict { .. }), "{error}");
        });
    }
}
