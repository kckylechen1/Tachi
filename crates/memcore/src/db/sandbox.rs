use rusqlite::{params, Connection};

use crate::error::MemoryError;

use super::common::now_utc_iso;

// ─── Semantic Sandboxing (Role-Based Memory Isolation) ───────────────────────

/// Set (upsert) a sandbox access rule for a given agent role and path pattern.
pub fn set_sandbox_rule(
    conn: &Connection,
    agent_role: &str,
    path_pattern: &str,
    access_level: &str,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "INSERT INTO sandbox_rules (agent_role, path_pattern, access_level, created_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(agent_role, path_pattern) DO UPDATE SET
             access_level = excluded.access_level,
             created_at = excluded.created_at",
        params![agent_role, path_pattern, access_level, &now],
    )?;
    Ok(())
}

/// Check if an agent role can access a given path for a specific operation.
/// Returns (allowed, matching_rule_description).
/// Logic: "deny" overrides everything (checked across ALL matching rules).
/// Among non-deny rules, the most specific match wins. Default = allow if no rule matches.
///
/// Composed of [`list_sandbox_rules_for_role`] (the SQL fetch) + [`evaluate_sandbox_access`]
/// (the pure decision) so callers that check many paths for the same role in a loop can fetch
/// the rules once and reuse the in-memory evaluation, avoiding an N+1 query per result row.
pub fn check_sandbox_access(
    conn: &Connection,
    agent_role: &str,
    path: &str,
    operation: &str,
) -> Result<(bool, Option<String>), MemoryError> {
    let rules = list_sandbox_rules_for_role(conn, agent_role)?;
    Ok(evaluate_sandbox_access(&rules, agent_role, path, operation))
}

/// Fetch all `(path_pattern, access_level)` rules for `agent_role`, ordered by specificity
/// (longest pattern first, then alphabetical) — the same ordering [`evaluate_sandbox_access`]
/// relies on to pick the most specific match.
pub fn list_sandbox_rules_for_role(
    conn: &Connection,
    agent_role: &str,
) -> Result<Vec<(String, String)>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT path_pattern, access_level FROM sandbox_rules WHERE agent_role = ?1 ORDER BY LENGTH(path_pattern) DESC, path_pattern ASC"
    )?;
    let rows = stmt.query_map(params![agent_role], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Pure decision over pre-fetched rules: "deny" overrides everything; among non-deny rules
/// the most specific match (longest pattern) wins; default = allow if no rule matches.
/// `rules` must be ordered by specificity descending (longest pattern first) — i.e. the order
/// produced by [`list_sandbox_rules_for_role`].
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

fn parse_json_text(text: &str, fallback: serde_json::Value) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or(fallback)
}

/// Upsert sandbox runtime policy for a capability (typically an MCP server).
#[allow(clippy::too_many_arguments)]
pub fn set_sandbox_policy(
    conn: &Connection,
    capability_id: &str,
    runtime_type: &str,
    env_allowlist_json: &str,
    fs_read_roots_json: &str,
    fs_write_roots_json: &str,
    cwd_roots_json: &str,
    max_startup_ms: u64,
    max_tool_ms: u64,
    max_concurrency: u32,
    enabled: bool,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "INSERT INTO sandbox_policies
         (capability_id, runtime_type, env_allowlist, fs_read_roots, fs_write_roots, cwd_roots,
          max_startup_ms, max_tool_ms, max_concurrency, enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
         ON CONFLICT(capability_id) DO UPDATE SET
           runtime_type = excluded.runtime_type,
           env_allowlist = excluded.env_allowlist,
           fs_read_roots = excluded.fs_read_roots,
           fs_write_roots = excluded.fs_write_roots,
           cwd_roots = excluded.cwd_roots,
           max_startup_ms = excluded.max_startup_ms,
           max_tool_ms = excluded.max_tool_ms,
           max_concurrency = excluded.max_concurrency,
           enabled = excluded.enabled,
           updated_at = excluded.updated_at",
        params![
            capability_id,
            runtime_type,
            env_allowlist_json,
            fs_read_roots_json,
            fs_write_roots_json,
            cwd_roots_json,
            max_startup_ms as i64,
            max_tool_ms as i64,
            max_concurrency as i64,
            enabled as i32,
            &now,
        ],
    )?;
    Ok(())
}

