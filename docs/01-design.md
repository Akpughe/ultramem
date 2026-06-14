# Recally Memory Engine — Replacing SuperMemory with Our Own Infrastructure

**Status:** Design proposal (2026-06-12)
**Goal:** Replace the locally-spawned SuperMemory sidecar with a cloud-backed memory engine built on Qdrant + hosted embeddings (Jina/Cohere/Voyage) + Mistral OCR + Groq, eliminating all on-device ML inference while matching or exceeding SuperMemory's retrieval quality.

---

## Part 1 — How SuperMemory Actually Works

This is a distillation of their docs (intro, how-it-works, graph-memory, content-types, super-rag, memory-vs-rag, container-tags, SDK/Mastra/LangGraph integrations), focused on what we need to replicate.

### 1.1 The core idea: Documents vs Memories

SuperMemory's central design decision is splitting storage into two layers that share one context pool:

| | **Documents (RAG layer)** | **Memories (understanding layer)** |
|---|---|---|
| What | Raw input: text, PDFs, web pages, images, transcripts | LLM-extracted facts, preferences, episodes |
| State | Stateless, universal, unversioned | Stateful, temporal, per-entity |
| Stored as | Semantic chunks + embeddings | Fact nodes in a graph, linked to source chunks |
| Answers | "What do I know?" | "What do I remember about you?" |
| Recally analog | Clipboard text, files, browser pages, transcripts | "Davak is building a Tauri app", "User's meetings with X are about Y" |

A 50-page PDF doesn't just become 200 chunks — it also yields dozens of *distilled facts* that are individually searchable. Recally already consumes both layers: `/v3/search` (chunks) and `/v4/search` (distilled facts) are queried in parallel in `commands.rs`.

### 1.2 The graph: facts built on facts

Not a classic entity–relation–entity triple store. When new content arrives, an LLM extracts facts and connects each one to existing memories via **three relationship types**:

1. **UPDATES** — new fact contradicts an old one. The old memory is kept (history) but flagged `isLatest = false`. Searches return only the latest version. This is what makes "I switched from Adidas to Puma" return Puma, not the semantically-closest "I love Adidas".
2. **EXTENDS** — new fact enriches an old one without replacing it ("Alex is a PM at Stripe" + "Alex leads payments infra"). Both stay valid and searchable.
3. **DERIVES** — the system *infers* a new fact from patterns across memories ("PM at Stripe" + "always discusses payment APIs" → "likely works on core payments").

Plus **memory types** with different lifecycle behavior:

| Type | Example | Behavior |
|---|---|---|
| Fact | "Davak uses Tauri 2" | Persists until UPDATED |
| Preference | "Prefers concise answers" | Strengthens with repetition |
| Episode | "Met Alex for coffee Tuesday" | Decays unless significant |

And **automatic forgetting**: time-bound facts ("exam tomorrow") expire after their date; noise (casual filler) never becomes a memory; contradictions resolve via UPDATES.

**Key insight: none of this is magic — it's an LLM pass at ingest time.** Extract facts → embed each fact → find nearest existing memories → ask the LLM "does this update, extend, or stand alone?" → write edges. That entire loop is buildable with Groq + Qdrant.

### 1.3 The processing pipeline

Every document moves through visible states (Recally already polls these in `ingest.rs`):

```
queued → extracting → chunking → embedding → indexing → done | failed
```

- **Extracting** — type-specific: OCR for images/scanned PDFs, transcription for audio, readability-cleanup for URLs, textual extraction for Office docs.
- **Chunking ("Super RAG")** — strategy per content type:
  - Documents/PDF: semantic sections (headers, paragraphs, logical boundaries)
  - Markdown: heading hierarchy
  - Code: AST-aware (their open-source `code-chunk` lib) — functions/classes stay intact
  - Web: article structure after stripping nav/ads
- **Embedding** — vectors per chunk (model choice abstracted away from the user).
- **Indexing** — the graph pass: fact extraction + relationship classification described above.

### 1.4 Retrieval

