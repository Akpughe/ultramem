//! Object storage for original uploaded files (Phase A Task 2b).
//!
//! Before this, an uploaded file's bytes were deleted after ingest and only the
//! extracted text survived. A [`BlobStore`] keeps the original so a document can
//! always be re-derived (re-OCR, re-extract) or downloaded. The default impl is
//! [`LocalFsBlobStore`]; the trait keeps S3/GCS a drop-in later.

use async_trait::async_trait;
use std::path::PathBuf;

/// Store and retrieve original file bytes by an opaque key (the document id).
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String>;
    async fn get(&self, key: &str) -> Result<Vec<u8>, String>;
}

/// Filesystem-backed blob store: one file per key under `dir`.
pub struct LocalFsBlobStore {
    dir: PathBuf,
}

impl LocalFsBlobStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
    /// Key → a safe basename (strip any path separators / traversal). Keys are
    /// document uuids, so this is belt-and-suspenders.
    fn path_for(&self, key: &str) -> PathBuf {
        let safe: String = key
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        self.dir.join(if safe.is_empty() {
            "blob".to_string()
        } else {
            safe
        })
    }
}

#[async_trait]
impl BlobStore for LocalFsBlobStore {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|e| format!("blob mkdir failed: {e}"))?;
        tokio::fs::write(self.path_for(key), bytes)
            .await
            .map_err(|e| format!("blob put failed: {e}"))
    }
    async fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        tokio::fs::read(self.path_for(key))
            .await
            .map_err(|e| format!("blob get failed: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_roundtrips_and_sanitizes_keys() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir =
                std::env::temp_dir().join(format!("ultramem-blob-test-{}", std::process::id()));
            let store = LocalFsBlobStore::new(&dir);
            store.put("doc-123", b"hello bytes").await.unwrap();
            assert_eq!(store.get("doc-123").await.unwrap(), b"hello bytes");
            // A traversal-y key can't escape the dir (sanitized to a basename).
            store.put("../evil", b"x").await.unwrap();
            assert!(store.get("../evil").await.is_ok());
            assert!(!dir.parent().unwrap().join("evil").exists());
            let _ = tokio::fs::remove_dir_all(&dir).await;
        });
    }
}
