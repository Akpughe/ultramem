//! Provider abstractions — the seams that make UltraMem provider-agnostic.
//!
//! The engine talks to four external capabilities through traits, never to a
//! concrete vendor:
//!
//! - [`Embedder`] — text → dense vectors (Jina, OpenAI, …)
//! - [`Reranker`] — cross-encoder relevance scoring (Jina, …)
//! - [`Ocr`] — PDF/image → text (Mistral, …)
//! - [`Llm`] — chat/distillation (any OpenAI-compatible or Anthropic model via
//!   [`crate::llm::LlmClient`] + [`crate::llm::ResolvedModel`])
//! - [`VectorStore`] — the index itself (Qdrant by default)
//!
//! [`MemoryEngine`](crate::MemoryEngine) holds these as `Arc<dyn …>`, selected
//! from [`EngineCfg`](crate::EngineCfg) (e.g. `ULTRAMEM_EMBEDDER=openai`) or
//! injected wholesale via the `with_*` builders — so a deployment can swap a
//! provider without touching engine code.

use crate::llm::ResolvedModel;
use async_trait::async_trait;
use serde_json::Value;

pub mod blob;
pub mod jina;
pub mod llm_provider;
pub mod mistral;
#[cfg(test)]
pub mod mock;
pub mod openai;
pub mod qdrant_store;

pub use blob::{BlobStore, LocalFsBlobStore};
pub use jina::{JinaEmbedder, JinaReranker};
pub use mistral::MistralOcr;
pub use openai::OpenAiEmbedder;
pub use qdrant_store::QdrantStore;

/// What an embedding is *for*. Some providers (Jina) adapt the vector to the
/// task; others (OpenAI) ignore it. The engine always says which it wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedTask {
    /// Content being ingested and stored.
    Passage,
    /// A query being matched against stored passages.
    Query,
}

/// Text → dense vectors. `dim` MUST equal the length of every returned vector;
/// the engine sizes its collections from it, so a wrong value corrupts the index.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed `inputs`, one vector per input, in order. Implementations batch
    /// internally as needed.
    async fn embed(&self, task: EmbedTask, inputs: &[String]) -> Result<Vec<Vec<f32>>, String>;
    /// Dimensionality of the vectors this embedder produces.
    fn dim(&self) -> usize;
    /// Short identifier for logs/health (e.g. "jina-embeddings-v3").
    fn id(&self) -> &str;
}

/// Cross-encoder rerank: score each document against the query. Returns
/// `(original_index, relevance_score)` best-first. This is the precision gate
/// that separates "contains similar words" from "actually answers this".
#[async_trait]
pub trait Reranker: Send + Sync {
    async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<(usize, f64)>, String>;
}

/// PDF/image → text. `image_mime` classifies a path as a supported image type
/// (returns its MIME) or `None` for "treat as a document / PDF".
#[async_trait]
pub trait Ocr: Send + Sync {
    async fn ocr_pdf(&self, bytes: &[u8]) -> Result<String, String>;
    async fn ocr_image(&self, bytes: &[u8], mime: &str) -> Result<String, String>;
    fn image_mime(&self, path: &str) -> Option<&'static str>;
}

/// Chat completion. The model (provider, base URL, key) is carried by
/// [`ResolvedModel`], so one client serves OpenAI-compatible and Anthropic
/// backends; selection is `EngineCfg::with_models`.
#[async_trait]
pub trait Llm: Send + Sync {
    async fn chat(
        &self,
        m: &ResolvedModel,
        system: &str,
        user: &str,
        temperature: f64,
    ) -> Result<String, String>;
    async fn complete(
        &self,
        m: &ResolvedModel,
        messages: Value,
        temperature: f64,
    ) -> Result<String, String>;
}

/// The vector index. Default impl is [`QdrantStore`]; the trait keeps the engine
/// from hardwiring Qdrant's REST shape. Methods take a collection name and the
/// op's parameters; connection state (URL/key/client) lives in the impl. Values
/// are Qdrant-shaped point objects (`{id, score, payload}`) — the seam is the
/// transport, not the document model.
#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn health(&self) -> bool;
    async fn ensure_collection(&self, name: &str, dim: usize) -> Result<(), String>;
    async fn ensure_collection_hybrid(&self, name: &str, dim: usize) -> Result<(), String>;
    async fn ensure_payload_index(&self, collection: &str, field: &str, schema: &str);
    async fn upsert(&self, collection: &str, points: Vec<Value>) -> Result<(), String>;
    async fn search(
        &self,
        collection: &str,
        vector: &[f32],
        limit: usize,
        score_threshold: f32,
        filter: Option<Value>,
    ) -> Result<Vec<Value>, String>;
    async fn search_hybrid(
        &self,
        collection: &str,
        dense: &[f32],
        sparse: &(Vec<u32>, Vec<f32>),
        limit: usize,
        filter: Option<Value>,
    ) -> Result<Vec<Value>, String>;
    async fn set_payload(
        &self,
        collection: &str,
        point_ids: &[String],
        payload: Value,
    ) -> Result<(), String>;
    async fn set_payload_by_filter(
        &self,
        collection: &str,
        filter: Value,
        payload: Value,
    ) -> Result<(), String>;
    async fn scroll(&self, collection: &str, limit: usize) -> Result<Vec<Value>, String>;
    async fn scroll_all(
        &self,
        collection: &str,
        filter: Option<Value>,
        cap: usize,
    ) -> Result<Vec<Value>, String>;
    async fn chunks_of_doc(
        &self,
        collection: &str,
        doc_id: &str,
        limit: usize,
    ) -> Result<Vec<String>, String>;
    async fn doc_chunks_indexed(
        &self,
        collection: &str,
        doc_id: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String)>, String>;
    async fn delete_by_filter(&self, collection: &str, filter: Value) -> Result<(), String>;
    async fn delete_by_doc(&self, collection: &str, doc_id: &str) -> Result<(), String>;
    async fn delete_collection(&self, name: &str) -> Result<(), String>;
}
