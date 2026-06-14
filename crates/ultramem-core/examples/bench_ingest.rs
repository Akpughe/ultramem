//! Ingest benchmark for the UltraMem memory engine — ported from Recally's
//! `src-tauri/src/bin/bench_ingest.rs`.
//!
//! Walks document folders, extracts text, and pushes up to N files (default 500)
//! through the full pipeline — OCR → chunk → embed → upsert → distill — at real
//! concurrency. Writes into throwaway `ultramem_bench_*` collections and drops
//! them afterwards, so it never pollutes real memories.
//!
//! Usage:
//!   cargo run -p ultramem-core --example bench_ingest [max_files] [dir ...]
//! Reads QDRANT_URL / QDRANT_API_KEY / JINA_API_KEY / MISTRAL_API_KEY /
//! GROQ_API_KEY from the environment or .env.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use ultramem_core::engine::{qdrant, EngineCfg, IngestDoc, MemoryEngine};
use walkdir::WalkDir;

const CONCURRENCY: usize = 8;
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_CONTENT_CHARS: usize = 20_000;
const EXTENSIONS: [&str; 8] = ["md", "txt", "rtf", "csv", "pdf", "docx", "doc", "pages"];
const SKIP_DIRS: [&str; 8] = [
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    ".supermemory",
    "Library",
];

fn should_skip(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.starts_with('.') && s.len() > 1 || SKIP_DIRS.contains(&s.as_ref())
    })
}

