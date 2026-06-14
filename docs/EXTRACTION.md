# Extraction Manifest — Recally engine → `ultramem-core`

> **✅ DONE.** This extraction has been executed — `crates/ultramem-core` holds the engine, decoupled and building (60 unit tests pass), plus the new `list_document_ids`. Kept as the record of exactly what moved and changed.

The engine was audited (2026-06-14): it has **zero** dependency on Tauri, `AppState`, SQLite/rusqlite, or `State<>`. Its only coupling to the rest of Recally is `crate::llm` (self-contained) and `crate::settings` used in **one** constructor. Extraction is a copy + ~3 surgical edits, not a rewrite.

## 1. Files to copy verbatim into `ultramem-core/src/`

From `recally/src-tauri/src/engine/` → `ultramem-core/src/engine/` (or flatten to `src/`):

| File | Lines | Purpose |
|---|---|---|
| `mod.rs` | 1381 | `MemoryEngine`, `EngineCfg`, `IngestDoc`, full ingest + retrieve pipeline, lifecycle wiring, filters, reindex helpers |
| `chunker.rs` | 331 | content-type-aware chunking (markdown/transcript/paragraph) |
| `memory.rs` | 260 | memory lifecycle — reconcile UPDATE/EXTEND/DUPLICATE/NEW, expiry parsing |
| `distill.rs` | 221 | fact distillation (segment → extract → merge) |
| `qdrant.rs` | 481 | Qdrant REST client (dense + hybrid search, payload ops, scroll) |
| `profile.rs` | 154 | standing user profile (static/dynamic) |
| `rewrite.rs` | 140 | retrieval planning (date/source/list rewrite) |
| `extract.rs` | 113 | Jina Reader file + URL extraction |
| `jina.rs` | 104 | embeddings + cross-encoder rerank |
| `mistral.rs` | 90 | OCR (PDF + images) |
| `sparse.rs` | 80 | BM25/IDF sparse vectors for hybrid |
| `context.rs` | 67 | contextual retrieval (off by default; keep behind flag) |
| `urlinfo.rs` | 187 | URL → readable description, junk filter |

Plus, from `recally/src-tauri/src/`:
| File | Lines | Purpose |
|---|---|---|
| `llm.rs` | 516 | `LlmClient`, `ResolvedModel`, provider kinds (OpenAI-compat/Anthropic). Self-contained. → `ultramem-core/src/llm.rs` |

**Total ≈ 4,125 lines.** All Tauri-free.

## 2. The ~3 surgical edits (decoupling)

All coupling is in `engine/mod.rs`:

1. **Drop `use crate::settings::Role;`** (line ~24).
2. **Delete `EngineCfg::from_settings(&Settings)`** (lines ~117–127). Keep `Default`, `from_env`, and add a small builder (`EngineCfg::builder()...`) — the config already carries `plan_model`/`distill_model` as `ResolvedModel`, so nothing else needs settings.
3. **Fix the `crate::llm` / `crate::settings` import paths** to the new crate (`crate::llm` stays if `llm.rs` is a sibling module).

`profile.rs` imports `super::{qdrant, EngineCfg, DEFAULT_TAG}` — all internal, no change.

That's the entire decoupling. Everything else compiles as-is once the deps are present.

## 3. New code the standalone needs (small)

Recally used its SQLite `memories_log` to **enumerate document ids** for reindex/timeline. Standalone, replace with a Qdrant scroll over distinct `doc_id`:

- Add `MemoryEngine::list_document_ids(container_tag) -> Vec<(doc_id, title, source, reference, captured_at)>` — scroll `recally_chunks` (paginate via `next_page_offset`), dedup by `doc_id`, read metadata from the first chunk's payload. This replaces the `memories_log` enumeration in `reindex_memory_graph` and powers a `timeline`/`list` endpoint.

(Collection names `recally_chunks`/`recally_facts` should become configurable / renamed `ultramem_chunks`/`ultramem_facts` in `EngineCfg`.)

## 4. Dependencies for `ultramem-core/Cargo.toml`

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json", "multipart", "stream"] }
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
base64 = "0.22"
url = "2"
futures-util = "0.3"

[dev-dependencies]
dotenvy = "0.15"
```

(`jsonwebtoken` is auth, not engine — it belongs in `ultramem-server`, not core.)

## 5. Tests / eval to port

- The `#[cfg(test)]` modules inside each engine file copy over and pass as-is (72 unit tests, several live `RECALLY_PIPELINE_TESTS=1` integration tests incl. the contradiction + multi-tenant isolation tests).
- Port `src-tauri/src/bin/probe.rs` modes (`bench`, `abtest`, `memtest`, `reindex`) and `bench_ingest.rs` into `ultramem-core/examples/` or a `xtask` — this is the SOTA-proof harness (H@k/MRR/MemScore + LongMemEval-style memory suite). Rename `RECALLY_*` env vars → `ULTRAMEME_*`.

## 6. What NOT to bring

- Anything under `capture/`, `ingest.rs`, `commands.rs`, `state.rs`, `lib.rs`, `auth.rs`, `tray.rs`, `db.rs`, `migrate.rs`, `meetings.rs`, `composio.rs` — these are Recally's app/capture/Tauri layer, not the engine.
- `groq.rs` (legacy client) — superseded by `llm.rs`; leave it.
