//! The memory lifecycle — what turns Recally from "RAG over chunks" into
//! "memory" in SuperMemory's sense. Distilled facts don't just get embedded;
//! each new fact is reconciled against the memories already stored:
//!
//!   • DUPLICATE — already known. Drop it (bump the original's repetition).
//!   • UPDATE    — contradicts an existing memory ("switched Adidas → Puma").
//!                 The old memory is kept for history but flagged is_latest=false;
//!                 search returns only the new one.
//!   • EXTEND    — enriches an existing memory without contradicting it. Both stay.
//!   • NEW       — unrelated. Store as-is.
//!
//! This is the loop the design doc describes: embed fact → nearest existing
//! memories → LLM classifies relation → write edges + flip is_latest. It is an
//! ingest-time LLM pass, nothing magic. Reconciliation only runs against facts
//! whose nearest neighbour is similar enough to possibly relate (most facts are
//! NEW and skip the LLM entirely), so the cost is one classification call per
//! document, not per fact.
//!
//! Memories live in the facts collection with lifecycle metadata in the
//! payload (memory_id, is_latest, kind, supersedes, extends), so the engine
//! stays HTTP-only and headless-testable. Legacy facts predate these fields;
//! absence of `is_latest=false` means "latest", so they remain searchable.

use crate::llm::ResolvedModel;
use crate::providers::Llm;
use serde_json::Value;

/// Cosine similarity above which a new fact is close enough to an existing
/// memory that it might be a duplicate/update/extension worth an LLM check.
/// Below it, the fact is treated as NEW with no call.
pub const RELATE_THRESHOLD: f32 = 0.75;

/// How many nearest existing memories to consider per new fact. Single-neighbor
/// reconciliation misses the case where a fact contradicts memory #2 while #1 is
/// a coincidence; the classifier picks the truly related one from the top-k.
pub const RECONCILE_TOPK: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    New,
    Duplicate,
    Update,
    Extend,
    /// The classifier thinks this contradicts a memory but isn't confident. The
    /// fact is stored but quarantined (held out of active retrieval) instead of
    /// force-flipping the existing memory — a human/review step decides.
    NeedsReview,
}

/// How sure the classifier is. Only a high-confidence contradiction is allowed to
/// supersede an existing memory; anything less is quarantined for review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Low,
}

/// An existing memory near a new fact: its point id and text.
#[derive(Debug, Clone)]
pub struct Neighbor {
    pub memory_id: String,
    pub fact: String,
    pub score: f32,
}

/// The classifier's verdict for one new fact against its candidate neighbors.
#[derive(Debug, Clone, Copy)]
pub struct Classification {
    pub relation: Relation,
    /// 1-based index of the candidate neighbor this relates to; 0 = none.
    pub reference: usize,
    pub confidence: Confidence,
}

/// What to do with one new fact after reconciliation.
#[derive(Debug, Clone)]
pub struct Action {
    pub fact: String,
    pub relation: Relation,
    /// memory_id of the memory this one supersedes (UPDATE) — to be flagged
    /// is_latest=false.
    pub supersedes: Option<String>,
    /// memory_id this one extends (EXTEND) — recorded as a graph edge.
    pub extends: Option<String>,
}

impl Action {
    fn plain_new(fact: String) -> Self {
        Action {
            fact,
            relation: Relation::New,
            supersedes: None,
            extends: None,
        }
    }
}

/// Turn a classifier verdict into an action, applying the safety policy.
///
/// - UPDATE supersedes only on HIGH confidence with a real referenced neighbor;
///   otherwise the fact is quarantined (`NeedsReview`), never a blind flip.
/// - DUPLICATE drops only on HIGH confidence; a low-confidence "duplicate" is
///   kept as NEW rather than silently discarded.
/// - EXTEND links to the referenced neighbor; a bare NEW (or no valid ref)
///   stores the fact plainly.
///
/// Pure and deterministic — the offline test surface for the reconcile policy.
pub fn action_for(fact: String, neighbors: &[Neighbor], c: Classification) -> Action {
    let referenced = c
        .reference
        .checked_sub(1)
        .and_then(|i| neighbors.get(i))
        .map(|n| n.memory_id.clone());
    match (c.relation, c.confidence, referenced) {
        (Relation::Update, Confidence::High, Some(id)) => Action {
            fact,
            relation: Relation::Update,
            supersedes: Some(id),
            extends: None,
        },
        // Uncertain contradiction, or no memory to point at → quarantine.
        (Relation::Update, _, _) => Action {
            fact,
            relation: Relation::NeedsReview,
            supersedes: None,
            extends: None,
        },
        (Relation::Duplicate, Confidence::High, _) => Action {
            fact,
            relation: Relation::Duplicate,
            supersedes: None,
            extends: None,
        },
        (Relation::Extend, _, Some(id)) => Action {
            fact,
            relation: Relation::Extend,
            supersedes: None,
            extends: Some(id),
        },
        // Low-confidence duplicate, ref-less extend, or NEW → store plainly.
        _ => Action::plain_new(fact),
    }
}

