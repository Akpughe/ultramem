//! Mistral OCR client — extracts markdown text from PDFs (including scanned
//! ones). Replaces the local `pdftotext` dependency.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Value};

pub const MODEL: &str = "mistral-ocr-latest";
const URL: &str = "https://api.mistral.ai/v1/ocr";

/// OCR a PDF. Returns all pages' markdown concatenated.
pub async fn ocr_pdf(
    http: &reqwest::Client,
    api_key: &str,
    pdf_bytes: &[u8],
) -> Result<String, String> {
    let data_url = format!("data:application/pdf;base64,{}", B64.encode(pdf_bytes));
    ocr(
        http,
        api_key,
        json!({ "type": "document_url", "document_url": data_url }),
    )
    .await
}

/// OCR an image (screenshot, photo of a doc, etc.). `mime` is the image's MIME
/// type, e.g. "image/png". Returns the recognized text as markdown.
pub async fn ocr_image(
    http: &reqwest::Client,
    api_key: &str,
    image_bytes: &[u8],
    mime: &str,
) -> Result<String, String> {
    let data_url = format!("data:{mime};base64,{}", B64.encode(image_bytes));
    ocr(
        http,
        api_key,
        json!({ "type": "image_url", "image_url": data_url }),
    )
    .await
}

/// MIME type for a supported image extension, or None if not an image we OCR.
pub fn image_mime(path: &str) -> Option<&'static str> {
    match path
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("bmp") => Some("image/bmp"),
        Some("tiff") | Some("tif") => Some("image/tiff"),
        _ => None,
    }
}

/// Shared OCR call over a `document` value (PDF or image).
async fn ocr(http: &reqwest::Client, api_key: &str, document: Value) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("no Mistral API key configured".into());
    }
    let resp = http
        .post(URL)
        .bearer_auth(api_key)
        .timeout(std::time::Duration::from_secs(180))
        .json(&json!({
            "model": MODEL,
            "document": document,
        }))
        .send()
        .await
        .map_err(|e| format!("mistral unreachable: {e}"))?;
    let status = resp.status();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("mistral bad response: {e}"))?;
    if !status.is_success() {
        let detail = v["message"]
            .as_str()
            .or_else(|| v["detail"].as_str())
            .or_else(|| v["error"]["message"].as_str())
            .unwrap_or("unknown");
        return Err(format!("mistral ocr error {status}: {detail}"));
    }
    let text = v["pages"]
        .as_array()
        .map(|pages| {
            pages
                .iter()
                .filter_map(|p| p["markdown"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err("mistral ocr returned no text".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_mime_maps_known_extensions() {
        assert_eq!(image_mime("a.PNG"), Some("image/png"));
        assert_eq!(image_mime("/x/y.jpeg"), Some("image/jpeg"));
        assert_eq!(image_mime("z.webp"), Some("image/webp"));
        assert!(image_mime("doc.pdf").is_none());
        assert!(image_mime("noext").is_none());
    }
}
