# How UltraMem Works — and How Re-Kalei Builds On It

*A from-scratch, end-to-end explanation of the memory engine: what it stores, how
memory is constructed, exactly what the LLM does (and doesn't do), why it behaves
like memory instead of search, and how a product like Re-Kalei turns it into
agents that remember.*

This document is written to be read on two levels. The **plain-language** passages
explain the ideas with no jargon. The **`Under the hood`** call-outs give the exact
mechanism — file, constant, threshold, prompt — so an engineer can trust it and
extend it. Read the prose for the intuition; dip into the call-outs when you want
the proof.

---

## Part 0 — The one-sentence version

> UltraMem reads everything you give it, writes down the **durable facts** worth
> remembering about a person or an agent, **reconciles** each new fact against what
> it already knew (so a changed fact *replaces* the old one instead of piling up),
> and serves both the **raw passages** (like search) and the **current facts** (like
> memory) on demand — per user, per agent, kept correct over time.

Everything else in this document is detail on those verbs: *read, write down,
reconcile, serve*.

---

## Part 1 — The core idea: "memory, not RAG"

To understand UltraMem you have to understand the thing it refuses to be.

### What plain RAG does

Most "AI memory" today is **RAG** (Retrieval-Augmented Generation). Mechanically:

```
chunk documents → embed the chunks → at question time, embed the question →
vector-search for the nearest chunks → paste them into the prompt
```

RAG is genuinely useful. It grounds a model in text it was never trained on. But a
RAG index has **no opinion about you**. Every chunk it has ever seen is equally
"true." It has no notion of *facts*, *time*, or *identity* — only text and distance.

The consequence is the failure everyone has felt: you told the assistant in January
you use Adidas. In June you tell it you switched to Puma. Ask it what shoes you
wear, and RAG hands the model **both** passages — they're both in the corpus, both
similar to the question — and the model has to guess, or it hedges. RAG can't *forget*
the old truth because it never understood it was a truth in the first place.

### What a memory layer adds

A memory layer answers a different question. RAG answers *"what do I know?"*.
Memory answers *"what do I remember **about you**?"* To do that, UltraMem runs a
**second pass** over every document it ingests:

```
distill the document into atomic facts
  → embed each fact
  → find the nearest facts it already stored
  → classify each new fact: NEW / DUPLICATE / UPDATE / EXTEND
  → act on it: insert, drop, supersede-the-old-one, or link
```

The output of that pass isn't chunks. It's a **small, reconciled set of durable
facts that carry state** — each one knows whether it's still current and when it
expires. That single change is what unlocks the three things RAG structurally
cannot do:

| | RAG (document layer) | UltraMem (memory layer) |
|---|---|---|
| **Unit of memory** | a text chunk | a distilled fact |
| **State** | stateless; every chunk is permanent and equal | temporal; facts carry `is_latest` and `valid_until` |
| **Identity** | none — same index for everyone | per-namespace; one isolated pool per user / per agent |
| **Knowledge update** | old and new both returned forever | old fact superseded; only the current one is served |
| **Standing context** | none | a compiled, cached profile of "what's always true about you" |

UltraMem does **both** layers and serves them together. It takes RAG seriously
(smart chunking, a cross-encoder reranker, hybrid search, a query planner) *and*
adds the memory layer on top. A single search call returns `documents` (the RAG
hits, for grounding in specifics) and `memories` (the current facts, for durable
truth). You never choose; you get both.

> **Under the hood.** Two Qdrant collections: `ultramem_chunks` (the document
> layer) and `ultramem_facts` (the memory layer). They're searched in parallel on
> every query (`tokio::join!` in `engine/mod.rs::retrieve_for_plan_tagged`). The
> whole engine is a thin async Rust library — *no ML runs locally*; every heavy
> step is an HTTP call to a swappable provider.

---

## Part 2 — The anatomy: what the system is made of

Before the flows, here's the cast. UltraMem is deliberately built as a small core
that orchestrates five kinds of outside service, each behind a Rust **trait** (an
interface) so no vendor is welded in.

```
                       ┌──────────────────────────────────────────┐
   your app / agent →  │            ultramem-server (HTTP)         │
   (Re-Kalei)          │   /v1/memories  /v1/search  /v1/profile   │
                       │   /v1/timeline  /v1/reindex  /v1/health    │
                       └─────────────────────┬────────────────────┘
                                             │
                       ┌─────────────────────▼────────────────────┐
                       │            ultramem-core (engine)          │
                       │  chunk · embed · search · distill · …      │
                       └──┬───────┬────────┬────────┬────────┬─────┘
                          │       │        │        │        │
                    Embedder  Reranker   OCR      LLM   VectorStore
                     (Jina)    (Jina)  (Mistral) (Groq/  (Qdrant)
                       │                         Anthropic)
                  swap → OpenAI,           swap → any OpenAI-
                  any dim                  compatible or Anthropic
```

| Role | Default provider | What it does | Swappable? |
|---|---|---|---|
| **Embedder** | Jina embeddings v3 (1024-dim) | turns text into a vector so similarity = nearness | yes — OpenAI built in, or inject any `dyn Embedder` |
| **Reranker** | Jina cross-encoder | re-scores candidates for *true* relevance to the question | yes |
| **OCR** | Mistral OCR | reads scanned PDFs and images (pixels → text) | yes |
| **LLM** | Groq (OpenAI-compatible) + Anthropic | the *judgment*: distill, reconcile, plan, profile | yes — any OpenAI-shaped endpoint or Anthropic |
| **Vector store** | Qdrant | stores vectors + payloads, does the actual search & filtering | yes |

> **Under the hood.** `MemoryEngine::new` selects providers from config; `with_embedder`,
> `with_reranker`, `with_ocr`, `with_llm`, `with_store` let you replace any of them
> without touching engine code (`engine/mod.rs`). Swapping the embedder via env
> (`ULTRAMEM_EMBEDDER=openai`) is a one-line change; the engine reads the new
> `dim()` and creates collections to match. The LLM layer (`llm.rs`) speaks both
> the OpenAI Chat Completions wire format *and* Anthropic's Messages API behind one
> `ResolvedModel` type, so "which model does which job" is pure configuration.

A subtle but important design point: **different LLM "roles" can be different
models.** The engine resolves two model slots — a fast **plan** model
(default `llama-3.3-70b-versatile` on Groq) for the cheap query-rewriting step, and
a stronger **distill** model (default `openai/gpt-oss-120b` on Groq) for the
extraction/reconciliation/profile work where judgment matters. You can point either
at OpenAI, Anthropic, a local Ollama, OpenRouter — anything.

---

## Part 3 — Writing memory: the ingestion pipeline, step by step

This is the heart of "how memory is constructed." When you send a document to
`POST /v1/memories` (or call `engine.add_document`), here is everything that
happens, in order. Returning success means the document is fully indexed and
searchable.

```
   a document arrives
        │
   (1)  ACQUIRE TEXT  ── file? → Jina Reader → (empty? → Mistral OCR / textutil)
        │              ── image? → Mistral OCR directly
        │              ── web URL? → optional Jina Reader body fetch (off by default)
        │              └─ cap at 60,000 chars
        │
   (2)  CHUNK  ── markdown → by heading;  transcript → by speaker;  else → paragraphs
        │         (~1,200 chars each, ~200-char overlap)
        │
   (3)  EMBED  ── each chunk, prefixed with a readable title (+ optional context blurb)
        │
   (4)  UPSERT CHUNKS → the DOCUMENT LAYER is now searchable
        │
   (5)  DISTILL FACTS  ── (skipped for tiny captures) segment → extract → merge
        │
   (6)  RECONCILE & STORE  ── for each fact: nearest memory? classify. supersede/insert.
        │
        ▼
   the MEMORY LAYER is now updated
```

### Step 1 — Acquire the text

Different inputs need different readers. UltraMem uses a **hybrid extraction**
strategy because no single tool reads everything cross-platform:

- **Text PDFs, Office docs, HTML** → **Jina Reader**, which returns clean markdown.
- If Jina comes back empty (a *scanned* or image-only PDF has no text layer) →
  fall back to **Mistral OCR** for PDFs, or local `textutil` for Office docs.
- **Images** (screenshots, photos) → **Mistral OCR** directly, because neither a
  markdown reader nor `textutil` can read pixels.
- **Browser captures** can optionally have their full page body fetched and cleaned
  (Jina Reader URL mode) — but this is **off by default**, deliberately, because
  fetching every URL a user visits sends it to a third party. Privacy is a default,
  not an afterthought.

The result is truncated to 60,000 characters (~50 chunks) so a pathological input
can't blow up the pipeline.

> **Under the hood.** `add_document` in `engine/mod.rs`, with `extract.rs` (Jina
> Reader file + URL modes, local `textutil` fallback) and `mistral.rs` (OCR). The
> "is there a real text layer?" test is a 24-character floor (`MIN_EXTRACT`). All
> extraction failures are surfaced but the file's own header (name, path, dates) is
> always kept so date questions still work.

### Step 2 — Chunk, the way the content wants to be chunked

A chunk is a bite-sized passage that gets its own embedding. Naïve RAG chops every
document into fixed-size blocks and loses meaning at the seams. UltraMem chooses the
**split strategy from the content type** (an idea it calls "Super RAG"):

- **Markdown** splits on its heading hierarchy, and each section is prefixed with
  its **heading trail** (`# Guide ▸ ## Setup`) so a chunk always carries its location
  in the document.
- **Meeting transcripts** split on **speaker turns** (`Alex:`, `[00:12] Jordan:`),
  so a single person's point stays whole instead of being cut mid-thought.
- **Everything else** packs whole paragraphs up to the target size, splitting at
  sentence boundaries only when a paragraph is oversized.

Targets: ~1,200 characters per chunk, ~200 characters of **overlap** carried from
the end of one chunk to the start of the next so a fact straddling a boundary isn't
lost. All sizing is in *characters*, not bytes, so it's unicode-safe.

> **Under the hood.** `engine/chunker.rs`: `chunk_doc` routes by `source` and file
> extension (with a `looks_like_markdown` heuristic for pasted/captured markdown
> that has no `.md` extension); `chunk_markdown`, `chunk_transcript`, `chunk_text`
> share one `pack_pieces` packer. `CHUNK_TARGET = 1200`, `CHUNK_OVERLAP = 200`.

### Step 3 — Embed, with the title baked in

Each chunk is converted to a vector by the embedder. One quiet trick matters a lot:
**the title (or filename) is prepended to the embedding input.** A file called
`newton-profile_v2.pdf` describes every page of itself, but the body text rarely
repeats the filename. By normalising `newton-profile_v2.pdf` → `newton profile v2.pdf`
and prefixing it to each chunk's embed input, queries that mention the document's name
actually match it. **The stored chunk text stays clean** — this prefix only shapes the
vector, never what gets displayed back.

(Optionally, "Contextual Retrieval" prepends a one-line LLM-written blurb situating
the chunk in its document. It's behind a flag and **off by default**: an A/B test on
real documents showed no doc-level retrieval gain for the per-document LLM cost. The
machinery is kept so it can be revisited with a chunk-level metric.)

> **Under the hood.** `embed_input` in `engine/mod.rs`; the optional blurb is
> `context.rs::doc_context`. Embeddings are requested with `EmbedTask::Passage` for
> stored content and `EmbedTask::Query` for questions — Jina v3 uses different
> internal task encodings for the two, which improves matching.

### Step 4 — Upsert the chunks → the document is now searchable

The chunk vectors and their payloads (doc id, content, title, source, reference,
timestamp, **container tag**) are written to Qdrant. **At this point the RAG layer is
live** — the document can be found by search. Everything after this is the memory
layer, and it's intentionally non-fatal: if distillation fails, the document is still
searchable.

(In hybrid mode each chunk also stores a **sparse** vector — raw term frequencies —
alongside the dense one, so lexical matches like error codes and proper nouns aren't
blurred away. More on that in Part 6.)

### Step 5 — Distill the document into facts (the first big LLM job)

This is where "memory" begins. The document is handed to the **distill LLM** with one
instruction: *extract every distinct fact worth remembering about the user, their
work, projects, people, decisions, preferences, and plans — and each fact must stand
alone without surrounding context.*

"Stand alone" is the crucial constraint. The model doesn't extract `"the migration is
blocked"`; it extracts `"Recally's payments migration is blocked on the payments
team"`. A self-contained fact embeds well and retrieves correctly on its own later.

Three properties make this robust:

1. **It scales with the content, not a fixed cut.** The document is split into
   ~6,000-character segments; each segment yields the facts *it* genuinely supports
   (zero for boilerplate, many for a dense meeting). A final **merge pass** dedups
   near-duplicates across segments. A long meeting produces many facts; a one-line
   browser visit produces one or none. (An older "read only the first N characters"
   design would miss a decision made on the last page — there's a live test that
   guards exactly that.)

2. **It knows when a fact will stop being true.** If — and only if — a fact expires
   on a specific date (a deadline, "the exam is tomorrow"), the model appends
   `[until YYYY-MM-DD]`. That suffix is parsed off and stored as a `valid_until`
   timestamp, so time-bound facts quietly disappear from results after they lapse.

3. **It's allowed to find nothing.** Boilerplate, navigation text, generic content
   → the model returns `[]` and no memory is created. Memory is selective by design.

> **Under the hood.** `engine/distill.rs`. `SEGMENT_CHARS = 6000`,
> `MAX_FACTS_PER_SEGMENT = 10`, `MAX_TOTAL_FACTS = 50`. Extraction runs at
> temperature 0.3; a separate `MERGE_SYSTEM` prompt dedups. Tiny captures (content
> under 280 chars) skip distillation entirely — their own text is already embedded
> in the chunk layer, so distilling adds nothing and roughly halves backfill cost.
> Parsing tolerates code fences and prose around the JSON array; on a hard failure
> the engine logs and keeps whatever it already has (partial coverage beats none).

### Step 6 — Reconcile each fact against memory (the job that *makes* it memory)

Distillation gives you facts. Reconciliation is what stops them from piling into a
contradictory heap. For **each** new fact:

1. **Embed it**, then find its single **nearest existing memory** *within the same
   namespace* (a fact in user A's pool can never touch user B's).
2. If nothing is close enough, it's obviously **NEW** — store it, no LLM needed.
3. If something *is* close, ask the LLM to classify the relationship as exactly one of:

| Relation | Meaning | What happens |
|---|---|---|
| **NEW** | the near match was a coincidence; different subject | insert as a fresh memory |
| **DUPLICATE** | says the same thing already stored | drop it (no new information) |
| **UPDATE** | contradicts / supersedes the old fact (*"switched Adidas → Puma"*) | insert the new one **and flag the old one `is_latest = false`** |
| **EXTEND** | adds detail to the same subject without contradicting | insert and record an "extends" link; both stay |

The superseded fact is **never deleted** — history is preserved — but it's flagged so
that search only ever returns the current truth. *That* is the knowledge update RAG
can't do: the contradiction is resolved **at write time**, once, instead of being
dumped on the reader every time they ask.

Two efficiency decisions are worth calling out, because they're why this is cheap:

- **Most facts are NEW and never reach the LLM.** Only facts whose nearest neighbour
  is genuinely similar (cosine ≥ 0.75) are candidates worth classifying. The rest
  short-circuit to NEW for free.
- **All the candidates from one document are classified in a single batched call**,
  not one call per fact. So the cost is roughly *one* classification call per
  document, on top of the extraction calls — not per fact.

And it degrades safely: if the classifier call or its JSON parse fails for any
reason, every candidate falls back to **NEW**. The engine will never *lose* a fact
because a model misbehaved; the worst case is a duplicate, not a hole.

> **Under the hood.** `engine/memory.rs` (`reconcile`, `Relation`, `Action`,
> `RELATE_THRESHOLD = 0.75`) and `engine/mod.rs::index_memories`. The nearest-memory
> search is filtered to *latest, non-expired, same-tag* memories. Survivors are
> upserted with `is_latest = true` and lifecycle metadata (`supersedes`, `extends`,
> `valid_until`, `kind`); superseded ids are flipped in one `set_payload` call. The
> classification prompt forces a strict JSON array and runs at temperature 0.0 for
> determinism. Legacy facts that predate these fields are treated as "latest,
> never-expiring" so old data stays searchable with no migration.

---

## Part 4 — Reading memory: the retrieval pipeline, step by step

Now the other direction. A question comes in to `POST /v1/search`. Retrieval is *not*
"embed the question and grab the top 8." It's a small pipeline whose job is to behave
like a thoughtful librarian: understand the question, look in the right place, cast a
wide net, then judge what actually answers it.

```
   a question arrives
        │
   (1)  PLAN  ── fast LLM: rewrite to keywords, resolve "yesterday" → a real date,
        │        detect source intent ("sites I visited" → browser), flag "list…"
        │
   (2)  FILTER  ── turn the plan into a Qdrant filter, + the namespace tag
        │
   (3)  EMBED the (rewritten) query  ── optionally also the raw question (multi-query)
        │
   (4)  SEARCH in parallel:  chunks (document layer)  ‖  facts (memory layer)
        │     · facts search excludes superseded + expired
        │     · if a filtered search finds nothing → one wider retry, still in-namespace
        │
   (5)  GROUP chunk hits by document  → RERANK with the cross-encoder + title boost
        │
        ▼
   return { documents: [...ranked, with chunks], memories: [...current facts] }
```

### Step 1 — Plan the query (a fast, cheap LLM job)

A small fast model turns the user's natural question into a **search plan**:

- **Rewrite** the question into keyword-rich search text (embeddings match keywords
  and topics, not conversational phrasing).
- **Resolve relative dates.** "Yesterday" means nothing to an embedding. The model
  emits *calendar* dates (`2026-06-14`); the engine does the date→timestamp
  arithmetic in Rust, because small models reliably emit dates but reliably botch
  epoch math.
- **Detect source intent.** "Websites I visited" → restrict to `browser`; "the PDF I
  read" → `file`; "in our standup" → `meeting`. Only when the question clearly targets
  a source.
- **Flag list questions.** "List everything I…" → widen retrieval to return many
  documents instead of a similarity top-K.

If the planner fails or isn't configured, the raw question is searched as-is. The
plan only ever *helps*; it never blocks.

> **Under the hood.** `engine/rewrite.rs::plan`, temperature 0.0, returns a
> `SearchPlan { query, source, after, before, listy }`. Date strings are converted to
> unix bounds in Rust (`date_to_unix`), inclusive on both ends. Recent conversation
> turns can be passed in so follow-ups ("what's *it* about?") resolve their
> references to the real topic before searching.

### Step 2 — Build the filter (this is the privacy wall)

The plan's source/time constraints become a Qdrant payload filter, and then the
**namespace tag is added on top of every single search path** — the main chunk
search, the facts search, the multi-query union, *and* the wrong-source retry. There
is no code path that searches across tenants. This is what makes multi-tenant
isolation a guarantee rather than a hope.

> **Under the hood.** `tagged_filter` wraps every search filter with a
> `container_tag` constraint; an explicit tag becomes a hard `must` match. The
> default namespace also matches *legacy* points that have no tag field yet, so
> pre-namespace data keeps working. There are integration tests
> (`container_tags_isolate_namespaces`) that ingest deliberately conflicting facts
> into two tenants and assert neither can ever see the other's — *including* their
> profiles.

### Step 3–4 — Embed, then search both layers in parallel

The rewritten query is embedded once. **Multi-query** recall: when the planner's
rewrite differs from the user's wording, the engine *also* embeds the raw question
and unions the two candidate pools — so a document only the original phrasing would
surface still reaches the reranker. Both query vectors are embedded in one batch and
both searches run concurrently with the facts search, so the recall boost costs
almost no extra latency.

Then chunks and facts are searched **at the same time**:

- **Chunks** (document layer): a wide net — up to 60 hits (150 for list questions) —
  because a single multi-chunk document would otherwise crowd distinct files out of
  the candidate pool before the reranker ever sees them.
- **Facts** (memory layer): the top ~10 current facts, with a filter that **excludes
  superseded and expired memories**. This is the temporal correctness guarantee in
  action: the Adidas fact, flagged `is_latest = false` back at write time, simply
  cannot appear here. Neither can a deadline whose `valid_until` has passed.

If a *filtered* search comes back empty (the planner guessed the wrong source or time
window), there's one **fallback retry**: drop the source/time constraints, keep the
namespace tag, and search again — but at a *much higher* score bar, so loosely
related junk can't masquerade as an answer just because the precise query found
nothing.

> **Under the hood.** `retrieve_for_plan_tagged` in `engine/mod.rs`.
> `CHUNK_THRESHOLD = 0.30`, `FACT_THRESHOLD = 0.30`, `FALLBACK_THRESHOLD = 0.45`.
> The parallel searches are a `tokio::join!`. `active_facts_filter` adds the
> `is_latest != false` and `valid_until ≥ now` exclusions.

### Step 5 — Group, then rerank (the precision gate)

Dense vector search is good at *recall* (finding plausible candidates) but mediocre
at *precision* (ordering them by what truly answers the question). So UltraMem
separates the two:

1. **Group** the chunk hits by their document (and collapse a URL captured more than
   once into its best hit), preserving best-hit order.
2. **Rerank** the candidate documents with a **cross-encoder** — a model that reads
   the question and each document *together* and scores genuine relevance, not just
   vector nearness. Candidates below the relevance floor are *dropped*, not kept as
   filler.
3. **Title boost.** A small bonus when the query's words appear in a document's
   title, so someone asking for "the RAAS tenant setup guide" gets the guide itself,
   not a browser capture that's merely topically similar to it.

The reranked, truncated list of documents (each with its matching chunks and citation
metadata) plus the current facts are returned together.

> **Under the hood.** `group_chunk_hits`, then `reranker.rerank`.
> `RERANK_THRESHOLD = 0.15`, `TITLE_BOOST = 0.4` (modest — it breaks ties toward
> exact-name matches without overriding strong semantic relevance). If the reranker
> call fails, the engine keeps the dense order rather than dropping results.

---

## Part 5 — The standing profile: memory you don't have to ask for

Searching every turn is wasteful when there are things an agent should simply *always
know* about a user. The **profile** is that always-known context, compiled and cached.

It has two sections:

- **Static** — durable facts that are basically always true: who they are, their
  role, the projects and products they work on, the people around them, standing
  preferences. (Compiled by the LLM from the latest memories, with one-off events and
  dated items dropped.)
- **Dynamic** — what they've been doing **lately**: the last ~7 days of activity,
  current threads, recent decisions.

An agent prepends this block to its system prompt and **starts every session already
knowing who it's talking to — with zero retrieval round-trip.** It's compiled by an
LLM pass over the memory graph and cached for an hour per namespace, so it costs
nothing at question time.

> **Under the hood.** `engine/profile.rs::compile`, cached in `MemoryEngine` with a
> 3,600-second TTL per tag (`profile_tagged`, `refresh_profile`). It scrolls a sample
> of the latest facts, splits durable vs. recent (7-day window), and runs two small
> LLM passes (temperature 0.2). `Profile::as_prompt_block()` renders it ready to
> paste into a system prompt; it returns an empty string when there's nothing to say,
> so callers can prepend it unconditionally. Profiles are strictly per-namespace.

---

## Part 6 — The details that make it trustworthy

These aren't headline features; they're the engineering that makes the headline
features hold up in production.

**Namespaces (`container_tag`) — hard multi-tenant isolation.** Every point (chunk
and fact) carries a namespace tag. Pass one tag per user, or per agent, and their
memory pools are *hard*-isolated at the database-filter level on every read and
write — including reconciliation (a new fact only ever supersedes a memory in its
own pool) and profiles. Verified by tests that try to leak and fail.

**Temporal correctness — two independent mechanisms.** `is_latest` handles
*contradiction* (UPDATE supersedes the old fact); `valid_until` handles *expiry*
(time-bound facts lapse on their date). Both are simple payload filters at query
time, so they're cheap and always applied. Legacy facts that have neither field are
treated as current and never-expiring, so nothing pre-existing breaks.

**Hybrid (dense + sparse) search.** Dense embeddings blur rare exact tokens — error
codes, IDs, proper nouns. A BM25-style **sparse** vector restores that lexical
channel; Qdrant fuses the two server-side with Reciprocal Rank Fusion, so each
catches what the other misses. It's behind a flag (it needs a hybrid-schema
collection) and off by default, but the machinery is built and benchmarked.

