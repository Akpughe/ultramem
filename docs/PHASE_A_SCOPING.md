# Phase A — Postgres source of truth + typed memory records (scoping)

*The 7/10 rung from `docs/FABLE5_EXECUTION_PLAN.md`. This is the concrete,
repo-grounded scope for the next major step, written against the code as it
stands after Sprints 1A–1C (merged to `main`). It is a plan to execute, not
another audit. Date: 2026-07-07.*

## 1. Goal — what "done" means

Postgres becomes the **source of truth** for documents, memories, their evidence,
jobs, and an audit trail. Qdrant drops to a **pure vector index** that can be
rebuilt from Postgres at any time. Facts stop being flat strings and become typed
rows with provenance.

Pass/fail for the rung:
- **Recoverability:** dropping and rebuilding the Qdrant collections from Postgres
  reproduces identical search results (a `rebuild-index` xtask + parity test).
- **Provenance:** every memory returned by `/v1/search` carries `kind`,
  `confidence`, and at least one evidence row (document id + char span whose quote
  is a verified substring of the source chunk). `evidence_is_grounded` gates it.
- **Enumeration without scroll-alls:** `/v1/timeline` and the document registry
  read from the `documents` table, not a 50k-point Qdrant scroll.
- **Durability:** original uploads are retrievable (today they're deleted after
  ingest); a document's processing state is a queryable row, not always `"done"`.
- **No regressions:** the full 1A–1C suite still green; migration is reversible
  behind a flag.

Explicitly **out of scope** for Phase A (later phases): scopes/ACLs hierarchy
(Phase D), the entity graph + resolution (Phase F), the review-queue *UI* (Phase D
product surface — though the `NeedsReview` data already exists from Sprint 1B and
Phase A makes it queryable).

## 2. The inversion — what lives where

| Data | Today | After Phase A |
|---|---|---|
| Document metadata + processing state | synthesized by scrolling chunks (`list_document_ids`, 50k cap) | `documents` table (authoritative) |
| Original uploaded file | deleted post-ingest (`main.rs` temp cleanup) | object storage (S3/MinIO), pointer in `documents` |
| Chunk text + metadata | Qdrant payload (only copy) | `chunks` table (authoritative) + Qdrant point (vector + thin payload) |
| Fact/memory | Qdrant fact payload (flat `fact` string + lifecycle fields) | `memories` table (typed) + Qdrant point (vector + thin payload) |
| Evidence (why a memory exists) | none | `memory_evidence` table |
| Async work (reindex) | detached `tokio::spawn`, no record | `jobs` table |
| Who did what | none | `audit_events` table |
| Vectors + filterable keys | Qdrant | Qdrant (unchanged role) |

**Qdrant's thin payload after Phase A** (only what filtered ANN needs):
`{ memory_id | chunk_id, container_tag, is_latest, needs_review, valid_until, kind, source }`.
Everything else moves to Postgres. Qdrant becomes rebuildable → that's the backup
story and the embedder-migration path (re-embed from `chunks.content`).

## 3. First-slice schema (minimal to reach 7/10)

Five tables. DDL sketch (Postgres); timestamps unix-epoch `bigint` to match the
engine's existing `captured_at` convention.

```sql
create table documents (
  id            uuid primary key,
  container_tag text not null,
  source        text not null,                 -- clipboard|browser|file|meeting|api|web
  title         text not null default '',
  reference     text not null default '',      -- URL/path/canonical id
  canonical_url text,                            -- normalized (utm-stripped) for dedup
  content_hash  text,                            -- sha256 of extracted text, for dedup
  blob_key      text,                            -- object-storage key of the original upload
  captured_at   bigint not null,
  source_published_at bigint,                    -- when known (articles); vs capture time
  processing_state text not null default 'pending', -- pending|chunked|distilled|failed
  error         text,
  created_at    bigint not null,
  unique (container_tag, canonical_url),
  unique (container_tag, content_hash)           -- doc-level dedup (Sprint 1B backlog #11)
);
create index on documents (container_tag, captured_at desc);  -- timeline, no scroll-all

create table chunks (
  id           uuid primary key,               -- == the Qdrant point id
  document_id  uuid not null references documents(id) on delete cascade,
  chunk_index  int  not null,
  content      text not null,                   -- the authoritative chunk text
  char_start   int, char_end int,               -- offset in the (scrubbed) document text
  embed_model  text not null,                   -- guards mixed-dim corruption
  dim          int  not null
);
create index on chunks (document_id);

create table memories (
  id           uuid primary key,               -- == the Qdrant fact point id
  container_tag text not null,
  kind         text not null default 'unknown',-- preference|personal_fact|project_fact|decision|task|event|claim|quote|relationship|unknown
  statement    text not null,                   -- the standalone fact (was payload.fact)
  confidence   real,                            -- 0..1; null for legacy
  is_latest    boolean not null default true,
  needs_review boolean not null default false,  -- Sprint 1B quarantine, now a column
  supersedes   uuid references memories(id),
  superseded_by uuid references memories(id),   -- reversible (was write-only in Qdrant)
  extends      uuid references memories(id),
  event_from   bigint, event_to bigint,         -- parsed [on YYYY-MM-DD] (today trapped in text)
  valid_until  bigint,                          -- parsed [until ...]
  learned_at   bigint not null,                 -- transaction time (distinct from event time)
  document_id  uuid not null references documents(id) on delete cascade,
  created_at   bigint not null
);
create index on memories (container_tag, is_latest, needs_review);

create table memory_evidence (
  id           uuid primary key,
  memory_id    uuid not null references memories(id) on delete cascade,
  document_id  uuid not null references documents(id) on delete cascade,
  chunk_id     uuid references chunks(id) on delete set null,
  char_start   int, char_end int,
  quote        text not null,                   -- must be a substring of the chunk (validated)
  extractor    text not null                    -- model id + prompt version
);
create index on memory_evidence (memory_id);

create table jobs (
  id           uuid primary key,
  container_tag text,
  kind         text not null,                   -- reindex|backfill|rebuild_index
  state        text not null default 'queued',  -- queued|running|done|failed
  progress     int not null default 0, total int,
  error        text,
  created_at   bigint not null, updated_at bigint not null
);

create table audit_events (
  id           bigserial primary key,
  actor        text not null,                   -- credential/tenant id
  container_tag text,
  action       text not null,                   -- ingest|search|delete|reindex|promote
  target_id    uuid,
  request_id   text,
  ts           bigint not null
);
create index on audit_events (container_tag, ts desc);
```

