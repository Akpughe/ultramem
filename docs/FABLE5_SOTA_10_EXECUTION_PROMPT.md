# Fable 5 SOTA 10/10 Execution Prompt

Use this after the deep audit. The goal is to convert the audit's 6/10
early-stage score and 3.5/10 company-brain score into a careful execution plan
that can move UltraMem toward a genuine 10/10 state-of-the-art memory layer.

## Prompt

You are Fable 5 acting as the principal architect and execution planner for
UltraMem. You already audited the repo and concluded:

- UltraMem is about **6/10** against a promising early-stage OSS memory engine bar.
- UltraMem is about **3.5/10** against the bar of a world-class memory layer for
  company brains.
- The repo has strong retrieval bones, but the memory layer is still shallow.
- The highest-risk issues are security, tenancy, evidence, correctness, deletion,
  temporality, typed facts, evaluation, and source-of-truth storage.

Your job now is not to perform another broad audit. Your job is to produce the
implementation plan that gets UltraMem to a credible **10/10**. Be disciplined:
do not chase frontier graph or agentic retrieval until the system is safe,
testable, and truthful.

## North Star

UltraMem should become a world-class memory layer that can power:

- personal assistants with durable, editable memory;
- AI agents that remember across sessions;
- company brains with private, team, project, account, source, and company scopes;
- ingestion of links, articles, Twitter/X bookmarks, PDFs, images, meetings,
  notes, files, messages, web pages, code/docs, and arbitrary context;
- relationship discovery across time;
- temporal reasoning over changing facts;
- clean, provable forgetting.

The system is only SOTA if it can prove all of this with tests, benchmarks,
citations, permissions, and user-visible controls.

## Prime Directive

Build toward SOTA in this order:

1. **Safe**
2. **Correct**
3. **Measurable**
4. **Typed and provenance-grounded**
5. **Temporal**
6. **Scoped and permission-aware**
7. **Relationship-rich**
8. **Usable as a product**
9. **Scalable**
10. **Frontier-quality retrieval/reasoning**

If a proposed task improves retrieval but leaves unsafe tenancy, untestable
claims, or broken forgetting, it is not next.

## Required Output

Produce a concrete execution plan with these sections.

### 1. The Honest Target

Define what "10/10 UltraMem" means in measurable terms.

Include minimum bars for:

- security and tenancy;
- memory correctness;
- fact typing and provenance;
- relationship discovery;
- temporal reasoning;
- forgetting;
- source coverage;
- company-brain permissions;
- evaluation quality;
- developer API and product UX;
- observability and operations.

Do not use vague phrases like "better memory." Define pass/fail behavior.

### 2. Score Ladder

Create a ladder from the current state to 10/10:

- 4/10: safe local/personal prototype;
- 5/10: tested personal memory engine;
- 6/10: production-safe single-user/personal memory service;
- 7/10: typed, provenance-grounded memory with real forgetting;
- 8/10: multi-scope team/project/company memory with permissions;
- 9/10: temporal and relationship-rich company brain;
- 10/10: SOTA, benchmarked, explainable, enterprise-grade memory/reasoning layer.

For each rung include:

- what must be true;
- what tests must pass;
- what product behavior users can rely on;
- what still remains missing.

### 3. Stop-Ship List

List the issues that block any hosted or multi-user deployment. Start from the
audit's P0s:

- shared static API key with client-controlled `container_tag`;
- unscoped delete;
- arbitrary server-file read via `file_path`;
- no secret/PII screen before embedding;
- prompt-injection path into durable facts and profiles.

For each, specify:

- exact fix;
- likely files touched;
- acceptance criteria;
- tests;
- "done means" statement.

### 4. First 30 Days

Give a day-by-day or week-by-week plan for the first month.

The first month must prioritize:

- key-to-tenant binding or scoped auth claims;
- tag-scoped delete;
- removal or sandboxing of network `file_path`;
- secret/PII screening before embed;
- prompt-injection hardening for extraction/profile;
- cascade delete and profile invalidation;
- offline mock-backed lifecycle tests;
- CI;
- absence assertions for superseded facts;
- fixing broken benchmark metrics;
- aligning docs with code.

For every task, include:

- owner type: backend, infra, eval, product, security, docs;
- files likely touched;
- acceptance criteria;
- unit tests;
- live/integration tests;
- user-facing impact.

### 5. 90-Day Roadmap

Design the 90-day plan that can take UltraMem from promising OSS engine to a
credible production memory platform.

Include phases for:

- Postgres or another relational source of truth;
- object storage for original source snapshots;
- document registry and processing jobs;
- typed memory records;
- evidence spans and citations;
- source-level provenance;
- retention/forgetting state machine;
- scopes and ACLs;
- memory review/edit/pin/forget APIs;
- connector foundations;
- temporal graph in production;
- entity resolution and aliasing;
- hard evaluation suite.