**Re-index without re-extraction.** The indexed chunk text *is* the source of truth —
there's no separate document store. So you can rebuild a document's facts (e.g. after
improving the distillation prompt) by **reconstructing its text from the stored
chunks** and re-distilling — never reopening the original file, never re-running OCR.
Cheap and offline.

**Graceful degradation, everywhere.** Distillation is non-fatal to ingest. A failed
classification falls back to NEW. A failed reranker keeps dense order. A failed
planner searches the raw query. A failed merge falls back to local dedup. The system
is built so that *no single LLM hiccup can lose data or blank an answer.*

**Measured, not asserted.** The repo ships its own eval harness. The memory
capability suite (`memtest`) checks single-fact recall, cross-document synthesis,
and — the one RAG fails — the **knowledge update**, and passes 3/3 live. A separate
retrieval benchmark scores ranking quality (H@k / MRR / MemScore) against a frozen
golden set. Numbers you can reproduce against your own Qdrant, not screenshots.

> **Under the hood.** `engine/sparse.rs` (term-frequency vectors, Qdrant applies IDF),
> `reconstruct_doc_text` / `reindex_doc_facts` (re-index path), `examples/probe.rs`
> (`memtest` / `bench`), and the `pipeline_tests` module in `engine/mod.rs`.

---

