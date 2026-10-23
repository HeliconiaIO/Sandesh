//! [`SearchIndex`] over `SQLite` FTS5 (`STORAGE_DESIGN.md` §7).
//!
//! **A prefilter, not the search.** RFC 9051 §6.4.4 defines the IMAP text keys
//! as substring matches and FTS5 tokenises into words, so this index can prove
//! a message is worth loading but never that it matches. `SearchQuery::matches`
//! decides every result, here as before; all this removes is the cost of
//! loading messages that provably cannot match.
//!
//! Every method that cannot guarantee a **superset** of the matches returns
//! [`SearchCandidates::Unsupported`], which the caller reads as "scan
//! everything". That is the single invariant the whole design rests on: a false
//! positive costs one wasted load, a false negative silently loses mail.

use crate::StorageError;
use async_trait::async_trait;
use sandesh_core::{
    CoreResult, Message, MessageId, SearchCandidates, SearchIndex, SearchQuery, TextField,
};
use sqlx::{Row, SqlitePool};
use std::collections::BTreeSet;

/// The trigram tokenizer's minimum query length.
///
/// A trigram index stores three-character sequences, so it cannot answer a
/// one- or two-character query at all. Those fall back to a scan, which is
/// correct and no worse than the behaviour before this index existed.
const MIN_TRIGRAM_LEN: usize = 3;

/// `SQLite` FTS5-backed search index.
#[derive(Debug)]
pub struct SqliteSearchIndex {
    pool: SqlitePool,
}

impl SqliteSearchIndex {
    /// Creates an index over `pool`.
    #[must_use]
    pub const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// The text this index stores for a message.
///
/// Headers come from the parsed structure rather than the denormalised
/// `messages` columns so that the index and `matches` read the same source; the
/// body is every decoded non-multipart text part, which is what
/// `TextField::Body` compares against.
fn indexable(message: &Message) -> IndexedText {
    indexable_from_parts(&message.parsed, &message.raw)
}

/// The shared definition of what gets indexed.
///
/// Both the trait method and the in-transaction writer go through here, so the
/// two paths cannot drift into indexing different text — which would make the
/// index sound after a rebuild and unsound after a delivery, or the reverse.
fn indexable_from_parts(
    parsed: &sandesh_core::ParsedMessage,
    raw: &sandesh_core::RawMessage,
) -> IndexedText {
    let header = |name: &str| {
        parsed
            .headers_named(name)
            .map(|h| h.decoded_value.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };

    // Every non-multipart part, **decoded** — exactly the set and the form that
    // `search::body_contains` compares against. Indexing the raw bytes instead
    // would make a base64 body index as base64 while `matches` reads it as
    // text, and the index would then miss messages it must not miss.
    let mut body = String::new();
    for part in parsed.parts.iter().filter(|part| !part.is_multipart()) {
        if let Some(bytes) = sandesh_core::decoded_part_body(raw.bytes(), &part.path)
            .or_else(|| part.body_range.slice(raw.bytes()).map(<[u8]>::to_vec))
        {
            body.push_str(&String::from_utf8_lossy(&bytes));
            body.push('\n');
        }
    }

    IndexedText {
        subject: header("subject"),
        from_addr: header("from"),
        to_addr: header("to"),
        cc_addr: header("cc"),
        body,
    }
}

/// One message's indexed columns.
struct IndexedText {
    subject: String,
    from_addr: String,
    to_addr: String,
    cc_addr: String,
    body: String,
}

fn decode_id(row: &sqlx::sqlite::SqliteRow) -> CoreResult<MessageId> {
    let bytes: Vec<u8> = row.try_get("message_id").map_err(StorageError::from)?;
    let array: [u8; 16] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| StorageError::corrupt("messages_fts_map", "message_id is not 16 bytes"))?;
    Ok(MessageId::from_bytes(array))
}

/// Which FTS5 column a text field maps to, or `None` when the field spans
/// columns the index cannot isolate.
const fn column_for(field: &TextField) -> Option<&'static str> {
    match field {
        TextField::Subject => Some("subject"),
        TextField::From => Some("from_addr"),
        TextField::To => Some("to_addr"),
        TextField::Cc => Some("cc_addr"),
        TextField::Body => Some("body_text"),
        // `Bcc` is not indexed (it is rarely present and never in received
        // mail), `Header(_)` is an arbitrary name the index does not store, and
        // `Text` spans headers the index only partly holds. All must scan.
        TextField::Bcc | TextField::Header(_) | TextField::Text => None,
    }
}

