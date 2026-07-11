//! #919 (Fable's closer): inventory↔policy consistency.
//!
//! `tachi-hub::action_policy::facade_action_allowed` now default-DENIES any
//! action it can't classify to a `ToolBundle` (the fail-open bug this PR
//! closes). That correctness now depends on every *legitimate* action in the
//! machine-checkable inventory (`tachi-params::action_inventory`)
//! actually being classified — otherwise a real action added to the
//! inventory but forgotten in the policy's bundle map would be silently
//! denied to every non-full-bundle profile (observe/remember/coordinate/
//! operate/delegate), a functional regression indistinguishable from "the
//! feature was never wired". This test lives in `tachi-server` because it's
//! the one crate that depends on both `tachi-params` (the inventory)
//! and `tachi-hub` (the policy) — turning "forgot to classify a new action"
//! into a compile-time-adjacent test failure instead of a silent runtime gap.

/// Assert every action in `actions` for `tool_name` classifies to
/// `Some(bundle)` via `tachi_hub::facade_action_required_bundle`.
fn assert_all_classified(tool_name: &str, actions: &[&str]) {
    let mut unclassified = Vec::new();
    for &action in actions {
        if tachi_hub::facade_action_required_bundle(tool_name, action).is_none() {
            unclassified.push(action);
        }
    }
    assert!(
        unclassified.is_empty(),
        "{tool_name} inventory action(s) {unclassified:?} have no ToolBundle classification in \
         action_policy::facade_action_required_bundle — a restricted profile (observe/remember/\
         coordinate/operate/delegate) will fail-closed-deny these even though they are legitimate \
         actions. Add a bundle arm in tachi-hub/src/tool_profiles/action_policy.rs."
    );
}

#[test]
fn f919_tachi_task_primary_actions_are_all_classified() {
    assert_all_classified("tachi_task", tachi_params::TACHI_TASK_PRIMARY_ACTIONS);
}

#[test]
fn f757_tachi_task_removed_gh_lifecycle_actions_are_unclassified() {
    // #757: GH lifecycle left tachi_task entirely. They must NOT classify under
    // tachi_task (would re-open a dual entry) and must still classify under tachi_gh.
    for &action in tachi_params::TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS {
        assert!(
            tachi_hub::facade_action_required_bundle("tachi_task", action).is_none(),
            "removed tachi_task lifecycle action {action} must not classify under tachi_task"
        );
        assert!(
            tachi_hub::facade_action_required_bundle("tachi_gh", action).is_some(),
            "lifecycle action {action} must still classify under tachi_gh"
        );
    }
}

#[test]
fn f919_tachi_gh_actions_are_all_classified() {
    assert_all_classified("tachi_gh", tachi_params::TACHI_GH_ACTIONS);
}

#[test]
fn f919_tachi_memory_actions_are_all_classified() {
    assert_all_classified("tachi_memory", tachi_params::TACHI_MEMORY_ACTIONS);
}

#[test]
fn f919_tachi_verify_actions_are_all_classified() {
    assert_all_classified("tachi_verify", tachi_params::TACHI_VERIFY_ACTIONS);
}