## Part 7 — Exactly what the LLM does (and, just as important, what it doesn't)

You asked for this specifically, so here it is in one place. UltraMem uses LLMs
**surgically** — only where language understanding or judgment is genuinely required —
and never lets them touch the parts that should be deterministic. There are **six**
distinct LLM jobs inside the engine, plus the answer model in the consuming app:

| # | Job | When | Model role | Why an LLM (and not code) |
|---|---|---|---|---|
| 1 | **Distill** — document → atomic, standalone facts | ingest | distill | only a language model can read prose and decide what's *worth remembering* and phrase it to stand alone |
| 2 | **Merge** — dedup facts across a document's segments | ingest | distill | near-duplicate detection that respects meaning, not string equality |
| 3 | **Reconcile** — classify NEW/DUPLICATE/UPDATE/EXTEND | ingest | distill | judging whether "Puma" *contradicts* "Adidas" is a semantic call; this is the step that makes it memory |
| 4 | **Context blurb** (optional, off by default) — one-line doc situator | ingest | distill | summarise a document in a sentence |
| 5 | **Plan** — question → search plan, resolve dates, detect intent | query | plan (fast) | parse messy human phrasing and intent |
| 6 | **Profile** — latest facts → static + dynamic summary | cached | distill | compress many facts into a readable standing brief |
| (7) | **Answer** — read documents+memories+profile, respond | in your app | answer | the actual conversation; UltraMem supports it (streaming + tool-calling) but doesn't impose it |

