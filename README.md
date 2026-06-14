# UltraMem

**An open-source memory engine for AI agents — "memory, not RAG."**

UltraMem is a self-contained memory layer any app or agent can plug into over an HTTP API or MCP. It does what a good RAG system does (chunk → embed → retrieve raw content) *and* the thing RAG can't: it extracts durable facts about the user, reconciles them over time (dedup / supersede / extend), and serves a standing profile — so an agent that uses it actually *remembers* across sessions and conversations.

It is the memory engine built inside Recally, extracted to stand on its own. The goal: an open, provider-agnostic, well-documented alternative to SuperMemory that people can run themselves and build on.

> **Status: verified against live Qdrant; providers are swappable.** The engine is extracted and decoupled (`crates/ultramem-core`, 61 unit tests + live integration tests pass), the HTTP API runs and round-trips end-to-end (`crates/ultramem-server`), and embedder/reranker/OCR/LLM/vector-store are behind traits so no vendor is hardwired. The memory capability suite passes **3/3** live (`memtest`), including the knowledge-update case. What remains — the MCP server and publishing — is in [`KICKOFF.md`](KICKOFF.md). `docs/ROADMAP.md` has the phased plan; `docs/EXTRACTION.md` records exactly how the engine was lifted out.

## Quickstart

```bash
cp .env.example .env          # fill in QDRANT_URL / JINA_API_KEY / GROQ_API_KEY / ULTRAMEM_API_KEY
cargo run -p ultramem-server  # → http://localhost:8080

curl -sX POST localhost:8080/v1/memories -H "Authorization: Bearer $ULTRAMEM_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"content":"The user ships Rust daily and prefers Puma running shoes.","container_tag":"user_123"}'

curl -sX POST localhost:8080/v1/search -H "Authorization: Bearer $ULTRAMEM_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"query":"what shoes does the user like?","container_tag":"user_123"}'
```

Or `docker compose up` (server + Qdrant, fully local).

To swap the embedder, set `ULTRAMEM_EMBEDDER=openai` (+ `OPENAI_API_KEY`) — no code change. See [provider config](#provider-agnostic-self-hostable).

---

## Why it's different from plain RAG

> Full explainer: [`docs/memory-vs-rag.md`](docs/memory-vs-rag.md).

| | Documents layer (RAG) | Memory layer (the moat) |
|---|---|---|
| Stores | Raw content → semantic chunks + embeddings | LLM-distilled facts, reconciled over time |
| State | Stateless, same for everyone | Per-namespace, temporal, self-updating |
| Answers | "What do I know?" | "What do I remember about you?" |
| Mechanism | embed + vector search | distill → embed fact → nearest memories → classify **UPDATE / EXTEND / DUPLICATE / NEW** → write edges, flip `is_latest` |

Proven in Recally's `memtest` harness (recall, cross-document synthesis, and **knowledge update** — "switched Adidas → Puma" returns Puma and supersedes the old fact).

## What's already built (and measured) in the engine

- **Two-layer retrieval** — chunks + distilled facts, searched in parallel.
- **Memory lifecycle** — dedup / UPDATE (supersede via `is_latest`) / EXTEND / NEW, one batched LLM call per document.
- **Temporal correctness** — superseded and expired (`valid_until`) facts are filtered out of results.
- **Namespace isolation** — `container_tag` hard-isolates pools (one per user / per agent). Verified multi-tenant.
- **Standing profile** — static + dynamic, cached, for "always-known" agent context.
- **Retrieval planner** — date/source/list-intent rewriting; multi-query recall; cross-encoder rerank + lexical title boost.
- **Hybrid search** — dense + sparse (BM25/RRF) available behind a flag.
- **Content-type-aware chunking** — markdown by heading, transcripts by speaker turn.
- **Ingestion** — Jina Reader extraction, Mistral OCR (PDF + images), web body fetch.
- **Re-index without re-extraction** — reuses stored chunk text; never reopens files.
- **Eval harnesses** — frozen golden-set bench (H@k/MRR/latency/tokens/MemScore), A/B for ingest-side features, LongMemEval-style memory suite.

Full design + measured results: [`docs/`](docs/). Reproducible numbers and the eval harness: [`docs/benchmarks.md`](docs/benchmarks.md).

## Provider-agnostic, self-hostable

The engine talks to every external capability through a trait, so no vendor is baked in:

| Capability | Trait | Default | Swap to |
|---|---|---|---|
| Embeddings | `Embedder` | Jina (`jina-embeddings-v3`, 1024-d) | **OpenAI** (`text-embedding-3-*`) via `ULTRAMEM_EMBEDDER=openai`, or any custom impl |
| Reranking | `Reranker` | Jina reranker v2 | any custom impl |
| OCR | `Ocr` | Mistral OCR | any custom impl |
| LLM (plan/distill) | `Llm` | Groq | any OpenAI-compatible or Anthropic model via `EngineCfg::with_models` |
| Vector store | `VectorStore` | Qdrant | any custom impl |

Select via config/env, or inject a fully custom implementation with `MemoryEngine::with_embedder(...)` (and friends) — without touching engine code. See [`docs/ROADMAP.md`](docs/ROADMAP.md) Phase 3 and [`CONTRIBUTING.md`](CONTRIBUTING.md) for adding a provider.

## Shape of the project

Two repositories (per the plan):

- **`ultramem`** — Rust workspace:
  - `ultramem-core` — the engine (library crate). Provider-agnostic via traits.
  - `ultramem-server` — HTTP API (axum) wrapping the core. API-key auth, multi-tenant via `container_tag`.
- **`ultramem-mcp`** — MCP server exposing the memory tools to any MCP client (Claude, Cursor, etc.). Separate repo.

Consumers are language-agnostic: any stack talks to it over HTTP or MCP.

See [`docs/API.md`](docs/API.md) and [`docs/MCP.md`](docs/MCP.md).

## Contributing

Contributions welcome — see [`CONTRIBUTING.md`](CONTRIBUTING.md) (project layout, dev setup, running the live tests, and how to add a provider behind the existing traits).

## License

**Apache-2.0** — see [`LICENSE`](LICENSE). (The patent grant matters for infrastructure others build on.)
