//! Qdrant vector store — the default [`VectorStore`]. Thin adapter over the
//! low-level REST client in [`crate::engine::qdrant`]; holds the connection
//! (URL/key) and forwards each op so the engine never sees Qdrant's wire shape.

use super::VectorStore;
use crate::engine::qdrant;
use async_trait::async_trait;
use serde_json::Value;

#[derive(Clone)]
pub struct QdrantStore {
    http: reqwest::Client,
    url: String,
    key: String,
}

impl QdrantStore {
    pub fn new(url: impl Into<String>, key: impl Into<String>) -> Self {
        Self { http: reqwest::Client::new(), url: url.into(), key: key.into() }
    }
}

#[async_trait]
impl VectorStore for QdrantStore {
    async fn health(&self) -> bool {
        qdrant::health(&self.http, &self.url, &self.key).await
    }
    async fn ensure_collection(&self, name: &str, dim: usize) -> Result<(), String> {
        qdrant::ensure_collection(&self.http, &self.url, &self.key, name, dim).await
    }
    async fn ensure_collection_hybrid(&self, name: &str, dim: usize) -> Result<(), String> {
        qdrant::ensure_collection_hybrid(&self.http, &self.url, &self.key, name, dim).await
    }
    async fn ensure_payload_index(&self, collection: &str, field: &str, schema: &str) {
        qdrant::ensure_payload_index(&self.http, &self.url, &self.key, collection, field, schema).await
    }
    async fn upsert(&self, collection: &str, points: Vec<Value>) -> Result<(), String> {
        qdrant::upsert(&self.http, &self.url, &self.key, collection, points).await
    }
    async fn search(
        &self,
        collection: &str,
        vector: &[f32],
        limit: usize,
        score_threshold: f32,
        filter: Option<Value>,
    ) -> Result<Vec<Value>, String> {
        qdrant::search(&self.http, &self.url, &self.key, collection, vector, limit, score_threshold, filter).await
    }
    async fn search_hybrid(
        &self,
        collection: &str,
        dense: &[f32],
        sparse: &(Vec<u32>, Vec<f32>),
        limit: usize,
        filter: Option<Value>,
    ) -> Result<Vec<Value>, String> {
        qdrant::search_hybrid(&self.http, &self.url, &self.key, collection, dense, sparse, limit, filter).await
    }
    async fn set_payload(
        &self,
        collection: &str,
        point_ids: &[String],
        payload: Value,
    ) -> Result<(), String> {
        qdrant::set_payload(&self.http, &self.url, &self.key, collection, point_ids, payload).await
    }
    async fn set_payload_by_filter(
        &self,
        collection: &str,
        filter: Value,
        payload: Value,
    ) -> Result<(), String> {
        qdrant::set_payload_by_filter(&self.http, &self.url, &self.key, collection, filter, payload).await
    }
    async fn scroll(&self, collection: &str, limit: usize) -> Result<Vec<Value>, String> {
        qdrant::scroll(&self.http, &self.url, &self.key, collection, limit).await
    }
    async fn scroll_all(
        &self,
        collection: &str,
        filter: Option<Value>,
        cap: usize,
    ) -> Result<Vec<Value>, String> {
        qdrant::scroll_all(&self.http, &self.url, &self.key, collection, filter, cap).await
    }
    async fn chunks_of_doc(
        &self,
        collection: &str,
        doc_id: &str,
        limit: usize,
    ) -> Result<Vec<String>, String> {
        qdrant::chunks_of_doc(&self.http, &self.url, &self.key, collection, doc_id, limit).await
    }
    async fn doc_chunks_indexed(
        &self,
        collection: &str,
        doc_id: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String)>, String> {
        qdrant::doc_chunks_indexed(&self.http, &self.url, &self.key, collection, doc_id, limit).await
    }
    async fn delete_by_filter(&self, collection: &str, filter: Value) -> Result<(), String> {
        qdrant::delete_by_filter(&self.http, &self.url, &self.key, collection, filter).await
    }
    async fn delete_by_doc(&self, collection: &str, doc_id: &str) -> Result<(), String> {
        qdrant::delete_by_doc(&self.http, &self.url, &self.key, collection, doc_id).await
    }
    async fn delete_collection(&self, name: &str) -> Result<(), String> {
        qdrant::delete_collection(&self.http, &self.url, &self.key, name).await
    }
}