What the LLM **deliberately does *not* do:**

- It does **not** embed text — that's the embedder.
- It does **not** do vector search or ranking math — that's Qdrant + the cross-encoder.
- It does **not** do date arithmetic — the model emits calendar dates; **Rust** turns
  them into timestamps, because that's where small models are unreliable.
- It does **not** store anything or decide isolation — those are deterministic
  database operations gated by the namespace filter.
- It is **never** on the critical path for data safety — every LLM step has a
  non-LLM fallback.

That division is the whole philosophy: **LLMs for judgment, deterministic code for
correctness.** The model decides *what* a fact is and *whether* it's changed; the
engine decides, with certainty, *what gets stored and who can see it.*

---

## Part 8 — Why it works (the intuition, distilled)

If you remember five things about *why* this behaves like memory:

1. **Facts, not chunks, are the unit of memory.** A self-contained fact is something
   you can compare, update, and expire. A chunk is just text near other text.

2. **Contradictions are resolved at write time, once.** The expensive, ambiguous
   "which of these is true now?" question is answered when a fact arrives — not
   re-litigated on every read. The reader gets a single current truth for free.

3. **Time is first-class.** `is_latest` and `valid_until` mean the system can
   *forget* — superseded and lapsed facts vanish from results automatically. Memory
   that can't forget isn't memory; it's a landfill.

