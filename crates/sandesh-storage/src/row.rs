//! Row-decoding helpers.
//!
//! Every failure here means a stored row could not be mapped back into a domain
//! type, which is corruption or a schema/code mismatch — never a caller error.
//! These helpers keep that distinction explicit instead of unwrapping.

use crate::StorageError;
use sqlx::{Row, sqlite::SqliteRow};

/// Reads a 16-byte identifier column.
///
/// # Errors
/// Fails if the column is absent or not exactly 16 bytes.
pub fn uuid_bytes(
    row: &SqliteRow,
    column: &'static str,
    table: &'static str,
) -> Result<[u8; 16], StorageError> {
    let raw: Vec<u8> = row.try_get(column).map_err(StorageError::from)?;
    raw.try_into().map_err(|v: Vec<u8>| {
        StorageError::corrupt(table, format!("{column} is {} bytes, expected 16", v.len()))
    })
}

/// Reads an optional 16-byte identifier column.
///
/// # Errors
/// Fails if the column is present but not exactly 16 bytes.
pub fn uuid_bytes_opt(
    row: &SqliteRow,
    column: &'static str,
    table: &'static str,
) -> Result<Option<[u8; 16]>, StorageError> {
    let raw: Option<Vec<u8>> = row.try_get(column).map_err(StorageError::from)?;
    raw.map(|v| {
        v.try_into().map_err(|v: Vec<u8>| {
            StorageError::corrupt(table, format!("{column} is {} bytes, expected 16", v.len()))
        })
    })
    .transpose()
}

/// Reads a 32-byte digest column.
///
/// # Errors
/// Fails if the column is absent or not exactly 32 bytes.
pub fn digest_bytes(
    row: &SqliteRow,
    column: &'static str,
    table: &'static str,
) -> Result<[u8; 32], StorageError> {
    let raw: Vec<u8> = row.try_get(column).map_err(StorageError::from)?;
    raw.try_into().map_err(|v: Vec<u8>| {
        StorageError::corrupt(table, format!("{column} is {} bytes, expected 32", v.len()))
    })
}

/// Narrows an `INTEGER` column to `u32`.
///
/// # Errors
/// Fails if the stored value is out of range, which would indicate corruption.
pub fn u32_from(
    value: i64,
    column: &'static str,
    table: &'static str,
) -> Result<u32, StorageError> {
    u32::try_from(value).map_err(|_| {
        StorageError::corrupt(table, format!("{column} value {value} is out of range"))
    })
}
