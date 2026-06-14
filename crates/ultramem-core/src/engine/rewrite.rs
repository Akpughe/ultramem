//! Retrieval planning: a fast Groq pass that turns the user's question into a
//! search plan — relative dates resolved to absolutes ("yesterday" means
//! nothing to an embedding), source intent detected ("websites I visited" →
//! browser), and list-style questions flagged so retrieval widens. Non-fatal:
//! on any failure the raw question is searched as-is.

use crate::llm::ResolvedModel;
use crate::providers::Llm;
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct SearchPlan {
    /// rewritten, keyword-rich search text (falls back to the raw question)
    pub query: String,
    /// clipboard | browser | file | meeting
    pub source: Option<String>,
    pub after: Option<i64>,
    pub before: Option<i64>,
    /// "list everything…" style — retrieval should return more documents
    pub listy: bool,
}

pub async fn plan(llm: &dyn Llm, model: &ResolvedModel, question: &str, context: Option<&str>) -> SearchPlan {
    let fallback = SearchPlan {
        query: question.to_string(),
        ..Default::default()
    };
    if !model.is_ready() {
        return fallback;
    }
    let now = chrono::Local::now();
    let system = format!(
        "You turn a question about a user's personal memory (their clipboard, browsing history, files, \
and meetings) into a search plan. Today is {}.\n\
Respond with ONLY JSON: {{\"query\": string, \"source\": string|null, \"after\": string|null, \
\"before\": string|null, \"list\": boolean}}\n\
- query: the question rewritten as keyword-rich search text. Resolve relative dates to absolute \
ones (e.g. \"yesterday\" → \"{}\"). Keep names, sites and topics.\n\
- source: exactly one of clipboard|browser|file|meeting ONLY when the question clearly targets it \
(websites/links/videos/browsing → browser; copied/clipboard → clipboard; files/documents/PDFs → file; \
meetings/calls/standups → meeting). Otherwise null.\n\
- after/before: calendar dates as \"YYYY-MM-DD\" strings bounding the time window the question \
references, INCLUSIVE on both ends. A single day means after and before are that same date \
(\"yesterday\" → both set to {}). \"this past week\" → after = 7 days ago, before = {}. \
Both null when no time window is mentioned.\n\
- list: true when the user asks to list/enumerate many items.",
        now.format("%A, %B %e, %Y"),
        (now - chrono::Duration::days(1)).format("%B %e, %Y"),
        (now - chrono::Duration::days(1)).format("%Y-%m-%d"),
        now.format("%Y-%m-%d"),
    );
    // Follow-ups ("what is it about?") are meaningless alone — give the
    // planner the recent turns so references resolve to real names/topics.
    let user = match context {
        Some(c) if !c.is_empty() => format!(
            "Recent conversation:\n{c}\n\nQuestion: {question}\n\nIf the question refers to the \
conversation (\"it\", \"the doc\", \"this\"), the rewritten query MUST name the thing referred to \
(its title/topic from the conversation), never a generic phrase."
        ),
        _ => question.to_string(),
    };
    let Ok(raw) = llm.chat(model, &system, &user, 0.0).await else {
        return fallback;
    };
    parse_plan(&raw, question).unwrap_or(fallback)
}

/// "YYYY-MM-DD" (local calendar day) → unix seconds. `end_of_day` picks the
/// 23:59:59 bound so a single-day window covers the whole day.
fn date_to_unix(s: &str, end_of_day: bool) -> Option<i64> {
    let d = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()?;
    let t = if end_of_day {
        d.and_hms_opt(23, 59, 59)?
    } else {
        d.and_hms_opt(0, 0, 0)?
    };
    Some(
        t.and_local_timezone(chrono::Local)
            .earliest()?
            .timestamp(),
    )
}

fn parse_plan(raw: &str, question: &str) -> Option<SearchPlan> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    let v: Value = serde_json::from_str(&raw[start..=end]).ok()?;
    let query = v["query"].as_str().unwrap_or(question).trim().to_string();
    let source = v["source"]
        .as_str()
        .map(str::to_lowercase)
        .filter(|s| ["clipboard", "browser", "file", "meeting"].contains(&s.as_str()));
    // Date math happens here, not in the model — small models reliably emit
    // calendar dates but reliably botch epoch arithmetic.
    let after = v["after"].as_str().and_then(|s| date_to_unix(s, false));
    let before = v["before"].as_str().and_then(|s| date_to_unix(s, true));
    Some(SearchPlan {
        query: if query.is_empty() { question.to_string() } else { query },
        source,
        after,
        before,
        listy: v["list"].as_bool().unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_plan_with_calendar_dates() {
        let p = parse_plan(
            r#"{"query":"websites visited June 11 2026","source":"browser","after":"2026-06-11","before":"2026-06-11","list":true}"#,
            "q",
        )
        .unwrap();
        assert_eq!(p.source.as_deref(), Some("browser"));
        assert!(p.listy);
        let (a, b) = (p.after.unwrap(), p.before.unwrap());
        assert_eq!(b - a, 86_399, "single-day window covers the whole day");
    }

    #[test]
    fn garbage_dates_become_no_window() {
        let p = parse_plan(r#"{"query":"x","source":null,"after":"not-a-date","before":1781,"list":false}"#, "q").unwrap();
        assert!(p.after.is_none() && p.before.is_none());
    }

    #[test]
    fn rejects_unknown_source_and_keeps_question_on_empty_query() {
        let p = parse_plan(r#"{"query":"","source":"email","list":false}"#, "original").unwrap();
        assert_eq!(p.query, "original");
        assert!(p.source.is_none());
    }

    #[test]
    fn fenced_json_parses() {
        let p = parse_plan("```json\n{\"query\":\"x\",\"source\":null,\"after\":null,\"before\":null,\"list\":false}\n```", "q").unwrap();
        assert_eq!(p.query, "x");
    }
}
