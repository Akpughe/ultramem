# UltraMem HTTP API (v1)

`ultramem-server` (axum) wraps `ultramem-core`. Multi-tenant: every request is scoped to a `container_tag` (namespace) — one per user or per agent. Auth via `Authorization: Bearer <API_KEY>`.

Design goal: mirror SuperMemory's surface so it's a drop-in mental model, backed by our engine.

## Auth & namespaces

- `Authorization: Bearer <key>` on every protected endpoint. A missing/unknown key → `401`.
- **Credentials are bound to namespaces (enforced server-side).** Each key resolves to a
  *tag policy*; a request may only act on a `container_tag` the credential is allowed to use.
  Naming a tag outside that set → `403`. This closes the earlier hole where any key-holder
  could read/write/delete any tenant by changing the `container_tag` string.
  - `ULTRAMEM_TENANTS="keyA=tenant_a,shared; keyB=*"` — bind keys to tags. A tag of `*`
    means "any tag" (a trusted backend that manages its own per-user tags).
  - `ULTRAMEM_API_KEY=<key>` — a single key; treated as wildcard (`*`) for backward
    compatibility with the single-key/many-tags quickstart. Prefer `ULTRAMEM_TENANTS` to
    bind it to specific tags.
  - `ULTRAMEM_DEV=1` — run **unauthenticated** with no keys (local development only). With
    no keys and no `ULTRAMEM_DEV=1`, the server refuses to start.
- `container_tag` in the body/query selects the memory pool *within the credential's allowed
  set*. Omit → the credential's default tag.
- This is an intentional seam: a later version replaces the env-based policy with JWT claims
  or a tenant table without changing the endpoints.

## Endpoints

### `POST /v1/memories` — ingest
The endpoint dispatches on `Content-Type`. Provide exactly one source of content.

**JSON** (`application/json`) — one of `content` or `url`:
```jsonc
{
  "content": "string",           // raw text/markdown, OR
  "url": "https://…",            // fetch + clean the page (Jina Reader)
  "title": "string?",
  "source": "clipboard|browser|file|meeting|api|web",
  "reference": "string?",        // canonical id/url
  "container_tag": "string?",
  "captured_at": 1760000000       // unix; default now
}
```

> **`file_path` is rejected over the network** (returns `400`) — reading an arbitrary
> server-side path is a file-disclosure risk. To ingest a file from a client, use the
> multipart upload below. Local file ingestion remains available through the embedded Rust
> engine API (`MemoryEngine::add_document`), not the HTTP API.

**File upload** (`multipart/form-data`) — to send a file's bytes from a client.
A `file` part (PDF/image/Office/text — OCR'd or text-extracted server-side via
Mistral/Jina Reader) plus optional text fields `title`, `source`, `reference`,
`container_tag`, `captured_at`:
```
file=@report.pdf  title=Q3 report  container_tag=user_123
```
Upload size cap: 32 MB.

→ `{ "document_id": "uuid", "status": "done" }` (any mode). Returning `done` means chunks are searchable; fact distillation + lifecycle run inline (non-fatal). Maps to `MemoryEngine::add_document` (text/file) / `add_url` (url). Distillation only runs for content longer than ~280 chars.

### `POST /v1/search` — retrieve
```jsonc
{
  "query": "string",
  "container_tag": "string?",
  "limit": 8
}
```

> **Implemented fields today are `query`, `container_tag`, `limit`.** The planner still
> resolves source/date/list intent *from the query text*, but the following request-level
> filters are **planned, not yet wired** into `SearchBody`: `source`, `after`, `before`,
> `rerank`, `mode` (dense vs. hybrid). Sending them is accepted but ignored.

→
```jsonc
{
  "documents": [
    { "document_id": "…", "title": "…", "score": 0.0,
      "chunks": [ { "content": "…", "score": 0.0 } ],
      "metadata": { "source": "…", "reference": "…", "captured_at": 0 } }
  ],
  "memories": [ "distilled fact (latest, non-expired)", … ],
  "provenance": [
    { "statement": "…", "kind": "preference", "confidence": 0.9,
      "evidence": [ { "quote": "…verbatim source span…", "documentId": "…", "chunkId": "…" } ] }
  ]
}
```

