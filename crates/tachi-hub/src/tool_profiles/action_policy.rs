//! F3 (#495/#913): action-level ToolProfile filtering for facade tools.
//!
//! Tool-name filtering alone cannot deny `tachi_task(action=dispatch)` while
//! allowing `complete`/`status` on the same tool. This module maps facade
//! actions to required [`ToolBundle`]s and applies a curated allow-list for
//! the `delegate` worker profile.

use super::types::{default_tool_profile, ToolBundle, ToolProfile};

/// Whether `tool_name` + `action` is allowed under `profile`.
///
/// - Admin: always allowed.
/// - Missing/empty `action`: allowed (non-facade tools, or tool-level gate only).
/// - Delegate: curated per-facade action allow-list (prevents recursive dispatch).
/// - Other profiles: action must be covered by a bundle the profile allows.
/// - Unknown actions on known facades: fail open so the facade handler can
///   return a precise invalid-action error (profile is not a schema oracle).
pub fn facade_action_allowed(
    tool_name: &str,
    action: Option<&str>,
    profile: Option<ToolProfile>,
) -> bool {
    let profile = profile.unwrap_or_else(default_tool_profile);
    if profile.is_admin() {
        return true;
    }

    let Some(action) = action.map(str::trim).filter(|a| !a.is_empty()) else {
        return true;
    };
    let action = action.to_ascii_lowercase();

    if profile.uses_delegate_allow_list() {
        return delegate_facade_action_allowed(tool_name, &action);
    }

    if let Some(bundle) = facade_action_required_bundle(tool_name, &action) {
        return profile.allows(bundle);
    }

    true
}

/// Curated worker surface: facades that are on `DELEGATE_MINIMAL_TOOL_PATTERNS`
/// only expose safe actions. Unknown tools on the delegate list (no action map)
/// stay tool-level only.
fn delegate_facade_action_allowed(tool_name: &str, action: &str) -> bool {
    match tool_name {
        "tachi_task" => matches!(
            action,
            "plan" | "complete" | "status" | "board" | "wait" | "briefing" | "doc_index"
        ),
        "tachi_memory" => matches!(
            action,
            "search"
                | "get"
                | "save"
                | "extract_facts"
                | "briefing"
                | "checkpoint"
                | "alerts"
                | "ask"
                | "progress"
                | "readiness"
        ),
        "tachi_skill" => matches!(action, "discover" | "run" | "bundle"),
        "tachi_event" => matches!(action, "emit" | "query" | "metrics" | "context" | "a2a"),
        // Non-facade tools on the delegate list (tachi_complete, run_skill, …)
        // have no action map — tool visibility is enough.
        _ => true,
    }
}

