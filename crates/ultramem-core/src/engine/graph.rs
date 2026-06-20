//! Tier-3: a bi-temporal entity-attribute knowledge graph layered on the fact
//! store. Where `memory.rs` reconciles whole facts (is_latest/supersedes), this
//! layer extracts typed `(subject, predicate, object)` edges and stamps each
//! with **event time** — when the fact was true in the world — so "what is the
//! latest value?" becomes a deterministic query instead of an LLM guess.
//!
//! Two time axes (the Zep/Graphiti model):
//!   • event time   — `valid_from` / `valid_to`: when the edge held in the world.
//!   • transaction  — `captured_at` (the source doc) / `is_latest`: when learned.
//!
//! The keystone the old fact layer lacked: `[on YYYY-MM-DD]` lived only in fact
//! TEXT, never a queryable field, so facts could not be ordered by when-true.
//! That is exactly why knowledge-update's "use the latest value" failed (it
//! picked Hawaii over Paris). Here event time is structured and the resolution
//! is done in Rust.
//!
//! A `singular` edge is a single-valued STATE (Starbucks gold threshold, the
//! engineer headcount, a 5K personal best) — a newer value supersedes the old.
//! A non-`singular` edge is an EVENT that accumulates (each family trip); "most
//! recent" is then the max `valid_from`, and nothing is superseded.

use crate::llm::ResolvedModel;
use crate::providers::Llm;

/// One extracted edge with event-time validity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// Canonical entity the edge is about (usually "user", else a named person).
    pub subject: String,
    /// Stable snake_case attribute/relation key (e.g. "family_trip_destination").
    pub predicate: String,
    /// The value ("Paris", "120", "4 engineers").
    pub object: String,
    /// Event-time start (unix seconds) — when this became true.
    pub valid_from: i64,
    /// Event-time end (unix seconds), or None if still open.
    pub valid_to: Option<i64>,
    /// True = single-valued STATE (a newer value supersedes the old). False =
    /// an EVENT that accumulates over time.
    pub singular: bool,
}

/// A stored edge enriched with its point id and transaction time, as scrolled
/// back from the vector store for resolution/supersession.
#[derive(Debug, Clone)]
pub struct StoredEdge {
    pub id: String,
    pub edge: Edge,
    /// Transaction time — when the source document was captured.
    pub captured_at: i64,
    pub is_latest: bool,
}

/// The temporally-resolved view of one `(subject, predicate)` group: the full
/// dated timeline plus which object holds at the query timepoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub subject: String,
    pub predicate: String,
    /// (object, valid_from) sorted ascending by valid_from.
    pub timeline: Vec<(String, i64)>,
    /// The object that holds at the query timepoint (latest valid value).
    pub current: String,
    pub current_from: i64,
    pub singular: bool,
}

const EXTRACT_SYSTEM: &str = "You extract a TEMPORAL KNOWLEDGE GRAPH from a user's conversation \
history. Output edges of the form (subject, predicate, object) with the date each became true. \
Rules:\n\
- subject: the entity the fact is about — almost always \"user\" (use a person's name only when the \
fact is about someone else, e.g. \"Rachel\").\n\
- predicate: a SHORT, STABLE snake_case attribute key. Use the SAME key whenever the same kind of \
fact recurs, so updates line up (e.g. always \"job_title\", \"city_of_residence\", \
\"personal_best_5k_time\", \"starbucks_gold_star_threshold\", \"family_trip_destination\").\n\
- object: the concrete value, as short as possible (\"Paris\", \"120\", \"every week\", \"25:50\").\n\
- date: the YYYY-MM-DD on which the object became true. Resolve relative dates ('last Sunday', \
'two weeks ago') against the Conversation date given below. If no date is recoverable, use the \
Conversation date.\n\
- singular: true if this attribute holds ONE value at a time and a newer value REPLACES the old one \
(a status, a count, a current preference, a personal best). false if it is an EVENT that accumulates \
and does not replace earlier ones (each trip taken, each purchase, each class attended).\n\
Extract every distinct edge the content supports; many for dense content, none for boilerplate. \
Respond with ONLY a JSON array: \
[{\"subject\":\"user\",\"predicate\":\"family_trip_destination\",\"object\":\"Paris\",\"date\":\"2023-06-15\",\"singular\":false}, ...].";

