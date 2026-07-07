# Fable 5 Deep Memory Audit Prompt

Use this prompt with Fable 5 to produce a rigorous, repo-grounded analysis of
UltraMem and a roadmap toward a world-class memory layer for people, agents, and
company brains.

## Super Prompt

You are Fable 5 acting as a senior AI memory systems architect, product strategist,
distributed-systems reviewer, and adversarial evaluator. Your job is to deeply
audit the UltraMem repository and produce a concrete plan to turn it into a
state-of-the-art memory layer competitive with the best memory behavior users
expect from ChatGPT, Claude, Cursor, and future company-brain systems.

UltraMem is intended to be more than RAG. It should let people and companies put
in links, articles, Twitter/X bookmarks, PDFs, images, meetings, notes, files,
messages, web pages, and arbitrary captured context. It should then understand,
remember, relate, update, retrieve, and forget intelligently over time. If a user
adds data today, the system should be able to discover tomorrow's relationships,
not just store inert chunks. The target product is a memory layer for personal
assistants, agents, teams, projects, and full company brains.

Your goal is not to flatter the repo. Your goal is to identify what is already
strong, what is claimed but weakly proven, what is missing, what is dangerous at
scale, and what should be built next. Treat "world-class" as the bar.

### First, read the repo

Start by reading these files and source areas before judging:

- `README.md`
- `KICKOFF.md`
- `docs/API.md`
- `docs/HOW-IT-WORKS.md`
- `docs/ROADMAP.md`
- `docs/01-design.md`
- `docs/02-gap-analysis.md`
- `docs/benchmarks.md`
- `docs/MCP.md`
- `crates/ultramem-core/src/engine/mod.rs`
- `crates/ultramem-core/src/engine/distill.rs`
- `crates/ultramem-core/src/engine/memory.rs`
- `crates/ultramem-core/src/engine/graph.rs`
- `crates/ultramem-core/src/engine/profile.rs`
- `crates/ultramem-core/src/engine/rewrite.rs`
- `crates/ultramem-core/src/engine/chunker.rs`
- `crates/ultramem-core/src/providers/mod.rs`
- `crates/ultramem-core/examples/probe.rs`
- `crates/ultramem-server/src/main.rs`
- `docker-compose.yml`

If you can run commands, run:

```bash
cargo test -p ultramem-core --lib
cargo test
```

If provider keys or live Qdrant are unavailable, say so and distinguish unit-test
confidence from live-system confidence.

### Core questions

Answer these questions with evidence from the repo:

1. What is UltraMem today, as actually implemented?
2. Which parts are truly memory, and which parts are still retrieval/indexing?
3. What claims in the docs are implemented, partially implemented, or only
   aspirational?
4. What gaps prevent this from being a top-class memory layer for personal AI and
   company brains?
5. What should be built next, in what order, and why?
6. What needs to be measured before anyone can credibly claim state of the art?

### Audit dimensions

Audit each dimension below. For every major finding, include file/path evidence
and describe the product consequence.

#### 1. Ingestion and source coverage

Assess whether UltraMem is ready for real-world inputs:

- Links and articles
- Twitter/X bookmarks and social links
- PDFs and images
- Browser history and web captures
- Notes and clipboard snippets
- Meetings and transcripts
- Slack/Discord/email/calendar/document repositories
- Code/docs/repos
- User-uploaded files
- Repeated captures and changing web pages

Look for gaps in content extraction, metadata, deduplication, canonical URL
handling, snapshots, source trust, permissions, connector design, and incremental
sync. A world-class system should preserve provenance and source context well
enough to answer "where did this come from?", "when did we learn it?", "is this
still true?", "who is allowed to see it?", and "which source should win?"

#### 2. Facts

Evaluate the fact extraction and memory-write policy:

- Are facts atomic, durable, and independently useful?
- Are facts typed beyond a flat string?
- Is evidence/provenance attached at the right granularity?
- Is there a confidence model?
- Can the system distinguish user preference, personal fact, project fact,
  company policy, decision, task, event, claim, quote, and relationship?
