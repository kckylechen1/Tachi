//! F3 (#495/#913): action-level ToolProfile filtering for facade tools.
//!
//! Tool-name filtering alone cannot deny `tachi_task(action=dispatch)` while
//! allowing `complete`/`status` on the same tool. This module maps facade
//! actions to required [`ToolBundle`]s and applies a curated allow-list for
//! the `delegate` worker profile.

use super::types::{default_tool_profile, ToolBundle, ToolProfile};

/// Facades with a per-action bundle policy (`facade_action_required_bundle`
/// classifies at least one of their actions). A tool NOT in this list has no
/// action concept as far as the gate is concerned — it is governed by
/// tool-level visibility only, and an empty/unclassified action on it is
/// harmless. A tool IN this list is a genuine authorization surface: an
/// action the gate can't classify (missing, or a bundle-map miss) must be
/// denied by default rather than silently passed through — that is the
/// fail-open bug this function exists to close (#919).
fn is_gated_facade(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tachi_task"
            | "tachi_memory"
            | "tachi_skill"
            | "tachi_wiki"
            | "tachi_verify"
            | "tachi_gh"
            | "tachi_event"
            | "tachi_a2a"
    )
}

/// Retired model-facing Memory tokens. Reject these before the admin/full-
/// bundle pass-through so no profile can resurrect a deleted action.
fn memory_action_retired(action: &str) -> bool {
    matches!(
        action,
        "progress"
            | "readiness"
            | "delete"
            | "gc"
            | "doctor_scan"
            | "ingest"
            | "ingest_source"
            | "pattern_feedback"
    )
}

/// Tachi-owned worker launch is an expert/operator exception, never part of
/// an ordinary agent profile. Native harness delegation remains available to
/// standard/coordinate agents without exposing this action.
fn task_action_requires_admin(action: &str) -> bool {
    action == "dispatch"
}

fn task_action_retired(action: &str) -> bool {
    matches!(
        action,
        "plan"
            | "cycle_plan"
            | "recommend"
            | "refine_issues"
            | "merge"
            | "ux_matrix"
            | "briefing"
            | "doc_index"
            | "cycle_status"
            | "build_references"
            | "close_loop"
            | "profiles"
            | "profile"
            | "card"
    )
}

/// A profile that already allows every bundle (standard/admin) gains nothing
/// from an unclassified-action fallback: it would have allowed the action
/// anyway once classified, so letting the call through to the handler (for a
/// precise invalid-action error instead of an opaque permission error) is
/// pure UX and not a privilege escalation. Any profile that does NOT have
/// full bundle access must not be auto-granted an action it can't classify.
fn profile_has_full_bundle_access(profile: ToolProfile) -> bool {
    profile.allows(ToolBundle::Observe)
        && profile.allows(ToolBundle::Remember)
        && profile.allows(ToolBundle::Coordinate)
        && profile.allows(ToolBundle::Operate)
}

