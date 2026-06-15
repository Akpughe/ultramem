//! UltraMem HTTP API — a thin axum server over `ultramem-core`. See
//! `docs/API.md`. Multi-tenant via `container_tag`; Bearer-key auth.
//!
//! Run:  ULTRAMEM_API_KEY=… QDRANT_URL=… JINA_API_KEY=… GROQ_API_KEY=… \
//!       cargo run -p ultramem-server   # listens on :8080 (PORT overrides)

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Query, Request, State},
    http::{header::CONTENT_TYPE, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use ultramem_core::{EngineCfg, IngestDoc, MemoryEngine, DEFAULT_TAG};

struct AppState {
    engine: MemoryEngine,
    api_key: String,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let api_key = std::env::var("ULTRAMEM_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        eprintln!("[ultramem] WARNING: ULTRAMEM_API_KEY is empty — the API is UNAUTHENTICATED.");
    }
    let engine = MemoryEngine::new(EngineCfg::from_env());
    // Qdrant may still be starting (e.g. `docker compose up` boots both at once),
    // so retry rather than give up — collections must exist before the first write.
    let mut ensured = false;
    for attempt in 1..=15 {
        match engine.ensure_collections().await {
            Ok(()) => {
                ensured = true;
                break;
            }
            Err(e) => {
                eprintln!("[ultramem] waiting for Qdrant (attempt {attempt}/15): {e}");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
    if !ensured {
        eprintln!("[ultramem] WARNING: could not ensure Qdrant collections after retries — writes will fail until Qdrant is reachable.");
    }
    let state = Arc::new(AppState { engine, api_key });

    let protected = Router::new()
        .route(
            "/v1/memories",
            // 32 MB cap for file uploads (default body limit is 2 MB).
            post(add_memory).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/v1/memories/:id", axum::routing::delete(delete_memory))
        .route("/v1/search", post(search))
        .route("/v1/profile", get(profile))
        .route("/v1/timeline", get(timeline))
        .route("/v1/reindex", post(reindex))
        .layer(middleware::from_fn_with_state(state.clone(), auth));

    let app = Router::new()
        .route("/v1/health", get(health))
        .merge(protected)
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind");
    println!("[ultramem] listening on http://0.0.0.0:{port}");
    axum::serve(listener, app).await.expect("serve");
}

/// Bearer-key gate. When `ULTRAMEM_API_KEY` is set, every protected request must
/// present `Authorization: Bearer <key>`.
async fn auth(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    if state.api_key.is_empty() {
        return next.run(req).await; // unauthenticated mode (dev only)
    }
    let ok = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|t| t == state.api_key)
        .unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid or missing API key" })),
        )
            .into_response()
    }
}

fn err(e: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e })),
    )
        .into_response()
}
fn tag_or_default(t: &Option<String>) -> String {
    t.clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_TAG.to_string())
}

// ── health ──────────────────────────────────────────────────────────────────
async fn health(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({ "ok": state.engine.health().await })).into_response()
}

// ── ingest ──────────────────────────────────────────────────────────────────
#[derive(Deserialize)]
struct AddBody {
    content: Option<String>,
    /// Ingest a web page: fetched + cleaned via Jina Reader, then the pipeline.
    url: Option<String>,
    title: Option<String>,
    source: Option<String>,
    reference: Option<String>,
    container_tag: Option<String>,
    captured_at: Option<i64>,
    /// Path to a file **on the server's filesystem** (for local/embedded use).
    /// To upload a file from a client, use `multipart/form-data` instead.
    file_path: Option<String>,
}

