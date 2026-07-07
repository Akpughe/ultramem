//! UltraMem eval harness — ported from Recally's `src-tauri/src/bin/probe.rs`.
//!
//! Standalone proof that the engine works against a live Qdrant. Recally drove
//! these modes off its SQLite `memories_log`; UltraMem has no external store, so
//! enumeration goes through `MemoryEngine::list_document_ids` (a Qdrant scroll)
//! and query generation uses the engine's own `LlmClient` (`cfg.plan_model`).
//!
//! Modes:
//!   probe memtest              LongMemEval-style memory capability suite (the gate)
//!   probe bench [build [n]]    frozen golden-set retrieval bench (H@k / MRR / MemScore)
//!   probe abtest [feat] [build [n]]
//!                              A/B re-index bench for an ingest-side feature
//!                              (feat = contextual | chunking | hybrid)
//!   probe reindex <tags <tag> | latest | facts [tag]>
//!                              reprocess WITHOUT re-extraction (reuses stored chunk text)
//!   probe profile [tag]        compile + print the standing profile
//!
//! Env: QDRANT_URL / QDRANT_API_KEY / JINA_API_KEY / MISTRAL_API_KEY / GROQ_API_KEY.
//! Harness overrides: ULTRAMEM_GOLDEN, ULTRAMEM_CORPUS, ULTRAMEM_AB_LIMIT.

use std::sync::Arc;
use ultramem_core::engine::{qdrant, EngineCfg, IngestDoc, MemoryEngine, DEFAULT_TAG};
use ultramem_core::LlmClient;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_filename("../.env");
    let _ = dotenvy::from_filename(".env");

    let arg = std::env::args().nth(1).unwrap_or_default();

    let cfg = EngineCfg::from_env();
    assert!(!cfg.qdrant_url.is_empty(), "QDRANT_URL not set");
    let engine = Arc::new(MemoryEngine::new(cfg.clone()));
    assert!(
        engine.health().await,
        "engine unhealthy — check QDRANT_URL / JINA_API_KEY"
    );
    engine.ensure_collections().await.expect("collections");

    match arg.as_str() {
        "memtest" => run_memtest(&cfg).await,
        "bench" => {
            // probe bench build [n]  -> (re)generate eval/golden.json
            // probe bench            -> run the frozen set, print H@k/MRR/latency/tokens
            let build = std::env::args().nth(2).as_deref() == Some("build");
            run_bench(&engine, &cfg, build).await;
        }
        "abtest" => {
            // probe abtest <feature> build [n]  -> freeze eval/corpus.json
            // probe abtest <feature>            -> baseline vs enhanced
            let build = std::env::args().any(|a| a == "build");
            run_abtest(&engine, &cfg, build).await;
        }
        "seed" => {
            // probe seed <file.json>  -> ingest a committed [{title,content}] corpus
            // into the configured collections (DEFAULT_TAG). Reproducible bench setup.
            let path = std::env::args()
                .nth(2)
                .unwrap_or_else(|| "eval/corpus_demo.json".into());
            run_seed(&engine, &path).await;
        }
        "drop" => {
            // probe drop  -> delete the configured chunks+facts collections (cleanup).
            let http = reqwest::Client::new();
            for c in [&cfg.chunks_collection, &cfg.facts_collection] {
                match qdrant::delete_collection(&http, &cfg.qdrant_url, &cfg.qdrant_api_key, c)
                    .await
                {
                    Ok(()) => println!("dropped {c}"),
                    Err(e) => eprintln!("drop {c} failed: {e}"),
                }
            }
        }
        "reindex" => run_reindex(&engine, &cfg).await,
        "profile" => {
            let tag = std::env::args()
                .nth(2)
                .unwrap_or_else(|| DEFAULT_TAG.to_string());
            let p = engine.profile_tagged(&tag).await;
            println!("================ STANDING PROFILE ({tag}) ================\n");
            if p.is_empty() {
                println!("(empty — no memories yet, or no distill model configured)");
            } else {
                print!("{}", p.as_prompt_block());
            }
        }
        other => {
            eprintln!("unknown mode {other:?}");
            eprintln!("usage: probe <memtest | bench [build [n]] | abtest [feat] [build [n]] | reindex … | profile [tag]>");
        }
    }
}

// ============ MEMORY CAPABILITY SUITE (LongMemEval-style) ============
// Proves the *memory* layer, not just chunk retrieval: can the system recall a
// fact, synthesize across documents, and — crucially — return the CURRENT
// answer after a knowledge update (the thing plain RAG can't do)? Each scenario
// ingests scripted documents into fresh collections, then checks the latest
// memories. Prints a pass/fail scorecard with an accuracy score.