`memories.kind`, `confidence`, `event_from/to`, and `superseded_by` are the typed
upgrades over today's flat Qdrant payload. `is_latest`/`needs_review`/`valid_until`/
`supersedes`/`extends` already exist in the payload (Sprint 1B) → they migrate 1:1.

## 4. Migration mechanics — dual-write, backfill, parity, cutover

The whole point is to introduce Postgres **without breaking the running Qdrant
read path**. Sequence:

1. **Add the store, write nothing load-bearing.** Introduce a `Db` trait +
   `PgDb` impl behind `EngineCfg` (like the existing provider seams). Add
   `sqlx` + a `migrations/` dir + Postgres to `docker-compose.yml`. `ULTRAMEM_PG_URL`
   unset → engine behaves exactly as today (feature-flagged off).
2. **Dual-write.** When `PgDb` is configured, `add_document`/`index_memories`/
   `delete_document_tagged` write to Postgres **and** Qdrant. Reads still come from
   Qdrant. Original uploads go to object storage instead of being deleted.
3. **Backfill xtask.** `migrate-from-qdrant`: scroll existing chunks + facts →
   insert `documents`/`chunks`/`memories` rows (all lifecycle fields already in the
   payloads). Idempotent (upsert by point id), re-runnable, resumable.
4. **Parity gate.** `migration_parity` test: a fixed query set returns the same
   top-k whether the registry/metadata comes from Qdrant or Postgres; row counts
   match; `evidence_is_grounded` holds for re-distilled memories.
5. **Cutover, one read path at a time, each behind the flag:** document
   registry/timeline → PG; then memory metadata on retrieve → PG (Qdrant returns
   ids + scores, PG returns authoritative rows); then delete/reindex. Qdrant stays
   the vector search; PG becomes the source of the returned content.
6. **Rollback:** the flag reverts every read to the Qdrant-only path; dual-write
   means Qdrant is never behind.

## 5. Engine seam (files likely touched)

- `crates/ultramem-core/Cargo.toml`, root `Cargo.toml` — add `sqlx` (postgres,
  runtime-tokio-rustls, macros, migrate, uuid, chrono).
- New `crates/ultramem-core/src/db/` — `Db` trait + `PgDb` + `migrations/`.
- `engine/mod.rs` — `add_document` (write `documents`/`chunks` rows; store original;
  set `processing_state`), `index_memories` (write typed `memories` + `memory_evidence`;
  the transactional supersession from Sprint 1B maps naturally to a PG transaction),
  `delete_document_tagged` (delete cascades via FK; still delete Qdrant points),
  `list_document_ids` → PG query (retire the 50k scroll), retrieve (join Qdrant hits →
  PG rows for authoritative fields + evidence).
- `providers/mock.rs` — extend with an in-memory `Db` mock (mirrors `MemStore`) so
  the lifecycle/forget/migration tests stay offline.
- `ultramem-server/src/main.rs` — `/v1/documents` + `/v1/documents/:id/status`
  (processing state), real `/v1/jobs/:id` (replaces the detached spawn), audit
  middleware. Retrieve response gains `kind`/`confidence`/`evidence` per memory.
- `docker-compose.yml`, `Dockerfile`, `.env.example` — Postgres service + `ULTRAMEM_PG_URL`.

The `Db` trait mirrors the `VectorStore` pattern (trait + impl + mock), so nothing
in the engine hardwires Postgres and tests stay hermetic.

## 6. Sequenced tasks (each a PR-sized slice, acceptance test named)