- Can it merge, split, correct, retract, and supersede facts safely?
- Does it avoid memorizing noise, private secrets, and transient junk?
- Can it explain why a fact exists?
- Does it support human review or memory editing?

Be especially skeptical of LLM-only extraction. Identify where schemas, validators,
rubrics, typed memory records, source spans, confidence scoring, and review queues
would improve reliability.

#### 3. Profiles

Evaluate user, entity, project, team, and company profiles:

- Is the profile a reliable representation or just a summary of recent facts?
- Does it separate static identity, preferences, current work, recent activity,
  inferred traits, relationships, decisions, and stale items?
- Is there a profile diff/update process?
- Is there support for multiple people and organizations?
- Can teams have shared memory without leaking private user memory?
- Can the profile cite source facts?
- Can users inspect, edit, pin, unpin, or forget profile entries?

For a company brain, profiles should exist at multiple scopes: individual,
team, project, customer/account, document/source, organization, and global
company knowledge.

#### 4. Relationships

Evaluate relationship discovery:

- Entity resolution: people, companies, projects, products, teams, files, links,
  accounts, decisions, events.
- Fact-to-fact relations: duplicate, update, extend, contradict, derived,
  supports, refutes, depends-on, caused-by, belongs-to, mentions, authored-by.
- Entity graph: subject, predicate, object, aliases, identities, canonical IDs.
- Cross-document synthesis: can tomorrow's input link back to today's facts?
- Relationship confidence and provenance.
- Query-time graph traversal and explanation.
- Background relationship discovery, not just ingest-time nearest-neighbor
  relation checks.

Find whether the existing memory lifecycle and temporal graph are enough for a
company brain. If not, propose the minimum graph architecture needed.

#### 5. Temporality

Audit time as a first-class concept:

- captured_at versus event time versus valid_from/valid_to versus learned_at.
- Relative date resolution and time zones.
- Latest/current value resolution.
- Historical questions: "what did we believe then?"
- Temporal aggregation: counts, trends, recency, repeated preferences.
- Scheduled future facts and reminders.
- Changing company knowledge, policies, customer states, and user preferences.

World-class memory needs bitemporal semantics. Be precise about where UltraMem
has this, where it only stores time in text, and where retrieval could still
surface stale information.

#### 6. Forgetting

Audit forgetting beyond delete-by-document:

- Explicit user deletion.
- Expiry of temporary memories.
- Decay of low-value episodes.
- Retention policies by source/scope.
- Privacy-preserving redaction.
- Secret detection and non-memory rules.
- "Forget this fact" versus "delete this source."
- Legal/compliance deletion and audit logs.
- Rebuilding derived facts and profiles after deletion.
- Preventing forgotten data from surviving in summaries, profiles, graph edges,
  caches, benchmarks, and downstream agent logs.

Treat forgetting as a product, legal, and architecture requirement, not a cleanup
function.

#### 7. Retrieval and reasoning

Assess retrieval quality:

- Dense retrieval, sparse/hybrid search, reranking, query rewriting, multi-query.
- Chunk retrieval versus fact retrieval versus graph retrieval.
- Query planning for list/count/time/source questions.
- Whether retrieval returns enough evidence without overloading the model.
- Whether answers can cite sources and explain memory provenance.
- Whether agentic retrieval is needed for complex company-brain questions.
- Whether the current interface is enough for "find links to what I have."

Identify when vector search will fail and propose search-agent, graph traversal,
SQL/metadata filtering, and evaluation strategies.

#### 8. Multi-tenancy, scopes, and permissions

Audit the memory scoping model:

- container tags and namespace isolation.
- API-key-to-tenant enforcement.
- User, team, project, company, source, and agent scopes.
- Shared company memory versus private memory.
- Row-level/source-level permissions.
- Per-connector ACLs.
- Preventing client-supplied namespace escalation.
- Audit logs for memory reads/writes/deletes.

For a company brain, permissions are core memory semantics. A memory that cannot
prove access control is unsafe.

#### 9. Data model and storage

Assess whether Qdrant-only storage is enough:

