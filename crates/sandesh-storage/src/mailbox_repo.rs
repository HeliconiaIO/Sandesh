//! [`MailboxStore`] over `SQLite`, including UID allocation.

use crate::{StorageError, events::EventRecorder, row};
use async_trait::async_trait;
use sandesh_core::{
    AccountId, CoreError, CoreResult, EntityKind, EventPayload, Mailbox, MailboxAttributes,
    MailboxCounts, MailboxId, MailboxName, MailboxStore, ModSeq, Timestamp, Uid, UidValidity,
};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

/// `SQLite`-backed mailbox storage.
#[derive(Debug)]
pub struct SqliteMailboxStore {
    pool: SqlitePool,
    events: EventRecorder,
}

impl SqliteMailboxStore {
    /// Creates a store over `pool`.
    #[must_use]
    pub fn new(pool: SqlitePool, events: EventRecorder) -> Self {
        Self { pool, events }
    }
}

/// Inserts a mailbox row within an existing transaction.
///
/// Shared with account creation, which makes the default mailboxes part of the
/// same atomic unit as the account itself.
pub(crate) async fn insert_mailbox(
    tx: &mut Transaction<'_, Sqlite>,
    mailbox: &Mailbox,
) -> CoreResult<()> {
    let result = sqlx::query(
        "INSERT INTO mailboxes
            (id, account_id, name, parent_id, uid_validity, uid_next,
             attributes, subscribed, created_at, highest_mod_seq)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&mailbox.id.as_bytes()[..])
    .bind(&mailbox.account_id.as_bytes()[..])
    .bind(mailbox.name.as_str())
    .bind(mailbox.parent_id.map(|p| p.as_bytes().to_vec()))
    .bind(i64::from(mailbox.uid_validity.get()))
    .bind(i64::from(mailbox.uid_next.get()))
    .bind(mailbox.attributes.to_csv())
    .bind(i64::from(mailbox.subscribed))
    .bind(mailbox.created_at.as_micros())
    .bind(modseq_to_i64(mailbox.highest_mod_seq))
    .execute(&mut **tx)
    .await;

    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            let err = StorageError::from(e);
            Err(if err.is_unique_violation() {
                CoreError::already_exists(EntityKind::Mailbox, mailbox.name.as_str())
            } else {
                err.into()
            })
        }
    }
}

/// Allocates the next UID for `mailbox_id`, advancing the mailbox counter.
///
/// Must be called inside the same transaction as the entry insert. The
/// `RETURNING` clause makes read-and-increment a single atomic statement, which
/// is what prevents two concurrent deliveries from being handed the same UID —
/// the one race that would corrupt every connected client's cache
/// (`CORE_SPEC.md` §11).
pub(crate) async fn allocate_uid(
    tx: &mut Transaction<'_, Sqlite>,
    mailbox_id: MailboxId,
) -> CoreResult<Uid> {
    let row = sqlx::query(
        "UPDATE mailboxes SET uid_next = uid_next + 1
         WHERE id = ?
         RETURNING uid_next - 1 AS assigned",
    )
    .bind(&mailbox_id.as_bytes()[..])
    .fetch_optional(&mut **tx)
    .await
    .map_err(StorageError::from)?
    .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, mailbox_id))?;

    let assigned: i64 = row.try_get("assigned").map_err(StorageError::from)?;
    let assigned = row::u32_from(assigned, "uid_next", "mailboxes")?;

    Uid::new(assigned)
        .ok_or_else(|| CoreError::invariant("mailbox uid_next produced the reserved UID 0"))
}

/// Narrows a [`ModSeq`] to the `i64` `SQLite` stores.
///
/// `ModSeq` is a `u64` because RFC 7162 permits the full range, while `SQLite`
/// integers are signed. Saturating is safe rather than lossy in practice: the
/// sequence advances once per mailbox change, so reaching `i64::MAX` is not
/// reachable, and saturating beats a wrap that would re-issue a value.
fn modseq_to_i64(value: ModSeq) -> i64 {
    i64::try_from(value.get()).unwrap_or(i64::MAX)
}