1. **Scaffold `Db` trait + `PgDb` + migrations + compose Postgres.** *Accept:*
   `sqlx migrate run` creates the 5 tables; engine unchanged when `ULTRAMEM_PG_URL`
   unset. *Test:* `pg_smoke` (gated, like pipeline tests) + in-memory `Db` mock compiles.
2. **`documents` + `chunks` dual-write on ingest** (+ object storage for originals,
   content-hash/canonical-URL dedup). *Accept:* re-ingesting an identical file creates
   no second `documents` row; the original is retrievable. *Test:* `dedup_no_duplicate_document` (mock Db).
3. **Registry/timeline read from PG.** *Accept:* `/v1/timeline` returns from `documents`;
   no 50k scroll on that path. *Test:* `timeline_reads_pg`.
4. **Typed `memories` + `memory_evidence` dual-write.** Parse `[on …]`→`event_from`;
   emit `kind`/`confidence`/evidence from distillation (schema'd extraction). *Accept:*
   every stored memory has ≥1 grounded evidence row. *Test:* `evidence_is_grounded`.
5. **Retrieve returns provenance from PG.** Qdrant → ids+scores; PG → statement, kind,
   confidence, evidence. *Accept:* `/v1/search` facts carry source + evidence. *Test:* `retrieve_carries_provenance` (mock Db + mock store).
6. **`jobs` table + real `/v1/jobs/:id`; `audit_events` + middleware.** *Accept:* reindex
   is a tracked job; every write emits one audit row. *Test:* `job_status_roundtrip`, `writes_are_audited`.
7. **Backfill xtask + parity gate.** *Accept:* `migration_parity` passes; counts match.
8. **`rebuild-index` xtask (Qdrant from PG) + recoverability test.** *Accept:* drop Qdrant,
   rebuild, identical top-k. *Test:* `rebuild_reproduces_search` (gated).
9. **Cutover flag flip + docs.** *Accept:* default config uses PG SoT; rollback flag restores
   Qdrant-only; `docs/API.md` + `HOW-IT-WORKS.md` updated.

## 7. Risks & fallbacks

- **Two stores, no distributed transaction.** Mitigation: Postgres is the SoT and is
  written first inside a PG transaction; Qdrant is a derived index reconciled by the
  `rebuild-index` xtask if it drifts. A Qdrant write failure after a PG commit is a
  recoverable index gap, not data loss (the inverse of today's "Qdrant loss = total loss").
- **Evidence spans are fuzzy** (LLM char offsets). Mitigation: validate the `quote` is a
  substring of the cited chunk; if not, keep the memory, drop the span, flag `needs_review` —
  reuse the Sprint 1B quarantine path. Never fabricate a span.
- **Backfill of legacy data lacks types/spans.** Mitigation: import with `kind='unknown'`,
  `confidence=null`, no evidence; a background `reindex mode=facts` re-distills into typed rows
  over time. Legacy rows stay searchable meanwhile.
- **Migration risk on a live store.** Mitigation: everything is behind `ULTRAMEM_PG_URL`/flag;
  dual-write + parity gate before any read cuts over; rollback is one flag.
- **Scope creep into Phase D/F.** Keep `container_tag` as the scope key for Phase A (do NOT
  build the scope hierarchy or entity graph here); the schema leaves room (`container_tag` column)
  without committing to it.

## 8. Open decisions (need a call before building)

1. **Postgres driver/runtime:** `sqlx` (compile-checked queries, async, migrations) vs `diesel`.
   *Recommend `sqlx`* — async fits the tokio engine, and `migrate!` is simple.
2. **Object storage for originals:** include in Phase A (recommended — cheap, closes the
   data-loss gap) or defer? If included: S3-compatible (`aws-sdk-s3`/MinIO) vs local-fs for v1.
   *Recommend local-fs behind a trait for v1*, S3 impl later.
3. **Chunk text: dual-store or PG-only?** Keeping `content` in both PG and the Qdrant payload
   is simplest for the transition; PG-only (thin Qdrant payload) is the end state. *Recommend
   dual-store during transition, thin-payload at cutover.*
4. **Typed-extraction now or incremental?** Emit `kind`/`confidence`/evidence from a schema'd
   extractor in Phase A (task 4), or land the tables first and backfill types later? *Recommend
   schema'd extraction in task 4* so new data is typed from day one; legacy backfills as `unknown`.
5. **`captured_at` bigint vs `timestamptz`.** *Recommend bigint* to match the engine's existing
   unix-epoch convention and avoid churn; revisit if human-facing time queries need it.

## 9. First move

Task 1 (scaffold `Db` trait + `PgDb` + migrations + compose Postgres, all behind
`ULTRAMEM_PG_URL`, engine unchanged when unset) is the safe, reversible starting slice —
it introduces the seam and the infra without touching the read path. Everything else stacks
on it. Recommend confirming the five open decisions above, then executing tasks 1–9 in order,
one PR per slice, same green-CI→merge flow as Sprints 1A–1C.
