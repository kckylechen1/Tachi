//! Path normalization and routing rules for memory storage.
//!
//! Canonical path conventions:
//!   /handoff/{agent_id}   - handoff memos (project-local by default;
//!                           handoff_ops opts into cross-project for global DB)
//!   /kanban/{board}       - kanban cards (project-local by default;
//!                           kanban opts into cross-project for global DB)
//!   /wiki/{category}/...  - wiki entries (wiki DB only)
//!   /agents/{agent_id}/.. - agent state
//!   /domain/{domain}/...  - domain-scoped memories
//!   /                     - root (uncategorized)
//!
//! See docs/audit-2026-04-30.md (PR-3) — fixes B4, B6, B11, H1, H6.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRouting {
    /// Path belongs in the project DB (default).
    Project,
    /// Path belongs in the wiki DB only.
    WikiOnly,
    /// Path is global-only (none currently classified this way; reserved).
    /// Per audit B11, handoff/kanban are normally project-local; system-wide
    /// placements require explicit `allow_cross_project`.
    Global,
    /// Path is acceptable in any DB.
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRoutingError {
    /// A `/wiki/...` path was written to a non-wiki DB.
    WikiPathInNonWikiDb { path: String, db_label: String },
    /// A project-scoped path was written to the global DB without opt-in.
    /// Reserved for a future stricter routing mode; not currently emitted.
    #[allow(dead_code)]
    ProjectPathInGlobal { path: String, db_label: String },
}

impl fmt::Display for PathRoutingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathRoutingError::WikiPathInNonWikiDb { path, db_label } => write!(
                f,
                "wiki path {:?} cannot be written to non-wiki DB {:?} \
                 (set metadata.allow_cross_project=true to override)",
                path, db_label
            ),
            PathRoutingError::ProjectPathInGlobal { path, db_label } => write!(
                f,
                "project path {:?} cannot be written to global DB {:?} \
                 (set metadata.allow_cross_project=true to override)",
                path, db_label
            ),
        }
    }
}

impl std::error::Error for PathRoutingError {}

/// Normalize a memory path. Idempotent.
///
/// Rules:
/// - Trim trailing whitespace.
/// - Reject empty → `/`.
/// - Add leading slash if missing.
/// - Collapse repeated slashes.
/// - Strip trailing slash unless path is `/`.
/// - Lowercase the first segment after `/` (preserve case-sensitivity below).
pub fn normalize_path(p: &str) -> String {
    let trimmed = p.trim();
    if trimmed.is_empty() {
        return "/".to_string();
    }

    // Ensure leading slash, then collapse repeated slashes.
    let with_lead: String = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };

    let mut collapsed = String::with_capacity(with_lead.len());
    let mut last_was_slash = false;
    for ch in with_lead.chars() {
        if ch == '/' {
            if !last_was_slash {
                collapsed.push('/');
            }
            last_was_slash = true;
        } else {
            collapsed.push(ch);
            last_was_slash = false;
        }
    }

    // Strip trailing slash (but keep root "/").
    let stripped = if collapsed.len() > 1 {
        collapsed.trim_end_matches('/').to_string()
    } else {
        collapsed
    };

    if stripped.is_empty() {
        return "/".to_string();
    }

    // Lowercase only the first segment after the leading slash.
    if let Some(rest) = stripped.strip_prefix('/') {
        if rest.is_empty() {
            return "/".to_string();
        }
        match rest.find('/') {
            Some(idx) => {
                let head = rest[..idx].to_lowercase();
                let tail = &rest[idx..]; // starts with '/'
                format!("/{head}{tail}")
            }
            None => format!("/{}", rest.to_lowercase()),
        }
    } else {
        stripped
    }
}

/// Classify a path's intended routing.
pub fn classify_path(p: &str) -> PathRouting {
    let n = normalize_path(p);
    if n == "/wiki" || n.starts_with("/wiki/") {
        return PathRouting::WikiOnly;
    }
    if n == "/handoff" || n.starts_with("/handoff/") {
        return PathRouting::Project;
    }
    if n == "/agents" || n.starts_with("/agents/") {
        return PathRouting::Project;
    }
    if n == "/kanban" || n.starts_with("/kanban/") {
        return PathRouting::Project;
    }
    if n.starts_with("/domain/") {
        return PathRouting::Any;
    }
    PathRouting::Project
}

/// Validate that `path` is permitted in DB labelled `db_label`.
///
/// `allow_cross_project=true` bypasses all rejections (used by handoff/kanban
/// subsystems that intentionally write project-classified paths to global).
pub fn validate_path_for_db(
    path: &str,
    db_label: &str,
    allow_cross_project: bool,
) -> Result<(), PathRoutingError> {
    if allow_cross_project {
        return Ok(());
    }
    let normalized = normalize_path(path);
    match classify_path(&normalized) {
        PathRouting::WikiOnly => {
            if db_label != "wiki" {
                return Err(PathRoutingError::WikiPathInNonWikiDb {
                    path: normalized,
                    db_label: db_label.to_string(),
                });
            }
        }
        // The global DB is the canonical home for /handoff, /kanban, /agents,
        // /openclaw, /project, and uncategorized paths. Project-routed rules
        // are enforced primarily by migration v4 (cross-DB quarantine using
        // provenance evidence). At write time we only enforce wiki isolation
        // here; per-project routing would require a project registry.
        PathRouting::Global | PathRouting::Any | PathRouting::Project => {}
    }
    Ok(())
}

