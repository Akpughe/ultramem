# Memory vs RAG

UltraMem does everything a good RAG system does — and then the thing RAG structurally *can't*. This page explains the difference, because it's the whole reason the project exists.

## RAG is a document layer

Retrieval-Augmented Generation is, mechanically:

```
chunk documents → embed chunks → at query time, embed the query → vector search → stuff top-k into the prompt
```

It's stateless and content-addressed. Ask it "what do I know about X?" and it returns the chunks most similar to X. That's genuinely useful — it's how you ground a model in a corpus it wasn't trained on. UltraMem has this layer and takes it seriously: content-type-aware chunking, a cross-encoder reranker, hybrid dense+sparse retrieval, and a query planner.

But a document layer has no opinion about *you*. Every chunk it ever ingested is equally "true." It cannot tell you that something you believed last year is no longer the case, because it has no notion of facts, time, or identity — only text and cosine distance.

## Memory is a layer on top

A memory layer asks a different question: **"what do I remember about you?"** To answer it, UltraMem adds a second pass over every ingested document:

```
distill the document into atomic facts (one batched LLM call)
  → embed each fact
  → find the nearest existing memories
  → classify each new fact against them: UPDATE / EXTEND / DUPLICATE / NEW
  → write the result: supersede old facts (flip is_latest=false), add edges, or insert
```

The output isn't chunks — it's a small, reconciled set of durable facts with temporal state. That unlocks three things RAG can't do:

| | RAG (documents) | UltraMem memory layer |
|---|---|---|
| **Unit** | text chunk | distilled fact |
| **State** | stateless; every chunk is permanent and equal | temporal; facts have `is_latest` and `valid_until` |
| **Identity** | none | per-namespace (`container_tag`) — one pool per user/agent |
| **Knowledge update** | both old and new chunks returned forever | old fact superseded; only the current one is served |
| **Standing context** | none | a compiled profile ("what's always true about you") |

## The knowledge-update test

This is the canonical example, and UltraMem's `memtest` harness checks it on every run:

1. Ingest: *"The user has worn Adidas running shoes for years and recommends them to everyone."*
2. Later ingest: *"The user has switched entirely to Puma; Puma is now their preferred brand."*
3. Ask: *"What running shoe brand does the user prefer now?"*

- **Plain RAG** returns both passages — Adidas and Puma are both "in the corpus," equally similar to the query. The model has to guess which is current, or hedges.
- **UltraMem** distilled "prefers Adidas" and "prefers Puma" as facts, recognized the second as an UPDATE of the first, flipped the Adidas fact's `is_latest` to `false`, and serves **only Puma**. The contradiction was reconciled at write time, not punted to the reader.

(Measured: see [`benchmarks.md`](benchmarks.md). The contradiction scenario passes live, with the superseded Adidas fact still stored but filtered out of results.)

## The standing profile

Because the memory layer knows facts and identity, it can compile a **profile** — a short, cached, always-true summary of a namespace (static facts + recent activity). Inject it into an agent's system prompt every turn and the agent starts each session already knowing who it's talking to, without a retrieval round-trip. This is the "always-known context" trick, self-hostable.

## When to use which

You don't choose — UltraMem runs both layers in parallel and `search` returns both:

- `documents` — the RAG layer: ranked chunks for "find me the thing that says X."
- `memories` — the memory layer: current distilled facts for "what's true about this user/agent."

Use the document hits to ground answers in specifics; use the memories (and the profile) to give the agent durable, self-updating context. RAG retrieves text. UltraMem remembers.
