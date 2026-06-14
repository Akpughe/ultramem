# Recally vs SuperMemory — Gap Analysis & Strategic Roadmap

**Date:** 2026-06-14
**Purpose:** Compare the as-built Recally memory engine against SuperMemory (docs + LongMemBench research + open-source repo), find the gaps that keep us at "good RAG" instead of "state-of-the-art memory," and lay out a sequenced plan to close them and prove it with benchmarks.

**Method:** Code-explorer trace of the actual implemented Rust (`src-tauri/src/engine/*`, `ingest.rs`, `agent.rs`, `commands.rs`), our own `docs/memory-engine.md` design doc, and an adversarially-verified web research pass (18 confirmed claims, 6 primary-source claims left unverified due to a session limit — flagged below as **[TO CONFIRM]**).

---

## 1. The one-sentence gap

> **We built SuperMemory's *Documents* layer (RAG) very well. We have barely started their *Memory* layer — and the Memory layer is the entire thesis.**

SuperMemory's own framing, verified verbatim from their repo:

> *"Memory is not RAG. RAG retrieves document chunks — stateless, same results for everyone. Memory extracts and tracks **facts about users** over time."*

Everything below is downstream of that sentence.

---

## 2. What SuperMemory actually is (verified)

### 2.1 Two layers, one pool
- **Documents (RAG):** raw input → semantic chunks → embeddings. Stateless, same for everyone.
- **Memories (understanding):** LLM-extracted facts/preferences/episodes, tracked over time, per-user, temporal.
- A single hybrid query (`searchMode: "hybrid"`, the default) returns **both** knowledge-base chunks **and** personalized user facts merged.

### 2.2 The graph: "facts built on facts" (not entity-relation triples)
A *living knowledge graph where memories connect to other memories*, built by an LLM pass at ingest with three relationship types:
- **UPDATES** — new fact contradicts old → old kept as history but flagged `isLatest = false`; search returns only the latest. (This is "I switched Adidas → Puma" returning Puma.)
- **EXTENDS** — new fact enriches old; both stay valid.
- **DERIVES** — system *infers* a second-order fact from patterns across memories.

### 2.3 Super RAG = managed, content-type-aware RAG
- **Content-type detection** (PDF, code, markdown, image, video) → **type-specific extraction** (OCR / transcription / web-scrape) → **content-specific chunking** → embeddings → **relationship mapping to existing knowledge.**
- **Chunking is per-type:** documents by semantic section, **markdown by heading hierarchy**, **code by AST boundaries** (their open-source `code-chunk` lib — functions stay intact, classes split by method), web by article structure.
- **Retrieval:** hybrid (dense chunk similarity + memory facts), optional **cross-encoder rerank (~+100 ms)**, optional **query rewriting** ("how to auth" → "authentication login oauth jwt").

