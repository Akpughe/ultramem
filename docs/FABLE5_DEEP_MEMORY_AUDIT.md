# UltraMem — Deep Memory Audit (Fable 5)

*Repo-grounded audit of the UltraMem memory engine against the bar of a world-class
memory layer for people, agents, and company brains. Every finding cites
`file:line`. "Docs say" is kept separate from "code does." Date: 2026-07-07.*

**Method.** Five parallel read-only auditors traced the implemented Rust
(`crates/ultramem-core`, `crates/ultramem-server`), cross-checked the docs, and ran
the test suite. `cargo test` and `cargo test -p ultramem-core --lib` pass: **76 unit
tests + 1 doc-test, 0 failed, in ~0.04s**. Every one of those is a pure-function or
mock test (parsers, filter-shape, chunkers). All end-to-end memory tests
(contradiction, tenant isolation, ingest→search→delete) are gated behind
`ULTRAMEM_PIPELINE_TESTS=1` + live Qdrant + three API keys and **did not run** — so
this audit gives *unit-test confidence*, not *live-system confidence*. `ultramem-server`
has **zero tests**. There is **no CI** (`.github/` does not exist).

---

## 1. Executive verdict

UltraMem is a **well-built, honestly-documented two-layer RAG engine with the
*scaffolding* of a memory layer** — and it is roughly one serious quarter of work
away from being a safe production memory service, and considerably further from a
"company brain." The chunk/retrieval half is genuinely good: content-aware chunking,
a cross-encoder rerank stage, an LLM query planner that resolves relative dates, and
hard per-namespace filtering that is threaded through every read path. The memory
half — the entire thesis of the project — is real but shallow: facts are flat
strings, the reconciliation loop is a single-nearest-neighbor + one LLM call that
**fails open to NEW** on any error, deletion does not cascade to derived state, and
the temporal knowledge graph that produced the headline eval win is **disabled in the
shipped product** (`temporal_graph: false`, no env toggle, the server never calls it).
On top of that sit three P0 security defects that make the current server unsafe to
expose to more than one trusted user: a single shared API key with client-controlled
`container_tag` (so any key-holder reads any tenant), a `DELETE` that ignores the
namespace, and an arbitrary-server-file-read via the `file_path` ingest field. The
docs are unusually candid (the benchmark page tells you the corpus is easy), which is
a real asset — but several doc claims (`/v1/jobs` SSE, search filters, "only the
current fact is served") describe code that does not exist or is not asserted. **Bottom
line: strong retrieval bones, an embryonic memory layer, and a security/tenancy model
that has to be rebuilt before anyone puts real user data in it.**

---

## 2. Current architecture map

Implemented components (● shipped/on · ◐ built but off-by-default · ○ eval-only/dead):

- ● **Ingestion** — text / URL (Jina Reader) / PDF+image (Mistral OCR) / Office (macOS
  `textutil` fallback only). Synchronous inside the HTTP request.
- ● **Chunking** — markdown-by-heading, transcript-by-speaker, chat-by-round, else
  paragraph packing (`chunker.rs`).
- ● **Dense retrieval + rerank** — Jina embed → Qdrant search → group-by-doc →
  Jina cross-encoder rerank + lexical title boost (`mod.rs::retrieve_for_plan_tagged`).
- ● **Query planner** — fast LLM rewrites query, resolves relative dates in Rust,
  detects source/list intent (`rewrite.rs`).
- ● **Fact distillation** — segment → extract → merge, `Vec<String>` (`distill.rs`).
- ● **Fact reconciliation** — 1-NN + batched LLM classify NEW/DUPLICATE/UPDATE/EXTEND,
  `is_latest` flip (`memory.rs`, `mod.rs::index_memories`).
- ● **Profile** — two free-text sections, 1-hour TTL cache (`profile.rs`).
- ● **Namespace isolation** — `container_tag` payload filter on every read.
- ◐ **Hybrid dense+sparse (BM25/RRF)** — built, off by default.
- ◐ **Contextual Retrieval blurb / fact-augmented keys** — built, off by default.
- ○ **Bi-temporal knowledge graph** — `graph.rs`; **off in prod**, only the LongMemEval
  harness turns it on.
- ○ **`graph()` map view**, **`refresh_profile`**, **`urlinfo`** — dead code, zero
  callers in the workspace.

```mermaid
flowchart TD
  A[POST /v1/memories] --> B{acquire text}
  B -->|file| C[Jina Reader / Mistral OCR / textutil]
  B -->|url| D[Jina Reader URL]
  B -->|raw| E[content]
  C & D & E --> F[truncate 60k chars]
  F --> G[chunk_doc]
  G --> H[embed chunks + title prefix]
  H --> I[(Qdrant ultramem_chunks)]:::store
  I --> J{content >= 280 chars?}
  J -->|yes| K[distill: segment->extract->merge  Vec String]
  K --> L[reconcile: 1-NN >=0.75 -> batched LLM classify]
  L --> M[(Qdrant ultramem_facts + is_latest)]:::store
  J -->|no| I
  K -.temporal_graph=false in prod.-> N[graph edges]:::off
  N -.-> O[(Qdrant ultramem_graph)]:::off

  Q[POST /v1/search] --> R[plan query]
  R --> S[tagged_filter + build_filter]
  S --> T[embed query x multi-query]
  T --> U[parallel: chunk search || facts search active-only]
  U --> V[group-by-doc -> rerank + title boost]
  V --> W[return documents + memories]
  X[GET /v1/profile] --> Y[scroll facts 2400 -> 2x LLM compile -> TTL cache]
  classDef store fill:#1f6f43,color:#fff
  classDef off fill:#8a1c1c,color:#fff
```

Storage: **Qdrant only** — it is *both* index and source of truth. No Postgres/SQLite/
object store anywhere (`mod.rs:1489`: "the index IS the source of truth"). Original
files are deleted after ingest (`main.rs:256-272`); only extracted chunk text survives.

---

## 3. Claim vs. reality matrix

| Claim (source) | Evidence | Status | Risk | Verify next |
|---|---|---|---|---|
| "memory, not RAG" — distilled facts reconciled over time (`README:5`) | `distill.rs`, `memory.rs`, `mod.rs::index_memories` exist and run | **Implemented** | — | It's real but shallow — see rows below |
| Knowledge update: "only the current one is served" (`benchmarks.md:24`, `HOW-IT-WORKS`) | `is_latest=false` flip (`mod.rs:825-833`) + `active_facts_filter` (`mod.rs:1654-1664`) | **Partial** | P1 | **No test asserts the old value is *absent*** (`must_absent: vec![]`, `probe.rs:169`; no `!contains("adidas")`); a failed flip is `eprintln!` only (`mod.rs:835-839`) |
| Temporal correctness / bi-temporal graph (`HOW-IT-WORKS`, `longmemeval-STATUS`) | `graph.rs` full impl; `valid_to` never written (`graph.rs:154`); `temporal_graph:false` (`mod.rs:147`) | **Partial / Claimed** | P1 | The graph is **off in prod**; `captured_at`/`is_latest` never read at query time; run with it *on* before claiming bitemporal |
| Namespace isolation "hard-isolated… verified multi-tenant" (`README:50`) | `tagged_filter` on every read (`mod.rs:1631-1648`); isolation test (`mod.rs:2128`) | **Partial** | **P0** | Tag is **client-supplied, not key-derived** (`main.rs:140`, `API.md:10` admits it); DELETE ignores tag (`main.rs:279`) |
| Standing profile, cached (`README:51`, `HOW-IT-WORKS Part 5`) | `profile.rs::compile`, TTL cache (`mod.rs:1378-1395`) | **Implemented (shallow)** | P2 | Scroll is **unordered** not "latest" (`qdrant.rs:359` no `order_by`); `refresh_profile` has **zero callers**; no citations |
| Hybrid dense+sparse search (`README:53`) | `sparse.rs`, hybrid collection (`qdrant.rs:76`); `hybrid:false` (`mod.rs:144`) | **Partial** | P2 | Off by default, needs a hybrid-schema collection |
| Image OCR ingestion (`README:55`) | `ocr.ocr_image` (`mod.rs:443`) | **Implemented (text-only)** | P2 | Pixels/visual content discarded; no image embedding |
| Re-index without re-extraction (`README:56`) | `reconstruct_doc_text` (`mod.rs:1470`), `redistill_doc` (`mod.rs:1565`) | **Implemented** | P2 | `mode=facts` **destroys** the supersession chain (`delete_by_doc` then re-distill) |
| Eval harness / reproducible numbers (`README:57`, `benchmarks.md`) | `probe.rs`, `longmemeval.rs` exist | **Partial** | P1 | Result files **gitignored**; golden set regenerated per run; no CI; judge is Gemini not GPT-4o |
| "Retrieval solved, 97.5% gold retrieved" (`longmemeval-STATUS:8,36`) | `gold_retrieved` uses `.any()` (`longmemeval.rs:309`) | **Claimed** | P1 | Team's own roadmap calls this metric **broken** (`roadmap.md:29,73`); fix never implemented |
| `POST /v1/reindex` async jobs w/ SSE `/v1/jobs/:id` (`API.md:79`) | No jobs endpoints; detached `tokio::spawn`, errors dropped (`main.rs:369-391`) | **Missing** | P2 | Docs describe an API that isn't there |
| Search filters `source/after/before/rerank/mode` (`API.md:42`) | `SearchBody{query,container_tag,limit}` only (`main.rs:287`) | **Missing** | P2 | Params silently ignored |
| Forgetting / expiry / `valid_until` (`HOW-IT-WORKS Part 6`) | Retrieval-time filter only (`mod.rs:1660`); points never removed | **Partial** | P1 | Expired/superseded facts persist forever; deletion doesn't cascade to graph/profile |
| Provider-agnostic via traits (`README:61-73`) | `providers/mod.rs:45-155`, `with_*` builders | **Implemented** | P2 | True; but swapping embedder dim has **no migration path** |

---

## 4. Top 20 gaps (ordered by impact)

**P0 — blocks safe/correct production use**

1. **No API-key→tenant binding; `container_tag` is client-supplied.** `auth` compares
   one shared static key (`main.rs:96-101`); the tag comes straight from the request
   body (`main.rs:140`, `tag_or_default`). *Why it matters:* any key-holder reads/
   writes/deletes any tenant's memory by changing a string — multi-tenancy is
   cooperative, not enforced. *Fix:* derive the tag(s) from the key (key→tenant table);
   reject client tags outside the key's allowed set. *Effort:* M. *If ignored:* one
   leaked key exposes every customer.

2. **`DELETE /v1/memories/:id` ignores the namespace.** `delete_document(&id)` deletes
   by `doc_id` across both collections with no tag filter (`main.rs:279-284`,
   `mod.rs:1590-1598`). *Fix:* require + enforce tag on delete. *Effort:* S. *If
   ignored:* trivial cross-tenant data destruction, no undo, no audit.

3. **Arbitrary server-file read via `file_path`.** JSON ingest accepts a server-side
   path (`main.rs:142-144`) and the engine does `tokio::fs::read(p)` unrestricted
   (`mod.rs:435`); contents are then retrievable via search. *Fix:* remove `file_path`
   from the network API (multipart only) or allow-list a sandbox dir. *Effort:* S.
   *If ignored:* exfiltrate `.env`, secrets, `/etc/passwd`, cloud metadata.

4. **No secret/PII screening, with clipboard as a first-class source.** No redaction/
   denylist anywhere (repo-wide grep); a copied password/API key is embedded verbatim
   into `ultramem_chunks` and becomes semantically searchable
   (`distill.rs:23-24` names clipboard; upsert precedes any filtering). *Fix:* secret
   scanner + PII classifier at ingest, before embed. *Effort:* M. *If ignored:* the
   memory layer becomes a searchable credential store.

5. **Prompt-injection laundered into every assistant's system prompt.** Ingested web/
   file content flows unsanitized into distill → "facts" → profile → `as_prompt_block`
   (`distill.rs:72-77` → `profile.rs:117-141` → `profile.rs:46-59`), cached an hour.
   A malicious page can also trigger an UPDATE that flips a true memory to
   `is_latest=false` (`memory.rs:106-113` → `mod.rs:825-833`). *Fix:* delimit
   untrusted content, provenance-tag facts, keep injected instructions out of the
   profile sink, require corroboration before supersession. *Effort:* L. *If ignored:*
   one article durably rewrites what the system "knows" and steers future answers.

**P1 — blocks world-class quality / company-brain fit**

6. **Contradiction handling fails open.** Single nearest neighbor at cosine ≥ 0.75
   (`memory.rs:31`, `limit=1`), one batched LLM call, and *any* error → every candidate
   degrades to NEW (`memory.rs:86,145`); neighbor-search errors are swallowed
   (`unwrap_or_default`, `mod.rs:760`); a failed `is_latest` flip is only logged. *Result:*
   stale and corrected facts coexist as "latest." *Fix:* multi-neighbor, retries,
   transactional flip, dead-letter on failure. *Effort:* M.

7. **The temporal graph is off in the shipped product.** `temporal_graph:false`, no env
   override (`mod.rs:147,156-204`), server never calls any graph API — only
   `longmemeval.rs:86` enables it. *Result:* every real user still gets the
   "latest value" failure the graph was built to fix. *Fix:* wire it into prod behind a
   config, add the entity-node model. *Effort:* L.

8. **Facts are untyped flat strings; event time lives inside the text.** Stored `fact`
   is a `String`; `kind` is always `"fact"` (`mod.rs:803-819`); the `[on YYYY-MM-DD]`
   event date is never parsed into a field (only `[until …]` is). *Result:* no
   confidence, no type-aware ranking, no "what happened in March" filtering, no review
   workflow. *Fix:* typed memory record (below). *Effort:* L.

9. **No entity resolution.** Only `normalize_key` lowercasing (`graph.rs:164`); "Dave",
   "David Akpughe", and an email become three disjoint subjects. *Result:* any multi-
   person / CRM / team use degrades to noise. *Fix:* alias map + embedding-based entity
   merge + canonical IDs. *Effort:* L.

10. **Deletion doesn't cascade.** `delete_document` skips the graph collection
    (`mod.rs:1590-1598` touches only chunks+facts) and never invalidates the profile
    cache; the unfiltered `graph()` map view leaks superseded, expired, **and other
    tenants'** facts (`mod.rs:1407-1409`, no tag filter). *Result:* "forget" visibly
    doesn't forget. *Fix:* cascade delete + profile invalidation + filter `graph()`.
    *Effort:* M.

11. **No document-level dedup.** Fresh `Uuid::new_v4()` per ingest (`mod.rs:424`); no
    content hash, no canonical URL. Query-time collapse is by *score not recency*
    (`mod.rs:1769-1772`), so a stale capture can outrank the current page. *Fix:*
    content-hash + canonical-URL dedup, snapshot/version chain. *Effort:* M.

12. **Scroll-all is the data model.** `list_document_ids` scrolls up to **50,000 points**
    (`mod.rs:1492-1530`); profile samples an **unordered** 2,400 (`profile.rs:85`);
    graph supersession scrolls 2,000 per ingest, resolve/count 4,000 per question. *Result:*
    timelines, profiles, and supersession **silently truncate** past a few thousand docs —
    the product breaks for its target power users. *Fix:* a real document/registry table
    (Postgres). *Effort:* L.

13. **Failed distillation is invisible.** On distill error the doc keeps chunks, zero
    facts, forever; status is always `"done"` (`main.rs:175`, `mod.rs:625`). No marker,
    retry, or way to enumerate under-processed docs. *Fix:* per-doc processing state +
    retry queue. *Effort:* M.

14. **No CI and env-gated core tests.** No `.github/`; every end-to-end memory test needs
    live keys and never runs by default; no test asserts a superseded value is *absent*.
    *Result:* engine regressions ship silently. *Fix:* mock Qdrant/LLM, CI gates,
    absence assertions. *Effort:* M.

15. **`gold_retrieved` metric is broken but headlines the roadmap.** `.any()` over gold
    session ids (`longmemeval.rs:309`); the roadmap itself documents it as broken
    (`roadmap.md:29,73`) and the fix was never implemented. *Result:* "retrieval solved"
    is overstated; synthesis-vs-retrieval attribution is unreliable. *Fix:* all-not-any
    + per-question gold-chunk check. *Effort:* S.

16. **Time filters use capture time, not event time.** `build_filter` ranges on
    `captured_at` (`mod.rs:1672-1681`). *Result:* "what did I do last week" matches when
    it was *saved*, not when it *happened*. *Fix:* structured event-time field + filter.
    *Effort:* M.

**P2 — important, not a core blocker**

17. **No observability.** `eprintln!` only; `tower-http` `trace` feature compiled but no
    `TraceLayer` wired (`main.rs:68-72`); no metrics, no audit log. *Fix:* structured
    logging + tracing + audit table. *Effort:* M.

18. **Embedder swap has no migration.** Different dim needs fresh collections
    (`openai.rs:6-8`); no payload stamps the embedding model; reindex never re-embeds.
    *Result:* provider change = full manual re-ingest or silent corruption. *Fix:*
    model-id in payload + re-embed job. *Effort:* M.

19. **No backup/export; Qdrant loss is total.** Sole store, originals deleted, no export
    path in-repo. *Fix:* object-storage of originals + export API. *Effort:* M.

20. **Transport/auth hardening.** Permissive CORS (`main.rs:71`), plaintext HTTP
    (`main.rs:78`), timing-unsafe key compare (`main.rs:100`), unauthenticated mode on
    empty key, no rate limiting, no LLM-request timeout (`llm.rs:184-314`). *Fix:*
    constant-time compare, TLS/proxy guidance, per-tenant quota, timeouts. *Effort:* S–M.

---

## 5. Memory model proposal

Replace the flat-string fact with a typed, evidence-bearing record. Qdrant stays the
vector index; a relational store (Postgres) becomes the source of truth for the fields
that need transactions, history, and enumeration.

```rust
// A single durable memory. Qdrant holds {id, embedding, tenant, is_latest, valid_*}
// for filtered vector search; Postgres holds the authoritative row.
struct Memory {
    id: Uuid,
    tenant: Scope,                 // who owns/sees it (see §6)
    kind: MemoryKind,              // typed, not always "fact"
    statement: String,            // the standalone fact text (embedded)
    subject: EntityRef,            // resolved entity id, not a raw string
    predicate: Option<String>,     // for attribute-style facts
    object: Option<Value>,         // typed value (string|num|date|entity)
    confidence: f32,               // 0..1 from extractor + corroboration count
    // provenance
    source: SourceRef,             // document id + chunk span (char range)
    evidence_quote: String,       // the exact sentence it came from
    extractor: String,             // model id + prompt version
    // temporal (bitemporal)
    event_time: Option<DateRange>, // valid_from / valid_to (when true in the world)
    learned_at: i64,               // transaction time (when we ingested it)
    // lifecycle
    is_latest: bool,
    supersedes: Option<Uuid>,
    superseded_by: Option<Uuid>,   // reversible
    extends: Vec<Uuid>,
    corroborated_by: Vec<Uuid>,    // the "repetition bump" the docs promise
    review_state: ReviewState,     // Auto | Pinned | Rejected | NeedsReview
    forget_state: ForgetState,     // Active | Expired | Redacted | Deleted(when,why)
}
enum MemoryKind { Preference, PersonalFact, ProjectFact, Policy, Decision,
                  Task, Event, Claim, Quote, Relationship }
```

Example record (JSON, one memory):

```json
{
  "id": "9b1e…", "tenant": {"user":"u_123"}, "kind": "Preference",
  "statement": "The user's preferred running-shoe brand is Puma",
  "subject": {"entity":"ent_user_123","label":"the user"},
  "predicate": "running_shoe_brand", "object": {"string":"Puma"},
  "confidence": 0.86,
  "source": {"document":"db0eb2a4…","span":[142,193]},
  "evidence_quote": "they switched entirely from Adidas to Puma",
  "extractor": "gpt-oss-120b@distill-v3",
  "event_time": {"valid_from":"2026-06-01","valid_to":null},
  "learned_at": 1781463180,
  "is_latest": true, "supersedes": "7c22…", "superseded_by": null,
  "corroborated_by": ["a91…","f03…"],
  "review_state": "Auto", "forget_state": "Active"
}
```

- **Relationships** are first-class rows: `(src_memory|entity) --rel--> (dst)` with
  `rel ∈ {duplicate, updates, extends, contradicts, derived_from, supports, refutes,
  depends_on, caused_by, belongs_to, mentions, authored_by}`, each with confidence +
  provenance. This is the entity-**node** graph the roadmap already wants.
- **Profiles** become a materialized view over `Memory` filtered by `review_state != Rejected`
  and `is_latest`, per scope, **with citations** (each bullet keeps its source memory ids),
  recompiled on ingest/delete (event-driven, not just TTL).
- **Forgetting** is a state machine, not a row delete: `Redacted`/`Deleted` propagate to
  the profile view, graph edges, and caches, and are recorded in an audit table.

---

## 6. Company-brain architecture

The current scope primitive is one opaque `container_tag`. A company brain needs a
**scope hierarchy** with explicit membership and per-source ACLs:

```
Global company knowledge
  └─ Org
       ├─ Team ──── Project ──── Account/Customer
       │              └─ Source/Document (carries its own ACL)
       └─ Individual (private) ── Agent (private to a user)
```

- **Scopes** replace the flat tag: a memory is owned by exactly one scope and *visible*
  to a computed set (own scope + ancestors it was promoted to). Retrieval filters by the
  caller's *visible set*, not one tag.
- **Promotion**, not leakage: private → team → company memory moves only through an
  explicit promote action (with review), so a user's private facts never appear in a
  shared answer by default. This is the safe inverse of today's "any tag reads any tag."
- **Source-level ACLs at ingest**: connectors carry the source's own permissions (a Slack
  channel's members, a Drive file's sharing) into an ACL on every memory derived from it;
  retrieval intersects the ACL with the caller's identity.
