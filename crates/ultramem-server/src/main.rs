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
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use ultramem_core::{EngineCfg, IngestDoc, MemoryEngine};

mod tenant;
use tenant::{AuthConfig, TenantCtx};

struct AppState {
    engine: MemoryEngine,
    auth: AuthConfig,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    // NB: not named `auth` — that would shadow the `auth` middleware fn at the
    // `from_fn_with_state(state, auth)` call site below.
    let auth_cfg = AuthConfig::from_env();
    if auth_cfg.is_misconfigured() {
        eprintln!(
            "[ultramem] FATAL: no API credentials configured. Set ULTRAMEM_API_KEY (or \
             ULTRAMEM_TENANTS=\"key=tag1,tag2\"), or set ULTRAMEM_DEV=1 to run unauthenticated \
             for local development only."
        );
        std::process::exit(1);
    }
    if auth_cfg.is_open() {
        eprintln!(
            "[ultramem] WARNING: ULTRAMEM_DEV=1 and no keys — the API is UNAUTHENTICATED (dev only)."
        );
    }
    let cfg = EngineCfg::from_env();
    let mut engine = MemoryEngine::new(cfg.clone());
    // Phase A: attach the Postgres source of truth when configured. Connect +
    // migrate at startup; a failure logs and falls back to Qdrant-only rather
    // than blocking the server.
    if let Some(pg_url) = &cfg.pg_url {
        use ultramem_core::db::Db;
        match ultramem_core::db::PgDb::connect(pg_url).await {
            Ok(db) => match db.migrate().await {
                Ok(()) => {
                    engine = engine.with_db(std::sync::Arc::new(db));
                    println!("[ultramem] Postgres source of truth attached (dual-write enabled)");
                }
                Err(e) => eprintln!("[ultramem] WARNING: pg migrate failed ({e}); running Qdrant-only"),
            },
            Err(e) => eprintln!(
                "[ultramem] WARNING: ULTRAMEM_PG_URL set but connect failed ({e}); running Qdrant-only"
            ),
        }
    }
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
    let state = Arc::new(AppState {
        engine,
        auth: auth_cfg,
    });

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
        .route("/v1/jobs/:id", get(job_status))
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

/// Bearer-key gate. Resolves the credential to a [`TenantCtx`] (which namespaces
/// it may touch) and injects it for handlers. In dev/open mode every request gets
/// an unrestricted context. A missing/unknown key is `401`.
async fn auth(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut req: axum::extract::Request,
    next: Next,
) -> Response {
    let ctx = if state.auth.is_open() {
        TenantCtx::any() // unauthenticated mode (dev only)
    } else {
        let bearer = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "));
        match state.auth.resolve(bearer) {
            Some(ctx) => ctx,
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "invalid or missing API key" })),
                )
                    .into_response()
            }
        }
    };
    req.extensions_mut().insert(ctx);
    next.run(req).await
}

fn err(e: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e })),
    )
        .into_response()
}

/// The requested `container_tag` is outside the credential's allowed set.
fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "container_tag not permitted for this credential" })),
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}

/// Resolve the request's tag against the credential. `Err(())` means "denied";
/// call sites turn that into [`forbidden`] (kept a ZST error so the `Result`
/// doesn't carry a large `Response`).
fn resolve_tag(ctx: &TenantCtx, requested: &Option<String>) -> Result<String, ()> {
    ctx.resolve_tag(requested).map_err(|_| ())
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
    /// Rejected over the network (SS-3): reading an arbitrary server path is a
    /// file-disclosure risk. Present only so the server can return a clear `400`;
    /// local file ingestion stays available through the embedded Rust engine API.
    file_path: Option<Value>,
}

/// Reject a JSON ingest that tries to name a server-side `file_path` (SS-3).
fn check_add_body(b: &AddBody) -> Result<(), String> {
    if b.file_path.is_some() {
        return Err(
            "`file_path` is not accepted over the network; upload the file via multipart/form-data \
             instead"
                .into(),
        );
    }
    Ok(())
}

