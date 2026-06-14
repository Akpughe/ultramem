//! Contextual Retrieval (after Anthropic's technique, used in a modified form
//! by SuperMemory): before a chunk is embedded, it gets a short blurb situating
//! it inside its source document. A chunk that reads "the migration is blocked
//! on the payments team" embeds far better for "what's blocking Recally's
//! payments work" when prefixed with "This is from Newton's Q3 review of the
//! Recally payments migration."
//!
//! Anthropic situate every chunk individually (N calls/doc). We take the cheap
//! 80/20: ONE doc-level blurb per document, prepended to every chunk's embed
//! input. Because Recally ranks at the document level (group-by-doc, H@k is a
//! doc metric), a doc-level context is well matched to what we optimize — at a
//! flat one-call-per-document cost instead of one-per-chunk.
//!
//! The blurb only shapes the embedding vector; the stored/displayed chunk text
//! stays clean.

use crate::llm::ResolvedModel;
use crate::providers::Llm;

const SYSTEM: &str = "You write a one-sentence context header that situates a document inside a \
user's personal memory so its chunks embed and retrieve well. Given a title and the document's \
opening text, output ONE concise sentence (max 30 words) naming what the document is, its topic, \
and who/what it concerns. No preamble, no quotes — just the sentence. Example: \
\"Newton's Q3 engineering review covering the Recally payments migration and its blockers.\"";

/// Generate a one-line situating blurb for a document. Returns None on any
/// failure — the caller falls back to the plain title prefix, so context is a
/// pure bonus and never blocks ingest.
pub async fn doc_context(
    llm: &dyn Llm,
    model: &ResolvedModel,
    title: &str,
    content: &str,
) -> Option<String> {
    if !model.is_ready() {
        return None;
    }
    // The opening of a document is almost always enough to characterise it,
    // and keeps this a cheap, single, short-input call.
    let head: String = content.chars().take(2000).collect();
    let user = format!("Title: {title}\n\n{head}");
    match llm.chat(model, SYSTEM, &user, 0.2).await {
        Ok(raw) => {
            let line = raw
                .trim()
                .trim_matches('"')
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            // Guard against the model echoing instructions or returning junk.
            if line.len() >= 12 && line.chars().count() <= 240 {
                Some(line)
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_context_skips_when_model_unready() {
        let llm = crate::llm::LlmClient::new();
        let model = ResolvedModel::groq(String::new(), "x");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt.block_on(doc_context(&llm, &model, "t", "body"));
        assert!(out.is_none());
    }
}