- **Provenance + citations everywhere**: every answer cites the source memories and their
  documents; "where did this come from / who can see it / is it still true / which source
  wins" become first-class queries backed by the typed record in §5.
- **Governance**: audit log of every read/write/delete/promote; conflict resolution by
  source-trust ranking; admin controls for retention and legal deletion.
- **Data model**: Postgres for scopes, memberships, ACLs, documents, memories, edges, jobs,
  audit; Qdrant as the vector index keyed by memory/chunk id; object storage for original
  files (currently deleted). Qdrant stops being the source of truth.

---

## 7. State-of-the-art roadmap

Each phase is shippable with acceptance tests and expected user impact.

**Phase 0 — Verification & benchmark hardening.** Deliverables: mock-Qdrant + mock-LLM so
the contradiction/isolation/delete tests run in plain `cargo test`; add absence assertions
(`!contains("adidas")`, `must_absent`); fix `gold_retrieved` to all-not-any + gold-chunk
check; commit result files + golden set (or a seed) so numbers reproduce; add GitHub Actions
CI. *Acceptance:* `cargo test` exercises supersession + isolation with no live keys; CI red
on regression. *Impact:* claims become trustworthy.

**Phase 1 — Critical memory correctness & safety.** Deliverables: fix the three P0 security
defects (key→tenant binding, tag-scoped DELETE, remove `file_path`); secret/PII screening at
ingest; provenance out of the retrieve API (facts carry source + `captured_at` + span);
cascade delete + profile invalidation; document-level content-hash dedup; per-doc processing
state + retry. *Acceptance:* pen-test shows no cross-tenant read/write/delete; deleting a doc
removes it from search, graph, and profile within one request; re-ingesting a file creates no
duplicate. *Impact:* safe for real multi-user data.

