//! File text extraction. Jina Reader (r.jina.ai) is the primary extractor — one
//! cross-platform API that turns PDFs, Office docs and HTML into clean markdown.
//! It reads the text LAYER only (no OCR), so image/scanned PDFs come back empty
//! and the caller falls back to Mistral OCR. `local` is the offline path for
//! trivially-readable formats and the Office-doc fallback.

use std::path::Path;

const JINA_READER: &str = "https://r.jina.ai/";

fn mime_for(filename: &str) -> &'static str {
    let l = filename.to_lowercase();
    if l.ends_with(".pdf") {
        "application/pdf"
    } else if l.ends_with(".docx") {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    } else if l.ends_with(".doc") {
        "application/msword"
    } else if l.ends_with(".rtf") {
        "application/rtf"
    } else {
        "application/octet-stream"
    }
}

/// Extract a file's text via Jina Reader (multipart upload → markdown). An
/// empty result means there was no text layer (e.g. an image/scanned PDF) — the
/// caller should fall back to OCR.
pub async fn jina(
    http: &reqwest::Client,
    key: &str,
    bytes: Vec<u8>,
    filename: &str,
) -> Result<String, String> {
    if key.is_empty() {
        return Err("no Jina key".into());
    }
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename.to_string())
        .mime_str(mime_for(filename))
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new().part("file", part);
    let resp = http
        .post(JINA_READER)
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json")
        .multipart(form)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| format!("jina reader unreachable: {e}"))?;
    let status = resp.status();
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("jina reader bad response: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "jina reader {status}: {}",
            v["message"]
                .as_str()
                .or_else(|| v["readableMessage"].as_str())
                .unwrap_or("error")
        ));
    }
    Ok(v["data"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string())
}

/// Fetch and clean a web page's body as markdown via Jina Reader's URL mode
/// (GET r.jina.ai/{url}). Strips nav/ads/boilerplate server-side. Used to turn
/// a bare browser-history URL into searchable article text. Returns the cleaned
/// markdown (possibly truncated by the caller).
pub async fn jina_url(http: &reqwest::Client, key: &str, url: &str) -> Result<String, String> {
    if key.is_empty() {
        return Err("no Jina key".into());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("not an http(s) url".into());
    }
    let resp = http
        .get(format!("{JINA_READER}{url}"))
        .header("Authorization", format!("Bearer {key}"))
        .header("Accept", "application/json")
        .header("X-Return-Format", "markdown")
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| format!("jina reader unreachable: {e}"))?;
    let status = resp.status();
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("jina reader bad response: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "jina reader url {status}: {}",
            v["message"]
                .as_str()
                .or_else(|| v["readableMessage"].as_str())
                .unwrap_or("error")
        ));
    }
    Ok(v["data"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string())
}

/// Offline extraction: plain read for md/txt/csv, macOS `textutil` for Office
/// docs. Used as the fast path for plain text and the fallback when Jina yields
/// nothing for an Office doc.
pub fn local(path: &Path) -> Option<String> {
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
