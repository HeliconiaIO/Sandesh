//! Persistence of derived MIME structure: headers, parts and attachments.
//!
//! Everything here is regenerable from the authoritative raw bytes (ADR-003).
//! It is stored so that listing and fetching a message do not require
//! reparsing it, and it is rebuilt when `parse_version` changes.
//!
//! Split out of `message_repo` because it is a self-contained concern with its
//! own tables, and keeping it there made that module carry two jobs.

use crate::{StorageError, row};
use sandesh_core::{
    Attachment, AttachmentId, ByteRange, CoreResult, Disposition, Header, MessageId, MimePart,
    ParsedMessage, PartPath, TransferEncoding,
};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

/// Persists headers, MIME parts and attachment metadata.
///
/// All of this is derived from the raw bytes and regenerable; it is stored so
/// that listing and fetching do not require reparsing.
pub(crate) async fn insert_parsed_structure(
    tx: &mut Transaction<'_, Sqlite>,
    message_id: MessageId,
    parsed: &ParsedMessage,
) -> CoreResult<()> {
    for (ordinal, header) in parsed.headers.iter().enumerate() {
        sqlx::query(
            "INSERT INTO message_headers
                (message_id, ordinal, name, name_lower, raw_value, decoded_value,
                 byte_start, byte_end)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&message_id.as_bytes()[..])
        .bind(i64::try_from(ordinal).unwrap_or(i64::MAX))
        .bind(&header.name)
        .bind(header.name.to_lowercase())
        .bind(&header.raw_value)
        .bind(&header.decoded_value)
        .bind(i64::try_from(header.range.start()).unwrap_or(i64::MAX))
        .bind(i64::try_from(header.range.end()).unwrap_or(i64::MAX))
        .execute(&mut **tx)
        .await
        .map_err(StorageError::from)?;
    }

    for part in &parsed.parts {
        sqlx::query(
            "INSERT INTO mime_parts
                (message_id, part_path, parent_path, content_type, charset, encoding,
                 disposition, filename, content_id, header_start, header_end,
                 body_start, body_end, decoded_size, is_attachment)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&message_id.as_bytes()[..])
        .bind(part.path.to_string())
        .bind(part.path.parent().map(|p| p.to_string()))
        .bind(&part.content_type)
        .bind(part.charset.clone())
        .bind(part.encoding.as_str())
        .bind(match part.disposition {
            Disposition::Attachment => "attachment",
            Disposition::Inline => "inline",
        })
        .bind(part.filename.clone())
        .bind(part.content_id.clone())
        .bind(i64::try_from(part.header_range.start()).unwrap_or(i64::MAX))
        .bind(i64::try_from(part.header_range.end()).unwrap_or(i64::MAX))
        .bind(i64::try_from(part.body_range.start()).unwrap_or(i64::MAX))
        .bind(i64::try_from(part.body_range.end()).unwrap_or(i64::MAX))
        .bind(
            part.decoded_size
                .map(|s| i64::try_from(s).unwrap_or(i64::MAX)),
        )
        .bind(i64::from(part.is_attachment))
        .execute(&mut **tx)
        .await
        .map_err(StorageError::from)?;
    }

    for attachment in &parsed.attachments {
        sqlx::query(
            "INSERT INTO attachments
                (id, message_id, part_path, filename, content_type, content_id,
                 size, is_inline, cache_ref)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)",
        )
        .bind(&attachment.id.as_bytes()[..])
        .bind(&message_id.as_bytes()[..])
        .bind(attachment.part_path.to_string())
        .bind(attachment.filename.clone())
        .bind(&attachment.content_type)
        .bind(attachment.content_id.clone())
        .bind(i64::try_from(attachment.size).unwrap_or(i64::MAX))
        .bind(i64::from(attachment.is_inline))
        .execute(&mut **tx)
        .await
        .map_err(StorageError::from)?;
    }

    Ok(())
}

/// Rebuilds parsed structure from its stored rows.
///
/// All of this is derived from the raw bytes; it is read back rather than
/// reparsed so that listing and fetching stay cheap.
pub(crate) async fn load_parsed(
    pool: &SqlitePool,
    message_id: MessageId,
) -> CoreResult<ParsedMessage> {
    let meta = sqlx::query("SELECT parse_version, parse_ok FROM messages WHERE id = ?")
        .bind(&message_id.as_bytes()[..])
        .fetch_one(pool)
        .await
        .map_err(StorageError::from)?;

    let (parts, text_parts, html_parts) = load_parts(pool, message_id).await?;

    Ok(ParsedMessage {
        headers: load_headers(pool, message_id).await?,
        parts,
        text_parts,
        html_parts,
        attachments: load_attachments(pool, message_id).await?,
        // Diagnostics are not persisted in Phase 1: they are regenerable from
        // the raw bytes, and nothing yet reads them back from storage.
        diagnostics: Vec::new(),
        parse_ok: meta
            .try_get::<i64, _>("parse_ok")
            .map_err(StorageError::from)?
            != 0,
        parse_version: row::u32_from(
            meta.try_get("parse_version").map_err(StorageError::from)?,
            "parse_version",
            "messages",
        )?,
    })
}

