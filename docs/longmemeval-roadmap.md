# How UltraMem Gets to Near‑Perfect on LongMemEval‑S

> Engineering analysis + implementation plan. **Status (2026‑06‑20):** trustworthy **120‑question** baseline **63.3%**, lifted to **66.7%** by a type‑aware answer‑prompt pass. **Retrieval is essentially solved (97.5% gold retrieved); the bottleneck is now answer *synthesis* (42 of 44 failures).** **Tier‑3 — the bi‑temporal knowledge graph — is now built, unit‑tested, and smoke‑validated** (it fixes the exact knowledge‑update cases prompting could not); a full 120‑question measurement is running. This document explains *why*, what the SOTA does, the loopholes, and the tiered plan.

---

## 1. Goal and honest baseline

**Goal:** push per‑category accuracy on LongMemEval‑S from ~60% toward the leaders' band (and, with the Tier‑3 bet, ~90%).

**Where we are (measured, Groq `gpt-oss-120b`, 5/category):**

| Run | Overall | Notes |
|---|---|---|
| baseline | 50% | first harness |
| + retrieval/prompts/dates | 56.7% | hybrid, planner, type‑aware |
| **focused (best)** | **63.3%** | full sessions for reasoning only, snippets for recall, top_k=25 |
| + "completeness" (full sessions all types, top_k=40) | 46.7% | **regressed** — context overload; reverted |
| Gemini 2.5 Flash (focused) | 60% | ≈ Groq |