/// Escapes a value for an FTS5 string literal, which doubles its quotes.
fn fts_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[async_trait]
impl SearchIndex for SqliteSearchIndex {
    async fn index(&self, message: &Message) -> CoreResult<()> {
        let text = indexable(message);
        let id = &message.id.as_bytes()[..];

        // Delete-then-insert in one transaction, so re-indexing replaces rather
        // than duplicating and a failure cannot leave both rows.
        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;

        let existing: Option<i64> =
            sqlx::query_scalar("SELECT rowid_ref FROM messages_fts_map WHERE message_id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(StorageError::from)?;

        if let Some(rowid) = existing {
            sqlx::query("DELETE FROM messages_fts WHERE rowid = ?")
                .bind(rowid)
                .execute(&mut *tx)
                .await
                .map_err(StorageError::from)?;
        }

        let inserted = sqlx::query(
            "INSERT INTO messages_fts
                (message_id, subject, from_addr, to_addr, cc_addr, body_text)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(&text.subject)
        .bind(&text.from_addr)
        .bind(&text.to_addr)
        .bind(&text.cc_addr)
        .bind(&text.body)
        .execute(&mut *tx)
        .await
        .map_err(StorageError::from)?;

        sqlx::query(
            "INSERT INTO messages_fts_map (message_id, rowid_ref) VALUES (?, ?)
             ON CONFLICT(message_id) DO UPDATE SET rowid_ref = excluded.rowid_ref",
        )
        .bind(id)
        .bind(inserted.last_insert_rowid())
        .execute(&mut *tx)
        .await
        .map_err(StorageError::from)?;

        tx.commit().await.map_err(StorageError::from)?;
        Ok(())
    }

    async fn remove(&self, id: MessageId) -> CoreResult<()> {
        let bytes = &id.as_bytes()[..];
        let mut tx = self.pool.begin().await.map_err(StorageError::from)?;

        let existing: Option<i64> =
            sqlx::query_scalar("SELECT rowid_ref FROM messages_fts_map WHERE message_id = ?")
                .bind(bytes)
                .fetch_optional(&mut *tx)
                .await
                .map_err(StorageError::from)?;

        // Removing an absent message is not an error: the index is derived
        // data, and a caller repairing it should not have to check first.
        if let Some(rowid) = existing {
            sqlx::query("DELETE FROM messages_fts WHERE rowid = ?")
                .bind(rowid)
                .execute(&mut *tx)
                .await
                .map_err(StorageError::from)?;
            sqlx::query("DELETE FROM messages_fts_map WHERE message_id = ?")
                .bind(bytes)
                .execute(&mut *tx)
                .await
                .map_err(StorageError::from)?;
        }

        tx.commit().await.map_err(StorageError::from)?;
        Ok(())
    }

    async fn candidates(&self, query: &SearchQuery) -> CoreResult<SearchCandidates> {
        self.narrow(query).await
    }
}

impl SqliteSearchIndex {
    /// Narrows `query` to a superset of the messages that may match.
    ///
    /// Only the shapes that can be narrowed **soundly** are handled; everything
    /// else returns `Unsupported`. The recursion is deliberately conservative:
    ///
    /// - `And` — intersecting is sound, because a message must satisfy every
    ///   branch. An unsupported branch contributes no constraint and is skipped,
    ///   which widens the set rather than narrowing it wrongly.
    /// - `Or` — sound only if **every** branch narrows. One unsupported branch
    ///   means some matching message might be outside the union, so the whole
    ///   union is abandoned.
    /// - `Not` — never narrowed. The complement of a candidate superset is a
    ///   *subset* of the true complement, which is precisely the false negative
    ///   this design forbids.
    async fn narrow(&self, query: &SearchQuery) -> CoreResult<SearchCandidates> {
        match query {
            SearchQuery::Contains { field, value } => self.narrow_text(field, value).await,

            SearchQuery::And(children) => {
                let mut result: Option<BTreeSet<MessageId>> = None;
                for child in children {
                    if let Some(ids) = Box::pin(self.narrow(child)).await?.narrowed() {
                        result = Some(match result {
                            None => ids.clone(),
                            Some(acc) => acc.intersection(ids).copied().collect(),
                        });
                    }
                }
                Ok(result.map_or(SearchCandidates::Unsupported, SearchCandidates::Candidates))
            }

            SearchQuery::Or(children) => {
                let mut union = BTreeSet::new();
                for child in children {
                    match Box::pin(self.narrow(child)).await? {
                        // One branch we cannot narrow poisons the whole union.
                        SearchCandidates::Unsupported => {
                            return Ok(SearchCandidates::Unsupported);
                        }
                        SearchCandidates::Candidates(ids) => union.extend(ids),
                    }
                }
                if children.is_empty() {
                    return Ok(SearchCandidates::Unsupported);
                }
                Ok(SearchCandidates::Candidates(union))
            }

            // Everything else is either answerable from the entry alone (flags,
            // UID, MODSEQ — the caller never needed the index) or not indexed
            // (dates, sizes, attachments, Not).
            _ => Ok(SearchCandidates::Unsupported),
        }
    }

    /// Narrows one `Contains` key.
    ///
    /// Sound because the index is a **trigram** index, not a word index: it
    /// matches substrings with the same semantics RFC 9051 §6.4.4 requires, so
    /// a message that does not match here provably does not contain the value.
    /// A word-tokenised index could not make that claim — "xfoobar" contains
    /// "foo" but has no token starting with it — which is why the migration
    /// deviates from the `unicode61` originally specified.
    ///
    /// Two cases still refuse to narrow, both because a wrong answer would be a
    /// false negative:
    ///
    /// - a value shorter than a trigram, which the index cannot represent;
    /// - a field the index does not store in its own column (`Bcc`,
    ///   `Header(_)`, `Text`).
    async fn narrow_text(&self, field: &TextField, value: &str) -> CoreResult<SearchCandidates> {
        // An empty value matches on field *presence* (RFC 9051 §6.4.4), which
        // is not a substring question and not one FTS5 can answer.
        if value.len() < MIN_TRIGRAM_LEN {
            return Ok(SearchCandidates::Unsupported);
        }
        let Some(column) = column_for(field) else {
            return Ok(SearchCandidates::Unsupported);
        };

        // A message the index has not seen must never be excluded: the index is
        // derived data that may lag a write or be mid-rebuild. Rather than
        // silently dropping such messages, decline to narrow at all — the one
        // safe answer, and a state the maintenance path keeps rare.
        let unindexed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages
             WHERE id NOT IN (SELECT message_id FROM messages_fts_map)",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StorageError::from)?;
        if unindexed > 0 {
            return Ok(SearchCandidates::Unsupported);
        }

        let pattern = format!("{column} : {}", fts_quote(value));
        let rows = sqlx::query("SELECT message_id FROM messages_fts WHERE messages_fts MATCH ?")
            .bind(&pattern)
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        let hits: BTreeSet<MessageId> = rows.iter().map(decode_id).collect::<CoreResult<_>>()?;
        Ok(SearchCandidates::Candidates(hits))
    }
}

/// Indexes a message inside an existing transaction.
///
/// The insert path calls this rather than [`SearchIndex::index`] so the index
/// row lands in the **same transaction** as the message it describes. That is
/// what makes "indexed" and "stored" the same event: there is no window in
/// which a committed message is missing from the index, so `narrow_text`'s
/// refusal to narrow while unindexed messages exist stays a repair path rather
/// than the normal state.
///
/// Takes the parsed structure directly because the caller has it in hand and
/// the `Message` it would otherwise build is not yet readable from the
/// uncommitted transaction.
pub(crate) async fn index_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    message_id: MessageId,
    parsed: &sandesh_core::ParsedMessage,
    raw: &sandesh_core::RawMessage,
) -> CoreResult<()> {
    let text = indexable_from_parts(parsed, raw);
    let id = &message_id.as_bytes()[..];

    let existing: Option<i64> =
        sqlx::query_scalar("SELECT rowid_ref FROM messages_fts_map WHERE message_id = ?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(StorageError::from)?;

    if let Some(rowid) = existing {
        sqlx::query("DELETE FROM messages_fts WHERE rowid = ?")
            .bind(rowid)
            .execute(&mut **tx)
            .await
            .map_err(StorageError::from)?;
    }

    let inserted = sqlx::query(
        "INSERT INTO messages_fts
            (message_id, subject, from_addr, to_addr, cc_addr, body_text)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(&text.subject)
    .bind(&text.from_addr)
    .bind(&text.to_addr)
    .bind(&text.cc_addr)
    .bind(&text.body)
    .execute(&mut **tx)
    .await
    .map_err(StorageError::from)?;

    sqlx::query(
        "INSERT INTO messages_fts_map (message_id, rowid_ref) VALUES (?, ?)
         ON CONFLICT(message_id) DO UPDATE SET rowid_ref = excluded.rowid_ref",
    )
    .bind(id)
    .bind(inserted.last_insert_rowid())
    .execute(&mut **tx)
    .await
    .map_err(StorageError::from)?;

    Ok(())
}

/// Removes a message from the index inside an existing transaction.
///
/// The counterpart to [`index_in_tx`], for the same reason: the index row and
/// the message row must disappear together.
pub(crate) async fn remove_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    message_id: MessageId,
) -> CoreResult<()> {
    let id = &message_id.as_bytes()[..];

    let existing: Option<i64> =
        sqlx::query_scalar("SELECT rowid_ref FROM messages_fts_map WHERE message_id = ?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(StorageError::from)?;

    if let Some(rowid) = existing {
        sqlx::query("DELETE FROM messages_fts WHERE rowid = ?")
            .bind(rowid)
            .execute(&mut **tx)
            .await
            .map_err(StorageError::from)?;
        sqlx::query("DELETE FROM messages_fts_map WHERE message_id = ?")
            .bind(id)
            .execute(&mut **tx)
            .await
            .map_err(StorageError::from)?;
    }

    Ok(())
}