**Phase 2 — Sources / connectors / company-brain scaffolding.** Deliverables: Postgres source
of truth (scopes, memberships, ACLs, documents, memories, jobs, audit); object storage for
originals; scope hierarchy + promotion; first connectors (Slack/Drive/email) with ACL
ingestion + incremental sync + snapshots. *Acceptance:* a team member sees team memory but not
a colleague's private memory; revoking a source access removes its memories from that user's
answers. *Impact:* usable as a shared brain, not just a personal store.

**Phase 3 — Relationship & temporal intelligence.** Deliverables: entity-node graph with
resolution/aliases/canonical IDs; typed memory record (§5) with structured event time;
turn the temporal graph **on in prod**; background re-linking job (tomorrow's input links to
today's facts). *Acceptance:* "how many weddings this year" counts correctly; "Dave"/"David
Akpughe" resolve to one entity; "what did we believe on 2026-03-01" answers via bitemporal
query. *Impact:* real memory behavior, not nearest-neighbor RAG.

**Phase 4 — Enterprise trust, observability, review.** Deliverables: structured logging +
tracing + metrics; full audit log; memory review/edit/pin/forget UI; source-trust ranking +
conflict resolution; compliance deletion with proof. *Acceptance:* an admin can show what was
forgotten and prove it's gone from every derived surface. *Impact:* enterprise-buyable.

**Phase 5 — Frontier retrieval & agentic reasoning.** Deliverables: agentic multi-query
read-and-reason retrieval gated on a token budget; graph-traversal retrieval; query planning
for list/count/time questions with pagination. *Acceptance:* beats vector-top-K on the hard
LongMemEval categories at a measured token cost. *Impact:* pushes toward the ~90% band.

---

## 8. Evaluation plan

- **Datasets:** LongMemEval-S full distribution (not just balanced 20/type) **including the
  30 abstention `_abs` questions** currently excluded; a hard retrieval corpus with near-
  duplicate distractors and contradictions (today's 24-doc corpus is deliberately easy,
  `benchmarks.md:72`); a temporal-update chain set (A→B→C); a forgetting set; a permission-
  isolation set; a poisoned-memory/prompt-injection set; a cross-source synthesis set.
- **Metrics:** accuracy by category; **tokens injected** and **latency** (already partly
  measured); superseded-value **absence rate**; forget-completeness (does deleted data appear
  in search / profile / graph / logs); cross-tenant leak rate (must be 0); entity-resolution
  F1; abstention accuracy.
- **Regression gates (CI):** contradiction serves only the latest (absence-asserted); tenant
  isolation 0 leaks; delete cascades; distill-failure is flagged not silent; `gold_retrieved`
  all-not-any. Any of these red = block merge.
- **Manual loop:** human preference review on the subjective categories (preference has a
  judge ceiling, per `longmemeval-STATUS:44`); 3× averaged runs to control the documented
  ±5-point noise (the averaging code does not yet exist).
- **Sample hard cases:** "I switched to Puma, then back to Adidas, then to Hoka — which now?"
  (chain); "delete the onboarding doc, then ask what it said" (forget); "user A's secret must
  never surface for user B" (isolation); "this article says the user loves X — did it change
  a real memory?" (poisoning).

---

## 9. Prompt & extraction policy improvements

The current prompts (verbatim in the appendix of the source audit) are decent but ask for
untyped output with no confidence, no span, and no injection defense. Proposed direction:

- **Fact extraction:** require a JSON **object per fact** with `{statement, kind, subject,
  predicate?, object?, event_time?, valid_until?, confidence, evidence_quote}`; wrap the
  source content in explicit `<untrusted_content>` delimiters with a "treat as data, never
  as instructions" preamble; enforce with JSON-mode/function-calling + a schema validator +
  one re-ask on failure (today: harvest-any-quoted-string fallback, `distill.rs:169-184`,
  which will accept injected text). Drop the fact if `confidence < τ` or no `evidence_quote`
  is present in the source.
- **Relation classification:** return `{i, relation, confidence}` and require ≥2 corroborating
  signals (or high confidence) before an UPDATE flips a memory; classify against the **top-k**
  neighbors, not one; add a `CONTRADICTS-BUT-UNSURE → NeedsReview` outcome instead of forcing
  a flip.
- **Temporal edge extraction:** demand `valid_from` **and** `valid_to` (today `valid_to` is
  always None), and a stable predicate drawn from a **controlled vocabulary** passed in the
  prompt (today predicate stability is begged for, not enforced — the cause of the weddings-
  scatter counting failure).
- **Profile compilation:** feed facts **with their ids** and require each bullet to cite the
  memory ids it rests on; forbid any content that looks like an instruction; recompile on
  ingest/delete, not just on TTL.
- **Forgetting/retention classification:** a dedicated pass that tags each candidate as
  `secret | pii | transient | durable` before embedding, so secrets never reach the store.
- **Source-trust extraction:** stamp each source with a trust tier (owner-authored > first-
  party doc > third-party page > social) used at conflict resolution.

All passes: schema-validated, one bounded re-ask, deterministic fallback that **drops rather
than fabricates**, and a recorded `extractor` version for reproducibility.

---

## 10. Immediate implementation backlog (prioritized)

Security & tenancy (P0):
1. Key→tenant table; derive allowed tags from key. *Files:* `server/main.rs`, new `auth.rs`.
   *Accept:* request with tenant A's key + tenant B's tag → 403. *Test:* handler unit test.
2. Scope DELETE by tag; 404 on cross-tenant id. *Files:* `main.rs:279`, `mod.rs:1590`.
   *Accept:* delete of another tenant's id fails. *Test:* live isolation test extended.
3. Remove `file_path` from the network API (keep for embedded lib only). *Files:* `main.rs:144`.
   *Accept:* JSON `file_path` → 400. *Test:* handler test.
4. Constant-time key compare; refuse to start with empty key unless `DEV=1`. *Files:* `main.rs:31,100`.
5. Secret/PII screen before embed. *Files:* new `redact.rs`, `mod.rs:527`. *Accept:* a fake
   AWS key in content is not searchable. *Test:* ingest→search unit test with a mock embedder.

Correctness (P1):
6. Absence assertions in memtest + integration test. *Files:* `probe.rs:169`, `mod.rs:2115`.
7. Transactional `is_latest` flip; dead-letter on failure. *Files:* `mod.rs:822-840`.
8. Top-k neighbors + confidence in reconcile; `NeedsReview` outcome. *Files:* `memory.rs`.
9. Cascade delete to graph + profile invalidation. *Files:* `mod.rs:1590`, add `refresh_profile` call.
10. Filter `graph()` by `container_tag` + `is_latest`. *Files:* `mod.rs:1407`.
11. Document content-hash dedup + canonical URL. *Files:* `mod.rs:424`, `extract.rs`.
12. Per-doc processing state (`done|facts_pending|failed`) + `/v1/documents/:id/status`. *Files:* `main.rs`, `mod.rs:605`.
13. Fix `gold_retrieved` to all-not-any + gold-chunk. *Files:* `longmemeval.rs:309`.
14. Provenance in retrieve output (facts return source+captured_at+span). *Files:* `mod.rs:1362`.
15. Event-time field on facts (parse `[on …]`); filter on it. *Files:* `distill.rs`, `memory.rs`, `mod.rs:1672`.

Data model & eval (P1/P2):
16. Introduce Postgres source of truth (documents, memories, edges, jobs, audit). *Files:* new `store_pg.rs`.
17. Object storage for originals (stop deleting uploads). *Files:* `main.rs:256`.
18. Real jobs table + `/v1/jobs/:id` (match the docs). *Files:* `main.rs:369`.
19. GitHub Actions CI running `cargo test` + clippy + fmt. *Files:* `.github/workflows/ci.yml`.
20. Mock Qdrant + mock LLM for offline lifecycle tests. *Files:* `providers/`, tests.
21. Commit golden set + result files (or a deterministic seed). *Files:* `eval/`, `.gitignore`.
22. Multi-run averaging in the LME harness. *Files:* `longmemeval.rs`.
23. Add abstention questions + judge path. *Files:* `longmemeval.rs`.
24. Hard distractor corpus for `bench`. *Files:* `eval/`.

Temporal & graph (P2/P3):
25. Turn `temporal_graph` on in prod behind config + env. *Files:* `mod.rs:147,156`, `main.rs`.
26. Write `valid_to` on supersession (close intervals). *Files:* `graph.rs`, `mod.rs:902`.
27. Controlled-vocabulary predicates. *Files:* `graph.rs:68`.
28. Entity-node model + resolution/aliases. *Files:* new `entity.rs`, `graph.rs`.
29. Query-time bitemporal (`as_of` on transaction time). *Files:* `mod.rs:939`.
30. Background re-linking job. *Files:* new worker.

Retrieval & product (P2):
31. Wire documented search filters (`source/after/before/rerank/mode`). *Files:* `main.rs:287`.
32. Pagination cursors on search/timeline. *Files:* `main.rs`.
33. Replace 50k scroll registry with the documents table. *Files:* `mod.rs:1492`.
34. Order profile scroll by `captured_at` (or read newest from PG). *Files:* `qdrant.rs:359`, `profile.rs:85`.
35. LLM-request timeouts. *Files:* `llm.rs:184`.

Observability & trust (P2/P4):
36. Wire `TraceLayer`; structured logging. *Files:* `main.rs:68`.
37. Audit log table + middleware. *Files:* `main.rs`, PG.
38. Per-tenant rate limit/quota. *Files:* `main.rs`.
39. Embedding-model id in payload + guard mixed dims. *Files:* `mod.rs:575,803`.
40. Memory review/edit/pin/forget endpoints. *Files:* `main.rs`, `mod.rs`.
41. Export/import API. *Files:* `main.rs`.
42. Fix docs to match code (jobs, search filters, health). *Files:* `docs/API.md`.

Each carries the same test strategy: a handler/unit test for the deterministic part, and a
gated live test for the pipeline part, both wired into CI.

---

## 11. Open decisions

- **Storage:** keep Qdrant-only (simple, but no transactions/audit/enumeration) or add
  Postgres as source of truth (required for company brain)? *Recommend: add Postgres.*
- **Tenancy model:** key→tenant table vs. per-request JWT with scoped claims. *Recommend:
  JWT/claims for the hosted product, key→tenant as the minimum.*
- **`file_path` API:** remove from network entirely vs. sandbox allow-list. *Recommend:
  remove; embedded lib users still have the Rust API.*
- **Privacy default:** the pipeline requires third-party providers (Jina/Mistral/Groq).
  Offer a fully-local profile (Ollama + local embed + local OCR) for "no data leaves"
  buyers? *Recommend: yes, as a documented tier.*
- **Graph bet:** entity-node graph (production 81–90% path) vs. agentic read-and-reason
  retrieval (frontier path). *Recommend: entity-node first — it also fixes counting and
  isolation, and the eval already proves the direction.*
- **Positioning:** personal memory first (ship P0+P1) vs. company brain first (needs P2).
  *Recommend: personal first — it's one quarter away; company brain is the next quarter.*

---

## 12. Final recommendation — the five highest-leverage actions

1. **Fix the three P0 security defects this week** (key→tenant binding, tag-scoped DELETE,
   remove `file_path`). Until then the server is unsafe for more than one trusted user, and
   every other improvement is built on sand.
2. **Make the memory layer honest under test**: absence assertions for supersession, mock-
   backed lifecycle tests in plain `cargo test`, fix the `gold_retrieved` metric, and add CI.
   The project's credibility rests on claims that are currently unasserted or irreproducible.
3. **Turn the temporal graph on in production and give it entity nodes.** The 63→72.5% eval
   win exists only in the harness; shipping it (plus entity resolution) is the single biggest
   jump in real memory quality and unlocks counting + multi-hop.
4. **Adopt a typed memory record with provenance and add Postgres as the source of truth.**
   Flat strings + Qdrant-only + 50k scroll-alls are the ceiling on everything: review,
   forgetting, permissions, enumeration, and company-brain scopes all need this.
5. **Screen secrets/PII at ingest and delimit untrusted content.** With clipboard/web capture
   as first-class sources feeding the profile→system-prompt sink, the current pipeline will
   both memorize credentials and be steerable by a malicious page.

*Everything above is grounded in the code as of 2026-07-07; where a claim is inference rather
than a cited line it is labeled as such in the source audit. Docs-say vs. code-does is kept
separate throughout.*
