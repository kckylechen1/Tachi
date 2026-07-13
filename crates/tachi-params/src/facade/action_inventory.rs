//! F0 facade action inventory (#913 / #495).
//!
//! Machine-checkable lists of MCP facade actions so action-count regressions
//! are visible. `tachi_task` primary actions and `tachi_verify` actions are
//! derived from their typed enums (`action_enums::TachiTaskAction::PRIMARY`,
//! `TachiVerifyAction::ALL`) — no duplicated `&[&str]` mirrors (#1085).
//! GitHub PR lifecycle actions live only on `tachi_gh` (canonical); they were
//! removed from `tachi_task` in #757.

/// GH PR lifecycle actions removed from `tachi_task` (#757). Canonical surface
/// is `tachi_gh`. Kept as a machine-checkable deny-list for schema/router tests.
pub const TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS: &[&str] =
    &["link_pr", "pr_status", "pr_handoff", "release_note"];

/// Canonical `tachi_gh` actions (includes lifecycle).
pub const TACHI_GH_ACTIONS: &[&str] = &[
    "repo_view",
    "issue_list",
    "issue_read",
    "issue_create",
    "issue_comment",
    "issue_label",
    "issue_freshness_scan",
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
    // #1001: manual presence-claim backstop.
    "claim",
    "release",
    // #964: read-once agent-to-agent ephemeral notes.
    "sticky_leave",
    "sticky_check",
];

/// Soft ceilings for F0 monitoring (primary schema actions only).
pub const TACHI_TASK_PRIMARY_ACTION_SOFT_MAX: usize = 28;
pub const TACHI_MEMORY_ACTION_SOFT_MAX: usize = 25;
pub const TACHI_GH_ACTION_SOFT_MAX: usize = 20;

#[cfg(test)]
mod tests {
    use super::super::action_enums::{TachiTaskAction, TachiVerifyAction};
    use super::*;

    #[test]
    fn f0_task_primary_does_not_advertise_gh_lifecycle() {
        let primary = TachiTaskAction::primary_wire_strings();
        for action in TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS {
            assert!(
                !primary.contains(action),
                "primary task schema must not advertise {action}; use tachi_gh"
            );
        }
        // #1002 Issue Refinery bumps this from 24 -> 25 (`refine_issues`,
        // read-only/proposal-only, Observe bundle). Still well under
        // TACHI_TASK_PRIMARY_ACTION_SOFT_MAX (28) — the soft-ceiling
        // assertion below still passes (`<=`), but the tripwire above is
        // deliberately exact so the next addition gets the same explicit
        // look this one did.
        assert_eq!(primary.len(), 25);
        assert!(primary.len() <= TACHI_TASK_PRIMARY_ACTION_SOFT_MAX);
    }

    #[test]
    fn f0_gh_owns_lifecycle_actions() {
        for action in TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS {
            assert!(
                TACHI_GH_ACTIONS.contains(action),
                "tachi_gh must own lifecycle action {action}"
            );
        }
        assert_eq!(TACHI_GH_ACTIONS.len(), 18);
        assert!(TACHI_GH_ACTIONS.len() <= TACHI_GH_ACTION_SOFT_MAX);
    }

    #[test]
    fn f0_memory_and_verify_counts() {
        // Merge of #1001 (claim, release) and #964 (sticky_leave,
        // sticky_check) landing together bumps this from 23 -> 25, which
        // lands exactly AT TACHI_MEMORY_ACTION_SOFT_MAX — the soft-ceiling
        // assertion below still passes (`<=`), but the next legitimate
        // addition needs an explicit look at whether the ceiling itself
        // should move, not just this exact-count tripwire.
        assert_eq!(TACHI_MEMORY_ACTIONS.len(), 25);
        assert!(TACHI_MEMORY_ACTIONS.len() <= TACHI_MEMORY_ACTION_SOFT_MAX);
        assert_eq!(TachiVerifyAction::ALL.len(), 4);
    }
}
