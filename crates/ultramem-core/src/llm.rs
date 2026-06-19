//! Provider-agnostic LLM client. Speaks the OpenAI Chat Completions shape
//! (Groq, OpenAI, Google's OpenAI-compatible endpoint, Ollama, OpenRouter, …)
//! and Anthropic's Messages API. A `ResolvedModel` is one concrete call target
//! — endpoint + key + model name — that the engine and commands hand to the
//! client. Which model fills which role (answering, planning, distilling,
//! transcribing, agent) is decided in settings; this layer just executes.

use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

pub const GROQ_BASE: &str = "https://api.groq.com/openai/v1";
pub const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

// Process-wide LLM token accounting (every `complete` call adds its usage here),
// so a harness can report measured tokens/cost across the engine + its own calls.
static PROMPT_TOKENS: AtomicU64 = AtomicU64::new(0);
static COMPLETION_TOKENS: AtomicU64 = AtomicU64::new(0);

/// (prompt_tokens, completion_tokens, total) accumulated since process start.
pub fn token_usage() -> (u64, u64, u64) {
    let p = PROMPT_TOKENS.load(Ordering::Relaxed);
    let c = COMPLETION_TOKENS.load(Ordering::Relaxed);
    (p, c, p + c)
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    /// OpenAI Chat Completions wire format (the de-facto standard).
    #[default]
    #[serde(alias = "openai", alias = "openai_compat", alias = "openaicompat")]
    OpenaiCompat,
    /// Anthropic Messages API.
    Anthropic,
    /// Google Gemini native API (`x-goog-api-key` auth, `contents`/`parts` shape).
    /// The OpenAI-compat layer rejects the newer `AQ.`-format keys, so we speak
    /// the native protocol.
    Gemini,
}

/// A fully-resolved model call target.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Gemini "thinking" budget: `None` = provider default, `Some(0)` = off,
    /// `Some(-1)` = dynamic (model decides), `Some(n)` = fixed token budget.
    /// Ignored by non-Gemini providers. Thinking tokens are billed, so turn it
    /// off for mechanical work (distillation) and on for reasoning answers.
    pub thinking_budget: Option<i32>,
}

impl ResolvedModel {
    pub fn groq(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            kind: ProviderKind::OpenaiCompat,
            base_url: GROQ_BASE.into(),
            api_key: api_key.into(),
            model: model.into(),
            thinking_budget: None,
        }
    }

    /// Google Gemini native API (e.g. model `gemini-2.5-flash`).
    pub fn gemini(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            kind: ProviderKind::Gemini,
            base_url: GEMINI_BASE.into(),
            api_key: api_key.into(),
            model: model.into(),
            thinking_budget: None,
        }
    }

    /// Set the Gemini thinking budget (`0` off, `-1` dynamic, `n` fixed). No-op
    /// for other providers.
    pub fn with_thinking(mut self, budget: i32) -> Self {
        self.thinking_budget = Some(budget);
        self
    }

    /// Local providers (Ollama) need no key; everyone else does.
    pub fn is_local(&self) -> bool {
        self.base_url.contains("localhost") || self.base_url.contains("127.0.0.1")
    }

    /// Usable for a call: has a key, or is a keyless local endpoint.
    pub fn is_ready(&self) -> bool {
        !self.api_key.is_empty() || self.is_local()
    }
}