- **Hybrid search by default** (`searchMode: "hybrid"`): chunk similarity (RAG) + memory facts, merged.
- **Reranking** (optional, ~+100 ms): cross-encoder rescores top results — precision boost for technical queries.
- **Query rewriting** (optional): expands short queries ("how to auth" → "authentication login oauth jwt") — recall boost.
- **User profiles**: a standing summary compiled from the graph, split into **static** facts (always-true context the agent should always know) and **dynamic** context (recent episodic activity). The `profile(containerTag, q)` call returns static + dynamic + query-relevant search results in one shot. Their Mastra/LangGraph integrations inject exactly this into the system prompt before each LLM call — that's the whole "agent memory" trick.

### 1.5 Container tags

A namespace string (`^[a-zA-Z0-9_:-]+$`, ≤100 chars) that hard-isolates memory pools — each tag maps to its own vector namespace, auto-created on first write, and flows through add/search/list/update/delete. Recally uses a single tag, `recally_main`. For us this maps to a Qdrant payload field (or separate collections if we ever do multi-profile).

### 1.6 Why SuperMemory is good — the checklist to replicate

1. Two-layer storage (raw chunks + distilled facts) sharing one pool
2. Temporal correctness (isLatest, expiry, contradiction resolution) — the thing plain RAG can't do
3. Content-type-aware extraction & chunking
4. Hybrid retrieval + rerank + query rewrite
5. Standing user profile (static/dynamic) for zero-latency "always-known" context
6. Async pipeline with status visibility and namespace isolation

---

## Part 2 — What Recally Does Today (and why laptops get hot)

### 2.1 Current architecture

```
capture sources                 ingest                    engine (THE PROBLEM)
┌─────────────────┐      ┌──────────────────┐      ┌──────────────────────────┐
│ clipboard (1s)   │      │ ingest_queue     │      │ SuperMemory sidecar      │
│ browser (15min)  │ ───▶ │ (SQLite, dedup,  │ ───▶ │ localhost:6767           │
│ files (watcher)  │      │  retry/backoff)  │      │ • local embedding pool   │
│ meetings (Groq   │      │ 4-way concurrent │      │   POOL_SIZE=5            │
│  Whisper)        │      │ bulk + live lane │      │ • local LLM extraction   │
└─────────────────┘      └──────────────────┘      │ • ~600–900 MB RAM        │
                                                    │ • full CPU (renice gone) │
        ask: tokio::join!(v3 chunk search, v4 facts) → Groq gpt-oss-120b stream
```

Key files:
- `src-tauri/src/supermemory.rs` — HTTP client: `add_document` (POST /v3/documents), `search` (POST /v3/search, limit 8, chunkThreshold 0.4), `search_memories` (POST /v4/search), `doc_status`, `delete_document`, `health`. Container tag `recally_main`.
- `src-tauri/src/ingest.rs` — queue worker: BULK_CONCURRENCY=4, polls doc status every 500 ms until `done` (backpressure), exponential retry 30→1920 s, logs to `memories_log` with `sm_doc_id`.
- `src-tauri/src/lib.rs` — engine-on-demand: spawns the sidecar with POOL_SIZE=5, health-waits 60 s, kills it after 5 min idle (frees the RAM).
- `src-tauri/src/commands.rs:130-220` — ask flow: ensure engine (60 s cold boot!) → parallel v3+v4 search → format `[n] Title` citations → Groq stream.

### 2.2 Why it's hot and slow

Every embedded chunk and every fact extraction runs **on the user's CPU** inside the sidecar. The two uncommitted changes (pool 2→5, renice removed) made backfill 4× faster *by design* at the cost of full CPU saturation — that's the heat. Plus:

- ~600–900 MB resident memory while the engine runs
- 60-second cold boot before the user's first question can even start
- Engine-on-demand supervisor complexity (spawn/health/idle-kill) exists *only* to manage this load

**Moving embedding + extraction to hosted APIs removes 100% of on-device inference.** The app becomes a thin Rust pipeline: capture → HTTP calls → SQLite bookkeeping. No sidecar, no pool, no cold boot, no idle-kill. CPU cost per ingest drops to JSON serialization.

---

## Part 3 — The Recally Memory Engine (proposed)

### 3.1 Component mapping