For each phase include:

- deliverables;
- schema/API changes;
- migration strategy from current Qdrant-only state;
- tests and release gates;
- risk and fallback.

### 6. Typed Memory Architecture

Turn the audit's proposed typed memory record into an implementation plan.

Design:

- `documents`;
- `chunks`;
- `memories`;
- `memory_evidence`;
- `memory_edges`;
- `entities`;
- `entity_aliases`;
- `sources`;
- `scopes`;
- `acl_entries`;
- `jobs`;
- `audit_events`;
- `forget_events`;
- `profile_entries`.

For each table or record, specify:

- purpose;
- key fields;
- indexes;
- relation to Qdrant;
- migration path from current payloads;
- tests.

Explain exactly what stays in Qdrant and what moves to the source-of-truth store.

### 7. Facts, Profiles, Relationships, Temporality, Forgetting

Create detailed implementation tracks for the five memory pillars.

#### Facts

Define:

- memory kinds;
- extraction schema;
- evidence quote/span;
- confidence model;
- review states;
- update/duplicate/extend/contradiction handling;
- source trust;
- tests.

#### Profiles

Define:

- personal profile;
- team profile;
- project profile;
- customer/account profile;
- company profile;
- profile entry citations;
- profile diffs;
- stale profile prevention;
- edit/pin/reject behavior;
- tests.

#### Relationships

Define:

- entity resolution;
- alias merging;
- fact-to-fact relationships;
- source-to-memory relationships;
- background relationship discovery;
- relationship confidence;
- graph traversal retrieval;
- tests.

#### Temporality

Define:

- captured_at;
- learned_at;
- source_published_at;
- event_time;
- valid_from;
- valid_to;
- transaction time;
- "current" resolution;
- "as of" resolution;
- time-window retrieval/counting;
- tests.

#### Forgetting

Define:

- source delete;
- memory delete;
- fact-level forget;
- expiry;
- decay;
- redaction;
- derived memory invalidation;
- profile invalidation;
- graph cleanup;
- audit proof;
- tests that prove forgotten data cannot reappear.

### 8. SOTA Evaluation Suite

Design the benchmark suite that UltraMem must pass before anyone can honestly
say "state of the art."

Include:

- LongMemEval-style suite;
- hard retrieval corpus with near-duplicate distractors;
- contradiction chains;
- temporal "as of" and "latest" questions;
- forgetting tests;
- permission-leak tests;
- prompt-injection and poisoned-memory tests;
- entity-resolution tests;
- relationship-discovery tests;
- profile-correctness tests;
- source-grounded QA;
- link/bookmark retrieval;
- company-brain scenarios.

For every benchmark, define:

- dataset shape;
- pass/fail metric;
- target score for 7/10, 8/10, 9/10, and 10/10;
- CI gate or nightly gate;
- how to avoid benchmark theater.

### 9. Product Surfaces

Define the product experiences needed for world-class memory:

- memory inbox/review queue;
- source browser;
- profile editor;
- "why do you remember this?";
- "forget this";
- "promote to team/company memory";
- connector setup and permission review;
- memory timeline;
- relationship map;
- admin audit view;
- import/export;
- developer SDKs;
- MCP tools.

For each, include:

- user story;
- backend requirements;
- API endpoints;
- acceptance criteria.

### 10. Implementation Backlog

Return a prioritized backlog of at least 60 tasks.

Each task must include:

- priority: P0, P1, P2, P3;
- score rung it unlocks;
- short title;
- description;
- files/modules likely touched;
- dependencies;
- acceptance tests;
- estimated complexity: S, M, L, XL.

Make the backlog sequenced. Do not put a Phase 5 graph-agent task before the
Phase 0 safety foundation.

### 11. Concrete Agent Instructions

Write the exact prompt we should give to an implementation coding agent for the
first execution sprint.

The sprint prompt must:

- be limited to a coherent slice, not the whole roadmap;
- target the P0 stop-ship issues and tests;
- name the files to inspect first;
- require tests;
- require docs alignment;
- prohibit broad refactors;
- define done criteria.

### 12. Final Answer

End with:

- the immediate next 5 tasks;
- the next 5 tests to add;
- the first schema migration to design;
- the first product surface to prototype;
- the one thing we should **not** build yet, even if it is tempting.

## Constraints

- Be honest about what the current repo can support.
- Treat docs claims as untrusted until verified against code and tests.
- Prefer sequenced execution over speculative architecture.
- Do not call something SOTA unless it is benchmarked against hard cases.
- Make forgetting, permissions, and provenance first-class.
- Company-brain memory must never leak private user memory into shared contexts.
- Every major recommendation must have acceptance criteria.