/// Allocates the next modification sequence for `mailbox_id`.
///
/// The CONDSTORE analogue of [`allocate_uid`], and deliberately the same shape:
/// a single `UPDATE ... RETURNING` so read-and-increment cannot interleave.
/// Two sessions storing flags on the same mailbox at the same instant must
/// receive **different** values — a duplicate would make `CHANGEDSINCE` skip a
/// change, silently desynchronising a client (RFC 7162 §3).
///
/// Unlike `allocate_uid`, this returns the value it just wrote rather than the
/// previous one: `highest_mod_seq` *is* the newest issued sequence, which is
/// what `HIGHESTMODSEQ` reports, whereas `uid_next` names a UID not yet used.
///
/// Must be called inside the transaction that stamps the entry, or the stamp
/// and the high-water mark could diverge under a crash.
pub(crate) async fn allocate_mod_seq(
    tx: &mut Transaction<'_, Sqlite>,
    mailbox_id: MailboxId,
) -> CoreResult<ModSeq> {
    let row = sqlx::query(
        "UPDATE mailboxes SET highest_mod_seq = highest_mod_seq + 1
         WHERE id = ?
         RETURNING highest_mod_seq AS assigned",
    )
    .bind(&mailbox_id.as_bytes()[..])
    .fetch_optional(&mut **tx)
    .await
    .map_err(StorageError::from)?
    .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, mailbox_id))?;

    let assigned: i64 = row.try_get("assigned").map_err(StorageError::from)?;

    ModSeq::new(assigned.unsigned_abs())
        .ok_or_else(|| CoreError::invariant("mailbox highest_mod_seq produced the reserved 0"))
}

#[async_trait]
impl MailboxStore for SqliteMailboxStore {
    /// Creates a mailbox, creating any missing ancestors first.
    ///
    /// Implicit parent creation is required by IMAP `CREATE` (`CORE_SPEC.md`
    /// §6): creating `Work/Projects/Sandesh` must succeed even when neither
    /// ancestor exists.
    async fn create(&self, mailbox: &Mailbox) -> CoreResult<()> {
        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;

        let mut parent_id = None;
        let mut ancestors: Vec<MailboxName> = Vec::new();
        let mut current = mailbox.name.parent();
        while let Some(name) = current {
            current = name.parent();
            ancestors.push(name);
        }

        // Outermost ancestor first, so each parent exists before its child.
        for name in ancestors.into_iter().rev() {
            let existing = fetch_by_name(&mut tx, mailbox.account_id, &name).await?;
            parent_id = Some(if let Some(found) = existing {
                found.id
            } else {
                let created = Mailbox {
                    id: MailboxId::new(),
                    account_id: mailbox.account_id,
                    name,
                    parent_id,
                    uid_validity: crate::account_repo::new_uid_validity(),
                    uid_next: Uid::MIN,
                    attributes: MailboxAttributes::none(),
                    subscribed: true,
                    created_at: mailbox.created_at,
                    highest_mod_seq: ModSeq::MIN,
                };
                insert_mailbox(&mut tx, &created).await?;
                created.id
            });
        }

        let to_insert = Mailbox {
            parent_id,
            ..mailbox.clone()
        };
        insert_mailbox(&mut tx, &to_insert).await?;
        tx.commit().await.map_err(StorageError::from)?;

        self.events.emit(EventPayload::MailboxCreated {
            mailbox_id: mailbox.id,
            account_id: mailbox.account_id,
            name: mailbox.name.as_str().to_owned(),
            uid_validity: mailbox.uid_validity.get(),
        });
        Ok(())
    }

