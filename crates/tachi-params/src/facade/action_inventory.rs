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

/// C1a of #1611/#1683 retired these primary `tachi_task` actions without
/// folding them into Task replacements. Keep a machine-checkable deny-list so
/// schemas, docs, and active profile discriminators cannot keep teaching them.
pub const TACHI_TASK_RETIRED_C1A_ACTIONS: &[&str] = &[
    "plan",
    "cycle_plan",
    "recommend",
    "refine_issues",
    "merge",
    "ux_matrix",
];

/// Canonical WorkClaim lifecycle actions exposed on `tachi_task` by #1253.
#[cfg(test)]
const TACHI_TASK_WORKCLAIM_ACTIONS: &[&str] = &["claim", "release", "heartbeat", "handoff"];

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

/// `tachi_tune` admin/operator actions. Introduced by #1426 to move route
/// and recall tuning out of daily `tachi_task` / `tachi_memory` schemas.
pub const TACHI_TUNE_ACTIONS: &[&str] = &[
    "route_simulate",
    "route_proposals",
    "route_review",
    "route_apply",
    "recall_simulate",
    "recall_proposals",
    "recall_review",
    "recall_apply",
];

/// `tachi_event` facade actions. Single source for `facade::tachi_event_action_schema`
/// and #1098's `action_effect` completeness test — previously each kept its own
/// inline copy of this list with no shared source to catch drift.
pub const TACHI_EVENT_ACTIONS: &[&str] = &[
    "emit",
    "query",
    "metrics",
    "project",
    "promote",
    "context",
    "a2a",
    "label_eval",
];

/// `tachi_wiki` facade actions. Single source for `facade::tachi_wiki_action_schema`
/// and #1098's `action_effect` completeness test.
pub const TACHI_WIKI_ACTIONS: &[&str] = &["search", "browse", "read", "write"];

/// `tachi_component` facade actions. The schema inventory is independent from
/// the server's replay-effect map so the live-router ratchet detects drift.
pub(super) const TACHI_COMPONENT_ACTIONS: &[&str] = &["list", "show", "check", "plan"];

/// `tachi_skill` facade actions. Single source for `facade::tachi_skill_action_schema`
/// and #1098's `action_effect` completeness test.
pub const TACHI_SKILL_ACTIONS: &[&str] = &["discover", "run", "bundle", "loadout", "from_pattern"];

/// `tachi_staff` facade actions. Single source for
/// `orchestration::tachi_staff_action_schema` and #1098's `action_effect`
/// completeness test.
pub const TACHI_STAFF_ACTIONS: &[&str] = &["start", "status"];

/// `tachi_orchestrator` facade actions. Single source for
/// `orchestration::tachi_orchestrator_action_schema` and #1098's
/// `action_effect` completeness test.
pub const TACHI_ORCHESTRATOR_ACTIONS: &[&str] = &[
    "todo_list",
    "todo_update",
    "handoff_write",
    "handoff_read",
    "recovery_briefing",
];

/// Soft ceilings for F0 monitoring (primary schema actions only).
pub const TACHI_TASK_PRIMARY_ACTION_SOFT_MAX: usize = 30;
pub const TACHI_MEMORY_ACTION_SOFT_MAX: usize = 25;
pub const TACHI_GH_ACTION_SOFT_MAX: usize = 20;

#[cfg(test)]
mod tests {
    use super::super::action_enums::{TachiTaskAction, TachiVerifyAction};
    use super::super::tune::TachiTuneAction;
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
        for action in TACHI_TASK_WORKCLAIM_ACTIONS {
            assert!(
                primary.contains(action),
                "#1253 WorkClaim action {action} must be advertised on tachi_task"
            );
        }
        for action in TACHI_TASK_RETIRED_C1A_ACTIONS {
            assert!(
                !primary.contains(action),
                "#1683 C1a retired task action {action} must not be advertised"
            );
        }
        assert_eq!(
            primary,
            [
                "intake",
                "claim",
                "heartbeat",
                "handoff",
                "release",
                "board",
                "status",
                "complete",
                "adjudicate",
                "briefing",
                "doc_index",
                "cycle_status",
                "profiles",
                "profile",
                "card",
                "build_references",
                "close_loop"
            ]
        );
        // #1683 C1a removes six task actions from the 23-action surface.
        // Keep this exact so the next inventory addition gets explicit review.
        assert_eq!(primary.len(), 17);
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
        // sticky_check) landed at 25. #1426 moves four recall-tuning actions
        // to tachi_tune: 25 -> 21.
        assert_eq!(TACHI_MEMORY_ACTIONS.len(), 21);
        assert!(TACHI_MEMORY_ACTIONS.len() <= TACHI_MEMORY_ACTION_SOFT_MAX);
        assert_eq!(TachiVerifyAction::ALL.len(), 4);
    }

    #[test]
    fn f0_tune_inventory_count() {
        // #1426: route tuning leaves tachi_task (27 -> 23) and recall tuning
        // leaves tachi_memory (25 -> 21). The eight moved actions live only on
        // this admin/operator inventory; keep the count exact so the next
        // tuning action gets explicit review instead of quietly widening the
        // surface.
        assert_eq!(TACHI_TUNE_ACTIONS.len(), 8);
        assert_eq!(
            TACHI_TUNE_ACTIONS,
            TachiTuneAction::all_wire_strings().as_slice()
        );
    }
}
