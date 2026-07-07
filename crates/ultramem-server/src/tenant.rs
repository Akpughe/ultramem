//! Credential → tag binding (Sprint 1A stop-ship fix SS-1).
//!
//! Before this, the server trusted a single static API key and let the client
//! pick any `container_tag` — so any key-holder could read/write/delete any
//! tenant by changing a string. Here, a credential resolves to a [`TagPolicy`]
//! that constrains which namespaces it may touch, and every handler derives its
//! tag through [`TenantCtx::resolve_tag`] instead of trusting the request.
//!
//! This is deliberately a *seam*, not the final design: the policy map is loaded
//! from env (`ULTRAMEM_TENANTS`, `ULTRAMEM_API_KEY`). A later sprint can replace
//! `AuthConfig` with JWT claims or a database-backed tenant table without
//! touching the handlers, which only ever see a `TenantCtx`.

use std::collections::HashMap;

use ultramem_core::DEFAULT_TAG;

/// Which namespaces a credential may act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagPolicy {
    /// May use any `container_tag` (a trusted backend that manages its own
    /// per-user tags). This is the backward-compatible policy for a bare
    /// `ULTRAMEM_API_KEY`.
    Any,
    /// May use only these tags; the first is the credential's default when the
    /// request omits `container_tag`. Never empty.
    Only(Vec<String>),
}

impl TagPolicy {
    /// Resolve the effective tag for a request, or `Err` if the requested tag is
    /// outside this policy (the handler maps that to `403`).
    ///
    /// - explicit non-empty tag → allowed only if the policy permits it;
    /// - omitted/empty tag → the policy's default.
    pub fn resolve(&self, requested: &Option<String>) -> Result<String, TagDenied> {
        let asked = requested
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match self {
            TagPolicy::Any => Ok(asked.unwrap_or(DEFAULT_TAG).to_string()),
            TagPolicy::Only(tags) => match asked {
                Some(t) if tags.iter().any(|a| a == t) => Ok(t.to_string()),
                Some(_) => Err(TagDenied),
                None => Ok(tags[0].clone()),
            },
        }
    }
}

/// The requested `container_tag` is not permitted for the credential.
#[derive(Debug, PartialEq, Eq)]
pub struct TagDenied;

/// Per-request identity injected by the auth middleware and read by handlers.
#[derive(Clone, Debug)]
pub struct TenantCtx {
    policy: TagPolicy,
}

impl TenantCtx {
    pub fn new(policy: TagPolicy) -> Self {
        Self { policy }
    }
    /// A wide-open context (dev mode with no keys configured).
    pub fn any() -> Self {
        Self {
            policy: TagPolicy::Any,
        }
    }
    /// Resolve the effective, authorized tag for this request.
    pub fn resolve_tag(&self, requested: &Option<String>) -> Result<String, TagDenied> {
        self.policy.resolve(requested)
    }
}

/// Server auth configuration: which credentials exist and what each may touch.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    keys: HashMap<String, TagPolicy>,
    dev: bool,
}

impl AuthConfig {
    /// Build from process env. Sources, in precedence order:
    /// - `ULTRAMEM_TENANTS`: `key=tag1,tag2` entries separated by `;` or newline;
    ///   a tag of `*` means [`TagPolicy::Any`]. An entry with no tags (`key=`) is
    ///   dropped (fail closed — a typo must not grant wildcard access). This is
    ///   the multi-tenant path.
    /// - `ULTRAMEM_API_KEY`: if set and not already present in `ULTRAMEM_TENANTS`,
    ///   it is added with [`TagPolicy::Any`] (backward compatible — one trusted
    ///   key that manages its own tags, as the quickstart shows).
    /// - `ULTRAMEM_DEV=1`: permits running with no keys (unauthenticated).
    pub fn from_env() -> Self {
        Self::build(
            std::env::var("ULTRAMEM_TENANTS").ok().as_deref(),
            std::env::var("ULTRAMEM_API_KEY").ok().as_deref(),
            std::env::var("ULTRAMEM_DEV").as_deref() == Ok("1"),
        )
    }

