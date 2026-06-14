# UltraMem — Execution Roadmap

For the session that executes this. Each phase is shippable and verifiable. Do them in order; don't start the server before the core compiles + tests pass.

> **Progress:** Phase 0 (workspace), Phase 1 (extract core), and Phase 2 (HTTP server) are **DONE** — both crates build, 60 unit tests pass, the server implements the full API. Resume at **"verify against a live Qdrant"** (port the eval harnesses + run the live integration tests), then Phase 3 (provider traits), Phase 4 (MCP), Phase 5 (publish). See `KICKOFF.md`.

## Phase 0 — Repo & workspace (start here)
1. Create the `ultramem` git repo (or `cd ultramem/` here, `git init`, later push). Add LICENSE (Apache-2.0 recommended), `.gitignore` (Rust), `README.md` (already drafted).
2. Cargo workspace: `crates/ultramem-core`, `crates/ultramem-server`. (`ultramem-mcp` is its own repo later.)
3. `cargo build` of empty crates passes.

## Phase 1 — Extract the core (the big copy)
Follow [`EXTRACTION.md`](EXTRACTION.md) exactly.
1. Copy the 13 `engine/*.rs` files + `llm.rs` into `ultramem-core/src/`.
2. Apply the 3 surgical edits (drop `settings::Role`, delete `from_settings`, fix import paths). Add the `EngineCfg` builder.
3. Add `list_document_ids(container_tag)` (Qdrant scroll, paginated) to replace the `memories_log` enumeration; rename collections to `ultramem_chunks`/`ultramem_facts` (configurable).
4. Add deps (EXTRACTION §4). `cargo build` + `cargo test --lib` green (the 72 unit tests come along).
5. **Verify against a real Qdrant:** port `memtest` + the live `RECALLY_PIPELINE_TESTS` integration tests (contradiction + multi-tenant isolation). Rename env → `ULTRAMEME_*`. These passing = the engine works standalone.
   - *Gate:* memtest 3/3 and isolation test pass before moving on.

## Phase 2 — HTTP server (`ultramem-server`)
1. axum app implementing [`API.md`](API.md): `/v1/memories`, `/v1/search`, `/v1/profile`, `/v1/timeline`, `/v1/reindex`, `DELETE /v1/memories/:id`, `/v1/health`.
2. Bearer-key auth middleware; map key → `container_tag` (enforce server-side so a client can't read another namespace — reuse Recally's verification discipline).
3. `reindex` as a background job + SSE/poll progress (port `reindex_memory_graph`).
4. Port the eval harness (`probe bench/abtest/memtest`) as `examples/` or `xtask` so SOTA claims stay reproducible.
   - *Gate:* `curl` add → search → profile round-trips; bench reproduces Recally's numbers.

## Phase 3 — Provider-agnostic (OSS-readiness)
The engine currently hardwires Jina (embed/rerank), Mistral (OCR), Groq/etc (LLM). For self-hosters:
1. Traits: `Embedder`, `Reranker`, `Ocr`, `Llm` (the design doc already proposed `Embedder` with Cohere/Voyage impls).
2. Implementations: Jina + Cohere + Voyage + OpenAI-embeddings; Mistral + (optional local) OCR; LLM via existing `llm.rs` (already multi-provider). Select via config.
3. Optional: pluggable vector store trait (Qdrant default) — note as future, not v1.
   - *Gate:* swap embedder via config without touching engine code.

## Phase 4 — MCP server (`ultramem-mcp`, separate repo)
Per [`MCP.md`](MCP.md). Base on `recally_mcp.rs`. Thin client of the HTTP API. Tools: `recall_search`, `recall_timeline`, `add_memory`, `get_profile`. Publish + one-line install snippet.

## Phase 5 — Docs & launch
1. Carry over `docs/memory-engine.md`, `memory-engine-gap-analysis.md`, `memory-engine-implementation.md` (rename/clean for UltraMem).
2. Quickstart (Docker compose: ultramem-server + Qdrant), API reference, MCP setup, "memory vs RAG" explainer, benchmark page (LongMemEval/MemScore numbers).
3. Thin SDKs (`ultramem-js`, `ultramem-py`) over the API.
4. Decide license, publish repos, write the launch post (mirror SuperMemory's positioning, lead with the open + self-hostable + provider-agnostic angle).

## Open decisions (resolve early)
- **Name:** "UltraMem" as given (consider "UltraMem"/"UltraMemory" if the `-meme` reads off-brand).
- **License:** Apache-2.0 (recommended) vs MIT (SuperMemory's choice).
- **Core language:** Rust (extract as-is) — consumers are language-agnostic via HTTP/MCP. A TS port is *not* needed for adoption.
- **Multi-tenancy depth:** v1 single static API key (single tenant) vs key→tenant table. Start single, design the seam for multi.
- **Vector store:** Qdrant required for v1; trait-abstract later.

## Definition of done (v1)
- `ultramem-server` + Qdrant via `docker compose up`; add/search/profile/reindex work; memtest 3/3; bench reproduces numbers; MCP server installable in Claude/Cursor in under a minute; docs published; repos public.