impl Default for ResolvedModel {
    fn default() -> Self {
        Self::groq(String::new(), "openai/gpt-oss-120b")
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// One step of a tool-calling conversation: either the model produced its final
/// message, or it wants to call one or more tools first.
#[derive(Debug, Clone)]
pub enum ChatStep {
    Message(String),
    ToolCalls(Vec<ToolCall>),
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments string as emitted by the model.
    pub arguments: String,
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
}

impl Default for LlmClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// Non-streaming completion from a system+user pair.
    pub async fn chat(
        &self,
        m: &ResolvedModel,
        system: &str,
        user: &str,
        temperature: f64,
    ) -> Result<String, String> {
        let messages = json!([
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ]);
        self.complete(m, messages, temperature).await
    }

    /// Non-streaming completion from OpenAI-shaped messages (system first).
    /// Completion with retry: transient failures (network/timeout/429/5xx) are
    /// retried with exponential backoff. Hosted LLM/embedding APIs blip under
    /// load, and a dropped call silently loses a memory at ingest time.
    pub async fn complete(
        &self,
        m: &ResolvedModel,
        messages: Value,
        temperature: f64,
    ) -> Result<String, String> {
        let mut delay_ms = 400u64;
        let mut last = String::new();
        for attempt in 0..4 {
            match self.complete_once(m, messages.clone(), temperature).await {
                Ok(v) => return Ok(v),
                Err(e) if is_transient(&e) && attempt < 3 => {
                    last = e;
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    delay_ms *= 2;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    async fn complete_once(
        &self,
        m: &ResolvedModel,
        messages: Value,
        temperature: f64,
    ) -> Result<String, String> {
        match m.kind {
            ProviderKind::OpenaiCompat => {
                let resp = self
                    .http
                    .post(format!("{}/chat/completions", m.base_url.trim_end_matches('/')))
                    .bearer_auth(&m.api_key)
                    .json(&json!({"model": m.model, "messages": messages, "temperature": temperature}))
                    .send()
                    .await
                    .map_err(|e| format!("{} unreachable: {e}", m.model))?;
                let status = resp.status();
                let v: Value = resp.json().await.map_err(|e| e.to_string())?;
                if !status.is_success() {
                    return Err(format!(
                        "LLM error {status}: {}",
                        v["error"]["message"].as_str().unwrap_or("unknown")
                    ));
                }
                record_usage(
                    v["usage"]["prompt_tokens"].as_u64(),
                    v["usage"]["completion_tokens"].as_u64(),
                );
                Ok(v["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string())
            }
            ProviderKind::Anthropic => {
                let (system, msgs) = split_system(&messages);
                let resp = self
                    .http
                    .post(format!("{}/v1/messages", m.base_url.trim_end_matches('/')))
                    .header("x-api-key", &m.api_key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&json!({
                        "model": m.model,
                        "max_tokens": 4096,
                        "system": system,
                        "messages": msgs,
                        "temperature": temperature,
                    }))
                    .send()
                    .await
                    .map_err(|e| format!("anthropic unreachable: {e}"))?;
                let status = resp.status();
                let v: Value = resp.json().await.map_err(|e| e.to_string())?;
                if !status.is_success() {
                    return Err(format!(
                        "anthropic error {status}: {}",
                        v["error"]["message"].as_str().unwrap_or("unknown")
                    ));
                }
                record_usage(
                    v["usage"]["input_tokens"].as_u64(),
                    v["usage"]["output_tokens"].as_u64(),
                );
                Ok(v["content"]
                    .as_array()
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default())
            }
            ProviderKind::Gemini => {
                // Native Gemini: x-goog-api-key auth, contents/parts shape.
                let (system, msgs) = split_system(&messages);
                let contents: Vec<Value> = msgs
                    .iter()
                    .map(|msg| {
                        let role = if msg["role"].as_str() == Some("assistant") { "model" } else { "user" };
                        json!({"role": role, "parts": [{"text": msg["content"].as_str().unwrap_or_default()}]})
                    })
                    .collect();
                let mut body = json!({
                    "contents": contents,
                    "generationConfig": {"temperature": temperature},
                });
                if let Some(budget) = m.thinking_budget {
                    body["generationConfig"]["thinkingConfig"] = json!({"thinkingBudget": budget});
                }
                if !system.is_empty() {
                    body["systemInstruction"] = json!({"parts": [{"text": system}]});
                }
                let resp = self
                    .http
                    .post(format!(
                        "{}/models/{}:generateContent",
                        m.base_url.trim_end_matches('/'),
                        m.model
                    ))
                    .header("x-goog-api-key", &m.api_key)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| format!("gemini unreachable: {e}"))?;
                let status = resp.status();
                let v: Value = resp.json().await.map_err(|e| e.to_string())?;
                if !status.is_success() {
                    return Err(format!(
                        "gemini error {status}: {}",
                        v["error"]["message"].as_str().unwrap_or("unknown")
                    ));
                }
                // `total - prompt` captures visible output PLUS billed "thinking"
                // tokens (which candidatesTokenCount alone omits).
                let prompt = v["usageMetadata"]["promptTokenCount"].as_u64();
                let total = v["usageMetadata"]["totalTokenCount"].as_u64();
                record_usage(prompt, total.zip(prompt).map(|(t, p)| t.saturating_sub(p)));
                Ok(v["candidates"][0]["content"]["parts"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default())
            }
        }
    }

    /// One tool-calling step. `messages` is OpenAI-shaped; `tools` is the
    /// OpenAI `tools` array. Returns the assistant message to append verbatim
    /// to the conversation, plus the parsed step (final text or tool calls).
    /// OpenAI-compatible providers only; Anthropic falls back to a plain
    /// (toolless) completion so it still answers.
    pub async fn chat_with_tools(
        &self,
        m: &ResolvedModel,
        messages: Value,
        tools: Value,
        temperature: f64,
    ) -> Result<(Value, ChatStep), String> {
        if m.kind != ProviderKind::OpenaiCompat {
            let text = self.complete(m, messages, temperature).await?;
            return Ok((
                json!({"role": "assistant", "content": text.clone()}),
                ChatStep::Message(text),
            ));
        }
        let resp = self
            .http
            .post(format!(
                "{}/chat/completions",
                m.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&m.api_key)
            .json(&json!({
                "model": m.model,
                "messages": messages,
                "tools": tools,
                "tool_choice": "auto",
                "temperature": temperature,
            }))
            .send()
            .await
            .map_err(|e| format!("{} unreachable: {e}", m.model))?;
        let status = resp.status();
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "LLM error {status}: {}",
                v["error"]["message"].as_str().unwrap_or("unknown")
            ));
        }
        let msg = v["choices"][0]["message"].clone();
        let tool_calls = msg["tool_calls"].as_array().cloned().unwrap_or_default();
        if tool_calls.is_empty() {
            let text = msg["content"].as_str().unwrap_or_default().to_string();
            Ok((msg, ChatStep::Message(text)))
        } else {
            let calls = tool_calls
                .iter()
                .map(|tc| ToolCall {
                    id: tc["id"].as_str().unwrap_or_default().to_string(),
                    name: tc["function"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    arguments: tc["function"]["arguments"]
                        .as_str()
                        .unwrap_or("{}")
                        .to_string(),
                })
                .collect();
            Ok((msg, ChatStep::ToolCalls(calls)))
        }
    }

    /// Streaming completion; calls `on_token` for each delta. Returns the full
    /// text. `messages` is OpenAI-shaped (system message first); Anthropic gets
    /// it translated transparently.
    pub async fn stream(
        &self,
        m: &ResolvedModel,
        messages: Value,
        temperature: f64,
        on_token: impl Fn(&str),
    ) -> Result<String, String> {
        match m.kind {
            ProviderKind::OpenaiCompat => {
                self.stream_openai(m, messages, temperature, on_token).await
            }
            ProviderKind::Anthropic => {
                self.stream_anthropic(m, messages, temperature, on_token)
                    .await
            }
            ProviderKind::Gemini => {
                // Native Gemini streaming differs; fall back to one non-streamed call.
                let text = self.complete(m, messages, temperature).await?;
                on_token(&text);
                Ok(text)
            }
        }
    }

    async fn stream_openai(
        &self,
        m: &ResolvedModel,
        messages: Value,
        temperature: f64,
        on_token: impl Fn(&str),
    ) -> Result<String, String> {
        let resp = self
            .http
            .post(format!(
                "{}/chat/completions",
                m.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&m.api_key)
            .json(&json!({
                "model": m.model,
                "messages": messages,
                "temperature": temperature,
                "stream": true,
            }))
            .send()
            .await
            .map_err(|e| format!("{} unreachable: {e}", m.model))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("LLM error {status}: {body}"));
        }
        let mut full = String::new();
        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };
                if data == "[DONE]" {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if let Some(tok) = v["choices"][0]["delta"]["content"].as_str() {
                        full.push_str(tok);
                        on_token(tok);
                    }
                }
            }
        }
        Ok(full)
    }

    async fn stream_anthropic(
        &self,
        m: &ResolvedModel,
        messages: Value,
        temperature: f64,
        on_token: impl Fn(&str),
    ) -> Result<String, String> {
        let (system, msgs) = split_system(&messages);
        let resp = self
            .http
            .post(format!("{}/v1/messages", m.base_url.trim_end_matches('/')))
            .header("x-api-key", &m.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": m.model,
                "max_tokens": 4096,
                "system": system,
                "messages": msgs,
                "temperature": temperature,
                "stream": true,
            }))
            .send()
            .await
            .map_err(|e| format!("anthropic unreachable: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("anthropic error {status}: {body}"));
        }
        let mut full = String::new();
        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if v["type"] == "content_block_delta" {
                        if let Some(tok) = v["delta"]["text"].as_str() {
                            full.push_str(tok);
                            on_token(tok);
                        }
                    }
                }
            }
        }
        Ok(full)
    }

    /// Transcribe an in-memory clip → plain text. OpenAI-compatible providers
    /// only (Groq, OpenAI); Anthropic/Gemini have no Whisper endpoint here.
    pub async fn transcribe_bytes(
        &self,
        m: &ResolvedModel,
        bytes: Vec<u8>,
        filename: &str,
        mime: &str,
    ) -> Result<String, String> {
        if m.kind != ProviderKind::OpenaiCompat {
            return Err(
                "transcription needs an OpenAI-compatible provider (Groq or OpenAI)".into(),
            );
        }
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(mime)
            .map_err(|e| e.to_string())?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", m.model.clone())
            .text("response_format", "json");
        let resp = self
            .http
            .post(format!(
                "{}/audio/transcriptions",
                m.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&m.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("transcription unreachable: {e}"))?;
        let status = resp.status();
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "transcription error {status}: {}",
                v["error"]["message"].as_str().unwrap_or("unknown")
            ));
        }
        Ok(v["text"].as_str().unwrap_or_default().trim().to_string())
    }

    /// Transcribe an audio file → timestamped segments (meetings pipeline).
    pub async fn transcribe(
        &self,
        m: &ResolvedModel,
        audio_path: &std::path::Path,
    ) -> Result<Vec<TranscriptSegment>, String> {
        if m.kind != ProviderKind::OpenaiCompat {
            return Err(
                "transcription needs an OpenAI-compatible provider (Groq or OpenAI)".into(),
            );
        }
        let bytes = tokio::fs::read(audio_path)
            .await
            .map_err(|e| format!("read audio: {e}"))?;
        let filename = audio_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "audio.m4a".into());
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str("audio/mp4")
            .map_err(|e| e.to_string())?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", m.model.clone())
            .text("response_format", "verbose_json");
        let resp = self
            .http
            .post(format!(
                "{}/audio/transcriptions",
                m.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&m.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("transcription unreachable: {e}"))?;
        let status = resp.status();
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "transcription error {status}: {}",
                v["error"]["message"].as_str().unwrap_or("unknown")
            ));
        }
        let segments = v["segments"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|s| TranscriptSegment {
                        start: s["start"].as_f64().unwrap_or(0.0),
                        end: s["end"].as_f64().unwrap_or(0.0),
                        text: s["text"].as_str().unwrap_or("").trim().to_string(),
                    })
                    .filter(|s| !s.text.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(segments)
    }
}

