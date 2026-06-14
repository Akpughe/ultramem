# Contributing to UltraMem

Thanks for your interest in UltraMem — an open-source memory engine for AI agents. Contributions of all kinds are welcome: bug reports, docs, new providers, and features.

## Project layout

```
crates/
  ultramem-core/     # the engine (library): chunk → embed → distill → reconcile → retrieve
    src/engine/      # pipeline + Qdrant/Jina/Mistral low-level clients
    src/providers/   # the swappable trait seams (Embedder/Reranker/Ocr/Llm/VectorStore)
    src/llm.rs       # multi-provider LLM client (OpenAI-compatible + Anthropic)
    examples/        # eval harness: probe (memtest/bench/abtest/reindex), bench_ingest
  ultramem-server/   # axum HTTP API over the engine (docs/API.md)
docs/                # design, API, MCP, roadmap, benchmarks
```

`ultramem-mcp` (the MCP server) lives in its own repository — see [`docs/MCP.md`](docs/MCP.md).

## Development setup

1. Install a recent stable Rust toolchain (`rustup`).
2. `cp .env.example .env` and fill in `QDRANT_URL`, `JINA_API_KEY`, `GROQ_API_KEY` (and `MISTRAL_API_KEY` for OCR). For a fully local stack: `docker compose up`.
3. Build and test:

   ```bash
   cargo build --workspace
   cargo test -p ultramem-core --lib        # 61 offline unit tests
   ```

### Live tests and the eval harness

Some tests and all benchmarks hit real provider APIs + Qdrant. They're opt-in:

```bash
# live pipeline integration tests (contradiction + multi-tenant isolation)
ULTRAMEM_PIPELINE_TESTS=1 cargo test -p ultramem-core --lib pipeline_tests -- --test-threads=1

# memory capability suite (the headline "memory, not RAG" proof)
cargo run -p ultramem-core --example probe -- memtest
```

`probe` also has `bench` (frozen golden-set H@k/MRR/MemScore), `abtest` (ingest-side A/B), and `reindex`. See [`docs/benchmarks.md`](docs/benchmarks.md).

## Ground rules

- **Keep `cargo build` + tests green.** Run `cargo test -p ultramem-core --lib` before opening a PR; if you touch the live pipeline, run `memtest` too.
- **Match the surrounding style.** `cargo fmt` and `cargo clippy` should be clean.
- **Don't invent provider APIs.** If you add or change an integration, link the vendor docs in the PR.
- **Keep the engine provider-agnostic.** New providers go behind the existing traits in `src/providers/` — see below. The engine must never name a vendor directly.

## Adding a provider

The engine talks to five capabilities through traits in `crates/ultramem-core/src/providers/`: `Embedder`, `Reranker`, `Ocr`, `Llm`, and `VectorStore`. To add one (e.g. a Cohere embedder):

1. Implement the trait in a new file under `src/providers/` (e.g. `cohere.rs`).
2. Re-export it from `providers/mod.rs` and `lib.rs`.
3. Wire selection into `MemoryEngine::new` (config-driven) and/or rely on the `with_*` injector. An embedder must report an accurate `dim()` — collections are sized from it.
4. Add a unit test proving config selection (see `embedder_is_config_selectable`).

## Submitting changes

1. Fork and branch from `main`.
2. Make focused commits with clear messages.
3. Open a PR describing the change and how you verified it (test output welcome).

By contributing, you agree that your contributions are licensed under the project's [Apache-2.0](LICENSE) license.
