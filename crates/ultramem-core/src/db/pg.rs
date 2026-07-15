//! Postgres implementation of [`Db`] using sqlx runtime queries.
//!
//! Runtime queries (not the `query!` macros) so the crate compiles with no
//! database available — CI stays hermetic; the live check is the gated
//! `pg_smoke` test below.

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

use super::{ChunkRow, Db, DocumentRow, EvidenceRow, MemoryRow};

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
             (id, container_tag, source, title, reference, content_hash, canonical_url, \
              captured_at, processing_state, created_at) \
             values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) on conflict (id) do nothing",
        )
        .bind(&d.id)
        .bind(&d.container_tag)
        .bind(&d.source)
        .bind(&d.title)
        .bind(&d.reference)
        .bind(&d.content_hash)
        .bind(&d.canonical_url)
        .bind(d.captured_at)
        .bind(&d.processing_state)
        .bind(d.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("insert_document failed: {e}"))?;
        Ok(())
    }

    async fn upsert_chunks(&self, chunks: &[ChunkRow]) -> Result<(), String> {
        for c in chunks {
            sqlx::query(
                "insert into chunks (id, document_id, chunk_index, content, embed_model, dim) \
                 values ($1,$2,$3,$4,$5,$6) on conflict (id) do nothing",
            )
            .bind(&c.id)
            .bind(&c.document_id)
            .bind(c.chunk_index)
            .bind(&c.content)
            .bind(&c.embed_model)
            .bind(c.dim)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("upsert_chunks failed: {e}"))?;
        }
        Ok(())
    }

    async fn find_document_id(
        &self,
        container_tag: &str,
        content_hash: &str,
        canonical_url: Option<&str>,
    ) -> Result<Option<String>, String> {
        let row = sqlx::query(
            "select id from documents \
             where container_tag = $1 and (content_hash = $2 or ($3 is not null and canonical_url = $3)) \
             limit 1",
        )
        .bind(container_tag)
        .bind(content_hash)
        .bind(canonical_url)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("find_document_id failed: {e}"))?;
        Ok(row.map(|r| r.get::<String, _>("id")))
    }

    async fn list_documents(
        &self,
        container_tag: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<Vec<DocumentRow>, String> {
        let rows = sqlx::query(
            "select id, container_tag, source, title, reference, content_hash, canonical_url, \
             captured_at, processing_state, created_at \
             from documents \
             where container_tag = $1 and ($2::bigint is null or captured_at < $2) \
             order by captured_at desc limit $3",
        )
        .bind(container_tag)
        .bind(before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("list_documents failed: {e}"))?;
        Ok(rows
            .into_iter()
            .map(|r| DocumentRow {
                id: r.get("id"),
                container_tag: r.get("container_tag"),
                source: r.get("source"),
                title: r.get("title"),
                reference: r.get("reference"),
                content_hash: r.get("content_hash"),
                canonical_url: r.get("canonical_url"),
                captured_at: r.get("captured_at"),
                processing_state: r.get("processing_state"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn get_document(
        &self,
        id: &str,
        container_tag: &str,
    ) -> Result<Option<DocumentRow>, String> {
        let row = sqlx::query(
            "select id, container_tag, source, title, reference, content_hash, canonical_url, \
             captured_at, processing_state, created_at \
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
            content_hash: r.get("content_hash"),
            canonical_url: r.get("canonical_url"),
            captured_at: r.get("captured_at"),
            processing_state: r.get("processing_state"),
            created_at: r.get("created_at"),
        }))
    }

    async fn insert_memories(&self, memories: &[MemoryRow]) -> Result<(), String> {
        for m in memories {
            sqlx::query(
                "insert into memories \
                 (id, container_tag, kind, statement, confidence, is_latest, needs_review, \
                  supersedes, superseded_by, extends, event_from, valid_until, learned_at, \
                  document_id, created_at) \
                 values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) \
                 on conflict (id) do nothing",
            )
            .bind(&m.id)
            .bind(&m.container_tag)
            .bind(&m.kind)
            .bind(&m.statement)
            .bind(m.confidence)
            .bind(m.is_latest)
            .bind(m.needs_review)
            .bind(&m.supersedes)
            .bind(&m.superseded_by)
            .bind(&m.extends)
            .bind(m.event_from)
            .bind(m.valid_until)
            .bind(m.learned_at)
            .bind(&m.document_id)
            .bind(m.created_at)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("insert_memories failed: {e}"))?;
        }
        Ok(())
    }

    async fn mark_superseded(&self, pairs: &[(String, String)]) -> Result<(), String> {
        for (old_id, new_id) in pairs {
            sqlx::query("update memories set is_latest = false, superseded_by = $2 where id = $1")
                .bind(old_id)
                .bind(new_id)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("mark_superseded failed: {e}"))?;
        }
        Ok(())
    }

    async fn insert_evidence(&self, rows: &[EvidenceRow]) -> Result<(), String> {
        for e in rows {
            sqlx::query(
                "insert into memory_evidence \
                 (id, memory_id, document_id, chunk_id, char_start, char_end, quote, extractor) \
                 values ($1,$2,$3,$4,$5,$6,$7,$8) on conflict (id) do nothing",
            )
            .bind(&e.id)
            .bind(&e.memory_id)
            .bind(&e.document_id)
            .bind(&e.chunk_id)
            .bind(e.char_start)
            .bind(e.char_end)
            .bind(&e.quote)
            .bind(&e.extractor)
            .execute(&self.pool)
            .await
            .map_err(|err| format!("insert_evidence failed: {err}"))?;
        }
        Ok(())
    }

    async fn memories_by_statement(
        &self,
        container_tag: &str,
        statements: &[String],
    ) -> Result<Vec<MemoryRow>, String> {
        let rows = sqlx::query(
            "select id, container_tag, kind, statement, confidence, is_latest, needs_review, \
             supersedes, superseded_by, extends, event_from, valid_until, learned_at, \
             document_id, created_at \
             from memories \
             where container_tag = $1 and is_latest = true and statement = any($2)",
        )
        .bind(container_tag)
        .bind(statements)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("memories_by_statement failed: {e}"))?;
        Ok(rows
            .into_iter()
            .map(|r| MemoryRow {
                id: r.get("id"),
                container_tag: r.get("container_tag"),
                kind: r.get("kind"),
                statement: r.get("statement"),
                confidence: r.get("confidence"),
                is_latest: r.get("is_latest"),
                needs_review: r.get("needs_review"),
                supersedes: r.get("supersedes"),
                superseded_by: r.get("superseded_by"),
                extends: r.get("extends"),
                event_from: r.get("event_from"),
                valid_until: r.get("valid_until"),
                learned_at: r.get("learned_at"),
                document_id: r.get("document_id"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn evidence_for(&self, memory_ids: &[String]) -> Result<Vec<EvidenceRow>, String> {
        let rows = sqlx::query(
            "select id, memory_id, document_id, chunk_id, char_start, char_end, quote, extractor \
             from memory_evidence where memory_id = any($1)",
        )
        .bind(memory_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("evidence_for failed: {e}"))?;
        Ok(rows
            .into_iter()
            .map(|r| EvidenceRow {
                id: r.get("id"),
                memory_id: r.get("memory_id"),
                document_id: r.get("document_id"),
                chunk_id: r.get("chunk_id"),
                char_start: r.get("char_start"),
                char_end: r.get("char_end"),
                quote: r.get("quote"),
                extractor: r.get("extractor"),
            })
            .collect())
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
                content_hash: Some("deadbeef".into()),
                canonical_url: None,
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
            // Chunk dual-write + content-hash dedup.
            db.upsert_chunks(&[ChunkRow {
                id: uuid::Uuid::new_v4().to_string(),
                document_id: id.clone(),
                chunk_index: 0,
                content: "chunk text".into(),
                embed_model: "mock".into(),
                dim: 3,
            }])
            .await
            .expect("upsert_chunks");
            assert_eq!(
                db.find_document_id(&doc.container_tag, "deadbeef", None)
                    .await
                    .expect("dedup lookup"),
                Some(id.clone())
            );
            assert!(db
                .find_document_id(&doc.container_tag, "nomatch", None)
                .await
                .expect("dedup miss")
                .is_none());
        });
    }
}
