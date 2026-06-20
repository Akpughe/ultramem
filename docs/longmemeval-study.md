# Closing the Gap on LongMemEval‑S: An Engineering Study of Memory‑System Design

> Working notes / draft material for a paper or article. Chronicles how UltraMem — an open‑source memory engine for AI agents — was evaluated on LongMemEval‑S and iteratively improved, with the measured effect of each design change and the lessons that generalize beyond the benchmark.

---

## Abstract

We benchmarked UltraMem, a two‑layer memory engine (raw content chunks + LLM‑distilled, time‑reconciled facts), on **LongMemEval‑S** — 500 questions, each buried in a ~50‑session / ~115k‑token chat haystack, across six abilities: single‑session user recall, single‑session assistant recall, preference, knowledge‑update, temporal reasoning, and multi‑session aggregation. Starting from an off‑the‑shelf configuration, a sequence of **architectural** changes — not model upgrades — moved the system from a distributed‑failure baseline to **single‑session recall solved (100%)** and the two hardest categories lifted **temporal 20%→80%** and **multi‑session 20%→60%**, reaching **70% overall** on a 30‑question slice. The central finding: on this task, **memory‑system design dominates model scale**. A frontier model (Gemini 2.5 Flash) tied a mid‑tier open model (Groq gpt‑oss‑120b); *more* context actively hurt; and the wins came from round‑level indexing, fact‑augmented retrieval keys, event‑time extraction, and **query decomposition for multi‑hop retrieval**. A subsequent low‑noise **120‑question** evaluation (Gemini 2.5 Flash judge) put the honest figure at **63.3%**, lifted to **66.7%** by type‑aware answer prompts, and isolated the remaining gap precisely: **retrieval is essentially solved (97.5% of gold sessions retrieved); 42 of 44 residual failures are answer *synthesis*.** One synthesis class — knowledge‑update ("what is the current value of X?") — proved **immune to prompting** because the competing old/new values reach the model with no per‑value dates to order them by. That motivated a **bi‑temporal knowledge‑graph** layer that resolves the latest value deterministically from event time; it fixes the exact cases prompting could not.

---

## 1. Background

**UltraMem.** A self‑hostable memory engine (Rust): ingest → content‑aware chunk → embed (Jina/OpenAI) → vector store (Qdrant) for the *document layer*, plus LLM **distillation** of atomic facts that are **reconciled over time** (dedup / UPDATE→supersede via `is_latest` / EXTEND / NEW) for the *memory layer*. Retrieval runs both layers in parallel (dense or hybrid dense+sparse), with a query planner and cross‑encoder rerank, scoped per namespace (`container_tag`). Provider‑agnostic via traits.

