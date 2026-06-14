//! Fact distillation, supermemory-style: extraction runs over the WHOLE
//! document, not a fixed-size head. The document is split into ~6k-char
//! segments, each segment yields the facts it genuinely supports (zero for
//! noise, many for dense content), and a final merge pass dedups across
//! segments. The number of memories scales with the content — a long meeting
//! produces many, a browser-visit record produces one or none.
//!
//! Failures are non-fatal to ingest — chunks are already indexed by the time
//! this runs.

use crate::llm::ResolvedModel;
use crate::providers::Llm;

/// Each extraction pass reads one segment of this size (paragraph-aware).
const SEGMENT_CHARS: usize = 6000;
/// Per-segment ceiling — forces selectivity within a passage without
/// capping what a long document can yield overall.
const MAX_FACTS_PER_SEGMENT: usize = 10;
/// Sanity ceiling across a whole document (10 segments × 10 facts would be
/// pathological extraction, not memory).
const MAX_TOTAL_FACTS: usize = 50;

const EXTRACT_SYSTEM: &str = "You extract memories from content captured on a user's computer \
(their files, clipboard, browsing, and meetings). Extract every distinct fact worth remembering \
about the user, their work, projects, people, decisions, preferences, and plans. Each fact must \
stand alone without any surrounding context, e.g. \"The Q3 roadmap prioritizes the mobile app \
redesign\". Extract as many facts as the content genuinely supports — dense content may yield \
many, and boilerplate, navigation text, or generic content with nothing personal or \
project-specific yields none. When nothing is worth remembering, return []. \
If (and only if) a fact stops being true after a specific calendar date — a deadline, an \
appointment, a 'tomorrow'/'next week' item — append \" [until YYYY-MM-DD]\" to that fact string \
using the absolute date. Do not add it to durable facts. \
Respond with ONLY a JSON array of strings.";

const MERGE_SYSTEM: &str = "You are given candidate facts extracted from different parts of the \
same document. Merge near-duplicates into a single best phrasing, drop generic or boilerplate \
facts, and keep every genuinely distinct fact — do not summarize distinct facts away. \
Preserve any trailing \" [until YYYY-MM-DD]\" expiry suffix on the facts that have one. \
Respond with ONLY a JSON array of strings.";

/// Extract memories from a whole document. Segment → extract per segment →
/// merge/dedup across segments. Returns an empty vec when the model
/// (correctly) finds nothing memorable.
pub async fn distill_facts(
    llm: &dyn Llm,
    model: &ResolvedModel,
    title: &str,
    content: &str,
) -> Result<Vec<String>, String> {
    if !model.is_ready() {
        return Err("no model configured for distillation".into());
    }
    let segments = super::chunker::chunk_text(content, SEGMENT_CHARS, 0);
    if segments.is_empty() {
        return Ok(vec![]);
    }

    let mut all: Vec<String> = Vec::new();
    let total = segments.len();
    for (i, seg) in segments.iter().enumerate() {
        let user = format!("Title: {title}\nPart {} of {total}\n\n{seg}", i + 1);
        match llm.chat(model, EXTRACT_SYSTEM, &user, 0.3).await {
            Ok(raw) => match parse_facts(&raw, MAX_FACTS_PER_SEGMENT) {
                Some(facts) => all.extend(facts),
                None => eprintln!("[recally] unparseable distill output for '{title}' part {}", i + 1),
            },
            // First segment failing means the doc got no extraction at all —
            // surface it so the caller can log. Later segments failing still
            // leaves partial coverage, which beats none.
            Err(e) if i == 0 => return Err(e),
            Err(e) => {
                eprintln!("[recally] distill part {}/{total} failed for '{title}': {e}", i + 1);
                break; // likely rate-limited; keep what we have
            }
        }
    }

    if all.is_empty() || total == 1 {
        all.truncate(MAX_TOTAL_FACTS);
        return Ok(all);
    }

    // Cross-segment merge: dedup near-duplicates, drop weak ones. On failure
    // fall back to a local exact-ish dedup — partial quality beats losing the
    // extraction entirely.
    let listing = all
        .iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    match llm.chat(model, MERGE_SYSTEM, &listing, 0.3).await {
        Ok(raw) => {
            if let Some(merged) = parse_facts(&raw, MAX_TOTAL_FACTS) {
                if !merged.is_empty() {
                    return Ok(merged);
                }
            }
            Ok(local_dedup(all, MAX_TOTAL_FACTS))
        }
        Err(e) => {
            eprintln!("[recally] fact merge failed for '{title}': {e}");
            Ok(local_dedup(all, MAX_TOTAL_FACTS))
        }
    }
}