/// Extract edges from a document. One LLM pass over the whole content (it is
/// already a single conversation/session by the time this runs). On any failure
/// returns an empty vec — the graph layer is strictly additive and non-fatal.
pub async fn extract_edges(
    llm: &dyn Llm,
    model: &ResolvedModel,
    title: &str,
    content: &str,
    doc_date: &str,
) -> Result<Vec<Edge>, String> {
    if !model.is_ready() {
        return Ok(vec![]);
    }
    let date_line = if doc_date.is_empty() {
        String::new()
    } else {
        format!("Conversation date: {doc_date}\n")
    };
    // Cap the content fed to one pass; edges are dense facts, not prose, so a
    // generous head covers a session without paying for the whole haystack.
    let body: String = content.chars().take(24_000).collect();
    let user = format!("{date_line}Title: {title}\n\n{body}");
    let raw = llm.chat(model, EXTRACT_SYSTEM, &user, 0.2).await?;
    Ok(parse_edges(&raw, doc_date))
}

/// Parse the extractor's JSON array into edges, resolving each `date` to a
/// `valid_from` unix timestamp. Tolerant of fences/prose; skips malformed rows
/// rather than failing the batch. `doc_date` is the fallback when a row omits a
/// usable date.
pub fn parse_edges(raw: &str, doc_date: &str) -> Vec<Edge> {
    let (Some(start), Some(end)) = (raw.find('['), raw.rfind(']')) else {
        return vec![];
    };
    if end <= start {
        return vec![];
    }
    let fallback = ymd_to_unix(doc_date);
    let arr: Vec<serde_json::Value> = match serde_json::from_str(&raw[start..=end]) {
        Ok(a) => a,
        Err(_) => return vec![],
    };
    let mut out = Vec::new();
    for v in arr {
        let subject = normalize_key(v["subject"].as_str().unwrap_or("user"));
        let predicate = normalize_key(v["predicate"].as_str().unwrap_or_default());
        let object = v["object"].as_str().unwrap_or_default().trim().to_string();
        if predicate.is_empty() || object.is_empty() {
            continue;
        }
        let valid_from = v["date"]
            .as_str()
            .and_then(ymd_to_unix)
            .or(fallback)
            .unwrap_or(0);
        // Accept bool or the strings "true"/"false"; default true (treat unknown
        // attributes as single-valued so they at least supersede cleanly).
        let singular = match &v["singular"] {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::String(s) => !s.eq_ignore_ascii_case("false"),
            _ => true,
        };
        out.push(Edge {
            subject: if subject.is_empty() { "user".into() } else { subject },
            predicate,
            object,
            valid_from,
            valid_to: None,
            singular,
        });
    }
    out
}

/// Canonicalize a subject/predicate key: lowercase, non-alphanumeric runs → `_`,
/// trimmed. Keeps the same attribute phrased slightly differently aligned so
/// supersession groups correctly.
pub fn normalize_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_us = false;
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// A YYYY-MM-DD date to a unix timestamp at local midnight. None on parse fail.
pub fn ymd_to_unix(s: &str) -> Option<i64> {
    let d = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()?;
    d.and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
        .map(|dt| dt.timestamp())
}

/// Group key for one attribute timeline.
fn group_key(e: &Edge) -> (String, String) {
    (e.subject.clone(), e.predicate.clone())
}

/// Compute supersession over a NEW edge against the existing stored edges that
/// share its `(subject, predicate)`. Pure so it is unit-testable: returns the
/// ids of existing edges that the new edge supersedes (to be flagged
/// is_latest=false) and whether the new edge should itself be stored as latest.
///
/// Only `singular` (state) attributes supersede — a newer-dated value replaces
/// older ones. Events never supersede (every occurrence stays latest).
pub fn supersession(new: &Edge, existing_same_group: &[StoredEdge]) -> (Vec<String>, bool) {
    if !new.singular {
        return (vec![], true); // events accumulate; always latest, supersede nothing
    }
    let mut superseded = Vec::new();
    let mut new_is_latest = true;
    for ex in existing_same_group {
        if !ex.is_latest {
            continue;
        }
        // Same value already known and latest → the new one adds nothing.
        if ex.edge.object.eq_ignore_ascii_case(&new.object) {
            new_is_latest = false;
            continue;
        }
        // A strictly newer (or same-time but later-learned) value wins.
        let newer = new.valid_from > ex.edge.valid_from
            || (new.valid_from == ex.edge.valid_from);
        if newer {
            superseded.push(ex.id.clone());
        } else {
            // The existing value is more recent; the new one is historical.
            new_is_latest = false;
        }
    }
    (superseded, new_is_latest)
}

