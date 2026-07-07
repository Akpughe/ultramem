//! Secret scrubbing (Sprint 1B, SS-4).
//!
//! High-confidence credentials are masked *before* content is chunked, embedded,
//! stored, distilled, or sent to any provider — the single choke point is
//! `add_document` right after text acquisition, so no downstream path (chunk
//! payload, embedding input, distillation, graph) ever sees the raw secret.
//!
//! Deliberately **conservative**: only patterns with a negligible false-positive
//! rate, so a real memory is never silently eaten. Ordinary PII (names, emails,
//! phone numbers) is intentionally NOT redacted here — broader PII policy is a
//! separate, more opinionated decision.

use regex::Regex;
use std::sync::OnceLock;

struct Pattern {
    kind: &'static str,
    re: Regex,
}

/// Compiled once, in priority order. More specific patterns come first so their
/// label wins (e.g. `sk-ant-` is an Anthropic key, not a generic `sk-` key).
fn patterns() -> &'static [Pattern] {
    static PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let mk = |kind: &'static str, pat: &str| Pattern {
            kind,
            re: Regex::new(pat).expect("valid secret regex"),
        };
        vec![
            // PEM private-key blocks (RSA/EC/OPENSSH/generic), multiline.
            mk(
                "private_key",
                r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
            ),
            // JWT: three base64url segments, first two are JSON objects ("eyJ").
            mk(
                "jwt",
                r"\beyJ[A-Za-z0-9_-]{6,}\.eyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\b",
            ),
            // AWS access key id.
            mk("aws_key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
            // GitHub tokens: PAT/OAuth/user/server/refresh + fine-grained PAT.
            mk("github_token", r"\bgh[pousr]_[A-Za-z0-9]{36,}\b"),
            mk("github_token", r"\bgithub_pat_[A-Za-z0-9_]{60,}\b"),
            // Anthropic (before the generic sk- rule so it labels correctly).
            mk("anthropic_key", r"\bsk-ant-[A-Za-z0-9_-]{20,}\b"),
            // OpenAI-style secret keys.
            mk("openai_key", r"\bsk-(?:proj-)?[A-Za-z0-9]{20,}\b"),
            // Google API key.
            mk("google_api_key", r"\bAIza[0-9A-Za-z_-]{35}\b"),
            // Slack tokens.
            mk("slack_token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
            // Stripe live secret/restricted keys.
            mk("stripe_key", r"\b[sr]k_live_[0-9A-Za-z]{16,}\b"),
        ]
    })
}

/// Replace every high-confidence secret in `text` with `[REDACTED:<kind>]`.
/// Returns the original string unchanged when nothing matches.
pub fn scrub(text: &str) -> String {
    let mut out = text.to_string();
    for p in patterns() {
        if p.re.is_match(&out) {
            let replacement = format!("[REDACTED:{}]", p.kind);
            out = p.re.replace_all(&out, replacement.as_str()).into_owned();
        }
    }
    out
}

/// Whether `text` contains any high-confidence secret (for flags/tests).
pub fn contains_secret(text: &str) -> bool {
    patterns().iter().any(|p| p.re.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redacted_away(secret: &str, kind: &str) {
        let doc = format!("here is a credential {secret} in some notes");
        let out = scrub(&doc);
        assert!(!out.contains(secret), "secret survived scrubbing: {secret}");
        assert!(
            out.contains(&format!("[REDACTED:{kind}]")),
            "wrong label for {secret}: {out}"
        );
        assert!(contains_secret(&doc));
    }

    #[test]
    fn redacts_aws_access_key() {
        redacted_away("AKIAIOSFODNN7EXAMPLE", "aws_key");
    }

    #[test]
    fn redacts_github_token() {
        redacted_away("ghp_1234567890abcdefghijklmnopqrstuvwxyz", "github_token");
    }

    #[test]
    fn redacts_openai_key() {
        redacted_away("sk-abcdefghijklmnopqrstuvwx1234", "openai_key");
    }

    #[test]
    fn redacts_anthropic_key_with_correct_label() {
        // Must label as anthropic, not be swallowed by the generic sk- rule.
        redacted_away("sk-ant-abcdefghijklmnopqrstuvwxyz", "anthropic_key");
    }

    #[test]
    fn redacts_jwt() {
        redacted_away(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N",
            "jwt",
        );
    }

    #[test]
    fn redacts_pem_private_key_block() {
        let doc = "config:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\nabcd/EF+gh\n-----END RSA PRIVATE KEY-----\nend";
        let out = scrub(doc);
        assert!(!out.contains("MIIEowIBAAKCAQEA"));
        assert!(out.contains("[REDACTED:private_key]"));
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        // Conservative: names, emails, and normal prose are NOT redacted.
        let doc = "Dave (dave@edusuc.net) prefers Rust and lives in Cape Town. Ticket ABC-1234.";
        assert_eq!(scrub(doc), doc);
        assert!(!contains_secret(doc));
    }

    #[test]
    fn scrub_is_idempotent() {
        let doc = "key AKIAIOSFODNN7EXAMPLE here";
        let once = scrub(doc);
        assert_eq!(scrub(&once), once);
    }
}
