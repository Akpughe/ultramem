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
    /// Object-storage key of the original uploaded file (Task 2b), if any.
    pub blob_key: Option<String>,
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
    /// Transaction time the memory stopped being current (was superseded), if it
    /// has been. `None` = still the latest we know. Powers bitemporal `as_of`.
    pub superseded_at: Option<i64>,
    pub extends: Option<String>,
    pub event_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub learned_at: i64,
    pub document_id: String,
    pub created_at: i64,
}

/// One evidence row: the verbatim source span supporting a memory. Written only
/// when the quote is validated as a substring of the cited chunk (never fabricated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRow {
    pub id: String,
    pub memory_id: String,
    pub document_id: String,
    pub chunk_id: Option<String>,
    pub char_start: Option<i32>,
    pub char_end: Option<i32>,
    pub quote: String,
    pub extractor: String,
}

/// A background job (e.g. a facts reindex) — retires the untracked detached
/// `tokio::spawn` with a queryable row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRow {
    pub id: String,
    pub container_tag: Option<String>,
    pub kind: String,
    pub state: String, // queued | running | done | failed
    pub progress: i32,
    pub total: Option<i32>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// An access grant: `principal` has `capability` on `scope` (a container_tag).
/// The foundation of the multi-scope company brain (8/10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclEntry {
    pub principal: String,
    pub scope: String,
    pub capability: String, // read | write | promote | admin
    pub created_at: i64,
}

/// An entity alias: within `container_tag`, the normalized surface form `alias`
/// refers to the canonical entity `canonical` (9/10 entity resolution). Explicit —
/// nothing is merged implicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasEntry {
    pub container_tag: String,
    pub alias: String,
    pub canonical: String,
    pub created_at: i64,
}

/// A forensic audit record of a mutating operation (id is DB-generated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub actor: String,
    pub container_tag: Option<String>,
    pub action: String,
    pub target_id: Option<String>,
    pub request_id: Option<String>,
    pub ts: i64,
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
    /// `is_latest = false`, record `superseded_by = new_id`, and stamp
    /// `superseded_at = ts` (the transaction time, for bitemporal `as_of`).
    async fn mark_superseded(&self, pairs: &[(String, String)], ts: i64) -> Result<(), String>;
    /// Insert memory-evidence rows (idempotent by id).
    async fn insert_evidence(&self, rows: &[EvidenceRow]) -> Result<(), String>;
    /// Fetch the current (is_latest) memory rows in `container_tag` whose
    /// `statement` is one of `statements` — the provenance join for retrieval.
    async fn memories_by_statement(
        &self,
        container_tag: &str,
        statements: &[String],
    ) -> Result<Vec<MemoryRow>, String>;
    /// Fetch the evidence rows for the given memory ids.
    async fn evidence_for(&self, memory_ids: &[String]) -> Result<Vec<EvidenceRow>, String>;
    /// Create a job row (state `queued`).
    async fn insert_job(&self, job: &JobRow) -> Result<(), String>;
    /// Update a job's state/progress/error (and bump `updated_at`).
    async fn update_job(
        &self,
        id: &str,
        state: &str,
        progress: i32,
        error: Option<&str>,
        updated_at: i64,
    ) -> Result<(), String>;
    /// Fetch a job by id, scoped to its namespace (`None` if absent/other-tenant).
    async fn get_job(&self, id: &str, container_tag: &str) -> Result<Option<JobRow>, String>;
    /// Append a forensic audit event.
    async fn insert_audit(&self, event: &AuditEvent) -> Result<(), String>;
    /// Count audit events for a namespace (for tests/inspection).
    async fn audit_count(&self, container_tag: &str) -> Result<i64, String>;
    /// The audit trail for a namespace, newest first, capped at `limit` — the
    /// read side of the forensic log (who did what, when).
    async fn audit_list(&self, container_tag: &str, limit: i64) -> Result<Vec<AuditEvent>, String>;
    /// All chunk rows for a document (ordered by `chunk_index`) — for rebuilding
    /// the vector index from the source of truth.
    async fn chunks_for_document(&self, document_id: &str) -> Result<Vec<ChunkRow>, String>;
    /// All memory rows in a namespace (for rebuilding the facts index).
    async fn memories_for_tag(
        &self,
        container_tag: &str,
        cap: i64,
    ) -> Result<Vec<MemoryRow>, String>;
    /// Bitemporal point-in-time read: the memories that were **current knowledge**
    /// as of transaction time `t` — learned at/before `t`, not yet superseded as of
    /// `t` (`superseded_at` is null or `> t`), still valid in the world at `t`
    /// (`valid_until` is null or `> t`), and not quarantined. Reconstructs "what we
    /// knew as of `t`", not just the present.
    async fn memories_as_of(
        &self,
        container_tag: &str,
        t: i64,
        cap: i64,
    ) -> Result<Vec<MemoryRow>, String>;
    /// Fetch one memory by id, scoped to its namespace (`None` if absent or in
    /// another tenant) — the source lookup for promotion.
    async fn get_memory(&self, id: &str, container_tag: &str) -> Result<Option<MemoryRow>, String>;
    /// Hard-delete a memory (and its evidence rows), scoped to its namespace.
    /// Returns whether a row was actually removed (`false` if absent or in another
    /// tenant) — the ownership gate for fact-granular forget / right-to-erasure.
    async fn delete_memory(&self, id: &str, container_tag: &str) -> Result<bool, String>;
    /// Register (or update) an entity alias in a namespace. Keyed by
    /// `(container_tag, alias)` — re-registering an alias updates its canonical.
    async fn add_alias(&self, entry: &AliasEntry) -> Result<(), String>;
    /// All entity aliases in a namespace (for resolution + the admin listing).
    async fn aliases_for_tag(&self, container_tag: &str) -> Result<Vec<AliasEntry>, String>;
    /// Grant a principal a capability on a scope (idempotent).
    async fn grant_acl(&self, entry: &AclEntry) -> Result<(), String>;
    /// Revoke a specific grant (idempotent — an absent grant is a no-op).
    async fn revoke_acl(&self, entry: &AclEntry) -> Result<(), String>;
    /// All ACL grants held by a principal.
    async fn acls_for_principal(&self, principal: &str) -> Result<Vec<AclEntry>, String>;
    /// All grants *on* a scope (who may access it) — for the admin listing.
    async fn acls_for_scope(&self, scope: &str) -> Result<Vec<AclEntry>, String>;
}