const SYSTEM: &str = "You reconcile a user's memory. For each numbered NEW fact you are given up to a \
few EXISTING memories already stored (its nearest matches). Decide the NEW fact's relationship to the \
MOST relevant existing memory, as exactly one of:\n\
- DUPLICATE: the new fact says the same thing already captured (no new information).\n\
- UPDATE: the new fact contradicts or supersedes an existing memory — a changed state, a correction, \
a newer value. Example: existing \"uses Adidas\", new \"switched to Puma\".\n\
- EXTEND: the new fact adds detail to an existing memory without contradicting it.\n\
- NEW: the new fact is about something different; the matches are coincidences.\n\
Also give `ref`: the number of the existing memory you related it to (0 if NEW), and `confidence`: \
\"high\" only when you are certain, otherwise \"low\". Be conservative — use UPDATE with high \
confidence ONLY for a clear contradiction of that specific memory.\n\
Respond with ONLY a JSON array, one object per NEW fact in order: \
[{\"i\":1,\"relation\":\"DUPLICATE|UPDATE|EXTEND|NEW\",\"ref\":2,\"confidence\":\"high|low\"}, ...]. No prose.";

/// Reconcile new facts (each with its top-k nearest existing memories) into
/// actions. Facts whose nearest neighbour is below `RELATE_THRESHOLD` are NEW
/// with no LLM cost; the rest are classified in one batched call. On any
/// LLM/parse failure every candidate degrades to NEW (never lose a fact).
pub async fn reconcile(
    llm: &dyn Llm,
    model: &ResolvedModel,
    facts_with_neighbors: Vec<(String, Vec<Neighbor>)>,
) -> Vec<Action> {
    // Split: clear-NEW (no neighbour clears the bar) vs candidates. Keep only the
    // neighbours at/above threshold, best-first, capped at RECONCILE_TOPK.
    let mut actions: Vec<Action> = Vec::with_capacity(facts_with_neighbors.len());
    let mut candidates: Vec<(String, Vec<Neighbor>)> = Vec::new();
    for (fact, mut neighbors) in facts_with_neighbors {
        neighbors.retain(|n| n.score >= RELATE_THRESHOLD);
        neighbors.sort_by(|a, b| b.score.total_cmp(&a.score));
        neighbors.truncate(RECONCILE_TOPK);
        if neighbors.is_empty() {
            actions.push(Action::plain_new(fact));
        } else {
            candidates.push((fact, neighbors));
        }
    }
    if candidates.is_empty() || !model.is_ready() {
        actions.extend(candidates.into_iter().map(|(f, _)| Action::plain_new(f)));
        return actions;
    }

    let prompt = candidates
        .iter()
        .enumerate()
        .map(|(i, (fact, neighbors))| {
            let listed = neighbors
                .iter()
                .enumerate()
                .map(|(j, n)| format!("     {}) {}", j + 1, n.fact))
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}. NEW: {fact}\n   EXISTING:\n{listed}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let classifications = match llm.chat(model, SYSTEM, &prompt, 0.0).await {
        Ok(raw) => parse_classifications(&raw, candidates.len()),
        Err(_) => None,
    };

    match classifications {
        Some(cls) => {
            for (i, (fact, neighbors)) in candidates.into_iter().enumerate() {
                let c = cls.get(i).copied().unwrap_or(Classification {
                    relation: Relation::New,
                    reference: 0,
                    confidence: Confidence::Low,
                });
                actions.push(action_for(fact, &neighbors, c));
            }
        }
        None => actions.extend(candidates.into_iter().map(|(f, _)| Action::plain_new(f))),
    }
    actions
}

/// Split an optional `[until YYYY-MM-DD]` expiry suffix off a fact. The
/// distiller is asked to append it to time-bound facts ("the exam is tomorrow
/// [until 2026-06-15]"). Returns `(clean_fact, Some(unix_end_of_day))` when a
/// valid date is found, else `(fact_trimmed, None)`.
pub fn parse_expiry(fact: &str) -> (String, Option<i64>) {
    let trimmed = fact.trim();
    if let Some(open) = trimmed.rfind("[until ") {
        if trimmed.ends_with(']') {
            let date_str = &trimmed[open + 7..trimmed.len() - 1];
            if let Ok(d) = chrono::NaiveDate::parse_from_str(date_str.trim(), "%Y-%m-%d") {
                if let Some(end) = d
                    .and_hms_opt(23, 59, 59)
                    .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
                {
                    let clean = trimmed[..open].trim().to_string();
                    return (clean, Some(end.timestamp()));
                }
            }
        }
    }
    (trimmed.to_string(), None)
}

/// Parse the classifier's JSON array into classifications, ordered by `i`.
/// Tolerates fences/prose around the array. Returns None if no array is found.
/// Missing/garbled fields default conservatively (relation NEW, ref 0, low
/// confidence) so a partial response can never force a supersede.
fn parse_classifications(raw: &str, n: usize) -> Option<Vec<Classification>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end < start {
        return None;
    }
    let arr: Vec<Value> = serde_json::from_str(&raw[start..=end]).ok()?;
    let mut out = vec![
        Classification {
            relation: Relation::New,
            reference: 0,
            confidence: Confidence::Low,
        };
        n
    ];
    for obj in arr {
        let idx = obj["i"].as_u64().unwrap_or(0) as usize;
        if idx == 0 || idx > n {
            continue;
        }
        let relation = match obj["relation"]
            .as_str()
            .unwrap_or("NEW")
            .to_ascii_uppercase()
            .as_str()
        {
            "DUPLICATE" => Relation::Duplicate,
            "UPDATE" => Relation::Update,
            "EXTEND" => Relation::Extend,
            _ => Relation::New,
        };
        let confidence = match obj["confidence"]
            .as_str()
            .unwrap_or("low")
            .to_ascii_lowercase()
            .as_str()
        {
            "high" => Confidence::High,
            _ => Confidence::Low,
        };
        let reference = obj["ref"].as_u64().unwrap_or(0) as usize;
        out[idx - 1] = Classification {
            relation,
            reference,
            confidence,
        };
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neighbor(id: &str, fact: &str, score: f32) -> Neighbor {
        Neighbor {
            memory_id: id.into(),
            fact: fact.into(),
            score,
        }
    }

    fn cls(relation: Relation, reference: usize, confidence: Confidence) -> Classification {
        Classification {
            relation,
            reference,
            confidence,
        }
    }

    #[test]
    fn parse_classifications_orders_by_index() {
        let raw = r#"[{"i":2,"relation":"UPDATE","ref":1,"confidence":"high"},{"i":1,"relation":"DUPLICATE","ref":1,"confidence":"high"}]"#;
        let cs = parse_classifications(raw, 2).unwrap();
        assert_eq!(cs[0].relation, Relation::Duplicate);
        assert_eq!(cs[1].relation, Relation::Update);
        assert_eq!(cs[1].confidence, Confidence::High);
        assert_eq!(cs[1].reference, 1);
    }

    #[test]
    fn parse_classifications_defaults_missing_conservatively() {
        // Missing confidence → low; unmentioned entries → NEW/low/0.
        let raw = "```json\n[{\"i\":1,\"relation\":\"UPDATE\",\"ref\":2}]\n```";
        let cs = parse_classifications(raw, 3).unwrap();
        assert_eq!(cs[0].relation, Relation::Update);
        assert_eq!(cs[0].confidence, Confidence::Low); // missing → low
        assert_eq!(cs[1].relation, Relation::New);
        assert_eq!(cs[2].relation, Relation::New);
    }

    #[test]
    fn parse_classifications_garbage_is_none() {
        assert!(parse_classifications("no array here", 2).is_none());
    }

    #[test]
    fn high_confidence_update_supersedes_referenced_memory() {
        let ns = vec![
            neighbor("m1", "coincidence", 0.80),
            neighbor("m2", "uses Adidas", 0.90),
        ];
        let a = action_for(
            "switched to Puma".into(),
            &ns,
            cls(Relation::Update, 2, Confidence::High),
        );
        assert_eq!(a.relation, Relation::Update);
        assert_eq!(a.supersedes.as_deref(), Some("m2"));
    }

    #[test]
    fn low_confidence_update_is_quarantined_not_flipped() {
        let ns = vec![neighbor("m1", "uses Adidas", 0.90)];
        let a = action_for(
            "maybe likes Puma".into(),
            &ns,
            cls(Relation::Update, 1, Confidence::Low),
        );
        assert_eq!(a.relation, Relation::NeedsReview);
        assert!(
            a.supersedes.is_none(),
            "uncertain contradiction must not supersede"
        );
    }

    #[test]
    fn update_without_valid_ref_is_quarantined() {
        let ns = vec![neighbor("m1", "x", 0.90)];
        let a = action_for("y".into(), &ns, cls(Relation::Update, 0, Confidence::High));
        assert_eq!(a.relation, Relation::NeedsReview);
    }

    #[test]
    fn low_confidence_duplicate_is_kept_as_new() {
        let ns = vec![neighbor("m1", "x", 0.90)];
        let a = action_for(
            "x-ish".into(),
            &ns,
            cls(Relation::Duplicate, 1, Confidence::Low),
        );
        assert_eq!(
            a.relation,
            Relation::New,
            "unsure duplicate should be stored, not dropped"
        );
    }

    #[test]
    fn high_confidence_duplicate_drops() {
        let ns = vec![neighbor("m1", "x", 0.90)];
        let a = action_for(
            "x".into(),
            &ns,
            cls(Relation::Duplicate, 1, Confidence::High),
        );
        assert_eq!(a.relation, Relation::Duplicate);
    }

    #[test]
    fn extend_links_referenced_memory() {
        let ns = vec![neighbor("m1", "works at Acme", 0.85)];
        let a = action_for(
            "works at Acme as CTO".into(),
            &ns,
            cls(Relation::Extend, 1, Confidence::High),
        );
        assert_eq!(a.relation, Relation::Extend);
        assert_eq!(a.extends.as_deref(), Some("m1"));
    }

    #[test]
    fn parse_expiry_extracts_date_and_cleans_text() {
        let (clean, until) = parse_expiry("The exam is tomorrow [until 2026-06-15]");
        assert_eq!(clean, "The exam is tomorrow");
        assert!(until.is_some());
    }

    #[test]
    fn parse_expiry_no_suffix_is_none() {
        let (clean, until) = parse_expiry("The user prefers Puma");
        assert_eq!(clean, "The user prefers Puma");
        assert!(until.is_none());
    }

    #[test]
    fn parse_expiry_garbage_date_is_ignored() {
        let (clean, until) = parse_expiry("Something [until not-a-date]");
        assert_eq!(clean, "Something [until not-a-date]");
        assert!(until.is_none());
    }

    #[test]
    fn reconcile_marks_far_neighbors_as_new_without_llm() {
        // Neighbor below threshold → NEW, no model needed.
        let llm = crate::llm::LlmClient::new();
        let model = ResolvedModel::groq(String::new(), "x"); // unready
        let rt = tokio::runtime::Runtime::new().unwrap();
        let actions = rt.block_on(reconcile(
            &llm,
            &model,
            vec![
                ("brand new fact".into(), vec![]),
                ("loosely related".into(), vec![neighbor("m1", "old", 0.50)]),
            ],
        ));
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().all(|a| a.relation == Relation::New));
    }

    #[test]
    fn reconcile_degrades_to_new_when_model_unready() {
        // Above-threshold candidate but no usable model → still safe (NEW).
        let llm = crate::llm::LlmClient::new();
        let model = ResolvedModel::groq(String::new(), "x");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let actions = rt.block_on(reconcile(
            &llm,
            &model,
            vec![("x".into(), vec![neighbor("m1", "old", 0.95)])],
        ));
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].relation, Relation::New);
    }
}
