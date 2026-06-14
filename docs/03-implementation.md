# Recally Memory Engine — SOTA Implementation Report

**Date:** 2026-06-14
**Context:** Execution of the roadmap in `docs/memory-engine-gap-analysis.md` — closing the gaps against SuperMemory and proving each change with measurement.

This documents what was built, what the measurements showed, and the data-driven decisions made. The guiding discipline: **every retrieval change is gated by an A/B benchmark; features that don't measurably help don't ship on by default.**

---

## 1. The measurement instruments (built first)

Three harnesses in `src-tauri/src/bin/probe.rs`, all against the real index (55,734 chunks / 41,125 facts):

| Harness | What it measures | Command |
|---|---|---|
| **`bench`** | Frozen 60-query golden set on the production index. H@1/H@3/H@10, MRR, latency, tokens-injected, MemScore. The yardstick for query-side changes. | `probe bench` |
| **`abtest`** | A/B for ingest-side features: ingests a frozen corpus twice (feature OFF vs ON) into throwaway collections, replays the same queries, prints the delta. One variable at a time. | `probe abtest <contextual\|chunking\|hybrid>` |
| **`memtest`** | LongMemEval-style memory-capability suite: scripted scenarios (recall, cross-doc synthesis, knowledge update) scored pass/fail. | `probe memtest` |

Golden set + corpus are frozen to `eval/golden.json` and `eval/corpus.json` so numbers are comparable across commits.

**Baseline (production bench, before changes):** `H@1≈55-56% · H@3≈76-78% · H@10≈80% · MRR≈0.66 · ~1850ms · ~1820 tok`. Noise band ±3% / ±0.02 MRR (Groq planner sampling).

---

## 2. What shipped, and the evidence

### Tier 1 — The memory layer (the differentiator) ✅ SHIPPED ON

- **Memory lifecycle** (`engine/memory.rs`): every distilled fact is reconciled against existing memories — DUPLICATE / UPDATE / EXTEND / NEW — via one batched Groq classification per document. UPDATE flips the old memory's `is_latest=false`; lifecycle metadata lives in the Qdrant facts payload (`memory_id`, `is_latest`, `supersedes`, `extends`, `kind`), keeping the engine HTTP-only and headless-testable.
- **Temporal correctness** (`active_facts_filter`): the facts search excludes superseded (`is_latest=false`) and expired (`valid_until < now`) memories. Distillation appends an optional `[until YYYY-MM-DD]` suffix to time-bound facts, parsed off at index time. Legacy facts lack these fields and stay searchable (treated as latest/never-expiring) — zero migration needed.
- **Evidence:** `memtest` **3/3 pass**, including the contradiction scenario: after "switched Adidas→Puma", a query for the user's shoe brand returns **Puma**, two memories are flagged superseded, and legitimately-historical Adidas facts ("owns Ultraboost") are correctly *kept*. This is "memory, not RAG" — the thing plain RAG can't do — working end to end.

### Tier 2.5 — Standing user profile ✅ SHIPPED ON

- `engine/profile.rs`: compiles a **static** (durable facts) + **dynamic** (last 7 days) profile from the memory graph, cached 1h, injected into every `ask` system prompt ("ask anything, it just knows"). Exposed via the `user_profile` Tauri command.

### Tier 2.6 — Content-type-aware chunking ✅ SHIPPED ON

- `engine/chunker.rs`: markdown by heading hierarchy (each chunk carries its heading trail), transcripts by speaker turn, paragraph fallback. **A/B: retrieval-neutral** (−2 H@1, within noise) on the reconstructed corpus — but it has **no API cost** and improves citation quality and transcript structure, so it stays on.

### Tier 3.8 — Image OCR + web bodies ✅ (image ON, web opt-in)

- **Image OCR** (`mistral::ocr_image`): screenshots/scans are OCR'd. The capturer ingests image files **only when screenshot-like** (`is_screenshotty`) so we don't OCR every icon and wallpaper.
- **Web bodies** (`extract::jina_url`): fetches full page text via Jina Reader. **Off by default** (`fetch_web_bodies`) — fetching every visited URL is a deliberate privacy choice.

### Tier 3.7 — Multi-query retrieval ✅ SHIPPED ON

- When the planner rewrites a question, search with **both** the rewrite and the original wording, union the candidate pool before reranking. Both query vectors are embedded in one batch and all searches run concurrently, so the recall boost costs only ~+250ms.
- **Evidence (production bench):** the consistent effect is **fewer hard misses** — H@10 80→**83** across runs — which is exactly its purpose (recover docs the planner's rewrite drops). H@3/MRR move within the noise band, never negative. This is the one retrieval-side lever that helped, because the production task (55k chunks, 18% baseline MISS) actually has headroom, unlike the near-ceiling A/B corpus.

---

## 3. What did NOT ship on — and why (the discipline)

The A/B gate is only meaningful if it can reject. Three features measured neutral-or-negative on a realistic doc-level task and were **defaulted off**:

| Feature | A/B result (150-doc corpus) | Decision |
|---|---|---|
| **Contextual Retrieval** (Tier 1.1) | H@1 −6, MRR −0.029 | **OFF.** A per-doc LLM cost for negative doc-level benefit. The doc-level blurb makes a doc's chunks more similar to each other and to neighbouring topics. Anthropic's gains were *per-chunk* context measured at *chunk* level; our approximation on a doc metric doesn't capture it. Kept behind the flag to revisit with a chunk-level metric. |
| **Hybrid dense+sparse** (Tier 2.4) | H@1 +1, MRR +0.009 | **OFF / available.** Marginal on natural-language queries (its strength is exact-term/identifier lookups). Requires a hybrid-schema re-index. Fully implemented (`sparse.rs`, Qdrant RRF) — flip on + re-index if lexical queries become important. |

**Why so little retrieval movement?** The gap analysis already found our retrieval was the *mature* layer (strong planner, cross-encoder rerank + title boost, follow-up blending). On a near-ceiling doc-level task (baseline 92% H@1 on the A/B corpus), marginal retrieval tweaks can't help. The real wins were always the **memory layer** — which is exactly what we built and proved.

---

## 4. Final defaults

```
contextual:       false   (measured negative; behind flag)
distill:          true
memory_graph:     true    ← the differentiator
smart_chunking:   true    (neutral retrieval, +citation/transcript quality)
hybrid_search:    false   (available; needs re-index)
multi_query:      true    (query-side recall: H@10 80→83, fewer misses, +250ms)
fetch_web_bodies: false   (privacy opt-in)
image OCR:        on       (screenshot-gated)
```

---

## 5. How to reproduce

```bash
cd src-tauri
RECALLY_PIPELINE_TESTS=1 cargo test --lib                 # unit + live pipeline + contradiction
./target/release/probe memtest                            # memory-capability suite (LongMemEval-style)
./target/release/probe bench                              # production retrieval yardstick + MemScore
RECALLY_AB_LIMIT=150 ./target/release/probe abtest hybrid # A/B any ingest-side feature
./target/release/probe profile                            # print the standing user profile
```

(All require the `.env` keys: QDRANT_URL/_API_KEY, JINA_API_KEY, MISTRAL_API_KEY, GROQ_API_KEY.)