- Is Qdrant the source of truth or only an index?
- What happens on reindex, provider swaps, schema changes, and deletes?
- Are payloads sufficient for durable memory state?
- Is there a need for Postgres/SQLite/object storage for documents, facts,
  graph edges, jobs, audit logs, users, permissions, connectors, and source
  snapshots?
- Can the system migrate, backfill, replay, and inspect memory deterministically?
- What are the scaling risks of scroll-all operations?

Propose a production data model suitable for personal and company memory.

#### 10. Evaluation

Audit the benchmark story:

- Unit tests versus live integration tests.
- Synthetic corpus difficulty.
- LongMemEval/LongMemBench-style evaluation.
- MemoryBench-style metrics: quality, latency, tokens injected.
- Temporal update tests.
- Forgetting tests.
- Permission isolation tests.
- Relationship discovery tests.
- Source-grounded answer tests.
- Human preference/product-quality evaluation.
- Regression gates for every memory behavior.

Do not accept easy synthetic retrieval as proof of SOTA. Propose an evaluation
suite with hard cases and pass/fail gates.

#### 11. Product and API surface

Evaluate the developer and user-facing product:

- HTTP API completeness.
- MCP server design.
- SDK shape for JS/Python.
- Memory review/edit UI requirements.
- Connector API.
- Webhook/event model.
- Background jobs and status visibility.
- Observability and debugging.
- Explainability: "why did you remember this?"
- Import/export and portability.

For "people can put in everything and anything," define the product surfaces
needed to make ingestion, review, retrieval, and correction easy.

#### 12. Security, privacy, and trust

Audit risks:

- Prompt injection from stored pages/articles/bookmarks.
- Cross-tenant leakage.
- Stored secrets and credentials.
- PII handling.
- Third-party provider exposure.
- Connector tokens.
- Model/provider logs.
- Poisoned memories.
- Malicious source content.
- Conflicting or low-trust sources.
- Enterprise auditability.

Propose trust levels, source ranking, safe extraction, secret scanning, and
policy controls.

### Required output format

Return a structured report with these sections:

1. Executive verdict
   - One paragraph: how close UltraMem is to a world-class memory layer today.

2. Current architecture map
   - Summarize implemented components.
   - Include a simple pipeline diagram in text or Mermaid.
   - Distinguish chunks, facts, profiles, graph, API, providers, and evals.

3. Claim versus reality matrix
   - Table with columns: Claim, Evidence, Status, Risk, What to verify next.
   - Status must be one of: Implemented, Partial, Claimed, Missing, Unclear.

4. Top 20 gaps
   - Ordered by product/architecture impact.
   - Each gap must include: why it matters, evidence, concrete fix, effort,
     risk if ignored.

5. Memory model proposal
   - Propose a first-class memory schema for:
     - facts
     - profiles
     - relationships
     - temporal state
     - source provenance
     - permissions
     - forgetting state
   - Include example records.

6. Company brain architecture
   - Explain how UltraMem should support private user memory, team memory,
     project memory, account/customer memory, source/document memory, and
     company memory.
   - Include permissions and source provenance.

7. State-of-the-art roadmap
   - Phase 0: verification and benchmark hardening.
   - Phase 1: critical memory correctness.
   - Phase 2: source/connectors and company-brain scaffolding.
   - Phase 3: relationship/temporal intelligence.
   - Phase 4: enterprise trust, observability, and review.
   - Phase 5: frontier retrieval and agentic reasoning.
   - For each phase include deliverables, acceptance tests, and expected user
     impact.

8. Evaluation plan
   - Define datasets, adversarial scenarios, metrics, regression tests, and
     release gates.
   - Include tests for facts, profiles, relationships, temporality, forgetting,
     permissions, and cross-source synthesis.

9. Prompt and extraction policy improvements
   - Rewrite or propose improved prompts for:
     - fact extraction
     - relation classification
     - temporal edge extraction
     - profile compilation
     - forgetting/retention classification
     - source trust and provenance extraction
   - Include schemas, validation rules, and failure-handling.

10. Immediate implementation backlog
    - A prioritized backlog of 30 to 50 concrete engineering tasks.
    - Each task should include files likely touched, acceptance criteria, and
      test strategy.

11. Open decisions
    - List product/architecture choices the team must decide before building.