/// Whether `tool_name` + `action` is allowed under `profile`.
///
/// - Admin: always allowed for live actions; retired `tachi_task` tokens are
///   denied before the admin pass-through so no model profile can resurrect a
///   deleted Task surface.
/// - Delegate: curated per-facade action allow-list (prevents recursive
///   dispatch); missing/unknown actions on a delegate-gated facade are
///   denied — a delegate tool must explicitly enumerate its allowed actions.
/// - Other profiles: action must be covered by a bundle the profile allows.
///   Missing/unknown actions on a known (gated) facade are denied UNLESS the
///   profile already has full bundle access (standard/admin), in which case
///   the call passes through so the facade handler can return a precise
///   invalid-action error — the gate is not a schema oracle, but it must
///   default-DENY for any profile it actually restricts (#919).
/// - Tools with no action policy at all (no action concept) stay tool-level
///   only, unaffected by any of the above.
pub fn facade_action_allowed(
    tool_name: &str,
    action: Option<&str>,
    profile: Option<ToolProfile>,
) -> bool {
    let profile = profile.unwrap_or_else(default_tool_profile);
    let action = action.map(str::trim).filter(|a| !a.is_empty());
    if tool_name == "tachi_agent_eval"
        && action.is_some_and(|action| {
            matches!(
                action.to_ascii_lowercase().as_str(),
                "attach_session" | "get_attachment"
            )
        })
    {
        // Attachment admission/projection is a host-coordination surface.
        // Keep the existing eval actions unchanged, while requiring the
        // explicit coordinate/admin profile before the attachment handler can
        // run (the server handler repeats this gate for direct callers).
        return profile.is_admin() || profile == ToolProfile::coordinate();
    }
    if tool_name == "tachi_task"
        && action
            .map(|a| task_action_retired(&a.to_ascii_lowercase()))
            .unwrap_or(false)
    {
        return false;
    }
    if tool_name == "tachi_memory"
        && action
            .map(|a| memory_action_retired(&a.to_ascii_lowercase()))
            .unwrap_or(false)
    {
        return false;
    }

    if profile.is_admin() {
        return true;
    }

    if tool_name == "tachi_task"
        && action
            .map(|a| task_action_requires_admin(&a.to_ascii_lowercase()))
            .unwrap_or(false)
    {
        return false;
    }

    if profile.uses_delegate_allow_list() {
        // Delegate is the narrowest profile: an empty/missing action on a
        // gated facade must be explicitly enumerated, same as a real action.
        // `delegate_facade_action_allowed` already denies by default for any
        // action string it doesn't recognize (including "").
        return delegate_facade_action_allowed(
            tool_name,
            action.unwrap_or("").to_ascii_lowercase().as_str(),
        );
    }

    let Some(action) = action else {
        return !is_gated_facade(tool_name) || profile_has_full_bundle_access(profile);
    };
    let action = action.to_ascii_lowercase();

    if let Some(bundle) = facade_action_required_bundle(tool_name, &action) {
        return profile.allows(bundle);
    }

    !is_gated_facade(tool_name) || profile_has_full_bundle_access(profile)
}

