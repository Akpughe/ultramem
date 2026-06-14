# UltraMem HTTP API (v1)

`ultramem-server` (axum) wraps `ultramem-core`. Multi-tenant: every request is scoped to a `container_tag` (namespace) — one per user or per agent. Auth via `Authorization: Bearer <API_KEY>`.

Design goal: mirror SuperMemory's surface so it's a drop-in mental model, backed by our engine.

## Auth & namespaces

- `Authorization: Bearer <key>` on every endpoint. Keys map to tenants/projects (configurable; simplest v1 = one static key from env).
- `container_tag` in the body/query selects the memory pool. Omit → default pool. The server should derive/enforce the tag from the API key in true multi-tenant mode, so a client can't read another tenant's namespace (mirror the CSRF/verification discipline already in Recally's auth).

## Endpoints

### `POST /v1/memories` — ingest
Body (JSON, or `multipart/form-data` for files):
```jsonc
{
  "content": "string",           // text/markdown, OR
  "url": "https://…",            // fetch + clean (Jina Reader), OR (multipart) a file part
  "title": "string?",
  "source": "clipboard|browser|file|meeting|api",
  "reference": "string?",        // canonical id/url
  "container_tag": "string?",
  "captured_at": 1760000000,      // unix; default now
  "metadata": { }
}
```
→ `{ "document_id": "uuid", "status": "done" }`. Returning `done` means chunks are searchable; fact distillation + lifecycle run inline (non-fatal). Maps to `MemoryEngine::add_document`.

### `POST /v1/search` — hybrid retrieve
```jsonc
{
  "query": "string",
  "container_tag": "string?",
  "limit": 8,
  "source": "browser?",          // optional filter
  "after": 1760000000, "before": 1760600000,
  "rerank": true,                 // default true
  "mode": "hybrid|dense"          // hybrid = dense+sparse RRF (needs hybrid collection)
}
```
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
→ `{ "job_id": "…", "total": 0 }` (async). Reuses stored chunk text. Maps to `claim_legacy_into_tag` / `backfill_facts_latest` / `reindex_memory_graph`. Progress via SSE `GET /v1/jobs/:id/stream` or polling `GET /v1/jobs/:id`.

### `DELETE /v1/memories/:document_id` — forget
Removes chunks + facts. Maps to `delete_document`.

### `GET /v1/health`
→ `{ "ok": true, "qdrant": true, "embeddings": true }`. Maps to `MemoryEngine::health`.

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
