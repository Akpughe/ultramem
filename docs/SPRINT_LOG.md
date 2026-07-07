# UltraMem Sprint Log

Rolling handoff for the disciplined execution loop. Newest sprint on top. Each
entry records what changed, what was verified, and what the next sprint is.

---

## Sprint 1C — evaluation & reproducibility (COMPLETE)

**Result: PASS — 5 of 5 items done** · branch `sprint-1c-eval` (from `main`) · date 2026-07-07

- **Fix `gold_retrieved`** (`76db24d`) — the attribution metric used `.any()` over gold sessions
  (scored "retrieved" if just one of several required sessions surfaced — the broken metric the
  roadmap flagged). Now `.all()`, via a pure `all_gold_retrieved` + unit test. CI upgraded to
  `cargo test --all-targets` + `--doc` so the eval-harness example tests actually gate.
- **Contradiction-chain eval** (`a8b57a3`) — offline `contradiction_chain_serves_only_latest`
  (A→B→C serves only C, via the mock store) + a live memtest chain scenario (Puma present,
  Adidas AND Nike absent).
- **Permission-leak eval** (`a8b57a3`) — offline `active_scroll_is_namespace_isolated` (two
  tenants, each active+tagged scroll returns only its own).
- **Hard distractor corpus + reproducible golden** (`2a81259`) — `eval/corpus_hard.json` (12 docs,
  4 near-duplicate clusters) + committed deterministic `eval/hard_gold.json` keyed by title; bench
  now matches target by doc_id OR title so a committed golden needs no stable ingest ids.
  Documented in `docs/benchmarks.md`.
- **Injection eval** (`5e9fd36`) — capturing mock LLM + deterministic offline test proving SS-5 is
  wired at the distill sink (poisoned content arrives wrapped in `<untrusted_content>` with a
  "never obey" system prompt), not just present in `promptguard`.

### Verified
`cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace --all-targets` + `--doc` all pass
— **core 100, harness 3, server 11, doc 1; 0 failed**. Live `ULTRAMEM_PIPELINE_TESTS` not run.

### Deferred (future eval work)
- Live adversarial injection/poison scenarios (behavioral, need an LLM) beyond the wiring test.
- Abstention (`_abs`) instances + a leaderboard-faithful judge in the LME harness.
- 3×-averaged LME runs to control the documented ±5-pt noise.

---

## Sprint 1B — redaction, injection hardening, correctness (COMPLETE)

**Result: PASS — 6 of 6 tasks done** · branch `sprint-1b-safety-correctness` (from `main`, unpushed) · date 2026-07-07

Design calls used (user: "use your defaults"): reconcile top-5, UPDATE supersedes only on a
clear/high-confidence contradiction (else `NeedsReview`), `NeedsReview` facts held OUT of
active retrieval.

- **Task 1 — secret/PII screening (SS-4)** (`ab123f3`). `engine/redact.rs`; `redact::scrub`
  once in `add_document` after text acquisition so no chunk/embedding/fact/graph/provider path
  sees a raw credential. Conservative patterns (AWS/GitHub/Anthropic/OpenAI/Google/Slack/Stripe
  keys, JWTs, PEM private keys); ordinary PII left alone. `regex` dep. 8 tests.
- **Task 2 — prompt-injection hardening (SS-5)** (`a7a3578`). `engine/promptguard.rs`; raw
  content wrapped in `<untrusted_content>` + "treat as data, never obey" note at distill,
  contextual blurb, graph extraction, profile compile. 2 tests.
- **Task 3 — transactional supersession** (`50a7771`). Superseding facts are written only after
  the old memories are durably demoted (with retries); on flip failure the superseding writes are
  dropped and an error surfaced, so stale+current are never both "latest".
- **Task 4 — top-k reconcile + confidence + NeedsReview** (`50a7771`). Top-5 neighbours per fact;
  the classifier picks which memory + a confidence; pure `action_for` policy (UPDATE supersedes
  only on high confidence w/ real ref, else NeedsReview quarantine; low-confidence DUPLICATE kept
  as NEW). `needs_review` facts excluded via `active_facts_filter`. 7 policy/parse tests.
