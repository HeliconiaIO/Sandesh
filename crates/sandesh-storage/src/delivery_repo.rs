//! [`DeliveryStore`] over `SQLite` — the auditable record of each delivery.
//!
//! A delivery answers "why did this message land where it did?": the envelope
//! presented, how each recipient resolved, and what the transaction looked like.
//! It is the record the Diagnostics view (Phase 7) is built on.
//!
//! This is also where Phase 1's OD-16 is resolved. `Message` carries an
//! `Envelope` whose `rcpt_to` comes back empty from `MessageStore::get`,
//! because recipients are multi-valued and belong to the delivery, not to the
//! immutable message content. `DeliveryStore` is what supplies them.

use crate::{StorageError, events::EventRecorder, row};
use async_trait::async_trait;
use sandesh_core::{
    AccountId, ConnectionId, CoreResult, Delivery, DeliveryId, DeliveryOutcome, DeliverySource,
    DeliveryStore, EmailAddress, EntryId, Envelope, Keyword, MessageId, RecipientOutcome, RuleId,
    RuleMatch, Timestamp,
};
use sqlx::{Row, SqlitePool};

/// `SQLite`-backed delivery records.
#[derive(Debug)]
pub struct SqliteDeliveryStore {
    pool: SqlitePool,
    #[allow(dead_code)] // Wired for DeliveryFailed, which Phase 2 does not yet emit.
    events: EventRecorder,
}

impl SqliteDeliveryStore {
    /// Creates a store over `pool`.
    #[must_use]
    pub fn new(pool: SqlitePool, events: EventRecorder) -> Self {
        Self { pool, events }
    }
}

