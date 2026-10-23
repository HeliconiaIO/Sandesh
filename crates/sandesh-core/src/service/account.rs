//! Account creation, and the invariants an account must satisfy to be usable.
//!
//! An account is created from three places — SMTP's `auto_create` policy, the
//! REST API, and first-run bootstrap — and each wrote its own version. The
//! versions disagreed about what an account needs to be valid, which is how the
//! HTTP surface came to accept two shapes that cannot work: an account with no
//! identity, which no mail can ever be routed to, and a second catch-all, when
//! resolution assumes at most one (ADR-070).
//!
//! ## What lives here, and what does not
//!
//! Here: the username rule, that an account must be reachable, and that at most
//! one account is the catch-all.
//!
//! Not here: password hashing. Argon2 is a `sandesh-proto` primitive and the
//! core declares no cryptography beyond `sha2` (ARCHITECTURE §3.2), so callers
//! hash first and hand over a [`Credential`]. That keeps one verifier in the
//! workspace rather than one per surface (ADR-032).
//!
//! Also not here: provisioning the default mailboxes. `AccountStore::create`
//! does that in the same transaction as the account row, so an account without
//! an `INBOX` is never observable — an invariant better enforced by the
//! transaction than by a caller remembering to follow up.

use std::sync::Arc;

use crate::{
    Account, AccountId, AccountStore, CoreError, CoreResult, Credential, EmailAddress, EntityKind,
    Timestamp,
};

/// Account operations shared by every surface.
pub struct AccountService {
    accounts: Arc<dyn AccountStore>,
}

impl std::fmt::Debug for AccountService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountService").finish_non_exhaustive()
    }
}

/// What an account should be created as.
///
/// A struct rather than a long argument list because the invariants relate the
/// fields to each other — whether an identity is required depends on
/// `is_catch_all` — and a caller assembling one field at a time is how the
/// three divergent versions arose.
#[derive(Debug, Clone)]
pub struct NewAccount {
    /// The login name. Trimmed, and required to be non-empty.
    pub username: String,
    /// Addresses routing to this account. The first becomes the primary
    /// identity.
    pub addresses: Vec<EmailAddress>,
    /// An **already-hashed** credential, or `None` for an account with no
    /// password.
    pub credential: Option<Credential>,
    /// Whether unmatched recipients route here.
    pub is_catch_all: bool,
}

impl AccountService {
    /// Builds the service over the account port.
    #[must_use]
    pub fn new(accounts: Arc<dyn AccountStore>) -> Self {
        Self { accounts }
    }

    /// The account with this id.
    ///
    /// # Errors
    /// [`CoreError::NotFound`] when there is no such account, and
    /// [`CoreError::Storage`] when the store could not answer.
    pub async fn get(&self, id: AccountId) -> CoreResult<Account> {
        self.accounts
            .get(id)
            .await?
            .ok_or_else(|| CoreError::not_found(EntityKind::Account, id))
    }

