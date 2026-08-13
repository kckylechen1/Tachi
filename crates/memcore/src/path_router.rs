//! Path normalization and routing rules for memory storage.
//!
//! Canonical path conventions:
//!   /handoff/{agent_id}   - handoff memos (project-local by default;
//!                           handoff_ops opts into cross-project for global DB)
//!   /sticky/{legacy-bucket} - retired legacy rows retained for cutover reads;
//!                             new writes fail loudly in favor of `/tachi_a2a`
//!   /kanban/{board}       - kanban cards (project-local by default;
//!                           kanban opts into cross-project for global DB)
//!   /wiki/{category}/...  - wiki entries (wiki DB only)
//!   /agents/{agent_id}/.. - agent state
//!   /domain/{domain}/...  - domain-scoped memories
//!   /                     - root (uncategorized)
//!
//! See docs/audit-2026-04-30.md (PR-3) — fixes B4, B6, B11, H1, H6.

use std::fmt;

/// Manifest label of the Wiki corpus database.
///
/// One constant for both sides of the store: the write-time routing guard
/// below and [`crate::MemoryStore::is_wiki_corpus_store`], which the read path
/// uses to decide whether a query is reading the Wiki corpus (tachi#1569).
/// Two separate literals is exactly the drift that made the read path key its
/// Wiki gate on the request's `path_prefix` instead of on the store.
pub const WIKI_CORPUS_DB_LABEL: &str = "wiki";

