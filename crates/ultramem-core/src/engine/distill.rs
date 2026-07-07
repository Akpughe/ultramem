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
When a fact describes something that happened (or is scheduled) on a specific date — stated \
explicitly OR relatively ('yesterday', 'last Sunday', 'two weeks ago', 'next Friday') — resolve \
it against the Conversation date given below and PREFIX that fact with the absolute event date as \
\"[on YYYY-MM-DD] \" (e.g. \"[on 2023-05-20] The user visited the Museum of Modern Art\"). This \
anchors temporal reasoning; use it whenever a fact has a 'when'. \
Separately, if (and only if) a fact stops being true after a specific calendar date — a deadline, \
an appointment, a 'tomorrow'/'next week' item — append \" [until YYYY-MM-DD]\" to that fact string. \
Respond with ONLY a JSON array of strings.";

const MERGE_SYSTEM: &str = "You are given candidate facts extracted from different parts of the \
same document. Merge near-duplicates into a single best phrasing, drop generic or boilerplate \
facts, and keep every genuinely distinct fact — do not summarize distinct facts away. \
Preserve any leading \"[on YYYY-MM-DD] \" event-date prefix and any trailing \" [until YYYY-MM-DD]\" \
expiry suffix on the facts that have one. \
Respond with ONLY a JSON array of strings.";

/// Extract memories from a whole document. Segment → extract per segment →
/// merge/dedup across segments. Returns an empty vec when the model
/// (correctly) finds nothing memorable.
pub async fn distill_facts(
    llm: &dyn Llm,
    model: &ResolvedModel,
    title: &str,
    content: &str,
    doc_date: &str,
) -> Result<Vec<String>, String> {
    if !model.is_ready() {
        return Err("no model configured for distillation".into());
    }
    let segments = super::chunker::chunk_text(content, SEGMENT_CHARS, 0);
    if segments.is_empty() {
        return Ok(vec![]);
    }

    // The conversation date anchors relative-date resolution ("last Sunday").
    let date_line = if doc_date.is_empty() {
        String::new()
    } else {
        format!("Conversation date: {doc_date}\n")
    };
    // SS-5: the document body is untrusted; wrap it and tell the model so.
    let system = format!("{EXTRACT_SYSTEM}{}", super::promptguard::UNTRUSTED_NOTE);
    let mut all: Vec<String> = Vec::new();
    let total = segments.len();
    for (i, seg) in segments.iter().enumerate() {
        let user = format!(
            "{date_line}Title: {title}\nPart {} of {total}\n\n{}",
            i + 1,
            super::promptguard::wrap_untrusted(seg)
        );
        match llm.chat(model, &system, &user, 0.3).await {
            Ok(raw) => match parse_facts(&raw, MAX_FACTS_PER_SEGMENT) {
                Some(facts) => all.extend(facts),
                None => eprintln!(
                    "[recally] unparseable distill output for '{title}' part {}",
                    i + 1
                ),
            },
            // First segment failing means the doc got no extraction at all —
            // surface it so the caller can log. Later segments failing still
            // leaves partial coverage, which beats none.
            Err(e) if i == 0 => return Err(e),
            Err(e) => {
                eprintln!(
                    "[recally] distill part {}/{total} failed for '{title}': {e}",
                    i + 1
                );
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

/// Dig the facts out of model output. Robust to format drift across providers:
/// accepts a JSON array of strings OR of objects (e.g. `[{"fact": "..."}]`), and
/// if the JSON is malformed, falls back to harvesting quoted strings — so a
/// stray format never silently drops an entire segment's facts.
pub fn parse_facts(raw: &str, cap: usize) -> Option<Vec<String>> {
    let finalize = |v: Vec<String>| -> Vec<String> {
        v.into_iter()
            .map(|f| f.trim().trim_matches('"').trim().to_string())
            .filter(|f| f.len() >= 8)
            .take(cap)
            .collect()
    };

    // 1. Proper JSON array span — parse as generic values so an array of strings
    //    OR of objects both work. A valid-but-empty array means "nothing
    //    memorable" (Some(empty)), distinct from unparseable output (None).
    if let (Some(start), Some(end)) = (raw.find('['), raw.rfind(']')) {
        if end > start {
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&raw[start..=end]) {
                let facts: Vec<String> = arr
                    .into_iter()
                    .filter_map(|v| match v {
                        serde_json::Value::String(s) => Some(s),
                        // {"fact": "..."} / {"text": "..."} / first string field.
                        serde_json::Value::Object(o) => o
                            .get("fact")
                            .or_else(|| o.get("text"))
                            .and_then(|x| x.as_str())
                            .map(str::to_string)
                            .or_else(|| o.values().find_map(|x| x.as_str().map(str::to_string))),
                        _ => None,
                    })
                    .collect();
                return Some(finalize(facts));
            }
        }
    }

    // 2. Fallback: harvest quoted strings from malformed/partial JSON (e.g. an
    //    unterminated array, or trailing commas the parser rejects). Only counts
    //    as a parse if it actually yields facts — otherwise None (unparseable).
    let mut harvested = Vec::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            harvested.push(s);
        }
    }
    let out = finalize(harvested);
    (!out.is_empty()).then_some(out)
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
        let facts = parse_facts(
            r#"["User is building a Tauri app", "The deadline is June 20"]"#,
            10,
        )
        .unwrap();
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
    fn parses_array_of_objects() {
        // Format drift: some models return objects, not bare strings.
        let raw =
            r#"[{"fact": "User builds a Tauri app daily"}, {"text": "Deadline is June 20th"}]"#;
        let facts = parse_facts(raw, 10).unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0], "User builds a Tauri app daily");
    }

    #[test]
    fn harvests_from_malformed_json() {
        // Unterminated array (cut off / trailing comma) — fall back to quoted strings.
        let raw = r#"["User prefers Rust over Go", "User lives in Cape Town","#;
        let facts = parse_facts(raw, 10).unwrap();
        assert_eq!(facts.len(), 2);
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
            let facts = distill_facts(&llm, &model, "Team meeting notes", &content, "2024-01-15")
                .await
                .expect("distill_facts");
            eprintln!("extracted {} facts: {facts:#?}", facts.len());
            let joined = facts.join(" | ").to_lowercase();
            assert!(joined.contains("zephyr-quartz-99"), "fact from the END segment missing");
            assert!(joined.contains("qdrant"), "fact from the first segment missing");
        });
    }
}