    /// Pure constructor (testable without touching the environment).
    pub fn build(tenants: Option<&str>, api_key: Option<&str>, dev: bool) -> Self {
        let mut keys = HashMap::new();
        if let Some(spec) = tenants {
            for entry in spec.split(['\n', ';']) {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                let Some((key, tags)) = entry.split_once('=') else {
                    continue;
                };
                let key = key.trim();
                if key.is_empty() {
                    continue;
                }
                let tags: Vec<String> = tags
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(String::from)
                    .collect();
                let policy = if tags.iter().any(|t| t == "*") {
                    TagPolicy::Any
                } else if tags.is_empty() {
                    // Malformed entry (`key=` with no tags): fail CLOSED — leave the
                    // key unregistered so it grants nothing, rather than defaulting
                    // to wildcard access. A typo must never widen access.
                    continue;
                } else {
                    TagPolicy::Only(tags)
                };
                keys.insert(key.to_string(), policy);
            }
        }
        if let Some(k) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
            keys.entry(k.to_string()).or_insert(TagPolicy::Any);
        }
        Self { keys, dev }
    }

    /// True when no credentials are configured *and* dev mode is on — the server
    /// runs unauthenticated. Non-dev empty configs are rejected at startup, so
    /// this can only be reached deliberately.
    pub fn is_open(&self) -> bool {
        self.keys.is_empty()
    }

    /// True when the config is unusable in production: no keys and not dev.
    /// `main` refuses to start in this state.
    pub fn is_misconfigured(&self) -> bool {
        self.keys.is_empty() && !self.dev
    }

    /// Resolve a Bearer token to a per-request context, or `None` if the token is
    /// missing/unknown (the middleware maps that to `401`). Uses a constant-time
    /// comparison so a valid key can't be discovered by timing.
    pub fn resolve(&self, bearer: Option<&str>) -> Option<TenantCtx> {
        let token = bearer?;
        for (key, policy) in &self.keys {
            if ct_eq(key.as_bytes(), token.as_bytes()) {
                return Some(TenantCtx::new(policy.clone()));
            }
        }
        None
    }
}

/// Constant-time byte-slice equality (length is allowed to leak; contents are
/// not). Avoids a timing side-channel on the API key comparison.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_policy_allows_bound_tag() {
        let p = TagPolicy::Only(vec!["tenant_a".into()]);
        assert_eq!(p.resolve(&Some("tenant_a".into())), Ok("tenant_a".into()));
    }

    #[test]
    fn only_policy_denies_other_tag() {
        // Tenant escalation: key bound to tenant_a asks for tenant_b → denied.
        let p = TagPolicy::Only(vec!["tenant_a".into()]);
        assert_eq!(p.resolve(&Some("tenant_b".into())), Err(TagDenied));
    }

    #[test]
    fn only_policy_defaults_to_first_tag() {
        let p = TagPolicy::Only(vec!["tenant_a".into(), "tenant_b".into()]);
        assert_eq!(p.resolve(&None), Ok("tenant_a".into()));
        assert_eq!(p.resolve(&Some(String::new())), Ok("tenant_a".into()));
    }

    #[test]
    fn any_policy_passes_through_and_defaults() {
        let p = TagPolicy::Any;
        assert_eq!(p.resolve(&Some("whatever".into())), Ok("whatever".into()));
        assert_eq!(p.resolve(&None), Ok(DEFAULT_TAG.to_string()));
    }

    #[test]
    fn tenants_spec_parses_bound_and_wildcard() {
        let cfg = AuthConfig::build(Some("ka=tenant_a,shared; kb=*"), None, false);
        assert_eq!(
            cfg.resolve(Some("ka"))
                .unwrap()
                .resolve_tag(&Some("tenant_a".into())),
            Ok("tenant_a".into())
        );
        // ka is not allowed the "other" tag.
        assert_eq!(
            cfg.resolve(Some("ka"))
                .unwrap()
                .resolve_tag(&Some("other".into())),
            Err(TagDenied)
        );
        // kb is a wildcard backend.
        assert_eq!(
            cfg.resolve(Some("kb"))
                .unwrap()
                .resolve_tag(&Some("anything".into())),
            Ok("anything".into())
        );
    }

    #[test]
    fn empty_tag_list_fails_closed() {
        // A malformed `key=` (no tags) must NOT become a wildcard. The key is left
        // unregistered so it grants nothing; a valid entry alongside it survives.
        let cfg = AuthConfig::build(Some("good=tenant_a; bad="), None, false);
        assert!(cfg.resolve(Some("good")).is_some());
        assert!(
            cfg.resolve(Some("bad")).is_none(),
            "empty-tag entry must not be a usable (wildcard) credential"
        );
        // A config with only a malformed entry has no usable keys.
        assert!(AuthConfig::build(Some("k="), None, false).is_misconfigured());
    }

    #[test]
    fn bare_api_key_is_wildcard_for_backward_compat() {
        let cfg = AuthConfig::build(None, Some("secret"), false);
        let ctx = cfg.resolve(Some("secret")).expect("known key");
        assert_eq!(
            ctx.resolve_tag(&Some("user_123".into())),
            Ok("user_123".into())
        );
        assert!(cfg.resolve(Some("wrong")).is_none());
        assert!(cfg.resolve(None).is_none());
    }

    #[test]
    fn empty_config_without_dev_is_misconfigured() {
        assert!(AuthConfig::build(None, None, false).is_misconfigured());
        assert!(!AuthConfig::build(None, None, true).is_misconfigured());
        assert!(AuthConfig::build(None, None, true).is_open());
        assert!(!AuthConfig::build(None, Some("k"), false).is_misconfigured());
    }

    #[test]
    fn ct_eq_matches_only_equal() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }
}