/// Resolve stored edges into per-attribute timelines as of `as_of`. For each
/// `(subject, predicate)` group: sort by event time, expose the full timeline,
/// and pick `current` = the latest object whose `valid_from <= as_of` (falling
/// back to the earliest if all are in the future). This is what gets surfaced
/// to the answer model — dated, ordered, with the live value marked.
pub fn resolve(edges: &[StoredEdge], as_of: i64) -> Vec<Resolved> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<(String, String), Vec<&StoredEdge>> = BTreeMap::new();
    for e in edges {
        groups.entry(group_key(&e.edge)).or_default().push(e);
    }
    let mut out = Vec::new();
    for ((subject, predicate), mut members) in groups {
        members.sort_by_key(|e| e.edge.valid_from);
        let singular = members.iter().any(|e| e.edge.singular);
        // Timeline: dedup consecutive identical objects, keep first date seen.
        let mut timeline: Vec<(String, i64)> = Vec::new();
        for m in &members {
            if timeline
                .last()
                .map(|(o, _)| !o.eq_ignore_ascii_case(&m.edge.object))
                .unwrap_or(true)
            {
                timeline.push((m.edge.object.clone(), m.edge.valid_from));
            }
        }
        // Current = latest object valid at as_of; else the earliest known.
        let current_member = members
            .iter()
            .filter(|e| e.edge.valid_from <= as_of)
            .next_back()
            .or_else(|| members.first())
            .unwrap();
        out.push(Resolved {
            subject,
            predicate,
            timeline,
            current: current_member.edge.object.clone(),
            current_from: current_member.edge.valid_from,
            singular,
        });
    }
    out
}