4. **Recall wide, judge narrow.** Dense search (and sparse, and multi-query) cast a
   generous net; the cross-encoder reranker is the precision gate that decides what
   actually answers the question. Two stages, each doing what it's good at.

5. **The LLM is a scalpel, not a hammer.** Used only for judgment, batched to one
   call per document, short-circuited whenever a fact is obviously new, and always
   backed by a deterministic fallback. That's what keeps it fast, cheap, and safe.

---

## Part 9 — How Re-Kalei builds on UltraMem

UltraMem is the engine; **Re-Kalei is what you build with it.** The engine is
deliberately headless — it exposes an HTTP API and (separately) an MCP server, and
takes a `container_tag` on every call. Everything below is the integration surface
Re-Kalei plugs into. (Note: in the codebase the engine still carries its origin name,
*Recally* — Re-Kalei is the product consuming it.)

### The two integration doors

1. **The HTTP API** — for your app's own backend.

   | Endpoint | Use |
   |---|---|
   | `POST /v1/memories` | ingest anything: a message, a file, a meeting transcript, an agent's note |
   | `POST /v1/search` | get back `documents` (grounding) + `memories` (current facts) |
   | `GET /v1/profile` | the static + dynamic standing profile, ready to inject |
   | `GET /v1/timeline` | newest-first enumeration ("what did I do this week") |
   | `POST /v1/reindex` | backfill tags / rebuild facts from stored text |
   | `GET /v1/health` | readiness |

   Auth is a single Bearer key; the `container_tag` in each request scopes the
   namespace. (The key is what gives an app access; the *tag* is what isolates one
   user from another inside it.)

