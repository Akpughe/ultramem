//! Postgres implementation of [`Db`] using sqlx runtime queries.
//!
//! Runtime queries (not the `query!` macros) so the crate compiles with no
//! database available — CI stays hermetic; the live check is the gated
//! `pg_smoke` test below.

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

use super::{Db, DocumentRow};

/// Migrations embedded at compile time from `crates/ultramem-core/migrations/`.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub struct PgDb {
    pool: PgPool,
}

impl PgDb {
    /// Connect to Postgres (a small pool). Does not migrate — call [`Db::migrate`].
    pub async fn connect(url: &str) -> Result<Self, String> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|e| format!("postgres connect failed: {e}"))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl Db for PgDb {
    async fn health(&self) -> bool {
        sqlx::query("select 1").execute(&self.pool).await.is_ok()
    }

    async fn migrate(&self) -> Result<(), String> {
        MIGRATOR
            .run(&self.pool)
            .await
            .map_err(|e| format!("migrate failed: {e}"))
    }

    async fn insert_document(&self, d: &DocumentRow) -> Result<(), String> {
        sqlx::query(
            "insert into documents \
             (id, container_tag, source, title, reference, captured_at, processing_state, created_at) \
             values ($1,$2,$3,$4,$5,$6,$7,$8) on conflict (id) do nothing",
        )
        .bind(&d.id)
        .bind(&d.container_tag)
        .bind(&d.source)
        .bind(&d.title)
        .bind(&d.reference)
        .bind(d.captured_at)
        .bind(&d.processing_state)
        .bind(d.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("insert_document failed: {e}"))?;
        Ok(())
    }

    async fn get_document(
        &self,
        id: &str,
        container_tag: &str,
    ) -> Result<Option<DocumentRow>, String> {
        let row = sqlx::query(
            "select id, container_tag, source, title, reference, captured_at, processing_state, created_at \
             from documents where id = $1 and container_tag = $2",
        )
        .bind(id)
        .bind(container_tag)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("get_document failed: {e}"))?;
        Ok(row.map(|r| DocumentRow {
            id: r.get("id"),
            container_tag: r.get("container_tag"),
            source: r.get("source"),
            title: r.get("title"),
            reference: r.get("reference"),
            captured_at: r.get("captured_at"),
            processing_state: r.get("processing_state"),
            created_at: r.get("created_at"),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end against a live Postgres: connect → migrate → insert → read.
    /// Gated on `ULTRAMEM_PG_URL` (like the Qdrant pipeline tests), so it is a
    /// no-op in CI and runs only when a database is provided.
    #[test]
    fn pg_smoke_migrate_insert_read() {
        let Ok(url) = std::env::var("ULTRAMEM_PG_URL") else {
            eprintln!("skipped (set ULTRAMEM_PG_URL to run)");
            return;
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let db = PgDb::connect(&url).await.expect("connect");
            db.migrate().await.expect("migrate");
            assert!(db.health().await);
            let id = uuid::Uuid::new_v4().to_string();
            let doc = DocumentRow {
                id: id.clone(),
                container_tag: "pg_smoke".into(),
                source: "api".into(),
                title: "Smoke".into(),
                reference: String::new(),
                captured_at: 1,
                processing_state: "pending".into(),
                created_at: 1,
            };
            db.insert_document(&doc).await.expect("insert");
            db.insert_document(&doc)
                .await
                .expect("insert is idempotent");
            let got = db.get_document(&id, "pg_smoke").await.expect("get");
            assert_eq!(got.as_ref().map(|d| d.id.as_str()), Some(id.as_str()));
            // Tag-scoped: another namespace can't read it.
            assert!(db.get_document(&id, "other").await.expect("get2").is_none());
        });
    }
}
