# UltraMem Cloud Loop Feedback + Exact Execution Instructions

Use this file as the single handoff to Fable 5, Claude Code, Codex Cloud, or any
implementation agent. It gives feedback on the current execution plan, asks for a
tighter execution plan, defines how to evaluate that plan, and then tells the
agent exactly what to execute first.

The goal is to keep the loop disciplined. Each cloud run should improve the plan,
evaluate it, execute the next safe slice, verify it, and leave clear evidence for
the next run.

## Context

Read these files first:

- `docs/FABLE5_DEEP_MEMORY_AUDIT.md`
- `docs/FABLE5_EXECUTION_PLAN.md`
- `docs/FABLE5_SOTA_10_EXECUTION_PROMPT.md`
- `docs/API.md`
- `crates/ultramem-server/src/main.rs`
- `crates/ultramem-core/src/engine/mod.rs`
- `crates/ultramem-core/src/engine/memory.rs`
- `crates/ultramem-core/src/engine/distill.rs`
- `crates/ultramem-core/src/providers/mod.rs`
- `crates/ultramem-core/examples/probe.rs`

Current audit verdict:

- UltraMem is about **6/10** as a promising early-stage OSS memory engine.
- UltraMem is about **3.5/10** as a world-class company-brain memory layer.
- The retrieval foundation is strong.
- The memory layer is real but shallow.
- The server is not safe for hosted/multi-user deployment until P0 safety issues
  are fixed.

The strategic order is:

1. Safe
2. Correct
3. Measurable
4. Typed and provenance-grounded
5. Temporal
6. Scoped and permission-aware
7. Relationship-rich
8. Usable as a product
9. Scalable
10. Frontier-quality retrieval/reasoning

Do not skip steps.

## Feedback on the Current Execution Plan

The plan direction is correct. It properly refuses to chase graph or agentic
retrieval until the system is safe, testable, and truthful.

The main problem: **Sprint 1 is too large.**

It currently mixes:

- mock store and mock LLM;
- CI;
- auth and tenant binding;
- scoped delete;
- network `file_path` removal;
- secret/PII screening;
- prompt-injection hardening;
- transactional supersession;
- top-k reconcile;
- cascade delete;
- docs cleanup;
- benchmark metric fixes.

That is not one coherent first sprint. It should be split.

Recommended split:

- **Sprint 1A: Stop-ship safety + offline correctness foundation.**
- **Sprint 1B: redaction, prompt-injection hardening, transactional supersession,
  and cascade forgetting.**
- **Sprint 1C: benchmark repair, docs parity, and reproducible evaluation.**

Do **Sprint 1A first**. Do not implement Postgres, typed memories, scope
hierarchies, connector systems, temporal graph production wiring, product UI, or
agentic retrieval in Sprint 1A.

## Required Revised Execution Plan

Before touching code, create a short revised plan in your working notes. It does
not need to be a huge new document. It must answer:

1. What exact Sprint 1A tasks will you execute?
2. Which files will you change?
3. Which tests will prove each task?
4. Which risks are explicitly out of scope?
5. What commands will you run before saying done?

Then evaluate your own revised plan using the rubric below.

## Plan Evaluation Rubric

Score the revised plan out of 10 before executing.

### Minimum acceptable score: 8/10

If the plan scores below 8/10, revise it before touching code.

Score dimensions:

1. **Safety focus, 2 points**
   - 2: directly fixes P0 hosted/multi-user risks.
   - 1: touches safety but mixes in distracting future work.
   - 0: focuses on graph/retrieval/product polish first.

2. **Testability, 2 points**
   - 2: new behavior has offline tests that do not require live Qdrant/API keys.
   - 1: tests exist but depend on live services.
   - 0: implementation without meaningful tests.

3. **Scope discipline, 2 points**
   - 2: Sprint 1A only; no Postgres, typed-record refactor, graph, connectors, or UI.
   - 1: mostly scoped but with some unnecessary refactor.
   - 0: broad architecture expansion.

4. **Acceptance clarity, 2 points**
   - 2: every task has concrete pass/fail criteria.
   - 1: some tasks have fuzzy outcomes.
   - 0: no clear done state.

5. **Continuity for next loop, 2 points**
   - 2: leaves notes on what changed, tests run, and next recommended sprint.
   - 1: partial handoff.
   - 0: no handoff.

## Sprint 1A: Execute This First