/// Loads headers in received order, duplicates preserved.
async fn load_headers(pool: &SqlitePool, message_id: MessageId) -> CoreResult<Vec<Header>> {
    let rows = sqlx::query("SELECT * FROM message_headers WHERE message_id = ? ORDER BY ordinal")
        .bind(&message_id.as_bytes()[..])
        .fetch_all(pool)
        .await
        .map_err(StorageError::from)?;

    let mut headers = Vec::with_capacity(rows.len());
    for r in &rows {
        let start: i64 = r.try_get("byte_start").map_err(StorageError::from)?;
        let end: i64 = r.try_get("byte_end").map_err(StorageError::from)?;
        headers.push(Header {
            name: r.try_get("name").map_err(StorageError::from)?,
            raw_value: r.try_get("raw_value").map_err(StorageError::from)?,
            decoded_value: r
                .try_get::<Option<String>, _>("decoded_value")
                .map_err(StorageError::from)?
                .unwrap_or_default(),
            range: range_from(start, end)?,
        });
    }
    Ok(headers)
}

/// Loads the MIME tree, returning the parts plus the text and HTML body paths.
type LoadedParts = (Vec<MimePart>, Vec<PartPath>, Vec<PartPath>);

async fn load_parts(pool: &SqlitePool, message_id: MessageId) -> CoreResult<LoadedParts> {
    let rows = sqlx::query("SELECT * FROM mime_parts WHERE message_id = ? ORDER BY part_path")
        .bind(&message_id.as_bytes()[..])
        .fetch_all(pool)
        .await
        .map_err(StorageError::from)?;

    let mut parts = Vec::with_capacity(rows.len());
    let mut text_parts = Vec::new();
    let mut html_parts = Vec::new();

    for r in &rows {
        let path_text: String = r.try_get("part_path").map_err(StorageError::from)?;
        let path = PartPath::parse(&path_text)
            .ok_or_else(|| StorageError::corrupt("mime_parts", format!("bad path {path_text}")))?;
        let content_type: String = r.try_get("content_type").map_err(StorageError::from)?;
        let encoding: String = r.try_get("encoding").map_err(StorageError::from)?;
        let disposition: Option<String> = r.try_get("disposition").map_err(StorageError::from)?;

        let hs: i64 = r.try_get("header_start").map_err(StorageError::from)?;
        let he: i64 = r.try_get("header_end").map_err(StorageError::from)?;
        let bs: i64 = r.try_get("body_start").map_err(StorageError::from)?;
        let be: i64 = r.try_get("body_end").map_err(StorageError::from)?;

        match content_type.as_str() {
            "text/plain" => text_parts.push(path.clone()),
            "text/html" => html_parts.push(path.clone()),
            _ => {}
        }

        parts.push(MimePart {
            path,
            content_type,
            charset: r.try_get("charset").map_err(StorageError::from)?,
            encoding: TransferEncoding::parse(&encoding),
            disposition: match disposition.as_deref() {
                Some("attachment") => Disposition::Attachment,
                _ => Disposition::Inline,
            },
            filename: r.try_get("filename").map_err(StorageError::from)?,
            content_id: r.try_get("content_id").map_err(StorageError::from)?,
            header_range: range_from(hs, he)?,
            body_range: range_from(bs, be)?,
            decoded_size: r
                .try_get::<Option<i64>, _>("decoded_size")
                .map_err(StorageError::from)?
                .map(i64::unsigned_abs),
            is_attachment: r
                .try_get::<i64, _>("is_attachment")
                .map_err(StorageError::from)?
                != 0,
            children: Vec::new(),
        });
    }

    // Rebuild child links from the path structure.
    let paths: Vec<PartPath> = parts.iter().map(|p| p.path.clone()).collect();
    for part in &mut parts {
        part.children = paths
            .iter()
            .filter(|candidate| candidate.parent().as_ref() == Some(&part.path))
            .cloned()
            .collect();
    }

    Ok((parts, text_parts, html_parts))
}

/// Loads attachment metadata.
async fn load_attachments(pool: &SqlitePool, message_id: MessageId) -> CoreResult<Vec<Attachment>> {
    let rows = sqlx::query("SELECT * FROM attachments WHERE message_id = ? ORDER BY part_path")
        .bind(&message_id.as_bytes()[..])
        .fetch_all(pool)
        .await
        .map_err(StorageError::from)?;

    let mut attachments = Vec::with_capacity(rows.len());
    for r in &rows {
        let path: String = r.try_get("part_path").map_err(StorageError::from)?;
        attachments.push(Attachment {
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
        });
    }
    Ok(attachments)
}

/// Reconstructs a byte range from its stored bounds.
///
/// A failure means the stored offsets are inconsistent — corruption or a
/// schema/code mismatch, never a caller error.
pub(crate) fn range_from(start: i64, end: i64) -> Result<ByteRange, StorageError> {
    let start = usize::try_from(start)
        .map_err(|_| StorageError::corrupt("byte range", format!("bad start {start}")))?;
    let end = usize::try_from(end)
        .map_err(|_| StorageError::corrupt("byte range", format!("bad end {end}")))?;
    ByteRange::new(start, end)
        .ok_or_else(|| StorageError::corrupt("byte range", format!("{start} > {end}")))
}
