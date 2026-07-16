# UltraMem — 10/10 Acceptance Rubric

The precise definition of done for "10/10 SOTA", and an honest map of **what is
already enforced in CI (offline)** versus **what requires a live benchmark run**
(a real Qdrant + embedding/rerank/LLM providers) that cannot execute in the
hermetic test environment. Derived from `FABLE5_EXECUTION_PLAN.md` §1 and §"gates".

Legend: ✅ enforced offline (red blocks merge) · ⏳ requires live run · 🔜 built, wiring pending.

---

## A. Hard security gates — **zero tolerance** (a single failure blocks merge)

These are the properties a memory layer must never violate. All are enforced
offline today via deterministic unit/scenario tests over the pure resolvers,
filters, and the `MockDb`/`MemStore` seams.

| Gate | Property | Status | Enforcing test(s) |
|------|----------|--------|-------------------|
| **Permission / namespace isolation** | A principal only ever reads its own scope + explicitly-granted scopes; no implicit inheritance | ✅ | `scope::tests::{own_scope_is_always_visible, fail_closed_no_implicit_inheritance, unknown_capability_does_not_grant_read, read_and_higher_grants_are_visible_lower_are_not}`; `engine … scope_filter_multi_scope_never_leaks_ungranted`, `visible_scopes_for_expands_only_via_grant`, `active_scroll_is_namespace_isolated` |
| **ACL admin confinement** | You can only administer a scope your credential already controls (no cross-scope grant escalation) | ✅ | `main.rs … acl_admin_is_confined_to_controlled_scopes`; `db::mock … acl_grant_is_idempotent_and_per_principal`, `acl_revoke_and_by_scope` |
| **Promotion authorization** | Writing into a shared scope needs an explicit `promote`/`admin` grant; `read`/`write` do not confer it | ✅ | `scope::tests::promote_needs_promote_or_admin_grant`; `engine … promote_copies_into_shared_scope_with_provenance` |
| **Right-to-erasure (forget)** | A forgotten fact is removed from **both** the vector index and the source of truth, evidence cascaded, scoped to owner, not resurrectable by rebuild | ✅ | `engine … forget_erases_memory_from_index_and_source_of_truth`, `forget_is_total_across_surfaces` |
| **Prompt-injection resistance** | Untrusted content is delimiter-wrapped with a never-obey instruction before it reaches the model at the distill sink | ✅ | `distill … ingested_content_is_injection_wrapped`; `promptguard::tests::{wrap_adds_delimiters, notes_tell_the_model_not_to_obey}` |
| **Secret redaction** | Credentials (AWS/GitHub/OpenAI/Anthropic/JWT/PEM…) are scrubbed before storage/embedding | ✅ | `redact::tests::{redacts_aws_access_key, redacts_github_token, redacts_openai_key, redacts_anthropic_key_with_correct_label, redacts_jwt, redacts_pem_private_key_block, scrub_is_idempotent}` |
| **Injection / poison / permission — live scale** | The above hold at LongMemEval scale over adversarial corpora | ⏳ | live `eval/` run (see §D) |

## B. Correctness & temporal gates

| Gate | Property | Status | Enforcing test(s) |
|------|----------|--------|-------------------|
| **Contradiction → latest-only** | A→B→C serves only C; superseded/expired/quarantined facts never surface | ✅ | `engine … contradiction_chain_serves_only_latest`, `contradiction_supersedes_old_memory`, `active_facts_filter_excludes_superseded_expired_and_review` |
| **Bitemporal `as_of`** | Reconstruct "what we knew as of time T" — a since-corrected fact surfaces before its correction, not after; expiry honored | ✅ | `db::mock … memories_as_of_is_bitemporal`, `mark_superseded_stamps_transaction_time` |
| **Entity resolution** | Explicit surface forms unify to one canonical, per namespace, losslessly (unknown → itself) | ✅ | `entity::tests::{normalize_folds_case_and_whitespace_and_edges, registered_surface_forms_resolve_to_canonical, unregistered_name_resolves_to_itself}`; `engine … entity_aliases_resolve_scoped_and_updatable` |
| **Recoverability** | Qdrant is a rebuildable index; backfill→drop→rebuild restores chunks+facts with ids preserved | ✅ | `engine … backfill_migrates_qdrant_into_pg_with_parity`, `rebuild_recovers_qdrant_from_pg` |
| **Attribution metric honesty** | `gold_retrieved` requires **all** gold sessions, not any | ✅ | `examples/longmemeval … gold_retrieved_requires_all_sessions_not_any` |
| **Temporal as-of / latest / count — live** | Scripted timelines, exact-match ≥ 92% | ⏳ | live `eval/` run (see §D) |

## C. Frontier retrieval & accuracy — **requires the live run**

| Gate | Target (10/10) | Status |
|------|----------------|--------|
| LongMemEval-S (full dist + abstention), 3×-averaged | **≥ 88%** accuracy | ⏳ live |
| Temporal as-of / latest / count | **≥ 92%** exact-match | ⏳ live |
| Agentic/graph-traversal retrieval beats vector-top-K at a fixed token budget | measured win | 🔜 graph built (`engine::graph`, `resolve_edges_tagged`), gated by `memory_graph`; needs live measurement |
| Query-time entity-alias expansion into retrieval | recall lift | 🔜 registry+resolver shipped (9b); retrieval wiring pending a live-measurable slice |

## D. How to run the live gate (the one thing CI can't)

The hermetic suite cannot stand up Qdrant or call embedding/rerank/LLM
providers, so the accuracy band (§C) and the at-scale security suites (§A last
row, §B last row) must be run against a live stack:

```sh
# 1. bring up Qdrant + set provider keys
export QDRANT_URL=… JINA_API_KEY=… GROQ_API_KEY=…            # + ULTRAMEM_PG_URL for the source of truth
# 2. run the benchmark harness (3× and average, per the plan)
cargo run -p ultramem-core --example longmemeval -- --dataset eval/longmemeval_s.json
```

A run is a **pass** only when every §A/§B suite is green (0 tolerance) **and**
the §C scores clear the 10/10 band, averaged over 3 runs with a committed seed.

---

## Current standing (offline)

Every gate in **§A and §B is enforced offline and green** (`cargo test --workspace
--all-targets` + `--doc`; 138 core-lib + 13 server tests as of this writing), plus
`fmt` and `clippy -D warnings`. The remaining distance to a *certified* 10/10 is
**§C — the live accuracy band and the at-scale adversarial suites** — which is an
operator action (a live benchmark run), not further offline code. The safety and
correctness foundation those runs would exercise is complete and locked by the
tests above.
