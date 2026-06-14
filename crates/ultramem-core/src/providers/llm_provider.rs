//! [`Llm`] is implemented by the built-in [`LlmClient`], which is already
//! multi-provider (OpenAI-compatible + Anthropic) via [`ResolvedModel`]. The
//! trait is the seam; selection is `EngineCfg::with_models`.

use super::Llm;
use crate::llm::{LlmClient, ResolvedModel};
use async_trait::async_trait;
use serde_json::Value;

#[async_trait]
impl Llm for LlmClient {
    async fn chat(
        &self,
        m: &ResolvedModel,
        system: &str,
        user: &str,
        temperature: f64,
    ) -> Result<String, String> {
        LlmClient::chat(self, m, system, user, temperature).await
    }
    async fn complete(
        &self,
        m: &ResolvedModel,
        messages: Value,
        temperature: f64,
    ) -> Result<String, String> {
        LlmClient::complete(self, m, messages, temperature).await
    }
}
