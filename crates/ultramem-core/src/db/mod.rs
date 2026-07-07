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

/// One document row. A subset of the `documents` table — the fields Task 1 needs;
/// it grows as later slices dual-write chunks/memories/evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentRow {
    pub id: String,
    pub container_tag: String,
    pub source: String,
    pub title: String,
    pub reference: String,
    pub captured_at: i64,
    pub processing_state: String,
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
}