| SuperMemory piece | Our replacement | Notes |
|---|---|---|
| Vector store + namespaces | **Qdrant** (already set up) | One collection, payload-filtered; dense + sparse vectors for hybrid |
| Embeddings | **Jina v3** primary (`jina-embeddings-v3`, 1024-dim, task adapters + late chunking) | Cohere embed-v4 / Voyage 3.5 are drop-in alternates behind a trait |
| Reranking | **Jina reranker** or Cohere Rerank API | Optional flag, like SuperMemory's `rerank: true` |
| OCR / image+PDF extraction | **Mistral OCR API** | Replaces local textutil/pdftotext for scanned content; keep textutil for native-text files (free, instant, local) |
| Fact extraction, update/extend classification, profiles, query rewrite | **Groq `gpt-oss-120b`** (already integrated in `groq.rs`) | Same model already used for answers; cheap + fast enough for ingest-time passes |
| Chunking | **Our Rust code** | Heading/paragraph-aware; no model needed |
| Audio | **Groq Whisper** (unchanged) | Already cloud-side today |
| Document store / graph / queue | **SQLite** (already there) | Source of truth; Qdrant is a rebuildable index |

### 3.2 Data model

**SQLite (source of truth — Qdrant can always be rebuilt from it):**

```sql
-- raw layer
documents(
  id TEXT PRIMARY KEY,            -- uuid, replaces sm_doc_id
  source TEXT, title TEXT, app TEXT, reference TEXT,
  content TEXT,                   -- full extracted text
  content_type TEXT,              -- text|url|pdf|image|code|transcript|...
  captured_at INTEGER,
  status TEXT                     -- queued|extracting|chunking|embedding|indexing|done|failed
)

chunks(
  id TEXT PRIMARY KEY, document_id TEXT REFERENCES documents,
  seq INTEGER, content TEXT, embedded INTEGER DEFAULT 0
)

-- understanding layer (the graph)
memories(
  id TEXT PRIMARY KEY,
  content TEXT,                   -- the distilled fact
  kind TEXT,                      -- fact|preference|episode|derived
  source_document_id TEXT,        -- provenance → citations
  is_latest INTEGER DEFAULT 1,
  confidence REAL,
  valid_from INTEGER, valid_until INTEGER,  -- NULL = no expiry; powers forgetting
  repetition_count INTEGER DEFAULT 1,       -- preferences strengthen
  created_at INTEGER
)

memory_edges(
  from_id TEXT, to_id TEXT,
  relation TEXT                   -- updates|extends|derives
)

profile(
  section TEXT PRIMARY KEY,       -- 'static' | 'dynamic'
  content TEXT, updated_at INTEGER
)
```

`ingest_queue`, `memories_log`, `cursors`, `meetings`, `settings` stay as they are (`memories_log.sm_doc_id` → `doc_id`).

**Qdrant — one collection `recally`, two named vectors + payload:**

```jsonc
// point payload
{
  "kind": "chunk" | "memory",       // both layers in one collection
  "container_tag": "recally_main",  // future multi-profile isolation
  "document_id": "...", "memory_id": "...",
  "source": "clipboard|browser|file|meeting",
  "app": "...", "title": "...", "captured_at": 1760000000,
  "is_latest": true                 // memories only; filter at query time
}
```

- Dense vector: Jina v3 1024-dim (`retrieval.passage` task for ingest, `retrieval.query` for queries).
- Sparse vector: BM25/SPLADE via Qdrant's built-in sparse support → **hybrid search with RRF fusion server-side in Qdrant**, replicating SuperMemory's hybrid mode with zero extra services.

### 3.3 Ingest pipeline (replaces the sidecar entirely)

The existing queue, dedup, retry, and concurrency machinery in `ingest.rs` survives unchanged — only the "send to localhost:6767 and poll" step is replaced by in-process stages:

```
ingest_queue item
  │
  1. EXTRACT      images/scanned PDFs → Mistral OCR
  │               native-text files → textutil/pdftotext (local, free, keep)
  │               urls → readability cleanup (already have content from browser capture)
  2. CHUNK        Rust: heading/paragraph-aware, ~512–800 tokens, 10–15% overlap;
  │               markdown by heading hierarchy; transcripts by speaker turns;
  │               clipboard snippets usually = 1 chunk
  3. EMBED        Jina API, batched (up to 128 chunks/call, retrieval.passage)
  4. UPSERT       Qdrant points (dense+sparse) + SQLite rows → status: done, SEARCHABLE NOW
  │
  5. UNDERSTAND   (async, non-blocking — search works before this finishes)
  │   a. Groq: "extract durable facts/preferences/episodes from this content,
  │      with expiry dates where temporal" → candidate memories (JSON mode)
  │   b. embed each fact → Qdrant top-k nearest existing memories (kind=memory)
  │   c. Groq: "given new fact + these neighbors: UPDATES / EXTENDS / NEW?
  │      noise → discard"
  │   d. write memories + memory_edges; UPDATES → flip old is_latest=false
  │      (SQLite + Qdrant payload update); repeated preference → bump count
  6. PROFILE      (debounced, e.g. after N new memories or nightly)
      Groq compiles static profile (durable facts) + dynamic (last ~7 days
      episodes) → profile table. Cached; costs nothing at query time.
```

