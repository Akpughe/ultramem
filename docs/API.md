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
  "memories": [ "distilled fact (latest, non-expired)", … ]
}
```
Maps to `MemoryEngine::retrieve_tagged` (planner + multi-query + rerank + `is_latest`/`valid_until` filtering).

> **Field casing:** the top-level keys are snake_case (`documents`, `memories`), but each document object inside `documents` is serialized **camelCase** — `documentId`, `title`, `metadata`, `chunks`, and within `metadata`: `capturedAt`, `source`, `reference`. See the worked example below for the exact shape.

### `GET /v1/profile?container_tag=…` — standing profile
→ `{ "static": "…bullets…", "dynamic": "…recent…" }`. Maps to `profile_tagged`. Inject into an agent's system prompt every turn.

### `GET /v1/timeline?container_tag=…&source=…&before=…&limit=60` — enumeration
Complete newest-first list (not similarity top-K) for "what did I do this week". Backed by the new `list_document_ids` scroll (see EXTRACTION §3).

### `POST /v1/reindex` — reprocess without re-extraction
```jsonc
{ "container_tag": "string?", "mode": "tags|latest|facts" }
```
→ `{ "ok": true, "mode": "…" }`, or for `mode=facts` `{ "ok": true, "mode": "facts", "total": N, "status": "running" }`. Reuses stored chunk text. Maps to `claim_legacy_into_tag` / `backfill_facts_latest` / `reindex_doc_facts`.

> **Planned, not implemented:** a persisted job record and progress endpoints
> (`GET /v1/jobs/:id`, SSE `GET /v1/jobs/:id/stream`). Today `mode=facts` runs as a
> detached background task with no status/cancellation surface.

### `DELETE /v1/memories/:document_id?container_tag=…` — forget
Removes the document's chunks + facts **within the caller's namespace only**. A document in
another tenant's namespace returns `404` (and is not touched). A `container_tag` the
credential doesn't own returns `403`. Maps to `delete_document_tagged`.

### `GET /v1/health`
→ `{ "ok": true }` (no auth). Maps to `MemoryEngine::health` (Qdrant reachability + a
provider-key presence check).

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
