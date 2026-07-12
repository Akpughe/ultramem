//! UltraMem — open-source memory engine for AI agents.
//!
//! Two layers over a vector store: raw content (chunks) and LLM-distilled facts
//! that are reconciled over time (dedup / UPDATE / EXTEND / NEW) with temporal
//! correctness and per-namespace isolation. See `../../docs/`.
//!
//! ```no_run
//! use ultramem_core::{MemoryEngine, EngineCfg};
//! # async fn demo() -> Result<(), String> {
//! let engine = MemoryEngine::new(EngineCfg::from_env());
//! engine.ensure_collections().await?;
//! let (docs, facts) = engine.retrieve_tagged("user_123", "what do I prefer?", None, 8).await?;
//! # let _ = (docs, facts); Ok(())
//! # }
//! ```

pub mod db;
pub mod engine;
pub mod llm;
pub mod providers;

// Public surface — what `ultramem-server` and embedded consumers use.
pub use engine::{EngineCfg, IngestDoc, MemoryEngine, SearchChunk, SearchResult, DEFAULT_TAG};
pub use llm::{LlmClient, ResolvedModel};
pub use providers::{
    EmbedTask, Embedder, JinaEmbedder, JinaReranker, Llm, MistralOcr, Ocr, OpenAiEmbedder,
    QdrantStore, Reranker, VectorStore,
};