### Non-negotiable constraints

- Do not introduce Postgres.
- Do not redesign the memory schema.
- Do not turn on the temporal graph.
- Do not add connectors.
- Do not create product UI.
- Do not build agentic retrieval.
- Do not do broad refactors.
- Keep edits small and directly tied to the Sprint 1A tasks.

### Sprint 1A Tasks

Execute these in order.

### Task 1: Remove network `file_path` ingest

Problem:

The HTTP API accepts `file_path` in JSON and reads that path from the server
filesystem. This is an arbitrary server-file read risk.

Required behavior:

- JSON ingest must reject `file_path`.
- Multipart upload must still work.
- Local file ingestion may remain available through the Rust engine API, not
  through the network API.

Likely files:

- `crates/ultramem-server/src/main.rs`
- `docs/API.md`

Acceptance tests:

- A JSON `POST /v1/memories` body containing `file_path` returns `400`.
- A normal JSON `content` ingest still reaches the engine path.
- Multipart upload behavior is not removed.

Done means:

- No network request can make UltraMem read an arbitrary server path.

### Task 2: Make delete scope-aware

Problem:

`DELETE /v1/memories/:id` currently deletes by document id without enforcing the
requester's namespace/scope.

Required behavior:

- Delete must be constrained to the caller's allowed tag/scope.
- Cross-tenant delete must fail with `404` or `403`.
- Data outside the caller's tag must remain intact.

Likely files:

- `crates/ultramem-server/src/main.rs`
- `crates/ultramem-core/src/engine/mod.rs`
- `crates/ultramem-core/src/engine/qdrant.rs`
- `crates/ultramem-core/src/providers/qdrant_store.rs`
- `crates/ultramem-core/src/providers/mod.rs`

Acceptance tests:

- Tenant A cannot delete Tenant B's document id.
- Delete inside the caller's tag succeeds.
- Delete outside the caller's tag leaves matching points intact.

Done means:

- Delete is no longer a cross-tenant destruction primitive.

### Task 3: Add key-to-tag binding

Problem:

The server uses one static API key and trusts client-supplied `container_tag`.
That means any key-holder can read/write another tenant by changing a string.

Required behavior:

- Auth must resolve a credential into allowed tag(s).
- A request may omit `container_tag`, in which case the default allowed tag is
  used.
- A request may include `container_tag` only if it is allowed for the credential.
- A request for a disallowed tag must return `403`.
- Empty `ULTRAMEM_API_KEY` must be rejected unless `ULTRAMEM_DEV=1`.

Minimal acceptable design for Sprint 1A:

- Keep the existing simple API-key model, but support a config mapping from API
  key to allowed tags.
- Preserve backward compatibility for a single-key local deployment by mapping
  `ULTRAMEM_API_KEY` to `default`.
- Add a clear seam so a later sprint can replace this with JWT claims or a
  database-backed tenant table.

Likely files:

- `crates/ultramem-server/src/main.rs`
- possibly new server module under `crates/ultramem-server/src/`
- `docs/API.md`

Acceptance tests:

- Key A + tag A succeeds.
- Key A + tag B returns `403`.
- Missing key returns `401` when auth is enabled.
- Empty key without `ULTRAMEM_DEV=1` refuses to start or fails configuration.
- Dev mode is explicit and documented.

Done means:

- No endpoint trusts `container_tag` without validating it against the
  credential.

### Task 4: Add supersession absence assertions

Problem:

The memory tests can pass when the new value appears, even if the old superseded
value also appears.

Required behavior:

- Contradiction tests must assert that superseded values are absent.
- The Puma/Adidas scenario must require Puma present and Adidas absent.

Likely files:

- `crates/ultramem-core/examples/probe.rs`
- `crates/ultramem-core/src/engine/mod.rs`
- `crates/ultramem-core/src/engine/memory.rs`

Acceptance tests:

- A test fails if both old and new values appear as active memories.
- `must_absent` is populated for contradiction scenarios.

Done means:

- "Only the current fact is served" is tested as absence, not only presence.

### Task 5: Add offline safety/correctness tests

Problem:

Important behavior currently depends on live Qdrant and provider keys, so it is
not reliably tested in normal `cargo test`.

Required behavior:

- Add enough mock-backed or handler-level tests to prove Sprint 1A behavior
  offline.
- Do not require live Qdrant, Jina, Groq, Mistral, or OpenAI keys for these tests.

