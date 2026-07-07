//! UltraMem's memory engine: chunking, embeddings, vector search, OCR, and
//! fact distillation — built on Qdrant (vectors), Jina (embeddings), Mistral
//! (OCR), and an OpenAI-compatible/Anthropic LLM (distillation). No ML runs
//! locally; everything is an HTTP call, so this is a thin async library.
//!
//! Pipeline per document (synchronous — returning Ok means fully indexed):
//!   PDF/image? → OCR → chunk → embed → Qdrant upsert (chunks)
//!                            → distill facts → memory lifecycle → upsert (facts)

pub mod chunker;
pub mod context;
pub mod distill;
pub mod extract;
pub mod graph;
pub mod jina;
pub mod memory;
pub mod mistral;
pub mod profile;
pub mod qdrant;
pub mod rewrite;
pub mod sparse;
pub mod urlinfo;

use crate::llm::{LlmClient, ResolvedModel};
use crate::providers::{
    EmbedTask, Embedder, JinaEmbedder, JinaReranker, Llm, MistralOcr, Ocr, OpenAiEmbedder,
    QdrantStore, Reranker, VectorStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Score floor for chunk hits (cosine, jina-v3). Mirrors the old
/// supermemory `chunkThreshold: 0.4` intent; jina cosine scores run lower.
const CHUNK_THRESHOLD: f32 = 0.30;
const FACT_THRESHOLD: f32 = 0.30;
/// Bar for the unfiltered retry after a filtered search found nothing.
const FALLBACK_THRESHOLD: f32 = 0.45;
/// Cross-encoder relevance floor — candidates below this don't answer the
/// question and are dropped rather than padding the sources list.
const RERANK_THRESHOLD: f64 = 0.15;
/// Hard cap on document size entering the chunker (~50 chunks).
const MAX_DOC_CHARS: usize = 60_000;

/// The default namespace. The local single-user app and all legacy data (which
/// predates the `container_tag` payload field) live here. Multi-tenant callers
/// pass their own tag to hard-isolate a memory pool (one per user or per agent).
pub const DEFAULT_TAG: &str = "default";

#[derive(Debug, Clone)]
pub struct EngineCfg {
    pub qdrant_url: String,
    pub qdrant_api_key: String,
    pub jina_api_key: String,
    pub mistral_api_key: String,
    /// Legacy single Groq key — still read by the probe/bench harness.
    pub groq_api_key: String,
    /// Which embedder `MemoryEngine::new` builds: `"jina"` (default) or
    /// `"openai"`. Env: `ULTRAMEM_EMBEDDER`. Inject any `dyn Embedder` directly
    /// with `MemoryEngine::with_embedder` to bypass this entirely.
    pub embedder: String,
    /// OpenAI embeddings config (used when `embedder == "openai"`).
    pub openai_api_key: String,
    pub openai_embed_model: String,
    pub openai_embed_dim: usize,
    pub chunks_collection: String,
    pub facts_collection: String,
    /// Tier-3 bi-temporal knowledge-graph collection (entity-attribute edges
    /// with event time). Only used when `temporal_graph` is on.
    pub graph_collection: String,
    /// Resolved model for retrieval planning (the Plan role).
    pub plan_model: ResolvedModel,
    /// Resolved model for fact distillation (the Distill role).
    pub distill_model: ResolvedModel,
    /// Contextual Retrieval: prefix each chunk's embedding with a one-line
    /// doc-level situating blurb (see `context.rs`). On in production; the A/B
    /// bench flips it to isolate the effect.
    pub contextual: bool,
    /// Fact-augmented keys (LongMemEval +9.4% recall): distill the document's
    /// facts BEFORE embedding chunks and fold a compact fact summary into each
    /// chunk's embedding key (stored content stays raw; the distilled facts are
    /// reused for memory indexing — no second distill pass). OFF by default so
    /// production keeps the "chunks searchable immediately" order (distill runs
    /// after upsert); turning it on makes distillation block the chunk path.
    pub fact_augmented_keys: bool,
    /// Run fact distillation at ingest. On in production; the A/B bench turns
    /// it off so a chunk-retrieval comparison isn't slowed by the (unrelated)
    /// distillation passes.
    pub distill: bool,
    /// Run the memory lifecycle (reconcile new facts against existing memories:
    /// dedup, UPDATE→flip is_latest, EXTEND edges). On in production.
    pub memory_graph: bool,
    /// Content-type-aware chunking (markdown by heading, transcripts by speaker
    /// turn). On in production; A/B flips it to compare against paragraph-only.
    pub smart_chunking: bool,
    /// Hybrid retrieval: dense + sparse (BM25/IDF) vectors fused server-side
    /// with RRF. Requires a hybrid-schema collection, so it's OFF by default
    /// (the existing dense-only index would need a re-index to enable it). The
    /// A/B harness builds fresh hybrid collections to measure the gain.
    pub hybrid_search: bool,
    /// Multi-query retrieval: when the planner rewrites the question, search
    /// with BOTH the rewritten and the original query and union the candidate
    /// pool before reranking. Cheap recall boost (one extra embed + search),
    /// purely query-side. The reranker remains the precision gate.
    pub multi_query: bool,
    /// Fetch full web page bodies (Jina Reader) for browser captures instead of
    /// indexing URL metadata only. OFF by default: it sends every visited URL
    /// to a third party and fetches its content — a deliberate privacy choice.
    pub fetch_web_bodies: bool,
    /// Tier-3: extract bi-temporal entity-attribute edges at ingest and resolve
    /// them at answer time (see `graph.rs`). Additive and non-fatal; OFF by
    /// default (adds one extraction LLM pass per document and a graph
    /// collection). Turns knowledge-update's "use the latest value" from an LLM
    /// guess into a deterministic event-time query.
    pub temporal_graph: bool,
}

impl Default for EngineCfg {
    fn default() -> Self {
        Self {
            qdrant_url: String::new(),
            qdrant_api_key: String::new(),
            jina_api_key: String::new(),
            mistral_api_key: String::new(),
            groq_api_key: String::new(),
            embedder: "jina".into(),
            openai_api_key: String::new(),
            openai_embed_model: "text-embedding-3-small".into(),
            openai_embed_dim: 1536,
            chunks_collection: "ultramem_chunks".into(),
            facts_collection: "ultramem_facts".into(),
            graph_collection: "ultramem_graph".into(),
            plan_model: ResolvedModel::groq(String::new(), "llama-3.3-70b-versatile"),
            distill_model: ResolvedModel::groq(String::new(), "openai/gpt-oss-120b"),
            // Contextual Retrieval is OFF by default: A/B on real docs showed
            // no doc-level retrieval gain (slightly negative) for a per-doc LLM
            // cost. Kept behind the flag to revisit with a chunk-level metric.
            contextual: false,
            fact_augmented_keys: false,
            distill: true,
            memory_graph: true,
            smart_chunking: true,
            hybrid_search: false,
            multi_query: true,
            fetch_web_bodies: false,
            temporal_graph: false,
        }
    }
}

impl EngineCfg {
    /// Build from environment variables. Provider keys: `QDRANT_URL`,
    /// `QDRANT_API_KEY`, `JINA_API_KEY`, `MISTRAL_API_KEY`, `GROQ_API_KEY`.
    /// The plan/distill models default to Groq; override with `with_models`.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        let groq_key = var("GROQ_API_KEY");
        Self {
            qdrant_url: var("QDRANT_URL"),
            qdrant_api_key: var("QDRANT_API_KEY"),
            jina_api_key: var("JINA_API_KEY"),
            mistral_api_key: var("MISTRAL_API_KEY"),
            embedder: {
                let e = var("ULTRAMEM_EMBEDDER");
                if e.is_empty() {
                    "jina".into()
                } else {
                    e
                }
            },
            openai_api_key: var("OPENAI_API_KEY"),
            plan_model: ResolvedModel::groq(groq_key.clone(), "llama-3.3-70b-versatile"),
            distill_model: ResolvedModel::groq(groq_key.clone(), "openai/gpt-oss-120b"),
            groq_api_key: groq_key,
            // Optional collection-name overrides — handy to point the harness at
            // throwaway collections without disturbing the default namespace.
            chunks_collection: {
                let c = var("ULTRAMEM_CHUNKS_COLLECTION");
                if c.is_empty() {
                    "ultramem_chunks".into()
                } else {
                    c
                }
            },
            facts_collection: {
                let c = var("ULTRAMEM_FACTS_COLLECTION");
                if c.is_empty() {
                    "ultramem_facts".into()
                } else {
                    c
                }
            },
            graph_collection: {
                let c = var("ULTRAMEM_GRAPH_COLLECTION");
                if c.is_empty() {
                    "ultramem_graph".into()
                } else {
                    c
                }
            },
            ..Default::default()
        }
    }

    /// Override the planning and distillation models (e.g. OpenAI/Anthropic via
    /// `ResolvedModel`). Chainable on top of `from_env`/`default`.
    pub fn with_models(mut self, plan: ResolvedModel, distill: ResolvedModel) -> Self {
        self.plan_model = plan;
        self.distill_model = distill;
        self
    }
}

/// A document entering the engine. `file_path` triggers OCR for PDFs.
#[derive(Debug, Clone)]
pub struct IngestDoc {
    pub source: String,
    pub title: String,
    pub content: String,
    pub reference: String,
    pub app: String,
    pub captured_at: i64,
    pub file_path: Option<String>,
    /// Namespace this document belongs to. Empty = `DEFAULT_TAG`. Hard-isolates
    /// memory pools across users/agents in a multi-tenant deployment.
    pub container_tag: String,
}

