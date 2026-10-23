//! [`AccountStore`] over `SQLite`.

use crate::{StorageError, row};
use async_trait::async_trait;
use sandesh_core::{
    Account, AccountId, AccountStore, CoreError, CoreResult, Credential, DEFAULT_MAILBOXES,
    EmailAddress, EmailIdentity, EntityKind, EventPayload, IdentityId, Mailbox, MailboxAttributes,
    MailboxId, MailboxName, Timestamp, Uid, UidValidity,
};
use sqlx::{Row, SqlitePool};

use crate::events::EventRecorder;

/// `SQLite`-backed account storage.
#[derive(Debug)]
pub struct SqliteAccountStore {
    pool: SqlitePool,
    events: EventRecorder,
}

impl SqliteAccountStore {
    /// Creates a store over `pool`, emitting events to `events`.
    #[must_use]
    pub fn new(pool: SqlitePool, events: EventRecorder) -> Self {
        Self { pool, events }
    }
}

#[async_trait]
impl AccountStore for SqliteAccountStore {
    /// Creates an account and its default mailboxes in one transaction.
    ///
    /// The mailboxes are part of account creation rather than a follow-up call:
    /// an account without `INBOX` is not a valid account for either IMAP or
    /// POP3, so a partial result must not be observable.
    async fn create(&self, account: &Account) -> CoreResult<()> {
        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;

        let result = sqlx::query(
            "INSERT INTO accounts
                (id, username, credential_hash, credential_plain, is_catch_all, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&account.id.as_bytes()[..])
        .bind(&account.username)
        .bind(account.credential.as_ref().map(|c| c.hash.clone()))
        .bind(account.credential.as_ref().and_then(|c| c.plain.clone()))
        .bind(i64::from(account.is_catch_all))
        .bind(account.created_at.as_micros())
        .execute(&mut *tx)
        .await;

        if let Err(e) = result {
            let err = StorageError::from(e);
            return Err(if err.is_unique_violation() {
                CoreError::already_exists(EntityKind::Account, &account.username)
            } else {
                err.into()
            });
        }

        for (name, attribute) in DEFAULT_MAILBOXES {
            let mailbox_name = MailboxName::new(*name)
                .map_err(|e| CoreError::invalid("mailbox name", e.to_string()))?;
            let mailbox = Mailbox {
                id: MailboxId::new(),
                account_id: account.id,
                name: mailbox_name,
                parent_id: None,
                uid_validity: new_uid_validity(),
                uid_next: Uid::MIN,
                attributes: if attribute.is_empty() {
                    MailboxAttributes::none()
                } else {
                    MailboxAttributes::from_names([*attribute])
                },
                subscribed: true,
                created_at: account.created_at,
                // A brand-new mailbox has had no changes, so it starts at the
                // sequence floor (RFC 7162 §3 permits 1 as the base).
                highest_mod_seq: sandesh_core::ModSeq::MIN,
            };
            crate::mailbox_repo::insert_mailbox(&mut tx, &mailbox).await?;
        }

        tx.commit().await.map_err(StorageError::from)?;

        self.events.emit(EventPayload::AccountCreated {
            account_id: account.id,
            username: account.username.clone(),
        });
        Ok(())
    }

    async fn get(&self, id: AccountId) -> CoreResult<Option<Account>> {
        let row = sqlx::query("SELECT * FROM accounts WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?;

        row.map(|r| account_from_row(&r)).transpose()
    }

    async fn get_by_username(&self, username: &str) -> CoreResult<Option<Account>> {
        let row = sqlx::query("SELECT * FROM accounts WHERE username = ?")
            .bind(username)
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?;

        row.map(|r| account_from_row(&r)).transpose()
    }

    async fn list(&self) -> CoreResult<Vec<Account>> {
        let rows = sqlx::query("SELECT * FROM accounts ORDER BY created_at")
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        rows.iter().map(account_from_row).collect()
    }

    /// Deletes an account.
    ///
    /// `ON DELETE CASCADE` removes its identities, mailboxes and every
    /// placement in them. Messages left without any placement are cleaned up by
    /// [`collect_orphans`](sandesh_core::MessageStore::collect_orphans) rather
    /// than here — a message may still be referenced from another account's
    /// mailbox, so deletion is never assumed to be transitive.
    async fn delete(&self, id: AccountId) -> CoreResult<bool> {
        let account = self.get(id).await?;
        let Some(account) = account else {
            return Ok(false);
        };

        let affected = sqlx::query("DELETE FROM accounts WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .execute(&self.pool)
            .await
            .map_err(StorageError::from)?
            .rows_affected();

        if affected > 0 {
            self.events.emit(EventPayload::AccountDeleted {
                account_id: id,
                username: account.username,
            });
        }
        Ok(affected > 0)
    }

    async fn add_identity(
        &self,
        account_id: AccountId,
        address: &EmailAddress,
        is_primary: bool,
    ) -> CoreResult<()> {
        let result = sqlx::query(
            "INSERT INTO email_identities (id, account_id, address, address_raw, is_primary)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&IdentityId::new().as_bytes()[..])
        .bind(&account_id.as_bytes()[..])
        .bind(address.normalized())
        .bind(address.original())
        .bind(i64::from(is_primary))
        .execute(&self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                let err = StorageError::from(e);
                Err(if err.is_unique_violation() {
                    CoreError::already_exists(EntityKind::Identity, address.normalized())
                } else {
                    err.into()
                })
            }
        }
    }

    async fn remove_identity(
        &self,
        account_id: AccountId,
        address: &EmailAddress,
    ) -> CoreResult<bool> {
        let affected =
            sqlx::query("DELETE FROM email_identities WHERE account_id = ? AND address = ?")
                .bind(&account_id.as_bytes()[..])
                .bind(address.normalized())
                .execute(&self.pool)
                .await
                .map_err(StorageError::from)?
                .rows_affected();

        Ok(affected > 0)
    }

    async fn identities(&self, account_id: AccountId) -> CoreResult<Vec<EmailIdentity>> {
        let rows = sqlx::query(
            "SELECT * FROM email_identities WHERE account_id = ? ORDER BY is_primary DESC, address",
        )
        .bind(&account_id.as_bytes()[..])
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;

        rows.iter()
            .map(|r| {
                Ok(EmailIdentity {
                    id: IdentityId::from_bytes(row::uuid_bytes(r, "id", "email_identities")?),
                    account_id: AccountId::from_bytes(row::uuid_bytes(
                        r,
                        "account_id",
                        "email_identities",
                    )?),
                    address: EmailAddress::parse_lenient(
                        r.try_get::<String, _>("address_raw")
                            .map_err(StorageError::from)?
                            .as_str(),
                    ),
                    is_primary: r
                        .try_get::<i64, _>("is_primary")
                        .map_err(StorageError::from)?
                        != 0,
                })
            })
            .collect()
    }

    /// Resolves a recipient address to its account.
    ///
    /// Matches on the normalised form, so `Dev@EXAMPLE.test` finds the identity
    /// stored as `Dev@example.test`.
    async fn resolve_address(&self, address: &EmailAddress) -> CoreResult<Option<AccountId>> {
        let row = sqlx::query("SELECT account_id FROM email_identities WHERE address = ?")
            .bind(address.normalized())
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?;

        row.map(|r| {
            Ok(AccountId::from_bytes(row::uuid_bytes(
                &r,
                "account_id",
                "email_identities",
            )?))
        })
        .transpose()
    }

    async fn catch_all(&self) -> CoreResult<Option<Account>> {
        let row = sqlx::query("SELECT * FROM accounts WHERE is_catch_all = 1")
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?;

        row.map(|r| account_from_row(&r)).transpose()
    }

    async fn update_credential(
        &self,
        id: AccountId,
        credential: Option<&Credential>,
    ) -> CoreResult<bool> {
        let hash = credential.map(|c| c.hash.as_str());
        let plain = credential.and_then(|c| c.plain.as_deref());

        let affected = sqlx::query(
            "UPDATE accounts SET credential_hash = ?, credential_plain = ? WHERE id = ?",
        )
        .bind(hash)
        .bind(plain)
        .bind(&id.as_bytes()[..])
        .execute(&self.pool)
        .await
        .map_err(StorageError::from)?
        .rows_affected();

        Ok(affected > 0)
    }

    async fn set_catch_all(&self, id: AccountId, is_catch_all: bool) -> CoreResult<bool> {
        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;
        if is_catch_all {
            // Ensure no other account is catch-all (only one allowed per CORE_SPEC).
            sqlx::query("UPDATE accounts SET is_catch_all = 0 WHERE is_catch_all = 1")
                .execute(&mut *tx)
                .await
                .map_err(StorageError::from)?;
        }
        let affected = sqlx::query("UPDATE accounts SET is_catch_all = ? WHERE id = ?")
            .bind(i64::from(is_catch_all))
            .bind(&id.as_bytes()[..])
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?
            .rows_affected();

        tx.commit().await.map_err(StorageError::from)?;
        Ok(affected > 0)
    }
}

fn account_from_row(r: &sqlx::sqlite::SqliteRow) -> CoreResult<Account> {
    let hash: Option<String> = r.try_get("credential_hash").map_err(StorageError::from)?;
    let plain: Option<String> = r.try_get("credential_plain").map_err(StorageError::from)?;

    Ok(Account {
        id: AccountId::from_bytes(row::uuid_bytes(r, "id", "accounts")?),
        username: r.try_get("username").map_err(StorageError::from)?,
        credential: hash.map(|hash| Credential { hash, plain }),
        is_catch_all: r
            .try_get::<i64, _>("is_catch_all")
            .map_err(StorageError::from)?
            != 0,
        created_at: Timestamp::from_micros(r.try_get("created_at").map_err(StorageError::from)?),
    })
}

/// A fresh UID-space generation.
///
/// Delegates to [`UidValidity::next_epoch`], which owns the invariant. Kept as
/// a named function because the repositories read better for it.
pub(crate) fn new_uid_validity() -> UidValidity {
    UidValidity::next_epoch()
}