Backpressure becomes simpler: instead of polling a sidecar, each stage is awaited directly; concurrency = `BULK_CONCURRENCY` tasks, each making rate-limited API calls. Stage 5 runs on a lower-priority lane so live captures index fast.

**Status visibility** maps 1:1 to the existing UI progress states (`queued → extracting → ... → done`).

### 3.4 Query pipeline (ask/chat)

```
question
  ├─ (optional) Groq query-rewrite for short queries (<6 words), llama-3.1-8b-instant, ~50ms
  ├─ embed query (Jina, retrieval.query task)
  ├─ Qdrant hybrid search ×2 in parallel (tokio::join!, same as today):
  │    • kind=chunk            → top 8, score threshold ≈0.4   (replaces /v3/search)
  │    • kind=memory, is_latest=true, (valid_until IS NULL OR > now) → top 10
  │                                                            (replaces /v4/search)
  ├─ (optional) rerank top-30 → top-8 via Jina/Cohere reranker
  ├─ assemble context:  [profile.static] + [profile.dynamic]
  │                     + "Known facts: …" + "[1] Title\nchunks…" (citation format unchanged)
  └─ Groq gpt-oss-120b stream  →  {type:sources} {type:token} {type:done}  (unchanged)
```

No `ensure_engine`, no 60-second cold boot — **first-question latency drops from up to 60 s to ~1–2 s** (embed ~100 ms + Qdrant ~50 ms + rerank ~100 ms + Groq TTFT).

The standing profile is the piece that delivers "ask anything and it just knows" — every answer starts from always-loaded context about the user, exactly like SuperMemory's Mastra/LangGraph injection pattern.

### 3.5 Forgetting & temporal correctness

- **Expiry:** extraction stamps `valid_until` on temporal facts ("meeting at 3pm today"); query filter excludes expired; a daily sweep marks them in Qdrant.
- **Contradiction:** the UPDATES flow flips `is_latest`; queries filter `is_latest=true`. History preserved in SQLite ("previously worked at Google" remains answerable).
- **Decay:** episodes get a recency boost at scoring time (`score × recency_weight(captured_at)`); old insignificant episodes fall out of top-k naturally — no deletion needed.
- **Noise:** the extraction prompt explicitly discards filler; clipboard dedup already prevents most junk upstream.

### 3.6 Code changes (surgical — the abstraction already exists)

`SupermemoryClient` is already a clean seam. Replace it with a `MemoryEngine` exposing the same five operations:

| File | Change |
|---|---|
| `supermemory.rs` → `engine/mod.rs` | New: `add_document`, `search`, `search_memories`, `doc_status`, `delete_document` — same signatures, backed by Qdrant/Jina/Mistral/Groq. Plus `engine/embed.rs` (trait `Embedder` w/ Jina, Cohere, Voyage impls), `engine/chunk.rs`, `engine/extract.rs` (Mistral OCR), `engine/understand.rs` (Groq graph pass), `engine/qdrant.rs` |
| `ingest.rs` | Swap sidecar call+poll for direct pipeline stages; keep queue/dedup/retry/concurrency |
| `commands.rs:130-220` | Drop `ensure_engine`; same parallel search via the new engine; prepend profile to system prompt |
| `lib.rs` | **Delete** the whole engine-on-demand block (spawn, health-wait, idle supervisor, POOL_SIZE) |
| `settings.rs` / `db.rs` | Add `qdrant_url`, `qdrant_api_key`, `jina_api_key`, `mistral_api_key`; new tables above |
| `tauri.conf.json` | Remove the supermemory `externalBin` sidecar → smaller bundle |

### 3.7 Migration plan (phased, each phase shippable)

