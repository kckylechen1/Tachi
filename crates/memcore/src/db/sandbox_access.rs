//! Pure sandbox decisions over host-resolved rules; no policy storage or grants.

/// Pure decision over pre-fetched rules: "deny" overrides everything; among non-deny rules
/// the most specific match (longest pattern) wins; default = allow if no rule matches.
/// `rules` must be ordered by specificity descending (longest pattern first) — i.e. the order
/// produced by `list_sandbox_rules_for_role`.
pub fn evaluate_sandbox_access(
    rules: &[(String, String)],
    agent_role: &str,
    path: &str,
    operation: &str,
) -> (bool, Option<String>) {
    let mut best_non_deny: Option<(String, String, usize)> = None;

    for (pattern, access_level) in rules {
        if path_matches_pattern(path, pattern) {
            // deny overrides everything — return immediately
            if access_level == "deny" {
                let rule_desc = format!("{}:{} -> deny", agent_role, pattern);
                return (false, Some(rule_desc));
            }
            // Track best non-deny match by specificity
            let specificity = pattern.len();
            let is_better = match &best_non_deny {
                None => true,
                Some((_, _, best_spec)) => specificity > *best_spec,
            };
            if is_better {
                best_non_deny = Some((pattern.clone(), access_level.clone(), specificity));
            }
        }
    }

    match best_non_deny {
        None => {
            // No rule matches — default: allow
            (true, None)
        }
        Some((pattern, access_level, _)) => {
            let rule_desc = format!("{}:{} -> {}", agent_role, pattern, access_level);
            if operation == "write" && access_level == "read" {
                (false, Some(rule_desc))
            } else {
                (true, Some(rule_desc))
            }
        }
    }
}

/// Simple path pattern matching.
/// Supports:
/// - Exact match: "/domain-pack/reports" matches "/domain-pack/reports"
/// - Wildcard suffix: "/domain-pack/*" matches "/domain-pack/anything"
/// - Prefix match: "/domain-pack" matches "/domain-pack" and "/domain-pack/sub"
pub fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    if pattern == "*" || pattern == "/*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        // Wildcard: matches prefix and any sub-paths
        path == prefix || path.starts_with(&format!("{}/", prefix))
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        // Glob-style: "/foo*" matches anything starting with "/foo"
        path.starts_with(prefix)
    } else {
        // Exact match or prefix match
        path == pattern || path.starts_with(&format!("{}/", pattern))
    }
}
