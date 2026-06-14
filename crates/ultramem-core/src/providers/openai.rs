//! OpenAI embeddings provider — the alternative to Jina, behind config
//! (`ULTRAMEM_EMBEDDER=openai`). Implements the OpenAI `/v1/embeddings` API
//! (`text-embedding-3-small` / `-large`), which is also spoken by Azure OpenAI
//! and OpenAI-compatible gateways — hence the configurable base URL.
//!
//! Note: OpenAI embeddings have a different dimensionality than Jina (1536 for
//! `-small`, 3072 for `-large`), so switching embedders means fresh collections
//! sized to `dim()`. The optional `dimensions` request param can shorten them.

use super::{EmbedTask, Embedder};
use async_trait::async_trait;
use serde_json::{json, Value};

const BATCH: usize = 128;

/// OpenAI (or OpenAI-compatible) embeddings client.
#[derive(Clone)]
pub struct OpenAiEmbedder {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    dim: usize,
}

impl OpenAiEmbedder {
    /// `text-embedding-3-small` (1536-dim) against api.openai.com.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "text-embedding-3-small".into(),
            dim: 1536,
        }
    }

    /// Override the model + its dimensionality (e.g. `text-embedding-3-large`,
    /// 3072 — or a shortened `dim` via the API's `dimensions` param).
    pub fn with_model(mut self, model: impl Into<String>, dim: usize) -> Self {
        self.model = model.into();
        self.dim = dim;
        self
    }

    /// Point at an OpenAI-compatible gateway / Azure deployment.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    async fn embed(&self, _task: EmbedTask, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if self.api_key.is_empty() {
            return Err("no OpenAI API key configured (set OPENAI_API_KEY)".into());
        }
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(inputs.len());
        for batch in inputs.chunks(BATCH) {
            // `dimensions` pins the output length for the `text-embedding-3-*`
            // family; harmless to send and keeps `dim()` authoritative.
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .timeout(std::time::Duration::from_secs(60))
                .json(&json!({ "model": self.model, "input": batch, "dimensions": self.dim }))
                .send()
                .await
                .map_err(|e| format!("openai embeddings unreachable: {e}"))?;
            let status = resp.status();
            let v: Value = resp.json().await.map_err(|e| format!("openai bad response: {e}"))?;
            if !status.is_success() {
                let detail = v["error"]["message"].as_str().unwrap_or("unknown");
                return Err(format!("openai embeddings error {status}: {detail}"));
            }
            let mut data: Vec<&Value> = v["data"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
            if data.len() != batch.len() {
                return Err(format!("openai returned {} embeddings for {} inputs", data.len(), batch.len()));
            }
            data.sort_by_key(|d| d["index"].as_u64().unwrap_or(0));
            for d in data {
                let vec: Vec<f32> = d["embedding"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect())
                    .unwrap_or_default();
                if vec.len() != self.dim {
                    return Err(format!("openai embedding dim {} != {}", vec.len(), self.dim));
                }
                out.push(vec);
            }
        }
        Ok(out)
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn id(&self) -> &str {
        &self.model
    }
}
