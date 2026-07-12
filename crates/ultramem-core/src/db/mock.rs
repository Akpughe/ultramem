//! In-memory [`Db`] for offline tests (no Postgres). Mirrors the `MemStore`
//! pattern so lifecycle/migration tests stay hermetic in CI.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use super::{Db, DocumentRow};

#[derive(Default)]
pub struct MockDb {
    docs: Mutex<HashMap<String, DocumentRow>>,
}

impl MockDb {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn document_count(&self) -> usize {
        self.docs.lock().unwrap().len()
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
}
