//! [`ConnectionStore`] over `SQLite` — per-connection protocol transcripts.
//!
//! One row per accepted socket (ADR-066). Separate from `deliveries` because a
//! connection is not a delivery: one connection can produce many deliveries or
//! none, and IMAP and POP3 produce none at all.

use crate::{StorageError, row};
use async_trait::async_trait;
use sandesh_core::{
    ConnectionDisposition, ConnectionId, ConnectionRecord, ConnectionStore, CoreResult, Protocol,
    Timestamp,
};
use sqlx::{Row, SqlitePool};

/// The transcript's line separator on disk.
///
/// Lines are stored as one text blob rather than a child table: a transcript is
/// read whole or not at all, and a row per line would multiply the write cost
/// of every connection by its length for no query anyone makes.
const LINE_SEPARATOR: char = '\n';

/// `SQLite`-backed connection records.
#[derive(Debug)]
pub struct SqliteConnectionStore {
    pool: SqlitePool,
}

impl SqliteConnectionStore {
    /// Creates a store over `pool`.
    #[must_use]
    pub const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ConnectionStore for SqliteConnectionStore {
    async fn record(&self, connection: &ConnectionRecord) -> CoreResult<()> {
        // Upsert: a connection is recorded when it closes, but a long-lived
        // session may be written once while open and again on close.
        sqlx::query(
            "INSERT INTO connections
                (id, protocol, peer, encrypted, opened_at, closed_at, disposition,
                 transcript, truncated)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                encrypted   = excluded.encrypted,
                closed_at   = excluded.closed_at,
                disposition = excluded.disposition,
                transcript  = excluded.transcript,
                truncated   = excluded.truncated",
        )
        .bind(&connection.id.as_bytes()[..])
        .bind(connection.protocol.as_str())
        .bind(connection.peer.clone())
        .bind(i64::from(connection.encrypted))
        .bind(connection.opened_at.as_micros())
        .bind(connection.closed_at.map(Timestamp::as_micros))
        .bind(connection.disposition.map(ConnectionDisposition::as_str))
        .bind(connection.transcript.join(&LINE_SEPARATOR.to_string()))
        .bind(i64::from(connection.truncated))
        .execute(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(())
    }

    async fn get(&self, id: ConnectionId) -> CoreResult<Option<ConnectionRecord>> {
        let Some(r) = sqlx::query("SELECT * FROM connections WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .fetch_optional(&self.pool)
            .await
            .map_err(StorageError::from)?
        else {
            return Ok(None);
        };

        connection_from_row(&r).map(Some)
    }

    async fn recent(
        &self,
        protocol: Option<Protocol>,
        limit: u32,
    ) -> CoreResult<Vec<ConnectionRecord>> {
        // Two statements rather than one with `(?1 IS NULL OR protocol = ?1)`:
        // the parameterised form defeats the partial index on
        // (protocol, opened_at), and this listing is the trace viewer's
        // main query.
        let rows = match protocol {
            Some(protocol) => {
                sqlx::query(
                    "SELECT * FROM connections
                 WHERE protocol = ?
                 ORDER BY opened_at DESC
                 LIMIT ?",
                )
                .bind(protocol.as_str())
                .bind(i64::from(limit))
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query("SELECT * FROM connections ORDER BY opened_at DESC LIMIT ?")
                    .bind(i64::from(limit))
                    .fetch_all(&self.pool)
                    .await
            }
        }
        .map_err(StorageError::from)?;

        rows.iter().map(connection_from_row).collect()
    }

    async fn prune(&self, keep: u32) -> CoreResult<u64> {
        // Ordered by `opened_at`, matching the listing, so what survives a
        // prune is exactly what the viewer would have shown.
        let result = sqlx::query(
            "DELETE FROM connections
             WHERE id NOT IN (
                 SELECT id FROM connections ORDER BY opened_at DESC LIMIT ?
             )",
        )
        .bind(i64::from(keep))
        .execute(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(result.rows_affected())
    }
}

fn connection_from_row(r: &sqlx::sqlite::SqliteRow) -> CoreResult<ConnectionRecord> {
    let protocol: String = r.try_get("protocol").map_err(StorageError::from)?;
    let disposition: Option<String> = r.try_get("disposition").map_err(StorageError::from)?;
    let transcript: Option<String> = r.try_get("transcript").map_err(StorageError::from)?;
    let encrypted: i64 = r.try_get("encrypted").map_err(StorageError::from)?;
    let truncated: i64 = r.try_get("truncated").map_err(StorageError::from)?;
    let closed_at: Option<i64> = r.try_get("closed_at").map_err(StorageError::from)?;

    Ok(ConnectionRecord {
        id: ConnectionId::from_bytes(row::uuid_bytes(r, "id", "connections")?),
        protocol: Protocol::parse(&protocol).ok_or_else(|| {
            StorageError::corrupt("connections", format!("bad protocol {protocol}"))
        })?,
        peer: r.try_get("peer").map_err(StorageError::from)?,
        encrypted: encrypted != 0,
        opened_at: Timestamp::from_micros(r.try_get("opened_at").map_err(StorageError::from)?),
        closed_at: closed_at.map(Timestamp::from_micros),
        // An unparseable disposition is a corrupt row, not a `None`: silently
        // reporting "still open" for a closed connection would be a lie.
        disposition: disposition
            .map(|value| {
                ConnectionDisposition::parse(&value).ok_or_else(|| {
                    StorageError::corrupt("connections", format!("bad disposition {value}"))
                })
            })
            .transpose()?,
        transcript: transcript
            .filter(|text| !text.is_empty())
            .map(|text| text.split(LINE_SEPARATOR).map(ToOwned::to_owned).collect())
            .unwrap_or_default(),
        truncated: truncated != 0,
    })
}