    async fn get(&self, id: MailboxId) -> CoreResult<Option<Mailbox>> {
        let row = sqlx::query("SELECT * FROM mailboxes WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?;

        row.map(|r| mailbox_from_row(&r)).transpose()
    }

    async fn get_by_name(
        &self,
        account_id: AccountId,
        name: &MailboxName,
    ) -> CoreResult<Option<Mailbox>> {
        let row = sqlx::query("SELECT * FROM mailboxes WHERE account_id = ? AND name = ?")
            .bind(&account_id.as_bytes()[..])
            .bind(name.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?;

        row.map(|r| mailbox_from_row(&r)).transpose()
    }

    async fn list(&self, account_id: AccountId) -> CoreResult<Vec<Mailbox>> {
        let rows = sqlx::query("SELECT * FROM mailboxes WHERE account_id = ? ORDER BY name")
            .bind(&account_id.as_bytes()[..])
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        rows.iter().map(mailbox_from_row).collect()
    }

    /// Renames a mailbox and every descendant.
    ///
    /// RFC 9051 requires descendants to follow the parent. UIDs are preserved:
    /// a rename does not invalidate a client's cache, so `uid_validity` is
    /// untouched.
    async fn rename(&self, id: MailboxId, new_name: &MailboxName) -> CoreResult<()> {
        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;

        let row = sqlx::query("SELECT * FROM mailboxes WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .fetch_optional(&mut *tx)
            .await
            .map_err(StorageError::from)?
            .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, id))?;
        let existing = mailbox_from_row(&row)?;
        let old_name = existing.name.clone();

        let result = sqlx::query("UPDATE mailboxes SET name = ? WHERE id = ?")
            .bind(new_name.as_str())
            .bind(&id.as_bytes()[..])
            .execute(&mut *tx)
            .await;
        if let Err(e) = result {
            let err = StorageError::from(e);
            return Err(if err.is_unique_violation() {
                CoreError::already_exists(EntityKind::Mailbox, new_name.as_str())
            } else {
                err.into()
            });
        }

        // Descendants: replace the old prefix, keeping each subtree path.
        let prefix = format!("{}{}", old_name.as_str(), sandesh_core::HIERARCHY_DELIMITER);
        sqlx::query(
            "UPDATE mailboxes
             SET name = ? || substr(name, ?)
             WHERE account_id = ? AND name LIKE ? ESCAPE '\\'",
        )
        .bind(format!(
            "{}{}",
            new_name.as_str(),
            sandesh_core::HIERARCHY_DELIMITER
        ))
        .bind(i64::try_from(prefix.len() + 1).unwrap_or(i64::MAX))
        .bind(&existing.account_id.as_bytes()[..])
        .bind(format!("{}%", escape_like(&prefix)))
        .execute(&mut *tx)
        .await
        .map_err(StorageError::from)?;

        tx.commit().await.map_err(StorageError::from)?;

        self.events.emit(EventPayload::MailboxRenamed {
            mailbox_id: id,
            old_name: old_name.as_str().to_owned(),
            new_name: new_name.as_str().to_owned(),
        });
        Ok(())
    }

    async fn set_subscribed(&self, id: MailboxId, subscribed: bool) -> CoreResult<()> {
        let affected = sqlx::query("UPDATE mailboxes SET subscribed = ? WHERE id = ?")
            .bind(i64::from(subscribed))
            .bind(&id.as_bytes()[..])
            .execute(&self.pool)
            .await
            .map_err(StorageError::from)?
            .rows_affected();

        if affected == 0 {
            return Err(CoreError::not_found(EntityKind::Mailbox, id));
        }

        self.events.emit(EventPayload::MailboxSubscriptionChanged {
            mailbox_id: id,
            subscribed,
        });
        Ok(())
    }

    /// Deletes a mailbox and its placements.
    ///
    /// Placements go via `ON DELETE CASCADE`. Messages are not deleted here:
    /// one may still be placed in another mailbox. Unreferenced messages are
    /// reclaimed by `collect_orphans`.
    async fn delete(&self, id: MailboxId) -> CoreResult<bool> {
        let Some(mailbox) = self.get(id).await? else {
            return Ok(false);
        };

        let affected = sqlx::query("DELETE FROM mailboxes WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .execute(&self.pool)
            .await
            .map_err(StorageError::from)?
            .rows_affected();

        if affected > 0 {
            self.events.emit(EventPayload::MailboxDeleted {
                mailbox_id: id,
                account_id: mailbox.account_id,
                name: mailbox.name.as_str().to_owned(),
            });
        }
        Ok(affected > 0)
    }

    async fn counts(&self, id: MailboxId) -> CoreResult<MailboxCounts> {
        let row = sqlx::query(
            "SELECT
                (SELECT COUNT(*) FROM message_entries WHERE mailbox_id = ?) AS total,
                (SELECT COUNT(*) FROM message_entries
                  WHERE mailbox_id = ? AND (flags & 1) = 0)              AS unseen,
                (SELECT uid_next FROM mailboxes WHERE id = ?)            AS uid_next,
                (SELECT highest_mod_seq FROM mailboxes WHERE id = ?)     AS highest_mod_seq",
        )
        .bind(&id.as_bytes()[..])
        .bind(&id.as_bytes()[..])
        .bind(&id.as_bytes()[..])
        .bind(&id.as_bytes()[..])
        .fetch_one(&self.pool)
        .await
        .map_err(StorageError::from)?;

        let uid_next: Option<i64> = row.try_get("uid_next").map_err(StorageError::from)?;
        let uid_next = uid_next.ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, id))?;

        Ok(MailboxCounts {
            total: row
                .try_get::<i64, _>("total")
                .map_err(StorageError::from)?
                .unsigned_abs(),
            unseen: row
                .try_get::<i64, _>("unseen")
                .map_err(StorageError::from)?
                .unsigned_abs(),
            uid_next: row::u32_from(uid_next, "uid_next", "mailboxes")?,
            highest_mod_seq: ModSeq::new(
                row.try_get::<i64, _>("highest_mod_seq")
                    .map_err(StorageError::from)?
                    .unsigned_abs(),
            )
            .ok_or_else(|| StorageError::corrupt("mailboxes", "highest_mod_seq is 0"))?,
        })
    }
}

