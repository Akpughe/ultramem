//! Thin Qdrant REST client over reqwest. Works against any Qdrant —
//! the hosted instance the app is configured with, or a local one in tests.

use serde_json::{json, Value};

fn req(
    http: &reqwest::Client,
    method: reqwest::Method,
    base: &str,
    key: &str,
    path: &str,
) -> reqwest::RequestBuilder {
    let mut r = http
        .request(method, format!("{}{}", base.trim_end_matches('/'), path))
        .timeout(std::time::Duration::from_secs(30));
    if !key.is_empty() {
        r = r.header("api-key", key);
    }
    r
}

/// Reachability check (2s). Any non-5xx answer from /collections counts.
pub async fn health(http: &reqwest::Client, base: &str, key: &str) -> bool {
    req(http, reqwest::Method::GET, base, key, "/collections")
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await
        .map(|r| r.status().as_u16() < 500)
        .unwrap_or(false)
}

/// Create `name` (1024-dim cosine) if it doesn't exist. Idempotent.
pub async fn ensure_collection(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    name: &str,
    dim: usize,
) -> Result<(), String> {
    let exists = req(http, reqwest::Method::GET, base, key, &format!("/collections/{name}"))
        .send()
        .await
        .map_err(|e| format!("qdrant unreachable: {e}"))?
        .status()
        .is_success();
    if exists {
        return Ok(());
    }
    let resp = req(http, reqwest::Method::PUT, base, key, &format!("/collections/{name}"))
        .json(&json!({ "vectors": { "size": dim, "distance": "Cosine" } }))
        .send()
        .await
        .map_err(|e| format!("qdrant unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("qdrant create {name} failed: {}", resp.status()));
    }
    Ok(())
}

/// Create a HYBRID collection if missing: a named dense vector `dense`
/// (cosine) plus a sparse vector `text` with server-side IDF (Qdrant's BM25).
/// Distinct schema from `ensure_collection` — points need both vectors and
/// queries fuse them with RRF.
pub async fn ensure_collection_hybrid(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    name: &str,
    dim: usize,
) -> Result<(), String> {
    let exists = req(http, reqwest::Method::GET, base, key, &format!("/collections/{name}"))
        .send()
        .await
        .map_err(|e| format!("qdrant unreachable: {e}"))?
        .status()
        .is_success();
    if exists {
        return Ok(());
    }
    let resp = req(http, reqwest::Method::PUT, base, key, &format!("/collections/{name}"))
        .json(&json!({
            "vectors": { "dense": { "size": dim, "distance": "Cosine" } },
            "sparse_vectors": { "text": { "modifier": "idf" } }
        }))
        .send()
        .await
        .map_err(|e| format!("qdrant unreachable: {e}"))?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("qdrant create hybrid {name} failed: {}", body.chars().take(200).collect::<String>()));
    }
    Ok(())
}

/// Hybrid dense+sparse search fused server-side with Reciprocal Rank Fusion.
/// `sparse` is `(indices, values)`. Returns the same `{id, score, payload}`
/// hit shape as `search`.
#[allow(clippy::too_many_arguments)]
pub async fn search_hybrid(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    dense: &[f32],
    sparse: &(Vec<u32>, Vec<f32>),
    limit: usize,
    filter: Option<Value>,
) -> Result<Vec<Value>, String> {
    let prefetch_limit = (limit * 2).max(40);
    let mut prefetch = vec![json!({
        "query": dense,
        "using": "dense",
        "limit": prefetch_limit,
    })];
    if !sparse.0.is_empty() {
        prefetch.push(json!({
            "query": { "indices": sparse.0, "values": sparse.1 },
            "using": "text",
            "limit": prefetch_limit,
        }));
    }
    let mut body = json!({
        "prefetch": prefetch,
        "query": { "fusion": "rrf" },
        "limit": limit,
        "with_payload": true,
    });
    if let Some(f) = filter {
        body["filter"] = f;
    }
    let resp = req(http, reqwest::Method::POST, base, key, &format!("/collections/{collection}/points/query"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("qdrant unreachable: {e}"))?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| format!("qdrant bad response: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "qdrant hybrid query {collection} failed {status}: {}",
            v["status"]["error"].as_str().unwrap_or("unknown")
        ));
    }
    Ok(v["result"]["points"].as_array().cloned().unwrap_or_default())
}

