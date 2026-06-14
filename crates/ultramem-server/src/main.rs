//! UltraMem HTTP API — a thin axum server over `ultramem-core`. See
//! `docs/API.md`. Multi-tenant via `container_tag`; Bearer-key auth.
//!
//! Run:  ULTRAMEM_API_KEY=… QDRANT_URL=… JINA_API_KEY=… GROQ_API_KEY=… \
//!       cargo run -p ultramem-server   # listens on :8080 (PORT overrides)

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
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
        .route("/v1/memories", post(add_memory))
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
    title: Option<String>,
    source: Option<String>,
    reference: Option<String>,
    container_tag: Option<String>,
    captured_at: Option<i64>,
    file_path: Option<String>,
}

async fn add_memory(State(state): State<Arc<AppState>>, Json(b): Json<AddBody>) -> Response {
    let now = chrono::Utc::now().timestamp();
    let doc = IngestDoc {
        source: b.source.unwrap_or_else(|| "api".into()),
        title: b.title.unwrap_or_default(),
        content: b.content.unwrap_or_default(),
        reference: b.reference.unwrap_or_default(),
        app: String::new(),
        captured_at: b.captured_at.unwrap_or(now),
        file_path: b.file_path,
        container_tag: tag_or_default(&b.container_tag),
    };
    match state.engine.add_document(&doc).await {
        Ok(document_id) => {
            Json(json!({ "document_id": document_id, "status": "done" })).into_response()
        }
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