#[async_trait]
impl DeliveryStore for SqliteDeliveryStore {
    /// Records a delivery and its per-recipient outcomes atomically.
    ///
    /// The record and its recipients are one unit: a delivery listing some but
    /// not all of its recipients would be actively misleading in a diagnostics
    /// view, which is the only thing this data is for.
    async fn record(&self, delivery: &Delivery) -> CoreResult<()> {
        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;

        sqlx::query(
            "INSERT INTO deliveries
                (id, message_id, source, peer, mail_from, helo, received_at, duration_us,
                 connection_id, replay_of_message_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&delivery.id.as_bytes()[..])
        .bind(delivery.message_id.map(|id| id.as_bytes().to_vec()))
        .bind(delivery.source.as_str())
        .bind(delivery.envelope.peer.clone())
        .bind(
            delivery
                .envelope
                .mail_from
                .as_ref()
                .map(EmailAddress::normalized),
        )
        .bind(delivery.envelope.helo_domain.clone())
        .bind(delivery.received_at.as_micros())
        .bind(delivery.duration_us)
        .bind(delivery.connection_id.map(|id| id.as_bytes().to_vec()))
        .bind(delivery.replay_of.map(|id| id.as_bytes().to_vec()))
        .execute(&mut *tx)
        .await
        .map_err(StorageError::from)?;

        for outcome in &delivery.outcomes {
            sqlx::query(
                "INSERT INTO delivery_recipients
                    (delivery_id, rcpt_to, resolved_account_id, entry_id, outcome, reason)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&delivery.id.as_bytes()[..])
            .bind(outcome.rcpt_to.normalized())
            .bind(outcome.resolved_account.map(|id| id.as_bytes().to_vec()))
            .bind(outcome.entry_id.map(|id| id.as_bytes().to_vec()))
            .bind(outcome.outcome.as_str())
            .bind(outcome.reason.clone())
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;
        }

        // The rules that fired, in the same transaction as the record they
        // explain. A delivery claiming a tag whose explanation failed to write
        // would be exactly the unexplained keyword ADR-087 exists to prevent.
        for applied in &delivery.rules_applied {
            sqlx::query(
                "INSERT INTO delivery_rules (delivery_id, rule_id, rule_name, keyword)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&delivery.id.as_bytes()[..])
            .bind(&applied.rule_id.as_bytes()[..])
            .bind(applied.rule_name.clone())
            .bind(applied.keyword.as_str())
            .execute(&mut *tx)
            .await
            .map_err(StorageError::from)?;
        }

        tx.commit().await.map_err(StorageError::from)?;
        Ok(())
    }

    async fn get(&self, id: DeliveryId) -> CoreResult<Option<Delivery>> {
        let Some(r) = sqlx::query("SELECT * FROM deliveries WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?
        else {
            return Ok(None);
        };

        delivery_from_row(&self.pool, &r).await.map(Some)
    }

    async fn by_message_id(&self, message_id: MessageId) -> CoreResult<Option<Delivery>> {
        // Newest first: re-delivery of the same message is possible in
        // principle, and the most recent record is the one that explains the
        // message as it currently stands.
        let row = sqlx::query(
            "SELECT * FROM deliveries
             WHERE message_id = ?
             ORDER BY received_at DESC
             LIMIT 1",
        )
        .bind(&message_id.as_bytes()[..])
        .fetch_optional(&self.pool)
        .await
        .map_err(StorageError::from)?;

        let Some(r) = row else {
            return Ok(None);
        };
        delivery_from_row(&self.pool, &r).await.map(Some)
    }

    async fn recent(&self, limit: u32) -> CoreResult<Vec<Delivery>> {
        let rows = sqlx::query("SELECT * FROM deliveries ORDER BY received_at DESC LIMIT ?")
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        let mut deliveries = Vec::with_capacity(rows.len());
        for r in &rows {
            deliveries.push(delivery_from_row(&self.pool, r).await?);
        }
        Ok(deliveries)
    }
}

async fn delivery_from_row(pool: &SqlitePool, r: &sqlx::sqlite::SqliteRow) -> CoreResult<Delivery> {
    let id = DeliveryId::from_bytes(row::uuid_bytes(r, "id", "deliveries")?);
    let source: String = r.try_get("source").map_err(StorageError::from)?;
    let mail_from: Option<String> = r.try_get("mail_from").map_err(StorageError::from)?;

    let outcomes = recipients_for(pool, id).await?;
    let rules_applied = rules_for(pool, id).await?;

    Ok(Delivery {
        id,
        message_id: row::uuid_bytes_opt(r, "message_id", "deliveries")?.map(MessageId::from_bytes),
        source: DeliverySource::parse(&source)
            .ok_or_else(|| StorageError::corrupt("deliveries", format!("bad source {source}")))?,
        envelope: Envelope {
            mail_from: mail_from.as_deref().map(EmailAddress::parse_lenient),
            // The envelope's recipients are reconstructed from the outcome
            // rows: this is the one place the full RCPT TO list is preserved.
            rcpt_to: outcomes.iter().map(|o| o.rcpt_to.clone()).collect(),
            helo_domain: r.try_get("helo").map_err(StorageError::from)?,
            peer: r.try_get("peer").map_err(StorageError::from)?,
            tls: None,
            auth_identity: None,
        },
        outcomes,
        received_at: Timestamp::from_micros(r.try_get("received_at").map_err(StorageError::from)?),
        duration_us: r.try_get("duration_us").map_err(StorageError::from)?,
        connection_id: row::uuid_bytes_opt(r, "connection_id", "deliveries")?
            .map(ConnectionId::from_bytes),
        rules_applied,
        replay_of: row::uuid_bytes_opt(r, "replay_of_message_id", "deliveries")?
            .map(MessageId::from_bytes),
    })
}

/// The rules that fired for a delivery, in the order they were recorded.
///
/// A keyword that no longer validates is skipped rather than failing the whole
/// read: the row is history, and a delivery record that will not load because
/// of one malformed historical tag is worse than one missing that tag.
async fn rules_for(pool: &SqlitePool, delivery_id: DeliveryId) -> CoreResult<Vec<RuleMatch>> {
    let rows = sqlx::query(
        "SELECT rule_id, rule_name, keyword FROM delivery_rules
         WHERE delivery_id = ?
         ORDER BY rowid",
    )
    .bind(&delivery_id.as_bytes()[..])
    .fetch_all(pool)
    .await
    .map_err(StorageError::from)?;

    let mut applied = Vec::with_capacity(rows.len());
    for r in &rows {
        let keyword: String = r.try_get("keyword").map_err(StorageError::from)?;
        let Some(keyword) = Keyword::new(keyword) else {
            continue;
        };
        applied.push(RuleMatch {
            rule_id: RuleId::from_bytes(row::uuid_bytes(r, "rule_id", "delivery_rules")?),
            rule_name: r.try_get("rule_name").map_err(StorageError::from)?,
            keyword,
        });
    }
    Ok(applied)
}

async fn recipients_for(
    pool: &SqlitePool,
    delivery_id: DeliveryId,
) -> CoreResult<Vec<RecipientOutcome>> {
    let rows =
        sqlx::query("SELECT * FROM delivery_recipients WHERE delivery_id = ? ORDER BY rowid")
            .bind(&delivery_id.as_bytes()[..])
            .fetch_all(pool)
            .await
            .map_err(StorageError::from)?;

    rows.iter()
        .map(|r| {
            let rcpt: String = r.try_get("rcpt_to").map_err(StorageError::from)?;
            let outcome: String = r.try_get("outcome").map_err(StorageError::from)?;
            Ok(RecipientOutcome {
                rcpt_to: EmailAddress::parse_lenient(&rcpt),
                resolved_account: row::uuid_bytes_opt(
                    r,
                    "resolved_account_id",
                    "delivery_recipients",
                )?
                .map(AccountId::from_bytes),
                entry_id: row::uuid_bytes_opt(r, "entry_id", "delivery_recipients")?
                    .map(EntryId::from_bytes),
                outcome: DeliveryOutcome::parse(&outcome).ok_or_else(|| {
                    StorageError::corrupt("delivery_recipients", format!("bad outcome {outcome}"))
                })?,
                reason: r.try_get("reason").map_err(StorageError::from)?,
            })
        })
        .collect()
}