2. **The MCP server** (`ultramem-mcp`, separate repo) — for agents. It exposes the
   memory layer to any MCP client (Claude Code/Desktop, Cursor, your own agent
   runtime) as four tools: `recall_search`, `recall_timeline`, `add_memory`,
   `get_profile`. Add one server and an agent gains durable, cross-session memory.

### The core pattern: one tag per identity

Re-Kalei's leverage comes from how it assigns `container_tag`:

- **`container_tag = user_id`** → every user has a private, isolated memory pool. The
  same deployment serves all of them with a guaranteed wall between them.
- **`container_tag = agent_id`** (or `user_id:agent_id`) → give an *agent* its own
  memory: what it has learned, decided, or been told, persistent across sessions and
  separate from the user's.

### The agent loop Re-Kalei can ship today

```
   session start →  get_profile(tag)  →  prepend to the agent's system prompt
                    "the agent already knows who it's talking to"
        │
   during the task → recall_search(tag, question)
                    → ground answers in the user's own documents + current facts
        │
   after the task →  add_memory(tag, outcome / what the user said)
                    → the agent writes its own episodic memory back,
                      through the same distill → reconcile lifecycle
        │
   over time → the profile recompiles; superseded facts drop out;
               the agent gets steadily more useful without anyone curating it
```

