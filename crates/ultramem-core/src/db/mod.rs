//! Phase A — the relational source of truth (Task 1: scaffold).
//!
//! Qdrant stays the vector index; this layer owns the authoritative
//! document/chunk/memory/evidence/job/audit rows (see `migrations/0001_init.sql`
//! and `docs/PHASE_A_SCOPING.md`). It is introduced behind a trait — mirroring
//! the `VectorStore` provider seam — so nothing in the engine hardwires Postgres
//! and tests stay hermetic (`db::mock::MockDb`).
//!
//! **Scaffold only.** The engine does not dual-write yet; `EngineCfg::pg_url`
//! being `None` (the default) means the engine behaves exactly as before. Only
//! the `documents` operations exist so far — the trait grows one slice at a time.

use async_trait::async_trait;

pub mod pg;
pub use pg::PgDb;

#[cfg(test)]
pub mod mock;

/// One document row. A subset of the `documents` table — the fields the current
/// slices need; it grows as later slices dual-write memories/evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentRow {
    pub id: String,
    pub container_tag: String,
    pub source: String,
    pub title: String,
    pub reference: String,
    /// sha256 of the (scrubbed) extracted text — the doc-level dedup key.
    pub content_hash: Option<String>,
    /// Normalized reference URL (tracking params stripped) — the other dedup key.
    pub canonical_url: Option<String>,
    pub captured_at: i64,
    pub processing_state: String,
    pub created_at: i64,
}

/// One chunk row (mirrors the embedded chunk; `id` equals the Qdrant point id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkRow {
    pub id: String,
    pub document_id: String,
    pub chunk_index: i32,
    pub content: String,
    pub embed_model: String,
    pub dim: i32,
}

/// One memory row (mirrors a distilled fact; `id` equals the Qdrant fact point
/// id). `kind`/`confidence`/`event_from` are populated by the schema'd extractor
/// in Task 4b; for now they default (`unknown`/`None`).
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRow {
    pub id: String,
    pub container_tag: String,
    pub kind: String,
    pub statement: String,
    pub confidence: Option<f32>,
    pub is_latest: bool,
    pub needs_review: bool,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    pub extends: Option<String>,
    pub event_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub learned_at: i64,
    pub document_id: String,
    pub created_at: i64,
}

/// The relational source of truth. Connection state lives in the impl; the engine
/// holds an `Option<Arc<dyn Db>>` and uses it only when configured.
#[async_trait]
pub trait Db: Send + Sync {
    /// Cheap reachability check (`select 1`).
    async fn health(&self) -> bool;
    /// Apply pending schema migrations (idempotent).
    async fn migrate(&self) -> Result<(), String>;
    /// Insert a document row; a duplicate id is a no-op (idempotent backfill).
    async fn insert_document(&self, doc: &DocumentRow) -> Result<(), String>;
    /// Fetch a document by id, scoped to its namespace (`None` if absent/other-tenant).
    async fn get_document(
        &self,
        id: &str,
        container_tag: &str,
    ) -> Result<Option<DocumentRow>, String>;
    /// Insert chunk rows (idempotent by id).
    async fn upsert_chunks(&self, chunks: &[ChunkRow]) -> Result<(), String>;
    /// Doc-level dedup: return an existing document id in `container_tag` whose
    /// `content_hash` matches, or whose `canonical_url` matches (when provided) —
    /// so a re-capture of identical text or the same page isn't re-ingested.
    async fn find_document_id(
        &self,
        container_tag: &str,
        content_hash: &str,
        canonical_url: Option<&str>,
    ) -> Result<Option<String>, String>;
    /// The document registry for a namespace, newest first, for the timeline —
    /// an indexed query that replaces the full-collection Qdrant scroll. `before`
    /// (exclusive `captured_at` upper bound) paginates; `limit` caps the page.
    async fn list_documents(
        &self,
        container_tag: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<Vec<DocumentRow>, String>;
    /// Insert distilled memory rows (idempotent by id).
    async fn insert_memories(&self, memories: &[MemoryRow]) -> Result<(), String>;
    /// Mirror a supersession: for each `(old_id, new_id)`, mark the old memory
    /// `is_latest = false` and record `superseded_by = new_id`.
    async fn mark_superseded(&self, pairs: &[(String, String)]) -> Result<(), String>;
}