/// Curated worker surface: facades that are on `DELEGATE_MINIMAL_TOOL_PATTERNS`
/// only expose safe actions. Tools on the delegate list with no action concept
/// (no action-gated bundle) are explicitly enumerated as always-allowed so a
/// newly-added gated facade can never silently fall through the default; any
/// tool/action pair not recognized here is denied.
fn delegate_facade_action_allowed(tool_name: &str, action: &str) -> bool {
    match tool_name {
        "tachi_task" => matches!(action, "complete" | "status" | "board" | "brief"),
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
        ),
        "tachi_wiki" => matches!(action, "search" | "browse" | "read"),
        "tachi_skill" => matches!(action, "discover" | "run"),
        "tachi_event" => matches!(action, "emit" | "query" | "metrics" | "context" | "a2a"),
        "tachi_a2a" => matches!(action, "respond" | "status"),
        // Non-facade tools on the delegate list (no action concept): tool
        // visibility is enough, regardless of what's in the `action` arg.
        // `peer_query` (#1016 S1) is action-less (its selector is `noun`, gated
        // internally by an exhaustive whitelist) and read-only, so it belongs
        // here rather than in an action-bundle map.
        "tachi_tools" | "runtime_info" | "tachi_web_search" | "tachi_unstick" | "peer_query" => {
            true
        }
        // Anything else — a tool not on the delegate allow-list at all, or a
        // gated facade we forgot to enumerate above — is denied by default.
        _ => false,
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
            "brief" | "status" | "board" => Some(ToolBundle::Observe),
            "complete" | "adjudicate" | "claim" | "release" | "heartbeat" => {
                Some(ToolBundle::Remember)
            }
            "dispatch" | "cancel" | "intake" => Some(ToolBundle::Coordinate),
            "handoff" => Some(ToolBundle::Coordinate),
            _ => None,
        },
        "tachi_memory" => match action.as_str() {
            "search" | "get" | "briefing" | "alerts" | "ask" => Some(ToolBundle::Observe),
            "save" | "extract_facts" | "checkpoint" => Some(ToolBundle::Remember),
            "consolidate" => Some(ToolBundle::Operate),
            _ => None,
        },
        "tachi_skill" => match action.as_str() {
            "discover" => Some(ToolBundle::Observe),
            "run" => Some(ToolBundle::Remember),
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
            "repo_view"
            | "issue_list"
            | "issue_read"
            | "issue_freshness_scan"
            | "pr_list"
            | "pr_read"
            | "pr_comments"
            | "pr_review_digest"
            | "pr_status" => Some(ToolBundle::Observe),
            "issue_create" | "issue_comment" | "issue_label" | "pr_comment" | "safe_merge"
            | "ship" | "link_pr" | "pr_handoff" | "release_note" | "close_loop" => {
                Some(ToolBundle::Coordinate)
            }
            _ => None,
        },
        "tachi_event" => match action.as_str() {
            "query" | "metrics" | "context" | "a2a" => Some(ToolBundle::Observe),
            "emit" => Some(ToolBundle::Remember),
            "project" | "promote" | "label_eval" => Some(ToolBundle::Operate),
            _ => None,
        },
        "tachi_a2a" => match action.as_str() {
            "status" => Some(ToolBundle::Observe),
            "respond" => Some(ToolBundle::Remember),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f1751_a2a_profiles_split_status_from_respond_and_fail_closed() {
        assert!(facade_action_allowed(
            "tachi_a2a",
            Some("status"),
            Some(ToolProfile::observe())
        ));
        assert!(!facade_action_allowed(
            "tachi_a2a",
            Some("respond"),
            Some(ToolProfile::observe())
        ));
        for action in ["respond", "status"] {
            assert!(facade_action_allowed(
                "tachi_a2a",
                Some(action),
                Some(ToolProfile::delegate())
            ));
        }
        assert!(!facade_action_allowed(
            "tachi_a2a",
            Some("send"),
            Some(ToolProfile::delegate())
        ));
    }

    #[test]
    fn f3_delegate_denies_task_dispatch_allows_complete() {
        let profile = Some(ToolProfile::delegate());
        assert!(
            !facade_action_allowed("tachi_task", Some("dispatch"), profile),
            "delegate must not recurse via dispatch"
        );
        assert!(
            !facade_action_allowed("tachi_task", Some("recommend"), profile),
            "delegate must not call retired recommend action"
        );
        for retired in ["plan", "cycle_plan", "refine_issues", "merge", "ux_matrix"] {
            assert!(
                !facade_action_allowed("tachi_task", Some(retired), profile),
                "delegate must not call retired task action {retired}"
            );
        }
        assert!(facade_action_allowed(
            "tachi_task",
            Some("complete"),
            profile
        ));
        assert!(facade_action_allowed("tachi_task", Some("status"), profile));
        assert!(facade_action_allowed("tachi_task", Some("board"), profile));
        assert!(facade_action_allowed("tachi_task", Some("brief"), profile));
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
        assert!(!facade_action_allowed(
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
            Some("consolidate"),
            profile
        ));

        assert!(facade_action_allowed("tachi_browse", None, profile));
        assert!(!facade_action_allowed(
            "tachi_wiki",
            Some("search"),
            profile
        ));
        assert!(!facade_action_allowed(
            "tachi_wiki",
            Some("browse"),
            profile
        ));
        assert!(!facade_action_allowed("tachi_wiki", Some("read"), profile));
        assert!(!facade_action_allowed("tachi_wiki", Some("write"), profile));
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
        assert!(facade_action_allowed("tachi_task", Some("brief"), profile));
        assert!(!facade_action_allowed("tachi_task", Some("plan"), profile));
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
    fn native_first_profiles_hide_dispatch_while_admin_retains_the_exception() {
        assert!(!facade_action_allowed(
            "tachi_task",
            Some("dispatch"),
            Some(ToolProfile::standard())
        ));
        assert!(facade_action_allowed(
            "tachi_task",
            Some("dispatch"),
            Some(ToolProfile::admin())
        ));
        assert!(!facade_action_allowed(
            "tachi_task",
            Some("dispatch"),
            Some(ToolProfile::coordinate())
        ));
        assert!(!facade_action_allowed(
            "tachi_task",
            Some("recommend"),
            Some(ToolProfile::standard())
        ));
    }

    #[test]
    fn f1687_c1c_retired_task_actions_are_denied_by_active_contract() {
        for action in [
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
        ] {
            assert_eq!(
                facade_action_required_bundle("tachi_task", action),
                None,
                "retired task action {action} must not have a bundle classification"
            );
            for profile in [
                ToolProfile::delegate(),
                ToolProfile::observe(),
                ToolProfile::remember(),
                ToolProfile::coordinate(),
                ToolProfile::operate(),
                ToolProfile::standard(),
                ToolProfile::admin(),
            ] {
                assert!(
                    !facade_action_allowed("tachi_task", Some(action), Some(profile)),
                    "retired task action {action} must be denied for {}",
                    profile.as_str()
                );
            }
        }
    }

    #[test]
    fn f1712_c1b1_brief_is_observe_and_folded_tokens_are_denied() {
        assert_eq!(
            facade_action_required_bundle("tachi_task", "brief"),
            Some(ToolBundle::Observe),
            "tachi_task(action='brief') must retain the Observe bundle"
        );
        for profile in [
            ToolProfile::delegate(),
            ToolProfile::observe(),
            ToolProfile::remember(),
            ToolProfile::coordinate(),
            ToolProfile::operate(),
            ToolProfile::standard(),
            ToolProfile::admin(),
        ] {
            assert!(
                facade_action_allowed("tachi_task", Some("brief"), Some(profile)),
                "brief must be allowed for {}",
                profile.as_str()
            );
        }
        for retired in ["briefing", "doc_index", "cycle_status"] {
            assert_eq!(
                facade_action_required_bundle("tachi_task", retired),
                None,
                "retired folded action {retired} must not have a bundle classification"
            );
            for profile in [
                ToolProfile::delegate(),
                ToolProfile::observe(),
                ToolProfile::remember(),
                ToolProfile::coordinate(),
                ToolProfile::operate(),
                ToolProfile::standard(),
                ToolProfile::admin(),
            ] {
                assert!(
                    !facade_action_allowed("tachi_task", Some(retired), Some(profile)),
                    "retired folded action {retired} must be denied for {}",
                    profile.as_str()
                );
            }
        }
    }

    #[test]
    fn f1253_tachi_task_workclaim_actions_have_explicit_bundles() {
        for action in ["claim", "release", "heartbeat"] {
            assert_eq!(
                facade_action_required_bundle("tachi_task", action),
                Some(ToolBundle::Remember),
                "tachi_task(action='{action}') must map to Remember"
            );
        }
        assert_eq!(
            facade_action_required_bundle("tachi_task", "handoff"),
            Some(ToolBundle::Coordinate),
            "tachi_task(action='handoff') must map to Coordinate"
        );
    }

    #[test]
    fn f1253_tachi_task_workclaim_actions_respect_restricted_profiles() {
        for action in ["claim", "release", "heartbeat", "handoff"] {
            assert!(
                !facade_action_allowed("tachi_task", Some(action), Some(ToolProfile::observe())),
                "observe must not call tachi_task(action='{action}')"
            );
        }

        for action in ["claim", "release", "heartbeat"] {
            assert!(
                facade_action_allowed("tachi_task", Some(action), Some(ToolProfile::remember())),
                "remember must call tachi_task(action='{action}')"
            );
            assert!(
                facade_action_allowed("tachi_task", Some(action), Some(ToolProfile::coordinate())),
                "coordinate must call tachi_task(action='{action}')"
            );
        }
        assert!(
            !facade_action_allowed("tachi_task", Some("handoff"), Some(ToolProfile::remember())),
            "remember must not call coordinate-tier tachi_task(action='handoff')"
        );
        assert!(
            facade_action_allowed(
                "tachi_task",
                Some("handoff"),
                Some(ToolProfile::coordinate())
            ),
            "coordinate must call tachi_task(action='handoff')"
        );

        for action in ["claim", "release", "heartbeat", "handoff"] {
            assert!(
                !facade_action_allowed("tachi_task", Some(action), Some(ToolProfile::delegate())),
                "delegate must not gain tachi_task(action='{action}') without an owner policy change"
            );
        }
    }

    #[test]
    fn delegate_memory_claim_release_are_retired_but_search_allowed() {
        let profile = Some(ToolProfile::delegate());
        assert!(facade_action_allowed(
            "tachi_memory",
            Some("search"),
            profile
        ));
        assert!(!facade_action_allowed(
            "tachi_memory",
            Some("claim"),
            profile
        ));
        assert!(!facade_action_allowed(
            "tachi_memory",
            Some("release"),
            profile
        ));
        assert_eq!(facade_action_required_bundle("tachi_memory", "claim"), None);
        assert_eq!(
            facade_action_required_bundle("tachi_memory", "release"),
            None
        );
    }

    #[test]
    fn f3_missing_action_on_non_gated_tool_is_not_profile_gated() {
        // tachi_unstick has no action concept; tool-level visibility is enough.
        assert!(facade_action_allowed(
            "tachi_unstick",
            None,
            Some(ToolProfile::delegate())
        ));
        assert!(facade_action_allowed(
            "tachi_unstick",
            None,
            Some(ToolProfile::observe())
        ));
    }

    /// #919 CRITICAL: an empty/missing action on a *gated* facade must be
    /// denied by default for any profile the gate actually restricts — never
    /// silently allowed. This is the fail-open bug this module exists to close.
    #[test]
    fn f919_missing_action_on_gated_facade_is_denied_for_restricted_profiles() {
        for profile in [
            ToolProfile::delegate(),
            ToolProfile::observe(),
            ToolProfile::remember(),
            ToolProfile::coordinate(),
            ToolProfile::operate(),
        ] {
            assert!(
                !facade_action_allowed("tachi_task", Some(""), Some(profile)),
                "empty action on tachi_task must be denied for {}",
                profile.as_str()
            );
            assert!(
                !facade_action_allowed("tachi_task", Some("  "), Some(profile)),
                "whitespace-only action on tachi_task must be denied for {}",
                profile.as_str()
            );
            assert!(
                !facade_action_allowed("tachi_task", None, Some(profile)),
                "None action on tachi_task must be denied for {}",
                profile.as_str()
            );
        }
    }

    /// A profile with full bundle access (standard/admin) is not made *more*
    /// permissive by this gate; letting an unclassified action reach the
    /// handler there is a UX choice (precise invalid-action error), not a
    /// privilege escalation, since the profile already allows every bundle.
    #[test]
    fn f919_missing_or_unknown_action_passes_through_for_full_bundle_profiles() {
        for profile in [ToolProfile::standard(), ToolProfile::admin()] {
            assert!(facade_action_allowed("tachi_task", None, Some(profile)));
            assert!(facade_action_allowed("tachi_task", Some(""), Some(profile)));
            assert!(facade_action_allowed(
                "tachi_task",
                Some("not_a_real_action"),
                Some(profile)
            ));
        }
    }

    /// #919 CRITICAL: known facade + unknown/typo action must be denied for
    /// restricted profiles (was fail-open: any unmapped action returned `true`
    /// unconditionally regardless of profile).
    #[test]
    fn f919_unknown_action_on_known_facade_is_denied_for_restricted_profiles() {
        for profile in [
            ToolProfile::observe(),
            ToolProfile::remember(),
            ToolProfile::coordinate(),
            ToolProfile::operate(),
        ] {
            assert!(!facade_action_allowed(
                "tachi_task",
                Some("not_a_real_action"),
                Some(profile)
            ));
            assert!(!facade_action_allowed(
                "tachi_memory",
                Some("not_a_real_action"),
                Some(profile)
            ));
        }
    }

    /// #919 CRITICAL: a delegate-list tool that has no explicit action arm
    /// (not one of the enumerated facades, and not one of the enumerated
    /// no-action-concept tools) must be denied, not silently granted every
    /// action — closes the `_ => true` catch-all that used to grant ALL
    /// actions on any tool the delegate match didn't recognize.
    #[test]
    fn f919_delegate_unrecognized_tool_action_pair_is_denied() {
        assert!(!facade_action_allowed(
            "tachi_wiki",
            Some("write"),
            Some(ToolProfile::delegate())
        ));
        assert!(!facade_action_allowed(
            "some_future_gated_tool",
            Some("anything"),
            Some(ToolProfile::delegate())
        ));
    }

    /// Unrestricted tools on the delegate allow-list keep working regardless
    /// of what's in the (irrelevant) action arg — this is the "no action
    /// concept" carve-out, distinct from a real gated facade with an
    /// unrecognized action.
    #[test]
    fn f919_delegate_no_action_concept_tools_stay_allowed() {
        let profile = Some(ToolProfile::delegate());
        for tool in [
            "tachi_tools",
            "runtime_info",
            "tachi_web_search",
            "tachi_unstick",
        ] {
            assert!(facade_action_allowed(tool, None, profile));
            assert!(facade_action_allowed(tool, Some("whatever"), profile));
        }
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

    #[test]
    fn f1689_memory_actions_have_the_final_bundle_policy() {
        for action in ["search", "get", "briefing", "alerts", "ask"] {
            assert_eq!(
                facade_action_required_bundle("tachi_memory", action),
                Some(ToolBundle::Observe),
                "{action} must be Observe"
            );
        }
        for action in ["save", "extract_facts", "checkpoint"] {
            assert_eq!(
                facade_action_required_bundle("tachi_memory", action),
                Some(ToolBundle::Remember),
                "{action} must be Remember"
            );
        }
        assert_eq!(
            facade_action_required_bundle("tachi_memory", "consolidate"),
            Some(ToolBundle::Operate)
        );
    }

    #[test]
    fn f1689_retired_memory_actions_are_unclassified_and_denied() {
        for action in [
            "progress",
            "readiness",
            "delete",
            "gc",
            "doctor_scan",
            "ingest",
            "ingest_source",
            "pattern_feedback",
        ] {
            assert_eq!(
                facade_action_required_bundle("tachi_memory", action),
                None,
                "retired Memory action {action} must be unclassified"
            );
            for profile in [
                ToolProfile::observe(),
                ToolProfile::remember(),
                ToolProfile::coordinate(),
                ToolProfile::operate(),
                ToolProfile::standard(),
                ToolProfile::delegate(),
                ToolProfile::admin(),
            ] {
                assert!(
                    !facade_action_allowed("tachi_memory", Some(action), Some(profile)),
                    "retired tachi_memory(action='{action}') must be denied for {} profile",
                    profile.as_str()
                );
            }
        }
    }

    #[test]
    fn f1713_close_loop_is_coordinate_and_delegate_denied() {
        assert!(facade_action_allowed(
            "tachi_gh",
            Some("close_loop"),
            Some(ToolProfile::coordinate())
        ));
        assert!(!facade_action_allowed(
            "tachi_gh",
            Some("close_loop"),
            Some(ToolProfile::delegate())
        ));
    }

    #[test]
    fn f1713_retired_task_closure_actions_are_unclassified_and_denied() {
        for action in ["build_references", "close_loop"] {
            assert_eq!(
                facade_action_required_bundle("tachi_task", action),
                None,
                "retired Task closure action {action} must not have a bundle"
            );
            for profile in [
                ToolProfile::delegate(),
                ToolProfile::observe(),
                ToolProfile::remember(),
                ToolProfile::coordinate(),
                ToolProfile::operate(),
                ToolProfile::standard(),
                ToolProfile::admin(),
            ] {
                assert!(
                    !facade_action_allowed("tachi_task", Some(action), Some(profile)),
                    "retired Task closure action {action} must be denied for {}",
                    profile.as_str()
                );
            }
        }
    }
    #[test]
    fn f1733_attachment_actions_are_coordinate_or_admin_only() {
        for action in ["attach_session", "get_attachment"] {
            assert!(facade_action_allowed(
                "tachi_agent_eval",
                Some(action),
                Some(ToolProfile::coordinate())
            ));
            assert!(facade_action_allowed(
                "tachi_agent_eval",
                Some(action),
                Some(ToolProfile::admin())
            ));
            for profile in [ToolProfile::observe(), ToolProfile::delegate()] {
                assert!(
                    !facade_action_allowed("tachi_agent_eval", Some(action), Some(profile)),
                    "{action} must be denied for {}",
                    profile.as_str()
                );
            }
        }
    }

    #[test]
    fn f1733_non_attachment_eval_actions_keep_existing_policy_behavior() {
        for action in ["aggregate_live", "telemetry", "route_projection"] {
            assert!(facade_action_allowed(
                "tachi_agent_eval",
                Some(action),
                Some(ToolProfile::observe())
            ));
            assert!(!facade_action_allowed(
                "tachi_agent_eval",
                Some(action),
                Some(ToolProfile::delegate())
            ));
        }
    }
}