### 2.4 The SOTA result — and the surprise
- Open-source engine: **81.6% on LongMemEval** (#1 publicly benchmarked at the time).
- Experimental engine: **~98.6% on LongMemEval** (8-variant ensemble).
- **The single biggest unlock, in their words:** *"ditching vector embeddings for active search agents… Agents actively searching for context eliminated the semantic similarity trap."* Their frontier system uses **3 parallel search agents that read and reason over stored findings**, not vector similarity.

### 2.5 [TO CONFIRM] — LongMemBench paper specifics (session-limited, primary-source, plausible)
- LongMemEval-S: **~95% overall**, vs Zep 71.2%, Full-Context 60.2%.
- Achieves this injecting only **~720 mean tokens** of context (≈99.4% reduction vs full-context) — *efficiency, not just accuracy, is the headline.*
- Ingestion uses a **modified version of Anthropic's Contextual Retrieval** to resolve ambiguous references inside each chunk before embedding.
- They publish **`memorybench`** — a harness scoring **MemScore = f(Quality, Latency, Tokens)** comparing Supermemory vs Mem0 vs Zep.

> These four are worth re-verifying when your session resets, but they're consistent with everything verified and they shape the roadmap (esp. Contextual Retrieval and the token-efficiency metric).

---

## 3. Gap table (what we have vs what's missing)

Legend: ✅ shipped · 🟡 partial · ❌ missing

| Capability | SuperMemory | Recally today | Gap |
|---|---|---|---|
| **Raw chunk RAG** | ✅ | ✅ Jina v3 + paragraph chunking | None — solid |
| **Cross-encoder rerank** | ✅ ~100ms | ✅ Jina reranker + lexical title boost | None — we even add a title-match boost |
| **Query rewrite / planner** | ✅ basic expansion | ✅ **better than theirs** — date/source/list-intent planning | Ahead |
| **Follow-up context** | implied | ✅ multi-turn embed blend + reference resolution | Ahead |
| **PDF OCR** | ✅ | ✅ Mistral OCR | None |
| **Image/screenshot OCR** | ✅ | ❌ PDF only | Missing |
| **Web body extraction** | ✅ readability | ❌ URL metadata only | Missing |
| **Content-type-aware chunking** | ✅ md/code/web/transcript | ❌ paragraph-only | **Missing** |
| **Contextual Retrieval (chunk context prefix)** | ✅ [TO CONFIRM] | ❌ title prefix only | **Missing — high leverage** |
| **Hybrid dense+sparse (BM25/SPLADE) search** | ✅ server-side | ❌ dense-only; "hybrid" = post-hoc title boost | **Missing** |
| **Distilled fact layer** | ✅ | 🟡 `distill.rs` extracts facts to a 2nd collection | Have extraction, missing lifecycle |
| **Memory graph: UPDATES/EXTENDS/DERIVES** | ✅ | ❌ no `memory_edges`, no relationship pass | **Missing — the core gap** |
| **Temporal correctness (`is_latest`)** | ✅ | ❌ superseded facts stay fully searchable | **Missing — the core gap** |
| **Forgetting / expiry / decay** | ✅ `valid_until`, recency | ❌ facts persist forever at full weight | **Missing** |
| **Memory kinds (fact/preference/episode)** | ✅ different lifecycles | ❌ flat facts | Missing |
| **Standing user profile (static/dynamic)** | ✅ injected every call | ❌ no `profile` table | **Missing — agent unlock** |
| **Container tags / namespaces** | ✅ | ❌ single pool | Missing (fine for v1) |
| **Agentic retrieval (read+reason agents)** | ✅ frontier 99% | 🟡 we have an agent loop + grounding verifier already | **Closer than we think** |
| **Benchmark harness** | ✅ LongMemEval + memorybench | 🟡 `probe` H@1/3/10 audit, no golden set, no public bench | **Missing — can't prove SOTA** |

---

## 4. Strategic reading of the gaps

Three things matter more than the long list:

1. **The Memory layer is the moat, and it's mostly an ingest-time LLM pass we haven't written.** We already do fact *extraction* (`distill.rs`). What's missing is the *relationship + versioning* step: embed each new fact → find nearest existing memories → ask the LLM "UPDATES / EXTENDS / NEW / noise?" → write edges, flip `is_latest`. That loop + `is_latest` filtering at query time *is* "memory, not RAG." It's the highest-value, most-defensible work and it's buildable with the Groq + Qdrant we already have.

2. **Contextual Retrieval is the cheapest big win.** Before embedding a chunk, prepend a 1–2 sentence LLM-generated blurb situating it in its document ("This chunk is from Newton's Q3 review, discussing the payments migration…"). Anthropic's published numbers: 35–49% reduction in retrieval failures. We already have the `embed_input()` seam — this is a localized change, not an architecture change. SuperMemory reportedly uses a modified version of exactly this.

3. **We're nearer the *frontier* (agentic) design than the *production* (graph) design — by accident.** Their 99% system's "biggest unlock" was replacing vector search with parallel read-and-reason agents. We *already have* `agent.rs` with `recall_search` + `recall_timeline` tools and a grounding verifier. That's the skeleton of agentic retrieval. The graph layer and the agentic layer are two different bets toward SOTA; we should pick deliberately (see §6).

---

## 5. Roadmap — sequenced by leverage ÷ effort

### Tier 1 — Become "memory," not just RAG (the differentiator)
1. **Memory lifecycle layer.** Add `memories`, `memory_edges`, (and keep facts in Qdrant) per the design doc schema. New ingest stage after distillation: nearest-neighbour memory lookup → Groq UPDATES/EXTENDS/NEW/noise classification → write edges → flip `is_latest`. *This is the single most important piece.*
2. **Temporal correctness at query time.** Filter `is_latest = true AND (valid_until IS NULL OR > now)` on the facts/memories search. Stamp `valid_until` on temporal facts during extraction. Add a recency weight to episode scores.
3. **Contextual Retrieval.** LLM-prefix each chunk with a situating blurb before embedding. Cheap, proven, localized.

### Tier 2 — Retrieval quality to match Super RAG
4. **True hybrid search.** Add Qdrant sparse vectors (BM25/SPLADE) + RRF fusion server-side. Replaces the post-hoc lexical title boost with real lexical recall for rare-term/exact-match queries.
5. **Standing user profile.** Compile `profile(static, dynamic)` from the memory graph (debounced/nightly); prepend to every `ask` system prompt. This is the "ask anything, it just knows" unlock and the foundation for agents.
6. **Content-type-aware chunking.** Markdown by heading hierarchy; transcripts by speaker turn; (later) code by AST. We have the content-type signal at capture; route to the right chunker.

### Tier 3 — Push toward the frontier
7. **Lean into agentic retrieval.** We already have the agent + verifier. Evaluate multi-agent parallel retrieval (read-and-reason over candidates) vs pure vector top-K on our benchmark — this is SuperMemory's stated #1 unlock. Gate it behind the token-budget tradeoff (more tokens/query for higher accuracy).
8. **Image OCR + web body extraction.** Close the ingest coverage gaps (screenshots, real page bodies).

### Tier 4 — Prove it (this is non-negotiable for "state of the art")
9. **Adopt LongMemEval.** Run the *same public benchmark* SuperMemory reports (LongMemEval-S), so our number is apples-to-apples against their 81.6% and the field (Zep, Mem0, Letta).
10. **Build a memorybench-style harness.** Score **Quality + Latency + Tokens-injected** (their efficiency framing — ~720 tokens matters as much as accuracy). Extend the existing `probe` audit into a tracked, golden-set regression suite that runs per-commit.

---

## 6. The one decision to make first

Two distinct paths to SOTA, and they fork the next month of work:

- **Path A — Graph memory (their production 81.6% design):** build the UPDATES/EXTENDS/DERIVES lifecycle + profile. More engineering, lower per-query cost, defensible, matches the "memory vs RAG" pitch users understand.
- **Path B — Agentic retrieval (their frontier 98.6% design):** lean on the agent we already have, parallel read-and-reason search. Less new infrastructure, higher per-query token cost, higher ceiling.

**Recommendation:** Do **Tier 1 + the benchmark harness first regardless** (they're prerequisites for both and for proving anything), then let the benchmark decide A vs B with data instead of vibes — which is exactly the discipline that got SuperMemory to SOTA.

---

## 7. Source notes
- Verified (3-0 / 2-1 adversarial vote): SuperMemory docs (how-it-works, graph-memory, super-rag, ingesting), the open-source repo README, the "99% SOTA" engineering blog.
- **[TO CONFIRM]** (primary source, unverified — session limit): LongMemBench paper figures (95% / 720 tokens / Contextual Retrieval / memorybench MemScore). Re-run verification after session reset before quoting these externally.
