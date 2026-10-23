//! Content-addressed blob storage for large message bodies.
//!
//! Raw messages above [`INLINE_THRESHOLD`] are written here instead of inline
//! in the `messages` row (`STORAGE_DESIGN.md` §4, ADR-007). Most test messages
//! are small, and for those inline storage keeps delivery a single atomic
//! transaction with no orphan risk; large messages are rare and are exactly the
//! ones that would degrade row reads.
//!
//! Layout is `<root>/<sha[0:2]>/<sha[2:4]>/<sha>`, so writing identical bytes
//! twice is idempotent and costs one file.

use crate::{StorageError, StorageResult};
use async_trait::async_trait;
use sandesh_core::{BlobStore, CoreResult};
use std::path::PathBuf;

/// Messages at or below this size are stored inline in the row.
///
/// 256 KiB sits above typical HTML mail with inline images and below anything
/// that would strain a row read. The figure is an estimate pending the
/// benchmark in OD-4.
pub const INLINE_THRESHOLD: usize = 256 * 1024;

/// A blob store backed by the filesystem.
#[derive(Debug, Clone)]
pub struct FilesystemBlobStore {
    root: PathBuf,
}

impl FilesystemBlobStore {
    /// Creates a store rooted at `root`, creating the directory if needed.
    ///
    /// # Errors
    /// Fails if the directory cannot be created.
    pub async fn new(root: impl Into<PathBuf>) -> StorageResult<Self> {
        let root = root.into();
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|e| StorageError::Blob(format!("creating {}: {e}", root.display())))?;
        Ok(Self { root })
    }

    /// The path an object with digest `hex` lives at.
    fn path_for(&self, hex: &str) -> Option<PathBuf> {
        // Reject anything that is not a plain hex digest: the reference reaches
        // here from a database row, and a crafted value must not be able to
        // escape the root via `..` or an absolute path.
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.root.join(&hex[0..2]).join(&hex[2..4]).join(hex))
    }
}

#[async_trait]
impl BlobStore for FilesystemBlobStore {
    /// Stores bytes under their SHA-256 digest.
    ///
    /// Writes to a temporary file and renames, so a concurrent reader never
    /// observes a partial object.
    async fn put(&self, bytes: &[u8]) -> CoreResult<String> {
        use sha2::{Digest, Sha256};
        let hex = hex::encode(Sha256::digest(bytes));

        let path = self
            .path_for(&hex)
            .ok_or_else(|| StorageError::Blob(String::from("invalid digest")))?;

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::Blob(e.to_string()))?;
        }

        // Content-addressed, so an existing object is already correct.
        if tokio::fs::try_exists(&path)
            .await
            .map_err(|e| StorageError::Blob(e.to_string()))?
        {
            return Ok(hex);
        }

        let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::now_v7()));
        tokio::fs::write(&temp, bytes)
            .await
            .map_err(|e| StorageError::Blob(e.to_string()))?;
        tokio::fs::rename(&temp, &path)
            .await
            .map_err(|e| StorageError::Blob(e.to_string()))?;

        Ok(hex)
    }

    async fn get(&self, reference: &str) -> CoreResult<Option<Vec<u8>>> {
        let Some(path) = self.path_for(reference) else {
            return Ok(None);
        };
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StorageError::Blob(e.to_string()).into()),
        }
    }

    async fn delete(&self, reference: &str) -> CoreResult<bool> {
        let Some(path) = self.path_for(reference) else {
            return Ok(false);
        };
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(StorageError::Blob(e.to_string()).into()),
        }
    }
}

/// A blob store that keeps nothing, for databases with no external objects.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullBlobStore;

#[async_trait]
impl BlobStore for NullBlobStore {
    async fn put(&self, _bytes: &[u8]) -> CoreResult<String> {
        Err(StorageError::Blob(String::from("no blob store configured")).into())
    }

    async fn get(&self, _reference: &str) -> CoreResult<Option<Vec<u8>>> {
        Ok(None)
    }

    async fn delete(&self, _reference: &str) -> CoreResult<bool> {
        Ok(false)
    }
}

/// Where a message's raw bytes should live, given its size.
#[must_use]
pub fn should_store_inline(size: usize) -> bool {
    size <= INLINE_THRESHOLD
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    async fn store() -> (tempfile::TempDir, FilesystemBlobStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemBlobStore::new(dir.path())
            .await
            .expect("create store");
        (dir, store)
    }

    #[tokio::test]
    async fn round_trips_bytes_exactly() {
        let (_dir, store) = store().await;
        let payload = b"\x00\x01\x02 binary \xff\xfe content".to_vec();

        let reference = store.put(&payload).await.expect("put");
        let fetched = store.get(&reference).await.expect("get");

        assert_eq!(fetched, Some(payload), "bytes must survive verbatim");
    }

    #[tokio::test]
    async fn identical_content_stores_once() {
        let (_dir, store) = store().await;
        let a = store.put(b"same").await.expect("put a");
        let b = store.put(b"same").await.expect("put b");
        assert_eq!(a, b, "content addressing makes writes idempotent");
    }

    #[tokio::test]
    async fn missing_object_returns_none() {
        let (_dir, store) = store().await;
        let absent = "0".repeat(64);
        assert_eq!(store.get(&absent).await.expect("get"), None);
        assert!(!store.delete(&absent).await.expect("delete"));
    }

    #[tokio::test]
    async fn path_traversal_is_rejected() {
        // References come from database rows; a crafted value must not escape.
        let (_dir, store) = store().await;
        for hostile in [
            "../../etc/passwd",
            "/etc/passwd",
            "..",
            "zz".repeat(32).as_str(),
        ] {
            assert_eq!(store.get(hostile).await.expect("get"), None);
            assert!(!store.delete(hostile).await.expect("delete"));
        }
    }

    #[tokio::test]
    async fn delete_removes_the_object() {
        let (_dir, store) = store().await;
        let reference = store.put(b"transient").await.expect("put");
        assert!(store.delete(&reference).await.expect("delete"));
        assert_eq!(store.get(&reference).await.expect("get"), None);
    }

    #[test]
    fn threshold_decides_placement() {
        assert!(should_store_inline(INLINE_THRESHOLD));
        assert!(!should_store_inline(INLINE_THRESHOLD + 1));
    }
}