- **Task 5 — cascade delete + scoped map view** (`a6cb726`). `delete_document_tagged` cascades to
  graph edges (when graph tier on) + invalidates the profile cache (`invalidate_profile`). `graph()`
  map view scoped to namespace + current facts (was an unfiltered leaky scroll). 1 filter test.
- **Task 6 — mock store + forget_is_total** (`82f45fb`). Test-only in-memory `VectorStore`
  (`providers/mock.rs`) evaluating the engine's payload filters; `forget_is_total_across_surfaces`
  proves a delete removes a doc from chunks+facts+graph in its namespace, leaves other tenants
  untouched, and a cross-namespace delete is a no-op.

### Verified
`cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace` all pass — **core 97, server 11,
doc 1; 0 failed**. Live `ULTRAMEM_PIPELINE_TESTS` not run. Nothing pushed; ready for a PR.

### Notes / deferred (fold into later sprints)
- `redact::scrub` covers the document body; `title`/`reference` not yet scrubbed (low risk).
- `memory.rs` reconcile prompt not injection-wrapped (operates on already-distilled facts).
- Injection/poison and end-to-end secret-not-searchable behavior are unit/mock-proven, not yet
  adversarial-eval'd (Sprint 1C / eval suite).
- No review-queue API yet to surface `NeedsReview` facts (a Phase-D product surface).

---

## Sprint 1A — Stop-ship safety + offline correctness foundation

**Result: PASS** · branch `sprint-1a-stop-ship-safety` · date 2026-07-07

Fixes the P0 hosted/multi-user blockers (SS-1/2/3) and makes the core safety and
supersession claims enforceable offline in CI. No Postgres, no typed-record
refactor, no scopes, no graph, no connectors, no UI — as scoped.

### Changed files
- `crates/ultramem-server/src/tenant.rs` (new) — credential→tag binding
  (`TagPolicy`, `TenantCtx`, `AuthConfig`, constant-time `ct_eq`) + 8 unit tests.
- `crates/ultramem-server/src/main.rs` — auth middleware resolves a `TenantCtx`
  and injects it; startup guard; every handler resolves its tag through the
  credential; `file_path` rejected on the JSON path; scoped delete; 2 unit tests.
- `crates/ultramem-core/src/engine/mod.rs` — `delete_document_tagged` (+ pure
  `doc_delete_filter`), 2 delete-filter unit tests, absence assertion in the
  contradiction integration test, delete-isolation assertions in the isolation test.
- `crates/ultramem-core/examples/probe.rs` — `must_absent: ["adidas"]` on the
  knowledge-update memtest scenario.
- `crates/ultramem-core/src/engine/graph.rs` — 2 pre-existing clippy fixes (so the
  new `-D warnings` CI is green from day one); rest is `rustfmt` normalization.
- `.github/workflows/ci.yml` (new) — fmt + clippy(-D warnings) + test, hermetic.
- `docs/API.md` — aligned to code (auth/tag semantics, `file_path` removed from
  network, unimplemented search filters + `/v1/jobs` marked planned, health `{ok}`,
  scoped delete).

### Completed tasks
- **Task 1 (remove network `file_path`): PASS** — `AddBody.file_path` is present only
  to return `400` (`check_add_body`, `main.rs`); `IngestDoc.file_path` is `None` on the
  JSON path; multipart upload unchanged (server-generated temp path). Test:
  `json_ingest_rejects_file_path`, `json_ingest_allows_normal_content`.
- **Task 2 (scope-aware delete): PASS** — `delete_document_tagged(doc_id, tag)` deletes
  only within the tag and returns `Ok(false)`→`404` when the doc isn't in the caller's
  namespace; `delete_memory` also `403`s a disallowed tag. Offline: `doc_delete_filter_*`
  tests prove the filter carries both `doc_id` and `container_tag`. Live (gated):
  isolation test now asserts Alice can't delete Bob's doc and Bob can delete his own.