/// Add one call's token usage to the process-wide counters (best-effort —
/// providers that omit usage just contribute nothing).
fn record_usage(prompt: Option<u64>, completion: Option<u64>) {
    if let Some(p) = prompt {
        PROMPT_TOKENS.fetch_add(p, Ordering::Relaxed);
    }
    if let Some(c) = completion {
        COMPLETION_TOKENS.fetch_add(c, Ordering::Relaxed);
    }
}

/// Whether an error string looks like a transient failure worth retrying
/// (network/timeout/rate-limit/5xx) rather than a permanent one (4xx, parse).
pub(crate) fn is_transient(e: &str) -> bool {
    let e = e.to_lowercase();
    e.contains("unreachable")
        || e.contains("error sending request")
        || e.contains("timed out")
        || e.contains("timeout")
        || e.contains("connection")
        || e.contains(" 429")
        || e.contains(" 500")
        || e.contains(" 502")
        || e.contains(" 503")
        || e.contains(" 504")
}

/// Pull system messages out of an OpenAI-shaped array into Anthropic's
/// top-level `system` string, leaving user/assistant turns as the message list.
fn split_system(messages: &Value) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut rest = Vec::new();
    if let Some(arr) = messages.as_array() {
        for msg in arr {
            let role = msg["role"].as_str().unwrap_or("user");
            if role == "system" {
                if let Some(c) = msg["content"].as_str() {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(c);
                }
            } else {
                rest.push(json!({"role": role, "content": msg["content"].clone()}));
            }
        }
    }
    (system, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_system_separates_system_from_turns() {
        let messages = json!([
            {"role": "system", "content": "You are helpful"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
            {"role": "user", "content": "bye"},
        ]);
        let (system, rest) = split_system(&messages);
        assert_eq!(system, "You are helpful");
        assert_eq!(rest.len(), 3);
        assert_eq!(rest[0]["role"], "user");
    }

    #[test]
    fn resolved_model_readiness() {
        assert!(ResolvedModel::groq("key", "m").is_ready());
        assert!(!ResolvedModel::groq("", "m").is_ready());
        let ollama = ResolvedModel {
            kind: ProviderKind::OpenaiCompat,
            base_url: "http://localhost:11434/v1".into(),
            api_key: String::new(),
            model: "llama3.1".into(),
            thinking_budget: None,
        };
        assert!(ollama.is_ready(), "local endpoints need no key");
    }
}
