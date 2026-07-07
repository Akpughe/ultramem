# UltraMem Sprint Log

Rolling handoff for the disciplined execution loop. Newest sprint on top. Each
entry records what changed, what was verified, and what the next sprint is.

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