**Honest ceiling context:** **Zep/Graphiti reaches 90.2%** on LongMemEval via a temporal knowledge graph ([Zep, arXiv 2501.13956](https://arxiv.org/abs/2501.13956)). A well‑built *vector* system following the LongMemEval paper's recipe lands in roughly the **75–85%** band ([LongMemEval, arXiv 2410.10813, ICLR'25](https://arxiv.org/abs/2410.10813)). Literal 100% in every category is above current SOTA, especially temporal/multi‑session.

---

## 2. The single most important finding: it's the architecture, not the model

We proved this empirically: **Gemini 2.5 Flash (60%) ≈ Groq gpt‑oss‑120b (63%)** — swapping in a frontier model moved the needle ~0, and *adding more context made it worse* (the 47% regression). Therefore the ceiling is in **how UltraMem indexes, time‑stamps, retrieves, and reads memory** — not the LLM. Every fix below is architectural.

A second, quieter finding: across **every** run, `retrieval-miss = 0` on our metric — but that metric is **broken** (see Loophole #6), so "retrieval is solved" is overstated. Real multi‑evidence recall is below 100%.

---

## 3. What the SOTA / optimal designs actually do (researched, cited)

**The LongMemEval paper's measured recipe** ([html](https://arxiv.org/html/2410.10813)):

| Design | What it is | Measured gain |
|---|---|---|
| **Round‑level granularity** | index per user+assistant *round*, not whole sessions | more optimal than sessions; helps multi‑session |
| **Fact‑augmented keys** | embed extracted facts / keyphrases / **timestamped events** *together with* the raw value as the retrieval key | **+9.4% recall@k, +5.4% QA** |
| **Time‑aware indexing + query expansion** | attach event timestamps; an LLM extracts a **time range** from the query to filter | **+11.3% temporal recall** |
| **Chain‑of‑Note (CoN) reading + JSON** | reader first *copies* relevant notes, then *reasons* over them | **+10 absolute QA points** |
| **Multi‑pathway retrieval** | combine original values **and** extracted facts (don't replace one with the other) | "significantly outperforms" single‑pathway |

**Zep/Graphiti (90.2%)** adds the layer above: a **temporal knowledge graph** with **bi‑temporal fact‑validity intervals**, entity/relationship nodes, and retrieval that *fuses* time + full‑text + semantic + graph traversal. Supersession is modeled as edges gaining a `valid_to`, not deleted ([Zep blog](https://blog.getzep.com/state-of-the-art-agent-memory/)).

---

## 4. Current UltraMem architecture (grounded map)

**Ingest** (`engine/mod.rs::add_document`): `IngestDoc` → extract → `chunker::chunk_doc` → embed each chunk (Jina/OpenAI; embed input is **title‑prefixed chunk text**) → upsert to `ultramem_chunks` → **then** `distill::distill_facts` → `memory::reconcile` (UPDATE/EXTEND/DUPLICATE/NEW, flips `is_latest`) → upsert to `ultramem_facts`.

**Retrieve** (`retrieve_for_plan_tagged`): `rewrite::plan` (LLM: query rewrite + source + after/before + `list`) → embed query → `search_chunks` (dense, or hybrid dense+sparse RRF) **+** facts search in parallel → group chunk hits by doc → cross‑encoder rerank → return `SearchResult[]` (doc + matched chunks) and `memories[]` (latest facts).

**Config flags** (`EngineCfg`): `contextual` (**OFF**), `smart_chunking` (on), `hybrid_search` (off in prod, **on** in the benchmark), `multi_query` (on), `distill`/`memory_graph` (on).

**Key code facts confirmed:**
- Chat sessions use `source="chat"`, which is **not** routed to the transcript/turn chunker → they're **paragraph‑chunked** (loses round structure).
- **Contextual / fact‑augmented keys already exist** (`engine/context.rs`) but are **disabled by default** (`cfg.contextual=false`) — A/B on Recally's low‑density docs showed no gain; LongMemEval is exactly the high‑density case where the paper measured +9.4%.
- Facts are time‑stamped by **`captured_at` = session date**, not the event date in the text.

---

## 5. The loopholes (why we're stuck at ~60%) — grounded in our transcripts

| # | Loophole | Evidence | Recipe item it maps to |
|---|---|---|---|
| 1 | Whole‑session docs, paragraph chunks (not round‑level) | "Where did I redeem the coupon → *not in context*" (detail in a non‑matched chunk) | round‑level granularity |
| 2 | Retrieval keys are raw chunk text; facts stored in a separate collection, not fused into the key | recall‑precision plateau | fact‑augmented keys (+9.4%) |
| 3 | `captured_at` = session date, not event date | "keyboard vs bluegrass → 0 days" (discussed same day, happened days apart) | time‑aware indexing |
| 4 | No time‑range query extraction/filter | temporal stuck ~60% | time‑aware query expansion (+11.3%) |
| 5 | Single‑pass reader (CoN added only for count/temporal) | recall/preference answers ramble or abstain | Chain‑of‑Note (+10 pts) |
| 6 | `gold_retrieved` checks **any** evidence session, not **all** | MoMA's 2nd event never retrieved, yet counted "retrieved" | measurement / multi‑evidence recall |
| 7 | Reconciliation is a binary `is_latest` flip, not validity intervals | "Rachel → Chicago" (stale value chosen) | Zep bi‑temporal model |
| 8 | No entity/graph layer (flat vector search) | multi‑hop ("days between A and B") can't fetch both | Zep KG |
| 9 | Judge false‑negatives (even Gemini) | Miami/cultural answers correct, judged "no" | measurement ceiling |
| 10 | 5 questions/category | ±20% swing on one flip | measurement noise |

**What we already do right (don't rebuild):** two‑layer chunks+facts, `is_latest` supersession, hybrid dense+sparse, standing profile, namespace isolation, provider retry, and **extract‑then‑compute** (which *is* the paper's CoN idea for two categories — validated direction).

---

## 6. The plan — Tier 1 / 2 / 3

Each item: **rationale (with measured gain) · code changes · effort · risk · expected lift · validation.**

### Tier 1 — Adopt the LongMemEval recipe (incremental, paper‑backed; target ~75–85%)

#### T1.1 — Round‑level indexing for conversational content *(keystone)*
- **Why:** the paper's round granularity; fixes Loopholes #1/#6 (detail locality + multi‑evidence recall). The "I don't have that" misses are caused by paragraph chunks splitting a session so the answer detail isn't in the matched chunk.
- **Code:** `engine/chunker.rs` — route conversational sources (`chat`, `meeting`, or content with `role:`‑prefixed lines) to a **round chunker** that emits one chunk per user+assistant round (fallback to turn, then paragraph). Add `ChunkGranularity` or reuse `chunk_transcript` by making `is_speaker_line` recognize `user:`/`assistant:`. Tag each chunk with a `round_index`. No schema change needed (chunks already carry `chunk_index`).
- **Effort:** M · **Risk:** low‑med (more, smaller chunks → more points, more embed calls at ingest; re‑ingest required). · **Lift:** recall + multi‑session; unblocks T1.2.
- **Validate:** unit tests on the round chunker; re‑ingest the 60‑Q set; confirm coupon/Orlando‑class details now land in matched chunks.

#### T1.2 — Fact‑augmented keys (contextual retrieval, on + enhanced)
- **Why:** +9.4% recall@k, +5.4% QA — the paper's biggest retrieval lever. We **already have the mechanism** (`context.rs`), just disabled.
- **Code:** (a) enable `cfg.contextual` for conversational ingest; (b) enhance: the embed *key* for each chunk = `title + one‑line situating blurb + distilled keyphrases/entities + event date` ++ chunk text, while the **stored `content` stays the raw text** (so display/answer is clean). Requires a **pipeline reorder** in `add_document`: produce the doc‑level fact/keyphrase summary *before* embedding chunks (today distillation runs *after* chunk upsert), or run a cheap keyphrase pass first. Keep the heavy fact distillation where it is; add a light "key augmentation" step.
- **Effort:** M‑L (reorder + a light extraction) · **Risk:** med (ingest cost; don't pollute stored content). · **Lift:** retrieval precision across all categories.
- **Validate:** A/B with the existing `abtest` harness (`contextual` off vs on) on the frozen corpus; expect recall@k up.

#### T1.3 — Chain‑of‑Note reader for all categories
- **Why:** +10 absolute QA points (paper). We have CoN for count/temporal (extract‑then‑compute); generalize.
- **Code:** harness answer step (and document as the recommended consumer/MCP pattern): two‑step read — (1) "From the context, copy verbatim the facts relevant to the question as a short JSON note list"; (2) "Answer using only those notes." For recall/knowledge‑update/preference. Keep extract‑then‑compute for count/temporal.
- **Effort:** S · **Risk:** low (more LLM calls). · **Lift:** recall + knowledge‑update + preference.
- **Validate:** `MODE=eval` re‑run (fast) on the ingested 60‑Q; compare per‑category.

#### T1.4 — Time‑aware query expansion
- **Why:** +7–11% temporal recall (paper). The planner already extracts `after`/`before`; the engine already filters on them (`build_filter`).
- **Code:** strengthen `rewrite.rs` planner prompt to reliably emit a **time range** for time‑sensitive questions; ensure `build_filter` applies it. **Gate this on T2.1** (correct event dates) — filtering by wrong dates would exclude evidence.
- **Effort:** S‑M · **Risk:** med (over‑filtering if dates are wrong → do after T2.1). · **Lift:** temporal.
- **Validate:** temporal‑only `MODE=eval` slice.

### Tier 2 — Temporal correctness + honest measurement

#### T2.1 — Event‑time extraction (date by the event, not the session)
- **Why:** fixes Loophole #3 (the "0 days" class). Temporal questions reason over when events *happened*, stated in the text ("last Sunday", explicit dates), not when they were discussed.
- **Code:** `engine/distill.rs` — extract `(fact, event_date)` pairs; resolve relative dates ("last Sunday") against the **session date as anchor**; store `event_date` in the fact payload (new field + payload index). Retrieval/temporal logic uses `event_date` when present, else `captured_at`.
- **Effort:** M · **Risk:** med (distill prompt change; relative‑date resolution). · **Lift:** temporal (and feeds T1.4).
- **Validate:** unit tests for relative‑date resolution; temporal slice.

#### T2.2 — Fix the recall metric + bigger N
- **Why:** Loopholes #6/#10. "retrieval‑miss 0" is false; 5/category is noise.
- **Code:** harness — require **all** `answer_session_ids` retrieved for `gold_retrieved=true`; report per‑evidence recall. Default the canonical run to a **bigger N** (e.g. 20/category) for a real number.
- **Effort:** S · **Risk:** none. · **Lift:** measurement honesty (likely reveals true retrieval gaps to fix).

#### T2.3 — Faithful judge
- **Why:** several "failures" are correct answers (Loophole #9).
- **Code:** **done** — self‑consistency (majority of 3). Optional: add a GPT‑4o judge path (needs `OPENAI_API_KEY`) for leaderboard‑faithful scoring.
- **Effort:** done / S · **Risk:** none.

### Tier 3 — The SOTA leap: temporal knowledge graph (target ~88–92%)

#### T3.1 — Entity + relationship extraction
- Extend distillation to emit **entities** (people, places, orgs, items) and **relationships** (`(subject, predicate, object)`), in addition to flat facts. New `engine/graph.rs`.

#### T3.2 — Bi‑temporal edge store
- Store edges with `valid_from` / `valid_to` (event time) **and** ingest time. Supersession = set `valid_to` on the old edge, not delete (Zep's non‑lossy model). Could live as a new Qdrant collection of edge points (subject/predicate/object + temporal payload) or a dedicated graph store behind a `GraphStore` trait (mirrors our `VectorStore` trait).

#### T3.3 — Graph + vector fused retrieval
- Retrieve by fusing: semantic (existing) + entity‑node lookup + 1–2 hop graph traversal + time filter. Answer over the resolved sub‑graph.

- **Effort:** XL (weeks; a real subsystem) · **Risk:** high · **Lift:** the path to ~90%. **This is a deliberate architectural bet — decide separately after Tier 1–2 plateau.**

> **STATUS (2026‑06‑20): BUILT + smoke‑validated.** Implemented in `engine/graph.rs` as flat first‑class edge records (not nested payloads — Qdrant nested‑array temporal filtering is unreliable) on a dense `graph_collection`, with deterministic Rust supersession and an answer‑time `resolve_edges_tagged`. The `singular` state/event flag handles both "current value" (supersede) and "most recent" (max `valid_from`). It already fixes the knowledge‑update case prompting could not (the 5K personal best). 120‑Q measurement in progress — see the run‑history appendix.

---

## 7. Sequencing & milestones

```
M1 (Tier 1 core):  T1.1 round-level  →  T1.2 fact-augmented keys  →  T1.3 CoN reader
                   re-ingest 60-Q, MODE=eval after each.   Target: 63% → ~75%.
M2 (Tier 2):       T2.1 event-time  →  T1.4 time-aware queries  →  T2.2 metric+N  (+ T2.3 done)
                   Target: temporal/knowledge-update materially up; honest measurement.  ~80%.
M3 (Tier 3 bet):   T3.1 entities → T3.2 bi-temporal edges → T3.3 fused retrieval.
                   Target: ~88–92%.  Large build; gate on M1–M2 results.
```

**Dependency notes:** T1.2 builds on T1.1 (round units make fact‑augmented keys sharper). T1.4 must follow T2.1 (don't time‑filter on wrong dates). Everything before T3 reuses the existing two‑collection vector store; T3 adds a graph store behind a new trait.

---

## 8. Expected trajectory & honest ceiling

- **Tier 1:** ~63% → **mid‑70s to mid‑80s** (paper‑measured gains, minus our noise). These are also the right *product* investments — round‑level + fact‑augmented keys + CoN make the engine genuinely better for any consumer (Re‑Kalei), not just the benchmark.
- **Tier 2:** removes the temporal/measurement distortions; the number you see becomes trustworthy.
- **Tier 3:** the temporal KG is the proven route to **~90%** — but it's a multi‑week subsystem, not a tweak.
- **Caveat:** a few current "failures" are judge artifacts, so our *effective* accuracy already exceeds the printed score.

---

## 9. Measurement protocol (so numbers mean something)

1. **Ingest once, eval many** (already supported: `ULTRAMEM_LME_MODE=ingest|eval|both`) — iterate answer/retrieval logic in minutes, not hours.
2. **Bigger N** (≥20/category) for any claim; 5/category is for smoke only.
3. **Fixed recall metric** (all evidence sessions).
4. **Self‑consistent (or GPT‑4o) judge.**
5. **Measured tokens/cost** per run (already instrumented via `llm::token_usage()`).
6. Change **one tier item at a time**, `MODE=eval` re‑run, compare per‑category — never bundle (we learned this when "completeness" silently regressed).

---

## Appendix — run history (30‑Q, 5/category)

| Config | Overall | user | asst | pref | k‑update | temporal | multi |
|---|---|---|---|---|---|---|---|
| Groq baseline | 50.0 | 80 | 60 | 40 | 60 | 0 | 60 |
| Groq +hybrid/prompts | 56.7 | 100 | 40 | 60 | 100 | 40 | 0 |
| **Groq focused (best)** | **63.3** | 80 | 60 | 80 | 60 | 60 | 40 |
| Groq +completeness (reverted) | 46.7 | 80 | 40 | 20 | 60 | 60 | 20 |
| Gemini 2.5 Flash (focused) | 60.0 | 80 | 80 | 40 | 60 | 60 | 40 |

### 60‑question run (10/category) — the honest baseline

Groq `gpt-oss-120b`, extract‑then‑compute + self‑consistent judge, focused context:

| Overall | user | asst | pref | k‑update | temporal | multi |
|---|---|---|---|---|---|---|
| **46.7** (28/60) | 60 | 60 | 50 | 50 | 40 | 20 |

**This is the real baseline.** The ~63% on 5/category was small‑sample optimism (the first 5 of each type happened to be easier). At **10/category** the honest number is **~47%** — measure all future work against this, not the 5‑Q figures. (retrieval‑miss 1 / synthesis‑judge 31 on the lenient metric; eval cost ~$0.10 thanks to ingest/eval split.)

**Extract‑then‑compute had two bugs (found by inspecting `lme60_results.json`, now fixed):**
1. Counting extracted the *category* ("model kits") not the instances → every count = 1. Fixed: the extraction prompt now demands specific instances.
2. Duration questions ("how many *days* camping" → sum, GOLD 8) were misrouted to item‑counting. Fixed: duration phrases ("how many days/weeks/months/hours/years", "how long") no longer route to counting.
Temporal extract‑then‑compute works when extraction is right (ordering, "4 weeks", "2 months" all correct) but a wrong extracted date → wildly wrong ("1258 days"); event‑time extraction (**T2.1**) is the durable fix.

**Conclusion:** eval‑side cleverness is finicky and has plateaued (~47%). The structural Tier‑1 work — **round‑level chunking (T1.1), fact‑augmented keys (T1.2)** — is the durable lever.

### Tier‑1 result (T1.1 + T1.2, 30‑Q, Groq) — structural fixes validated

| Overall | user | asst | pref | k‑update | temporal | multi |
|---|---|---|---|---|---|---|
| **60.0** (18/30) | **100** | **100** | 60 | 60 | 20 | 20 |

**Round‑level chunks + fact‑augmented keys SOLVED single‑session recall** (user & assistant both **100%** — the "detail in a non‑matched chunk" / "I don't have that" misses are gone). The deficit is now **isolated to the two reasoning categories**, exactly as predicted:
- **Temporal (20%):** the extract‑then‑compute can't obtain event dates — it abstains ("I don't have the 2nd event's date") or extracts a wrong one ("1171 days"). **→ T2.1 (event‑time extraction)** is the targeted fix; pairs with multi‑evidence completeness (both event sessions must be retrieved + dated).
- **Multi‑session (20%):** the counting category bug is fixed (no more count=1) but answers are now **off‑by‑one/two** (2 of 3, 4 of 5, 6 vs 2) — extraction *precision/completeness*, not arithmetic.

**Read:** Tier‑1 worked where it was aimed (recall). The overall held at 60% only because temporal regressed into the same hole multi‑session is in. **Next: T2.1 event‑time extraction** (the temporal lever), then attack multi‑session extraction precision. 5/category remains noisy (±20%/cat); a bigger N is owed before any firm claim.

### Tier‑1 + T2.1 (event‑time extraction) — 30‑Q, Groq

| Overall | user | asst | pref | k‑update | temporal | multi |
|---|---|---|---|---|---|---|
| **63.3** (19/30) | 100 | 100 | **80** | 60 | 20 | 20 |

**T2.1 did its job:** facts now carry `[on YYYY-MM-DD]` event dates, and the **wrong‑date errors are gone** — no more "1171 days." Preference also rose to 80%. But temporal stayed at 20% because the failure mode **shifted**: every temporal miss is now *"I don't have enough information to determine the date"* — the model **abstains** because the **second event's dated fact isn't retrieved**. Multi‑session (20%) is the same shape (not all countable items retrieved).

**New unifying bottleneck → multi‑hop / multi‑evidence retrieval completeness.** A question naming two events ("days between MoMA and the Ancient Civilizations exhibit") embeds as one query that surfaces *one* event, not both. The fix is **query decomposition**: split a multi‑entity/multi‑event question into sub‑queries, retrieve each, union the dated facts, then compute. This is the lever for *both* temporal and multi‑session, and it subsumes the old T1.4/T2.2 completeness items. (Cited support: the LongMemEval paper's time‑aware *query expansion*; Zep's entity‑centric graph traversal.)

### Query decomposition — BEST RESULT (30‑Q, Groq, full stack)

| Overall | user | asst | pref | k‑update | temporal | multi |
|---|---|---|---|---|---|---|
| **70.0** (21/30) | 100 | 100 | 40\* | 40\* | **80** | **60** |

Implemented eval‑side (no re‑ingest): for temporal/multi‑session, an LLM splits the question into per‑event sub‑queries, each retrieved planner‑free, results unioned into context. **Temporal 20→80%, multi‑session 20→60%** — MoMA "7 days", keyboard "6 days", model kits "5", camping "8 days", Marvel+Star Wars "3.5 weeks" all correct. (\*preference/k‑update dips are 5‑Q noise + judge variance — they don't use decomposition.) Full narrative + analysis: **`docs/longmemeval-study.md`**.

### 120‑question runs (20/category) — the trustworthy baseline (Gemini 2.5 Flash judge)

The 30‑Q numbers above are noisy (±20%/cat). A **120‑question** run (20/category, ingest/eval split, Groq `gpt-oss-120b` answerer, **Gemini 2.5 Flash judge**) is the honest low‑noise number.

| Config | Overall | user | asst | pref | k‑update | temporal | multi |
|---|---|---|---|---|---|---|---|
| **Baseline (full stack)** | **63.3** (76/120) | 85 | 70 | 30 | 60 | 75 | 60 |
| **+ type‑aware answer prompts** | **66.7** (80/120) | 80 | 65 | **45** | 60 | 80 | 70 |

**Headline finding: retrieval is essentially solved — 117/120 gold sessions retrieved (97.5%), only 2 retrieval‑misses. 42 of 44 failures are *synthesis* (evidence in context, answer still wrong).** The bottleneck has fully shifted from retrieval to answering.

The type‑aware answer‑prompt pass (prompt‑only, no re‑ingest) gave **+4 overall**, but the signal is uneven:
- **Preference +3 (30→45%) — real.** Telling the model to answer the *on‑topic* preference (not the loudest interest) recovered the diagnosed cases (publications wanted AI‑in‑healthcare, not sustainability; Miami hotel, cocktail).
- **Knowledge‑update +0 — prompting can't fix it.** It reworded 19/20 answers but flipped zero: still 300‑not‑120 (Starbucks), Hawaii‑not‑Paris, 27:12‑not‑25:50. The values are *in context* but, lacking per‑value dates, "use the latest" is an instruction the model cannot act on. **This is what Tier‑3 is for.**
- The ±1–2 wobble in untouched categories (user, assistant) is `gpt-oss-120b` run‑to‑run noise (±2–4 overall even at 120‑Q).

### Tier‑3 (temporal knowledge graph) — BUILT + smoke‑validated; 120‑Q measurement running

`engine/graph.rs` (new) extracts `(subject, predicate, object)` edges stamped with **event time** (`valid_from`/`valid_to`), with a `singular` flag separating single‑valued **states** (supersede) from accumulating **events**. Deterministic Rust supersession + `resolve()` surface the dated timeline with the current value marked; the answer model reads a "Temporal knowledge (resolved…)" block first. **10 unit tests + 76 lib tests green.**

**Smoke validation on real conversations:** the graph extracted `personal_best_5k_time: 27:12 (superseded) → 25:50 (LATEST)` and the answer model used it — *"latest is 25:50… earlier was 27:12 but it has been superseded."* **That exact knowledge‑update question was wrong in both prior runs.** Both smoke KU questions passed 2/2.

Engineering note: rather than a 7–9h full re‑ingest, a **graph‑only backfill** (`add_document_graph_only` + `ULTRAMEM_LME_GRAPH_ONLY`) builds *only* the edges over the existing `lme120` chunk/fact index — ~2.6× cheaper (~160k vs ~425k tokens/question) and a clean A/B (retrieval byte‑identical to the 66.7% baseline; the only changed variable is the graph). The 120‑question backfill into `ultramem_lme120_graph` is in progress; the eval and per‑category before/after will land here next.