> **`provenance`** (Phase A) enriches each returned memory with its `kind`, `confidence`, and
> grounded `evidence` (verbatim source quotes + their document/chunk), joined from the relational
> source of truth by statement. It is **additive** — `memories` stays a plain string array — and is
> **empty unless `ULTRAMEM_PG_URL` is configured**.
Maps to `MemoryEngine::retrieve_tagged` (planner + multi-query + rerank + `is_latest`/`valid_until` filtering).

> **Field casing:** the top-level keys are snake_case (`documents`, `memories`), but each document object inside `documents` is serialized **camelCase** — `documentId`, `title`, `metadata`, `chunks`, and within `metadata`: `capturedAt`, `source`, `reference`. See the worked example below for the exact shape.

### `GET /v1/profile?container_tag=…` — standing profile
→ `{ "static": "…bullets…", "dynamic": "…recent…" }`. Maps to `profile_tagged`. Inject into an agent's system prompt every turn.

### `GET /v1/timeline?container_tag=…&source=…&before=…&limit=60` — enumeration
Complete newest-first list (not similarity top-K) for "what did I do this week". Backed by the new `list_document_ids` scroll (see EXTRACTION §3).

### Entity resolution — canonical entities (requires Postgres)
Unify surface forms of an entity within a namespace. Resolution is **explicit**: only
registered aliases unify, and an unknown name resolves to itself (never an invented merge).
Aliases are stored normalized (case/whitespace-folded), so lookups are variant-insensitive.

- `POST /v1/entities/alias` `{ "alias": "J. Smith", "canonical": "Jane A. Smith" }`
  → `{ "ok": true }`. Re-registering an alias updates its canonical.
- `GET /v1/entities/resolve?name=j.%20smith` → `{ "name": "j. smith", "canonical": "Jane A. Smith" }`.
- `GET /v1/entities/aliases?container_tag=…` → `{ "aliases": [ { alias, canonical, created_at } ] }`.

### `GET /v1/memories/as_of?t=…&container_tag=…&limit=200` — point-in-time recall (requires Postgres)
Bitemporal read: returns the memories that were **current knowledge as of transaction
time `t`** (unix seconds) — learned by then, not yet superseded as of `t`, still valid in
the world at `t`, and not quarantined. Answers *"what did we know as of `t`"*, not just the
present, so a contradicted fact still surfaces for a time before it was corrected. Each item
is `{ statement, kind, confidence, learned_at }`. Reads from the Postgres source of truth
(empty without it); the live vector search stays current-only.

### `POST /v1/reindex` — reprocess without re-extraction
```jsonc
{ "container_tag": "string?", "mode": "tags|latest|facts" }
```
→ `{ "ok": true, "mode": "…" }`, or for `mode=facts` `{ "ok": true, "mode": "facts", "total": N, "status": "running" }`. Reuses stored chunk text. Maps to `claim_legacy_into_tag` / `backfill_facts_latest` / `reindex_doc_facts`.

When a Db is configured, `mode=facts` returns a `job_id`; poll it at
`GET /v1/jobs/:id?container_tag=…` → `{ id, state: "queued|running|done|failed",
progress, total, error, … }`. Without Postgres, `mode=facts` still runs as a detached
task and `job_id` is `null`.

> **Planned, not implemented:** an SSE progress stream (`GET /v1/jobs/:id/stream`) and
> job cancellation.

### `DELETE /v1/memories/:document_id?container_tag=…` — forget
Removes the document's chunks + facts **within the caller's namespace only**. A document in
another tenant's namespace returns `404` (and is not touched). A `container_tag` the
credential doesn't own returns `403`. Maps to `delete_document_tagged`.

### ACL admin — company-brain scopes (requires Postgres)
Grants let a principal read another **scope** (a `container_tag`) beyond its own. Search
then spans the caller's own scope **plus** any it has been granted read (or higher) on —
fail-closed: with no grants, behavior is identical to single-namespace isolation. You may
only administer a scope you already control (a credential bound to it, or a wildcard
backend); administering a scope your credential can't act as returns `403`.

- `POST /v1/acl/grant` `{ "principal": "user_a", "scope": "team_eng", "capability": "read" }`
  → `{ "ok": true }`. `capability` ∈ `read | write | promote | admin` (higher implies read);
  an unknown capability is `400`.