    /// Creates an account with its identities.
    ///
    /// Three rules, each of which one caller was missing:
    ///
    /// *The username is trimmed and must be non-empty.* An account whose
    /// username is blank or whitespace cannot be logged into and cannot be
    /// told apart from another such account.
    ///
    /// *A non-catch-all account needs at least one address.* Mail is routed by
    /// identity (`CORE_SPEC` §7), so an account with no identity can never
    /// receive anything — creating one succeeds and produces something inert,
    /// which is worse than refusing. The catch-all is the deliberate
    /// exception: it is reached *because* nothing else matched, so it needs no
    /// identity of its own.
    ///
    /// *At most one account is the catch-all.* `AccountStore::catch_all`
    /// returns a single account, so a second one makes routing depend on which
    /// the store happens to answer with.
    ///
    /// The username is not required to look like an address: SMTP's
    /// `auto_create` uses the full address so that two addresses differing only
    /// by domain do not collide, while bootstrap uses a plain name. Both are
    /// legitimate.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] when the username is blank or a
    /// non-catch-all account has no address; [`CoreError::AlreadyExists`] when
    /// the username is taken or a catch-all already exists;
    /// [`CoreError::Storage`] on a store failure.
    pub async fn create(&self, request: NewAccount) -> CoreResult<Account> {
        let username = request.username.trim();
        if username.is_empty() {
            return Err(CoreError::InvalidArgument {
                field: String::from("username"),
                reason: String::from("a username cannot be blank"),
            });
        }

        if !request.is_catch_all && request.addresses.is_empty() {
            return Err(CoreError::InvalidArgument {
                field: String::from("addresses"),
                reason: String::from(
                    "an account needs at least one address to receive mail; \
                     only the catch-all account may have none",
                ),
            });
        }

        if self.accounts.get_by_username(username).await?.is_some() {
            return Err(CoreError::already_exists(EntityKind::Account, username));
        }

        if request.is_catch_all
            && let Some(existing) = self.accounts.catch_all().await?
        {
            return Err(CoreError::already_exists(
                EntityKind::Account,
                format!("{} is already the catch-all account", existing.username),
            ));
        }

        let account = Account {
            id: AccountId::new(),
            username: username.to_owned(),
            credential: request.credential,
            is_catch_all: request.is_catch_all,
            created_at: Timestamp::now(),
        };
        self.accounts.create(&account).await?;

        // The first address is the primary identity. Added after the account so
        // there is something to attach them to; `AccountStore::create` has
        // already provisioned the default mailboxes in its own transaction.
        for (index, address) in request.addresses.iter().enumerate() {
            self.accounts
                .add_identity(account.id, address, index == 0)
                .await?;
        }

        Ok(account)
    }

    /// The catch-all account, creating it if there is none.
    ///
    /// First-run setup: Sandesh's default policy routes unmatched recipients to
    /// the catch-all (`CORE_SPEC` §7), so an instance without one accepts a
    /// connection and then fails to deliver. Idempotent, because every start
    /// calls it.
    ///
    /// Returns the account and whether it was created.
    ///
    /// # Errors
    /// [`CoreError::Storage`] on a store failure, and whatever
    /// [`create`](Self::create) reports.
    pub async fn ensure_catch_all(&self, username: &str) -> CoreResult<(Account, bool)> {
        if let Some(existing) = self.accounts.catch_all().await? {
            return Ok((existing, false));
        }

        let account = self
            .create(NewAccount {
                username: username.to_owned(),
                // The catch-all needs no identity: it is reached precisely when
                // no identity matched.
                addresses: Vec::new(),
                credential: None,
                is_catch_all: true,
            })
            .await?;
        Ok((account, true))
    }

    /// Creates an account for an address that matched nothing, under SMTP's
    /// `auto_create` policy.
    ///
    /// The full normalised address is the username, so `a@x.test` and
    /// `a@y.test` do not collide, and it becomes the primary identity.
    ///
    /// # Errors
    /// As [`create`](Self::create).
    pub async fn create_for_address(&self, address: &EmailAddress) -> CoreResult<AccountId> {
        let account = self
            .create(NewAccount {
                username: address.normalized(),
                addresses: vec![address.clone()],
                credential: None,
                is_catch_all: false,
            })
            .await?;
        Ok(account.id)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::{EmailIdentity, IdentityId};

    /// Drives a future to completion without an async runtime.
    ///
    /// See the same helper in `service/mailbox.rs`: the core declares no
    /// runtime, dev-dependencies included, and `tests/architecture.rs` enforces
    /// that.
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
    struct FakeAccounts {
        accounts: Mutex<HashMap<AccountId, Account>>,
        identities: Mutex<Vec<EmailIdentity>>,
    }

    impl std::fmt::Debug for FakeAccounts {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FakeAccounts").finish_non_exhaustive()
        }
    }

    impl FakeAccounts {
        fn count(&self) -> usize {
            self.accounts.lock().expect("not poisoned").len()
        }