/// Fetch sandbox runtime policy by capability id.
pub fn get_sandbox_policy(
    conn: &Connection,
    capability_id: &str,
) -> Result<Option<serde_json::Value>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT capability_id, runtime_type, env_allowlist, fs_read_roots, fs_write_roots,
                cwd_roots, max_startup_ms, max_tool_ms, max_concurrency, enabled, created_at, updated_at
         FROM sandbox_policies
         WHERE capability_id = ?1",
    )?;
    let result = stmt.query_row(params![capability_id], |row| {
        let env_allowlist: String = row.get(2)?;
        let fs_read_roots: String = row.get(3)?;
        let fs_write_roots: String = row.get(4)?;
        let cwd_roots: String = row.get(5)?;
        Ok(serde_json::json!({
            "capability_id": row.get::<_, String>(0)?,
            "runtime_type": row.get::<_, String>(1)?,
            "env_allowlist": parse_json_text(&env_allowlist, serde_json::json!([])),
            "fs_read_roots": parse_json_text(&fs_read_roots, serde_json::json!([])),
            "fs_write_roots": parse_json_text(&fs_write_roots, serde_json::json!([])),
            "cwd_roots": parse_json_text(&cwd_roots, serde_json::json!([])),
            "max_startup_ms": row.get::<_, i64>(6)?,
            "max_tool_ms": row.get::<_, i64>(7)?,
            "max_concurrency": row.get::<_, i64>(8)?,
            "enabled": row.get::<_, i32>(9)? != 0,
            "created_at": row.get::<_, String>(10)?,
            "updated_at": row.get::<_, String>(11)?,
        }))
    });
    match result {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(MemoryError::from(e)),
    }
}

/// List sandbox runtime policies.
pub fn list_sandbox_policies(
    conn: &Connection,
    enabled_only: bool,
    limit: usize,
) -> Result<Vec<serde_json::Value>, MemoryError> {
    let mut sql = String::from(
        "SELECT capability_id, runtime_type, env_allowlist, fs_read_roots, fs_write_roots,
                cwd_roots, max_startup_ms, max_tool_ms, max_concurrency, enabled, created_at, updated_at
         FROM sandbox_policies",
    );
    if enabled_only {
        sql.push_str(" WHERE enabled = 1");
    }
    sql.push_str(" ORDER BY capability_id ASC LIMIT ?1");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        let env_allowlist: String = row.get(2)?;
        let fs_read_roots: String = row.get(3)?;
        let fs_write_roots: String = row.get(4)?;
        let cwd_roots: String = row.get(5)?;
        Ok(serde_json::json!({
            "capability_id": row.get::<_, String>(0)?,
            "runtime_type": row.get::<_, String>(1)?,
            "env_allowlist": parse_json_text(&env_allowlist, serde_json::json!([])),
            "fs_read_roots": parse_json_text(&fs_read_roots, serde_json::json!([])),
            "fs_write_roots": parse_json_text(&fs_write_roots, serde_json::json!([])),
            "cwd_roots": parse_json_text(&cwd_roots, serde_json::json!([])),
            "max_startup_ms": row.get::<_, i64>(6)?,
            "max_tool_ms": row.get::<_, i64>(7)?,
            "max_concurrency": row.get::<_, i64>(8)?,
            "enabled": row.get::<_, i32>(9)? != 0,
            "created_at": row.get::<_, String>(10)?,
            "updated_at": row.get::<_, String>(11)?,
        }))
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Insert one sandbox execution audit row.
#[allow(clippy::too_many_arguments)]
pub fn insert_sandbox_exec_audit(
    conn: &Connection,
    timestamp: &str,
    capability_id: &str,
    stage: &str,
    decision: &str,
    reason: Option<&str>,
    duration_ms: u64,
    tool_name: Option<&str>,
    error_kind: Option<&str>,
    metadata_json: &str,
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO sandbox_exec_audit
         (timestamp, capability_id, stage, decision, reason, duration_ms, tool_name, error_kind, metadata, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?1)",
        params![
            timestamp,
            capability_id,
            stage,
            decision,
            reason,
            duration_ms as i64,
            tool_name,
            error_kind,
            metadata_json,
        ],
    )?;
    Ok(())
}