- `POST /v1/acl/revoke` `{ "principal": "user_a", "scope": "team_eng", "capability": "read" }`
  → `{ "ok": true }` (idempotent).
- `GET /v1/acl?scope=team_eng` → `{ "grants": [ { principal, scope, capability, created_at } ] }`
  — who may access the scope.

### `DELETE /v1/facts/:id?container_tag=…` — forget one fact (requires Postgres)
Fact-granular **right-to-erasure**: hard-removes a single distilled memory (and its
evidence) from **both** the vector index and the relational source of truth, scoped to
the caller's namespace. Ownership is verified against Postgres first, so a fact in another
tenant's namespace returns `404` and is untouched — never a cross-tenant erasure. The
searchable vector is erased before the relational row, so a forgotten fact can't be
resurrected by a later index rebuild, and a mid-way failure is retry-safe. This is the
fact-level counterpart to document-level `DELETE /v1/memories/:id`.

### `GET /v1/export?container_tag=…` — data portability (requires Postgres)
Export everything held about a namespace — its `documents` and distilled `memories` —
from the source of truth, scoped to the caller. The portability counterpart to
`DELETE /v1/facts/:id` (erasure): hand a user everything you know about them, then forget
it. Bounded (up to 100k of each); the operation is audited. Returns
`{ container_tag, documents: [ { id, source, title, reference, captured_at } ], memories: [ { id, kind, statement, confidence, is_latest, document_id, learned_at } ] }`.