12. Final recommendation
    - The 5 highest-leverage actions to take next.

### Evidence requirements

- Cite local file paths and, when possible, line numbers.
- If you cannot cite a finding, label it as an inference.
- Separate "docs say" from "code does."
- Do not quote external claims unless you can verify them.
- If you use web research to compare against Zep, Mem0, Supermemory, Graphiti,
  ChatGPT, Claude, or other systems, cite sources and mark dated claims.

### Severity rubric

Use this severity scale for gaps:

- P0: Blocks safe or correct production use.
- P1: Blocks world-class memory quality or company-brain fit.
- P2: Important improvement but not a core blocker.
- P3: Polish, ergonomics, or later optimization.

### Definition of "world-class memory"

Use this target definition throughout:

- It remembers durable facts, preferences, decisions, relationships, events, and
  source-grounded claims.
- It tracks time explicitly and can answer current and historical questions.
- It discovers relationships across sources after the fact.
- It explains every memory with evidence and source provenance.
- It respects scopes, permissions, and privacy.
- It can forget cleanly and prove what was forgotten.
- It has human review and correction paths.
- It retrieves exact links, articles, bookmarks, files, people, projects, and
  facts with high precision.
- It handles conflicting sources and changing truths.
- It measures itself with hard benchmarks, not toy examples.
- It supports personal assistants and company brains without leaking private
  memory into shared contexts.

## Focused Follow-Up Prompts

Use these after the super prompt if you want Fable 5 to go deeper on a specific
axis.

### Facts Prompt

Audit UltraMem's fact model and extraction pipeline only. Design a production
schema for typed memories with evidence spans, confidence, source provenance,
scope, permissions, temporal fields, review state, and forgetting metadata.
Identify every place the current implementation stores a flat string where a
typed object is needed. Return proposed Rust structs, JSON API shapes, Qdrant
payload fields, database tables, and tests.

### Profiles Prompt

Audit UltraMem's profile compiler. Design a multi-scope profile system for
individuals, teams, projects, customers/accounts, documents, and company-level
memory. Include profile entry provenance, confidence, editability, profile diffs,
cache invalidation, stale profile prevention, and answer-time injection policy.
Return a migration plan from the current static/dynamic profile output.

### Relationships Prompt

Audit UltraMem's relationship and graph layers. Design relationship discovery
that works across documents and over time: entity resolution, aliases, fact
links, source links, people/project/company graphs, relationship confidence,
background re-linking, graph traversal retrieval, and explanations. Compare the
current nearest-neighbor UPDATE/EXTEND logic and temporal graph to the proposed
target.

### Temporality Prompt

Audit UltraMem's temporal semantics. Design bitemporal memory handling with
event time, transaction time, valid intervals, learned_at, observed_at,
source_published_at, current value resolution, historical queries, future events,
recurrence, decay, and time-zone handling. Return concrete schema changes and
tests for "latest", "as of", "between", "how many times", and "what changed".

### Forgetting Prompt

Audit UltraMem's forgetting model. Design explicit delete, fact-level forget,
source-level delete, expiry, decay, retention policies, redaction, secret
avoidance, profile/derived-memory invalidation, graph-edge cleanup, cache purge,
audit logs, and compliance deletion. Return an implementation plan and tests that
prove forgotten data cannot reappear in search, profiles, graph context, derived
facts, or logs.

### Company Brain Prompt

Design UltraMem as a company brain. Include connector architecture, source
snapshots, ACL ingestion, team/project/account scopes, memory promotion from
private to shared, governance, review queues, trust ranking, conflict resolution,
audit logs, enterprise search, answer citations, and admin controls. Return a
reference architecture, API surface, data model, and V1/V2 roadmap.

### Evaluation Prompt

Design the benchmark suite required before claiming UltraMem is state of the art.
Include LongMemEval-style memory tests, hard retrieval corpora, temporal updates,
forgetting, permissions, source-grounded QA, relationship discovery, profile
correctness, link/bookmark retrieval, poisoned memory, prompt injection, and
company-brain scenarios. Define metrics, datasets, CI gates, manual review loops,
and sample test cases.

