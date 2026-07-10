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
    )
}

/// `tachi_memory` actions that were standalone ADMIN-ONLY tools before the
/// #757 fold (`delete_memory`, `memory_gc`, `ingest`, `ingest_source` — see
/// `ADMIN_ONLY_NATIVE_ROUTE_NAMES` in
/// `tachi-server/src/tests/profile_tests/tool_profile_router_coverage.rs`).
/// `doctor_scan` (`tachi_doctor_scan`) is intentionally excluded: it was
/// OBSERVE-tier pre-fold (read-only), not admin-only.
///
/// `action` must already be lowercased by the caller.
fn memory_action_requires_admin(action: &str) -> bool {
    matches!(action, "delete" | "gc" | "ingest" | "ingest_source")
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
/// - Admin: always allowed.
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
    if profile.is_admin() {
        return true;
    }

    let action = action.map(str::trim).filter(|a| !a.is_empty());

    // #757-fold fail-safe fix (gpt-5.6-terra review): `delete`/`gc`/`ingest`/
    // `ingest_source` were standalone tools with NO bundle-pattern match at
    // all pre-fold (`tool_visible` denies any non-admin profile once no
    // bundle pattern matches — see tool_profile_router_coverage.rs's
    // ADMIN_ONLY_NATIVE_ROUTE_NAMES for delete_memory/memory_gc/ingest/
    // ingest_source). `ToolBundle` has no admin tier — every one of its four
    // variants is already satisfied by the non-admin `standard()` profile —
    // so no bundle assignment in `facade_action_required_bundle` can
    // reproduce an admin-only boundary; folding these into `tachi_memory`
    // (visible to Standard) silently widened them to any Operate-allowed
    // caller. Gate explicitly on `profile.is_admin()` (already false here)
    // instead, before the bundle lookup runs, so they stay admin-only no
    // matter what bundle they're classified under below.
    if tool_name == "tachi_memory"
        && action
            .map(|a| memory_action_requires_admin(&a.to_ascii_lowercase()))
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
        // Non-facade tools on the delegate list (no action concept): tool
        // visibility is enough, regardless of what's in the `action` arg.
        "tachi_tools" | "runtime_info" | "tachi_web_search" | "tachi_browse" | "tachi_unstick"
        | "tachi_complete" | "run_skill" => true,
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
            // doctor_scan: read-only (own docstring: "Read-only only — all
            // mutations are CLI-only") and was OBSERVE-tier pre-#757-fold
            // (`tachi_doctor_scan` in OBSERVE_TOOL_PATTERNS) — keep parity,
            // do not narrow a read-only action past its prior visibility.
            "search" | "get" | "briefing" | "alerts" | "ask" | "progress" | "readiness"
            | "doctor_scan" => Some(ToolBundle::Observe),
            "save" | "extract_facts" | "checkpoint" => Some(ToolBundle::Remember),
            "consolidate"
            | "recall_simulate"
            | "recall_proposals"
            | "review_recall_proposal"
            | "apply_recall_proposals"
            | "pattern_feedback" => Some(ToolBundle::Operate),
            // #757-fold fail-safe fix (gpt-5.6-terra review): delete/gc/
            // ingest/ingest_source were standalone ADMIN-ONLY tools pre-fold
            // (absent from every bundle pattern list — no non-admin profile
            // could see them at all). `ToolBundle` has no admin tier, so
            // classifying them Operate here would let any Operate-allowed
            // (non-admin) caller through — that's the fold's fail-safe
            // violation. The real gate is `memory_action_requires_admin` in
            // `facade_action_allowed`, which returns `false` for these four
            // actions before this bundle lookup is ever consulted for a
            // non-admin profile. The Operate classification below only
            // exists to satisfy the action-inventory-classification
            // completeness test (`f919_tachi_memory_actions_are_all_classified`)
            // — it is NOT the enforcement point; do not rely on it.
            "delete" | "gc" | "ingest" | "ingest_source" => Some(ToolBundle::Operate),
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
    fn f3_missing_action_on_non_gated_tool_is_not_profile_gated() {
        // tachi_complete has no action concept; tool-level visibility is enough.
        assert!(facade_action_allowed(
            "tachi_complete",
            None,
            Some(ToolProfile::delegate())
        ));
        assert!(facade_action_allowed(
            "tachi_complete",
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
            "tachi_browse",
            "tachi_unstick",
            "tachi_complete",
            "run_skill",
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

    /// #757-fold fail-safe fix (gpt-5.6-terra review): `delete`/`gc`/
    /// `ingest`/`ingest_source` were standalone ADMIN-ONLY tools pre-fold.
    /// `tachi_memory` is visible to the non-admin Standard profile (which
    /// allows every `ToolBundle` including Operate), so a bundle-only
    /// classification would have silently widened these four to any
    /// Operate-allowed caller. Assert every non-admin profile — including
    /// Standard and Operate, which allow every `ToolBundle` — is DENIED,
    /// and only `admin()` is allowed.
    #[test]
    fn f757_terra_memory_admin_only_actions_denied_for_non_admin_profiles() {
        for action in ["delete", "gc", "ingest", "ingest_source"] {
            for profile in [
                ToolProfile::observe(),
                ToolProfile::remember(),
                ToolProfile::coordinate(),
                ToolProfile::operate(),
                ToolProfile::standard(),
                ToolProfile::delegate(),
            ] {
                assert!(
                    !facade_action_allowed("tachi_memory", Some(action), Some(profile)),
                    "tachi_memory(action='{action}') must be admin-only, denied for {} profile",
                    profile.as_str()
                );
            }
            assert!(
                facade_action_allowed("tachi_memory", Some(action), Some(ToolProfile::admin())),
                "tachi_memory(action='{action}') must remain allowed for admin"
            );
        }
    }

    /// `doctor_scan` is the one folded action that was OBSERVE-tier (read-only)
    /// pre-fold, not admin-only — the admin-only gate above must not catch it.
    #[test]
    fn f757_terra_doctor_scan_stays_observe_not_admin_only() {
        assert!(facade_action_allowed(
            "tachi_memory",
            Some("doctor_scan"),
            Some(ToolProfile::observe())
        ));
        assert!(facade_action_allowed(
            "tachi_memory",
            Some("doctor_scan"),
            Some(ToolProfile::standard())
        ));
        assert!(!facade_action_allowed(
            "tachi_memory",
            Some("doctor_scan"),
            Some(ToolProfile::delegate())
        ));
    }
}