Because anything written back flows through the *same* lifecycle, an agent that notes
"the user now prefers async standups" will **automatically supersede** an earlier
"prefers daily standups" fact — no special handling. The memory curates itself.

### Concrete use cases this unlocks

- **A personal assistant that actually remembers** across days and devices: it knows
  your projects, the people you work with, your standing preferences, and what you
  did this week — on session start, with no retrieval lag, via the profile.
- **Per-customer support / sales agents:** one namespace per customer account; the
  agent recalls every prior interaction and the *current* state of that account, with
  contradictions already reconciled (the customer "moved from the Starter to the Pro
  plan" supersedes the old plan fact).
- **Meeting memory:** drop a transcript in (it chunks by speaker, distills decisions
  and owners into facts); later ask "what did we decide about the launch?" and get the
  current decision, not every version ever discussed.
- **Document Q&A with a memory of *you*:** the `documents` layer grounds answers in
  the exact source passages (with citations); the `memories` layer + profile make the
  agent answer *as if it knows you*, not as a stateless search box.
- **Multi-agent systems:** each agent gets its own tag and its own evolving memory,
  while sharing a user's namespace for common context — clean separation, no leakage.
- **Self-hostable and provider-agnostic:** because every provider is swappable, Re-Kalei
  can run fully local (Ollama + Qdrant), on Groq for speed, or on OpenAI/Anthropic for
  quality, and change that decision without touching product code.

### The possibilities to lean into next

- Ship the profile-injection pattern as the *default* for every Re-Kalei agent — it's
  the highest-leverage, lowest-cost win (one cached call, instant "it knows me").
- Let agents write back liberally (`add_memory`) — the reconciliation lifecycle means
  more writes make memory *better*, not noisier, because duplicates and contradictions
  are absorbed automatically.
- Use `container_tag` creatively: per-project pools, per-team pools, ephemeral
  session pools — the isolation is free and total.
- Turn on hybrid search for namespaces heavy with codes/IDs/proper nouns; turn on web
  body fetching only where the privacy trade-off is explicitly acceptable.

---

## Appendix — Reading the source

| Concern | File |
|---|---|
| Orchestration: ingest, retrieve, profile cache, namespaces, reindex | `crates/ultramem-core/src/engine/mod.rs` |
| Fact distillation (segment → extract → merge) | `engine/distill.rs` |
| Memory lifecycle (reconcile, relations, expiry parsing) | `engine/memory.rs` |
| Query planning (rewrite, dates, source/list intent) | `engine/rewrite.rs` |
| Standing profile (static + dynamic compile) | `engine/profile.rs` |
| Content-aware chunking (markdown / transcript / paragraph) | `engine/chunker.rs` |
| Contextual Retrieval blurb (optional) | `engine/context.rs` |
| Hybrid sparse vectors | `engine/sparse.rs` |
| Text extraction (Jina Reader, local fallback) | `engine/extract.rs` |
| Provider-agnostic LLM client (OpenAI-shape + Anthropic) | `llm.rs` |
| Provider traits + implementations (embed/rerank/OCR/store) | `providers/` |
| HTTP API | `crates/ultramem-server/src/main.rs` |
| MCP design | `docs/MCP.md` |
| Memory-vs-RAG explainer | `docs/memory-vs-rag.md` |
| Reproducible benchmarks | `docs/benchmarks.md` |

*Every threshold, prompt, and flow in this document is taken directly from the source
as of this writing. When in doubt, the code in the table above is the ground truth.*
