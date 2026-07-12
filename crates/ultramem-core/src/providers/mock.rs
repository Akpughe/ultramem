//! In-memory `VectorStore` for offline tests (no Qdrant, no network).
//!
//! Backs the memory-lifecycle and forgetting tests so `cargo test` can prove
//! end-to-end behavior — delete cascades, active-only retrieval — without a live
//! vector database. It evaluates the same `must`/`should`/`must_not` payload
//! filters the engine builds (match / range / is_empty), which is all the
//! lifecycle paths need. Similarity search is intentionally unimplemented; tests
//! that need reconciliation drive it through the pure `memory::action_for`
//! surface instead.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

use super::{EmbedTask, Embedder, Llm, VectorStore};
use crate::llm::ResolvedModel;

/// A deterministic embedder that returns fixed-dimension zero vectors — enough
/// for offline ingest tests that don't exercise similarity.
pub struct MockEmbedder {
    pub dim: usize,
}

#[async_trait]
impl Embedder for MockEmbedder {
    async fn embed(&self, _task: EmbedTask, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        Ok(inputs.iter().map(|_| vec![0.0; self.dim]).collect())
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn id(&self) -> &str {
        "mock-embedder"
    }
}

/// An `Llm` that records every prompt it receives and returns a canned response.
/// Lets a test assert what actually reached the model — e.g. that ingested
/// content was wrapped in injection-guard delimiters before distillation.
pub struct CapturingLlm {
    reply: String,
    pub calls: Mutex<Vec<(String, String)>>, // (system, user)
}

impl CapturingLlm {
    pub fn new(reply: impl Into<String>) -> Self {
        Self {
            reply: reply.into(),
            calls: Mutex::new(Vec::new()),
        }
    }
    /// The (system, user) of the most recent chat call.
    pub fn last(&self) -> (String, String) {
        self.calls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl Llm for CapturingLlm {
    async fn chat(
        &self,
        _m: &ResolvedModel,
        system: &str,
        user: &str,
        _temperature: f64,
    ) -> Result<String, String> {
        self.calls
            .lock()
            .unwrap()
            .push((system.to_string(), user.to_string()));
        Ok(self.reply.clone())
    }
    async fn complete(
        &self,
        _m: &ResolvedModel,
        _messages: Value,
        _temperature: f64,
    ) -> Result<String, String> {
        Ok(self.reply.clone())
    }
}

/// A collection of points, each shaped `{ "id", "vector", "payload" }`.
#[derive(Default)]
pub struct MemStore {
    cols: Mutex<HashMap<String, Vec<Value>>>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
    /// Number of points currently stored in a collection.
    pub fn count(&self, collection: &str) -> usize {
        self.cols
            .lock()
            .unwrap()
            .get(collection)
            .map(|v| v.len())
            .unwrap_or(0)
    }
    /// Number of points in a collection whose payload matches `filter`.
    pub fn count_matching(&self, collection: &str, filter: &Value) -> usize {
        self.cols
            .lock()
            .unwrap()
            .get(collection)
            .map(|v| {
                v.iter()
                    .filter(|p| filter_matches(&p["payload"], filter))
                    .count()
            })
            .unwrap_or(0)
    }
}

/// One filter condition against a payload: `match` (value equality), `range`
/// (integer bounds), or `is_empty` (missing/null key).
fn cond_matches(payload: &Value, cond: &Value) -> bool {
    if let Some(key) = cond.get("key").and_then(Value::as_str) {
        if let Some(m) = cond.get("match") {
            return payload.get(key) == m.get("value");
        }
        if let Some(r) = cond.get("range") {
            let Some(v) = payload.get(key).and_then(Value::as_i64) else {
                return false;
            };
            let ok = |b: &str, f: &dyn Fn(i64) -> bool| {
                r.get(b).and_then(Value::as_i64).map(f).unwrap_or(true)
            };
            return ok("lt", &|x| v < x)
                && ok("lte", &|x| v <= x)
                && ok("gt", &|x| v > x)
                && ok("gte", &|x| v >= x);
        }
    }
    if let Some(k) = cond
        .get("is_empty")
        .and_then(|e| e.get("key"))
        .and_then(Value::as_str)
    {
        return payload.get(k).map(Value::is_null).unwrap_or(true);
    }
    false
}

/// Evaluate a Qdrant-shaped `{ must, should, must_not }` filter against a payload.
fn filter_matches(payload: &Value, filter: &Value) -> bool {
    if let Some(must) = filter.get("must").and_then(Value::as_array) {
        if !must.iter().all(|c| cond_matches(payload, c)) {
            return false;
        }
    }
    if let Some(must_not) = filter.get("must_not").and_then(Value::as_array) {
        if must_not.iter().any(|c| cond_matches(payload, c)) {
            return false;
        }
    }
    if let Some(should) = filter.get("should").and_then(Value::as_array) {
        if !should.is_empty() && !should.iter().any(|c| cond_matches(payload, c)) {
            return false;
        }
    }
    true
}

#[async_trait]
impl VectorStore for MemStore {
    async fn health(&self) -> bool {
        true
    }
    async fn ensure_collection(&self, name: &str, _dim: usize) -> Result<(), String> {
        self.cols.lock().unwrap().entry(name.into()).or_default();
        Ok(())
    }
    async fn ensure_collection_hybrid(&self, name: &str, _dim: usize) -> Result<(), String> {
        self.ensure_collection(name, 0).await
    }
    async fn ensure_payload_index(&self, _collection: &str, _field: &str, _schema: &str) {}

    async fn upsert(&self, collection: &str, points: Vec<Value>) -> Result<(), String> {
        self.cols
            .lock()
            .unwrap()
            .entry(collection.into())
            .or_default()
            .extend(points);
        Ok(())
    }

    async fn set_payload(
        &self,
        collection: &str,
        point_ids: &[String],
        payload: Value,
    ) -> Result<(), String> {
        let mut cols = self.cols.lock().unwrap();
        if let Some(pts) = cols.get_mut(collection) {
            for p in pts.iter_mut() {
                if p["id"].as_str().map(|id| point_ids.iter().any(|x| x == id)) == Some(true) {
                    if let (Some(dst), Some(src)) =
                        (p["payload"].as_object_mut(), payload.as_object())
                    {
                        for (k, v) in src {
                            dst.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn set_payload_by_filter(
        &self,
        collection: &str,
        filter: Value,
        payload: Value,
    ) -> Result<(), String> {
        let mut cols = self.cols.lock().unwrap();
        if let Some(pts) = cols.get_mut(collection) {
            for p in pts.iter_mut() {
                if filter_matches(&p["payload"], &filter) {
                    if let (Some(dst), Some(src)) =
                        (p["payload"].as_object_mut(), payload.as_object())
                    {
                        for (k, v) in src {
                            dst.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn scroll(&self, collection: &str, limit: usize) -> Result<Vec<Value>, String> {
        Ok(self
            .cols
            .lock()
            .unwrap()
            .get(collection)
            .map(|v| v.iter().take(limit).cloned().collect())
            .unwrap_or_default())
    }

    async fn scroll_all(
        &self,
        collection: &str,
        filter: Option<Value>,
        cap: usize,
    ) -> Result<Vec<Value>, String> {
        let cols = self.cols.lock().unwrap();
        let Some(pts) = cols.get(collection) else {
            return Ok(vec![]);
        };
        Ok(pts
            .iter()
            .filter(|p| {
                filter
                    .as_ref()
                    .map(|f| filter_matches(&p["payload"], f))
                    .unwrap_or(true)
            })
            .take(cap)
            .cloned()
            .collect())
    }

    async fn delete_by_filter(&self, collection: &str, filter: Value) -> Result<(), String> {
        let mut cols = self.cols.lock().unwrap();
        if let Some(pts) = cols.get_mut(collection) {
            pts.retain(|p| !filter_matches(&p["payload"], &filter));
        }
        Ok(())
    }

    async fn delete_by_doc(&self, collection: &str, doc_id: &str) -> Result<(), String> {
        let mut cols = self.cols.lock().unwrap();
        if let Some(pts) = cols.get_mut(collection) {
            pts.retain(|p| p["payload"]["doc_id"].as_str() != Some(doc_id));
        }
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<(), String> {
        self.cols.lock().unwrap().remove(name);
        Ok(())
    }

    // Not needed by the current offline tests.
    async fn search(
        &self,
        _collection: &str,
        _vector: &[f32],
        _limit: usize,
        _score_threshold: f32,
        _filter: Option<Value>,
    ) -> Result<Vec<Value>, String> {
        Ok(vec![])
    }
    async fn search_hybrid(
        &self,
        _collection: &str,
        _dense: &[f32],
        _sparse: &(Vec<u32>, Vec<f32>),
        _limit: usize,
        _filter: Option<Value>,
    ) -> Result<Vec<Value>, String> {
        Ok(vec![])
    }
    async fn chunks_of_doc(
        &self,
        _collection: &str,
        _doc_id: &str,
        _limit: usize,
    ) -> Result<Vec<String>, String> {
        Ok(vec![])
    }
    async fn doc_chunks_indexed(
        &self,
        _collection: &str,
        _doc_id: &str,
        _limit: usize,
    ) -> Result<Vec<(i64, String)>, String> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_match_range_and_is_empty() {
        let p = json!({ "container_tag": "t1", "is_latest": true, "captured_at": 500 });
        assert!(filter_matches(
            &p,
            &json!({ "must": [{ "key": "container_tag", "match": { "value": "t1" } }] })
        ));
        // must_not is_latest=false does not exclude a latest point.
        assert!(filter_matches(
            &p,
            &json!({ "must_not": [{ "key": "is_latest", "match": { "value": false } }] })
        ));
        // range: captured_at < 400 is false, so a must_not on it does NOT exclude.
        assert!(filter_matches(
            &p,
            &json!({ "must_not": [{ "key": "captured_at", "range": { "lt": 400 } }] })
        ));
        // is_empty matches a missing key.
        assert!(filter_matches(
            &p,
            &json!({ "must": [{ "is_empty": { "key": "needs_review" } }] })
        ));
        // wrong tag is excluded.
        assert!(!filter_matches(
            &p,
            &json!({ "must": [{ "key": "container_tag", "match": { "value": "t2" } }] })
        ));
    }
}
