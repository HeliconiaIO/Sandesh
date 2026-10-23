//! [`OutboxStore`] over `SQLite` — webhook endpoints and the durable outbox.
//!
//! The outbox is a table rather than an in-memory queue because the Phase 8 A2
//! acceptance criterion names restart survival explicitly (ADR-086). The worker
//! in `sandesh-webhook` holds no state between ticks, so every fact it needs to
//! resume is read back from here and a restart is indistinguishable from a slow
//! tick.
//!
//! [`enqueue_in_tx`] is the other half, and the more important one: it writes
//! the outbox rows inside the *delivery* transaction, so "stored" and "will be
//! notified" commit together. That mirrors what ADR-074 established for the
//! full-text index, and for the same reason — any gap leaves committed messages
//! that nothing downstream was told about.

use crate::{StorageError, row};
use async_trait::async_trait;
use sandesh_core::{
    CoreResult, EndpointId, OutboxEntry, OutboxId, OutboxStatus, OutboxStore, Timestamp,
    WebhookEndpoint, WebhookSecret,
};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

/// `SQLite`-backed webhook endpoints and outbox.
#[derive(Debug)]
pub struct SqliteOutboxStore {
    pool: SqlitePool,
}

impl SqliteOutboxStore {
    /// Creates a store over `pool`.
    #[must_use]
    pub const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// Enqueues one notification per enabled endpoint, inside an open transaction.
///
/// Called from the message-insert transaction rather than after it. An SMTP
/// client never waits on this: the write is a local insert, and the HTTP
/// request happens later in the worker (ADR-086).
///
/// Returns how many entries were enqueued. Zero endpoints means zero rows and
/// no error — webhooks are opt-in, and a server with none configured must
/// behave exactly as it did before Phase 8.
///
/// # Errors
/// Fails if the endpoint lookup or an insert fails, which aborts the delivery
/// transaction along with it. That is the intended coupling: a notification
/// that could not be queued must not be reported as a stored message.
pub async fn enqueue_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    payload: &str,
    now: Timestamp,
) -> Result<usize, StorageError> {
    let endpoints = sqlx::query("SELECT id FROM webhook_endpoints WHERE enabled = 1")
        .fetch_all(&mut **tx)
        .await
        .map_err(StorageError::from)?;

    for endpoint in &endpoints {
        let endpoint_id = row::uuid_bytes(endpoint, "id", "webhook_endpoints")?;
        sqlx::query(
            "INSERT INTO webhook_outbox
                (id, endpoint_id, payload, attempts, next_attempt_at, status,
                 last_error, created_at, updated_at)
             VALUES (?, ?, ?, 0, ?, ?, NULL, ?, ?)",
        )
        .bind(&OutboxId::new().as_bytes()[..])
        .bind(&endpoint_id[..])
        .bind(payload)
        // Due immediately: the first attempt is not a retry.
        .bind(now.as_micros())
        .bind(OutboxStatus::Pending.as_str())
        .bind(now.as_micros())
        .bind(now.as_micros())
        .execute(&mut **tx)
        .await
        .map_err(StorageError::from)?;
    }

    Ok(endpoints.len())
}

/// Decodes one `webhook_endpoints` row.
fn decode_endpoint(r: &sqlx::sqlite::SqliteRow) -> Result<WebhookEndpoint, StorageError> {
    let id = row::uuid_bytes(r, "id", "webhook_endpoints")?;
    let enabled: i64 = r.try_get("enabled").map_err(StorageError::from)?;
    let created_at: i64 = r.try_get("created_at").map_err(StorageError::from)?;

    let secret: Option<String> = r.try_get("secret").map_err(StorageError::from)?;

    Ok(WebhookEndpoint {
        id: EndpointId::from_uuid(Uuid::from_bytes(id)),
        url: r.try_get("url").map_err(StorageError::from)?,
        enabled: enabled != 0,
        created_at: Timestamp::from_micros(created_at),
        // An empty string is treated as absent: a column that round-tripped
        // through a form field should not produce an endpoint that signs every
        // payload with a zero-length key.
        secret: secret.filter(|s| !s.is_empty()).map(WebhookSecret::new),
    })
}

