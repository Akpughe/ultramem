//! In-memory [`Db`] for offline tests (no Postgres). Mirrors the `MemStore`
//! pattern so lifecycle/migration tests stay hermetic in CI.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use super::{ChunkRow, Db, DocumentRow};

#[derive(Default)]
pub struct MockDb {
    docs: Mutex<HashMap<String, DocumentRow>>,
    chunks: Mutex<HashMap<String, ChunkRow>>,
}

impl MockDb {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn document_count(&self) -> usize {
        self.docs.lock().unwrap().len()
    }
    pub fn chunk_count(&self) -> usize {
        self.chunks.lock().unwrap().len()
    }
}

#[async_trait]
impl Db for MockDb {
    async fn health(&self) -> bool {
        true
    }
    async fn migrate(&self) -> Result<(), String> {
        Ok(())
    }
    async fn insert_document(&self, d: &DocumentRow) -> Result<(), String> {
        // Idempotent: a duplicate id is a no-op, matching `on conflict do nothing`.
        self.docs
            .lock()
            .unwrap()
            .entry(d.id.clone())
            .or_insert_with(|| d.clone());
        Ok(())
    }
    async fn get_document(
        &self,
        id: &str,
        container_tag: &str,
    ) -> Result<Option<DocumentRow>, String> {
        Ok(self
            .docs
            .lock()
            .unwrap()
            .get(id)
            .filter(|d| d.container_tag == container_tag)
            .cloned())
    }
    async fn upsert_chunks(&self, chunks: &[ChunkRow]) -> Result<(), String> {
        let mut store = self.chunks.lock().unwrap();
        for c in chunks {
            store.entry(c.id.clone()).or_insert_with(|| c.clone());
        }
        Ok(())
    }
    async fn find_document_id(
        &self,
        container_tag: &str,
        content_hash: &str,
        canonical_url: Option<&str>,
    ) -> Result<Option<String>, String> {
        Ok(self
            .docs
            .lock()
            .unwrap()
            .values()
            .find(|d| {
                d.container_tag == container_tag
                    && (d.content_hash.as_deref() == Some(content_hash)
                        || (canonical_url.is_some() && d.canonical_url.as_deref() == canonical_url))
            })
            .map(|d| d.id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, tag: &str) -> DocumentRow {
        DocumentRow {
            id: id.into(),
            container_tag: tag.into(),
            source: "api".into(),
            title: "T".into(),
            reference: String::new(),
            content_hash: Some(format!("hash-of-{id}")),
            canonical_url: None,
            captured_at: 1,
            processing_state: "pending".into(),
            created_at: 1,
        }
    }

    #[test]
    fn insert_get_are_idempotent_and_tag_scoped() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let db = MockDb::new();
            db.insert_document(&doc("d1", "t")).await.unwrap();
            db.insert_document(&doc("d1", "t")).await.unwrap(); // idempotent
            assert_eq!(db.document_count(), 1);
            assert_eq!(
                db.get_document("d1", "t").await.unwrap(),
                Some(doc("d1", "t"))
            );
            // Tag isolation: another namespace can't read it.
            assert!(db.get_document("d1", "other").await.unwrap().is_none());
            assert!(db.get_document("missing", "t").await.unwrap().is_none());
        });
    }

    #[test]
    fn dedup_lookup_matches_hash_within_tag() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let db = MockDb::new();
            db.insert_document(&doc("d1", "t")).await.unwrap();
            // Same hash, same tag → dedup hit.
            assert_eq!(
                db.find_document_id("t", "hash-of-d1", None).await.unwrap(),
                Some("d1".into())
            );
            // Same hash, different tag → miss (no cross-tenant dedup).
            assert!(db
                .find_document_id("other", "hash-of-d1", None)
                .await
                .unwrap()
                .is_none());
            // Unknown hash → miss.
            assert!(db
                .find_document_id("t", "nope", None)
                .await
                .unwrap()
                .is_none());
        });
    }
}
