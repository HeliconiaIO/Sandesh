//! [`MessageStore`] over `SQLite` — messages, placements, flags and raw bytes.

use crate::parsed_repo::{insert_parsed_structure, load_parsed};
use crate::{StorageError, blob, events::EventRecorder, row};
use async_trait::async_trait;
use sandesh_core::{
    Attachment, AttachmentId, BlobStore, CoreError, CoreResult, DeliverySource, EmailAddress,
    EntityKind, EntryId, Envelope, EventPayload, FlagUpdate, InsertedMessage, Keyword, MailboxId,
    Message, MessageEntry, MessageFlags, MessageId, MessageStore, NewMessage, PartPath, RawMessage,
    SystemFlags, Timestamp, Uid,
};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use std::collections::BTreeSet;
use std::sync::Arc;

/// `SQLite`-backed message storage.
#[derive(Debug)]
pub struct SqliteMessageStore {
    pool: SqlitePool,
    blobs: Arc<dyn BlobStore>,
    events: EventRecorder,
}

impl SqliteMessageStore {
    /// Creates a store over `pool`, using `blobs` for oversized messages.
    #[must_use]
    pub fn new(pool: SqlitePool, blobs: Arc<dyn BlobStore>, events: EventRecorder) -> Self {
        Self {
            pool,
            blobs,
            events,
        }
    }

    /// Begins a transaction holding the write lock from the outset.
    ///
    /// Required by any transaction that reads a row and then writes it; see
    /// `update_flags_conditional` for why such a transaction cannot be
    /// retried into success once it has begun as a reader.
    async fn begin_immediate(&self) -> CoreResult<Transaction<'_, Sqlite>> {
        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;
        // `sqlx` has already issued a deferred BEGIN; replace it in place.
        sqlx::query("ROLLBACK; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;
        Ok(tx)
    }
}

