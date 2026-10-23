//! [`MetricsSource`] over `SQLite` — the readings behind `/metrics` (Phase 7 A4).
//!
//! Every figure is produced by an aggregate query. Nothing here loads a row it
//! then counts in Rust: a scrape runs on a schedule forever, so a linear pass
//! over the messages table would degrade quietly as a developer's instance
//! fills up — the same shape of defect A1 removed from the delivery lookup,
//! which scanned 500 records and returned `404` for the 501st.
//!
//! The whole snapshot is read inside one transaction so the numbers agree with
//! each other. Without it a delivery landing mid-scrape can be counted in the
//! delivery tally and not in the message gauge, publishing a state the server
//! was never in.

use crate::StorageError;
use async_trait::async_trait;
use sandesh_core::{
    CoreResult, DeliveryOutcome, DeliverySource, DeliveryTally, DurationHistogram, MetricsSnapshot,
    MetricsSource, Protocol, ProtocolTally,
};
use sqlx::{Row, SqlitePool};

/// `SQLite`-backed metric readings.
#[derive(Debug)]
pub struct SqliteMetricsSource {
    pool: SqlitePool,
}

impl SqliteMetricsSource {
    /// Creates a source over `pool`.
    #[must_use]
    pub const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MetricsSource for SqliteMetricsSource {
    async fn snapshot(&self) -> CoreResult<MetricsSnapshot> {
        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;

        let totals = sqlx::query(
            "SELECT
                (SELECT COUNT(*) FROM messages)        AS messages,
                (SELECT COUNT(*) FROM mailboxes)       AS mailboxes,
                (SELECT COUNT(*) FROM message_entries) AS entries,
                (SELECT COUNT(*) FROM accounts)        AS accounts",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(StorageError::from)?;

        // Grouped on the recipient rather than the delivery: a single
        // transaction can deliver to one address and reject another, so
        // counting deliveries by "their" outcome would have to pick one and
        // discard the rest. A rejected transaction stores no recipient row at
        // all, which the LEFT JOIN keeps visible as a NULL outcome.
        let delivery_rows = sqlx::query(
            "SELECT d.source AS source,
                    COALESCE(r.outcome, 'rejected') AS outcome,
                    COUNT(*) AS count
               FROM deliveries d
               LEFT JOIN delivery_recipients r ON r.delivery_id = d.id
              GROUP BY d.source, COALESCE(r.outcome, 'rejected')",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(StorageError::from)?;

        // `C:` marks a line the client sent (`sandesh_proto::Direction`), so
        // counting those counts commands and not the server's replies. A
        // transcript that hit its cap undercounts, which is correct: the
        // commands beyond the cap were never recorded.
        //
        // Counted only at the start of a line. `Transcript::line` renders the
        // client's own bytes after the marker, so a bare occurrence count
        // would let `MAIL FROM:<a@C:b>` report two commands where one was
        // sent — the count must key on the marker's position, not its
        // presence. Hence the leading separator, and the `+ 1` for the first
        // line, which has none before it.
        let connection_rows = sqlx::query(
            "SELECT protocol,
                    COUNT(*) AS connections,
                    COALESCE(SUM(
                        CASE WHEN transcript IS NULL OR transcript = '' THEN 0
                             ELSE (LENGTH(transcript)
                                   - LENGTH(REPLACE(transcript, char(10) || 'C:', ''))) / 3
                                  + (transcript LIKE 'C:%')
                        END
                    ), 0) AS commands
               FROM connections
              GROUP BY protocol",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(StorageError::from)?;

        // Bucketing in SQL would hard-code the bounds in two places; the
        // durations are a single integer column and the row count is capped by
        // the same retention that caps the table.
        let durations = sqlx::query("SELECT duration_us FROM deliveries")
            .fetch_all(&mut *tx)
            .await
            .map_err(StorageError::from)?;

        tx.commit().await.map_err(StorageError::from)?;

        let mut deliveries = Vec::with_capacity(delivery_rows.len());
        for row in delivery_rows {
            let source: String = row.try_get("source").map_err(StorageError::from)?;
            let outcome: String = row.try_get("outcome").map_err(StorageError::from)?;
            let count: i64 = row.try_get("count").map_err(StorageError::from)?;

            // A discriminant the domain does not know is skipped rather than
            // guessed at: inventing a bucket for it would report a figure no
            // code produced.
            let (Some(source), Some(outcome)) = (parse_source(&source), parse_outcome(&outcome))
            else {
                continue;
            };

            deliveries.push(DeliveryTally {
                source,
                outcome,
                count: count.unsigned_abs(),
            });
        }

        let mut connections = Vec::with_capacity(connection_rows.len());
        for row in connection_rows {
            let protocol: String = row.try_get("protocol").map_err(StorageError::from)?;
            let accepted: i64 = row.try_get("connections").map_err(StorageError::from)?;
            let commands: i64 = row.try_get("commands").map_err(StorageError::from)?;

            let Some(protocol) = parse_protocol(&protocol) else {
                continue;
            };

            connections.push(ProtocolTally {
                protocol,
                connections: accepted.unsigned_abs(),
                commands: commands.unsigned_abs(),
            });
        }

        let mut observations = Vec::with_capacity(durations.len());
        for row in durations {
            observations.push(
                row.try_get::<i64, _>("duration_us")
                    .map_err(StorageError::from)?,
            );
        }

        Ok(MetricsSnapshot {
            messages: count_of(&totals, "messages")?,
            mailboxes: count_of(&totals, "mailboxes")?,
            entries: count_of(&totals, "entries")?,
            accounts: count_of(&totals, "accounts")?,
            deliveries,
            connections,
            delivery_durations: DurationHistogram::from_micros(observations),
        })
    }
}

fn count_of(row: &sqlx::sqlite::SqliteRow, column: &str) -> CoreResult<u64> {
    Ok(row
        .try_get::<i64, _>(column)
        .map_err(StorageError::from)?
        .unsigned_abs())
}

fn parse_source(value: &str) -> Option<DeliverySource> {
    match value {
        "smtp" => Some(DeliverySource::Smtp),
        "imap_append" => Some(DeliverySource::ImapAppend),
        "api" => Some(DeliverySource::Api),
        _ => None,
    }
}

fn parse_outcome(value: &str) -> Option<DeliveryOutcome> {
    match value {
        "delivered" => Some(DeliveryOutcome::Delivered),
        "catch_all" => Some(DeliveryOutcome::CatchAll),
        "rejected" => Some(DeliveryOutcome::Rejected),
        _ => None,
    }
}

fn parse_protocol(value: &str) -> Option<Protocol> {
    match value {
        "smtp" => Some(Protocol::Smtp),
        "imap" => Some(Protocol::Imap),
        "pop3" => Some(Protocol::Pop3),
        _ => None,
    }
}