/// `POST /v1/memories` — ingest. Dispatches on `Content-Type`:
/// - `application/json`: `content` (text) or `url` (fetch+clean).
/// - `multipart/form-data`: a `file` part (PDF/image/text → OCR/extraction) plus
///   optional `title`/`source`/`reference`/`container_tag`/`captured_at` fields.
async fn add_memory(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<TenantCtx>,
    req: Request,
) -> Response {
    let is_multipart = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("multipart/form-data"))
        .unwrap_or(false);
    if is_multipart {
        match Multipart::from_request(req, &state).await {
            Ok(mp) => ingest_multipart(&state, &ctx, mp).await,
            Err(e) => bad_request(format!("invalid multipart: {e}")),
        }
    } else {
        let bytes = match Bytes::from_request(req, &state).await {
            Ok(b) => b,
            Err(e) => return bad_request(format!("could not read body: {e}")),
        };
        match serde_json::from_slice::<AddBody>(&bytes) {
            Ok(b) => ingest_json(&state, &ctx, b).await,
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

async fn ingest_json(state: &Arc<AppState>, ctx: &TenantCtx, b: AddBody) -> Response {
    if let Err(m) = check_add_body(&b) {
        return bad_request(m);
    }
    let tag = match resolve_tag(ctx, &b.container_tag) {
        Ok(t) => t,
        Err(()) => return forbidden(),
    };
    let now = chrono::Utc::now().timestamp();
    let captured_at = b.captured_at.unwrap_or(now);
    // URL ingestion: fetch + clean via Jina Reader, then the normal pipeline.
    if let Some(url) = b.url.filter(|u| !u.is_empty()) {
        return match state.engine.add_url(&url, b.title, &tag, captured_at).await {
            Ok(id) => {
                state.engine.audit(&tag, "ingest", Some(&id)).await;
                doc_done(id)
            }
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
        // Never set from the network — the request path can't name a server file.
        file_path: None,
        container_tag: tag,
    };
    match state.engine.add_document(&doc).await {
        Ok(id) => {
            state
                .engine
                .audit(&doc.container_tag, "ingest", Some(&id))
                .await;
            doc_done(id)
        }
        Err(e) => err(e),
    }
}

async fn ingest_multipart(state: &Arc<AppState>, ctx: &TenantCtx, mut mp: Multipart) -> Response {
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
    let tag = match resolve_tag(ctx, &container_tag) {
        Ok(t) => t,
        Err(()) => return forbidden(),
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
        // Server-generated temp path, not client-supplied — safe.
        file_path: Some(tmp.to_string_lossy().into_owned()),
        container_tag: tag,
    };
    let result = state.engine.add_document(&doc).await;
    let _ = tokio::fs::remove_file(&tmp).await; // best-effort cleanup
    match result {
        Ok(id) => {
            state
                .engine
                .audit(&doc.container_tag, "ingest", Some(&id))
                .await;
            doc_done(id)
        }
        Err(e) => err(e),
    }
}

/// `DELETE /v1/memories/:id` — scoped to the caller's namespace (SS-2). A
/// document outside the caller's tag is `404` (not deleted, not disclosed); a
/// `container_tag` the credential doesn't own is `403`.
async fn delete_memory(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<TenantCtx>,
    Query(q): Query<TagQuery>,
    Path(id): Path<String>,
) -> Response {
    let tag = match resolve_tag(&ctx, &q.container_tag) {
        Ok(t) => t,
        Err(()) => return forbidden(),
    };
    match state.engine.delete_document_tagged(&id, &tag).await {
        Ok(true) => {
            state.engine.audit(&tag, "delete", Some(&id)).await;
            Json(json!({ "ok": true })).into_response()
        }
        Ok(false) => not_found(),
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

async fn search(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<TenantCtx>,
    Json(b): Json<SearchBody>,
) -> Response {
    let tag = match resolve_tag(&ctx, &b.container_tag) {
        Ok(t) => t,
        Err(()) => return forbidden(),
    };
    let limit = b.limit.unwrap_or(8).clamp(1, 50);
    match state
        .engine
        .retrieve_tagged(&tag, &b.query, None, limit)
        .await
    {
        Ok((docs, memories)) => {
            // Phase A Task 5: attach provenance (kind/confidence/evidence) from
            // the relational source of truth. `memories` stays a string array
            // (unchanged); `provenance` is additive and empty without a Db.
            let provenance = state.engine.memory_provenance(&tag, &memories).await;
            Json(json!({ "documents": docs, "memories": memories, "provenance": provenance }))
                .into_response()
        }
        Err(e) => err(e),
    }
}

// ── profile ─────────────────────────────────────────────────────────────────
#[derive(Deserialize)]
struct TagQuery {
    container_tag: Option<String>,
}

async fn profile(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<TenantCtx>,
    Query(q): Query<TagQuery>,
) -> Response {
    let tag = match resolve_tag(&ctx, &q.container_tag) {
        Ok(t) => t,
        Err(()) => return forbidden(),
    };
    let p = state.engine.profile_tagged(&tag).await;
    Json(json!({ "static": p.static_text, "dynamic": p.dynamic_text })).into_response()
}

// ── timeline ────────────────────────────────────────────────────────────────
#[derive(Deserialize)]
struct TimelineQuery {
    container_tag: Option<String>,
    before: Option<i64>,
    limit: Option<usize>,
}

async fn timeline(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<TenantCtx>,
    Query(q): Query<TimelineQuery>,
) -> Response {
    let tag = match resolve_tag(&ctx, &q.container_tag) {
        Ok(t) => t,
        Err(()) => return forbidden(),
    };
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

async fn reindex(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<TenantCtx>,
    Json(b): Json<ReindexBody>,
) -> Response {
    let tag = match resolve_tag(&ctx, &b.container_tag) {
        Ok(t) => t,
        Err(()) => return forbidden(),
    };
    state.engine.audit(&tag, "reindex", None).await;
    match b.mode.as_deref().unwrap_or("latest") {
        // Phase A: migrate this namespace's existing Qdrant data into Postgres.
        "backfill" => match state.engine.backfill_to_pg(&tag).await {
            Ok(stats) => {
                Json(json!({ "ok": true, "mode": "backfill", "stats": stats })).into_response()
            }
            Err(e) => err(e),
        },
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
                // Track the background work as a job when a Db is attached, so it
                // is observable via GET /v1/jobs/:id instead of a fire-and-forget
                // spawn. Falls back to the untracked task when no Db.
                let job_id = state
                    .engine
                    .job_create(&tag, "reindex_facts", total as i32)
                    .await;
                let spawn_job = job_id.clone();
                tokio::spawn(async move {
                    if let Some(id) = &spawn_job {
                        st.engine.job_update(id, "running", 0, None).await;
                    }
                    let mut done = 0i32;
                    let mut failed = 0i32;
                    for (doc_id, title, source, reference, captured_at) in rows {
                        if let Err(e) = st
                            .engine
                            .reindex_doc_facts(
                                &doc_id,
                                &title,
                                &source,
                                &reference,
                                captured_at,
                                &tag,
                            )
                            .await
                        {
                            failed += 1;
                            eprintln!("[ultramem] reindex_doc_facts failed for {doc_id}: {e}");
                        }
                        done += 1;
                        if let Some(id) = &spawn_job {
                            st.engine.job_update(id, "running", done, None).await;
                        }
                    }
                    if let Some(id) = &spawn_job {
                        let err = (failed > 0).then(|| format!("{failed} document(s) failed"));
                        st.engine.job_update(id, "done", done, err.as_deref()).await;
                    }
                });
                Json(json!({
                    "ok": true, "mode": "facts", "total": total, "status": "running",
                    "job_id": job_id,
                }))
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

/// `GET /v1/jobs/:id?container_tag=…` — status of a tracked background job.
/// `404` if the job isn't in the caller's namespace (or job tracking is off,
/// i.e. no Postgres configured).
async fn job_status(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<TenantCtx>,
    Query(q): Query<TagQuery>,
    Path(id): Path<String>,
) -> Response {
    let tag = match resolve_tag(&ctx, &q.container_tag) {
        Ok(t) => t,
        Err(()) => return forbidden(),
    };
    match state.engine.job_get(&id, &tag).await {
        Some(job) => Json(job).into_response(),
        None => not_found(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(file_path: Option<Value>) -> AddBody {
        AddBody {
            content: Some("hello".into()),
            url: None,
            title: None,
            source: None,
            reference: None,
            container_tag: None,
            captured_at: None,
            file_path,
        }
    }

    #[test]
    fn json_ingest_rejects_file_path() {
        // SS-3: a network request must not be able to name a server-side path.
        assert!(check_add_body(&body(Some(json!("/etc/passwd")))).is_err());
    }

    #[test]
    fn json_ingest_allows_normal_content() {
        assert!(check_add_body(&body(None)).is_ok());
    }
}