#[async_trait]
impl MessageStore for SqliteMessageStore {
    /// Persists a message and every requested placement in one transaction.
    ///
    /// Delivery is atomic per message (`CORE_SPEC.md` §7.5): if any placement
    /// fails, nothing is persisted. UIDs are allocated inside this transaction,
    /// which is what makes concurrent delivery to one mailbox safe.
    ///
    /// Raw bytes are written **before** parsed structure, and a message that
    /// failed to parse is stored regardless — malformed-message testing is the
    /// point of the product, so a parse failure must never lose mail.
    async fn insert(&self, message: &NewMessage) -> CoreResult<InsertedMessage> {
        let message_id = MessageId::new();
        let raw_bytes = message.raw.bytes();

        // Oversized messages go to the blob store first. Writing the object
        // before the row means a crash between the two leaves a reclaimable
        // orphan, never a row pointing at nothing (STORAGE_DESIGN §4).
        let (inline, blob_ref) = if blob::should_store_inline(raw_bytes.len()) {
            (Some(raw_bytes.to_vec()), None)
        } else {
            (None, Some(self.blobs.put(raw_bytes).await?))
        };

        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;

        let header = |name: &str| message.parsed.header(name).map(|h| h.decoded_value.clone());

        sqlx::query(
            "INSERT INTO messages
                (id, sha256, size, raw_inline, raw_blob_ref, received_at, source,
                 env_mail_from, env_helo, env_peer, env_tls, env_auth_ident,
                 hdr_subject, hdr_from, hdr_to, hdr_cc, hdr_bcc, hdr_date,
                 hdr_message_id, hdr_in_reply_to,
                 has_attachments, has_inline, parse_version, parse_ok)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&message_id.as_bytes()[..])
        .bind(&message.raw.sha256()[..])
        .bind(i64::try_from(message.raw.size()).unwrap_or(i64::MAX))
        .bind(inline)
        .bind(blob_ref)
        .bind(message.received_at.as_micros())
        .bind(message.source.as_str())
        .bind(
            message
                .envelope
                .mail_from
                .as_ref()
                .map(EmailAddress::normalized),
        )
        .bind(message.envelope.helo_domain.clone())
        .bind(message.envelope.peer.clone())
        .bind(message.envelope.tls.clone())
        .bind(message.envelope.auth_identity.clone())
        .bind(header("subject"))
        .bind(header("from"))
        .bind(header("to"))
        .bind(header("cc"))
        .bind(header("bcc"))
        .bind(Option::<i64>::None)
        .bind(header("message-id"))
        .bind(header("in-reply-to"))
        .bind(i64::from(
            message.parsed.attachments.iter().any(|a| !a.is_inline),
        ))
        .bind(i64::from(
            message.parsed.attachments.iter().any(|a| a.is_inline),
        ))
        .bind(i64::from(message.parsed.parse_version))
        .bind(i64::from(message.parsed.parse_ok))
        .execute(&mut *tx)
        .await
        .map_err(StorageError::from)?;

        insert_parsed_structure(&mut tx, message_id, &message.parsed).await?;

        // The full-text index is written in this transaction, not after it, so
        // "stored" and "indexed" are the same event. Any gap between them would
        // leave committed messages the index does not know about, and
        // `SqliteSearchIndex` must then refuse to narrow anything at all
        // (`STORAGE_DESIGN.md` §7).
        crate::search_index::index_in_tx(&mut tx, message_id, &message.parsed, &message.raw)
            .await?;

        let mut entries = Vec::with_capacity(message.placements.len());
        let mut owners = Vec::with_capacity(message.placements.len());
        for placement in &message.placements {
            let uid = crate::mailbox_repo::allocate_uid(&mut tx, placement.mailbox_id).await?;
            // A new placement is a change to the mailbox, so it takes a fresh
            // sequence rather than the mailbox's floor: a client that has
            // already synced to `n` must see the arrival as "> n".
            let mod_seq =
                crate::mailbox_repo::allocate_mod_seq(&mut tx, placement.mailbox_id).await?;
            owners.push(mailbox_owner(&mut tx, placement.mailbox_id).await?);
            let entry = MessageEntry {
                id: EntryId::new(),
                message_id,
                mailbox_id: placement.mailbox_id,
                uid,
                flags: placement.flags.clone(),
                internal_date: placement.internal_date,
                added_at: message.received_at,
                mod_seq,
            };
            insert_entry(&mut tx, &entry).await?;
            entries.push(entry);
        }

        // Webhook notifications are enqueued in this transaction, not after it
        // (ADR-086). "Stored" and "will be notified" therefore commit together,
        // exactly as the full-text index above does: a gap either way would
        // leave committed messages that nothing downstream was ever told about.
        enqueue_notifications(&mut tx, message, message_id, &entries, &owners).await?;

        tx.commit().await.map_err(StorageError::from)?;

        // Events are emitted only after commit, so a consumer woken by
        // MessageReceived can never race an empty read (EVENT_MODEL §5).
        for (entry, account_id) in entries.iter().zip(owners) {
            self.events.emit(EventPayload::EntryCreated {
                entry_id: entry.id,
                message_id,
                mailbox_id: entry.mailbox_id,
                uid: entry.uid,
            });
            self.events.emit(EventPayload::MessageReceived {
                message_id,
                entry_id: entry.id,
                mailbox_id: entry.mailbox_id,
                account_id,
                uid: entry.uid,
                size: message.raw.size(),
            });
        }

        Ok(InsertedMessage {
            message_id,
            entries,
        })
    }

