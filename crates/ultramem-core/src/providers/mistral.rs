//! Mistral OCR provider — the default for PDF + image text extraction. Thin
//! adapter over the low-level client in [`crate::engine::mistral`].

use super::Ocr;
use crate::engine::mistral;
use async_trait::async_trait;

/// `mistral-ocr-latest` for PDFs and images.
#[derive(Clone)]
pub struct MistralOcr {
    http: reqwest::Client,
    api_key: String,
}

impl MistralOcr {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self { http: reqwest::Client::new(), api_key: api_key.into() }
    }
}

#[async_trait]
impl Ocr for MistralOcr {
    async fn ocr_pdf(&self, bytes: &[u8]) -> Result<String, String> {
        mistral::ocr_pdf(&self.http, &self.api_key, bytes).await
    }
    async fn ocr_image(&self, bytes: &[u8], mime: &str) -> Result<String, String> {
        mistral::ocr_image(&self.http, &self.api_key, bytes, mime).await
    }
    fn image_mime(&self, path: &str) -> Option<&'static str> {
        mistral::image_mime(path)
    }
}