        fn identities_of(&self, account: AccountId) -> Vec<EmailIdentity> {
            self.identities
                .lock()
                .expect("not poisoned")
                .iter()
                .filter(|identity| identity.account_id == account)
                .cloned()
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl AccountStore for FakeAccounts {
        async fn create(&self, account: &Account) -> CoreResult<()> {
            self.accounts
                .lock()
                .expect("not poisoned")
                .insert(account.id, account.clone());
            Ok(())
        }
        async fn get(&self, id: AccountId) -> CoreResult<Option<Account>> {
            Ok(self
                .accounts
                .lock()
                .expect("not poisoned")
                .get(&id)
                .cloned())
        }
        async fn get_by_username(&self, username: &str) -> CoreResult<Option<Account>> {
            Ok(self
                .accounts
                .lock()
                .expect("not poisoned")
                .values()
                .find(|a| a.username == username)
                .cloned())
        }
        async fn list(&self) -> CoreResult<Vec<Account>> {
            Ok(self
                .accounts
                .lock()
                .expect("not poisoned")
                .values()
                .cloned()
                .collect())
        }
        async fn delete(&self, id: AccountId) -> CoreResult<bool> {
            Ok(self
                .accounts
                .lock()
                .expect("not poisoned")
                .remove(&id)
                .is_some())
        }
        async fn add_identity(
            &self,
            account_id: AccountId,
            address: &EmailAddress,
            is_primary: bool,
        ) -> CoreResult<()> {
            self.identities
                .lock()
                .expect("not poisoned")
                .push(EmailIdentity {
                    id: IdentityId::new(),
                    account_id,
                    address: address.clone(),
                    is_primary,
                });
            Ok(())
        }
        async fn remove_identity(
            &self,
            _account_id: AccountId,
            _address: &EmailAddress,
        ) -> CoreResult<bool> {
            Ok(false)
        }
        async fn identities(&self, account_id: AccountId) -> CoreResult<Vec<EmailIdentity>> {
            Ok(self.identities_of(account_id))
        }
        async fn resolve_address(&self, address: &EmailAddress) -> CoreResult<Option<AccountId>> {
            Ok(self
                .identities
                .lock()
                .expect("not poisoned")
                .iter()
                .find(|identity| identity.address.normalized() == address.normalized())
                .map(|identity| identity.account_id))
        }
        async fn catch_all(&self) -> CoreResult<Option<Account>> {
            Ok(self
                .accounts
                .lock()
                .expect("not poisoned")
                .values()
                .find(|a| a.is_catch_all)
                .cloned())
        }
        async fn update_credential(
            &self,
            _id: AccountId,
            _credential: Option<&Credential>,
        ) -> CoreResult<bool> {
            Ok(false)
        }
        async fn set_catch_all(&self, _id: AccountId, _is_catch_all: bool) -> CoreResult<bool> {
            Ok(false)
        }
    }

    fn address(text: &str) -> EmailAddress {
        EmailAddress::parse_lenient(text)
    }

    fn service(store: &Arc<FakeAccounts>) -> AccountService {
        AccountService::new(Arc::clone(store) as Arc<dyn AccountStore>)
    }

    fn request(username: &str, addresses: Vec<EmailAddress>) -> NewAccount {
        NewAccount {
            username: username.to_owned(),
            addresses,
            credential: None,
            is_catch_all: false,
        }
    }

    #[test]
    fn an_account_with_no_address_cannot_be_created() {
        // Mail is routed by identity, so this account could never receive
        // anything. The HTTP handler used to accept it.
        block_on(async {
            let store = Arc::new(FakeAccounts::default());
            let error = service(&store)
                .create(request("nobody", Vec::new()))
                .await
                .expect_err("an unreachable account is refused");

            assert!(
                matches!(error, CoreError::InvalidArgument { .. }),
                "{error}"
            );
            assert_eq!(store.count(), 0, "nothing may be written");
        });
    }

    #[test]
    fn a_blank_username_is_refused() {
        block_on(async {
            let store = Arc::new(FakeAccounts::default());
            for blank in ["", "   ", "\t"] {
                let error = service(&store)
                    .create(request(blank, vec![address("a@example.test")]))
                    .await
                    .expect_err("a blank username is refused");
                assert!(
                    matches!(error, CoreError::InvalidArgument { .. }),
                    "{blank:?}"
                );
            }
            assert_eq!(store.count(), 0);
        });
    }

    #[test]
    fn a_username_is_trimmed() {
        block_on(async {
            let store = Arc::new(FakeAccounts::default());
            let account = service(&store)
                .create(request("  alice  ", vec![address("alice@example.test")]))
                .await
                .expect("the account is created");
            assert_eq!(account.username, "alice");
        });
    }

    #[test]
    fn a_duplicate_username_is_refused() {
        block_on(async {
            let store = Arc::new(FakeAccounts::default());
            let svc = service(&store);
            svc.create(request("alice", vec![address("alice@example.test")]))
                .await
                .expect("the first create succeeds");

            let error = svc
                .create(request("alice", vec![address("other@example.test")]))
                .await
                .expect_err("the username is taken");
            assert!(matches!(error, CoreError::AlreadyExists { .. }), "{error}");
            assert_eq!(store.count(), 1);
        });
    }

    #[test]
    fn a_second_catch_all_is_refused() {
        // `AccountStore::catch_all` answers with one account, so a second makes
        // routing depend on which the store happens to return.
        block_on(async {
            let store = Arc::new(FakeAccounts::default());
            let svc = service(&store);
            svc.ensure_catch_all("catch-all")
                .await
                .expect("the first catch-all is created");

            let error = svc
                .create(NewAccount {
                    username: String::from("second"),
                    addresses: Vec::new(),
                    credential: None,
                    is_catch_all: true,
                })
                .await
                .expect_err("a second catch-all is refused");
            assert!(matches!(error, CoreError::AlreadyExists { .. }), "{error}");
            assert_eq!(store.count(), 1);
        });
    }

    #[test]
    fn the_catch_all_may_have_no_address() {
        // The deliberate exception: it is reached precisely when no identity
        // matched, so requiring one would make it uncreatable.
        block_on(async {
            let store = Arc::new(FakeAccounts::default());
            let (account, created) = service(&store)
                .ensure_catch_all("catch-all")
                .await
                .expect("the catch-all is created");

            assert!(created);
            assert!(account.is_catch_all);
            assert!(store.identities_of(account.id).is_empty());
        });
    }

    #[test]
    fn ensuring_the_catch_all_twice_creates_one_account() {
        // Every start calls it, so it has to be idempotent.
        block_on(async {
            let store = Arc::new(FakeAccounts::default());
            let svc = service(&store);

            let (first, created_first) =
                svc.ensure_catch_all("catch-all").await.expect("first call");
            let (second, created_second) = svc
                .ensure_catch_all("catch-all")
                .await
                .expect("second call");

            assert!(created_first);
            assert!(!created_second, "the second call creates nothing");
            assert_eq!(first.id, second.id);
            assert_eq!(store.count(), 1);
        });
    }

    #[test]
    fn the_first_address_becomes_the_primary_identity() {
        block_on(async {
            let store = Arc::new(FakeAccounts::default());
            let account = service(&store)
                .create(request(
                    "alice",
                    vec![address("alice@example.test"), address("alias@example.test")],
                ))
                .await
                .expect("the account is created");

            let identities = store.identities_of(account.id);
            assert_eq!(identities.len(), 2);
            assert_eq!(
                identities.iter().filter(|i| i.is_primary).count(),
                1,
                "exactly one identity is primary"
            );
            assert!(
                identities
                    .iter()
                    .find(|i| i.is_primary)
                    .is_some_and(|i| i.address.normalized() == "alice@example.test"),
                "the first address is the primary one"
            );
        });
    }

    #[test]
    fn smtp_auto_create_uses_the_full_address_as_the_username() {
        // So `a@x.test` and `a@y.test` do not collide — the rationale the SMTP
        // path documented and the other paths did not share.
        block_on(async {
            let store = Arc::new(FakeAccounts::default());
            let svc = service(&store);

            let first = svc
                .create_for_address(&address("a@x.test"))
                .await
                .expect("the first account is created");
            let second = svc
                .create_for_address(&address("a@y.test"))
                .await
                .expect("the second does not collide");

            assert_ne!(first, second);
            assert_eq!(store.count(), 2);
            assert!(
                store
                    .identities_of(first)
                    .iter()
                    .any(|i| i.is_primary && i.address.normalized() == "a@x.test")
            );
        });
    }
}