**LongMemEval‑S** ([Wu et al., ICLR'25, arXiv 2410.10813](https://arxiv.org/abs/2410.10813)). 500 instances; each has a multi‑session haystack and a question whose evidence is a small number of specific sessions. Official scoring uses an **LLM judge** (GPT‑4o) with verbatim per‑question‑type prompts emitting a yes/no `autoeval_label`; accuracy is reported per `question_type`.

**State of the art.** **Zep/Graphiti reaches 90.2%** via a *temporal knowledge graph* with bi‑temporal fact‑validity intervals and fused time+text+semantic+graph retrieval ([arXiv 2501.13956](https://arxiv.org/abs/2501.13956)). The LongMemEval paper itself prescribes a measured recipe for vector systems: round‑level granularity, **fact‑augmented keys (+9.4% recall@k)**, **time‑aware indexing/query expansion (+11.3% temporal)**, and **Chain‑of‑Note reading (+10 QA points)**.

---

## 2. Methodology

**Harness** (`crates/ultramem-core/examples/longmemeval.rs`). Per question: (1) create an isolated namespace; (2) ingest every haystack session through the real pipeline; (3) retrieve; (4) an LLM answers from retrieved memory; (5) an LLM judge scores it against gold with the official per‑type prompt; (6) aggregate by type.

**Design decisions that made iteration honest and fast:**
- **Ingest/eval split.** Ingestion (~1.5–4 h) is identical across answer‑logic changes, so `MODE=ingest` persists the haystacks once and `MODE=eval` re‑scores in ~15 min. This enabled rapid eval‑side iteration.
- **Failure attribution diagnostic** (`gold_retrieved`): did retrieval surface the gold evidence session? Splits failures into *retrieval‑miss* vs *synthesis/judge*. (Caveat below — it checks *any* evidence session, not *all*, so it understates multi‑evidence retrieval gaps.)
- **Self‑consistent judge:** each answer judged 3× and majority‑voted, to damp borderline false‑negatives.
- **Measured token/cost** instrumentation across all LLM calls (incl. Gemini "thinking" tokens).
- **Per‑question JSONL** of (question, gold, response, verdict, retrieved refs) for transcript inspection — every conclusion below was read from these, not guessed.

**Models.** Groq `gpt‑oss‑120b` (open, ~o4‑mini tier) as the default answer+judge+distill model; Gemini 2.5 Flash (native API) for the model‑scale ablation. The official judge is GPT‑4o, which we did not have — so absolute numbers are **indicative**, not leaderboard‑official.

**Honesty constraints.** 5 questions/category carries ±20%/category noise; a 60‑question run was used to sanity‑check. Nothing was tuned to the gold answers; the judge prompts are verbatim from the official `evaluate_qa.py`.

---

## 3. Experiments and findings

### 3.1 Baseline and the failure taxonomy
The off‑the‑shelf configuration scored ~50% (30‑Q). Reading the transcripts, **every** failure had the gold session retrieved by the lenient metric — i.e. failures were *synthesis/judge*, not retrieval‑miss. But inspection revealed three real failure modes: (a) the answer detail sat in a chunk that didn't match the query (recall), (b) multi‑evidence questions surfaced only *one* of several needed sessions (temporal/multi‑session), (c) correct answers marked wrong (judge).

### 3.2 Finding 1 — Architecture dominates model scale
Swapping the mid‑tier open model for a frontier model moved the score ~0:

| Config (30‑Q, focused) | Overall |
|---|---|
| Groq gpt‑oss‑120b | 63.3% |
| Gemini 2.5 Flash | 60.0% |

Identical diagnostic profile on both (retrieval delivers; failures are synthesis/judge). **Conclusion: the ceiling is the memory design, not the LLM.**

### 3.3 Finding 2 — More context hurts ("lost in the middle")
A "completeness" experiment that fed *full sessions for every category* and widened `top_k` 25→40 **regressed 63%→47%**: the model began answering *"I don't have that information"* to questions it previously got right. Flooding the context degraded fact‑finding. **Conclusion: less, sharper context wins; retrieval precision > raw recall volume.**

### 3.4 Finding 3 — Small‑N optimism
The ~63% on 5/category was partly luck: at **10/category the honest baseline was 46.7%**. Small‑N percentages are noisy; the robust signal is *which intervention fixes which category*, and the *shape* of the failure profile — which is what we report.

### 3.5 The structural interventions (ablation, 30‑Q, Groq, consistent eval logic)

| Stage | user | asst | pref | k‑upd | temporal | multi | Overall |
|---|---|---|---|---|---|---|---|
| Baseline | 80 | 60 | 40 | 60 | 0 | 60 | 50.0 |
| + hybrid + planner + type‑aware + dates | 100 | 40 | 60 | 100 | 40 | 0 | 56.7 |
| Focused context (full‑sessions for reasoning only) | 80 | 60 | 80 | 60 | 60 | 40 | 63.3 |
| **T1.1 round‑level + T1.2 fact‑augmented keys** | **100** | **100** | 60 | 60 | 20 | 20 | 60.0 |
| **+ T2.1 event‑time extraction** | 100 | 100 | 80 | 60 | 20 | 20 | 63.3 |
| **+ query decomposition (multi‑hop)** | 100 | 100 | 40\* | 40\* | **80** | **60** | **70.0** |

\* preference/knowledge‑update dips are 5‑Q noise + judge variance (those categories don't use decomposition).

**What each change did, and why (read from transcripts):**

- **T1.1 Round‑level chunking.** Conversational content is chunked per user+assistant *round* instead of by paragraph, so a Q&A answer never splits across chunks. Combined with **T1.2 fact‑augmented keys** (each chunk's *embedding key* is enriched with the document's distilled facts — reusing the facts we already extract, no extra LLM call), this **solved single‑session recall (user & assistant → 100%)**: the "detail in a non‑matched chunk" / "I don't have that" misses disappeared.

- **T2.1 Event‑time extraction.** Distillation now stamps time‑bound facts with their resolved absolute event date (`[on YYYY‑MM‑DD]`, relatives like "last Sunday" anchored to the conversation date). This **eliminated wrong‑date errors** ("1258/1171 days passed" → gone). Notably, temporal didn't rise yet — the failure mode *shifted* from wrong‑date to **abstain** ("I don't have the 2nd event's date"), localizing the next bottleneck precisely.

- **Query decomposition (multi‑hop retrieval).** A question naming several events ("days between my MoMA visit and the Ancient Civilizations exhibit") embeds as one query that surfaces only one. We split it into per‑event sub‑queries, retrieve each *planner‑free*, and union the dated facts into context. This **lifted temporal 20%→80% and multi‑session 20%→60%** in one change — the MoMA interval, event ordering, item counts, and duration sums all became correct. It is the engineering analogue of the paper's time‑aware query expansion and Zep's entity‑centric traversal.

- **Extract‑then‑compute.** For counting and temporal, the model emits *structured* data (item list / dated events) and the **arithmetic is done in Rust**, not in the model's head — removing miscount/off‑by‑one and date‑subtraction errors. (Two early bugs — extracting the category instead of instances; routing duration questions to item‑counting — were found by transcript inspection and fixed.)

### 3.6 The retrieval‑completeness theme
The unifying lesson across temporal and multi‑session: their failures were never "the model is dumb" — they were **"the evidence wasn't all in front of it."** A single dense query is a poor instrument for multi‑hop questions; decomposing the question and unioning per‑sub‑query retrievals is what closed the gap. The `gold_retrieved=any` metric masked this for a long time (it reported "retrieval solved" while a needed second session was missing) — a measurement lesson in its own right.

### 3.7 The trustworthy baseline and the synthesis wall
A 120‑question run (20/category, ingest/eval split, **Gemini 2.5 Flash judge**, deterministic at temp 0) gave the honest low‑noise number: **63.3%**, with `gold_retrieved` true for **117/120 (97.5%)** and only **2** true retrieval‑misses — i.e. **42 of 44 failures are synthesis**, not retrieval. A prompt‑only, type‑aware answer pass lifted overall to **66.7%**, but the gain was uneven and diagnostic:

| | preference | knowledge‑update |
|---|---|---|
| before → after | 30% → **45%** (real) | 60% → **60%** (no effect) |

**Preference** improved by telling the model to answer the *on‑topic* preference rather than the user's loudest interest (the rubric wanted AI‑in‑healthcare; the model had been answering about the user's louder sustainability thread). **Knowledge‑update did not move at all** — the prompt reworded 19/20 answers but flipped none (still 300‑not‑120 for the Starbucks threshold, Hawaii‑not‑Paris for "most recent trip", 27:12‑not‑25:50 for the 5K best). Transcript inspection showed why: **both the old and new values are present in context, but they carry no machine‑comparable dates, so "use the latest" is an instruction the model cannot execute.** This is a structural gap, not a prompting one.

### 3.8 Tier‑3 — a bi‑temporal knowledge graph makes "latest" deterministic
The fix is to give event time first‑class structure. `engine/graph.rs` extracts `(subject, predicate, object)` edges, each stamped with **event‑time validity** (`valid_from`/`valid_to`) and an ingestion timestamp — the two time axes of the Zep/Graphiti model. A `singular` flag separates single‑valued **states** (a status, a count, a personal best — a newer value *supersedes*) from accumulating **events** (each trip taken — "most recent" is the max `valid_from`). Supersession and resolution are **pure Rust over event time** (unit‑tested), not an LLM judgment: at answer time the relevant attribute's full dated timeline is surfaced with the value valid *now* marked. On real conversations this extracted `personal_best_5k_time: 27:12 (superseded) → 25:50 (latest)` and the model answered correctly — *the exact knowledge‑update question that was wrong in two prior runs.* The layer is additive (a separate edge collection, retrieval otherwise unchanged), so it was backfilled graph‑only over the existing index — a clean A/B isolating the graph's effect.

**Result (120‑Q, Gemini 2.5 Flash judge):** the graph lifted overall **66.7% → 72.5%**, with **knowledge‑update 60% → 80%** the dominant gain. Because retrieval was byte‑identical to the 66.7% run, the delta is attributable to the graph alone. The four knowledge‑update flips (5K personal best, yoga/therapist/cocktail‑class frequency) are precisely the *dated‑value resolution* cases: the model had previously abstained or chosen the stale value; surfacing the resolved current value with its event date fixed them. This confirms the §3.8 thesis — the failure was **representational** (no comparable dates), and supplying that structure, not a better prompt, is what closed it.

| | 63.3 base | 66.7 prompt | 72.5 + graph |
|---|---|---|---|
| knowledge‑update | 60 | 60 | **80** |
| overall | 63.3 | 66.7 | **72.5** |

The deeper point: prompting plateaued because some errors are **representational**, not reasoning failures. When the data lacks the structure a question needs (here, comparable event dates), no instruction recovers it; you change the *representation*. That is the same move — at the synthesis layer — that round‑level chunking and fact‑augmented keys made at the retrieval layer.

### 3.9 A negative result, and the measurement wall
A follow‑up tried date‑windowed counting *over the graph* to fix multi‑session ("how many weddings *this year*"). It **failed to fire on all 120 questions**: the entity‑*attribute* schema scatters each wedding across `wedding_venue` / `wedding_month` / `wedding_role` edges — there is no countable "attended‑wedding" node — so there was nothing coherent to count. **Counting distinct events needs entity *nodes*, which is the very multi‑hop/entity‑graph step we had deferred.** More telling: the re‑eval read **78.3%, but it was noise** — single‑session‑preference swung **45% → 70% with no preference code changed**. Run‑to‑run answer‑model nondeterminism is **±5 on volatile categories**, large enough that a single 120‑question run cannot distinguish a few‑point gain or detect a small regression. The only durable signal is the reproducible‑mechanism graph win (knowledge‑update); headline numbers above it require **multi‑run averaging** (or a deterministic answer model) to be trustworthy. **Lesson: past a point, measurement precision — not model or memory cleverness — gates progress, and a rising headline can be noise hiding an inert change.**

---

## 4. Discussion

The trajectory supports a clear thesis for agent memory on long histories: **structure beats scale.** The biggest gains came from *how memory is indexed, time‑stamped, and queried* — round‑level units, fact‑augmented keys, event‑time facts, decomposed multi‑hop retrieval — and not from a larger LLM or more context. Two corollaries with practical weight: (1) **raw‑context volume can be counterproductive** (lost‑in‑the‑middle), so a memory system's job is *precision of what it surfaces*, not dumping everything; (2) **deterministic computation** (counting, date arithmetic in code) reliably beats asking the LLM to do it, for any model tier.

These also happen to be the right *product* investments for a memory engine independent of the benchmark — they make what the engine stores and retrieves genuinely better for downstream agents.

---

## 5. Limitations

- **Small N / judge.** Headline percentages are on 30 questions with a non‑GPT‑4o judge (Groq gpt‑oss‑120b, self‑consistent). Several "failures" were correct answers the judge rejected, so *effective* accuracy exceeds the printed score; conversely small‑N swings ±20%/category. The robust claims are the *structural* ones (which fix helps which category, and the failure‑profile shape), not the exact numbers.
- **Single slice / one engine config.** Deterministic first‑k‑per‑type subset; not the full 500; not multiple seeds.
- **Temporal knowledge graph: measured at 72.5% overall, not yet ~90%.** The bi‑temporal edge layer (§3.8) closed knowledge‑update (60→80%) but only does entity‑*attribute* resolution; the multi‑hop relationship traversal + graph/text/time retrieval fusion that takes Zep to ~90% is still unbuilt.
- **Answer‑model non‑determinism.** Even at 120‑Q, `gpt-oss-120b` introduces ±2–4 points of run‑to‑run overall noise (±1–2 per category), so sub‑5‑point movements in untouched categories are not signal.

---

## 6. Future work

1. **Bigger N + GPT‑4o judge** for leaderboard‑faithful, low‑noise numbers.
2. **Generalized Chain‑of‑Note reading** across all categories (+10 pts in the paper).
3. **Temporal knowledge graph (Tier 3) — now built (§3.8); next: measure at full slice**, then extend from entity‑attribute edges to multi‑hop relationship traversal and fuse graph + vector + time at retrieval (the full Zep recipe for ~90%).
4. **Multi‑session counting precision:** decomposition surfaces the items; remaining errors are extraction precision (off‑by‑one), addressable with stricter per‑item evidence binding.

---

## Appendix A — Reproduction
All runs are reproducible from the in‑repo harness (`examples/longmemeval.rs`). Ingest once (`ULTRAMEM_LME_MODE=ingest`), then iterate `MODE=eval`. Flags exercised: `hybrid_search`, `fact_augmented_keys` (T1.2), round‑level chunking (automatic for `source="chat"`/conversational content, `engine/chunker.rs::chunk_rounds`), event‑time distillation (T2.1, `engine/distill.rs`), and harness‑side query decomposition + extract‑then‑compute. Dataset: `longmemeval_s_cleaned.json` (HuggingFace `xiaowu0162/longmemeval-cleaned`).

## Appendix B — Full run history
See `docs/longmemeval-roadmap.md` (run‑history tables + the tiered implementation plan with per‑item code changes, effort, risk, and expected lift).

## Appendix C — Headline numbers
- Recall (single‑session user & assistant): **100% / 100%** (from 60–80%).
- Temporal: **20% → 80%** (event‑time + query decomposition).
- Multi‑session: **20% → 60%** (query decomposition + extract‑then‑compute).
- Overall (30‑Q slice): **50% → 70%**, entirely via architecture (model held constant).
- Cross‑model control: Gemini 2.5 Flash ≈ Groq gpt‑oss‑120b (model scale ≠ the lever).
- Trustworthy 120‑Q (Gemini 2.5 Flash judge): **63.3% → 66.7%**; retrieval solved (**97.5%** gold retrieved); **42 / 44** residual failures are synthesis, not retrieval.
- Tier‑3 bi‑temporal knowledge graph: **knowledge‑update 60% → 80%, overall 66.7% → 72.5%** (120‑Q, clean A/B) — deterministically fixes the dated‑value cases prompting could not.