impl IngestDoc {
    /// The effective namespace: the document's tag, or the default when blank.
    fn tag(&self) -> &str {
        if self.container_tag.is_empty() {
            DEFAULT_TAG
        } else {
            &self.container_tag
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchChunk {
    pub content: String,
    #[serde(default)]
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    #[serde(default)]
    pub document_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub chunks: Vec<SearchChunk>,
}

pub struct MemoryEngine {
    /// Kept only for Jina **Reader** file/URL extraction (`extract.rs`) — text
    /// extraction, distinct from the embedder/OCR providers.
    http: reqwest::Client,
    embedder: Arc<dyn Embedder>,
    reranker: Arc<dyn Reranker>,
    ocr: Arc<dyn Ocr>,
    llm: Arc<dyn Llm>,
    store: Arc<dyn VectorStore>,
    cfg: RwLock<EngineCfg>,
    /// Cached standing profile per namespace: tag → (profile, computed_at_unix).
    /// Recompiled lazily when older than `PROFILE_TTL`.
    profile_cache: RwLock<HashMap<String, (profile::Profile, i64)>>,
}

impl MemoryEngine {
    /// Build the engine, selecting providers from `cfg` — the embedder follows
    /// `cfg.embedder` (`"jina"` / `"openai"`); reranker/OCR/store are the
    /// defaults (Jina / Mistral / Qdrant). Override any of them afterwards with
    /// the `with_*` builders, which is how you swap a provider without touching
    /// engine code.
    pub fn new(cfg: EngineCfg) -> Self {
        let embedder: Arc<dyn Embedder> = match cfg.embedder.as_str() {
            "openai" => Arc::new(
                OpenAiEmbedder::new(cfg.openai_api_key.clone())
                    .with_model(cfg.openai_embed_model.clone(), cfg.openai_embed_dim),
            ),
            _ => Arc::new(JinaEmbedder::new(cfg.jina_api_key.clone())),
        };
        Self {
            http: reqwest::Client::new(),
            embedder,
            reranker: Arc::new(JinaReranker::new(cfg.jina_api_key.clone())),
            ocr: Arc::new(MistralOcr::new(cfg.mistral_api_key.clone())),
            llm: Arc::new(LlmClient::new()),
            store: Arc::new(QdrantStore::new(
                cfg.qdrant_url.clone(),
                cfg.qdrant_api_key.clone(),
            )),
            cfg: RwLock::new(cfg),
            profile_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Swap the embedder (e.g. a custom provider) without touching engine code.
    /// Remember its `dim()` must match the collections you ensure/use.
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = embedder;
        self
    }
    /// Swap the reranker.
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = reranker;
        self
    }
    /// Swap the OCR provider.
    pub fn with_ocr(mut self, ocr: Arc<dyn Ocr>) -> Self {
        self.ocr = ocr;
        self
    }
    /// Swap the LLM client.
    pub fn with_llm(mut self, llm: Arc<dyn Llm>) -> Self {
        self.llm = llm;
        self
    }
    /// Swap the vector store (e.g. a non-Qdrant backend).
    pub fn with_store(mut self, store: Arc<dyn VectorStore>) -> Self {
        self.store = store;
        self
    }

    pub fn update_cfg(&self, cfg: EngineCfg) {
        *self.cfg.write().unwrap() = cfg;
    }

    fn cfg(&self) -> EngineCfg {
        self.cfg.read().unwrap().clone()
    }

    /// Identifier of the active embedder (e.g. `"jina-embeddings-v3"`).
    pub fn embedder_id(&self) -> &str {
        self.embedder.id()
    }
    /// Vector dimensionality of the active embedder — the size its collections
    /// are created with.
    pub fn embedder_dim(&self) -> usize {
        self.embedder.dim()
    }

    /// Engine is usable: Qdrant reachable and an embedding key present.
    pub async fn health(&self) -> bool {
        let cfg = self.cfg();
        if cfg.qdrant_url.is_empty() || cfg.jina_api_key.is_empty() {
            return false;
        }
        self.store.health().await
    }

    /// Create both collections (and the payload indexes filtered search
    /// needs) if missing. Call once on startup.
    pub async fn ensure_collections(&self) -> Result<(), String> {
        let cfg = self.cfg();
        // Chunks go hybrid (dense+sparse) when enabled; facts stay dense (the
        // fact layer is short, semantic, and searched dense-only).
        if cfg.hybrid_search {
            self.store
                .ensure_collection_hybrid(&cfg.chunks_collection, self.embedder.dim())
                .await?;
        } else {
            self.store
                .ensure_collection(&cfg.chunks_collection, self.embedder.dim())
                .await?;
        }
        self.store
            .ensure_collection(&cfg.facts_collection, self.embedder.dim())
            .await?;
        for c in [&cfg.chunks_collection, &cfg.facts_collection] {
            self.store
                .ensure_payload_index(c, "source", "keyword")
                .await;
            self.store
                .ensure_payload_index(c, "captured_at", "integer")
                .await;
            // Namespace isolation filter (per-user / per-agent pools).
            self.store
                .ensure_payload_index(c, "container_tag", "keyword")
                .await;
        }
        // Memory lifecycle filtering (exclude superseded + expired facts).
        self.store
            .ensure_payload_index(&cfg.facts_collection, "is_latest", "bool")
            .await;
        self.store
            .ensure_payload_index(&cfg.facts_collection, "valid_until", "integer")
            .await;
        // Tier-3 bi-temporal knowledge graph: a dense collection of edges, keyed
        // by subject/predicate for grouping and valid_from for event-time order.
        if cfg.temporal_graph {
            self.store
                .ensure_collection(&cfg.graph_collection, self.embedder.dim())
                .await?;
            for (field, kind) in [
                ("subject", "keyword"),
                ("predicate", "keyword"),
                ("is_latest", "bool"),
                ("valid_from", "integer"),
                ("valid_to", "integer"),
                ("captured_at", "integer"),
                ("container_tag", "keyword"),
            ] {
                self.store
                    .ensure_payload_index(&cfg.graph_collection, field, kind)
                    .await;
            }
        }
        Ok(())
    }

    /// Ingest one document end to end. Returning Ok(doc_id) means chunks are
    /// embedded, upserted, and searchable. Fact distillation runs inside this
    /// call too but is non-fatal.
    pub async fn add_document(&self, doc: &IngestDoc) -> Result<String, String> {
        let cfg = self.cfg();
        let doc_id = uuid::Uuid::new_v4().to_string();

        // 1. Content. For files routed here (PDFs, Office docs), extract a body
        // and append it to the capturer's header (name, path, dates — date
        // questions depend on it). Hybrid extraction: Jina Reader first (clean
        // markdown for text PDFs / Office / HTML, cross-platform); if it finds
        // no text layer, fall back to Mistral OCR for PDFs (scanned/image) and
        // to local `textutil` for Office docs.
        const MIN_EXTRACT: usize = 24;
        let content = match &doc.file_path {
            Some(p) => {
                let bytes = tokio::fs::read(p)
                    .await
                    .map_err(|e| format!("read {p}: {e}"))?;
                let filename = std::path::Path::new(p)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".into());
                let lower = p.to_lowercase();
                let body = if let Some(mime) = self.ocr.image_mime(&lower) {
                    // Images (screenshots, photos): OCR directly — neither Jina
                    // Reader nor textutil can read pixels.
                    self.ocr.ocr_image(&bytes, mime).await?
                } else {
                    match extract::jina(&self.http, &cfg.jina_api_key, bytes.clone(), &filename)
                        .await
                    {
                        Ok(t) if t.chars().count() >= MIN_EXTRACT => t,
                        jina => {
                            if let Err(e) = &jina {
                                eprintln!(
                                    "[recally] jina reader extract failed for '{filename}': {e}"
                                );
                            }
                            if lower.ends_with(".pdf") {
                                self.ocr.ocr_pdf(&bytes).await?
                            } else {
                                extract::local(std::path::Path::new(p)).unwrap_or_default()
                            }
                        }
                    }
                };
                if body.trim().is_empty() {
                    return Err(format!("no extractable text in {filename}"));
                }
                format!("{}:\n{body}", doc.content)
            }
            None => doc.content.clone(),
        };

        // Optionally enrich a browser capture with the page's actual body
        // (Jina Reader). Off by default — fetching every visited URL is a
        // privacy decision. Failures are non-fatal: keep the URL metadata.
        let content = if cfg.fetch_web_bodies
            && doc.source == "browser"
            && (doc.reference.starts_with("http://") || doc.reference.starts_with("https://"))
        {
            match extract::jina_url(&self.http, &cfg.jina_api_key, &doc.reference).await {
                Ok(body) if body.chars().count() >= 200 => format!("{content}\n\n{body}"),
                _ => content,
            }
        } else {
            content
        };
        let content: String = content.chars().take(MAX_DOC_CHARS).collect();

        // 2. Chunk — strategy follows content type (markdown by heading,
        // transcript by speaker turn, else paragraph).
        let chunks = chunker::chunk_doc(
            &content,
            &doc.source,
            doc.file_path.as_deref(),
            cfg.smart_chunking,
        );
        if chunks.is_empty() {
            return Err("empty content".into());
        }

        // 2b. Fact-augmented keys (T1.2): when enabled, distill the doc's facts
        // BEFORE embedding so a compact fact summary can enrich each chunk's
        // embedding key (paper: +9.4% recall@k). The facts are reused for memory
        // indexing below — no second distill. Off by default (production keeps
        // the distill-after-upsert order so chunks are searchable immediately).
        let do_distill = cfg.distill && content.chars().count() >= 280;
        let early_facts: Option<Vec<String>> = if cfg.fact_augmented_keys && do_distill {
            Some(
                distill::distill_facts(
                    self.llm.as_ref(),
                    &cfg.distill_model,
                    &doc.title,
                    &content,
                    &date_str(doc.captured_at),
                )
                .await
                .unwrap_or_else(|e| {
                    eprintln!("[recally] distill failed for '{}': {e}", doc.title);
                    vec![]
                }),
            )
        } else {
            None
        };

        // 3. Embed. Titles and filenames carry meaning the body often never
        // repeats ("newton-profile.pdf" describes every page of it), so every
        // chunk's embedding input is prefixed with a readable form of the
        // title. Contextual Retrieval adds a one-line doc-level blurb, and
        // fact-augmented keys add a compact distilled-fact summary, on top — so
        // each chunk embeds with awareness of the whole document. The stored
        // chunk text stays clean — this only shapes vectors.
        let blurb = if cfg.contextual {
            context::doc_context(self.llm.as_ref(), &cfg.distill_model, &doc.title, &content).await
        } else {
            None
        };
        let fact_key = early_facts.as_ref().filter(|f| !f.is_empty()).map(|f| {
            f.iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
                .chars()
                .take(600)
                .collect::<String>()
        });
        let augmentation = match (blurb, fact_key) {
            (Some(b), Some(f)) => Some(format!("{b}\n{f}")),
            (Some(b), None) => Some(b),
            (None, Some(f)) => Some(f),
            (None, None) => None,
        };
        let inputs: Vec<String> = chunks
            .iter()
            .map(|c| embed_input(&doc.title, augmentation.as_deref(), c))
            .collect();
        let vectors = self.embedder.embed(EmbedTask::Passage, &inputs).await?;

        // 4. Upsert chunks — after this the document is searchable. In hybrid
        // mode each point carries a named `dense` vector plus a `text` sparse
        // vector (term frequencies over the same input the dense side saw).
        let points: Vec<Value> = chunks
            .iter()
            .zip(vectors.iter())
            .enumerate()
            .map(|(i, (chunk, vec))| {
                let vector = if cfg.hybrid_search {
                    let (indices, values) = sparse::sparse_vector(&inputs[i]);
                    json!({ "dense": vec, "text": { "indices": indices, "values": values } })
                } else {
                    json!(vec)
                };
                json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "vector": vector,
                    "payload": {
                        "doc_id": doc_id,
                        "chunk_index": i,
                        "content": chunk,
                        "title": doc.title,
                        "source": doc.source,
                        "reference": doc.reference,
                        "app": doc.app,
                        "captured_at": doc.captured_at,
                        "container_tag": doc.tag(),
                    },
                })
            })
            .collect();
        self.store.upsert(&cfg.chunks_collection, points).await?;

        // 5. Index memories. Tiny captures carry no facts beyond their own text
        // (already embedded), so distillation is skipped for them.
        match early_facts {
            // Fact-augmented path: facts were already distilled in step 2b.
            Some(facts) => {
                if !facts.is_empty() {
                    if let Err(e) = self.index_memories(&cfg, doc, &doc_id, facts).await {
                        eprintln!("[recally] memory indexing failed for '{}': {e}", doc.title);
                    }
                }
            }
            // Default path: distill AFTER upsert (chunks already searchable).
            None => {
                if !do_distill {
                    return Ok(doc_id);
                }
                match distill::distill_facts(
                    self.llm.as_ref(),
                    &cfg.distill_model,
                    &doc.title,
                    &content,
                    &date_str(doc.captured_at),
                )
                .await
                {
                    Ok(facts) if !facts.is_empty() => {
                        if let Err(e) = self.index_memories(&cfg, doc, &doc_id, facts).await {
                            eprintln!("[recally] memory indexing failed for '{}': {e}", doc.title);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("[recally] distill failed for '{}': {e}", doc.title),
                }
            }
        }

        // 6. Tier-3: extract bi-temporal edges into the knowledge graph. Additive
        // and non-fatal — failures never block the (already-searchable) chunks.
        if cfg.temporal_graph && do_distill {
            match graph::extract_edges(
                self.llm.as_ref(),
                &cfg.distill_model,
                &doc.title,
                &content,
                &date_str(doc.captured_at),
            )
            .await
            {
                Ok(edges) if !edges.is_empty() => {
                    if let Err(e) = self.index_edges(&cfg, doc, &doc_id, edges).await {
                        eprintln!("[recally] edge indexing failed for '{}': {e}", doc.title);
                    }
                }
                Ok(_) => {}
                Err(e) => eprintln!("[recally] edge extraction failed for '{}': {e}", doc.title),
            }
        }

        Ok(doc_id)
    }

    /// Graph-only ingest: extract and store bi-temporal edges for a document
    /// WITHOUT touching chunks or distilled facts. Augments an existing
    /// chunk/fact index with the knowledge graph in a fast backfill pass, and
    /// keeps an A/B clean — retrieval is unchanged, only the graph is added.
    pub async fn add_document_graph_only(&self, doc: &IngestDoc) -> Result<(), String> {
        let cfg = self.cfg();
        if !cfg.temporal_graph || doc.content.chars().count() < 280 {
            return Ok(());
        }
        let doc_id = uuid::Uuid::new_v4().to_string();
        let edges = graph::extract_edges(
            self.llm.as_ref(),
            &cfg.distill_model,
            &doc.title,
            &doc.content,
            &date_str(doc.captured_at),
        )
        .await?;
        if !edges.is_empty() {
            self.index_edges(&cfg, doc, &doc_id, edges).await?;
        }
        Ok(())
    }

    /// Ingest a web page: fetch + clean it via Jina Reader, then run the normal
    /// pipeline (chunk → embed → distill → reconcile). `tag` empty = default
    /// namespace. Returns the new document id. Errors if the page yields no text.
    pub async fn add_url(
        &self,
        url: &str,
        title: Option<String>,
        tag: &str,
        captured_at: i64,
    ) -> Result<String, String> {
        let cfg = self.cfg();
        let body = extract::jina_url(&self.http, &cfg.jina_api_key, url).await?;
        if body.trim().is_empty() {
            return Err(format!("no extractable text at {url}"));
        }
        let title = title.unwrap_or_default();
        let header = if title.is_empty() {
            url.to_string()
        } else {
            format!("{title} — {url}")
        };
        let doc = IngestDoc {
            source: "web".into(),
            title,
            content: format!("{header}\n\n{body}"),
            reference: url.to_string(),
            app: String::new(),
            captured_at,
            file_path: None,
            container_tag: tag.to_string(),
        };
        self.add_document(&doc).await
    }

    /// Index a document's distilled facts as memories, reconciling each against
    /// existing memories (the lifecycle in `memory.rs`): dedup, UPDATE (flip the
    /// old memory's is_latest), EXTEND (edge), or NEW. When `memory_graph` is
    /// off, every fact is stored plainly (the pre-lifecycle behaviour).
    /// Non-fatal: chunks are already searchable by the time this runs.
    async fn index_memories(
        &self,
        cfg: &EngineCfg,
        doc: &IngestDoc,
        doc_id: &str,
        facts: Vec<String>,
    ) -> Result<(), String> {
        // Strip any "[until YYYY-MM-DD]" expiry suffix before embedding, so the
        // vector reflects the fact, not the bookkeeping. `facts` below is the
        // cleaned text; `expiry` maps clean text → valid_until.
        let parsed: Vec<(String, Option<i64>)> =
            facts.iter().map(|f| memory::parse_expiry(f)).collect();
        let facts: Vec<String> = parsed.iter().map(|(f, _)| f.clone()).collect();
        let expiry: std::collections::HashMap<&str, Option<i64>> = facts
            .iter()
            .map(|s| s.as_str())
            .zip(parsed.iter().map(|(_, e)| *e))
            .collect();
        let fvecs = self.embedder.embed(EmbedTask::Passage, &facts).await?;

        // Reconcile against the existing memory graph (skip when disabled — then
        // every fact is simply NEW).
        let actions = if cfg.memory_graph {
            // For each new fact, find its single nearest *latest* memory.
            let mut with_neighbors: Vec<(String, Option<memory::Neighbor>)> =
                Vec::with_capacity(facts.len());
            for (fact, vec) in facts.iter().zip(fvecs.iter()) {
                // Reconcile only within the same namespace — a fact in tenant A
                // must never supersede or extend a memory in tenant B.
                let neighbor_filter = tagged_filter(
                    Some(active_facts_filter(None, chrono::Utc::now().timestamp())),
                    doc.tag(),
                );
                let hits = self
                    .store
                    .search(
                        &cfg.facts_collection,
                        vec,
                        1,
                        memory::RELATE_THRESHOLD,
                        Some(neighbor_filter),
                    )
                    .await
                    .unwrap_or_default();
                let neighbor = hits.first().and_then(|h| {
                    Some(memory::Neighbor {
                        memory_id: h["id"].as_str()?.to_string(),
                        fact: h["payload"]["fact"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        score: h["score"].as_f64().unwrap_or(0.0) as f32,
                    })
                });
                with_neighbors.push((fact.clone(), neighbor));
            }
            memory::reconcile(self.llm.as_ref(), &cfg.distill_model, with_neighbors).await
        } else {
            facts
                .iter()
                .map(|f| memory::Action {
                    fact: f.clone(),
                    relation: memory::Relation::New,
                    supersedes: None,
                    extends: None,
                })
                .collect()
        };

        // Build points for everything that survives (drop DUPLICATEs), and
        // collect the ids of memories that got superseded.
        let fact_vec: std::collections::HashMap<&str, &Vec<f32>> =
            facts.iter().map(|s| s.as_str()).zip(fvecs.iter()).collect();
        let mut points: Vec<Value> = Vec::new();
        let mut superseded: Vec<String> = Vec::new();
        for action in &actions {
            if action.relation == memory::Relation::Duplicate {
                continue;
            }
            if let Some(old) = &action.supersedes {
                superseded.push(old.clone());
            }
            let Some(vec) = fact_vec.get(action.fact.as_str()) else {
                continue;
            };
            points.push(json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "vector": vec,
                "payload": {
                    "doc_id": doc_id,
                    "fact": action.fact,
                    "title": doc.title,
                    "source": doc.source,
                    "captured_at": doc.captured_at,
                    "is_latest": true,
                    "kind": "fact",
                    "supersedes": action.supersedes,
                    "extends": action.extends,
                    "valid_until": expiry.get(action.fact.as_str()).copied().flatten(),
                    "container_tag": doc.tag(),
                },
            }));
        }

        self.store.upsert(&cfg.facts_collection, points).await?;

        // Flag superseded memories as no longer latest (history preserved).
        if !superseded.is_empty() {
            if let Err(e) = self
                .store
                .set_payload(
                    &cfg.facts_collection,
                    &superseded,
                    json!({ "is_latest": false }),
                )
                .await
            {
                eprintln!(
                    "[recally] failed to flag {} superseded memories: {e}",
                    superseded.len()
                );
            }
        }
        Ok(())
    }

    /// Index a document's extracted edges into the bi-temporal knowledge graph,
    /// applying deterministic event-time supersession (`graph::supersession`).
    /// Non-fatal: a failure here never blocks the already-searchable chunks.
    async fn index_edges(
        &self,
        cfg: &EngineCfg,
        doc: &IngestDoc,
        doc_id: &str,
        edges: Vec<graph::Edge>,
    ) -> Result<(), String> {
        if edges.is_empty() {
            return Ok(());
        }
        // A readable statement per edge — embedded for answer-time lookup.
        let statements: Vec<String> = edges.iter().map(edge_statement).collect();
        let vecs = self.embedder.embed(EmbedTask::Passage, &statements).await?;

        // Existing latest edges in this namespace, grouped by (subject,
        // predicate), for cross-session supersession.
        let mut by_group: std::collections::HashMap<(String, String), Vec<graph::StoredEdge>> =
            std::collections::HashMap::new();
        for se in self.latest_edges(cfg, doc.tag()).await {
            by_group
                .entry((se.edge.subject.clone(), se.edge.predicate.clone()))
                .or_default()
                .push(se);
        }

        let mut points: Vec<Value> = Vec::new();
        let mut superseded: Vec<String> = Vec::new();
        for ((edge, vec), statement) in edges.iter().zip(vecs.iter()).zip(statements.iter()) {
            let empty: &[graph::StoredEdge] = &[];
            let group = by_group
                .get(&(edge.subject.clone(), edge.predicate.clone()))
                .map(|v| v.as_slice())
                .unwrap_or(empty);
            let (sup, is_latest) = graph::supersession(edge, group);
            superseded.extend(sup);
            points.push(json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "vector": vec,
                "payload": {
                    "kind": "edge",
                    "subject": edge.subject,
                    "predicate": edge.predicate,
                    "object": edge.object,
                    "valid_from": edge.valid_from,
                    "valid_to": edge.valid_to,
                    "singular": edge.singular,
                    "is_latest": is_latest,
                    "captured_at": doc.captured_at,
                    "doc_id": doc_id,
                    "statement": statement,
                    "container_tag": doc.tag(),
                },
            }));
        }
        self.store.upsert(&cfg.graph_collection, points).await?;
        if !superseded.is_empty() {
            superseded.sort();
            superseded.dedup();
            if let Err(e) = self
                .store
                .set_payload(
                    &cfg.graph_collection,
                    &superseded,
                    json!({ "is_latest": false }),
                )
                .await
            {
                eprintln!(
                    "[recally] failed to flag {} superseded edges: {e}",
                    superseded.len()
                );
            }
        }
        Ok(())
    }