/// Upsert points (`{id, vector, payload}` values). `wait=true` so a returned
/// Ok means the data is durable and searchable — that's the ingest
/// backpressure contract.
pub async fn upsert(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    points: Vec<Value>,
) -> Result<(), String> {
    if points.is_empty() {
        return Ok(());
    }
    let resp = req(
        http,
        reqwest::Method::PUT,
        base,
        key,
        &format!("/collections/{collection}/points?wait=true"),
    )
    .json(&json!({ "points": points }))
    .send()
    .await
    .map_err(|e| format!("qdrant unreachable: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "qdrant upsert into {collection} failed {status}: {}",
            body.chars().take(200).collect::<String>()
        ));
    }
    Ok(())
}

/// Create a payload index so filtered search stays fast. Idempotent-ish:
/// re-creating an existing index errors and we ignore it.
pub async fn ensure_payload_index(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    field: &str,
    schema: &str,
) {
    let _ = req(http, reqwest::Method::PUT, base, key, &format!("/collections/{collection}/index"))
        .json(&json!({ "field_name": field, "field_schema": schema }))
        .send()
        .await;
}

/// Dense search. Returns the raw hit values: `{id, score, payload}`.
/// `filter` is a Qdrant filter object (e.g. source/time constraints).
pub async fn search(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    vector: &[f32],
    limit: usize,
    score_threshold: f32,
    filter: Option<Value>,
) -> Result<Vec<Value>, String> {
    let mut body = json!({
        "vector": vector,
        "limit": limit,
        "with_payload": true,
        "score_threshold": score_threshold,
    });
    if let Some(f) = filter {
        body["filter"] = f;
    }
    let resp = req(
        http,
        reqwest::Method::POST,
        base,
        key,
        &format!("/collections/{collection}/points/search"),
    )
    .json(&body)
    .send()
    .await
    .map_err(|e| format!("qdrant unreachable: {e}"))?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| format!("qdrant bad response: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "qdrant search {collection} failed {status}: {}",
            v["status"]["error"].as_str().unwrap_or("unknown")
        ));
    }
    Ok(v["result"].as_array().cloned().unwrap_or_default())
}

/// Merge `payload` into the given points' existing payloads (Qdrant set-payload
/// — additive, leaves other keys intact). Used to flip a superseded memory's
/// `is_latest` to false without re-upserting its vector.
pub async fn set_payload(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    point_ids: &[String],
    payload: Value,
) -> Result<(), String> {
    if point_ids.is_empty() {
        return Ok(());
    }
    let resp = req(
        http,
        reqwest::Method::POST,
        base,
        key,
        &format!("/collections/{collection}/points/payload?wait=true"),
    )
    .json(&json!({ "payload": payload, "points": point_ids }))
    .send()
    .await
    .map_err(|e| format!("qdrant unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("qdrant set_payload in {collection} failed: {}", resp.status()));
    }
    Ok(())
}

/// Set `payload` on every point matching `filter` (server-side, no scroll). An
/// empty filter `{}` matches all points. Returns Ok on success. Used by the
/// reindex backfills (assign container_tag, flag is_latest) — reuses the stored
/// text and embeddings, touching only the payload.
pub async fn set_payload_by_filter(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    filter: Value,
    payload: Value,
) -> Result<(), String> {
    let resp = req(
        http,
        reqwest::Method::POST,
        base,
        key,
        &format!("/collections/{collection}/points/payload?wait=true"),
    )
    .json(&json!({ "payload": payload, "filter": filter }))
    .send()
    .await
    .map_err(|e| format!("qdrant unreachable: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "qdrant set_payload_by_filter in {collection} failed {status}: {}",
            body.chars().take(200).collect::<String>()
        ));
    }
    Ok(())
}

/// Scroll up to `limit` points (payloads only, no vectors).
pub async fn scroll(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let resp = req(
        http,
        reqwest::Method::POST,
        base,
        key,
        &format!("/collections/{collection}/points/scroll"),
    )
    .json(&json!({ "limit": limit, "with_payload": true, "with_vector": false }))
    .send()
    .await
    .map_err(|e| format!("qdrant unreachable: {e}"))?;
    let status = resp.status();
    let v: Value = resp.json().await.map_err(|e| format!("qdrant bad response: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "qdrant scroll {collection} failed {status}: {}",
            v["status"]["error"].as_str().unwrap_or("unknown")
        ));
    }
    Ok(v["result"]["points"].as_array().cloned().unwrap_or_default())
}

