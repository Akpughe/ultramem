# Structure Beats Scale: An Honest Engineering Log of UltraMem on LongMemEval-S

*How we climbed from 50% to 72.5% on a brutal long-term-memory benchmark — what worked, what quietly failed, and why the bottleneck was never the model.*

---

> **TL;DR**
> - We ran [UltraMem](https://github.com/Akpughe/ultramem) — an open-source, two-layer memory engine — against **LongMemEval-S**, the standard test of whether an AI system can recall, synthesize, and *update* knowledge across ~50-session chat histories.
> - We went from a **50% baseline to a trustworthy 72.5%**, and *every* gain came from **memory architecture, not a bigger LLM**. A frontier model and a mid-tier open model scored within noise of each other.
> - The single biggest lever was giving **time first-class structure**: a **bi-temporal knowledge graph** that resolves "what is the current value?" deterministically lifted **knowledge-update from 60% to 80%** — a problem no amount of prompting could fix.
> - We're publishing the **failures too**: a prompting plateau, an inert feature that did nothing, and a measurement-noise wall where a single category swung 25 points with *zero code changes*. If you only read the wins, you learn the wrong lessons.
> - Numbers are **indicative** (120-question slice, judged by Gemini 2.5 Flash, not the leaderboard's GPT-4o). Everything is measured, nothing is tuned, and the harness is in the repo so you can reproduce it.

---

## Why this benchmark, and why it's hard

Most "memory" demos test the easy case: store a fact, retrieve it a minute later. Real assistants face the hard case — a fact stated months and dozens of conversations ago, later **contradicted**, asked about now. [**LongMemEval**](https://arxiv.org/abs/2410.10813) (Wu et al., ICLR 2025) is built for exactly this. Each of its 500 questions hides its evidence inside a ~115k-token, ~50-session chat haystack, and grades six distinct abilities:

| Category | The skill it tests |
|---|---|
| `single-session-user` | recall a fact the user stated once |
| `single-session-assistant` | recall something the assistant said |
| `single-session-preference` | apply a preference expressed in passing |
| `knowledge-update` | report the **latest** value of a fact that changed |
| `temporal-reasoning` | ordering, "how long ago", date arithmetic |
| `multi-session` | aggregate evidence spread across sessions |

The honest top of the field is **Zep/Graphiti at 90.2%**, achieved with a *temporal knowledge graph* ([arXiv 2501.13956](https://arxiv.org/abs/2501.13956)). The LongMemEval authors also publish a measured recipe for vector systems — round-level indexing, fact-augmented keys (**+9.4% recall**), time-aware querying (**+11.3% temporal**), and Chain-of-Note reading (**+10 QA points**). Those two references became our map.

## The system under test

UltraMem is a self-hostable memory engine (Rust). It keeps **two layers**:

1. A **document layer** — content is chunked, embedded (Jina/OpenAI), and stored in a vector DB (Qdrant), retrieved with hybrid dense+sparse search, a query planner, and a cross-encoder reranker.
2. A **memory layer** — an LLM **distills atomic facts** from each document, and those facts are **reconciled over time**: duplicates dropped, contradictions marked as an `UPDATE` (the old fact's `is_latest` flips to false), enrichments as `EXTEND`. The contradiction is resolved *at write time*, not punted to the reader.

That second layer is the whole thesis: memory isn't retrieval over chunks; it's a maintained, time-aware model of what's true now.

## How we kept ourselves honest

Before any result, the method — because on a benchmark this noisy, method *is* the result:

- **Ingest once, evaluate many.** Ingesting a haystack takes hours; re-scoring an answer-logic change takes minutes. Splitting the two let us iterate dozens of times.
- **Failure attribution on every question.** A `gold_retrieved` flag tells us, per question, whether retrieval even surfaced the evidence — splitting every failure into *retrieval-miss* vs *synthesis*. You cannot fix what you can't localize.
- **Read the transcripts.** Every conclusion below was read out of per-question logs (question, gold, answer, judge verdict, retrieved sessions). Not one was guessed from an aggregate.
- **State the caveats up front.** Our judge is Gemini 2.5 Flash, not the leaderboard's GPT-4o; our headline runs are a 120-question slice (20/category), not the full 500. So our numbers are **indicative, not leaderboard-official** — and we'll show you exactly where that matters.

---

## The climb: what actually moved the number

We'll give the trustworthy 120-question figures for the headline, and use a smaller, noisier 30-question ablation to show the *direction* each change pushed each category. Here's that ablation — the shape of the journey:

| Stage | user | asst | pref | k-upd | temporal | multi | Overall |
|---|---|---|---|---|---|---|---|
| Baseline | 80 | 60 | 40 | 60 | 0 | 60 | **50.0** |
| + hybrid search, planner, type-aware prompts, dates | 100 | 40 | 60 | 100 | 40 | 0 | 56.7 |
| Focused context (full sessions for reasoning only) | 80 | 60 | 80 | 60 | 60 | 40 | 63.3 |
| **+ round-level chunking + fact-augmented keys** | **100** | **100** | 60 | 60 | 20 | 20 | 60.0 |
| **+ event-time fact extraction** | 100 | 100 | 80 | 60 | 20 | 20 | 63.3 |
| **+ query decomposition (multi-hop)** | 100 | 100 | 40\* | 40\* | **80** | **60** | **70.0** |

\* small-sample noise; those categories don't use decomposition.

Four interventions did the work.

**1. Round-level chunking + fact-augmented keys → single-session recall solved.** Early on, the model kept answering *"I don't have that information"* to facts that were demonstrably in the haystack. The cause was mundane and fixable: conversational content was being chunked by paragraph, so a question's answer landed in a chunk that *didn't* match the query. We re-chunked conversations **one user+assistant round per chunk**, and enriched each chunk's embedding key with the document's distilled facts (reusing facts we already extract — no extra LLM call). Single-session recall went to the ceiling. **The model was never dumb; the evidence just wasn't in front of it.**

**2. Event-time extraction → wrong dates disappeared.** Temporal questions reason about when things *happened*, but our facts were stamped with when they were *discussed*. So "how many days between X and Y" produced absurdities like "1,258 days." We taught distillation to resolve and stamp the **event date** (`[on 2023-05-20]`, with relatives like "last Sunday" anchored to the conversation date). The wrong-date errors vanished — and, tellingly, the failure *mode shifted* from wrong-date to *"I don't have the second event's date."* The bug moved, which told us exactly where to aim next.

**3. Query decomposition → temporal and multi-session jumped.** A question naming two events ("days between my MoMA visit and the Ancient Civilizations exhibit") embeds as a *single* vector that surfaces *one* of them. So we split such questions into per-event sub-queries, retrieve each independently, and union the dated evidence. In one change, **temporal went 20%→80% and multi-session 20%→60%.** This is the engineering analogue of the paper's time-aware query expansion and Zep's entity-centric traversal: a single dense query is simply the wrong instrument for a multi-hop question.

**4. Extract-then-compute → no more arithmetic in the model's head.** For counting and date math, we have the model emit *structured* data (a list, dated events) and do the arithmetic **in Rust**. LLMs miscount and fumble subtraction; code doesn't.

---

## Two findings that reframed the whole effort

### Architecture dominates model scale

We swapped our mid-tier open model (Groq `gpt-oss-120b`) for a frontier model (Gemini 2.5 Flash), same everything else:

| Model | Overall (30-Q) |
|---|---|
| Groq `gpt-oss-120b` | 63.3% |
| Gemini 2.5 Flash | 60.0% |

Within noise. **Identical** failure profiles. The ceiling was never the LLM — it was how memory is indexed, time-stamped, and queried. Every dollar of effort went to architecture after this, and it paid off; a bigger model would not have.

### More context actively hurt

The intuitive move on a recall benchmark is "give the model more." We tried it: full sessions for every category, wider retrieval. The score **regressed from 63% to 47%.** The model started abstaining on questions it had previously gotten right — classic [lost-in-the-middle](https://arxiv.org/abs/2307.03172). A memory system's job is **precision of what it surfaces, not volume.** Dumping the haystack back into the context window defeats the entire point of having a memory.

---

## The centerpiece: making "latest" deterministic

Here's where it gets interesting — and where we hit the wall that defines the rest of the story.

The trustworthy 120-question run put us at **63.3%**, and the failure attribution was stark: **117 of 120 gold sessions were retrieved (97.5%). 42 of 44 failures were synthesis, not retrieval.** We had essentially *solved retrieval* and the remaining problem was the model reasoning over evidence it already had.

So we tried the obvious thing: better prompts. Type-aware instructions lifted us to **66.7%** — but the gain was lopsided and diagnostic.

| Category | before → after prompts |
|---|---|
| single-session-preference | 30% → **45%** (real improvement) |
| **knowledge-update** | 60% → **60%** (nothing) |

Preference improved (we told the model to answer the *on-topic* preference, not the user's loudest interest). But **knowledge-update did not move at all.** The prompt reworded 19 of 20 answers and flipped *zero*: still "300 stars" not the user's "120", still "Hawaii" not the latest "Paris", still "27:12" not the improved "25:50".

We read the transcripts, and the reason was profound: **both the old and new values were sitting right there in the context — with no machine-comparable dates to order them by.** "Use the latest value" is an instruction the model literally *cannot execute* when the data has no notion of "latest." This wasn't a reasoning failure. It was a **representational** one. No prompt fixes a representation.

**So we changed the representation.** We built a **bi-temporal knowledge graph** (`engine/graph.rs`): every fact becomes a `(subject, predicate, object)` edge stamped with two time axes — **event time** (`valid_from`/`valid_to`, when it was true in the world) and **ingestion time** (when we learned it). A `singular` flag separates single-valued **states** (a status, a count, a personal best — where a newer value *supersedes* the old) from accumulating **events** (each trip you took). Supersession and "what holds now" are computed in **pure, unit-tested Rust over event time** — not asked of the model.

On real conversations it extracted `personal_best_5k_time: 27:12 (superseded) → 25:50 (latest)`, surfaced the resolved value with its date, and the model answered correctly — *the exact question that had been wrong in two prior runs.*

We backfilled the graph over the **identical** existing index (retrieval byte-for-byte unchanged), so the result is a clean A/B that isolates the graph's effect:

| | 63.3% base | 66.7% prompts | **72.5% + graph** |
|---|---|---|---|
| knowledge-update | 60 | 60 | **80** |
| overall | 63.3 | 66.7 | **72.5** |

**Knowledge-update 60% → 80%, overall to 72.5%** — driven entirely by giving time a structure the model could query. The same move that round-level chunking made at the *retrieval* layer, the graph made at the *synthesis* layer: when the data lacks the structure a question needs, you don't write a cleverer prompt — you fix the data.

---

## The failures we're keeping in

Most benchmark write-ups are a highlight reel. Ours isn't, because the failures taught us more than the wins.

**The prompting plateau.** We genuinely believed type-aware prompts would crack knowledge-update. They did nothing. We only learned *why* — the missing date representation — by accepting the null result and reading transcripts instead of trying prompt #20.

**A feature that did literally nothing.** Buoyed by the graph win, we built date-windowed counting *over* the graph to fix "how many weddings *this year*." It **fired on 0 of 120 questions.** The reason was humbling: our entity-*attribute* schema scatters each wedding across `wedding_venue`, `wedding_month`, `wedding_role` edges. There is no countable "attended-wedding" *node*. Counting distinct events needs entity **nodes** — the very graph-traversal step we'd deferred. The cheap version wasn't a shortcut; it was a dead end, and the only honest move was to ship nothing and say so.

**The measurement wall.** That same inert experiment's re-run read **78.3%** — a "+6" we did *not* earn. Single-session-preference had swung **45% → 70% with no preference code changed at all.** Run-to-run answer-model nondeterminism is **±5 points on volatile categories** — large enough that a single 120-question run *cannot* distinguish a real few-point gain from noise, or catch a small regression hiding under a rising headline. This is the most important thing we learned: **past a point, measurement precision — not model or memory cleverness — gates progress.** A number going up is not evidence that you did something.

And a related one: **preference is partly a judge ceiling, not an engineering gap.** Auditing the "failures," roughly half were defensible, on-topic answers the strict judge rejected. Chasing that category harder would mostly have been chasing the judge's subjectivity.

---

## Where we landed, vs the field

Here's UltraMem's per-category accuracy next to published figures for Shram, Supermemory, Zep, and a full-context baseline:

| Category | **UltraMem** | Shram | Supermemory | Zep | Full context |
|---|---|---|---|---|---|
| Single-Session User | 90% | 100% | 97.1% | 92.9% | 81.4% |
| Single-Session Assistant | 70% | 100% | 96.4% | 80.4% | 94.6% |
| Single-Session Preference | 45% | 90% | 70% | 56.7% | 20% |
| Knowledge Update | 80% | 93.6% | 88.4% | 83.3% | 78.2% |
| **Temporal Reasoning** | **85%** | 71.4% | 76.7% | 62.4% | 45.1% |
| Multi-Session | 65% | 72.9% | 71.4% | 57.9% | 44.3% |

**Read this honestly** — and we mean it: UltraMem's bars are the **120-question slice, judged by Gemini 2.5 Flash**; the others are **full-set, GPT-4o-judged** leaderboard figures. *Same benchmark, not the same N or judge — so this is ballpark positioning, not a head-to-head ranking.* With that stated plainly: we're **mid-pack overall, best-in-class on temporal reasoning (85%)**, competitive on knowledge-update (where the graph earned its keep), and weakest on single-session-assistant — a clear, honest next target.

---

## What generalizes (even if you never touch this benchmark)

1. **Structure beats scale.** Every gain came from *how memory is organized*, not from a larger model or more context. For agent memory on long histories, the architecture is the lever.
2. **Some errors are representational, not reasoning.** If the data lacks the structure a question needs — comparable dates, countable nodes — no prompt recovers it. Change the representation.
3. **More context can be negative.** A memory system's value is precision, not recall volume. Lost-in-the-middle is real.
4. **Compute deterministically.** Counting and date math belong in code, not in the model's head — at every model tier.
5. **Measure before you believe.** Under ±5 noise, a rising headline can hide an inert change. Multi-run averaging isn't bureaucracy; it's the difference between signal and self-deception.

## What's next

The path to the ~90% band is the one Zep proved: promote events and entities to **first-class nodes** and add **multi-hop relationship traversal** with fused graph + vector + time retrieval. That single step fixes our weakest spots (multi-session counting) *and* unlocks the queries our attribute-only graph can't express. First, though, we'll do the unglamorous thing our own data demands — **pin the number with averaged runs** so the next gain is real, not noise.

## Reproduce it

The harness ships in the repo. It uses per-question namespace isolation, ingests each haystack into a throwaway namespace, and never touches real data.

```bash
# QDRANT_URL / JINA_API_KEY / GROQ_API_KEY from env or .env
# args: [questions-per-category] [dataset.json]
cargo run --release -p ultramem-core --example longmemeval -- 20 eval/longmemeval_s.json
# → accuracy by question_type + overall, with per-question logs to inspect.
```

Code, harness, and the full run history: **[github.com/Akpughe/ultramem](https://github.com/Akpughe/ultramem)**.

---

### References

- Wu et al., *LongMemEval: Benchmarking Chat Assistants on Long-Term Interactive Memory*, ICLR 2025 — [arXiv:2410.10813](https://arxiv.org/abs/2410.10813)
- Rasmussen et al., *Zep: A Temporal Knowledge Graph Architecture for Agent Memory* — [arXiv:2501.13956](https://arxiv.org/abs/2501.13956)
- Liu et al., *Lost in the Middle: How Language Models Use Long Contexts*, TACL 2024 — [arXiv:2307.03172](https://arxiv.org/abs/2307.03172)
- Yu et al., *Chain-of-Note: Enhancing Robustness in Retrieval-Augmented Language Models* — [arXiv:2311.09210](https://arxiv.org/abs/2311.09210)

---

## Appendix — a short version for LinkedIn

> We took our open-source memory engine, **UltraMem**, from **50% to 72.5%** on **LongMemEval-S** — the benchmark that tests whether an AI can remember, *update*, and reason over months of conversation.
>
> The lesson wasn't "use a bigger model." A frontier model and a mid-tier open model scored within noise of each other. **Every gain came from memory architecture:**
>
> → Round-level chunking solved single-session recall (the model wasn't dumb — the evidence wasn't in front of it).
> → A **bi-temporal knowledge graph** took knowledge-update from 60% → 80% by making "what's the *latest* value?" a deterministic lookup instead of an LLM guess. No prompt could fix it; only a better data representation could.
> → We're **best-in-class on temporal reasoning (85%)**.
>
> And the parts nobody posts: a feature we built fired on **0 of 120** questions. A "+6" jump turned out to be **pure noise** — one category swung 25 points with zero code changed. If you only celebrate the wins, you learn the wrong lessons.
>
> Three takeaways for anyone building agent memory: **structure beats scale; some failures are representational, not reasoning; and measure before you believe a number went up.**
>
> Full write-up + reproducible harness 👇
