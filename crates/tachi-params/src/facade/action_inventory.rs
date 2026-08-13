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

/// C1a of #1611/#1683, C1b-1 of #1712, and #1713 retired these primary
/// `tachi_task` actions without leaving them in the live Task inventory. Keep
/// a machine-checkable deny-list so schemas, docs, and active profile
/// discriminators cannot keep teaching them.
pub const TACHI_TASK_RETIRED_C1A_ACTIONS: &[&str] = &[
    "plan",
    "cycle_plan",
    "recommend",
    "refine_issues",
    "merge",
    "ux_matrix",
    "briefing",
    "doc_index",
    "cycle_status",
    "build_references",
    "close_loop",
];

/// C1c of #1687 retires model-facing profile/card inspection from
/// `tachi_task`. The local operator-only `tachi card` command remains the
/// diagnostics surface; evaluators and static dispatch admission remain
/// separate owners.
pub const TACHI_TASK_RETIRED_C1C_ACTIONS: &[&str] = &["profiles", "profile", "card"];

/// Complete deny-list for retired `tachi_task` action tokens across C1a/b/c.
pub const TACHI_TASK_RETIRED_ACTIONS: &[&str] = &[
    "plan",
    "cycle_plan",
    "recommend",
    "refine_issues",
    "merge",
    "ux_matrix",
    "briefing",
    "doc_index",
    "cycle_status",
    "build_references",
    "close_loop",
    "profiles",
    "profile",
    "card",
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
    "close_loop",
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

/// Closed local advisory-mailbox actions (#1751).
pub const TACHI_A2A_ACTIONS: &[&str] = &["respond", "status"];

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
                "#1683 C1a / #1712 C1b retired task action {action} must not be advertised"
            );
        }
        for action in TACHI_TASK_RETIRED_C1C_ACTIONS {
            assert!(
                !primary.contains(action),
                "#1687 C1c retired task action {action} must not be advertised"
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
                "brief",
            ]
        );
        // #1683 C1a plus #1712 C1b-1 plus #1713 contract the Task surface.
        // Keep this exact so the next inventory addition gets explicit review.
        assert_eq!(primary.len(), 10);
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
        assert_eq!(TACHI_GH_ACTIONS.len(), 19);
        assert!(TACHI_GH_ACTIONS.len() <= TACHI_GH_ACTION_SOFT_MAX);
    }

    #[test]
    fn f0_memory_and_verify_counts() {
        // #1426 moved four recall-tuning actions to tachi_tune, and #1688
        // removed duplicate claim/release compatibility routes. #1751 then
        // retires the two legacy sticky actions: 19 -> 17.
        assert_eq!(TACHI_MEMORY_ACTIONS.len(), 17);
        assert!(TACHI_MEMORY_ACTIONS.len() <= TACHI_MEMORY_ACTION_SOFT_MAX);
        assert_eq!(TachiVerifyAction::ALL.len(), 4);
    }

    #[test]
    fn f1751_memory_inventory_is_exactly_the_seventeen_survivors() {
        assert_eq!(
            TACHI_MEMORY_ACTIONS,
            &[
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
                "delete",
                "gc",
                "doctor_scan",
                "ingest",
                "ingest_source",
            ],
            "#1751 must pin the literal 17-action Memory inventory after sticky retirement",
        );
    }

    #[test]
    fn f0_tune_inventory_count() {
        // #1426: route tuning leaves tachi_task (27 -> 23) and recall tuning
        // leaves tachi_memory (25 -> 21); #1688 then removes claim/release
        // (21 -> 19). The eight moved actions live only on
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