1. **Phase 1 — RAG parity (the big win).** Engine module: extract → chunk → embed → Qdrant; hybrid search; swap into ingest + ask. Sidecar deleted. *Heat problem solved here.* Existing data: re-ingest from `memories_log`/`documents` content or accept fresh start (decide based on how much users have accumulated).
2. **Phase 2 — Understanding layer.** Groq fact extraction + updates/extends/derives graph; `search_memories` reads our `memories` instead of `/v4/search`.
3. **Phase 3 — Profiles + temporal.** Static/dynamic profile compilation, expiry/decay, recency weighting. This is where "ask anything, it knows" lands.
4. **Phase 4 — Polish.** Reranking flag, query rewriting, Mistral OCR for screenshots (a capture source the sidecar never handled well), evaluation harness (golden questions vs old engine).

### 3.8 Performance comparison

| | SuperMemory sidecar (today) | Recally engine (proposed) |
|---|---|---|
| On-device CPU | Full cores during ingest (heat) | ~0 (HTTP + SQLite only) |
| RAM | +600–900 MB while engine alive | ~0 incremental |
| First-question latency | Up to 60 s (cold boot) | ~1–2 s, always |
| Ingest throughput | Bounded by local CPU (4 concurrent) | Bounded by API rate limits — raise concurrency freely |
| Battery / fans | The complaint | Quiet |
| Works offline | Yes (only selling point lost) | Queue persists & drains on reconnect; **answers need network** (but Groq already did) |
| Bundle size | + sidecar binary | Smaller |

### 3.9 Costs (rough, per active user per month — verify current pricing before launch)

- **Jina embeddings:** heavy use ≈ 5–10 M tokens/mo ≈ **a few cents** (Voyage 3.5 ~$0.06/M, Cohere v4 ~$0.12/M as alternates)
- **Mistral OCR:** ~$1 per 1,000 pages → light use ≈ **cents**
- **Groq extraction passes:** the dominant cost; gpt-oss-120b ≈ $0.15/M in. ~2k tokens per document understood × ~3k docs/mo ≈ **~$1–3/mo**. Use `llama-3.1-8b-instant` for rewrite/noise-gate to cut this.
- **Qdrant:** already provisioned; one user ≈ tens of thousands of points ≈ negligible.

≈ **$1–5/user/month** at heavy usage — the trade for cold laptops and instant answers.

### 3.10 Risks & honest trade-offs

1. **Privacy (the big one).** Today embeddings/extraction happen on-device; only Groq sees ask-time context and meeting audio. The new design sends **all captured content** — clipboard, files, browsed pages — to Jina + Qdrant + Groq (+ Mistral for images). For an app whose pitch is "remembers everything you do," this needs: clear disclosure, per-source cloud opt-outs, a sensitive-content filter before upload (password-manager app exclusions already exist conceptually in capture), and vendors with no-retention API terms. Worth deciding deliberately, not incidentally.
2. **Offline:** captures queue fine offline (existing queue), but search/ask requires network. Acceptable — ask already required Groq.
3. **Rate limits:** burst backfills (initial file walk = 5k files) need client-side rate limiting + the existing exponential backoff. Jina/Groq batch endpoints help.
4. **Vendor coupling:** mitigated by the `Embedder` trait (Jina/Cohere/Voyage swappable) and SQLite-as-source-of-truth (Qdrant index rebuildable, embeddings re-generatable).
5. **Quality regression risk:** SuperMemory's extraction prompts are tuned. Build a 30–50 golden-question eval set against the current engine before cutover (Phase 4 harness, but write the questions in Phase 1).

---

## Part 4 — Why this engine is the right foundation for agents

The user's stated goal: this becomes the core that "runs everything else, from agents to performing tasks." The design above gives agents exactly what SuperMemory's integration docs show agents need:

1. **`profile()`** — static + dynamic context injected into every agent's system prompt (the Mastra `withSupermemory` pattern), so any future agent starts already knowing the user.
2. **`search()` / `search_memories()`** — tool-callable retrieval over everything ever captured, with citations.
3. **`add()`** — agents write their own episodic memories back (task outcomes, user feedback), which flow through the same UPDATES/EXTENDS graph and improve the profile.
4. **Container tags** — each future agent can get its own namespace (`agent_scheduler`, `agent_email`) sharing the user's pool but isolating its working memory.

Because it's all in-process Rust + your own Qdrant, agents get this at function-call latency with no sidecar lifecycle to manage.
