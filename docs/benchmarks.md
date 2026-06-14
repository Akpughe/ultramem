# Benchmarks

UltraMem ships its eval harness in-repo (`crates/ultramem-core/examples/probe.rs`) so every number here is reproducible against your own Qdrant, not a screenshot. There are two things worth measuring, and they're different:

1. **Memory capability** — can the system recall, synthesize, and *update* knowledge? (`memtest`)
2. **Retrieval quality** — for a known target document, how high does it rank? (`bench`, scored as H@k / MRR / MemScore)

All harness modes read `QDRANT_URL` / `JINA_API_KEY` / `GROQ_API_KEY` (and `MISTRAL_API_KEY` for OCR) from the environment or `.env`.

---

## 1. Memory capability suite (`memtest`)

The headline "memory, not RAG" proof. Each scenario ingests scripted documents into fresh throwaway collections and checks the *latest* distilled facts — including the case plain RAG fails: a knowledge update.

```bash
cargo run -p ultramem-core --example probe -- memtest
```

| Scenario | What it proves | Result |
|---|---|---|
| single-fact recall | a stated fact is distilled and retrievable | **PASS** |
| cross-document synthesis | facts spread across two docs are both recalled | **PASS** |
| knowledge update (contradiction) | a superseded fact (Adidas→Puma) is filtered out; only the current one is served | **PASS** |
| | | **3/3 (100%)** |

The contradiction scenario is the important one: after ingesting "prefers Adidas" then "switched to Puma," the engine flips the Adidas fact's `is_latest=false` and returns only Puma. Verified live; the same behavior is covered by the `contradiction_supersedes_old_memory` integration test.

---

## 2. Retrieval quality (`bench`)

A deterministic benchmark against a **frozen golden set**: a sample of indexed documents, each paired with one natural-language query generated from its own content. For each query we record the rank of its source document, then aggregate.

### Metrics

- **H@k** — fraction of queries whose target document is in the top *k* (H@1, H@3, H@10).
- **MRR** — mean reciprocal rank (`1/rank`, 0 on a miss); the headline quality number.
- **latency** — wall-clock per query (mean and p95), full pipeline (plan → embed → search → rerank).
- **tokens injected** — approximate context size the answer model would receive (top-8 chunk bodies capped + facts) — the efficiency axis.
- **MemScore** — after SuperMemory's memorybench: `100 · MRR · efficiency`, where efficiency stays at 1.0 until latency exceeds ~2s or context exceeds ~2k tokens, so a fast, precise system scores ≈ MRR·100.

### Reproduce it

Against a throwaway namespace so your real data is untouched, using the committed synthetic corpus (`eval/corpus_demo.json`, 24 documents across distinct topics):

```bash
export ULTRAMEM_CHUNKS_COLLECTION=ultramem_demo_chunks
export ULTRAMEM_FACTS_COLLECTION=ultramem_demo_facts
export ULTRAMEM_GOLDEN=eval/golden_demo.json

cargo run -p ultramem-core --example probe -- seed eval/corpus_demo.json   # ingest the corpus
cargo run -p ultramem-core --example probe -- bench build                  # freeze one query per doc
cargo run -p ultramem-core --example probe -- bench                        # score it
cargo run -p ultramem-core --example probe -- drop                         # clean up the demo collections
```

### Results (synthetic 24-doc corpus)

Measured against the committed 24-doc corpus, default engine config (Jina embeddings + reranker, Groq planner), frozen golden set of 24 self-generated queries:

| Metric | Value |
|---|---|
| H@1 | 100% (24/24) |
| H@3 | 100% |
| H@10 | 100% |
| MRR | 1.000 |
| latency mean / p95 | 2602 ms / 7139 ms |
| tokens injected (mean) | 176 |
| MemScore | 97/100 |

> **Read this honestly.** This corpus is 24 documents on *deliberately distinct* topics (Postgres pooling, espresso dial-in, marathon training, JWT auth, …), so a working retriever *should* rank every target #1 — perfect H@1 here demonstrates the pipeline is sound and the result is reproducible on your machine, **not** that retrieval is "solved." A real difficulty signal needs near-duplicate distractors; for that, point the harness at your own indexed documents (omit the collection overrides) and run `bench build` / `bench`, or grow the corpus.
>
> Latency is dominated by per-query provider round-trips (planner LLM + embedding + reranker, all remote HTTP); it's network-bound and varies run to run. `tokens injected` (176) is the small, precise context the answer model would receive — the efficiency half of MemScore.

---

## 3. Ingest-side A/B (`abtest`)

`bench` measures a *fixed* index, so it can't show the effect of an ingest-time change (different chunking, contextual prefix, hybrid search). `abtest` ingests a frozen corpus twice into throwaway collections — feature OFF then ON — replays the same queries against each, and prints the delta. Same corpus, same queries, one variable.

```bash
cargo run -p ultramem-core --example probe -- abtest chunking build   # freeze eval/corpus.json
cargo run -p ultramem-core --example probe -- abtest chunking         # smart chunking OFF vs ON
# feature ∈ { contextual | chunking | hybrid }
```

---

## 4. Ingest throughput (`bench_ingest`)

Walks document folders and pushes files through the full pipeline (OCR → chunk → embed → upsert → distill) at real concurrency, into throwaway `ultramem_bench_*` collections it drops afterward. Reports per-document latency (mean/median/p95) and throughput.

```bash
cargo run -p ultramem-core --example bench_ingest -- 200 ~/Documents
```

---

## Methodology notes

- **Self-supervising queries.** `bench build` derives each query from a document's own text via the LLM, then freezes it — so the golden set never drifts between runs, but isn't hand-tuned to flatter the retriever.
- **Isolation.** Every harness mode either uses dedicated collections or drops what it creates; production namespaces are never touched.
- **No cherry-picking.** The frozen golden set is regenerated by stride-sampling the corpus deterministically, not by selecting easy documents.
