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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    New,
    Duplicate,
    Update,
    Extend,
}

/// An existing memory near a new fact: its point id and text.
#[derive(Debug, Clone)]
pub struct Neighbor {
    pub memory_id: String,
    pub fact: String,
    pub score: f32,
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
        Action { fact, relation: Relation::New, supersedes: None, extends: None }
    }
}

const SYSTEM: &str = "You reconcile a user's memory. For each numbered pair you are given a NEW fact \
and the most similar EXISTING memory already stored. Classify the NEW fact's relationship to that \
existing memory as exactly one of:\n\
- DUPLICATE: the new fact says the same thing already captured (no new information).\n\
- UPDATE: the new fact contradicts or supersedes the existing memory (a changed state, a correction, \
a newer value). Example: existing \"uses Adidas\", new \"switched to Puma\".\n\
- EXTEND: the new fact adds detail to the same subject without contradicting it.\n\
- NEW: the new fact is about something different; the similar memory is a coincidence.\n\
Respond with ONLY a JSON array, one object per pair in order: \
[{\"i\":1,\"relation\":\"DUPLICATE|UPDATE|EXTEND|NEW\"}, ...]. No prose.";

/// Reconcile new facts (each with its single nearest existing memory, if any)
/// into actions. Facts with no near neighbour are NEW with no LLM cost; the
/// rest are classified in one batched call. On any LLM/parse failure every
/// candidate degrades to NEW (never lose a fact).
pub async fn reconcile(
    llm: &dyn Llm,
    model: &ResolvedModel,
    facts_with_neighbors: Vec<(String, Option<Neighbor>)>,
) -> Vec<Action> {
    // Split: clear-NEW (no neighbour) vs candidates (have a near neighbour).
    let mut actions: Vec<Action> = Vec::with_capacity(facts_with_neighbors.len());
    let mut candidates: Vec<(String, Neighbor)> = Vec::new();
    for (fact, neighbor) in facts_with_neighbors {
        match neighbor {
            Some(n) if n.score >= RELATE_THRESHOLD => candidates.push((fact, n)),
            _ => actions.push(Action::plain_new(fact)),
        }
    }
    if candidates.is_empty() || !model.is_ready() {
        actions.extend(candidates.into_iter().map(|(f, _)| Action::plain_new(f)));
        return actions;
    }

    let prompt = candidates
        .iter()
        .enumerate()
        .map(|(i, (fact, n))| format!("{}. NEW: {fact}\n   EXISTING: {}", i + 1, n.fact))
        .collect::<Vec<_>>()
        .join("\n");

    let relations = match llm.chat(model, SYSTEM, &prompt, 0.0).await {
        Ok(raw) => parse_relations(&raw, candidates.len()),
        Err(_) => None,
    };

    match relations {
        Some(rels) => {
            for (i, (fact, n)) in candidates.into_iter().enumerate() {
                let rel = rels.get(i).copied().unwrap_or(Relation::New);
                actions.push(match rel {
                    Relation::New => Action::plain_new(fact),
                    Relation::Duplicate => Action { fact, relation: Relation::Duplicate, supersedes: None, extends: None },
                    Relation::Update => Action { fact, relation: Relation::Update, supersedes: Some(n.memory_id), extends: None },
                    Relation::Extend => Action { fact, relation: Relation::Extend, supersedes: None, extends: Some(n.memory_id) },
                });
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

/// Parse the classifier's JSON array into relations, ordered by `i`. Tolerates
/// fences/prose around the array. Returns None if no array is found.
fn parse_relations(raw: &str, n: usize) -> Option<Vec<Relation>> {
    let start = raw.find('[')?;
    let end = raw.rfind(']')?;
    if end < start {
        return None;
    }
    let arr: Vec<Value> = serde_json::from_str(&raw[start..=end]).ok()?;
    let mut out = vec![Relation::New; n];
    for obj in arr {
        let idx = obj["i"].as_u64().unwrap_or(0) as usize;
        if idx == 0 || idx > n {
            continue;
        }
        out[idx - 1] = match obj["relation"].as_str().unwrap_or("NEW").to_ascii_uppercase().as_str() {
            "DUPLICATE" => Relation::Duplicate,
            "UPDATE" => Relation::Update,
            "EXTEND" => Relation::Extend,
            _ => Relation::New,
        };
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neighbor(id: &str, fact: &str, score: f32) -> Neighbor {
        Neighbor { memory_id: id.into(), fact: fact.into(), score }
    }

    #[test]
    fn parse_relations_orders_by_index() {
        let raw = r#"[{"i":2,"relation":"UPDATE"},{"i":1,"relation":"DUPLICATE"}]"#;
        let rels = parse_relations(raw, 2).unwrap();
        assert_eq!(rels[0], Relation::Duplicate);
        assert_eq!(rels[1], Relation::Update);
    }

    #[test]
    fn parse_relations_fenced_and_defaults_missing_to_new() {
        let raw = "```json\n[{\"i\":1,\"relation\":\"EXTEND\"}]\n```";
        let rels = parse_relations(raw, 3).unwrap();
        assert_eq!(rels[0], Relation::Extend);
        assert_eq!(rels[1], Relation::New); // unmentioned → New
        assert_eq!(rels[2], Relation::New);
    }

    #[test]
    fn parse_relations_garbage_is_none() {
        assert!(parse_relations("no array here", 2).is_none());
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
                ("brand new fact".into(), None),
                ("loosely related".into(), Some(neighbor("m1", "old", 0.50))),
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
            vec![("x".into(), Some(neighbor("m1", "old", 0.95)))],
        ));
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].relation, Relation::New);
    }
}