### `POST /v1/memories/:id/promote` — share into a company scope (requires Postgres)
Copy a memory from the caller's own scope into a shared **scope** it holds the `promote`
(or `admin`) capability on. Body: `{ "to_scope": "team_eng", "container_tag": "user_a" }`
(the `container_tag` is the caller's own namespace; defaults per credential). The fact is
re-embedded into the shared namespace and provenance links back to the origin memory
(`extends`) and its source document. Returns `{ "ok": true, "id": "<new>", "scope": "team_eng" }`.
`403` without a `promote`/`admin` grant on `to_scope`; `404` if the memory isn't in the
caller's scope. `read`/`write` grants do **not** authorize promotion — writing into a shared
brain is a higher bar than reading it.

### `GET /v1/health`
→ `{ "ok": true }` (no auth). Maps to `MemoryEngine::health` (Qdrant reachability + a
provider-key presence check).

## Source of truth (Postgres, Phase A)

Set `ULTRAMEM_PG_URL` to run Postgres as the relational **source of truth**. When
configured, the engine additionally:

- **dual-writes** each ingest — `documents`, `chunks`, typed `memories`
  (kind/confidence), and grounded `memory_evidence` (validated verbatim source
  spans) — and **dedups** documents by content hash / canonical URL;
- serves `/v1/timeline` and search `provenance` from indexed Postgres queries
  (no full-collection scans);
- tracks background work as `jobs` (`GET /v1/jobs/:id`) and records an
  `audit_events` trail of mutating operations;
- treats **Qdrant as a rebuildable index**: `POST /v1/reindex {"mode":"backfill"}`
  migrates existing Qdrant data into Postgres, and `{"mode":"rebuild"}`
  regenerates Qdrant from Postgres — so losing Qdrant is recoverable, not data loss.

With `ULTRAMEM_PG_URL` **unset**, the engine runs Qdrant-only exactly as before.
Set `ULTRAMEM_PG_REQUIRED=1` to make the server refuse to start (rather than
silently fall back to Qdrant-only) if Postgres can't be attached — the recommended
production posture.

## Provider config (env)
`QDRANT_URL`, `QDRANT_API_KEY`, `JINA_API_KEY`, `MISTRAL_API_KEY`, and the LLM provider keys (Groq/OpenAI/Anthropic/Ollama via `llm.rs`). Once the provider traits land (ROADMAP Phase 3) these become swappable per-deployment.

## Worked examples (verified end-to-end)

Captured live against `cargo run -p ultramem-server` + Qdrant. `KEY` is your `ULTRAMEM_API_KEY`; every protected call sends `Authorization: Bearer $KEY`. Distillation only runs on documents over ~280 characters — shorter snippets are stored and searchable as chunks but produce no `memories`/profile facts.

**Add** — `POST /v1/memories`
```bash
curl -sX POST localhost:8080/v1/memories -H "Authorization: Bearer $KEY" \
  -H 'content-type: application/json' -d '{
    "content": "Personal preferences note. The user ships Rust every day and prefers it over Go and Python for backend work. For running, they switched entirely from Adidas to Puma — Puma is now their preferred brand. They live in Cape Town and train for marathons.",
    "title": "User preferences",
    "container_tag": "user_123"
  }'
# → {"document_id":"db0eb2a4-7888-4d1f-a12f-7183f295bd31","status":"done"}
```

**Upload a file** — `POST /v1/memories` (multipart; PDF/image/Office/text)
```bash
curl -sX POST localhost:8080/v1/memories -H "Authorization: Bearer $KEY" \
  -F "file=@report.pdf" -F "title=Q3 report" -F "container_tag=user_123"
# → {"document_id":"ba70cf15-…","status":"done"}   # PDF/image → Mistral OCR; text/Office → extracted
```

**Ingest a URL** — `POST /v1/memories` (fetched + cleaned via Jina Reader)
```bash
curl -sX POST localhost:8080/v1/memories -H "Authorization: Bearer $KEY" \
  -H 'content-type: application/json' \
  -d '{"url":"https://example.com/article","container_tag":"user_123"}'
# → {"document_id":"522c61ef-…","status":"done"}   # source recorded as "web"
```

**Search** — `POST /v1/search` (returns the document *and* distilled facts)
```bash
curl -sX POST localhost:8080/v1/search -H "Authorization: Bearer $KEY" \
  -H 'content-type: application/json' \
  -d '{"query":"what running shoes and language does the user prefer?","container_tag":"user_123"}'
```
```jsonc
{
  "documents": [
    {
      "chunks": [ { "content": "Personal preferences note. The user ships Rust …", "score": 0.5868428 } ],
      "documentId": "db0eb2a4-7888-4d1f-a12f-7183f295bd31",
      "title": "User preferences",
      "metadata": { "source": "api", "reference": "", "capturedAt": 1781463180, "app": "" }
    }
  ],
  "memories": [
    "Puma is the user's current and preferred running shoe brand",
    "The user has switched entirely away from Adidas for running shoes",
    "The user only wears Puma running shoes going forward"
  ]
}
```

**Profile** — `GET /v1/profile`
```bash
curl -s "localhost:8080/v1/profile?container_tag=user_123" -H "Authorization: Bearer $KEY"
```
```jsonc
{
  "static":  "- Prefers Rust over Go and Python for backend development\n- Uses Puma as the exclusive running shoe brand\n- Resides in Cape Town\n- …",
  "dynamic": "- Shipping Rust code daily, building on five years of professional experience.\n- Switched exclusively to Puma running shoes, abandoning Adidas.\n- …"
}
```

**Timeline** — `GET /v1/timeline`
```bash
curl -s "localhost:8080/v1/timeline?container_tag=user_123&limit=60" -H "Authorization: Bearer $KEY"
# → {"items":[{"document_id":"db0eb2a4-…","title":"User preferences","source":"api","reference":"","captured_at":1781463180}]}
```

**Reindex** — `POST /v1/reindex` (`mode`: `tags` | `latest` | `facts`; `facts` re-distills from stored chunk text, async)
```bash
curl -sX POST localhost:8080/v1/reindex -H "Authorization: Bearer $KEY" \
  -H 'content-type: application/json' -d '{"container_tag":"user_123","mode":"facts"}'
# → {"ok":true,"mode":"facts","total":1,"status":"running"}
```

**Delete** — `DELETE /v1/memories/:id` (removes chunks + facts)
```bash
curl -sX DELETE "localhost:8080/v1/memories/db0eb2a4-7888-4d1f-a12f-7183f295bd31" -H "Authorization: Bearer $KEY"
# → {"ok":true}
```

**Health** — `GET /v1/health` (no auth)
```bash
curl -s localhost:8080/v1/health   # → {"ok":true}
```

A request to a protected endpoint with a missing/invalid key returns `401` with `{"error":"invalid or missing API key"}`.

## SDK surface (later)
Thin clients (`ultramem-js`, `ultramem-py`) over this API — `add()`, `search()`, `profile()`, `reindex()` — matching SuperMemory's SDK ergonomics.
