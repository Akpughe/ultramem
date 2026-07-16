//! Entity resolution (9/10 quality) — mapping surface forms of an entity
//! ("J. Smith", "jane smith") to one canonical name, **per namespace**. Pure and
//! I/O-free so it is exhaustively testable; the alias store and the admin
//! endpoints live outside.
//!
//! Resolution is **explicit and fail-open on identity**: only surface forms an
//! operator has registered as aliases are unified, and an unknown name resolves
//! to itself — resolution never invents, merges, or drops an entity on its own.

use crate::db::AliasEntry;

/// Normalize a surface form for alias matching: trim, fold case, collapse runs of
/// whitespace to one space, and drop leading/trailing non-alphanumerics (so
/// "  Jane Smith. " and "jane smith" key the same). Internal punctuation is kept
/// (so "j.smith" stays distinct from "j smith"). Deterministic.
pub fn normalize(s: &str) -> String {
    let trimmed = s.trim().trim_matches(|c: char| !c.is_alphanumeric());
    let mut out = String::with_capacity(trimmed.len());
    let mut prev_space = false;
    for c in trimmed.chars() {
        if c.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.extend(c.to_lowercase());
            prev_space = false;
        }
    }
    out
}

/// Resolve a name to its canonical form within a namespace's `aliases`. An
/// unregistered name resolves to itself (its trimmed original), so resolution is
/// lossless: it only ever collapses forms an operator has explicitly aliased.
pub fn resolve(name: &str, aliases: &[AliasEntry]) -> String {
    let key = normalize(name);
    aliases
        .iter()
        .find(|a| a.alias == key)
        .map(|a| a.canonical.clone())
        .unwrap_or_else(|| name.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(tag: &str, alias: &str, canonical: &str) -> AliasEntry {
        AliasEntry {
            container_tag: tag.into(),
            alias: normalize(alias),
            canonical: canonical.into(),
            created_at: 0,
        }
    }

    #[test]
    fn normalize_folds_case_and_whitespace_and_edges() {
        assert_eq!(normalize("  Jane   Smith. "), "jane smith");
        assert_eq!(normalize("JANE SMITH"), "jane smith");
        assert_eq!(normalize("«Jane Smith»"), "jane smith");
        // Internal punctuation is preserved (distinct entities stay distinct).
        assert_eq!(normalize("j.smith"), "j.smith");
        assert_ne!(normalize("j.smith"), normalize("j smith"));
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn registered_surface_forms_resolve_to_canonical() {
        let aliases = vec![
            alias("t", "J. Smith", "Jane A. Smith"),
            alias("t", "jane smith", "Jane A. Smith"),
        ];
        assert_eq!(resolve("J. Smith", &aliases), "Jane A. Smith");
        // Case/spacing variants of a registered alias resolve too.
        assert_eq!(resolve("  JANE   SMITH ", &aliases), "Jane A. Smith");
    }

    #[test]
    fn unregistered_name_resolves_to_itself() {
        let aliases = vec![alias("t", "jane smith", "Jane A. Smith")];
        // Never invents a merge — an unknown entity is returned as-is (trimmed).
        assert_eq!(resolve("  Bob Jones ", &aliases), "Bob Jones");
        assert_eq!(resolve("Bob Jones", &[]), "Bob Jones");
    }
}