struct MemScenario {
    name: &'static str,
    /// Documents ingested in order (later ones may update earlier memories).
    docs: Vec<&'static str>,
    query: &'static str,
    /// Lowercased substrings that SHOULD appear in the latest facts.
    must_contain: Vec<&'static str>,
    /// Lowercased substrings that must NOT appear in the latest facts (they were
    /// superseded). Empty to skip.
    must_absent: Vec<&'static str>,
    /// Require at least one memory to have been superseded (is_latest=false).
    expect_superseded: bool,
}

async fn run_memtest(cfg: &EngineCfg) {
    let scenarios = vec![
        MemScenario {
            name: "single-fact recall",
            docs: vec![
                "Engineering notes for the week. The user's primary programming language is Rust, \
                 which they use every single day to build the Recally desktop application on top of \
                 Tauri. They have written Rust professionally for about five years now and strongly \
                 prefer it for systems and backend work over Go and Python. The user maintains \
                 several open-source Rust crates and reviews Rust pull requests for their team \
                 regularly, and they mentor two junior engineers who are learning the language.",
            ],
            query: "what programming language does the user mainly use",
            must_contain: vec!["rust"],
            must_absent: vec![],
            expect_superseded: false,
        },
        MemScenario {
            name: "cross-document synthesis",
            docs: vec![
                "Project kickoff document for the new initiative. Project Zephyr is a brand new \
                 payments platform being built at the company this year. It is led by Alex, who is \
                 the engineering lead for the whole initiative and reports directly to the VP of \
                 Engineering. The team has eight engineers and started work in the second quarter. \
                 Zephyr replaces the legacy billing system that the company has outgrown.",
                "Roadmap update for Project Zephyr planning. Project Zephyr is scheduled to launch in \
                 September 2026 and will integrate Stripe as its primary payment processor for the \
                 very first release. The launch will start with a limited beta for enterprise \
                 customers before a wider rollout, and the team is targeting full general \
                 availability by the end of the year with multi-currency support.",
            ],
            query: "who leads project zephyr and when does it launch",
            must_contain: vec!["zephyr"],
            must_absent: vec![],
            expect_superseded: false,
        },
        MemScenario {
            name: "knowledge update (contradiction)",
            docs: vec![
                "Personal footwear note for the record. For the last several years the user has \
                 exclusively worn Adidas running shoes for every training session and considers \
                 Adidas their favorite and preferred running shoe brand for all training and racing. \
                 They own multiple pairs of Adidas Ultraboost, buy Adidas exclusively, and \
                 recommend the Adidas brand to everyone at their running club without exception.",
                "Footwear update from this month, an important change. The user has now switched \
                 entirely away from Adidas and only wears Puma running shoes going forward. Puma is \
                 now their current and preferred running shoe brand, they have replaced their whole \
                 shoe rotation with Puma Deviate models, and they no longer buy Adidas at all. The \
                 old Adidas preference has been completely replaced by the new Puma preference now.",
            ],
            query: "what running shoe brand does the user prefer now",
            must_contain: vec!["puma"],
            // The superseded brand must be GONE from the served facts, not merely
            // outranked — this is the "only the current fact is served" guarantee.
            must_absent: vec!["adidas"],
            expect_superseded: true,
        },
        MemScenario {
            name: "contradiction chain (A to B to C)",
            docs: vec![
                "Footwear note for the record. The user's preferred running shoe brand is Adidas. \
                 They train exclusively in Adidas, own several pairs of Adidas Ultraboost, and \
                 recommend Adidas to everyone at their running club without a single exception.",
                "Footwear update. The user has switched away from Adidas and now runs only in Nike. \
                 Nike is now their current and preferred running shoe brand; they replaced their \
                 whole rotation with Nike Pegasus and no longer buy Adidas at all going forward.",
                "Another footwear update, the latest one. The user has now moved on from Nike as well \
                 and switched entirely to Puma. Puma is now their current and preferred running shoe \
                 brand; they replaced the Nike rotation with Puma Deviate and no longer buy Nike.",
            ],
            query: "what running shoe brand does the user prefer now",
            must_contain: vec!["puma"],
            // Every earlier link in the chain must be gone, not just the first.
            must_absent: vec!["adidas", "nike"],
            expect_superseded: true,
        },
    ];

    let http = reqwest::Client::new();
    let mut passed = 0usize;
    let total = scenarios.len();
    println!("================ MEMORY CAPABILITY SUITE ================\n");

    for sc in &scenarios {
        let mut cfg = cfg.clone();
        let tag = sc.name.replace(' ', "_").replace(['(', ')'], "");
        cfg.chunks_collection = format!("ultramem_mem_{tag}_c");
        cfg.facts_collection = format!("ultramem_mem_{tag}_f");
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
        let engine = MemoryEngine::new(cfg.clone());
        if engine.ensure_collections().await.is_err() {
            println!("FAIL {} (collections)", sc.name);
            continue;
        }

        for (i, body) in sc.docs.iter().enumerate() {
            let doc = IngestDoc {
                source: "file".into(),
                title: format!("{} doc {}", sc.name, i + 1),
                content: body.to_string(),
                reference: format!("/mem/{tag}/{i}"),
                app: String::new(),
                captured_at: 1_750_000_000 + i as i64 * 86_400,
                file_path: None,
                container_tag: String::new(),
            };
            if let Err(e) = engine.add_document(&doc).await {
                println!("  ingest error: {e}");
            }
        }

        let (_r, facts) = engine.retrieve_raw(sc.query, 10).await.unwrap_or_default();
        let joined = facts.join(" | ").to_lowercase();
        let all = qdrant::scroll(
            &http,
            &cfg.qdrant_url,
            &cfg.qdrant_api_key,
            &cfg.facts_collection,
            200,
        )
        .await
        .unwrap_or_default();
        let superseded = all
            .iter()
            .filter(|p| p["payload"]["is_latest"].as_bool() == Some(false))
            .count();

        let contains_ok = sc.must_contain.iter().all(|s| joined.contains(s));
        let absent_ok = sc.must_absent.iter().all(|s| !joined.contains(s));
        let superseded_ok = !sc.expect_superseded || superseded >= 1;
        let pass = contains_ok && absent_ok && superseded_ok;
        if pass {
            passed += 1;
        }
        println!("{} {}", if pass { "PASS" } else { "FAIL" }, sc.name);
        if !pass {
            if !contains_ok {
                println!(
                    "     missing expected: {:?}",
                    sc.must_contain
                        .iter()
                        .filter(|s| !joined.contains(**s))
                        .collect::<Vec<_>>()
                );
            }
            if !absent_ok {
                println!(
                    "     leaked superseded: {:?}",
                    sc.must_absent
                        .iter()
                        .filter(|s| joined.contains(**s))
                        .collect::<Vec<_>>()
                );
            }
            if !superseded_ok {
                println!("     expected a superseded memory, found none");
            }
            println!(
                "     latest facts: {}",
                joined.chars().take(200).collect::<String>()
            );
        }

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
    }

    println!("\n================ SCORE ================");
    println!(
        "  {passed}/{total} scenarios passed  ({}%)",
        passed * 100 / total.max(1)
    );
}