/// `POST /v1/memories` — ingest. Dispatches on `Content-Type`:
/// - `application/json`: `content` (text), `url` (fetch+clean), or server-side `file_path`.
/// - `multipart/form-data`: a `file` part (PDF/image/text → OCR/extraction) plus
///   optional `title`/`source`/`reference`/`container_tag`/`captured_at` fields.
async fn add_memory(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let is_multipart = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("multipart/form-data"))
        .unwrap_or(false);
    if is_multipart {
        match Multipart::from_request(req, &state).await {
            Ok(mp) => ingest_multipart(&state, mp).await,
            Err(e) => bad_request(format!("invalid multipart: {e}")),
        }
    } else {
        let bytes = match Bytes::from_request(req, &state).await {
            Ok(b) => b,
            Err(e) => return bad_request(format!("could not read body: {e}")),
        };
        match serde_json::from_slice::<AddBody>(&bytes) {
            Ok(b) => ingest_json(&state, b).await,
            Err(e) => bad_request(format!("invalid JSON: {e}")),
        }
    }
}

fn doc_done(id: String) -> Response {
    Json(json!({ "document_id": id, "status": "done" })).into_response()
}
fn bad_request(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response()
}

async fn ingest_json(state: &Arc<AppState>, b: AddBody) -> Response {
    let now = chrono::Utc::now().timestamp();
    let tag = tag_or_default(&b.container_tag);
    let captured_at = b.captured_at.unwrap_or(now);
    // URL ingestion: fetch + clean via Jina Reader, then the normal pipeline.
    if let Some(url) = b.url.filter(|u| !u.is_empty()) {
        return match state.engine.add_url(&url, b.title, &tag, captured_at).await {
            Ok(id) => doc_done(id),
            Err(e) => err(e),
        };
    }
    let doc = IngestDoc {
        source: b.source.unwrap_or_else(|| "api".into()),
        title: b.title.unwrap_or_default(),
        content: b.content.unwrap_or_default(),
        reference: b.reference.unwrap_or_default(),
        app: String::new(),
        captured_at,
        file_path: b.file_path,
        container_tag: tag,
    };
    match state.engine.add_document(&doc).await {
        Ok(id) => doc_done(id),
        Err(e) => err(e),
    }
}

async fn ingest_multipart(state: &Arc<AppState>, mut mp: Multipart) -> Response {
    let mut file_bytes: Option<Bytes> = None;
    let mut filename = String::new();
    let (mut title, mut source, mut reference, mut container_tag) = (
        None::<String>,
        None::<String>,
        None::<String>,
        None::<String>,
    );
    let mut captured_at: Option<i64> = None;
    loop {
        match mp.next_field().await {
            Ok(Some(field)) => {
                let name = field.name().unwrap_or("").to_string();
                match name.as_str() {
                    "file" => {
                        filename = field.file_name().unwrap_or("upload").to_string();
                        match field.bytes().await {
                            Ok(b) => file_bytes = Some(b),
                            Err(e) => return bad_request(format!("reading file part: {e}")),
                        }
                    }
                    "title" => title = field.text().await.ok(),
                    "source" => source = field.text().await.ok(),
                    "reference" => reference = field.text().await.ok(),
                    "container_tag" => container_tag = field.text().await.ok(),
                    "captured_at" => {
                        captured_at = field.text().await.ok().and_then(|s| s.trim().parse().ok())
                    }
                    _ => {}
                }
            }
            Ok(None) => break,
            Err(e) => return bad_request(format!("multipart error: {e}")),
        }
    }
    let Some(bytes) = file_bytes else {
        return bad_request("multipart upload requires a 'file' part".into());
    };

    // Buffer to a temp file preserving the extension (the engine routes OCR vs
    // text-extraction by it), ingest via the normal file pipeline, then clean up.
    let base = std::path::Path::new(&filename)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "upload".into());
    let unique = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("ultramem-{unique}-{base}"));
    if let Err(e) = tokio::fs::write(&tmp, &bytes).await {
        return err(format!("could not buffer upload: {e}"));
    }
    let now = chrono::Utc::now().timestamp();
    let doc = IngestDoc {
        source: source.unwrap_or_else(|| "file".into()),
        title: title.unwrap_or_else(|| base.clone()),
        content: format!("File \"{base}\""),
        reference: reference.unwrap_or_else(|| base.clone()),
        app: String::new(),
        captured_at: captured_at.unwrap_or(now),
        file_path: Some(tmp.to_string_lossy().into_owned()),
        container_tag: tag_or_default(&container_tag),
    };
    let result = state.engine.add_document(&doc).await;
    let _ = tokio::fs::remove_file(&tmp).await; // best-effort cleanup
    match result {
        Ok(id) => doc_done(id),
        Err(e) => err(e),
    }
}