    async fn get(&self, id: MessageId) -> CoreResult<Option<Message>> {
        let Some(r) = sqlx::query("SELECT * FROM messages WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?
        else {
            return Ok(None);
        };

        let raw = self.load_raw(&r).await?;
        let parsed = load_parsed(&self.pool, id).await?;

        let source: String = r.try_get("source").map_err(StorageError::from)?;
        Ok(Some(Message {
            id,
            raw,
            parsed,
            envelope: Envelope {
                mail_from: r
                    .try_get::<Option<String>, _>("env_mail_from")
                    .map_err(StorageError::from)?
                    .as_deref()
                    .map(EmailAddress::parse_lenient),
                rcpt_to: Vec::new(), // recorded per-delivery, not on the message
                helo_domain: r.try_get("env_helo").map_err(StorageError::from)?,
                peer: r.try_get("env_peer").map_err(StorageError::from)?,
                tls: r.try_get("env_tls").map_err(StorageError::from)?,
                auth_identity: r.try_get("env_auth_ident").map_err(StorageError::from)?,
            },
            source: DeliverySource::parse(&source)
                .ok_or_else(|| StorageError::corrupt("messages", format!("bad source {source}")))?,
            received_at: Timestamp::from_micros(
                r.try_get("received_at").map_err(StorageError::from)?,
            ),
        }))
    }

    /// Loads only the raw bytes.
    ///
    /// Kept separate from [`get`](MessageStore::get) because IMAP `FETCH
    /// BODY[]` and POP3 `RETR` want the octets without rebuilding parsed
    /// structure.
    async fn raw(&self, id: MessageId) -> CoreResult<Option<RawMessage>> {
        let Some(r) =
            sqlx::query("SELECT sha256, raw_inline, raw_blob_ref FROM messages WHERE id = ?")
                .bind(&id.as_bytes()[..])
                .fetch_optional(&self.pool)
                .await
                .map_err(StorageError::from)?
        else {
            return Ok(None);
        };
        self.load_raw(&r).await.map(Some)
    }

    async fn get_entry(&self, id: EntryId) -> CoreResult<Option<MessageEntry>> {
        let Some(r) = sqlx::query("SELECT * FROM message_entries WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?
        else {
            return Ok(None);
        };
        entry_from_row(&self.pool, &r).await.map(Some)
    }

    async fn get_entry_by_uid(
        &self,
        mailbox_id: MailboxId,
        uid: Uid,
    ) -> CoreResult<Option<MessageEntry>> {
        let Some(r) = sqlx::query("SELECT * FROM message_entries WHERE mailbox_id = ? AND uid = ?")
            .bind(&mailbox_id.as_bytes()[..])
            .bind(i64::from(uid.get()))
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?
        else {
            return Ok(None);
        };
        entry_from_row(&self.pool, &r).await.map(Some)
    }

    async fn list_entries(&self, mailbox_id: MailboxId) -> CoreResult<Vec<MessageEntry>> {
        let rows = sqlx::query("SELECT * FROM message_entries WHERE mailbox_id = ? ORDER BY uid")
            .bind(&mailbox_id.as_bytes()[..])
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        let mut entries = Vec::with_capacity(rows.len());
        for r in &rows {
            entries.push(entry_from_row(&self.pool, r).await?);
        }
        Ok(entries)
    }

    async fn entries_for_message(&self, message_id: MessageId) -> CoreResult<Vec<MessageEntry>> {
        let rows = sqlx::query("SELECT * FROM message_entries WHERE message_id = ?")
            .bind(&message_id.as_bytes()[..])
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        let mut entries = Vec::with_capacity(rows.len());
        for r in &rows {
            entries.push(entry_from_row(&self.pool, r).await?);
        }
        Ok(entries)
    }

    /// The message list's page, in SQL rather than in memory.
    ///
    /// The default implementation reads every placement in scope and sorts in
    /// Rust: 2.6 s and ~170 MB per page at 100,000 messages, paid again on every
    /// page. Here the grouping, ordering and limit run over the entries table,
    /// and only the page's placements are decoded.
    async fn page_entries(
        &self,
        mailboxes: &[MailboxId],
        before_uid: Option<u32>,
        limit: usize,
    ) -> CoreResult<sandesh_core::EntryPage> {
        if mailboxes.is_empty() {
            return Ok(sandesh_core::EntryPage::default());
        }
        // The scope travels as one JSON array of hex ids, so the statement is
        // static whatever the number of mailboxes; `unhex` turns each back into
        // the BLOB key, which keeps the `mailbox_id` indexes usable.
        let scope = serde_json::to_string(
            &mailboxes
                .iter()
                .map(|mailbox| hex::encode(mailbox.as_bytes()))
                .collect::<Vec<_>>(),
        )
        .map_err(|error| CoreError::Storage(sandesh_core::StoreError::new(error.to_string())))?;
        let cursor = before_uid.map(i64::from);

        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT message_id) FROM message_entries
             WHERE mailbox_id IN (SELECT unhex(value) FROM json_each(?1))
               AND (?2 IS NULL OR uid < ?2)",
        )
        .bind(&scope)
        .bind(cursor)
        .fetch_one(&self.pool)
        .await
        .map_err(StorageError::from)?;

        // Ties on `added_at` fall back to the id, ascending, which is the order
        // the reference implementation's `BTreeMap` leaves them in.
        let rows = sqlx::query(
            "SELECT message_id, MAX(added_at) AS newest FROM message_entries
             WHERE mailbox_id IN (SELECT unhex(value) FROM json_each(?1))
               AND (?2 IS NULL OR uid < ?2)
             GROUP BY message_id
             ORDER BY newest DESC, message_id ASC
             LIMIT ?3",
        )
        .bind(&scope)
        .bind(cursor)
        .bind(i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;

        let has_more = rows.len() > limit;
        let mut messages = Vec::with_capacity(limit.min(rows.len()));
        for r in rows.iter().take(limit) {
            let message_id =
                MessageId::from_bytes(row::uuid_bytes(r, "message_id", "message_entries")?);

            let mut entries = Vec::new();
            for entry in self.entries_for_message(message_id).await? {
                let admitted = mailboxes.contains(&entry.mailbox_id)
                    && before_uid.is_none_or(|uid| entry.uid.get() < uid);
                if admitted {
                    entries.push(entry);
                }
            }
            // Placements in the order the reference yields them: by the
            // mailbox's position in scope, then by UID.
            entries.sort_by_key(|entry| {
                (
                    mailboxes.iter().position(|m| *m == entry.mailbox_id),
                    entry.uid,
                )
            });
            messages.push((message_id, entries));
        }

        Ok(sandesh_core::EntryPage {
            messages,
            has_more,
            total: u64::try_from(total).unwrap_or_default(),
        })
    }

    /// Places an existing message in another mailbox — IMAP `COPY`.
    ///
    /// The new placement gets a **new UID** from the target mailbox and a copy
    /// of the source flags. The message itself is shared: its bytes are not
    /// duplicated, and the two placements' flags are thereafter independent.
    async fn copy_entry(
        &self,
        entry_id: EntryId,
        target_mailbox: MailboxId,
    ) -> CoreResult<MessageEntry> {
        let source = self
            .get_entry(entry_id)
            .await?
            .ok_or_else(|| CoreError::not_found(EntityKind::Entry, entry_id))?;

        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;
        let uid = crate::mailbox_repo::allocate_uid(&mut tx, target_mailbox).await?;
        let mod_seq = crate::mailbox_repo::allocate_mod_seq(&mut tx, target_mailbox).await?;

        let entry = MessageEntry {
            id: EntryId::new(),
            message_id: source.message_id,
            mailbox_id: target_mailbox,
            uid,
            flags: source.flags.clone(),
            internal_date: source.internal_date,
            added_at: Timestamp::now(),
            mod_seq,
        };
        insert_entry(&mut tx, &entry).await?;
        tx.commit().await.map_err(StorageError::from)?;

        self.events.emit(EventPayload::EntryCreated {
            entry_id: entry.id,
            message_id: entry.message_id,
            mailbox_id: entry.mailbox_id,
            uid: entry.uid,
        });
        Ok(entry)
    }

    /// Moves an existing placement to another mailbox — IMAP `MOVE`.
    ///
    /// Atomically creates a new placement in `target_mailbox` with a new UID,
    /// removes the source placement, and emits both `EntryDeleted` and `EntryCreated`
    /// events so clients update live.
    async fn move_entry(
        &self,
        entry_id: EntryId,
        target_mailbox: MailboxId,
    ) -> CoreResult<MessageEntry> {
        let source = self
            .get_entry(entry_id)
            .await?
            .ok_or_else(|| CoreError::not_found(EntityKind::Entry, entry_id))?;

        if source.mailbox_id == target_mailbox {
            return Ok(source);
        }

        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;
        let uid = crate::mailbox_repo::allocate_uid(&mut tx, target_mailbox).await?;
        let mod_seq = crate::mailbox_repo::allocate_mod_seq(&mut tx, target_mailbox).await?;

        let new_entry = MessageEntry {
            id: EntryId::new(),
            message_id: source.message_id,
            mailbox_id: target_mailbox,
            uid,
            flags: source.flags.clone(),
            internal_date: source.internal_date,
            added_at: Timestamp::now(),
            mod_seq,
        };
        insert_entry(&mut tx, &new_entry).await?;

        sqlx::query("DELETE FROM message_entries WHERE id = ?")
            .bind(&entry_id.as_bytes()[..])
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;

        tx.commit().await.map_err(StorageError::from)?;

        self.events.emit(EventPayload::EntryDeleted {
            entry_id,
            message_id: source.message_id,
            mailbox_id: source.mailbox_id,
            uid: source.uid,
        });
        self.events.emit(EventPayload::EntryCreated {
            entry_id: new_entry.id,
            message_id: new_entry.message_id,
            mailbox_id: new_entry.mailbox_id,
            uid: new_entry.uid,
        });

        Ok(new_entry)
    }

    /// Updates one placement's flags — IMAP `STORE`.
    ///
    /// Touches only the named placement. Other placements of the same message,
    /// in other mailboxes, are unaffected.
    ///
    /// Delegates to [`update_flags_conditional`](Self::update_flags_conditional)
    /// with no precondition, so there is exactly one flag-mutation
    /// implementation and the modseq stamping cannot differ between the two
    /// entry points.
    async fn update_flags(
        &self,
        entry_id: EntryId,
        update: FlagUpdate,
    ) -> CoreResult<MessageFlags> {
        self.update_flags_conditional(entry_id, update, None)
            .await
            .map(|outcome| outcome.flags)
    }

    /// Updates one placement's flags under an optional `UNCHANGEDSINCE`
    /// precondition — IMAP `STORE` with CONDSTORE (RFC 7162 §3.2).
    ///
    /// # Why the check is inside the transaction
    ///
    /// The precondition and the write are one unit. Reading the modseq,
    /// deciding, then writing would leave a window in which another session
    /// stores flags between the two — exactly the lost update `UNCHANGEDSINCE`
    /// exists to prevent, and the client would be told it succeeded.
    ///
    /// # Why a no-op allocates nothing
    ///
    /// RFC 7162 §3.1.11 notes that advancing the sequence when nothing changed
    /// forces every other client to re-fetch a message that is identical.
    /// Comparing the flags before and after the operation — rather than
    /// assuming the write changed something — is what keeps `HIGHESTMODSEQ`
    /// meaningful.
    async fn update_flags_conditional(
        &self,
        entry_id: EntryId,
        update: FlagUpdate,
        unchanged_since: Option<sandesh_core::ModSeq>,
    ) -> CoreResult<sandesh_core::FlagOutcome> {
        // `BEGIN IMMEDIATE`, not the default deferred transaction. This method
        // reads the entry, decides, then writes, so a deferred transaction
        // would begin as a *reader* and have to upgrade to a writer at the
        // UPDATE. `SQLite` refuses that upgrade with `SQLITE_BUSY` as soon as
        // another writer holds the lock, and — unlike ordinary contention —
        // `busy_timeout` does not wait it out, because two readers both
        // wanting to upgrade can never resolve by waiting. Taking the write
        // lock up front turns a hard failure into a queue.
        let mut tx = self.begin_immediate().await?;

        let r =
            sqlx::query("SELECT flags, mailbox_id, uid, mod_seq FROM message_entries WHERE id = ?")
                .bind(&entry_id.as_bytes()[..])
                .fetch_optional(&mut *tx)
                .await
                .map_err(StorageError::from)?
                .ok_or_else(|| CoreError::not_found(EntityKind::Entry, entry_id))?;

        let current_bits: i64 = r.try_get("flags").map_err(StorageError::from)?;
        let current_mod_seq: i64 = r.try_get("mod_seq").map_err(StorageError::from)?;
        let current_mod_seq = sandesh_core::ModSeq::new(current_mod_seq.unsigned_abs())
            .ok_or_else(|| StorageError::corrupt("message_entries", "mod_seq is 0"))?;
        let mailbox_id =
            MailboxId::from_bytes(row::uuid_bytes(&r, "mailbox_id", "message_entries")?);
        let uid_value: i64 = r.try_get("uid").map_err(StorageError::from)?;
        let uid = Uid::new(row::u32_from(uid_value, "uid", "message_entries")?)
            .ok_or_else(|| StorageError::corrupt("message_entries", "uid is 0"))?;

        let current_keywords = load_keywords(&mut tx, entry_id).await?;
        let before = MessageFlags::new(
            SystemFlags::from_bits_truncate(u8::try_from(current_bits).unwrap_or(0)),
            current_keywords,
        );

        // The precondition, before anything is written. RFC 7162 §3.2 defines
        // the failure as "the entry changed since the client's value", i.e.
        // strictly greater — equality means the client is looking at exactly
        // this revision and may proceed.
        if unchanged_since.is_some_and(|limit| current_mod_seq > limit) {
            tx.rollback().await.map_err(StorageError::from)?;
            return Ok(sandesh_core::FlagOutcome {
                flags: before,
                mod_seq: current_mod_seq,
                status: sandesh_core::FlagChangeStatus::Failed,
            });
        }

        let mut flags = before.clone();
        match update {
            FlagUpdate::Add { system, keywords } => flags.add(system, keywords),
            FlagUpdate::Remove { system, keywords } => flags.remove(system, &keywords),
            FlagUpdate::Replace { system, keywords } => flags.replace(system, keywords),
        }

        // Nothing to do, and therefore nothing to stamp. Committing an
        // unchanged row would still be correct, but allocating a sequence for
        // it would not (RFC 7162 §3.1.11).
        if flags == before {
            tx.rollback().await.map_err(StorageError::from)?;
            return Ok(sandesh_core::FlagOutcome {
                flags: before,
                mod_seq: current_mod_seq,
                status: sandesh_core::FlagChangeStatus::Unchanged,
            });
        }

        let mod_seq = crate::mailbox_repo::allocate_mod_seq(&mut tx, mailbox_id).await?;

        sqlx::query("UPDATE message_entries SET flags = ?, mod_seq = ? WHERE id = ?")
            .bind(i64::from(flags.system().bits()))
            .bind(i64::try_from(mod_seq.get()).unwrap_or(i64::MAX))
            .bind(&entry_id.as_bytes()[..])
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;

        sqlx::query("DELETE FROM entry_keywords WHERE entry_id = ?")
            .bind(&entry_id.as_bytes()[..])
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;
        for keyword in flags.keywords() {
            sqlx::query("INSERT INTO entry_keywords (entry_id, keyword) VALUES (?, ?)")
                .bind(&entry_id.as_bytes()[..])
                .bind(keyword.as_str())
                .execute(&mut *tx)
                .await
                .map_err(StorageError::from)?;
        }

        tx.commit().await.map_err(StorageError::from)?;

        let before_names = before.all_names();
        let after_names = flags.all_names();
        self.events.emit(EventPayload::EntryFlagsChanged {
            entry_id,
            mailbox_id,
            uid,
            added: after_names
                .iter()
                .filter(|n| !before_names.contains(n))
                .cloned()
                .collect(),
            removed: before_names
                .iter()
                .filter(|n| !after_names.contains(n))
                .cloned()
                .collect(),
            flags_now: after_names,
            mod_seq,
        });

        Ok(sandesh_core::FlagOutcome {
            flags,
            mod_seq,
            status: sandesh_core::FlagChangeStatus::Changed,
        })
    }

    /// Removes one placement. The message survives if others remain.
    async fn delete_entry(&self, entry_id: EntryId) -> CoreResult<bool> {
        let Some(entry) = self.get_entry(entry_id).await? else {
            return Ok(false);
        };

        let affected = sqlx::query("DELETE FROM message_entries WHERE id = ?")
            .bind(&entry_id.as_bytes()[..])
            .execute(&self.pool)
            .await
            .map_err(StorageError::from)?
            .rows_affected();

        if affected > 0 {
            self.events.emit(EventPayload::EntryDeleted {
                entry_id,
                message_id: entry.message_id,
                mailbox_id: entry.mailbox_id,
                uid: entry.uid,
            });
        }
        Ok(affected > 0)
    }

    /// Deletes messages no placement references.
    ///
    /// Run after expunge operations rather than inside the delivery path.
    /// Blob objects are removed after their rows, so a crash mid-sweep leaves a
    /// reclaimable orphan rather than a dangling reference.
    async fn collect_orphans(&self) -> CoreResult<Vec<MessageId>> {
        let rows = sqlx::query(
            "SELECT id, raw_blob_ref FROM messages
             WHERE id NOT IN (SELECT message_id FROM message_entries)",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;

        let mut collected = Vec::with_capacity(rows.len());
        for r in &rows {
            let id = MessageId::from_bytes(row::uuid_bytes(r, "id", "messages")?);
            let blob_ref: Option<String> = r.try_get("raw_blob_ref").map_err(StorageError::from)?;

            // The index row is removed in the same transaction as the message.
            // `messages_fts_map` cascades, but `messages_fts` is a virtual
            // table and cannot carry a foreign key, so without this its rows
            // outlive their messages: harmless for correctness, since every
            // candidate is re-checked, but unbounded growth in a table nothing
            // else prunes.
            let mut tx = self.pool.begin().await.map_err(StorageError::from)?;
            crate::search_index::remove_in_tx(&mut tx, id).await?;
            sqlx::query("DELETE FROM messages WHERE id = ?")
                .bind(&id.as_bytes()[..])
                .execute(&mut *tx)
                .await
                .map_err(StorageError::from)?;
            tx.commit().await.map_err(StorageError::from)?;

            if let Some(reference) = blob_ref {
                self.blobs.delete(&reference).await?;
            }

            collected.push(id);
            self.events.emit(EventPayload::MessageDeleted {
                message_id: id,
                mailbox_ids: Vec::new(),
            });
        }
        Ok(collected)
    }

    async fn attachments(&self, message_id: MessageId) -> CoreResult<Vec<Attachment>> {
        let rows = sqlx::query("SELECT * FROM attachments WHERE message_id = ? ORDER BY part_path")
            .bind(&message_id.as_bytes()[..])
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        rows.iter()
            .map(|r| {
                let path: String = r.try_get("part_path").map_err(StorageError::from)?;
                Ok(Attachment {
                    id: AttachmentId::from_bytes(row::uuid_bytes(r, "id", "attachments")?),
                    part_path: PartPath::parse(&path).unwrap_or_else(PartPath::root),
                    filename: r.try_get("filename").map_err(StorageError::from)?,
                    content_type: r.try_get("content_type").map_err(StorageError::from)?,
                    content_id: r.try_get("content_id").map_err(StorageError::from)?,
                    size: r
                        .try_get::<i64, _>("size")
                        .map_err(StorageError::from)?
                        .unsigned_abs(),
                    is_inline: r
                        .try_get::<i64, _>("is_inline")
                        .map_err(StorageError::from)?
                        != 0,
                })
            })
            .collect()
    }

    async fn get_attachment(
        &self,
        id: AttachmentId,
    ) -> CoreResult<Option<(Attachment, MessageId)>> {
        let row = sqlx::query("SELECT * FROM attachments WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?;

        let Some(r) = row else {
            return Ok(None);
        };

        let message_id = MessageId::from_bytes(row::uuid_bytes(&r, "message_id", "attachments")?);
        let path: String = r.try_get("part_path").map_err(StorageError::from)?;
        let attachment = Attachment {
            id,
            part_path: PartPath::parse(&path).unwrap_or_else(PartPath::root),
            filename: r.try_get("filename").map_err(StorageError::from)?,
            content_type: r.try_get("content_type").map_err(StorageError::from)?,
            content_id: r.try_get("content_id").map_err(StorageError::from)?,
            size: r
                .try_get::<i64, _>("size")
                .map_err(StorageError::from)?
                .unsigned_abs(),
            is_inline: r
                .try_get::<i64, _>("is_inline")
                .map_err(StorageError::from)?
                != 0,
        };

        Ok(Some((attachment, message_id)))
    }
}

impl SqliteMessageStore {
    /// Reassembles raw bytes from inline storage or the blob store.
    async fn load_raw(&self, r: &sqlx::sqlite::SqliteRow) -> CoreResult<RawMessage> {
        let sha256 = row::digest_bytes(r, "sha256", "messages")?;
        let inline: Option<Vec<u8>> = r.try_get("raw_inline").map_err(StorageError::from)?;
        let blob_ref: Option<String> = r.try_get("raw_blob_ref").map_err(StorageError::from)?;

        let bytes = match (inline, blob_ref) {
            (Some(bytes), _) => bytes,
            (None, Some(reference)) => self.blobs.get(&reference).await?.ok_or_else(|| {
                CoreError::from(StorageError::Blob(format!("object {reference} is missing")))
            })?,
            (None, None) => {
                return Err(StorageError::corrupt("messages", "no raw bytes stored").into());
            }
        };

        Ok(RawMessage::from_parts(bytes, sha256))
    }
}

/// The account that owns `mailbox_id`.
///
/// Needed so `MessageReceived` can name the account without a second round
/// trip after the transaction commits.
async fn mailbox_owner(
    tx: &mut Transaction<'_, Sqlite>,
    mailbox_id: MailboxId,
) -> CoreResult<sandesh_core::AccountId> {
    let r = sqlx::query("SELECT account_id FROM mailboxes WHERE id = ?")
        .bind(&mailbox_id.as_bytes()[..])
        .fetch_optional(&mut **tx)
        .await
        .map_err(StorageError::from)?
        .ok_or_else(|| CoreError::not_found(EntityKind::Mailbox, mailbox_id))?;

    Ok(sandesh_core::AccountId::from_bytes(row::uuid_bytes(
        &r,
        "account_id",
        "mailboxes",
    )?))
}

async fn insert_entry(tx: &mut Transaction<'_, Sqlite>, entry: &MessageEntry) -> CoreResult<()> {
    let result = sqlx::query(
        "INSERT INTO message_entries
            (id, message_id, mailbox_id, uid, flags, internal_date, added_at, mod_seq)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&entry.id.as_bytes()[..])
    .bind(&entry.message_id.as_bytes()[..])
    .bind(&entry.mailbox_id.as_bytes()[..])
    .bind(i64::from(entry.uid.get()))
    .bind(i64::from(entry.flags.system().bits()))
    .bind(entry.internal_date.as_micros())
    .bind(entry.added_at.as_micros())
    .bind(i64::try_from(entry.mod_seq.get()).unwrap_or(i64::MAX))
    .execute(&mut **tx)
    .await;

    if let Err(e) = result {
        let err = StorageError::from(e);
        return Err(if err.is_unique_violation() {
            // Either a UID collision or a repeat placement in the same mailbox;
            // both are conflicts rather than opaque failures.
            CoreError::Conflict {
                reason: format!(
                    "message is already placed in mailbox {} or uid {} is taken",
                    entry.mailbox_id, entry.uid
                ),
            }
        } else {
            err.into()
        });
    }

    for keyword in entry.flags.keywords() {
        sqlx::query("INSERT INTO entry_keywords (entry_id, keyword) VALUES (?, ?)")
            .bind(&entry.id.as_bytes()[..])
            .bind(keyword.as_str())
            .execute(&mut **tx)
            .await
            .map_err(StorageError::from)?;
    }
    Ok(())
}

async fn load_keywords(
    tx: &mut Transaction<'_, Sqlite>,
    entry_id: EntryId,
) -> CoreResult<BTreeSet<Keyword>> {
    let rows = sqlx::query("SELECT keyword FROM entry_keywords WHERE entry_id = ?")
        .bind(&entry_id.as_bytes()[..])
        .fetch_all(&mut **tx)
        .await
        .map_err(StorageError::from)?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String, _>("keyword").ok())
        .filter_map(Keyword::new)
        .collect())
}

async fn entry_from_row(
    pool: &SqlitePool,
    r: &sqlx::sqlite::SqliteRow,
) -> CoreResult<MessageEntry> {
    let entry_id = EntryId::from_bytes(row::uuid_bytes(r, "id", "message_entries")?);
    let flag_bits: i64 = r.try_get("flags").map_err(StorageError::from)?;
    let uid_value: i64 = r.try_get("uid").map_err(StorageError::from)?;
    let mod_seq: i64 = r.try_get("mod_seq").map_err(StorageError::from)?;

    let keyword_rows = sqlx::query("SELECT keyword FROM entry_keywords WHERE entry_id = ?")
        .bind(&entry_id.as_bytes()[..])
        .fetch_all(pool)
        .await
        .map_err(StorageError::from)?;
    let keywords: BTreeSet<Keyword> = keyword_rows
        .iter()
        .filter_map(|k| k.try_get::<String, _>("keyword").ok())
        .filter_map(Keyword::new)
        .collect();

    Ok(MessageEntry {
        id: entry_id,
        message_id: MessageId::from_bytes(row::uuid_bytes(r, "message_id", "message_entries")?),
        mailbox_id: MailboxId::from_bytes(row::uuid_bytes(r, "mailbox_id", "message_entries")?),
        uid: Uid::new(row::u32_from(uid_value, "uid", "message_entries")?)
            .ok_or_else(|| StorageError::corrupt("message_entries", "uid is 0"))?,
        flags: MessageFlags::new(
            SystemFlags::from_bits_truncate(u8::try_from(flag_bits).unwrap_or(0)),
            keywords,
        ),
        internal_date: Timestamp::from_micros(
            r.try_get("internal_date").map_err(StorageError::from)?,
        ),
        added_at: Timestamp::from_micros(r.try_get("added_at").map_err(StorageError::from)?),
        mod_seq: sandesh_core::ModSeq::new(mod_seq.unsigned_abs())
            .ok_or_else(|| StorageError::corrupt("message_entries", "mod_seq is 0"))?,
    })
}

/// Queues one webhook notification per placement, inside the delivery
/// transaction.
///
/// One entry per placement because `MessageReceived` is emitted per placement;
/// collapsing them would invent an aggregate the event bus does not have.
/// Sending happens later, in the worker -- an SMTP client must never wait on a
/// consumer's endpoint (ADR-086).
async fn enqueue_notifications(
    tx: &mut Transaction<'_, Sqlite>,
    message: &NewMessage,
    message_id: MessageId,
    entries: &[MessageEntry],
    owners: &[sandesh_core::AccountId],
) -> Result<(), StorageError> {
    for (entry, account_id) in entries.iter().zip(owners) {
        let payload = serde_json::json!({
            "event": "MessageReceived",
            "message_id": message_id.to_string(),
            "entry_id": entry.id.to_string(),
            "mailbox_id": entry.mailbox_id.to_string(),
            "account_id": account_id.to_string(),
            "uid": entry.uid.get(),
            "size": message.raw.size(),
            "received_at": message.received_at.as_micros(),
        })
        .to_string();

        crate::outbox_repo::enqueue_in_tx(tx, &payload, message.received_at).await?;
    }

    Ok(())
}
