//! Jina providers — the default embedder and reranker. Thin adapters over the
//! low-level client in [`crate::engine::jina`].

use super::{EmbedTask, Embedder, Reranker};
use crate::engine::jina;
use async_trait::async_trait;

/// `jina-embeddings-v3`, 1024-dim, task-adapted vectors.
#[derive(Clone)]
pub struct JinaEmbedder {
    http: reqwest::Client,
    api_key: String,
}

impl JinaEmbedder {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
        }
    }
}

#[async_trait]
impl Embedder for JinaEmbedder {
    async fn embed(&self, task: EmbedTask, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let task = match task {
            EmbedTask::Passage => "retrieval.passage",
            EmbedTask::Query => "retrieval.query",
        };
        jina::embed(&self.http, &self.api_key, task, inputs).await
    }
    fn dim(&self) -> usize {
        jina::DIM
    }
    fn id(&self) -> &str {
        jina::MODEL
    }
}

/// `jina-reranker-v2-base-multilingual` cross-encoder.
#[derive(Clone)]
pub struct JinaReranker {
    http: reqwest::Client,
    api_key: String,
}

impl JinaReranker {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
        }
    }
}

#[async_trait]
impl Reranker for JinaReranker {
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<(usize, f64)>, String> {
        jina::rerank(&self.http, &self.api_key, query, documents).await
    }
}