/// Scroll ALL points matching an optional filter, paginating via Qdrant's
/// `next_page_offset` up to `cap` points. Payloads only. Used to enumerate
/// documents in a namespace (UltraMem has no external document registry).
pub async fn scroll_all(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    filter: Option<Value>,
    cap: usize,
) -> Result<Vec<Value>, String> {
    let mut out: Vec<Value> = Vec::new();
    let mut offset: Option<Value> = None;
    loop {
        let mut body = json!({ "limit": 256, "with_payload": true, "with_vector": false });
        if let Some(f) = &filter {
            body["filter"] = f.clone();
        }
        if let Some(o) = &offset {
            body["offset"] = o.clone();
        }
        let resp = req(http, reqwest::Method::POST, base, key, &format!("/collections/{collection}/points/scroll"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("qdrant unreachable: {e}"))?;
        let v: Value = resp.json().await.map_err(|e| format!("qdrant bad response: {e}"))?;
        let pts = v["result"]["points"].as_array().cloned().unwrap_or_default();
        if pts.is_empty() {
            break;
        }
        out.extend(pts);
        if out.len() >= cap {
            out.truncate(cap);
            break;
        }
        match v["result"]["next_page_offset"].clone() {
            Value::Null => break,
            next => offset = Some(next),
        }
    }
    Ok(out)
}

/// Fetch a few chunks of one document (content payloads), for tooling that
/// needs the exact indexed text — e.g. the retrieval audit's query generator.
pub async fn chunks_of_doc(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    doc_id: &str,
    limit: usize,
) -> Result<Vec<String>, String> {
    let resp = req(
        http,
        reqwest::Method::POST,
        base,
        key,
        &format!("/collections/{collection}/points/scroll"),
    )
    .json(&json!({
        "limit": limit,
        "with_payload": true,
        "with_vector": false,
        "filter": { "must": [ { "key": "doc_id", "match": { "value": doc_id } } ] },
    }))
    .send()
    .await
    .map_err(|e| format!("qdrant unreachable: {e}"))?;
    let v: Value = resp.json().await.map_err(|e| format!("qdrant bad response: {e}"))?;
    Ok(v["result"]["points"]
        .as_array()
        .map(|pts| {
            pts.iter()
                .filter_map(|p| p["payload"]["content"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

/// All of a document's chunks as `(chunk_index, content)`, unsorted. Used by
/// the A/B benchmark to reconstruct a document's indexed text (join in index
/// order) without re-reading/re-extracting the original file.
pub async fn doc_chunks_indexed(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    doc_id: &str,
    limit: usize,
) -> Result<Vec<(i64, String)>, String> {
    let resp = req(
        http,
        reqwest::Method::POST,
        base,
        key,
        &format!("/collections/{collection}/points/scroll"),
    )
    .json(&json!({
        "limit": limit,
        "with_payload": true,
        "with_vector": false,
        "filter": { "must": [ { "key": "doc_id", "match": { "value": doc_id } } ] },
    }))
    .send()
    .await
    .map_err(|e| format!("qdrant unreachable: {e}"))?;
    let v: Value = resp.json().await.map_err(|e| format!("qdrant bad response: {e}"))?;
    Ok(v["result"]["points"]
        .as_array()
        .map(|pts| {
            pts.iter()
                .filter_map(|p| {
                    let idx = p["payload"]["chunk_index"].as_i64().unwrap_or(0);
                    p["payload"]["content"].as_str().map(|c| (idx, c.to_string()))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Delete every point matching an arbitrary payload filter.
pub async fn delete_by_filter(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    filter: Value,
) -> Result<(), String> {
    let resp = req(
        http,
        reqwest::Method::POST,
        base,
        key,
        &format!("/collections/{collection}/points/delete?wait=true"),
    )
    .json(&json!({ "filter": filter }))
    .send()
    .await
    .map_err(|e| format!("qdrant unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("qdrant filtered delete from {collection} failed: {}", resp.status()));
    }
    Ok(())
}

/// Delete every point whose payload `doc_id` matches.
pub async fn delete_by_doc(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    collection: &str,
    doc_id: &str,
) -> Result<(), String> {
    let resp = req(
        http,
        reqwest::Method::POST,
        base,
        key,
        &format!("/collections/{collection}/points/delete?wait=true"),
    )
    .json(&json!({
        "filter": { "must": [ { "key": "doc_id", "match": { "value": doc_id } } ] }
    }))
    .send()
    .await
    .map_err(|e| format!("qdrant unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("qdrant delete from {collection} failed: {}", resp.status()));
    }
    Ok(())
}

/// Drop a whole collection (used by the benchmark to clean up after itself).
pub async fn delete_collection(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    name: &str,
) -> Result<(), String> {
    let resp = req(http, reqwest::Method::DELETE, base, key, &format!("/collections/{name}"))
        .send()
        .await
        .map_err(|e| format!("qdrant unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("qdrant drop {name} failed: {}", resp.status()));
    }
    Ok(())
}
