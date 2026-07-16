//! In-memory [`Db`] for offline tests (no Postgres). Mirrors the `MemStore`
//! pattern so lifecycle/migration tests stay hermetic in CI.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use super::{AclEntry, AuditEvent, ChunkRow, Db, DocumentRow, EvidenceRow, JobRow, MemoryRow};

#[derive(Default)]
pub struct MockDb {
    docs: Mutex<HashMap<String, DocumentRow>>,
    chunks: Mutex<HashMap<String, ChunkRow>>,
    memories: Mutex<HashMap<String, MemoryRow>>,
    evidence: Mutex<Vec<EvidenceRow>>,
    jobs: Mutex<HashMap<String, JobRow>>,
    audits: Mutex<Vec<AuditEvent>>,
    acls: Mutex<Vec<AclEntry>>,
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
    pub fn memory_count(&self) -> usize {
        self.memories.lock().unwrap().len()
    }
    /// Test helper: a clone of a stored memory row by id.
    pub fn memory(&self, id: &str) -> Option<MemoryRow> {
        self.memories.lock().unwrap().get(id).cloned()
    }
    /// Test helper: all stored memory rows.
    pub fn memories(&self) -> Vec<MemoryRow> {
        self.memories.lock().unwrap().values().cloned().collect()
    }
    /// Test helper: all stored evidence rows.
    pub fn evidence(&self) -> Vec<EvidenceRow> {
        self.evidence.lock().unwrap().clone()
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
    async fn list_documents(
        &self,
        container_tag: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<Vec<DocumentRow>, String> {
        let mut rows: Vec<DocumentRow> = self
            .docs
            .lock()
            .unwrap()
            .values()
            .filter(|d| {
                d.container_tag == container_tag
                    && before.map(|b| d.captured_at < b).unwrap_or(true)
            })
            .cloned()
            .collect();
        rows.sort_by_key(|d| std::cmp::Reverse(d.captured_at)); // newest first
        rows.truncate(limit.max(0) as usize);
        Ok(rows)
    }
    async fn insert_memories(&self, memories: &[MemoryRow]) -> Result<(), String> {
        let mut store = self.memories.lock().unwrap();
        for m in memories {
            store.entry(m.id.clone()).or_insert_with(|| m.clone());
        }
        Ok(())
    }
    async fn mark_superseded(&self, pairs: &[(String, String)]) -> Result<(), String> {
        let mut store = self.memories.lock().unwrap();
        for (old_id, new_id) in pairs {
            if let Some(m) = store.get_mut(old_id) {
                m.is_latest = false;
                m.superseded_by = Some(new_id.clone());
            }
        }
        Ok(())
    }
    async fn insert_evidence(&self, rows: &[EvidenceRow]) -> Result<(), String> {
        self.evidence.lock().unwrap().extend_from_slice(rows);
        Ok(())
    }
    async fn memories_by_statement(
        &self,
        container_tag: &str,
        statements: &[String],
    ) -> Result<Vec<MemoryRow>, String> {
        Ok(self
            .memories
            .lock()
            .unwrap()
            .values()
            .filter(|m| {
                m.container_tag == container_tag && m.is_latest && statements.contains(&m.statement)
            })
            .cloned()
            .collect())
    }
    async fn evidence_for(&self, memory_ids: &[String]) -> Result<Vec<EvidenceRow>, String> {
        Ok(self
            .evidence
            .lock()
            .unwrap()
            .iter()
            .filter(|e| memory_ids.contains(&e.memory_id))
            .cloned()
            .collect())
    }
    async fn insert_job(&self, job: &JobRow) -> Result<(), String> {
        self.jobs
            .lock()
            .unwrap()
            .entry(job.id.clone())
            .or_insert_with(|| job.clone());
        Ok(())
    }
    async fn update_job(
        &self,
        id: &str,
        state: &str,
        progress: i32,
        error: Option<&str>,
        updated_at: i64,
    ) -> Result<(), String> {
        if let Some(j) = self.jobs.lock().unwrap().get_mut(id) {
            j.state = state.to_string();
            j.progress = progress;
            j.error = error.map(String::from);
            j.updated_at = updated_at;
        }
        Ok(())
    }
    async fn get_job(&self, id: &str, container_tag: &str) -> Result<Option<JobRow>, String> {
        Ok(self
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .filter(|j| j.container_tag.as_deref() == Some(container_tag))
            .cloned())
    }
    async fn insert_audit(&self, event: &AuditEvent) -> Result<(), String> {
        self.audits.lock().unwrap().push(event.clone());
        Ok(())
    }
    async fn audit_count(&self, container_tag: &str) -> Result<i64, String> {
        Ok(self
            .audits
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.container_tag.as_deref() == Some(container_tag))
            .count() as i64)
    }
    async fn chunks_for_document(&self, document_id: &str) -> Result<Vec<ChunkRow>, String> {
        let mut rows: Vec<ChunkRow> = self
            .chunks
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.document_id == document_id)
            .cloned()
            .collect();
        rows.sort_by_key(|c| c.chunk_index);
        Ok(rows)
    }
    async fn memories_for_tag(
        &self,
        container_tag: &str,
        cap: i64,
    ) -> Result<Vec<MemoryRow>, String> {
        Ok(self
            .memories
            .lock()
            .unwrap()
            .values()
            .filter(|m| m.container_tag == container_tag)
            .take(cap.max(0) as usize)
            .cloned()
            .collect())
    }
    async fn grant_acl(&self, entry: &AclEntry) -> Result<(), String> {
        let mut acls = self.acls.lock().unwrap();
        if !acls.iter().any(|a| {
            a.principal == entry.principal
                && a.scope == entry.scope
                && a.capability == entry.capability
        }) {
            acls.push(entry.clone());
        }
        Ok(())
    }
    async fn revoke_acl(&self, entry: &AclEntry) -> Result<(), String> {
        self.acls.lock().unwrap().retain(|a| {
            !(a.principal == entry.principal
                && a.scope == entry.scope
                && a.capability == entry.capability)
        });
        Ok(())
    }
    async fn acls_for_principal(&self, principal: &str) -> Result<Vec<AclEntry>, String> {
        Ok(self
            .acls
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.principal == principal)
            .cloned()
            .collect())
    }
    async fn acls_for_scope(&self, scope: &str) -> Result<Vec<AclEntry>, String> {
        Ok(self
            .acls
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.scope == scope)
            .cloned()
            .collect())
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
            blob_key: None,
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
    fn acl_grant_is_idempotent_and_per_principal() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let db = MockDb::new();
            let mk = |p: &str, s: &str, c: &str| AclEntry {
                principal: p.into(),
                scope: s.into(),
                capability: c.into(),
                created_at: 0,
            };
            db.grant_acl(&mk("u1", "team", "read")).await.unwrap();
            db.grant_acl(&mk("u1", "team", "read")).await.unwrap(); // idempotent
            db.grant_acl(&mk("u2", "team", "read")).await.unwrap();
            assert_eq!(db.acls_for_principal("u1").await.unwrap().len(), 1);
            assert_eq!(db.acls_for_principal("u2").await.unwrap().len(), 1);
            assert!(db.acls_for_principal("nobody").await.unwrap().is_empty());
        });
    }

    #[test]
    fn acl_revoke_and_by_scope() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let db = MockDb::new();
            let mk = |p: &str, s: &str, c: &str| AclEntry {
                principal: p.into(),
                scope: s.into(),
                capability: c.into(),
                created_at: 0,
            };
            db.grant_acl(&mk("u1", "team", "read")).await.unwrap();
            db.grant_acl(&mk("u2", "team", "write")).await.unwrap();
            db.grant_acl(&mk("u1", "other", "read")).await.unwrap();

            // acls_for_scope returns every grant ON that scope, across principals.
            let team = db.acls_for_scope("team").await.unwrap();
            assert_eq!(team.len(), 2);
            assert!(team.iter().all(|a| a.scope == "team"));

            // Revoke is specific to (principal, scope, capability) and idempotent.
            db.revoke_acl(&mk("u1", "team", "read")).await.unwrap();
            db.revoke_acl(&mk("u1", "team", "read")).await.unwrap(); // no-op
            let team = db.acls_for_scope("team").await.unwrap();
            assert_eq!(team.len(), 1);
            assert_eq!(team[0].principal, "u2");
            // The unrelated grant on another scope is untouched.
            assert_eq!(db.acls_for_scope("other").await.unwrap().len(), 1);
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