/// Decodes one `webhook_outbox` row.
fn decode_entry(r: &sqlx::sqlite::SqliteRow) -> Result<OutboxEntry, StorageError> {
    let id = row::uuid_bytes(r, "id", "webhook_outbox")?;
    let endpoint_id = row::uuid_bytes(r, "endpoint_id", "webhook_outbox")?;
    let attempts: i64 = r.try_get("attempts").map_err(StorageError::from)?;
    let next_attempt_at: i64 = r.try_get("next_attempt_at").map_err(StorageError::from)?;
    let created_at: i64 = r.try_get("created_at").map_err(StorageError::from)?;
    let updated_at: i64 = r.try_get("updated_at").map_err(StorageError::from)?;
    let status: String = r.try_get("status").map_err(StorageError::from)?;

    let status = OutboxStatus::parse(&status).ok_or_else(|| {
        StorageError::corrupt("webhook_outbox", format!("unknown status {status:?}"))
    })?;

    Ok(OutboxEntry {
        id: OutboxId::from_uuid(Uuid::from_bytes(id)),
        endpoint_id: EndpointId::from_uuid(Uuid::from_bytes(endpoint_id)),
        payload: r.try_get("payload").map_err(StorageError::from)?,
        attempts: row::u32_from(attempts, "attempts", "webhook_outbox")?,
        next_attempt_at: Timestamp::from_micros(next_attempt_at),
        status,
        last_error: r.try_get("last_error").map_err(StorageError::from)?,
        created_at: Timestamp::from_micros(created_at),
        updated_at: Timestamp::from_micros(updated_at),
    })
}

#[async_trait]
impl OutboxStore for SqliteOutboxStore {
    async fn register_endpoint(&self, endpoint: &WebhookEndpoint) -> CoreResult<()> {
        sqlx::query(
            "INSERT INTO webhook_endpoints (id, url, enabled, created_at, secret)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                url     = excluded.url,
                enabled = excluded.enabled,
                secret  = excluded.secret",
        )
        .bind(&endpoint.id.as_bytes()[..])
        .bind(endpoint.url.as_str())
        .bind(i64::from(endpoint.enabled))
        .bind(endpoint.created_at.as_micros())
        .bind(endpoint.secret.as_ref().map(WebhookSecret::expose))
        .execute(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(())
    }

    async fn endpoints(&self) -> CoreResult<Vec<WebhookEndpoint>> {
        let rows = sqlx::query("SELECT * FROM webhook_endpoints ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        Ok(rows
            .iter()
            .map(decode_endpoint)
            .collect::<Result<Vec<_>, _>>()?)
    }

    async fn remove_endpoint(&self, id: EndpointId) -> CoreResult<bool> {
        // The pending entries go with it, by the ON DELETE CASCADE the
        // migration declares. Foreign keys must be on for that to fire, which
        // `db.rs` sets per connection.
        let result = sqlx::query("DELETE FROM webhook_endpoints WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .execute(&self.pool)
            .await
            .map_err(StorageError::from)?;

        Ok(result.rows_affected() > 0)
    }

    async fn due(&self, now: Timestamp, limit: u32) -> CoreResult<Vec<OutboxEntry>> {
        let rows = sqlx::query(
            "SELECT * FROM webhook_outbox
             WHERE status = ? AND next_attempt_at <= ?
             ORDER BY next_attempt_at ASC
             LIMIT ?",
        )
        .bind(OutboxStatus::Pending.as_str())
        .bind(now.as_micros())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(rows
            .iter()
            .map(decode_entry)
            .collect::<Result<Vec<_>, _>>()?)
    }

    async fn record_attempt(
        &self,
        id: OutboxId,
        status: OutboxStatus,
        next_attempt_at: Timestamp,
        error: Option<&str>,
    ) -> CoreResult<()> {
        // The attempt count is incremented here rather than by the caller, so
        // the row and the retry policy cannot disagree about how many attempts
        // have happened -- `RetryPolicy::schedule` reads the count this write
        // produces.
        sqlx::query(
            "UPDATE webhook_outbox
                SET attempts        = attempts + 1,
                    status          = ?,
                    next_attempt_at = ?,
                    last_error      = ?,
                    updated_at      = ?
              WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(next_attempt_at.as_micros())
        .bind(error)
        .bind(Timestamp::now().as_micros())
        .bind(&id.as_bytes()[..])
        .execute(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(())
    }

    async fn recent(&self, limit: u32) -> CoreResult<Vec<OutboxEntry>> {
        let rows = sqlx::query("SELECT * FROM webhook_outbox ORDER BY created_at DESC LIMIT ?")
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        Ok(rows
            .iter()
            .map(decode_entry)
            .collect::<Result<Vec<_>, _>>()?)
    }
}