/// Bundle required to execute a known facade action.
///
/// Multi-bundle tools (`tachi_task`, `tachi_memory`, `tachi_skill`, …) use this
/// so observe-only profiles can see the tool name but cannot call write/dispatch
/// actions.
pub fn facade_action_required_bundle(tool_name: &str, action: &str) -> Option<ToolBundle> {
    let action = action.trim().to_ascii_lowercase();
    match tool_name {
        "tachi_task" => match action.as_str() {
            "plan" | "briefing" | "doc_index" | "status" | "board" | "wait" | "profiles"
            | "profile" | "card" | "cycle_status" | "cycle_plan" | "ux_matrix"
            | "build_references" => Some(ToolBundle::Observe),
            "complete" => Some(ToolBundle::Remember),
            "dispatch" | "recommend" | "cancel" | "merge" | "intake" | "close_loop" | "link_pr"
            | "pr_status" | "pr_handoff" | "release_note" => Some(ToolBundle::Coordinate),
            "route_simulate" | "proposals" | "review_proposal" | "apply_proposals" => {
                Some(ToolBundle::Operate)
            }
            _ => None,
        },
        "tachi_memory" => match action.as_str() {
            "search" | "get" | "briefing" | "alerts" | "ask" | "progress" | "readiness" => {
                Some(ToolBundle::Observe)
            }
            "save" | "extract_facts" | "checkpoint" => Some(ToolBundle::Remember),
            "consolidate"
            | "recall_simulate"
            | "recall_proposals"
            | "review_recall_proposal"
            | "apply_recall_proposals"
            | "pattern_feedback" => Some(ToolBundle::Operate),
            _ => None,
        },
        "tachi_skill" => match action.as_str() {
            "discover" => Some(ToolBundle::Observe),
            "run" | "bundle" => Some(ToolBundle::Remember),
            "loadout" | "from_pattern" => Some(ToolBundle::Operate),
            _ => None,
        },
        "tachi_wiki" => match action.as_str() {
            "search" | "browse" | "read" => Some(ToolBundle::Observe),
            "write" => Some(ToolBundle::Remember),
            _ => None,
        },
        "tachi_verify" => match action.as_str() {
            // Tool is listed under Coordinate patterns; treat status reads as
            // Observe so pure observe profiles that gain the tool later stay safe.
            "status" | "board" => Some(ToolBundle::Observe),
            "start" | "record" => Some(ToolBundle::Coordinate),
            _ => None,
        },
        "tachi_gh" => match action.as_str() {
            "repo_view" | "issue_list" | "issue_read" | "pr_list" | "pr_read" | "pr_comments"
            | "pr_review_digest" | "pr_status" => Some(ToolBundle::Observe),
            "issue_create" | "issue_comment" | "pr_comment" | "safe_merge" | "ship" | "link_pr"
            | "pr_handoff" | "release_note" => Some(ToolBundle::Coordinate),
            _ => None,
        },
        "tachi_event" => match action.as_str() {
            "query" | "metrics" | "context" | "a2a" => Some(ToolBundle::Observe),
            "emit" => Some(ToolBundle::Remember),
            "project" | "promote" | "label_eval" => Some(ToolBundle::Operate),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f3_delegate_denies_task_dispatch_allows_complete() {
        let profile = Some(ToolProfile::delegate());
        assert!(
            !facade_action_allowed("tachi_task", Some("dispatch"), profile),
            "delegate must not recurse via dispatch"
        );
        assert!(
            !facade_action_allowed("tachi_task", Some("recommend"), profile),
            "delegate must not recommend/dispatch"
        );
        assert!(facade_action_allowed(
            "tachi_task",
            Some("complete"),
            profile
        ));
        assert!(facade_action_allowed("tachi_task", Some("status"), profile));
        assert!(facade_action_allowed("tachi_task", Some("plan"), profile));
        assert!(facade_action_allowed("tachi_task", Some("board"), profile));
        assert!(facade_action_allowed("tachi_task", Some("wait"), profile));
    }

    #[test]
    fn f3_delegate_skill_and_memory_allowlists() {
        let profile = Some(ToolProfile::delegate());
        assert!(facade_action_allowed(
            "tachi_skill",
            Some("discover"),
            profile
        ));
        assert!(facade_action_allowed("tachi_skill", Some("run"), profile));
        assert!(facade_action_allowed(
            "tachi_skill",
            Some("bundle"),
            profile
        ));
        assert!(!facade_action_allowed(
            "tachi_skill",
            Some("loadout"),
            profile
        ));
        assert!(!facade_action_allowed(
            "tachi_skill",
            Some("from_pattern"),
            profile
        ));

        assert!(facade_action_allowed(
            "tachi_memory",
            Some("search"),
            profile
        ));
        assert!(facade_action_allowed("tachi_memory", Some("save"), profile));
        assert!(!facade_action_allowed(
            "tachi_memory",
            Some("apply_recall_proposals"),
            profile
        ));
        assert!(!facade_action_allowed(
            "tachi_memory",
            Some("consolidate"),
            profile
        ));
    }

    #[test]
    fn f3_observe_denies_dispatch_allows_status() {
        let profile = Some(ToolProfile::observe());
        assert!(!facade_action_allowed(
            "tachi_task",
            Some("dispatch"),
            profile
        ));
        assert!(facade_action_allowed("tachi_task", Some("status"), profile));
        assert!(facade_action_allowed("tachi_task", Some("plan"), profile));
        assert!(!facade_action_allowed(
            "tachi_memory",
            Some("save"),
            profile
        ));
        assert!(facade_action_allowed(
            "tachi_memory",
            Some("search"),
            profile
        ));
    }

    #[test]
    fn f3_standard_and_admin_allow_dispatch() {
        assert!(facade_action_allowed(
            "tachi_task",
            Some("dispatch"),
            Some(ToolProfile::standard())
        ));
        assert!(facade_action_allowed(
            "tachi_task",
            Some("dispatch"),
            Some(ToolProfile::admin())
        ));
        assert!(facade_action_allowed(
            "tachi_task",
            Some("dispatch"),
            Some(ToolProfile::coordinate())
        ));
    }

    #[test]
    fn f3_missing_action_is_not_profile_gated() {
        assert!(facade_action_allowed(
            "tachi_complete",
            None,
            Some(ToolProfile::delegate())
        ));
        assert!(facade_action_allowed(
            "tachi_task",
            Some(""),
            Some(ToolProfile::delegate())
        ));
        assert!(facade_action_allowed(
            "tachi_task",
            Some("  "),
            Some(ToolProfile::delegate())
        ));
    }

    #[test]
    fn f3_action_matching_is_case_insensitive() {
        let profile = Some(ToolProfile::delegate());
        assert!(facade_action_allowed(
            "tachi_task",
            Some("Complete"),
            profile
        ));
        assert!(!facade_action_allowed(
            "tachi_task",
            Some("DISPATCH"),
            profile
        ));
    }
}