async fn delete_memory(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.engine.delete_document(&id).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(e),
    }
}

// ── search ──────────────────────────────────────────────────────────────────
#[derive(Deserialize)]
struct SearchBody {
    query: String,
    container_tag: Option<String>,
    limit: Option<usize>,
}

async fn search(State(state): State<Arc<AppState>>, Json(b): Json<SearchBody>) -> Response {
    let tag = tag_or_default(&b.container_tag);
    let limit = b.limit.unwrap_or(8).clamp(1, 50);
    match state
        .engine
        .retrieve_tagged(&tag, &b.query, None, limit)
        .await
    {
        Ok((docs, memories)) => {
            Json(json!({ "documents": docs, "memories": memories })).into_response()
        }
        Err(e) => err(e),
    }
}

// ── profile ─────────────────────────────────────────────────────────────────
#[derive(Deserialize)]
struct TagQuery {
    container_tag: Option<String>,
}

async fn profile(State(state): State<Arc<AppState>>, Query(q): Query<TagQuery>) -> Response {
    let p = state
        .engine
        .profile_tagged(&tag_or_default(&q.container_tag))
        .await;
    Json(json!({ "static": p.static_text, "dynamic": p.dynamic_text })).into_response()
}

// ── timeline ────────────────────────────────────────────────────────────────
#[derive(Deserialize)]
struct TimelineQuery {
    container_tag: Option<String>,
    before: Option<i64>,
    limit: Option<usize>,
}

async fn timeline(State(state): State<Arc<AppState>>, Query(q): Query<TimelineQuery>) -> Response {
    let tag = tag_or_default(&q.container_tag);
    let limit = q.limit.unwrap_or(60).clamp(1, 500);
    match state.engine.list_document_ids(&tag, q.before, limit).await {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|(id, title, source, reference, captured_at)| {
                    json!({ "document_id": id, "title": title, "source": source, "reference": reference, "captured_at": captured_at })
                })
                .collect();
            Json(json!({ "items": items })).into_response()
        }
        Err(e) => err(e),
    }
}

// ── reindex (reuses stored text — no re-extraction) ──────────────────────────
#[derive(Deserialize)]
struct ReindexBody {
    container_tag: Option<String>,
    mode: Option<String>, // "tags" | "latest" | "facts"
}

async fn reindex(State(state): State<Arc<AppState>>, Json(b): Json<ReindexBody>) -> Response {
    let tag = tag_or_default(&b.container_tag);
    match b.mode.as_deref().unwrap_or("latest") {
        "tags" => match state.engine.claim_legacy_into_tag(&tag).await {
            Ok(()) => {
                let _ = state.engine.backfill_facts_latest().await;
                Json(json!({ "ok": true, "mode": "tags" })).into_response()
            }
            Err(e) => err(e),
        },
        "latest" => match state.engine.backfill_facts_latest().await {
            Ok(()) => Json(json!({ "ok": true, "mode": "latest" })).into_response(),
            Err(e) => err(e),
        },
        "facts" => match state.engine.list_document_ids(&tag, None, 1_000_000).await {
            Ok(rows) => {
                let total = rows.len();
                let st = state.clone();
                let tag = tag.clone();
                tokio::spawn(async move {
                    for (doc_id, title, source, reference, captured_at) in rows {
                        let _ = st
                            .engine
                            .reindex_doc_facts(
                                &doc_id,
                                &title,
                                &source,
                                &reference,
                                captured_at,
                                &tag,
                            )
                            .await;
                    }
                });
                Json(json!({ "ok": true, "mode": "facts", "total": total, "status": "running" }))
                    .into_response()
            }
            Err(e) => err(e),
        },
        other => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("unknown mode '{other}'") })),
        )
            .into_response(),
    }
}