async fn fetch_by_name(
    tx: &mut Transaction<'_, Sqlite>,
    account_id: AccountId,
    name: &MailboxName,
) -> CoreResult<Option<Mailbox>> {
    let row = sqlx::query("SELECT * FROM mailboxes WHERE account_id = ? AND name = ?")
        .bind(&account_id.as_bytes()[..])
        .bind(name.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(StorageError::from)?;

    row.map(|r| mailbox_from_row(&r)).transpose()
}

/// Escapes `LIKE` wildcards so a mailbox literally named `A_B` cannot match
/// `AxB` during a rename.
fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn mailbox_from_row(r: &sqlx::sqlite::SqliteRow) -> CoreResult<Mailbox> {
    let name: String = r.try_get("name").map_err(StorageError::from)?;
    let uid_next: i64 = r.try_get("uid_next").map_err(StorageError::from)?;
    let uid_validity: i64 = r.try_get("uid_validity").map_err(StorageError::from)?;

    Ok(Mailbox {
        id: MailboxId::from_bytes(row::uuid_bytes(r, "id", "mailboxes")?),
        account_id: AccountId::from_bytes(row::uuid_bytes(r, "account_id", "mailboxes")?),
        name: MailboxName::new(name)
            .map_err(|e| StorageError::corrupt("mailboxes", e.to_string()))?,
        parent_id: row::uuid_bytes_opt(r, "parent_id", "mailboxes")?.map(MailboxId::from_bytes),
        uid_validity: UidValidity::new(row::u32_from(uid_validity, "uid_validity", "mailboxes")?),
        uid_next: Uid::new(row::u32_from(uid_next, "uid_next", "mailboxes")?)
            .ok_or_else(|| StorageError::corrupt("mailboxes", "uid_next is 0"))?,
        attributes: MailboxAttributes::from_csv(
            r.try_get::<String, _>("attributes")
                .map_err(StorageError::from)?
                .as_str(),
        ),
        subscribed: r
            .try_get::<i64, _>("subscribed")
            .map_err(StorageError::from)?
            != 0,
        created_at: Timestamp::from_micros(r.try_get("created_at").map_err(StorageError::from)?),
        highest_mod_seq: ModSeq::new(
            r.try_get::<i64, _>("highest_mod_seq")
                .map_err(StorageError::from)?
                .unsigned_abs(),
        )
        .ok_or_else(|| StorageError::corrupt("mailboxes", "highest_mod_seq is 0"))?,
    })
}