// ============ SEED: ingest a committed corpus (reproducible bench setup) ============

#[derive(serde::Deserialize)]
struct SeedDoc {
    title: String,
    content: String,
}

async fn run_seed(engine: &Arc<MemoryEngine>, path: &str) {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read corpus {path}: {e}");
            return;
        }
    };
    let docs: Vec<SeedDoc> =
        serde_json::from_str(&raw).expect("parse seed corpus (expected [{title,content}])");
    let total = docs.len();
    println!("seeding {total} docs into '{}' …", engine_chunks_label());
    let sem = Arc::new(tokio::sync::Semaphore::new(6));
    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for (i, d) in docs.into_iter().enumerate() {
        let (engine, sem, done) = (engine.clone(), sem.clone(), done.clone());
        handles.push(tokio::spawn(async move {
            let _p = sem.acquire_owned().await.unwrap();
            let doc = IngestDoc {
                source: "file".into(),
                title: d.title,
                content: d.content,
                reference: format!("/seed/{i}"),
                app: String::new(),
                captured_at: 1_750_000_000 + i as i64,
                file_path: None,
                container_tag: String::new(),
            };
            match engine.add_document(&doc).await {
                Ok(_) => {
                    done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Err(e) => eprintln!("  seed doc {i} failed: {e}"),
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    println!(
        "seeded {}/{total} docs",
        done.load(std::sync::atomic::Ordering::Relaxed)
    );
}

fn engine_chunks_label() -> String {
    std::env::var("ULTRAMEM_CHUNKS_COLLECTION").unwrap_or_else(|_| "ultramem_chunks".into())
}

// ============ FROZEN GOLDEN-SET RETRIEVAL BENCH ============
// Deterministic retrieval benchmark against a frozen golden set, so before/after
// numbers are comparable across code changes.

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct GoldenItem {
    query: String,
    doc_id: String,
    title: String,
}

fn golden_path() -> std::path::PathBuf {
    std::env::var("ULTRAMEM_GOLDEN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("eval/golden.json"))
}

async fn run_bench(engine: &MemoryEngine, cfg: &EngineCfg, build: bool) {
    let path = golden_path();
    if build || !path.exists() {
        build_golden(engine, cfg, &path).await;
    }
    let golden: Vec<GoldenItem> = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).expect("parse golden.json"),
        Err(e) => {
            eprintln!(
                "no golden set at {} ({e}); run `probe bench build`",
                path.display()
            );
            return;
        }
    };
    if golden.is_empty() {
        eprintln!("golden set is empty — ingest some documents first, then `probe bench build`");
        return;
    }
    println!(
        "benchmarking {} frozen queries from {}\n",
        golden.len(),
        path.display()
    );

    // Approx GPT tokens from chars. Good enough for a relative trend metric.
    let approx_tokens = |chars: usize| (chars as f64 / 4.0).round() as usize;

    let mut ranks: Vec<i32> = Vec::with_capacity(golden.len());
    let mut latencies_ms: Vec<u128> = Vec::with_capacity(golden.len());
    let mut tokens: Vec<usize> = Vec::with_capacity(golden.len());
    let mut lines: Vec<(i32, String, String)> = Vec::new();

    for item in &golden {
        let t0 = std::time::Instant::now();
        let (docs, facts) = engine.retrieve(&item.query, 10).await.unwrap_or_default();
        latencies_ms.push(t0.elapsed().as_millis());

        let rank = docs
            .iter()
            .position(|d| d.document_id == item.doc_id)
            .map(|p| p as i32 + 1)
            .unwrap_or(-1);
        ranks.push(rank);

        // Tokens injected = what the answer model would actually receive: the
        // top-8 docs' chunk bodies (capped at 900 chars) + facts.
        let mut ctx_chars = 0usize;
        for r in docs.iter().take(8) {
            let body: usize = r.chunks.iter().map(|c| c.content.chars().count()).sum();
            ctx_chars += body.min(900);
        }
        ctx_chars += facts
            .iter()
            .take(10)
            .map(|f| f.chars().count())
            .sum::<usize>();
        tokens.push(approx_tokens(ctx_chars));

        let top1 = docs
            .first()
            .and_then(|d| d.title.clone())
            .unwrap_or_default();
        lines.push((rank, item.title.clone(), top1));
    }

    lines.sort_by_key(|l| l.0);
    for (rank, title, top1) in &lines {
        let tag = if *rank == 1 {
            "#1  ".into()
        } else if *rank > 0 {
            format!("#{rank}  ")
        } else {
            "MISS".to_string()
        };
        println!("{:<5} {}", tag, title.chars().take(58).collect::<String>());
        if *rank < 0 || *rank > 3 {
            println!(
                "        instead: {}",
                top1.chars().take(58).collect::<String>()
            );
        }
    }

    let n = ranks.len().max(1);
    let h1 = ranks.iter().filter(|r| **r == 1).count();
    let h3 = ranks.iter().filter(|r| (1..=3).contains(*r)).count();
    let h10 = ranks.iter().filter(|r| (1..=10).contains(*r)).count();
    let miss = ranks.iter().filter(|r| **r < 0).count();
    let mrr: f64 = ranks
        .iter()
        .filter(|r| **r > 0)
        .map(|r| 1.0 / *r as f64)
        .sum::<f64>()
        / n as f64;
    let mean = |v: &[u128]| {
        if v.is_empty() {
            0
        } else {
            v.iter().sum::<u128>() / v.len() as u128
        }
    };
    let mut lat_sorted = latencies_ms.clone();
    lat_sorted.sort_unstable();
    let p95 = lat_sorted
        .get((lat_sorted.len() * 95 / 100).min(lat_sorted.len().saturating_sub(1)))
        .copied()
        .unwrap_or(0);
    let mean_tokens = if tokens.is_empty() {
        0
    } else {
        tokens.iter().sum::<usize>() / tokens.len()
    };

    println!("\n================ BENCH ({} queries) ================", n);
    println!("  H@1 (rank 1):     {h1}  ({}%)", h1 * 100 / n);
    println!("  H@3 (top-3):      {h3}  ({}%)", h3 * 100 / n);
    println!("  H@10 (top-10):    {h10}  ({}%)", h10 * 100 / n);
    println!("  MISS (>10):       {miss}  ({}%)", miss * 100 / n);
    println!("  MRR:              {mrr:.3}");
    println!("  latency mean:     {} ms", mean(&latencies_ms));
    println!("  latency p95:      {p95} ms");
    println!("  tokens injected:  {mean_tokens} mean (≈ context sent to answer model)");
    // MemScore (after SuperMemory's memorybench): quality is the headline,
    // efficiency tunes it. quality = MRR; efficiency multipliers ramp in only
    // when latency >2s or context >2k tokens, so a fast precise system scores ~MRR·100.
    let mean_lat = mean(&latencies_ms) as f64;
    let lat_factor = (2000.0 / mean_lat.max(1.0)).clamp(0.5, 1.0);
    let tok_factor = (2000.0 / (mean_tokens as f64).max(1.0)).clamp(0.5, 1.0);
    let memscore = (100.0 * mrr * (0.7 + 0.15 * lat_factor + 0.15 * tok_factor)).round();
    println!(
        "  MemScore:         {memscore}/100  (quality {:.0} × efficiency)",
        mrr * 100.0
    );
    println!("  --- copy this line to track deltas ---");
    println!(
        "  H@1={}% H@3={}% H@10={}% MRR={mrr:.3} lat={}ms tok={mean_tokens} mem={memscore}",
        h1 * 100 / n,
        h3 * 100 / n,
        h10 * 100 / n,
        mean(&latencies_ms)
    );
}

/// Build (freeze) the golden set: deterministic doc sample + one generated query
/// each. Self-supervising (query derived from a known doc's own text) but
/// persisted so the queries never change. Enumerates the live index via
/// `list_document_ids` (no external store) and generates queries with the
/// engine's own `LlmClient` (`cfg.plan_model`).
async fn build_golden(engine: &MemoryEngine, cfg: &EngineCfg, path: &std::path::Path) {
    let n: usize = std::env::args()
        .nth(3)
        .and_then(|a| a.parse().ok())
        .unwrap_or(60);
    let mut rows = engine
        .list_document_ids(DEFAULT_TAG, None, 100_000)
        .await
        .unwrap_or_default();
    rows.sort_by(|a, b| a.3.cmp(&b.3)); // deterministic order by reference
    let total = rows.len();
    let stride = (total / n.max(1)).max(1);
    let sample: Vec<_> = rows.iter().step_by(stride).take(n).cloned().collect();
    println!(
        "freezing {} golden queries from {} indexed docs…",
        sample.len(),
        total
    );

    let llm = LlmClient::new();
    let http = reqwest::Client::new();
    let sem = Arc::new(tokio::sync::Semaphore::new(6));
    let out = Arc::new(tokio::sync::Mutex::new(Vec::<GoldenItem>::new()));
    let mut handles = Vec::new();
    for (doc_id, title, _source, _reference, _captured_at) in sample {
        let (cfg, llm, http, sem, out) = (
            cfg.clone(),
            llm.clone(),
            http.clone(),
            sem.clone(),
            out.clone(),
        );
        handles.push(tokio::spawn(async move {
            let _p = sem.acquire_owned().await.unwrap();
            let chunks = qdrant::chunks_of_doc(&http, &cfg.qdrant_url, &cfg.qdrant_api_key, &cfg.chunks_collection, &doc_id, 2).await.unwrap_or_default();
            let content: String = chunks.join("\n").chars().take(1400).collect();
            if content.trim().len() < 40 { return; }
            let sys = "You are given the text of a document a user saved. Write ONE natural search query (8-16 words) the user would type to find this document later from memory. Base it on the document's topic and purpose. Do NOT mention the filename or quote long phrases verbatim. Respond with ONLY the query.";
            let query = match llm.chat(&cfg.plan_model, sys, &content, 0.0).await {
                Ok(q) => q.trim().trim_matches('"').lines().next().unwrap_or("").to_string(),
                Err(_) => return,
            };
            if query.is_empty() { return; }
            out.lock().await.push(GoldenItem { query, doc_id, title });
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    let mut golden = Arc::try_unwrap(out).unwrap().into_inner();
    golden.sort_by(|a, b| a.doc_id.cmp(&b.doc_id)); // stable order
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, serde_json::to_string_pretty(&golden).unwrap())
        .expect("write golden.json");
    println!(
        "wrote {} golden queries to {}",
        golden.len(),
        path.display()
    );
}

// ============ REINDEX: reprocess without re-extracting ============
// The extracted text already lives in each chunk's payload, so we never touch
// the original files again. Cheap modes (tags/latest) are payload-only updates
// that also reuse the embeddings; `facts` reconstructs each doc's text from its
// chunks and re-runs distillation + the memory lifecycle (re-embeds only facts).

async fn run_reindex(engine: &Arc<MemoryEngine>, cfg: &EngineCfg) {
    let mode = std::env::args().nth(2).unwrap_or_default();
    match mode.as_str() {
        "tags" => {
            let tag = std::env::args().nth(3).unwrap_or_default();
            if tag.is_empty() {
                eprintln!("usage: probe reindex tags <container_tag>");
                return;
            }
            println!("claiming all un-namespaced data into tag '{tag}' (payload-only)…");
            match engine.claim_legacy_into_tag(&tag).await {
                Ok(()) => println!("  done — existing chunks + facts now belong to '{tag}'"),
                Err(e) => eprintln!("  failed: {e}"),
            }
            match engine.backfill_facts_latest().await {
                Ok(()) => println!("  is_latest backfilled on legacy facts"),
                Err(e) => eprintln!("  is_latest backfill failed: {e}"),
            }
        }
        "latest" => {
            println!("backfilling is_latest=true on legacy facts (payload-only)…");
            match engine.backfill_facts_latest().await {
                Ok(()) => println!("  done"),
                Err(e) => eprintln!("  failed: {e}"),
            }
        }
        "facts" => {
            let tag = std::env::args()
                .nth(3)
                .unwrap_or_else(|| DEFAULT_TAG.to_string());
            reindex_facts(engine, cfg, &tag).await;
        }
        _ => {
            eprintln!("usage: probe reindex <tags <tag> | latest | facts [tag]>");
        }
    }
}

/// Re-distill facts for every indexed document in `tag`, reconstructing each
/// doc's text from its stored chunks (NO file access). Re-embeds only the small
/// fact set. Enumeration via `list_document_ids`; the per-doc work is the
/// engine's own `reindex_doc_facts`.
async fn reindex_facts(engine: &Arc<MemoryEngine>, _cfg: &EngineCfg, tag: &str) {
    let mut rows = engine
        .list_document_ids(tag, None, 100_000)
        .await
        .unwrap_or_default();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let total = rows.len();
    println!("re-distilling facts for {total} docs from stored chunk text (tag '{tag}')…");

    let sem = Arc::new(tokio::sync::Semaphore::new(4));
    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = Vec::new();
    for (doc_id, title, source, reference, captured_at) in rows {
        let (engine, sem, done, tag) = (engine.clone(), sem.clone(), done.clone(), tag.to_string());
        handles.push(tokio::spawn(async move {
            let _p = sem.acquire_owned().await.unwrap();
            let r = engine
                .reindex_doc_facts(&doc_id, &title, &source, &reference, captured_at, &tag)
                .await;
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            match r {
                Ok(facts) => {
                    if n % 50 == 0 || n == total {
                        println!("  {n}/{total} done ({facts} facts last)");
                    }
                }
                Err(e) => eprintln!("  doc {doc_id} failed: {e}"),
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    println!(
        "re-distill complete: {} docs processed",
        done.load(std::sync::atomic::Ordering::Relaxed)
    );
}

// ============ A/B RE-INDEX BENCHMARK: ingest-side features ============
// `bench` measures the live production index, which was built with whatever
// pipeline produced it — so it can't show the effect of an INGEST-time change
// (new embedding input, new chunking) without a re-index. `abtest` does the
// honest thing: take a frozen corpus, ingest it twice into throwaway
// collections (feature OFF, then ON), replay the same queries against each,
// print both scores and the delta. Same corpus, same queries, one variable.

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct CorpusItem {
    title: String,
    query: String,
    content: String,
}

fn corpus_path() -> std::path::PathBuf {
    std::env::var("ULTRAMEM_CORPUS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("eval/corpus.json"))
}

/// Which single feature an A/B run isolates. `apply` sets exactly that flag;
/// all other engine flags are held constant across both arms.
#[derive(Clone, Copy)]
enum Feature {
    Contextual,
    Chunking,
    Hybrid,
}

impl Feature {
    fn parse(s: &str) -> Feature {
        match s {
            "chunking" => Feature::Chunking,
            "hybrid" => Feature::Hybrid,
            _ => Feature::Contextual,
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Feature::Contextual => "contextual",
            Feature::Chunking => "smart_chunking",
            Feature::Hybrid => "hybrid_search",
        }
    }
    fn apply(&self, cfg: &mut EngineCfg, on: bool) {
        // Hold every ingest-side flag fixed, then flip only the one under test,
        // so the two arms differ by exactly one variable.
        cfg.contextual = false;
        cfg.smart_chunking = false;
        cfg.hybrid_search = false;
        match self {
            Feature::Contextual => cfg.contextual = on,
            Feature::Chunking => cfg.smart_chunking = on,
            Feature::Hybrid => cfg.hybrid_search = on,
        }
    }
}

/// One scoring pass: ingest the corpus into throwaway collections with the
/// engine `cfg`, then replay each query and rank that item's freshly-minted
/// doc. Both ingest and replay run concurrently (bounded). Returns
/// (H@1%, H@3%, H@10%, MRR).
async fn abtest_variant(
    base_cfg: &EngineCfg,
    corpus: &[CorpusItem],
    feature: Feature,
    on: bool,
) -> (usize, usize, usize, f64) {
    let mut cfg = base_cfg.clone();
    feature.apply(&mut cfg, on);
    cfg.distill = false; // isolate chunk retrieval; distillation is orthogonal
    cfg.chunks_collection = "ultramem_ab_chunks".into();
    cfg.facts_collection = "ultramem_ab_facts".into();
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
    let engine = Arc::new(MemoryEngine::new(cfg.clone()));
    engine
        .ensure_collections()
        .await
        .expect("ensure ab collections");

    // Ingest, recording each item's new doc_id (parallel, bounded).
    let sem = Arc::new(tokio::sync::Semaphore::new(8));
    let mut handles = Vec::new();
    for (i, item) in corpus.iter().enumerate() {
        let (engine, sem, item) = (engine.clone(), sem.clone(), item.clone());
        handles.push(tokio::spawn(async move {
            let _p = sem.acquire_owned().await.unwrap();
            let doc = IngestDoc {
                source: "file".into(),
                title: item.title.clone(),
                content: item.content.clone(),
                reference: format!("/ab/{i}"),
                app: String::new(),
                captured_at: 1_750_000_000,
                file_path: None,
                container_tag: String::new(),
            };
            (i, engine.add_document(&doc).await.ok())
        }));
    }
    let mut doc_ids: Vec<Option<String>> = vec![None; corpus.len()];
    for h in handles {
        if let Ok((i, id)) = h.await {
            doc_ids[i] = id;
        }
    }

    // Replay queries — concurrently; this is the slow part (full pipeline/query).
    let doc_ids = Arc::new(doc_ids);
    let mut qhandles = Vec::new();
    for (i, item) in corpus.iter().enumerate() {
        let (engine, sem, doc_ids, query) = (
            engine.clone(),
            sem.clone(),
            doc_ids.clone(),
            item.query.clone(),
        );
        qhandles.push(tokio::spawn(async move {
            let _p = sem.acquire_owned().await.unwrap();
            let expected = doc_ids[i].clone()?;
            // Planner-free: isolate the ingest-side variable, no planner noise.
            let (docs, _) = engine.retrieve_raw(&query, 10).await.unwrap_or_default();
            Some(
                docs.iter()
                    .position(|d| d.document_id == expected)
                    .map(|p| p as i32 + 1)
                    .unwrap_or(-1),
            )
        }));
    }
    let mut ranks: Vec<i32> = Vec::new();
    for h in qhandles {
        if let Ok(Some(r)) = h.await {
            ranks.push(r);
        }
    }

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

    let n = ranks.len().max(1);
    let h1 = ranks.iter().filter(|r| **r == 1).count() * 100 / n;
    let h3 = ranks.iter().filter(|r| (1..=3).contains(*r)).count() * 100 / n;
    let h10 = ranks.iter().filter(|r| (1..=10).contains(*r)).count() * 100 / n;
    let mrr: f64 = ranks
        .iter()
        .filter(|r| **r > 0)
        .map(|r| 1.0 / *r as f64)
        .sum::<f64>()
        / n as f64;
    (h1, h3, h10, mrr)
}

async fn run_abtest(engine: &MemoryEngine, cfg: &EngineCfg, build: bool) {
    let path = corpus_path();
    if build || !path.exists() {
        build_corpus(engine, cfg, &path).await;
    }
    let mut corpus: Vec<CorpusItem> = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).expect("parse corpus.json"),
        Err(e) => {
            eprintln!(
                "no corpus at {} ({e}); run `probe abtest build`",
                path.display()
            );
            return;
        }
    };
    if corpus.is_empty() {
        eprintln!(
            "corpus is empty — ingest some documents first, then `probe abtest <feat> build`"
        );
        return;
    }
    // ULTRAMEM_AB_LIMIT caps corpus size — smaller = faster, still has distractors.
    if let Ok(lim) =
        std::env::var("ULTRAMEM_AB_LIMIT").map(|v| v.parse::<usize>().unwrap_or(usize::MAX))
    {
        corpus.truncate(lim);
    }
    // `probe abtest <feature> [build]` (defaults to contextual).
    let feature = Feature::parse(&std::env::args().nth(2).unwrap_or_default());
    println!(
        "A/B [{}] over {} docs from {}\n",
        feature.label(),
        corpus.len(),
        path.display()
    );

    println!("running BASELINE ({} OFF)…", feature.label());
    let (b1, b3, b10, bmrr) = abtest_variant(cfg, &corpus, feature, false).await;
    println!("running ENHANCED ({} ON)…", feature.label());
    let (e1, e3, e10, emrr) = abtest_variant(cfg, &corpus, feature, true).await;

    let d = |a: usize, b: usize| -> String { format!("{:+}", b as i64 - a as i64) };
    println!(
        "\n================ A/B RESULTS [{}] ================",
        feature.label()
    );
    println!("                 baseline   enhanced   delta");
    println!("  H@1            {b1:>5}%    {e1:>5}%    {}", d(b1, e1));
    println!("  H@3            {b3:>5}%    {e3:>5}%    {}", d(b3, e3));
    println!("  H@10           {b10:>5}%    {e10:>5}%    {}", d(b10, e10));
    println!(
        "  MRR            {bmrr:>6.3}   {emrr:>6.3}   {:+.3}",
        emrr - bmrr
    );
}

/// Freeze the A/B corpus: sample N indexed docs, reconstruct each one's text
/// from its chunks (in index order), generate one query each. Stored so the
/// corpus and queries never change between runs.
async fn build_corpus(engine: &MemoryEngine, cfg: &EngineCfg, path: &std::path::Path) {
    let n: usize = std::env::args()
        .nth(3)
        .and_then(|a| a.parse().ok())
        .unwrap_or(60);
    let mut rows = engine
        .list_document_ids(DEFAULT_TAG, None, 100_000)
        .await
        .unwrap_or_default();
    rows.sort_by(|a, b| a.3.cmp(&b.3));
    let total = rows.len();
    let stride = (total / n.max(1)).max(1);
    let sample: Vec<_> = rows.iter().step_by(stride).take(n).cloned().collect();
    println!("freezing A/B corpus from {} indexed docs…", total);

    let llm = LlmClient::new();
    let http = reqwest::Client::new();
    let sem = Arc::new(tokio::sync::Semaphore::new(6));
    let out = Arc::new(tokio::sync::Mutex::new(Vec::<CorpusItem>::new()));
    let mut handles = Vec::new();
    for (doc_id, title, _source, _reference, _captured_at) in sample {
        let (cfg, llm, http, sem, out) = (
            cfg.clone(),
            llm.clone(),
            http.clone(),
            sem.clone(),
            out.clone(),
        );
        handles.push(tokio::spawn(async move {
            let _p = sem.acquire_owned().await.unwrap();
            // Reconstruct the document's indexed text in chunk order.
            let mut chunks = qdrant::doc_chunks_indexed(&http, &cfg.qdrant_url, &cfg.qdrant_api_key, &cfg.chunks_collection, &doc_id, 200).await.unwrap_or_default();
            chunks.sort_by_key(|(i, _)| *i);
            let content: String = chunks.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n\n");
            if content.trim().chars().count() < 200 { return; }
            let head: String = content.chars().take(1400).collect();
            let sys = "You are given the text of a document a user saved. Write ONE natural search query (8-16 words) the user would type to find this document later from memory. Base it on the document's topic and purpose. Do NOT mention the filename or quote long phrases verbatim. Respond with ONLY the query.";
            let query = match llm.chat(&cfg.plan_model, sys, &head, 0.0).await {
                Ok(q) => q.trim().trim_matches('"').lines().next().unwrap_or("").to_string(),
                Err(_) => return,
            };
            if query.is_empty() { return; }
            out.lock().await.push(CorpusItem { title, query, content });
        }));
    }
    for h in handles {
        let _ = h.await;
    }

    let mut corpus = Arc::try_unwrap(out).unwrap().into_inner();
    corpus.sort_by(|a, b| a.title.cmp(&b.title));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, serde_json::to_string_pretty(&corpus).unwrap())
        .expect("write corpus.json");
    println!("wrote {} corpus docs to {}", corpus.len(), path.display());
}