/// List recent sandbox execution audit rows.
pub fn list_sandbox_exec_audit(
    conn: &Connection,
    capability_id: Option<&str>,
    stage: Option<&str>,
    decision: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>, MemoryError> {
    let mut sql = String::from(
        "SELECT timestamp, capability_id, stage, decision, reason, duration_ms, tool_name, error_kind, metadata
         FROM sandbox_exec_audit",
    );
    let mut clauses: Vec<&str> = Vec::new();
    let mut params_buf: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(v) = capability_id {
        clauses.push("capability_id = ?");
        params_buf.push(Box::new(v.to_string()));
    }
    if let Some(v) = stage {
        clauses.push("stage = ?");
        params_buf.push(Box::new(v.to_string()));
    }
    if let Some(v) = decision {
        clauses.push("decision = ?");
        params_buf.push(Box::new(v.to_string()));
    }
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY timestamp DESC LIMIT ?");
    params_buf.push(Box::new(limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_buf.iter().map(|v| v.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), |row| {
        let metadata_text: String = row.get(8)?;
        Ok(serde_json::json!({
            "timestamp": row.get::<_, String>(0)?,
            "capability_id": row.get::<_, String>(1)?,
            "stage": row.get::<_, String>(2)?,
            "decision": row.get::<_, String>(3)?,
            "reason": row.get::<_, Option<String>>(4)?,
            "duration_ms": row.get::<_, i64>(5)?,
            "tool_name": row.get::<_, Option<String>>(6)?,
            "error_kind": row.get::<_, Option<String>>(7)?,
            "metadata": parse_json_text(&metadata_text, serde_json::json!({})),
        }))
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── path_matches_pattern equivalence classes ────────────────────────────

    #[test]
    fn path_matches_wildcard_root() {
        assert!(path_matches_pattern("/anything", "*"));
        assert!(path_matches_pattern("/anything", "/*"));
    }

    #[test]
    fn path_matches_glob_suffix() {
        assert!(path_matches_pattern("/foo/bar", "/foo/*"));
        assert!(path_matches_pattern("/foo", "/foo/*"));
        assert!(!path_matches_pattern("/foobar", "/foo/*"));
    }

    #[test]
    fn path_matches_trailing_star() {
        assert!(path_matches_pattern("/foobar", "/foo*"));
        assert!(path_matches_pattern("/foo", "/foo*"));
        assert!(!path_matches_pattern("/bar/foo", "/foo*"));
    }

    #[test]
    fn path_matches_exact_and_prefix() {
        assert!(path_matches_pattern("/x", "/x"));
        assert!(path_matches_pattern("/x/y", "/x"));
        assert!(!path_matches_pattern("/xy", "/x"));
    }

    // ── evaluate_sandbox_access equivalence classes ────────────────────────
    // Rules are ordered longest-pattern-first, matching list_sandbox_rules_for_role's ORDER BY.

    const ROLE: &str = "tester";

    #[test]
    fn evaluate_no_rules_defaults_to_allow() {
        assert_eq!(
            evaluate_sandbox_access(&[], ROLE, "/secret/x", "read"),
            (true, None)
        );
    }

    #[test]
    fn evaluate_deny_overrides_allow() {
        // "/secret/*" deny is more specific than "/" read — deny wins.
        let rules = vec![
            ("/secret/*".to_string(), "deny".to_string()),
            ("/".to_string(), "read".to_string()),
        ];
        let (allowed, rule) = evaluate_sandbox_access(&rules, ROLE, "/secret/x", "read");
        assert!(!allowed);
        assert_eq!(rule.as_deref(), Some("tester:/secret/* -> deny"));
    }

    #[test]
    fn evaluate_most_specific_non_deny_wins() {
        let rules = vec![
            ("/secret/reports".to_string(), "write".to_string()),
            ("/secret".to_string(), "read".to_string()),
        ];
        let (allowed, rule) = evaluate_sandbox_access(&rules, ROLE, "/secret/reports/q1", "read");
        assert!(allowed);
        assert_eq!(rule.as_deref(), Some("tester:/secret/reports -> write"));
    }

    #[test]
    fn evaluate_equal_length_tie_breaker_is_alphabetical() {
        // Same LENGTH(), ORDER BY path_pattern ASC → "/a/b" before "/a/c".
        // Strict ">" tie-breaker keeps the first-encountered (alphabetical) match.
        let rules = vec![
            ("/a/b".to_string(), "deny".to_string()),
            ("/a/c".to_string(), "read".to_string()),
        ];
        // /a/b matches its deny rule → denied.
        let (allowed, rule) = evaluate_sandbox_access(&rules, ROLE, "/a/b", "read");
        assert!(!allowed);
        assert_eq!(rule.as_deref(), Some("tester:/a/b -> deny"));
        // /a/c matches its read rule → allowed (deny rule doesn't match /a/c).
        let (allowed, rule) = evaluate_sandbox_access(&rules, ROLE, "/a/c", "read");
        assert!(allowed);
        assert_eq!(rule.as_deref(), Some("tester:/a/c -> read"));
    }

    #[test]
    fn evaluate_write_downgrades_read_rule_to_deny() {
        let rules = vec![("/data".to_string(), "read".to_string())];
        let (allowed, rule) = evaluate_sandbox_access(&rules, ROLE, "/data/x", "write");
        assert!(!allowed);
        assert_eq!(rule.as_deref(), Some("tester:/data -> read"));
    }

    #[test]
    fn evaluate_unmatched_rule_falls_through_to_allow() {
        let rules = vec![("/other".to_string(), "deny".to_string())];
        let (allowed, rule) = evaluate_sandbox_access(&rules, ROLE, "/unmatched/x", "read");
        assert!(allowed);
        assert_eq!(rule, None);
    }
}
