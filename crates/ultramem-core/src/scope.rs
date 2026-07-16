//! Multi-scope visibility (8/10 company brain) — the pure, security-critical
//! resolver, deliberately separated from I/O so it can be exhaustively tested.
//!
//! A "scope" is a `container_tag` (an individual, team, project, account, or
//! company namespace). Visibility is **fail-closed**: a principal sees ONLY
//! - its own scope (always), and
//! - scopes it has an explicit grant on (read / write / promote / admin).
//!
//! There is no implicit inheritance — a company member does not automatically
//! see every team's private memory; access is granted explicitly. This is the
//! least-privilege default. Slice 8b wires [`visible_scopes`] into the retrieval
//! filter; slice 8c administers the grants themselves via the ACL endpoints.

use crate::db::AclEntry;

/// The recognized capabilities, in ascending strength. A grant carrying anything
/// outside this set is rejected at the admin boundary (fail-closed) so a typo
/// can't create an inert or surprising grant.
pub const CAPABILITIES: [&str; 4] = ["read", "write", "promote", "admin"];

/// Whether `capability` is one the system recognizes (see [`CAPABILITIES`]).
pub fn is_valid_capability(capability: &str) -> bool {
    CAPABILITIES.contains(&capability)
}

/// Capabilities that include the right to READ a scope's memory. `write`,
/// `promote`, and `admin` all imply read; a bare unknown capability does not.
fn grants_read(capability: &str) -> bool {
    matches!(capability, "read" | "write" | "promote" | "admin")
}

/// The set of scopes (container_tags) a principal may read: its own scope plus
/// every scope it has a read-granting ACL on. Deterministic, order-stable
/// (own first, then grants in input order), de-duplicated.
pub fn visible_scopes(own_scope: &str, acls: &[AclEntry]) -> Vec<String> {
    let mut out = vec![own_scope.to_string()];
    for a in acls {
        if a.principal_reads_here() && !out.contains(&a.scope) {
            out.push(a.scope.clone());
        }
    }
    out
}

impl AclEntry {
    /// Whether this grant lets its principal read the scope.
    fn principal_reads_here(&self) -> bool {
        grants_read(&self.capability)
    }
}

/// Whether `principal` may read `scope`, given its own scope and its grants.
pub fn can_read(principal_own: &str, scope: &str, acls: &[AclEntry]) -> bool {
    visible_scopes(principal_own, acls)
        .iter()
        .any(|s| s == scope)
}

/// Whether `principal` may PROMOTE a memory INTO `scope`. Writing into a shared
/// brain is a higher bar than reading it: only an explicit `promote` or `admin`
/// grant confers it — `read`/`write` on the scope do NOT. Fail-closed. `acls`
/// are checked against `principal` so a grant to someone else never applies.
pub fn can_promote(principal: &str, scope: &str, acls: &[AclEntry]) -> bool {
    acls.iter().any(|a| {
        a.principal == principal
            && a.scope == scope
            && matches!(a.capability.as_str(), "promote" | "admin")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acl(principal: &str, scope: &str, cap: &str) -> AclEntry {
        AclEntry {
            principal: principal.into(),
            scope: scope.into(),
            capability: cap.into(),
            created_at: 0,
        }
    }

    #[test]
    fn own_scope_is_always_visible() {
        assert_eq!(visible_scopes("user_1", &[]), vec!["user_1".to_string()]);
        assert!(can_read("user_1", "user_1", &[]));
    }

    #[test]
    fn read_and_higher_grants_are_visible_lower_are_not() {
        let acls = vec![
            acl("user_1", "team_eng", "read"),
            acl("user_1", "company", "admin"), // admin implies read
            acl("user_1", "acct_x", "write"),  // write implies read
        ];
        let v = visible_scopes("user_1", &acls);
        assert!(v.contains(&"user_1".to_string()));
        assert!(v.contains(&"team_eng".to_string()));
        assert!(v.contains(&"company".to_string()));
        assert!(v.contains(&"acct_x".to_string()));
        assert!(can_read("user_1", "team_eng", &acls));
    }

    #[test]
    fn unknown_capability_does_not_grant_read() {
        let acls = vec![acl("user_1", "team_eng", "list-only-nonsense")];
        assert!(!can_read("user_1", "team_eng", &acls));
        assert_eq!(visible_scopes("user_1", &acls), vec!["user_1".to_string()]);
    }

    #[test]
    fn fail_closed_no_implicit_inheritance() {
        // A grant on the company scope does NOT reveal a sibling team's private
        // scope — only the exact scopes granted are visible.
        let acls = vec![acl("user_1", "company", "read")];
        assert!(!can_read("user_1", "team_secret", &acls));
    }

    #[test]
    fn promote_needs_promote_or_admin_grant() {
        // read/write on the scope do NOT confer promote (write into a shared
        // brain is a higher bar than reading it).
        let read_only = vec![acl("user_1", "company", "read")];
        assert!(!can_promote("user_1", "company", &read_only));
        let write_only = vec![acl("user_1", "company", "write")];
        assert!(!can_promote("user_1", "company", &write_only));

        // promote and admin both confer it.
        assert!(can_promote(
            "user_1",
            "company",
            &[acl("user_1", "company", "promote")]
        ));
        assert!(can_promote(
            "user_1",
            "company",
            &[acl("user_1", "company", "admin")]
        ));

        // A grant for a different principal or scope never applies.
        let other = vec![acl("someone_else", "company", "promote")];
        assert!(!can_promote("user_1", "company", &other));
        let wrong_scope = vec![acl("user_1", "team", "promote")];
        assert!(!can_promote("user_1", "company", &wrong_scope));
    }

    #[test]
    fn visibility_is_deduped_and_own_first() {
        let acls = vec![
            acl("user_1", "user_1", "admin"), // duplicate of own scope
            acl("user_1", "team_eng", "read"),
            acl("user_1", "team_eng", "write"), // duplicate scope, different cap
        ];
        let v = visible_scopes("user_1", &acls);
        assert_eq!(v[0], "user_1");
        assert_eq!(v.iter().filter(|s| *s == "user_1").count(), 1);
        assert_eq!(v.iter().filter(|s| *s == "team_eng").count(), 1);
    }
}
