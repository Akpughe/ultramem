# UltraMem MCP Server

Separate repo: **`ultramem-mcp`**. Exposes the memory layer to any MCP client (Claude Code/Desktop, Cursor, etc.) so an agent gains durable, cross-session memory by adding one server.

Base it on Recally's existing `src-tauri/src/bin/recally_mcp.rs` (already a working stdio MCP server with `recall_search` + `recall_timeline`). Two ways to back it:

- **Thin mode (recommended v1):** the MCP server is a thin client of `ultramem-server`'s HTTP API. Keeps one source of truth, language-flexible (could even be a TS MCP server). Config: `ULTRAMEME_API_URL`, `ULTRAMEME_API_KEY`, `ULTRAMEME_CONTAINER_TAG`.
- **Embedded mode:** link `ultramem-core` directly and talk to Qdrant. Fewer moving parts for single-user local use; no server needed.

## Tools

### `recall_search`
Find relevant memories in natural language.
```jsonc
{ "query": "string", "limit": 8, "container_tag": "string?" }
```
Returns numbered documents + distilled facts (cite as `[n]`). → `POST /v1/search`.

### `recall_timeline`
Complete newest-first enumeration over a recent window (for "what did I … this week"). → `GET /v1/timeline`.
```jsonc
{ "source": "clipboard|browser|file|meeting?", "days": 7, "limit": 60, "container_tag": "string?" }
```

### `add_memory`
Let the agent write its own episodic memory back (task outcomes, user statements). Flows through the same lifecycle.
```jsonc
{ "content": "string", "title": "string?", "container_tag": "string?" }
```
→ `POST /v1/memories`.

### `get_profile`
Fetch the standing static+dynamic profile to inject into the agent's context.
```jsonc
{ "container_tag": "string?" }
```
→ `GET /v1/profile`.

## The agent pattern
On session start, call `get_profile` → prepend to the system prompt ("what you always know about the user"). During the task, `recall_search` for specifics and `add_memory` to persist outcomes. This is exactly the SuperMemory "always-known context" trick, now self-hostable.

## Packaging
- Publish to npm (if TS) / crates.io (if Rust) and as a one-line `claude mcp add` / Cursor config snippet.
- Ship a `README` with the 30-second setup and a security note (the API key scopes the namespace; never share it).