fn wanted(path: &Path) -> bool {
    path.extension()
        .map(|e| EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Local text extraction. PDFs pass through — the engine OCRs them. `textutil`
/// is macOS-only; on other platforms rtf/doc/docx/pages are simply skipped.
fn extract_text(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "md" | "txt" | "csv" => std::fs::read_to_string(path).ok(),
        "rtf" | "doc" | "docx" | "pages" => {
            let out = std::process::Command::new("textutil")
                .args(["-convert", "txt", "-stdout"])
                .arg(path)
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        }
        _ => None,
    }
}

fn collect_files(dirs: &[String], max: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in dirs {
        for entry in WalkDir::new(dir)
            .max_depth(6)
            .into_iter()
            .filter_entry(|e| !should_skip(e.path()))
            .flatten()
        {
            if files.len() >= max {
                return files;
            }
            if entry.file_type().is_file() && wanted(entry.path()) {
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if len > 0 && len <= MAX_FILE_BYTES {
                    files.push(entry.path().to_path_buf());
                }
            }
        }
    }
    files
}

fn percentile(sorted_ms: &[u128], p: f64) -> u128 {
    if sorted_ms.is_empty() {
        return 0;
    }
    let idx = ((sorted_ms.len() as f64 - 1.0) * p).round() as usize;
    sorted_ms[idx]
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_filename("../.env");
    let _ = dotenvy::from_filename(".env");

    let args: Vec<String> = std::env::args().skip(1).collect();
    let max_files: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(500);
    let home = dirs::home_dir().expect("home dir");
    let dirs: Vec<String> = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        ["Documents", "Desktop", "Downloads"]
            .iter()
            .map(|d| home.join(d).to_string_lossy().into_owned())
            .collect()
    };

    let mut cfg = EngineCfg::from_env();
    cfg.chunks_collection = "ultramem_bench_chunks".into();
    cfg.facts_collection = "ultramem_bench_facts".into();
    assert!(!cfg.qdrant_url.is_empty(), "QDRANT_URL not set");
    assert!(!cfg.jina_api_key.is_empty(), "JINA_API_KEY not set");
    let qdrant_url = cfg.qdrant_url.clone();
    let qdrant_key = cfg.qdrant_api_key.clone();

    let engine = Arc::new(MemoryEngine::new(cfg));
    assert!(
        engine.health().await,
        "engine unhealthy — check QDRANT_URL / keys"
    );
    engine
        .ensure_collections()
        .await
        .expect("ensure_collections");

    println!("Scanning {dirs:?} for up to {max_files} files…");
    let files = collect_files(&dirs, max_files);
    let pdf_count = files
        .iter()
        .filter(|f| {
            wanted(f)
                && f.extension()
                    .map(|e| e.to_string_lossy().to_lowercase() == "pdf")
                    .unwrap_or(false)
        })
        .count();
    println!(
        "Found {} files ({} PDFs → Mistral OCR). Ingesting at concurrency {CONCURRENCY}…\n",
        files.len(),
        pdf_count
    );

    let total_start = std::time::Instant::now();
    let sem = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
    let mut handles = Vec::new();

    for path in files {
        let engine = engine.clone();
        let sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.unwrap();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let is_pdf = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase() == "pdf")
                .unwrap_or(false);
            let (content, file_path) = if is_pdf {
                (
                    format!("PDF file \"{name}\""),
                    Some(path.to_string_lossy().into_owned()),
                )
            } else {
                let Some(text) = extract_text(&path) else {
                    return None; // unextractable — skip it
                };
                let text: String = text.chars().take(MAX_CONTENT_CHARS).collect();
                if text.trim().len() < 20 {
                    return None;
                }
                (format!("File \"{name}\":\n{text}"), None)
            };
            let doc = IngestDoc {
                source: "file".into(),
                title: name.clone(),
                content,
                reference: path.to_string_lossy().into_owned(),
                app: String::new(),
                captured_at: 1_750_000_000,
                file_path,
                container_tag: String::new(),
            };
            let start = std::time::Instant::now();
            let result = engine.add_document(&doc).await;
            let ms = start.elapsed().as_millis();
            match result {
                Ok(_) => {
                    println!("  ok   {ms:>6}ms  {name}");
                    Some((ms, is_pdf, true))
                }
                Err(e) => {
                    println!("  FAIL {ms:>6}ms  {name}: {e}");
                    Some((ms, is_pdf, false))
                }
            }
        }));
    }

    let mut ok_ms: Vec<u128> = Vec::new();
    let mut pdf_ms: Vec<u128> = Vec::new();
    let mut failures = 0usize;
    let mut skipped = 0usize;
    for h in handles {
        match h.await.ok().flatten() {
            Some((ms, is_pdf, true)) => {
                ok_ms.push(ms);
                if is_pdf {
                    pdf_ms.push(ms);
                }
            }
            Some((_, _, false)) => failures += 1,
            None => skipped += 1,
        }
    }
    let total = total_start.elapsed();

    ok_ms.sort_unstable();
    pdf_ms.sort_unstable();
    let sum: u128 = ok_ms.iter().sum();
    let n = ok_ms.len();

    println!("\n================ BENCHMARK ================");
    println!("ingested ok:      {n}");
    println!("failed:           {failures}");
    println!("skipped (empty/unreadable): {skipped}");
    println!("total wall time:  {:.1}s", total.as_secs_f64());
    if n > 0 {
        println!("avg per doc:      {}ms", sum / n as u128);
        println!("median per doc:   {}ms", percentile(&ok_ms, 0.5));
        println!("p95 per doc:      {}ms", percentile(&ok_ms, 0.95));
        println!("min / max:        {}ms / {}ms", ok_ms[0], ok_ms[n - 1]);
        println!(
            "throughput:       {:.1} docs/min",
            n as f64 / total.as_secs_f64() * 60.0
        );
    }
    if !pdf_ms.is_empty() {
        let psum: u128 = pdf_ms.iter().sum();
        println!(
            "pdf (OCR) avg:    {}ms over {} PDFs",
            psum / pdf_ms.len() as u128,
            pdf_ms.len()
        );
    }
    println!("===========================================");

    // Clean up: the benchmark never pollutes real collections.
    let http = reqwest::Client::new();
    for c in ["ultramem_bench_chunks", "ultramem_bench_facts"] {
        match qdrant::delete_collection(&http, &qdrant_url, &qdrant_key, c).await {
            Ok(()) => println!("dropped {c}"),
            Err(e) => println!("WARN: failed to drop {c}: {e}"),
        }
    }
}