/// Render resolved edges into a compact, dated context block for the answer
/// model. Singular attributes show the live value with its history; events show
/// the dated list with the most recent marked.
pub fn render_block(resolved: &[Resolved]) -> String {
    if resolved.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "Temporal knowledge (resolved from the memory graph; dates are when each became true):\n",
    );
    for r in resolved {
        let date = |unix: i64| {
            chrono::DateTime::from_timestamp(unix, 0)
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "?".into())
        };
        if r.singular {
            // State: show the current value, then prior values if any.
            let history: Vec<String> = r
                .timeline
                .iter()
                .filter(|(o, _)| !o.eq_ignore_ascii_case(&r.current))
                .map(|(o, t)| format!("{o} ({})", date(*t)))
                .collect();
            if history.is_empty() {
                s.push_str(&format!(
                    "- {} {}: {} (as of {})\n",
                    r.subject,
                    r.predicate,
                    r.current,
                    date(r.current_from)
                ));
            } else {
                s.push_str(&format!(
                    "- {} {}: CURRENT = {} (since {}); earlier: {}\n",
                    r.subject,
                    r.predicate,
                    r.current,
                    date(r.current_from),
                    history.join(", ")
                ));
            }
        } else {
            // Event: dated list, most recent last and marked.
            let items: Vec<String> = r
                .timeline
                .iter()
                .map(|(o, t)| format!("{o} ({})", date(*t)))
                .collect();
            s.push_str(&format!(
                "- {} {} over time: {} — most recent: {} ({})\n",
                r.subject,
                r.predicate,
                items.join(", "),
                r.current,
                date(r.current_from)
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> i64 {
        ymd_to_unix(s).unwrap()
    }
    fn stored(id: &str, pred: &str, obj: &str, date: &str, singular: bool, latest: bool) -> StoredEdge {
        StoredEdge {
            id: id.into(),
            edge: Edge {
                subject: "user".into(),
                predicate: pred.into(),
                object: obj.into(),
                valid_from: day(date),
                valid_to: None,
                singular,
            },
            captured_at: day(date),
            is_latest: latest,
        }
    }

    #[test]
    fn normalize_key_canonicalizes() {
        assert_eq!(normalize_key("Family Trip Destination"), "family_trip_destination");
        assert_eq!(normalize_key("  job-title!! "), "job_title");
        assert_eq!(normalize_key("starbucks_gold_star_threshold"), "starbucks_gold_star_threshold");
    }

    #[test]
    fn parse_edges_basic_and_dates() {
        let raw = r#"[
          {"subject":"user","predicate":"family_trip_destination","object":"Paris","date":"2023-06-15","singular":false},
          {"subject":"user","predicate":"Starbucks Gold Threshold","object":"120","date":"2023-04-01","singular":true}
        ]"#;
        let edges = parse_edges(raw, "2023-07-01");
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].object, "Paris");
        assert!(!edges[0].singular);
        assert_eq!(edges[1].predicate, "starbucks_gold_threshold");
        assert!(edges[1].singular);
        assert_eq!(edges[1].valid_from, day("2023-04-01"));
    }

    #[test]
    fn parse_edges_missing_date_falls_back_to_doc_date() {
        let raw = r#"[{"predicate":"city","object":"Lagos"}]"#;
        let edges = parse_edges(raw, "2024-02-02");
        assert_eq!(edges[0].valid_from, day("2024-02-02"));
        assert!(edges[0].singular); // default true
    }

    #[test]
    fn parse_edges_tolerates_garbage() {
        assert!(parse_edges("no json here", "2024-01-01").is_empty());
        // skips rows with no object/predicate but keeps the good one
        let raw = r#"[{"predicate":"x"},{"predicate":"job","object":"SWE","date":"2024-01-01"}]"#;
        assert_eq!(parse_edges(raw, "2024-01-01").len(), 1);
    }

    #[test]
    fn singular_supersedes_older_value() {
        // Existing: Starbucks threshold 250 (older). New: 120 (newer) → supersedes.
        let existing = vec![stored("a", "thr", "250", "2023-01-01", true, true)];
        let new = Edge {
            subject: "user".into(),
            predicate: "thr".into(),
            object: "120".into(),
            valid_from: day("2023-04-01"),
            valid_to: None,
            singular: true,
        };
        let (superseded, latest) = supersession(&new, &existing);
        assert_eq!(superseded, vec!["a".to_string()]);
        assert!(latest);
    }

    #[test]
    fn singular_older_value_does_not_win() {
        // Existing newer (Paris 2023-06). New older (Hawaii 2023-02) → new not latest.
        let existing = vec![stored("p", "trip_current", "Paris", "2023-06-15", true, true)];
        let new = Edge {
            subject: "user".into(),
            predicate: "trip_current".into(),
            object: "Hawaii".into(),
            valid_from: day("2023-02-10"),
            valid_to: None,
            singular: true,
        };
        let (superseded, latest) = supersession(&new, &existing);
        assert!(superseded.is_empty());
        assert!(!latest);
    }

    #[test]
    fn events_never_supersede() {
        let existing = vec![stored("h", "trip", "Hawaii", "2023-02-10", false, true)];
        let new = Edge {
            subject: "user".into(),
            predicate: "trip".into(),
            object: "Paris".into(),
            valid_from: day("2023-06-15"),
            valid_to: None,
            singular: false,
        };
        let (superseded, latest) = supersession(&new, &existing);
        assert!(superseded.is_empty());
        assert!(latest);
    }

    #[test]
    fn resolve_picks_most_recent_event() {
        // The Hawaii-vs-Paris failure: "most recent family trip" must be Paris.
        let edges = vec![
            stored("h", "family_trip", "Hawaii", "2023-02-10", false, true),
            stored("p", "family_trip", "Paris", "2023-06-15", false, true),
        ];
        let r = resolve(&edges, day("2023-07-01"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].current, "Paris");
        assert_eq!(r[0].timeline.len(), 2);
        assert_eq!(r[0].timeline[0].0, "Hawaii"); // sorted ascending
    }

    #[test]
    fn resolve_picks_current_state_value() {
        // 5K personal best improved 27:12 → 25:50; current is the latest-dated.
        let edges = vec![
            stored("a", "pb_5k", "27:12", "2023-01-01", true, false),
            stored("b", "pb_5k", "25:50", "2023-05-01", true, true),
        ];
        let r = resolve(&edges, day("2023-06-01"));
        assert_eq!(r[0].current, "25:50");
        assert!(r[0].singular);
    }

    #[test]
    fn render_block_marks_current() {
        let edges = vec![
            stored("a", "city", "Lagos", "2022-01-01", true, false),
            stored("b", "city", "Nairobi", "2024-01-01", true, true),
        ];
        let block = render_block(&resolve(&edges, day("2025-01-01")));
        assert!(block.contains("CURRENT = Nairobi"));
        assert!(block.contains("earlier: Lagos"));
    }
}
