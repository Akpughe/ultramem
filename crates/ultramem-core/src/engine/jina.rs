//! Jina embeddings API client. jina-embeddings-v3, 1024-dim, task-adapted
//! vectors: "retrieval.passage" for ingested content, "retrieval.query" for
//! questions.

use serde_json::{json, Value};

pub const MODEL: &str = "jina-embeddings-v3";
pub const RERANK_MODEL: &str = "jina-reranker-v2-base-multilingual";
pub const DIM: usize = 1024;
const URL: &str = "https://api.jina.ai/v1/embeddings";
const RERANK_URL: &str = "https://api.jina.ai/v1/rerank";
const BATCH: usize = 64;

/// Cross-encoder rerank: score each document against the query. Returns
/// (original_index, relevance_score) sorted by relevance, best first. This is
/// what separates "contains similar words" from "actually answers this".
pub async fn rerank(
    http: &reqwest::Client,
    api_key: &str,
    query: &str,
    documents: &[String],
) -> Result<Vec<(usize, f64)>, String> {
    if api_key.is_empty() {
        return Err("no Jina API key configured".into());
    }
    let resp = http
        .post(RERANK_URL)
        .bearer_auth(api_key)
        .timeout(std::time::Duration::from_secs(20))
        .json(&json!({
            "model": RERANK_MODEL,
            "query": query,
            "documents": documents,
            "top_n": documents.len(),
        }))
        .send()
        .await
        .map_err(|e| format!("jina rerank unreachable: {e}"))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("jina rerank bad response: {e}"))?;
    if !status.is_success() {
        let detail = v["detail"].as_str().unwrap_or("unknown");
        return Err(format!("jina rerank error {status}: {detail}"));
    }
    Ok(v["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    Some((
                        r["index"].as_u64()? as usize,
                        r["relevance_score"].as_f64()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Embed `inputs`, batching requests. Returns one vector per input, in order.
pub async fn embed(
    http: &reqwest::Client,
    api_key: &str,
    task: &str,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    if api_key.is_empty() {
        return Err("no Jina API key configured".into());
    }
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(inputs.len());
    for batch in inputs.chunks(BATCH) {
        let resp = http
            .post(URL)
            .bearer_auth(api_key)
            .timeout(std::time::Duration::from_secs(60))
            .json(&json!({ "model": MODEL, "task": task, "input": batch }))
            .send()
            .await
            .map_err(|e| format!("jina unreachable: {e}"))?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("jina bad response: {e}"))?;
        if !status.is_success() {
            let detail = v["detail"]
                .as_str()
                .or_else(|| v["error"]["message"].as_str())
                .unwrap_or("unknown");
            return Err(format!("jina error {status}: {detail}"));
        }
        let mut data: Vec<&Value> = v["data"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        if data.len() != batch.len() {
            return Err(format!(
                "jina returned {} embeddings for {} inputs",
                data.len(),
                batch.len()
            ));
        }
        // The API documents index-ordered results; sort defensively.
        data.sort_by_key(|d| d["index"].as_u64().unwrap_or(0));
        for d in data {
            let vec: Vec<f32> = d["embedding"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_f64().map(|f| f as f32))
                        .collect()
                })
                .unwrap_or_default();
            if vec.len() != DIM {
                return Err(format!("jina embedding dim {} != {DIM}", vec.len()));
            }
            out.push(vec);
        }
    }
    Ok(out)
}
