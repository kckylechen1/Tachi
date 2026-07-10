//! F0 facade action inventory (#913 / #495).
//!
//! Machine-checkable lists of primary MCP facade actions so action-count
//! regressions are visible. GitHub PR lifecycle actions live only on
//! `tachi_gh` (canonical); `tachi_task` keeps them as compatibility aliases
//! at the router layer but **not** in the primary schema enum after F2.

/// Primary `tachi_task` actions advertised in the MCP schema (F2: no GH lifecycle).
pub const TACHI_TASK_PRIMARY_ACTIONS: &[&str] = &[
    "plan",
    "briefing",
    "doc_index",
    "recommend",
    "dispatch",
    "complete",
    "profiles",
    "profile",
    "card",
    "route_simulate",
    "proposals",
    "review_proposal",
    "apply_proposals",
    "status",
    "cancel",
    "board",
    "wait",
    "merge",
    "intake",
    "cycle_status",
    "cycle_plan",
    "ux_matrix",
    "build_references",
    "close_loop",
];

/// Compatibility-only `tachi_task` actions (still accepted by the router; use `tachi_gh`).
pub const TACHI_TASK_COMPAT_GH_LIFECYCLE_ACTIONS: &[&str] =
    &["link_pr", "pr_status", "pr_handoff", "release_note"];

/// Canonical `tachi_gh` actions (includes lifecycle).
pub const TACHI_GH_ACTIONS: &[&str] = &[
    "repo_view",
    "issue_list",
    "issue_read",
    "issue_create",
    "issue_comment",
    "pr_list",
    "pr_read",
    "pr_comments",
    "pr_comment",
    "pr_review_digest",
    "safe_merge",
    "ship",
    "link_pr",
    "pr_status",
    "pr_handoff",
    "release_note",
];

/// `tachi_memory` facade actions.
pub const TACHI_MEMORY_ACTIONS: &[&str] = &[
    "search",
    "get",
    "save",
    "extract_facts",
    "briefing",
    "checkpoint",
    "alerts",
    "ask",
    "consolidate",
    "recall_simulate",
    "recall_proposals",
    "review_recall_proposal",
    "apply_recall_proposals",
    "pattern_feedback",
    "progress",
    "readiness",
    // #757 fold: standalone memory-admin + pipeline tools re-fronted as actions.
    "delete",
    "gc",
    "doctor_scan",
    "ingest",
    "ingest_source",
];

/// `tachi_verify` actions.
pub const TACHI_VERIFY_ACTIONS: &[&str] = &["start", "record", "status", "board"];

/// Soft ceilings for F0 monitoring (primary schema actions only).
pub const TACHI_TASK_PRIMARY_ACTION_SOFT_MAX: usize = 28;
pub const TACHI_MEMORY_ACTION_SOFT_MAX: usize = 25;
pub const TACHI_GH_ACTION_SOFT_MAX: usize = 20;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f0_task_primary_does_not_advertise_gh_lifecycle() {
        for action in TACHI_TASK_COMPAT_GH_LIFECYCLE_ACTIONS {
            assert!(
                !TACHI_TASK_PRIMARY_ACTIONS.contains(action),
                "primary task schema must not advertise {action}; use tachi_gh"
            );
        }
        assert_eq!(TACHI_TASK_PRIMARY_ACTIONS.len(), 24);
        assert!(TACHI_TASK_PRIMARY_ACTIONS.len() <= TACHI_TASK_PRIMARY_ACTION_SOFT_MAX);
    }

    #[test]
    fn f0_gh_owns_lifecycle_actions() {
        for action in TACHI_TASK_COMPAT_GH_LIFECYCLE_ACTIONS {
            assert!(
                TACHI_GH_ACTIONS.contains(action),
                "tachi_gh must own lifecycle action {action}"
            );
        }
        assert_eq!(TACHI_GH_ACTIONS.len(), 16);
        assert!(TACHI_GH_ACTIONS.len() <= TACHI_GH_ACTION_SOFT_MAX);
    }

    #[test]
    fn f0_memory_and_verify_counts() {
        assert_eq!(TACHI_MEMORY_ACTIONS.len(), 21);
        assert!(TACHI_MEMORY_ACTIONS.len() <= TACHI_MEMORY_ACTION_SOFT_MAX);
        assert_eq!(TACHI_VERIFY_ACTIONS.len(), 4);
    }
}