/// Build the canonical handoff path for an agent, defaulting to `/handoff/unknown`.
pub fn standardize_handoff_path(agent_id: Option<&str>) -> String {
    let id = agent_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    format!("/handoff/{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_idempotent() {
        for p in [
            "/",
            "/wiki/foo",
            "/wiki/Foo/Bar",
            "/handoff/agent-1",
            "/agents/X/notes",
        ] {
            let once = normalize_path(p);
            let twice = normalize_path(&once);
            assert_eq!(once, twice, "not idempotent for {p}");
        }
    }

    #[test]
    fn normalize_collapses_double_slash() {
        assert_eq!(normalize_path("/wiki//foo//bar"), "/wiki/foo/bar");
        assert_eq!(normalize_path("///wiki///foo///"), "/wiki/foo");
    }

    #[test]
    fn normalize_injects_leading_slash() {
        assert_eq!(normalize_path("wiki/foo"), "/wiki/foo");
        assert_eq!(normalize_path("kanban"), "/kanban");
    }

    #[test]
    fn normalize_strips_trailing_slash_except_root() {
        assert_eq!(normalize_path("/wiki/"), "/wiki");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path(""), "/");
        assert_eq!(normalize_path("   "), "/");
    }

    #[test]
    fn normalize_lowercases_only_first_segment() {
        assert_eq!(normalize_path("/Wiki/Foo"), "/wiki/Foo");
        assert_eq!(normalize_path("/HANDOFF/Agent-X"), "/handoff/Agent-X");
        assert_eq!(
            normalize_path("/AGENTS/CamelCase/Sub"),
            "/agents/CamelCase/Sub"
        );
    }

    #[test]
    fn classify_path_matrix() {
        assert_eq!(classify_path("/wiki"), PathRouting::WikiOnly);
        assert_eq!(classify_path("/wiki/foo"), PathRouting::WikiOnly);
        assert_eq!(classify_path("/Wiki/foo"), PathRouting::WikiOnly);
        assert_eq!(classify_path("/handoff"), PathRouting::Project);
        assert_eq!(classify_path("/handoff/agent-x"), PathRouting::Project);
        assert_eq!(classify_path("/kanban/board-1"), PathRouting::Project);
        assert_eq!(classify_path("/agents/x"), PathRouting::Project);
        assert_eq!(classify_path("/domain/finance/notes"), PathRouting::Any);
        assert_eq!(classify_path("/foo/bar"), PathRouting::Project);
        assert_eq!(classify_path("/"), PathRouting::Project);
    }

    #[test]
    fn validate_rejects_wiki_in_non_wiki_db() {
        let err = validate_path_for_db("/wiki/foo", "hapi", false).unwrap_err();
        matches!(err, PathRoutingError::WikiPathInNonWikiDb { .. });
    }

    #[test]
    fn validate_allows_project_paths_in_global_db() {
        // Global DB is the catch-all for /handoff, /kanban, /agents, /openclaw,
        // /project, etc. Per-project pollution is enforced by migration v4
        // using provenance evidence, not by this write-time check.
        assert!(validate_path_for_db("/foo/bar", "global", false).is_ok());
        assert!(validate_path_for_db("/handoff/agent", "global", false).is_ok());
        assert!(validate_path_for_db("/kanban/board", "global", false).is_ok());
        assert!(validate_path_for_db("/", "global", false).is_ok());
    }

    #[test]
    fn validate_allows_wiki_in_wiki() {
        assert!(validate_path_for_db("/wiki/foo", "wiki", false).is_ok());
    }

    #[test]
    fn validate_allows_project_in_project() {
        assert!(validate_path_for_db("/foo/bar", "hapi", false).is_ok());
        assert!(validate_path_for_db("/handoff/x", "hapi", false).is_ok());
    }

    #[test]
    fn validate_allows_any_in_any() {
        assert!(validate_path_for_db("/domain/finance/x", "global", false).is_ok());
        assert!(validate_path_for_db("/domain/finance/x", "wiki", false).is_ok());
    }

    #[test]
    fn validate_allow_cross_project_bypass() {
        assert!(validate_path_for_db("/wiki/foo", "hapi", true).is_ok());
        assert!(validate_path_for_db("/foo/bar", "global", true).is_ok());
    }

    #[test]
    fn standardize_handoff_path_defaults_unknown() {
        assert_eq!(standardize_handoff_path(None), "/handoff/unknown");
        assert_eq!(standardize_handoff_path(Some("")), "/handoff/unknown");
        assert_eq!(standardize_handoff_path(Some("   ")), "/handoff/unknown");
        assert_eq!(
            standardize_handoff_path(Some("agent-x")),
            "/handoff/agent-x"
        );
    }
}
