# UltraMem — Kickoff

Everything in this folder is ready. The Rust **engine** (`ultramem-core`) and **HTTP server** (`ultramem-server`) are already extracted, decoupled, and **building** (60 engine unit tests pass). What remains is: verify against a live Qdrant, make providers swappable, build the MCP server, and publish.

## How to use this

1. Move this `ultramem/` folder to wherever you want the repo to live (or keep it here).
2. Open Claude Code **in this `ultramem/` directory**.
3. Paste the prompt in the box below.

---

## ⤵️ PASTE THIS INTO CLAUDE CODE

> You are working in the UltraMem repo — an open-source memory engine for AI agents, extracted from the Recally project. Read `README.md`, `KICKOFF.md`, and everything in `docs/` first (especially `ROADMAP.md`, `EXTRACTION.md`, `API.md`, `MCP.md`). The Rust workspace (`ultramem-core` engine + `ultramem-server` HTTP API) is already extracted and builds — confirm with `cargo build`. Then execute the roadmap in order, treating each phase's gate as a checkpoint you must verify before moving on:
>
> 1. **Verify the core against a live Qdrant.** Create a `.env` from `.env.example` (ask me for the keys, or use my Recally `.env` values for QDRANT/JINA/MISTRAL/GROQ). Port Recally's eval harnesses — `probe` modes `memtest`, `bench`, `abtest`, `reindex` and `bench_ingest` — from `../recally/src-tauri/src/bin/` into `crates/ultramem-core/examples/` (or an `xtask`), renaming `RECALLY_*` env → `ULTRAMEM_*` and any `recally_*` collection names → `ultramem_*`. Run `memtest` and the live `ULTRAMEM_PIPELINE_TESTS=1 cargo test` integration tests (contradiction + multi-tenant isolation). **Gate: memtest 3/3 and the isolation test pass.**
> 2. **Exercise the server end-to-end.** `cargo run -p ultramem-server`, then `curl` the flow: `POST /v1/memories` → `POST /v1/search` → `GET /v1/profile` → `GET /v1/timeline` → `POST /v1/reindex`. Fix anything that doesn't round-trip. Add request/response examples to `docs/API.md`. **Gate: add → search returns the document; profile compiles.**
> 3. **Make providers swappable (OSS-readiness).** Introduce `Embedder` / `Reranker` / `Ocr` / `Llm` traits in `ultramem-core` so Jina/Mistral/Groq aren't hardwired. Provide Jina + (one alternative, e.g. OpenAI embeddings) behind config. The LLM client (`llm.rs`) is already multi-provider — wire it through `EngineCfg::with_models`. **Gate: swap the embedder via config without touching engine code.**
> 4. **Polish docs + Docker.** Verify `docker compose up` brings up Qdrant + the server and the curl flow works against it. Tighten `README.md`, write a "memory vs RAG" explainer, and a benchmark page using the harness numbers (H@k / MRR / MemScore / LongMemEval-style). Add the full **Apache-2.0** `LICENSE` text and a `CONTRIBUTING.md`.
> 5. **Initialize and publish.** `git init`, first commit, push to a new public GitHub repo `ultramem`. (Decide org/owner with me first.)
> 6. **MCP server** is a SEPARATE repo (`ultramem-mcp`) — see `docs/MCP.md`. Scaffold it as a thin client of this HTTP API with tools `recall_search`, `recall_timeline`, `add_memory`, `get_profile`, basing it on `../recally/src-tauri/src/bin/recally_mcp.rs`. Publish it with a one-line install snippet.
>
> Work gradually, keep `cargo build` + tests green at every step, and check in with me at each gate before proceeding. Do not invent provider APIs — if unsure about an SDK/endpoint, fetch current docs.

---

## What's already done (so you don't redo it)
- ✅ Engine extracted verbatim from Recally into `crates/ultramem-core/src/engine/` + `llm.rs`, decoupled from Tauri/SQLite (the only edits were dropping `settings::Role`, deleting `from_settings`, renaming collections to `ultramem_*`, default tag → `"default"`, and adding `with_models`).
- ✅ `list_document_ids` (Qdrant scroll) added — UltraMem's document registry, replacing Recally's SQLite enumeration.
- ✅ `ultramem-server` (axum) implementing `docs/API.md`: `/v1/memories`, `/search`, `/profile`, `/timeline`, `/reindex`, `DELETE`, `/health`, Bearer-key auth.
- ✅ `docker-compose.yml`, `Dockerfile`, `.env.example`.
- ✅ Full docs: `README`, `API`, `MCP`, `EXTRACTION`, `ROADMAP`, and the carried-over design/gap/implementation docs.
- ✅ `cargo build` (workspace) and `cargo test -p ultramem-core` (60 tests) pass.

## Open decisions to confirm with the user
- GitHub org/owner for the repo(s).
- License: **Apache-2.0** (recommended, already set in `Cargo.toml`).
- Whether to keep Groq as the default LLM or switch the public default to OpenAI/Anthropic.