    /// Scroll the latest edges in a namespace and parse them into StoredEdges.
    async fn latest_edges(&self, cfg: &EngineCfg, tag: &str) -> Vec<graph::StoredEdge> {
        let filter = tagged_filter(
            Some(json!({ "must_not": [ { "key": "is_latest", "match": { "value": false } } ] })),
            tag,
        );
        self.store
            .scroll_all(&cfg.graph_collection, Some(filter), 2000)
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(stored_edge_from_payload)
            .collect()
    }

    /// Answer-time temporal resolution: semantic-search the edge groups relevant
    /// to `query`, build their full dated timelines, and render a context block
    /// with the value valid at `as_of` marked. Empty when the graph is off or
    /// nothing relevant is found. This is what turns "use the latest value" into
    /// a deterministic event-time lookup instead of an LLM guess.
    pub async fn resolve_edges_tagged(
        &self,
        tag: &str,
        query: &str,
        as_of: i64,
        limit: usize,
    ) -> String {
        let cfg = self.cfg();
        if !cfg.temporal_graph {
            return String::new();
        }
        let qv = match self
            .embedder
            .embed(EmbedTask::Query, &[query.to_string()])
            .await
        {
            Ok(v) => v,
            Err(_) => return String::new(),
        };
        let Some(qvec) = qv.first() else {
            return String::new();
        };
        let hits = self
            .store
            .search(
                &cfg.graph_collection,
                qvec,
                limit,
                0.25,
                Some(tagged_filter(None, tag)),
            )
            .await
            .unwrap_or_default();
        if hits.is_empty() {
            return String::new();
        }
        // The relevant attribute groups (subject, predicate).
        let relevant: std::collections::HashSet<(String, String)> = hits
            .iter()
            .filter_map(|h| {
                let p = &h["payload"];
                Some((
                    p["subject"].as_str()?.to_string(),
                    p["predicate"].as_str()?.to_string(),
                ))
            })
            .collect();
        // Pull every edge (latest or historical) in those groups for the full
        // timeline, then resolve as of the query timepoint.
        let edges: Vec<graph::StoredEdge> = self
            .store
            .scroll_all(&cfg.graph_collection, Some(tagged_filter(None, tag)), 4000)
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(stored_edge_from_payload)
            .filter(|se| relevant.contains(&(se.edge.subject.clone(), se.edge.predicate.clone())))
            .collect();
        if edges.is_empty() {
            return String::new();
        }
        graph::render_block(&graph::resolve(&edges, as_of))
    }