/// Dig a JSON string array out of model output that may be wrapped in prose
/// or code fences.
pub fn parse_facts(raw: &str, cap: usize) -> Option<Vec<String>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end < start {
        return None;
    }
    let arr: Vec<String> = serde_json::from_str(&raw[start..=end]).ok()?;
    Some(
        arr.into_iter()
            .map(|f| f.trim().to_string())
            .filter(|f| f.len() >= 8)
            .take(cap)
            .collect(),
    )
}

/// Case-insensitive exact dedup, preserving first occurrence order.
fn local_dedup(facts: Vec<String>, cap: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    facts
        .into_iter()
        .filter(|f| seen.insert(f.trim().to_lowercase()))
        .take(cap)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_array() {
        let facts = parse_facts(r#"["User is building a Tauri app", "The deadline is June 20"]"#, 10).unwrap();
        assert_eq!(facts.len(), 2);
    }

    #[test]
    fn parses_fenced_array() {
        let raw = "Here are the facts:\n```json\n[\"Recally uses Qdrant for vectors\"]\n```";
        let facts = parse_facts(raw, 10).unwrap();
        assert_eq!(facts, vec!["Recally uses Qdrant for vectors"]);
    }

    #[test]
    fn empty_array_is_ok() {
        assert_eq!(parse_facts("[]", 10).unwrap().len(), 0);
    }

    #[test]
    fn garbage_is_none() {
        assert!(parse_facts("I could not find any facts.", 10).is_none());
    }

    #[test]
    fn caps_and_drops_tiny_fragments() {
        let raw = r#"["a", "fact number one here", "fact number two here", "fact three is here"]"#;
        let facts = parse_facts(raw, 2).unwrap();
        assert_eq!(facts.len(), 2);
        assert!(!facts.contains(&"a".to_string()));
    }

    #[test]
    fn local_dedup_is_case_insensitive_and_ordered() {
        let facts = vec![
            "The launch is June 20".to_string(),
            "the launch is june 20".to_string(),
            "Davak leads the project".to_string(),
        ];
        let out = local_dedup(facts, 50);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "The launch is June 20");
    }
}

/// Live distillation test against Groq. Run with:
///   ULTRAMEM_PIPELINE_TESTS=1 cargo test --lib engine::distill::live_tests -- --nocapture
#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    fn extracts_facts_from_the_end_of_a_long_document() {
        if std::env::var("ULTRAMEM_PIPELINE_TESTS").as_deref() != Ok("1") {
            eprintln!("skipped (set ULTRAMEM_PIPELINE_TESTS=1 to run)");
            return;
        }
        let _ = dotenvy::dotenv();
        let _ = dotenvy::from_filename("../.env");
        let key = std::env::var("GROQ_API_KEY").expect("GROQ_API_KEY missing");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // ~14k chars → 3 segments. Distinctive facts in the FIRST and
            // LAST segments; filler in between. The old head-only design
            // would miss the last one.
            let filler = "The weather report mentioned scattered clouds over the bay area today. \
                          General observations about the office continued without anything notable. "
                .repeat(80); // ~12.5k chars
            let content = format!(
                "Meeting notes. Decision: the team chose Qdrant as the vector database for Recally.\n\n\
                 {filler}\n\n\
                 Final agenda item: the launch codename was set to Zephyr-Quartz-99 and Davak owns the rollout."
            );
            let llm = crate::llm::LlmClient::new();
            let model = crate::llm::ResolvedModel::groq(&key, "openai/gpt-oss-120b");
            let facts = distill_facts(&llm, &model, "Team meeting notes", &content)
                .await
                .expect("distill_facts");
            eprintln!("extracted {} facts: {facts:#?}", facts.len());
            let joined = facts.join(" | ").to_lowercase();
            assert!(joined.contains("zephyr-quartz-99"), "fact from the END segment missing");
            assert!(joined.contains("qdrant"), "fact from the first segment missing");
        });
    }
}
