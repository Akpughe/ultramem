//! The standing user profile — SuperMemory's "always-known context" trick.
//! Instead of retrieving from scratch every turn, a compiled profile is
//! prepended to every answer so the assistant starts already knowing the user.
//! Two sections:
//!
//!   • static  — durable facts that are basically always true (who they are,
//!     what they work on, standing preferences). Compiled from the latest
//!     memories.
//!   • dynamic — what they've been doing lately (last ~7 days of episodes).
//!
//! Compilation is an LLM pass over the memory graph; it's cached (see
//! `MemoryEngine::profile`) so it costs nothing at query time.

use serde::{Deserialize, Serialize};

use super::{EngineCfg, DEFAULT_TAG};
use crate::providers::{Llm, VectorStore};

/// Whether a fact payload belongs to the given namespace. The default tag also
/// claims legacy points that have no `container_tag` field.
fn in_tag(payload: &serde_json::Value, tag: &str) -> bool {
    match payload.get("container_tag").and_then(|v| v.as_str()) {
        Some(t) => t == tag,
        None => tag == DEFAULT_TAG,
    }
}

/// How many latest memories to feed the profile compiler. A sample, not the
/// whole graph — enough to characterise the user without an unbounded prompt.
const SAMPLE: usize = 600;
const RECENT_WINDOW_SECS: i64 = 7 * 86_400;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    pub static_text: String,
    pub dynamic_text: String,
}

impl Profile {
    pub fn is_empty(&self) -> bool {
        self.static_text.trim().is_empty() && self.dynamic_text.trim().is_empty()
    }

    /// Render for injection into a system prompt. Empty string when there's
    /// nothing to say, so callers can prepend unconditionally.
    pub fn as_prompt_block(&self) -> String {
        let mut s = String::new();
        if !self.static_text.trim().is_empty() {
            s.push_str("What you always know about the user:\n");
            s.push_str(self.static_text.trim());
            s.push_str("\n\n");
        }
        if !self.dynamic_text.trim().is_empty() {
            s.push_str("What the user has been doing recently:\n");
            s.push_str(self.dynamic_text.trim());
            s.push_str("\n\n");
        }
        s
    }
}

const STATIC_SYSTEM: &str =
    "You compile a durable profile of a user from facts extracted from their \
files, messages, and meetings. Keep only facts that are basically always true: who they are, their \
role, the projects and products they work on, the people and companies around them, and standing \
preferences. Drop one-off events, transient tasks, and anything dated. Write 4-10 terse bullet \
points (each on its own line starting with '- '). No preamble.";

const DYNAMIC_SYSTEM: &str =
    "You summarize what a user has been doing recently from facts extracted \
in the last week. Focus on active work, recent decisions, and current threads. Write 3-6 terse \
bullet points (each on its own line starting with '- '). No preamble.";

/// Compile the profile for one namespace from its latest memories. Either
/// section may come back empty (no facts yet, or the model declined) — that's
/// fine.
pub async fn compile(
    store: &dyn VectorStore,
    llm: &dyn Llm,
    cfg: &EngineCfg,
    tag: &str,
) -> Result<Profile, String> {
    // Scroll a wider sample than we keep — points from other namespaces are
    // filtered out in Rust, so over-fetch to still get enough of ours.
    let points = store.scroll(&cfg.facts_collection, SAMPLE * 4).await?;
    let now = chrono::Utc::now().timestamp();

    let mut durable: Vec<String> = Vec::new();
    let mut recent: Vec<(i64, String)> = Vec::new();
    for p in &points {
        let pl = &p["payload"];
        if !in_tag(pl, tag) {
            continue; // another namespace's memory
        }
        if pl["is_latest"].as_bool() == Some(false) {
            continue; // superseded — not part of the current profile
        }
        if durable.len() >= SAMPLE {
            break;
        }
        let Some(fact) = pl["fact"].as_str() else {
            continue;
        };
        let ts = pl["captured_at"].as_i64().unwrap_or(0);
        durable.push(fact.to_string());
        if now - ts <= RECENT_WINDOW_SECS {
            recent.push((ts, fact.to_string()));
        }
    }

    let model = &cfg.distill_model;
    if !model.is_ready() || durable.is_empty() {
        return Ok(Profile::default());
    }

    // Static section from durable facts (cap the prompt).
    let durable_input = durable
        .iter()
        .take(250)
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    // SS-5: facts are derived from untrusted content; the profile feeds a
    // downstream system prompt, so it must never launder an injected instruction.
    let static_system = format!("{STATIC_SYSTEM}{}", super::promptguard::DERIVED_NOTE);
    let static_text = llm
        .chat(model, &static_system, &durable_input, 0.2)
        .await
        .unwrap_or_default();

    // Dynamic section from recent facts, newest first.
    recent.sort_by_key(|x| std::cmp::Reverse(x.0));
    let dynamic_text = if recent.is_empty() {
        String::new()
    } else {
        let recent_input = recent
            .iter()
            .take(80)
            .map(|(_, f)| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        let dynamic_system = format!("{DYNAMIC_SYSTEM}{}", super::promptguard::DERIVED_NOTE);
        llm.chat(model, &dynamic_system, &recent_input, 0.2)
            .await
            .unwrap_or_default()
    };

    Ok(Profile {
        static_text: static_text.trim().to_string(),
        dynamic_text: dynamic_text.trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_block_omits_empty_sections() {
        let p = Profile {
            static_text: "- works on Recally".into(),
            dynamic_text: String::new(),
        };
        let block = p.as_prompt_block();
        assert!(block.contains("always know"));
        assert!(!block.contains("recently"));
    }

    #[test]
    fn empty_profile_renders_nothing() {
        assert_eq!(Profile::default().as_prompt_block(), "");
        assert!(Profile::default().is_empty());
    }

    #[test]
    fn in_tag_matches_and_claims_legacy() {
        use serde_json::json;
        // Exact tag match.
        assert!(in_tag(
            &json!({ "container_tag": "tenant_42" }),
            "tenant_42"
        ));
        assert!(!in_tag(
            &json!({ "container_tag": "tenant_42" }),
            DEFAULT_TAG
        ));
        // Legacy point (no field) belongs to the default namespace only.
        assert!(in_tag(&json!({ "fact": "x" }), DEFAULT_TAG));
        assert!(!in_tag(&json!({ "fact": "x" }), "tenant_42"));
    }
}