    /// For a counting question, return the dated instances of the single
    /// best-matching EVENT group (non-singular edges) in the graph, deduped by
    /// object. The caller applies any date window and counts — so "how many
    /// weddings this year" becomes a structured, date-filtered count in Rust
    /// rather than an LLM tally that ignores the window. Empty when the graph is
    /// off or no event group matches.
    pub async fn count_event_instances_tagged(
        &self,
        tag: &str,
        query: &str,
        limit: usize,
    ) -> Vec<(String, i64)> {
        let cfg = self.cfg();
        if !cfg.temporal_graph {
            return vec![];
        }
        let qv = match self
            .embedder
            .embed(EmbedTask::Query, &[query.to_string()])
            .await
        {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let Some(qvec) = qv.first() else {
            return vec![];
        };
        let hits = self
            .store
            .search(
                &cfg.graph_collection,
                qvec,
                limit,
                0.3,
                Some(tagged_filter(None, tag)),
            )
            .await
            .unwrap_or_default();
        // The single best-matching EVENT group: the top hit whose edge is an
        // accumulating event (singular=false), not a single-valued state.
        let best = hits.iter().find_map(|h| {
            let p = &h["payload"];
            if p["singular"].as_bool() == Some(false) {
                Some((
                    p["subject"].as_str()?.to_string(),
                    p["predicate"].as_str()?.to_string(),
                ))
            } else {
                None
            }
        });
        let Some((subject, predicate)) = best else {
            return vec![];
        };
        // Every distinct instance in that group, with its event date.
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<(String, i64)> = Vec::new();
        for p in self
            .store
            .scroll_all(&cfg.graph_collection, Some(tagged_filter(None, tag)), 4000)
            .await
            .unwrap_or_default()
        {
            let Some(se) = stored_edge_from_payload(&p) else {
                continue;
            };
            if se.edge.subject == subject
                && se.edge.predicate == predicate
                && seen.insert(se.edge.object.to_lowercase())
            {
                out.push((se.edge.object, se.edge.valid_from));
            }
        }
        out
    }

    /// Ask-time retrieval. A fast planning pass first resolves relative dates,
    /// detects source intent ("websites I visited" → browser) and list-style
    /// questions; the plan becomes a Qdrant payload filter. Then the planned
    /// query is embedded once and chunks + facts are searched in parallel.
    /// If a filtered search finds nothing, it retries unfiltered.
    pub async fn retrieve(
        &self,
        q: &str,
        limit: usize,
    ) -> Result<(Vec<SearchResult>, Vec<String>), String> {
        self.retrieve_with_context(q, None, limit).await
    }

    /// Namespaced retrieval — searches only the given container tag's pool.
    /// The entry point a multi-tenant host (one tag per user/agent) calls.
    pub async fn retrieve_tagged(
        &self,
        tag: &str,
        q: &str,
        context: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<SearchResult>, Vec<String>), String> {
        let plan = self.plan_query(q, context).await;
        self.retrieve_for_plan_tagged(tag, q, &plan, context, limit)
            .await
    }

    /// Expose the search plan so callers can route enumeration ("list all
    /// files from last week") to the structured timeline instead of semantic
    /// search, which only ever returns a similarity top-K.
    pub async fn plan_query(&self, q: &str, context: Option<&str>) -> rewrite::SearchPlan {
        let cfg = self.cfg();
        rewrite::plan(self.llm.as_ref(), &cfg.plan_model, q, context).await
    }

    /// Retrieval WITHOUT the LLM planning pass — the query is searched as-is.
    /// Used by the A/B benchmark so it measures the embedding/retrieval change
    /// in isolation (no planner cost, no planner non-determinism) and runs fast.
    pub async fn retrieve_raw(
        &self,
        q: &str,
        limit: usize,
    ) -> Result<(Vec<SearchResult>, Vec<String>), String> {
        let plan = rewrite::SearchPlan {
            query: q.to_string(),
            ..Default::default()
        };
        self.retrieve_for_plan(q, &plan, None, limit).await
    }

    /// Like `retrieve`, with recent conversation turns so follow-up
    /// questions ("what is the doc about?") resolve their references.
    pub async fn retrieve_with_context(
        &self,
        q: &str,
        context: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<SearchResult>, Vec<String>), String> {
        let plan = self.plan_query(q, context).await;
        self.retrieve_for_plan(q, &plan, context, limit).await
    }

    /// Chunk search, branching on hybrid mode (namespaced filter applied by the
    /// caller via `filter`): dense-only cosine (with a score
    /// floor) or dense+sparse fused server-side with RRF. `embed_text` feeds the
    /// sparse (lexical) side; `qv` is its dense embedding.
    async fn search_chunks(
        &self,
        cfg: &EngineCfg,
        qv: &[f32],
        embed_text: &str,
        limit: usize,
        threshold: f32,
        filter: Option<Value>,
    ) -> Result<Vec<Value>, String> {
        if cfg.hybrid_search {
            let sv = sparse::sparse_vector(embed_text);
            self.store
                .search_hybrid(&cfg.chunks_collection, qv, &sv, limit, filter)
                .await
        } else {
            self.store
                .search(&cfg.chunks_collection, qv, limit, threshold, filter)
                .await
        }
    }

    /// Semantic retrieval for an already-computed plan in the default namespace.
    pub async fn retrieve_for_plan(
        &self,
        q: &str,
        plan: &rewrite::SearchPlan,
        context: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<SearchResult>, Vec<String>), String> {
        self.retrieve_for_plan_tagged(DEFAULT_TAG, q, plan, context, limit)
            .await
    }

    /// Semantic retrieval for an already-computed plan, scoped to one namespace.
    /// Every Qdrant search below threads `tag` through its filter, so neither
    /// the chunk search, the facts search, the multi-query union, nor the
    /// wrong-source retry can ever surface another tenant's data.
    pub async fn retrieve_for_plan_tagged(
        &self,
        tag: &str,
        q: &str,
        plan: &rewrite::SearchPlan,
        context: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<SearchResult>, Vec<String>), String> {
        let cfg = self.cfg();

        // The plan's own constraints (source/time); the namespace tag is added
        // on top. We remember whether the plan added anything so the retry below
        // only fires when there were source/time constraints to relax.
        let plan_filter = build_filter(plan);
        let had_plan_constraints = plan_filter.is_some();
        let filter = Some(tagged_filter(plan_filter, tag));
        let doc_limit = if plan.listy { limit.max(20) } else { limit };
        // Recall wide: multi-chunk documents would otherwise crowd distinct
        // files out of the candidate pool before the reranker ever sees them.
        let hit_limit = if plan.listy { 150 } else { 60 };

        // Belt and braces for follow-ups: regardless of how well the planner
        // resolved references, short questions get the previous user question
        // blended into the embedding so retrieval stays anchored to the topic.
        let mut embed_text = plan.query.clone();
        if let Some(ctx) = context {
            if q.chars().count() < 100 {
                // Blend ALL prior user questions, not just the last — by the
                // 3rd turn the immediately-previous one ("what is it about?")
                // is itself generic; the topical keywords live in turn 1.
                let prior: Vec<&str> = ctx
                    .lines()
                    .filter_map(|l| l.strip_prefix("user: "))
                    .collect();
                if !prior.is_empty() {
                    embed_text = format!("{}\n{embed_text}", prior.join("\n"));
                }
            }
        }
        // Multi-query: the planner's rewrite is keyword-rich but can drift from
        // the user's phrasing. When it differs, also search the raw question and
        // union the pools so a doc only the original wording would surface still
        // reaches the reranker. Both query vectors are embedded in ONE batch and
        // both chunk searches run concurrently with the facts search, so the
        // recall boost costs ~no extra latency. The reranker stays the gate.
        let want_mq = cfg.multi_query && plan.query.trim() != q.trim();
        let embed_inputs: Vec<String> = if want_mq {
            vec![embed_text.clone(), q.to_string()]
        } else {
            vec![embed_text.clone()]
        };
        let qvs = self.embedder.embed(EmbedTask::Query, &embed_inputs).await?;
        let qv = qvs.first().cloned().ok_or("no query embedding")?;
        let facts_filter = Some(active_facts_filter(
            filter.clone(),
            chrono::Utc::now().timestamp(),
        ));

        // Facts exclude superseded/expired memories so a contradicted fact
        // ("uses Adidas" after "switched to Puma") never surfaces.
        let (chunk_hits, chunk_hits2, fact_hits) = if want_mq {
            let qv2 = qvs.get(1).cloned().unwrap_or_else(|| qv.clone());
            let (a, b, f) = tokio::join!(
                self.search_chunks(
                    &cfg,
                    &qv,
                    &embed_text,
                    hit_limit,
                    CHUNK_THRESHOLD,
                    filter.clone()
                ),
                self.search_chunks(&cfg, &qv2, q, hit_limit, CHUNK_THRESHOLD, filter.clone()),
                self.store
                    .search(&cfg.facts_collection, &qv, 10, FACT_THRESHOLD, facts_filter),
            );
            (a, Some(b), f)
        } else {
            let (a, f) = tokio::join!(
                self.search_chunks(
                    &cfg,
                    &qv,
                    &embed_text,
                    hit_limit,
                    CHUNK_THRESHOLD,
                    filter.clone()
                ),
                self.store
                    .search(&cfg.facts_collection, &qv, 10, FACT_THRESHOLD, facts_filter),
            );
            (a, None, f)
        };
        let mut chunk_hits = chunk_hits?;

        // Union the second query's hits (dedup by point id).
        if let Some(Ok(extra)) = chunk_hits2 {
            let seen: std::collections::HashSet<String> = chunk_hits
                .iter()
                .filter_map(|h| h["id"].as_str().map(String::from))
                .collect();
            for h in extra {
                if !h["id"]
                    .as_str()
                    .map(|id| seen.contains(id))
                    .unwrap_or(false)
                {
                    chunk_hits.push(h);
                }
            }
        }

        // A wrong source/time guess shouldn't blank the answer — but the retry
        // must clear a much higher bar (else loosely related junk masquerades as
        // an answer) and STILL stay inside the namespace: drop source/time, keep
        // the tag, never search the whole cross-tenant pool.
        if chunk_hits.is_empty() && had_plan_constraints {
            chunk_hits = self
                .search_chunks(
                    &cfg,
                    &qv,
                    &embed_text,
                    hit_limit,
                    FALLBACK_THRESHOLD,
                    Some(tagged_filter(None, tag)),
                )
                .await?;
        }

        // Group wide, then rerank: dense similarity recalls candidates, the
        // cross-encoder decides what actually answers the question. Junk that
        // merely shares vocabulary drops below the relevance bar instead of
        // outranking a file literally named after the query.
        let mut results = group_chunk_hits(&chunk_hits, doc_limit.max(30));
        if results.len() > 1 {
            let docs: Vec<String> = results
                .iter()
                .map(|r| {
                    let title = r.title.clone().unwrap_or_default();
                    let body: String = r
                        .chunks
                        .iter()
                        .map(|c| c.content.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("{title}\n{}", body.chars().take(1500).collect::<String>())
                })
                .collect();
            match self.reranker.rerank(q, &docs).await {
                Ok(scored) => {
                    // Hybrid: cross-encoder relevance + a lexical bonus when
                    // query words appear in the document's title. A user asking
                    // for "the RAAS tenant setup guide" should beat a browser
                    // capture of the same repo that's only topically similar.
                    let q_tokens = tokenize(q);
                    let mut ranked: Vec<(usize, f64)> = scored
                        .into_iter()
                        .filter(|(_, s)| *s >= RERANK_THRESHOLD)
                        .map(|(i, s)| {
                            let title_match = results
                                .get(i)
                                .and_then(|r| r.title.as_ref())
                                .map(|t| title_overlap(&q_tokens, t))
                                .unwrap_or(0.0);
                            (i, s + title_match * TITLE_BOOST)
                        })
                        .collect();
                    ranked
                        .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    let keep: Vec<SearchResult> = ranked
                        .into_iter()
                        .filter_map(|(i, _)| results.get(i).cloned())
                        .collect();
                    if !keep.is_empty() {
                        results = keep;
                    }
                }
                Err(e) => eprintln!("[recally] rerank failed (keeping dense order): {e}"),
            }
        }
        results.truncate(doc_limit);

        let facts = fact_hits
            .unwrap_or_default()
            .iter()
            .filter_map(|h| h["payload"]["fact"].as_str().map(String::from))
            .collect();
        Ok((results, facts))
    }

    /// The default namespace's standing profile. See `profile_tagged`.
    pub async fn profile(&self) -> profile::Profile {
        self.profile_tagged(DEFAULT_TAG).await
    }

    /// The standing user profile (static + dynamic) for one namespace, cached
    /// for an hour so it costs nothing at query time. Prepend `as_prompt_block()`
    /// to any assistant's system prompt so it starts already knowing the user.
    pub async fn profile_tagged(&self, tag: &str) -> profile::Profile {
        const PROFILE_TTL: i64 = 3600;
        let now = chrono::Utc::now().timestamp();
        if let Some((p, t)) = self.profile_cache.read().unwrap().get(tag) {
            if now - t < PROFILE_TTL {
                return p.clone();
            }
        }
        let cfg = self.cfg();
        let p = profile::compile(self.store.as_ref(), self.llm.as_ref(), &cfg, tag)
            .await
            .unwrap_or_default();
        self.profile_cache
            .write()
            .unwrap()
            .insert(tag.to_string(), (p.clone(), now));
        p
    }

    /// Force a profile recompile for a namespace (e.g. after a large ingest),
    /// bypassing the TTL.
    pub async fn refresh_profile(&self, tag: &str) -> profile::Profile {
        self.profile_cache.write().unwrap().remove(tag);
        self.profile_tagged(tag).await
    }

    /// The memory graph for the map view: distilled facts connected to the
    /// documents they were learned from. Document metadata comes from each
    /// fact's payload, so one scroll over the facts collection is enough.
    pub async fn graph(&self, limit: usize) -> Result<Value, String> {
        let cfg = self.cfg();
        let points = self.store.scroll(&cfg.facts_collection, limit).await?;

        let mut nodes: Vec<Value> = Vec::new();
        let mut edges: Vec<Value> = Vec::new();
        let mut seen_docs: std::collections::HashSet<String> = Default::default();

        for p in &points {
            let pl = &p["payload"];
            let (Some(fact), Some(doc_id)) = (pl["fact"].as_str(), pl["doc_id"].as_str()) else {
                continue;
            };
            let fact_id = p["id"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| p["id"].to_string());
            nodes.push(json!({
                "id": fact_id,
                "label": fact,
                "kind": "fact",
                "docId": doc_id,
                "capturedAt": pl["captured_at"],
            }));
            if seen_docs.insert(doc_id.to_string()) {
                nodes.push(json!({
                    "id": doc_id,
                    "label": pl["title"].as_str().unwrap_or("Memory"),
                    "kind": pl["source"].as_str().unwrap_or("file"),
                    "docId": doc_id,
                    "capturedAt": pl["captured_at"],
                }));
            }
            edges.push(json!({ "from": fact_id, "to": doc_id }));
        }
        Ok(json!({ "nodes": nodes, "edges": edges }))
    }

    /// Backfill `container_tag` onto existing data that predates namespaces,
    /// claiming it into `tag` (e.g. a signed-in user's id). Only touches points
    /// with NO container_tag yet — never reassigns another namespace's data.
    /// Payload-only: reuses the stored text and embeddings, no re-extract/embed.
    pub async fn claim_legacy_into_tag(&self, tag: &str) -> Result<(), String> {
        let cfg = self.cfg();
        let filter = json!({ "must": [ { "is_empty": { "key": "container_tag" } } ] });
        for c in [&cfg.chunks_collection, &cfg.facts_collection] {
            self.store
                .set_payload_by_filter(c, filter.clone(), json!({ "container_tag": tag }))
                .await?;
        }
        Ok(())
    }

    /// Backfill `is_latest = true` on facts that predate the memory lifecycle, so
    /// the temporal filter treats them explicitly as current. Payload-only.
    pub async fn backfill_facts_latest(&self) -> Result<(), String> {
        let cfg = self.cfg();
        let filter = json!({ "must": [ { "is_empty": { "key": "is_latest" } } ] });
        self.store
            .set_payload_by_filter(&cfg.facts_collection, filter, json!({ "is_latest": true }))
            .await
    }

    /// Reconstruct a document's text from its INDEXED chunks (in order), without
    /// touching the original file. The extracted text already lives in the chunk
    /// payloads, so this is how re-indexing avoids re-running OCR/extraction.
    pub async fn reconstruct_doc_text(&self, doc_id: &str) -> Result<String, String> {
        let cfg = self.cfg();
        let mut chunks = self
            .store
            .doc_chunks_indexed(&cfg.chunks_collection, doc_id, 500)
            .await?;
        chunks.sort_by_key(|(i, _)| *i);
        Ok(chunks
            .iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"))
    }

    /// One row per distinct document in a namespace: `(doc_id, title, source,
    /// reference, captured_at)`. Built by scrolling the chunks collection and
    /// deduping by `doc_id`. This is UltraMem's document registry — it has no
    /// external store, so the index IS the source of truth. Powers `/timeline`
    /// and the reindex enumeration. `before`/`limit` page a newest-first view.
    pub async fn list_document_ids(
        &self,
        tag: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<(String, String, String, String, i64)>, String> {
        let cfg = self.cfg();
        let filter = tagged_filter(None, tag);
        // Cap the scroll generously; dedup collapses it to distinct documents.
        let points = self
            .store
            .scroll_all(&cfg.chunks_collection, Some(filter), 50_000)
            .await?;
        let mut seen: std::collections::HashSet<String> = Default::default();
        let mut rows: Vec<(String, String, String, String, i64)> = Vec::new();
        for p in &points {
            let pl = &p["payload"];
            let Some(doc_id) = pl["doc_id"].as_str() else {
                continue;
            };
            if !seen.insert(doc_id.to_string()) {
                continue;
            }
            let captured_at = pl["captured_at"].as_i64().unwrap_or(0);
            if before.map(|b| captured_at >= b).unwrap_or(false) {
                continue;
            }
            rows.push((
                doc_id.to_string(),
                pl["title"].as_str().unwrap_or_default().to_string(),
                pl["source"].as_str().unwrap_or_default().to_string(),
                pl["reference"].as_str().unwrap_or_default().to_string(),
                captured_at,
            ));
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.4)); // newest first
        rows.truncate(limit);
        Ok(rows)
    }

    /// Rebuild one document's memory facts from its stored chunk text (no file
    /// access): reconstruct → re-distill → memory lifecycle, into namespace
    /// `tag`. Returns the number of facts produced (0 if too little text).
    pub async fn reindex_doc_facts(
        &self,
        doc_id: &str,
        title: &str,
        source: &str,
        reference: &str,
        captured_at: i64,
        tag: &str,
    ) -> Result<usize, String> {
        let content = self.reconstruct_doc_text(doc_id).await?;
        if content.trim().chars().count() < 200 {
            return Ok(0);
        }
        let doc = IngestDoc {
            source: source.into(),
            title: title.into(),
            content,
            reference: reference.into(),
            app: String::new(),
            captured_at,
            file_path: None, // ← reuse stored text; never re-extract
            container_tag: tag.into(),
        };
        self.redistill_doc(&doc, doc_id).await
    }

    /// Reconstruct one document's text from its INDEXED chunks (no file access)
    /// and re-run fact distillation + the memory lifecycle for it. Reuses the
    /// stored extraction — only facts are re-embedded. Old facts for the doc are
    /// replaced. Returns the number of facts produced.
    pub async fn redistill_doc(&self, doc: &IngestDoc, doc_id: &str) -> Result<usize, String> {
        let cfg = self.cfg();
        if !cfg.distill_model.is_ready() {
            return Err("no distill model configured".into());
        }
        // Fresh facts replace stale ones for this document.
        self.store
            .delete_by_doc(&cfg.facts_collection, doc_id)
            .await?;
        let facts = distill::distill_facts(
            self.llm.as_ref(),
            &cfg.distill_model,
            &doc.title,
            &doc.content,
            &date_str(doc.captured_at),
        )
        .await?;
        let n = facts.len();
        if !facts.is_empty() {
            self.index_memories(&cfg, doc, doc_id, facts).await?;
        }
        Ok(n)
    }

    /// Remove a document's chunks and facts from both collections.
    ///
    /// Unscoped — deletes by `doc_id` across every namespace. Prefer
    /// [`Self::delete_document_tagged`] on any multi-tenant surface; this remains
    /// for embedded single-tenant use and internal tests.
    pub async fn delete_document(&self, doc_id: &str) -> Result<(), String> {
        let cfg = self.cfg();
        let (a, b) = tokio::join!(
            self.store.delete_by_doc(&cfg.chunks_collection, doc_id),
            self.store.delete_by_doc(&cfg.facts_collection, doc_id),
        );
        a?;
        b
    }

    /// Delete a document only if it lives in `tag`'s namespace (SS-2). Returns
    /// `Ok(false)` when no point with that `doc_id` exists in the tag (the caller
    /// maps that to `404`) so a document in another tenant is never touched and
    /// its existence is not disclosed. `Ok(true)` when something was deleted.
    pub async fn delete_document_tagged(&self, doc_id: &str, tag: &str) -> Result<bool, String> {
        let cfg = self.cfg();
        let filter = doc_delete_filter(doc_id, tag);
        // Existence within the caller's namespace (a doc always has chunks; check
        // facts too so a chunkless-but-facted doc still resolves).
        let (c, f) = tokio::join!(
            self.store
                .scroll_all(&cfg.chunks_collection, Some(filter.clone()), 1),
            self.store
                .scroll_all(&cfg.facts_collection, Some(filter.clone()), 1),
        );
        let exists = !c.unwrap_or_default().is_empty() || !f.unwrap_or_default().is_empty();
        if !exists {
            return Ok(false);
        }
        let (a, b) = tokio::join!(
            self.store
                .delete_by_filter(&cfg.chunks_collection, filter.clone()),
            self.store.delete_by_filter(&cfg.facts_collection, filter),
        );
        a?;
        b?;
        Ok(true)
    }
}

/// How much a full title-word match can lift a reranked score. Modest — it
/// breaks ties toward exact-name matches without overriding strong semantic
/// relevance (rerank scores run 0..1).
const TITLE_BOOST: f64 = 0.4;

/// Lowercase alphanumeric tokens of length ≥3, separators (`_-.`) split.
fn tokenize(s: &str) -> std::collections::HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(String::from)
        .collect()
}

/// Fraction of the query's distinctive tokens that appear in the title.
fn title_overlap(q_tokens: &std::collections::HashSet<String>, title: &str) -> f64 {
    if q_tokens.is_empty() {
        return 0.0;
    }
    let t_tokens = tokenize(title);
    let hits = q_tokens.iter().filter(|t| t_tokens.contains(*t)).count();
    hits as f64 / q_tokens.len() as f64
}

/// Add a container-tag (namespace) constraint to a filter, building one if
/// absent. Explicit tags hard-isolate — only points carrying that exact tag
/// match. The default tag ALSO matches legacy points with no `container_tag`
/// field (via `is_empty`), so pre-namespace data stays searchable with no
/// migration. Always call this on a search filter so a wrong-source retry or
/// any other path can never leak across namespaces.
fn tagged_filter(base: Option<Value>, tag: &str) -> Value {
    let mut f = base.unwrap_or_else(|| json!({}));
    let obj = f.as_object_mut().expect("filter is an object");
    if tag == DEFAULT_TAG {
        // default OR legacy(no field) — `should` means "at least one matches".
        let should = obj.entry("should").or_insert_with(|| json!([]));
        if let Some(a) = should.as_array_mut() {
            a.push(json!({ "key": "container_tag", "match": { "value": DEFAULT_TAG } }));
            a.push(json!({ "is_empty": { "key": "container_tag" } }));
        }
    } else {
        let must = obj.entry("must").or_insert_with(|| json!([]));
        if let Some(a) = must.as_array_mut() {
            a.push(json!({ "key": "container_tag", "match": { "value": tag } }));
        }
    }
    f
}

/// Filter that matches exactly one document's points *within a namespace*: the
/// `doc_id` constraint plus the tag scope from [`tagged_filter`]. Used by
/// [`MemoryEngine::delete_document_tagged`] so a delete can never reach across
/// tenants. For the default tag this also matches legacy (untagged) points, the
/// same rule reads use.
fn doc_delete_filter(doc_id: &str, tag: &str) -> Value {
    let base = json!({ "must": [ { "key": "doc_id", "match": { "value": doc_id } } ] });
    tagged_filter(Some(base), tag)
}

/// Wrap a base filter (or none) so it also excludes memories that are no longer
/// current: superseded (`is_latest = false`) or expired (`valid_until < now`).
/// Legacy facts lack both fields, so neither exclusion matches them — they stay
/// searchable (treated as latest, never-expiring). `now` is unix seconds.
fn active_facts_filter(base: Option<Value>, now: i64) -> Value {
    let mut f = base.unwrap_or_else(|| json!({}));
    if let Some(obj) = f.as_object_mut() {
        let must_not = obj.entry("must_not").or_insert_with(|| json!([]));
        if let Some(a) = must_not.as_array_mut() {
            a.push(json!({ "key": "is_latest", "match": { "value": false } }));
            a.push(json!({ "key": "valid_until", "range": { "lt": now } }));
        }
    }
    f
}

/// Qdrant filter from a search plan: source match + captured_at range.
fn build_filter(plan: &rewrite::SearchPlan) -> Option<Value> {
    let mut must: Vec<Value> = Vec::new();
    if let Some(src) = &plan.source {
        must.push(json!({ "key": "source", "match": { "value": src } }));
    }
    if plan.after.is_some() || plan.before.is_some() {
        let mut range = serde_json::Map::new();
        if let Some(a) = plan.after {
            range.insert("gte".into(), json!(a));
        }
        if let Some(b) = plan.before {
            range.insert("lte".into(), json!(b));
        }
        must.push(json!({ "key": "captured_at", "range": range }));
    }
    if must.is_empty() {
        None
    } else {
        Some(json!({ "must": must }))
    }
}

/// Format a unix timestamp as `YYYY-MM-DD` (UTC). Used to anchor relative-date
/// resolution during distillation (T2.1 event-time extraction).
fn date_str(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// A readable one-line statement for a knowledge-graph edge — embedded for
/// answer-time semantic lookup and shown in the resolved block.
fn edge_statement(e: &graph::Edge) -> String {
    format!(
        "{} {}: {} ({})",
        e.subject,
        e.predicate.replace('_', " "),
        e.object,
        date_str(e.valid_from)
    )
}

/// Parse a Qdrant edge point back into a `StoredEdge` for resolution.
fn stored_edge_from_payload(p: &Value) -> Option<graph::StoredEdge> {
    let pl = &p["payload"];
    let id = p["id"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| p["id"].to_string());
    Some(graph::StoredEdge {
        id,
        edge: graph::Edge {
            subject: pl["subject"].as_str()?.to_string(),
            predicate: pl["predicate"].as_str()?.to_string(),
            object: pl["object"].as_str()?.to_string(),
            valid_from: pl["valid_from"].as_i64().unwrap_or(0),
            valid_to: pl["valid_to"].as_i64(),
            singular: pl["singular"].as_bool().unwrap_or(true),
        },
        captured_at: pl["captured_at"].as_i64().unwrap_or(0),
        is_latest: pl["is_latest"].as_bool().unwrap_or(true),
    })
}

/// Embedding input for a chunk: readable title + optional doc-context blurb +
/// body. Separator-heavy names like "newton-profile_v2.pdf" become "newton
/// profile v2.pdf" so their words actually match queries; the blurb (when
/// Contextual Retrieval is on) situates the chunk in its document.
fn embed_input(title: &str, context: Option<&str>, chunk: &str) -> String {
    let readable: String = title
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect();
    let mut prefix = readable.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(ctx) = context.map(str::trim).filter(|c| !c.is_empty()) {
        prefix = if prefix.is_empty() {
            ctx.to_string()
        } else {
            format!("{prefix}\n{ctx}")
        };
    }
    if prefix.is_empty() {
        chunk.to_string()
    } else {
        format!("{prefix}\n{chunk}")
    }
}

/// Group Qdrant chunk hits (already score-descending) into per-document
/// results, preserving best-hit order, keeping at most `limit` documents.
/// Documents sharing a reference (the same URL captured more than once)
/// collapse into the best-scoring one.
fn group_chunk_hits(hits: &[Value], limit: usize) -> Vec<SearchResult> {
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, SearchResult> = HashMap::new();
    let mut seen_refs: std::collections::HashSet<String> = Default::default();
    for hit in hits {
        let payload = &hit["payload"];
        let doc_id = payload["doc_id"].as_str().unwrap_or_default().to_string();
        if doc_id.is_empty() {
            continue;
        }
        if let Some(r) = payload["reference"].as_str().filter(|r| !r.is_empty()) {
            if !map.contains_key(&doc_id) && !seen_refs.insert(r.to_string()) {
                continue; // same URL already represented by a better hit
            }
        }
        let entry = map.entry(doc_id.clone()).or_insert_with(|| {
            order.push(doc_id.clone());
            SearchResult {
                document_id: doc_id.clone(),
                title: payload["title"].as_str().map(String::from),
                metadata: Some(json!({
                    "source": payload["source"],
                    "reference": payload["reference"],
                    "app": payload["app"],
                    "capturedAt": payload["captured_at"],
                })),
                chunks: vec![],
            }
        });
        entry.chunks.push(SearchChunk {
            content: payload["content"].as_str().unwrap_or_default().to_string(),
            score: hit["score"].as_f64().unwrap_or(0.0),
        });
    }
    order
        .into_iter()
        .take(limit)
        .filter_map(|id| map.remove(&id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gate (Phase 3): the embedder is chosen from config — flipping
    /// `cfg.embedder` swaps the provider (and its dimensionality) with no engine
    /// edits — and a fully custom `dyn Embedder` can be injected on top.
    #[test]
    #[allow(clippy::field_reassign_with_default)] // incremental cfg mutation reads clearer here
    fn embedder_is_config_selectable() {
        let mut cfg = EngineCfg::default();
        cfg.embedder = "jina".into();
        let jina = MemoryEngine::new(cfg.clone());
        assert_eq!(jina.embedder_dim(), 1024);
        assert_eq!(jina.embedder_id(), "jina-embeddings-v3");

        cfg.embedder = "openai".into(); // text-embedding-3-small, 1536
        let openai = MemoryEngine::new(cfg.clone());
        assert_eq!(openai.embedder_dim(), 1536);
        assert_eq!(openai.embedder_id(), "text-embedding-3-small");

        // Custom dim via config (e.g. text-embedding-3-large or a shortened dim).
        cfg.openai_embed_model = "text-embedding-3-large".into();
        cfg.openai_embed_dim = 3072;
        assert_eq!(MemoryEngine::new(cfg).embedder_dim(), 3072);

        // Escape hatch: inject any embedder without touching engine code.
        struct Tiny;
        #[async_trait::async_trait]
        impl Embedder for Tiny {
            async fn embed(&self, _t: EmbedTask, ins: &[String]) -> Result<Vec<Vec<f32>>, String> {
                Ok(ins.iter().map(|_| vec![0.0; 8]).collect())
            }
            fn dim(&self) -> usize {
                8
            }
            fn id(&self) -> &str {
                "tiny-test"
            }
        }
        let custom = MemoryEngine::new(EngineCfg::default()).with_embedder(Arc::new(Tiny));
        assert_eq!(custom.embedder_dim(), 8);
        assert_eq!(custom.embedder_id(), "tiny-test");
    }

    fn hit(doc_id: &str, content: &str, score: f64) -> Value {
        json!({
            "id": "x", "score": score,
            "payload": {
                "doc_id": doc_id, "content": content, "title": format!("T-{doc_id}"),
                "source": "file", "reference": format!("/tmp/{doc_id}"), "app": "", "captured_at": 1700000000,
            }
        })
    }

    #[test]
    fn groups_hits_by_document_preserving_order() {
        let hits = vec![
            hit("a", "best chunk", 0.9),
            hit("b", "second doc", 0.8),
            hit("a", "another a chunk", 0.7),
            hit("c", "third doc", 0.6),
        ];
        let results = group_chunk_hits(&hits, 8);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].document_id, "a");
        assert_eq!(results[0].chunks.len(), 2);
        assert_eq!(results[1].document_id, "b");
        assert_eq!(results[2].document_id, "c");
    }

    #[test]
    fn respects_doc_limit() {
        let hits = vec![hit("a", "x", 0.9), hit("b", "y", 0.8), hit("c", "z", 0.7)];
        let results = group_chunk_hits(&hits, 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn metadata_carries_citation_fields() {
        let results = group_chunk_hits(&[hit("a", "x", 0.9)], 8);
        let meta = results[0].metadata.as_ref().unwrap();
        assert_eq!(meta["source"], "file");
        assert_eq!(meta["reference"], "/tmp/a");
        assert_eq!(meta["capturedAt"], 1700000000);
    }

    #[test]
    fn duplicate_references_collapse_to_best_hit() {
        let mk = |doc: &str, score: f64| {
            json!({"id":"x","score":score,"payload":{
                "doc_id":doc,"content":"c","title":"PRs","source":"browser",
                "reference":"https://github.com/a/b/pulls","app":"","captured_at":1}})
        };
        let results = group_chunk_hits(&[mk("d1", 0.9), mk("d2", 0.8), mk("d3", 0.7)], 8);
        assert_eq!(results.len(), 1, "same URL should appear once");
        assert_eq!(results[0].document_id, "d1");
    }

    #[test]
    fn skips_hits_without_doc_id() {
        let bad = json!({"id": "x", "score": 0.5, "payload": {"content": "orphan"}});
        assert!(group_chunk_hits(&[bad], 8).is_empty());
    }

    #[test]
    fn embed_input_makes_filenames_readable() {
        assert_eq!(
            embed_input("newton-profile_v2.pdf", None, "body text"),
            "newton profile v2.pdf\nbody text"
        );
        assert_eq!(embed_input("", None, "body text"), "body text");
        assert_eq!(embed_input("  --  ", None, "x"), "x");
    }

    #[test]
    fn tagged_filter_default_includes_legacy() {
        // Default tag matches the tag OR points with no field (legacy).
        let f = tagged_filter(None, DEFAULT_TAG);
        let should = f["should"].as_array().unwrap();
        assert_eq!(should.len(), 2);
        assert_eq!(should[0]["match"]["value"], DEFAULT_TAG);
        assert!(should[1]["is_empty"]["key"] == "container_tag");
        // No `must` constraint for the default namespace.
        assert!(f.get("must").is_none());
    }

    #[test]
    fn tagged_filter_explicit_isolates() {
        // An explicit tenant tag becomes a hard `must` match — only that tag.
        let f = tagged_filter(None, "tenant_42");
        let must = f["must"].as_array().unwrap();
        assert_eq!(must.len(), 1);
        assert_eq!(must[0]["key"], "container_tag");
        assert_eq!(must[0]["match"]["value"], "tenant_42");
        assert!(f.get("should").is_none());
    }

    #[test]
    fn tagged_filter_preserves_base_constraints() {
        // Source/time constraints from the plan survive tagging.
        let base = json!({ "must": [ { "key": "source", "match": { "value": "file" } } ] });
        let f = tagged_filter(Some(base), "tenant_42");
        let must = f["must"].as_array().unwrap();
        assert_eq!(must.len(), 2); // original source + tag
    }

    #[test]
    fn doc_delete_filter_is_tag_scoped() {
        // SS-2: a scoped delete must constrain BOTH the doc_id and the namespace,
        // so it can never remove another tenant's document.
        let f = doc_delete_filter("doc-abc", "tenant_42");
        let must = f["must"].as_array().unwrap();
        assert!(must
            .iter()
            .any(|c| c["key"] == "doc_id" && c["match"]["value"] == "doc-abc"));
        assert!(must
            .iter()
            .any(|c| c["key"] == "container_tag" && c["match"]["value"] == "tenant_42"));
    }

    #[test]
    fn doc_delete_filter_default_tag_keeps_doc_and_legacy_scope() {
        // Default namespace: doc_id is a hard `must`; the tag/legacy match rides in
        // `should` (default OR untagged), exactly as reads scope it.
        let f = doc_delete_filter("doc-xyz", DEFAULT_TAG);
        assert!(f["must"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["key"] == "doc_id" && c["match"]["value"] == "doc-xyz"));
        assert!(f["should"].as_array().unwrap().len() == 2);
    }

    #[test]
    fn embed_input_prepends_context_blurb() {
        assert_eq!(
            embed_input("newton-profile.pdf", Some("Newton's Q3 review."), "body"),
            "newton profile.pdf\nNewton's Q3 review.\nbody"
        );
        // Blank/whitespace context is ignored.
        assert_eq!(embed_input("t", Some("   "), "body"), "t\nbody");
        // Context with no title still applies.
        assert_eq!(embed_input("", Some("ctx"), "body"), "ctx\nbody");
    }
}

/// Live pipeline test against real services. Run explicitly with:
///   ULTRAMEM_PIPELINE_TESTS=1 cargo test --lib engine::pipeline_tests -- --nocapture
/// Requires QDRANT_URL/QDRANT_API_KEY/JINA_API_KEY (GROQ_API_KEY optional —
/// distillation is non-fatal) in the environment or repo .env.
#[cfg(test)]
mod pipeline_tests {
    use super::*;

    #[test]
    fn ingest_search_delete_roundtrip() {
        if std::env::var("ULTRAMEM_PIPELINE_TESTS").as_deref() != Ok("1") {
            eprintln!("skipped (set ULTRAMEM_PIPELINE_TESTS=1 to run)");
            return;
        }
        let _ = dotenvy::dotenv();
        let _ = dotenvy::from_filename("../.env");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut cfg = EngineCfg::from_env();
            cfg.chunks_collection = "ultramem_test_chunks".into();
            cfg.facts_collection = "ultramem_test_facts".into();
            assert!(!cfg.qdrant_url.is_empty(), "QDRANT_URL missing");
            let engine = MemoryEngine::new(cfg.clone());

            assert!(
                engine.health().await,
                "engine unhealthy — check QDRANT_URL/keys"
            );
            engine
                .ensure_collections()
                .await
                .expect("ensure_collections");

            let marker = uuid::Uuid::new_v4().to_string();
            let doc = IngestDoc {
                source: "file".into(),
                title: "Pipeline test doc".into(),
                content: format!(
                    "Recally pipeline verification document. The magic marker is {marker}. \
                     The user is testing the new memory engine built on Qdrant and Jina."
                ),
                reference: "/tmp/pipeline-test.txt".into(),
                app: String::new(),
                captured_at: 1750000000,
                file_path: None,
                container_tag: String::new(),
            };
            let doc_id = engine.add_document(&doc).await.expect("add_document");

            let (results, _facts) = engine
                .retrieve(&format!("what is the magic marker? {marker}"), 8)
                .await
                .expect("retrieve");
            assert!(
                results.iter().any(|r| r.document_id == doc_id),
                "ingested doc not found in search results"
            );
            let found = results.iter().find(|r| r.document_id == doc_id).unwrap();
            assert!(found.chunks.iter().any(|c| c.content.contains(&marker)));

            engine
                .delete_document(&doc_id)
                .await
                .expect("delete_document");
            let (after, _) = engine
                .retrieve(&format!("what is the magic marker? {marker}"), 8)
                .await
                .expect("retrieve after delete");
            assert!(
                !after.iter().any(|r| r.document_id == doc_id),
                "doc still searchable after delete"
            );

            // Clean up test collections.
            let http = reqwest::Client::new();
            let _ = qdrant::delete_collection(
                &http,
                &cfg.qdrant_url,
                &cfg.qdrant_api_key,
                &cfg.chunks_collection,
            )
            .await;
            let _ = qdrant::delete_collection(
                &http,
                &cfg.qdrant_url,
                &cfg.qdrant_api_key,
                &cfg.facts_collection,
            )
            .await;
        });
    }

    /// The "memory, not RAG" test: a contradicting fact must SUPERSEDE the old
    /// one. After "switched to Puma", a search for the user's shoe brand must
    /// not return the superseded "uses Adidas" memory. Requires GROQ for the
    /// distill + reconcile passes.
    #[test]
    fn contradiction_supersedes_old_memory() {
        if std::env::var("ULTRAMEM_PIPELINE_TESTS").as_deref() != Ok("1") {
            eprintln!("skipped (set ULTRAMEM_PIPELINE_TESTS=1 to run)");
            return;
        }
        let _ = dotenvy::dotenv();
        let _ = dotenvy::from_filename("../.env");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut cfg = EngineCfg::from_env();
            let tag = uuid::Uuid::new_v4().to_string();
            cfg.chunks_collection = format!("ultramem_test_chunks_{tag}");
            cfg.facts_collection = format!("ultramem_test_facts_{tag}");
            if !cfg.distill_model.is_ready() {
                eprintln!("skipped (GROQ_API_KEY needed for distill/reconcile)");
                return;
            }
            let engine = MemoryEngine::new(cfg.clone());
            engine.ensure_collections().await.expect("collections");
            let http = reqwest::Client::new();

            let mk = |body: &str| IngestDoc {
                source: "file".into(),
                title: "Shoe preferences".into(),
                content: body.into(),
                reference: format!("/tmp/shoes-{}", uuid::Uuid::new_v4()),
                app: String::new(),
                captured_at: 1_750_000_000,
                file_path: None,
                container_tag: String::new(),
            };

            // Establish the original memory, then contradict it. Bodies are
            // padded past the 280-char distillation floor so facts are extracted.
            engine.add_document(&mk(
                "Personal note about the user's footwear and training habits. For the last several \
                 years the user has worn Adidas running shoes for every single training session and \
                 genuinely likes the Adidas brand above all others. The user buys Adidas shoes \
                 exclusively, owns multiple pairs of Adidas Ultraboost, and regularly recommends \
                 Adidas to friends and training partners at the running club. Adidas is the user's \
                 default and preferred running shoe brand.",
            )).await.expect("add 1");
            engine.add_document(&mk(
                "Important footwear update from this month. The user has now completely switched \
                 away from Adidas and going forward only wears Puma running shoes for training. \
                 After years on Adidas the user prefers Puma now, has replaced their entire shoe \
                 rotation with Puma Deviate models, and no longer buys Adidas at all. Puma is now \
                 the user's current and preferred running shoe brand, replacing the old Adidas \
                 preference entirely.",
            )).await.expect("add 2");

            // Count latest vs superseded memories directly.
            let all = qdrant::scroll(&http, &cfg.qdrant_url, &cfg.qdrant_api_key, &cfg.facts_collection, 200).await.expect("scroll");
            let superseded = all.iter().filter(|p| p["payload"]["is_latest"].as_bool() == Some(false)).count();
            eprintln!("memories: {} total, {} superseded", all.len(), superseded);
            assert!(superseded >= 1, "expected at least one superseded (Adidas) memory after the switch");

            // The latest-only fact search must surface Puma, not the superseded Adidas.
            let (_r, facts) = engine.retrieve("what running shoe brand does the user wear", 8).await.expect("retrieve");
            let joined = facts.join(" | ").to_lowercase();
            eprintln!("latest facts for shoe query: {facts:#?}");
            assert!(joined.contains("puma"), "current brand (Puma) missing from latest facts");
            // Absence, not just presence: the superseded brand must not be served.
            assert!(!joined.contains("adidas"), "superseded brand (Adidas) still served in latest facts");

            let _ = qdrant::delete_collection(&http, &cfg.qdrant_url, &cfg.qdrant_api_key, &cfg.chunks_collection).await;
            let _ = qdrant::delete_collection(&http, &cfg.qdrant_url, &cfg.qdrant_api_key, &cfg.facts_collection).await;
        });
    }

    /// Multi-tenant isolation: two namespaces ingest conflicting facts; each
    /// must see ONLY its own. The proof that one Clerk user can never read
    /// another's memory.
    #[test]
    fn container_tags_isolate_namespaces() {
        if std::env::var("ULTRAMEM_PIPELINE_TESTS").as_deref() != Ok("1") {
            eprintln!("skipped (set ULTRAMEM_PIPELINE_TESTS=1 to run)");
            return;
        }
        let _ = dotenvy::dotenv();
        let _ = dotenvy::from_filename("../.env");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut cfg = EngineCfg::from_env();
            let suffix = uuid::Uuid::new_v4().to_string();
            cfg.chunks_collection = format!("ultramem_iso_chunks_{suffix}");
            cfg.facts_collection = format!("ultramem_iso_facts_{suffix}");
            let engine = MemoryEngine::new(cfg.clone());
            engine.ensure_collections().await.expect("collections");
            let http = reqwest::Client::new();

            let mk = |tag: &str, body: &str| IngestDoc {
                source: "file".into(),
                title: "Workspace note".into(),
                content: body.into(),
                reference: format!("/iso/{}", uuid::Uuid::new_v4()),
                app: String::new(),
                captured_at: 1_750_000_000,
                file_path: None,
                container_tag: tag.into(),
            };

            // Alice's pool and Bob's pool hold deliberately conflicting facts.
            // Bodies run past the 280-char distillation floor so facts extract.
            engine.add_document(&mk("tenant_alice",
                "Internal company communication policy note for the record. Alice's company \
                 exclusively uses Slack for all team communication and has fully standardized on \
                 Slack across every single department without exception. Slack is the official and \
                 only sanctioned chat tool at Alice's company, every employee has a Slack account, \
                 and all project channels, standups, and incident response happen in Slack. The \
                 company has no plans to ever move away from Slack.",
            )).await.expect("alice");
            engine.add_document(&mk("tenant_bob",
                "Internal company communication policy note for the record. Bob's company \
                 exclusively uses Microsoft Teams for all team communication and has fully \
                 standardized on Microsoft Teams across every single department without exception. \
                 Teams is the official and only sanctioned chat tool at Bob's company, every \
                 employee has a Teams account, and all project channels, standups, and incident \
                 response happen in Microsoft Teams. The company has no plans to ever move away from Teams.",
            )).await.expect("bob");

            // Alice's namespace must surface Slack and NEVER Bob's Teams.
            let (_r, a_facts) = engine.retrieve_tagged("tenant_alice", "what chat tool does the company use", None, 8).await.expect("alice retrieve");
            let a = a_facts.join(" | ").to_lowercase();
            eprintln!("alice sees: {a_facts:#?}");
            assert!(a.contains("slack"), "alice should see her own Slack fact");
            assert!(!a.contains("teams"), "LEAK: alice saw Bob's Teams fact");

            // …and symmetrically for Bob.
            let (_r, b_facts) = engine.retrieve_tagged("tenant_bob", "what chat tool does the company use", None, 8).await.expect("bob retrieve");
            let b = b_facts.join(" | ").to_lowercase();
            eprintln!("bob sees: {b_facts:#?}");
            assert!(b.contains("teams"), "bob should see his own Teams fact");
            assert!(!b.contains("slack"), "LEAK: bob saw Alice's Slack fact");

            // Profiles are per-namespace too.
            let ap = engine.profile_tagged("tenant_alice").await.as_prompt_block().to_lowercase();
            assert!(!ap.contains("teams"), "LEAK: alice's profile mentions Bob's Teams");

            // SS-2: delete is namespace-scoped. Find one of Bob's documents.
            let bob_docs = engine.list_document_ids("tenant_bob", None, 10).await.expect("bob docs");
            let bob_id = bob_docs.first().map(|(id, ..)| id.clone()).expect("bob has a document");
            // Alice cannot delete Bob's document — wrong namespace → no-op (404).
            let cross = engine.delete_document_tagged(&bob_id, "tenant_alice").await.expect("cross delete");
            assert!(!cross, "LEAK: alice deleted a document she does not own");
            let (_r, b2) = engine.retrieve_tagged("tenant_bob", "what chat tool does the company use", None, 8).await.expect("bob retrieve 2");
            assert!(b2.join(" ").to_lowercase().contains("teams"), "cross-tenant delete wrongly removed Bob's data");
            // Bob can delete his own document.
            let own = engine.delete_document_tagged(&bob_id, "tenant_bob").await.expect("own delete");
            assert!(own, "bob could not delete his own document");

            let _ = qdrant::delete_collection(&http, &cfg.qdrant_url, &cfg.qdrant_api_key, &cfg.chunks_collection).await;
            let _ = qdrant::delete_collection(&http, &cfg.qdrant_url, &cfg.qdrant_api_key, &cfg.facts_collection).await;
        });
    }
}