/// Label carried by a store opened without a manifest identity. Path-routing
/// validation is disabled for these, and every identity-keyed predicate must
/// answer "no" for them.
pub const UNKNOWN_DB_LABEL: &str = "unknown";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathRouting {
    /// Path belongs in the project DB (default).
    Project,
    /// Path belongs in the wiki DB only.
    WikiOnly,
    /// Path is acceptable in any DB.
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PathRoutingError {
    /// The legacy `/sticky` namespace is read-only historical input. New
    /// writers must use the typed A2A mailbox instead.
    RetiredStickyPath { path: String },
    /// The legacy `sticky` category is read-only historical input. New
    /// writers must use the typed A2A mailbox instead.
    RetiredStickyCategory { category: String },
    /// A `/wiki/...` path was written to a non-wiki DB.
    WikiPathInNonWikiDb { path: String, db_label: String },
}

impl fmt::Display for PathRoutingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathRoutingError::RetiredStickyPath { path } => write!(
                f,
                "legacy sticky path {:?} is retired and read-only; use tachi_a2a instead",
                path
            ),
            PathRoutingError::RetiredStickyCategory { category } => write!(
                f,
                "legacy sticky category {:?} is retired and read-only; use tachi_a2a instead",
                category
            ),
            PathRoutingError::WikiPathInNonWikiDb { path, db_label } => write!(
                f,
                "wiki path {:?} cannot be written to non-wiki DB {:?} \
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
pub(crate) fn classify_path(p: &str) -> PathRouting {
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

/// Whether `db_label` names the Wiki corpus store.
///
/// The single definition behind both the write-time routing guard and
/// [`crate::MemoryStore::is_wiki_corpus_store`]. It is an exact match on
/// [`WIKI_CORPUS_DB_LABEL`], not a prefix or suffix test: a label like
/// `named-project:wiki` is deliberately *not* the Wiki corpus, because the
/// read and write paths are required to hand the same store the same label
/// (tachi#1569) rather than teaching every consumer to parse decorations.
pub fn db_label_is_wiki_corpus(db_label: &str) -> bool {
    db_label == WIKI_CORPUS_DB_LABEL
}

/// Validate that `path` is permitted in DB labelled `db_label`.
///
/// `allow_cross_project=true` bypasses ordinary routing rejections (used by
/// handoff/kanban subsystems that intentionally write project-classified paths
/// to global). The retired `/sticky` namespace is checked before that escape
/// hatch and is never writable through this validator.
pub(crate) fn validate_path_for_db(
    path: &str,
    db_label: &str,
    allow_cross_project: bool,
) -> Result<(), PathRoutingError> {
    let normalized = normalize_path(path);
    if normalized == "/sticky" || normalized.starts_with("/sticky/") {
        return Err(PathRoutingError::RetiredStickyPath { path: normalized });
    }
    if allow_cross_project {
        return Ok(());
    }
    match classify_path(&normalized) {
        PathRouting::WikiOnly => {
            // Same predicate the read path consults through
            // `MemoryStore::is_wiki_corpus_store` (tachi#1569): one definition
            // of "this store is the Wiki corpus", consulted from both sides.
            if !db_label_is_wiki_corpus(db_label) {
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
        PathRouting::Any | PathRouting::Project => {}
    }
    Ok(())
}

/// Validate the retirement boundary shared by every ordinary memory writer.
/// This check deliberately has no store-label or policy escape hatch: legacy
/// sticky rows remain readable for cutover, but no normal writer may recreate
/// them after the A2A replacement.
pub(crate) fn validate_retired_sticky_write(
    path: &str,
    category: &str,
) -> Result<(), PathRoutingError> {
    let normalized_category = crate::types::MemoryCategory::normalize(category);
    if normalized_category == "sticky" {
        return Err(PathRoutingError::RetiredStickyCategory {
            category: normalized_category.to_string(),
        });
    }

    let normalized_path = normalize_path(path);
    if normalized_path == "/sticky" || normalized_path.starts_with("/sticky/") {
        return Err(PathRoutingError::RetiredStickyPath {
            path: normalized_path,
        });
    }
    Ok(())
}

/// Validate an ordinary memory write's path and category together. The
/// retired sticky category is checked before any cross-project or policy
/// escape hatch, just like the retired path namespace, so no generic writer
/// can resurrect the legacy producer accidentally.
pub(crate) fn validate_memory_write_for_db(
    path: &str,
    category: &str,
    db_label: &str,
    allow_cross_project: bool,
) -> Result<(), PathRoutingError> {
    validate_retired_sticky_write(path, category)?;
    validate_path_for_db(path, db_label, allow_cross_project)
}

// #1099: `standardize_handoff_path` (the `/handoff/<agent_id>` path builder)
// is retired — its only caller was `handoff_ops::memo::memo_to_memory_entry`
// (the `handoff_leave` writer), which is gone. `/handoff` path
// classification/validation below (`classify_path`, `validate_path_for_db`)
// stays: legacy `/handoff/*` rows are retained read-only and must still
// route/validate correctly.

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
        assert_eq!(classify_path("/domain/domain-pack/notes"), PathRouting::Any);
        assert_eq!(classify_path("/foo/bar"), PathRouting::Project);
        assert_eq!(classify_path("/"), PathRouting::Project);
    }

    #[test]
    fn validate_rejects_wiki_in_non_wiki_db() {
        let err = validate_path_for_db("/wiki/foo", "hapi", false).unwrap_err();
        matches!(err, PathRoutingError::WikiPathInNonWikiDb { .. });
    }

    #[test]
    fn validate_rejects_retired_sticky_paths_before_all_bypasses() {
        for path in ["/sticky", "/sticky/legacy", "//STICKY///legacy/"] {
            for (db_label, allow_cross_project) in
                [("global", false), ("wiki", true), ("unknown", false)]
            {
                let error = validate_path_for_db(path, db_label, allow_cross_project)
                    .expect_err("retired sticky paths must fail closed");
                assert!(
                    error.to_string().contains("tachi_a2a"),
                    "error must name successor: {error}"
                );
                assert!(matches!(error, PathRoutingError::RetiredStickyPath { .. }));
            }
        }
    }

    #[test]
    fn validate_rejects_retired_sticky_category_before_all_bypasses() {
        for category in ["sticky", " Sticky ", "STICKY"] {
            let error = validate_memory_write_for_db("/notes/ordinary", category, "wiki", true)
                .expect_err("retired sticky category must fail closed");
            assert!(error.to_string().contains("tachi_a2a"));
            assert!(matches!(
                error,
                PathRoutingError::RetiredStickyCategory { .. }
            ));
        }
        for path in ["/sticky", "//STICKY///legacy/"] {
            let error = validate_memory_write_for_db(path, "fact", "global", true)
                .expect_err("retired sticky path must fail before cross-project bypass");
            assert!(error.to_string().contains("tachi_a2a"));
            assert!(matches!(error, PathRoutingError::RetiredStickyPath { .. }));
        }
        for path in ["/notes", "/stickiness", "/sticky-old"] {
            assert!(validate_memory_write_for_db(path, "fact", "global", true).is_ok());
        }
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
        assert!(validate_path_for_db("/domain/domain-pack/x", "global", false).is_ok());
        assert!(validate_path_for_db("/domain/domain-pack/x", "wiki", false).is_ok());
    }

    #[test]
    fn validate_allow_cross_project_bypass() {
        assert!(validate_path_for_db("/wiki/foo", "hapi", true).is_ok());
        assert!(validate_path_for_db("/foo/bar", "global", true).is_ok());
    }
}