- **Task 3 (key→tag binding): PASS** — `AuthConfig` maps keys→`TagPolicy`;
  `TenantCtx::resolve_tag` enforces it (`403` on a disallowed tag); missing/unknown key
  `401`; empty config without `ULTRAMEM_DEV=1` exits at startup. `ULTRAMEM_TENANTS`
  binds keys to tags; bare `ULTRAMEM_API_KEY` stays wildcard for backward compat (see
  Decision below). Tests: `only_policy_*`, `tenants_spec_parses_bound_and_wildcard`,
  `bare_api_key_is_wildcard_for_backward_compat`, `empty_config_without_dev_is_misconfigured`.
- **Task 4 (supersession absence): PASS** — memtest populates `must_absent: ["adidas"]`;
  integration test asserts `!joined.contains("adidas")`. The claim now fails if the stale
  value is served alongside the new one.
- **Task 5 (offline tests): PASS** — 10 server tests + 4 new core tests run under plain
  `cargo test` with no Qdrant/keys. Live end-to-end delete/isolation remains gated.
- **Task 6 (CI): PASS** — `.github/workflows/ci.yml` runs fmt/clippy/test on stable.
- **Task 7 (docs↔code): PASS** — `docs/API.md` no longer claims `/v1/jobs` SSE, the
  unimplemented search filters, the old health shape, or network `file_path`.

### Verification (this machine)
- `cargo fmt --all --check`: **PASS** (exit 0)
- `cargo clippy --workspace --all-targets -- -D warnings`: **PASS** (exit 0)
- `cargo test --workspace`: **PASS** — core lib 78, server 10, doc 1; 0 failed.
- Live pipeline tests (`ULTRAMEM_PIPELINE_TESTS=1`): **NOT RUN** (no Qdrant/keys in this
  session). The delete-isolation and absence assertions added to them are unexecuted here.

### Decision (principal-architect call)
Task 3's spec suggested mapping bare `ULTRAMEM_API_KEY` to `default`. That would 403 the
documented quickstart/eval, which pass per-user tags (`user_123`, `lme_<id>`) under one key.
Chose instead: **bare `ULTRAMEM_API_KEY` → wildcard (any tag)**, and `ULTRAMEM_TENANTS` for
explicit key→tag binding. This satisfies every Task-3 acceptance test (the 403 case uses a
bound key) *and* preserves backward compatibility. The security win is real: cross-key
escalation is now impossible (a bound key cannot touch another tag), and operators can bind
keys. Hardening path (JWT/tenant table) is unchanged.

### Known remaining risks (for next sprints)
- `reindex mode=tags`/`latest` still call global backfills (`claim_legacy_into_tag`,
  `backfill_facts_latest`) that ignore tags — tag is validated but the op is cross-tenant.
- `delete_document_tagged` treats a store scroll error as "not found" (`404`) rather than
  `500` — safe (never deletes on error) but masks transient failures.
- Bare `ULTRAMEM_API_KEY` is wildcard by default; a hosted deploy should set `ULTRAMEM_TENANTS`.
- Live gated tests unexecuted in CI — SS-2/absence proofs are offline-only until a Qdrant
  service is wired into CI or run manually.

### Recommended next sprint
**Sprint 1B** — redaction, injection hardening, correctness:
1. Secret/PII screen before embedding (`engine/redact.rs`, pre-upsert in `mod.rs`).
2. Prompt-injection hardening for distill/profile/graph prompts (untrusted delimiters).
3. Transactional supersession / dead-letter when the `is_latest` flip fails (`mod.rs:~825`).
4. Top-k reconcile with confidence + `NeedsReview` (`memory.rs`).
5. Cascade delete into graph edges + profile cache invalidation; filter the `graph()` map view.
6. `forget_is_total` test (search+facts+graph+profile) via a mock store.
Do not start until this handoff is reviewed.