Minimum tests:

- `file_path` JSON rejection.
- tenant escalation denied.
- cross-tenant delete denied.
- delete inside allowed tag accepted.
- superseded value absent in contradiction logic.

Likely files:

- `crates/ultramem-server/src/main.rs`
- `crates/ultramem-core/src/providers/mod.rs`
- test modules near changed code

Acceptance tests:

- `cargo test` passes without live service credentials.

Done means:

- Sprint 1A safety claims are enforceable in CI.

### Task 6: Add CI

Required workflow:

- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test`

Likely files:

- `.github/workflows/ci.yml`

Acceptance tests:

- Workflow exists and uses stable Rust.
- Commands match local verification expectations.

Done means:

- Every PR can catch format, lint, and offline test failures.

### Task 7: Align docs with code

Problem:

Docs currently overclaim some API capabilities.

Required behavior:

- Remove or mark planned any endpoints/params the code does not support yet.
- Especially check:
  - `/v1/jobs/:id`
  - SSE job stream
  - search filters not implemented by `SearchBody`
  - `file_path` network ingest after Task 1
  - auth/tag semantics after Task 3

Likely files:

- `docs/API.md`

Acceptance tests:

- Every documented current endpoint exists.
- Planned endpoints are clearly labeled planned, not current.

Done means:

- Docs no longer ask users to rely on behavior the code does not honor.

## Verification Commands

Run these before final handoff:

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

If `cargo clippy` is unavailable or fails because of pre-existing unrelated
warnings, report that clearly and still run `cargo test`.

Do not claim live-system verification unless you actually ran the live Qdrant/API
key gated tests.

## Sprint 1A Final Handoff Format

When Sprint 1A is complete, report in this exact format:

```text
Sprint 1A Result: PASS | PARTIAL | BLOCKED

Changed files:
- ...

Completed tasks:
- Task 1: PASS/PARTIAL/BLOCKED — evidence
- Task 2: PASS/PARTIAL/BLOCKED — evidence
- Task 3: PASS/PARTIAL/BLOCKED — evidence
- Task 4: PASS/PARTIAL/BLOCKED — evidence
- Task 5: PASS/PARTIAL/BLOCKED — evidence
- Task 6: PASS/PARTIAL/BLOCKED — evidence
- Task 7: PASS/PARTIAL/BLOCKED — evidence

Verification:
- cargo fmt --check: PASS/FAIL/NOT RUN
- cargo clippy -- -D warnings: PASS/FAIL/NOT RUN
- cargo test: PASS/FAIL/NOT RUN

Known remaining risks:
- ...

Recommended next sprint:
- Sprint 1B: ...
```

## Sprint 1B: Do Not Start Until Sprint 1A Is Done

Sprint 1B should include:

1. Secret/PII screening before embedding.
2. Prompt-injection hardening for distill/profile/graph prompts.
3. Transactional supersession or dead-letter behavior when `is_latest` flip
   fails.
4. Top-k reconcile with confidence and `NeedsReview`.
5. Cascade delete into graph/profile/cache.
6. `forget_is_total` tests.
7. Benchmark metric repair if it was not completed in Sprint 1A.

Do not start Sprint 1B until Sprint 1A has a clean handoff.

## Sprint 1C: Evaluation and Reproducibility

Sprint 1C should include:

1. Fix `gold_retrieved` to all-not-any and gold-chunk matching.
2. Commit deterministic golden seeds or fixtures.
3. Separate easy smoke-test corpora from hard benchmark corpora.
4. Add hard retrieval distractor data.
5. Add contradiction-chain evaluation.
6. Add permission-leak evaluation.
7. Add prompt-injection evaluation.
8. Document which claims are proven by unit tests, live integration tests, or
   benchmarks.

## Longer-Term Rule

Do not move to Postgres, typed memory records, company scopes, temporal graph
production wiring, connectors, review UI, or agentic retrieval until:

- Sprint 1A safety is done.
- Sprint 1B forgetting/redaction/injection/correctness is done.
- Sprint 1C measurement credibility is done.

The path to SOTA is not "add more clever retrieval." The path is:

1. enforce boundaries;
2. prove memory correctness;
3. make facts typed and cited;
4. make forgetting real;
5. make scopes and permissions first-class;
6. then add temporal and relationship intelligence;
7. only then chase frontier retrieval.

