//! #757 Cut3-S1: contract tests for folding the six sandbox tools into the
//! `tachi_sandbox` verb. Covers the fold-invariant authorization matrix
//! (`fold_contract` harness), old↔new output goldens for the pure-read actions,
//! and the two alias-manifest tripwires (version-past-removal, zombie-alias).

use super::fold_contract::{
    assert_admin_only, assert_fold_matrix_equivalence, FoldPair,
};
use super::{call_tool_via_server, make_server};
use crate::tools::alias_manifest::{
    all_aliases, find_alias, release_at_or_past, AliasKind, CURRENT_RELEASE,
};
use std::collections::BTreeMap;

/// FoldPairs for the sandbox fold, derived from the manifest so the harness and
/// the manifest can never drift.
fn sandbox_fold_pairs() -> Vec<FoldPair> {
    all_aliases()
        .filter(|entry| entry.canonical_tool == "tachi_sandbox")
        .map(|entry| FoldPair {
            legacy_name: entry.legacy_name,
            canonical_tool: entry.canonical_tool,
            canonical_action: entry.canonical_action,
        })
        .collect()
}

fn router_descriptions() -> BTreeMap<String, String> {
    let server = make_server();
    server
        .tool_router
        .list_all()
        .into_iter()
        .map(|tool| {
            (
                tool.name.into_owned(),
                tool.description
                    .map(|description| description.into_owned())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// Run one admin `tachi_sandbox`/legacy call and return its text payload.
async fn admin_call_text(
    tool: &str,
    args: serde_json::Map<String, serde_json::Value>,
) -> String {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("admin").expect("admin profile parses"),
    ));
    let result = call_tool_via_server(server, tool, Some(args))
        .await
        .expect("admin call should yield a tool result");
    result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.clone())
        .unwrap_or_default()
}

// ── Fold-invariant authorization matrix (#975-proof) ────────────────────────

#[tokio::test]
async fn sandbox_fold_matrix_is_cell_for_cell_equal() {
    // Old six names and the six new verb actions have identical reachability
    // for every one of the seven tool profiles.
    assert_fold_matrix_equivalence(&sandbox_fold_pairs()).await;
}

#[tokio::test]
async fn sandbox_fold_is_admin_only_both_sides() {
    // Absolute pin: callable only under admin, hidden everywhere else — for the
    // legacy aliases AND every action of the new verb. This is the "non-admin
    // profile calling any new action is refused" discrimination.
    assert_admin_only(&sandbox_fold_pairs()).await;
}

// ── Old↔new output goldens (pure-read actions) ──────────────────────────────

#[tokio::test]
async fn check_golden_matches_between_alias_and_verb() {
    let mut legacy_args = serde_json::Map::new();
    legacy_args.insert("agent_role".to_string(), serde_json::json!("code-review"));
    legacy_args.insert("path".to_string(), serde_json::json!("/project/secrets"));
    legacy_args.insert("operation".to_string(), serde_json::json!("read"));

    let mut verb_args = legacy_args.clone();
    verb_args.insert("action".to_string(), serde_json::json!("check"));

    let legacy = admin_call_text("sandbox_check", legacy_args).await;
    let verb = admin_call_text("tachi_sandbox", verb_args).await;

    assert!(!legacy.is_empty(), "legacy check produced no payload");
    assert_eq!(
        legacy, verb,
        "sandbox_check alias and tachi_sandbox(action='check') must return identical output"
    );
}

#[tokio::test]
async fn get_policy_golden_matches_between_alias_and_verb() {
    let mut legacy_args = serde_json::Map::new();
    legacy_args.insert("capability_id".to_string(), serde_json::json!("mcp:exa"));

    let mut verb_args = legacy_args.clone();
    verb_args.insert("action".to_string(), serde_json::json!("get_policy"));

    let legacy = admin_call_text("sandbox_get_policy", legacy_args).await;
    let verb = admin_call_text("tachi_sandbox", verb_args).await;

    assert!(!legacy.is_empty(), "legacy get_policy produced no payload");
    assert_eq!(
        legacy, verb,
        "sandbox_get_policy alias and tachi_sandbox(action='get_policy') must return identical output"
    );
}

// ── Alias-manifest tripwires ────────────────────────────────────────────────

/// A registered alias whose `remove_in_release` has been reached (current
/// package version >= that release) but that is STILL routed is a zombie: the
/// removal was forgotten. Turn it red at that point.
#[test]
fn no_alias_is_routed_past_its_removal_release() {
    let routed: std::collections::BTreeSet<String> = router_descriptions()
        .into_keys()
        .collect();

    for entry in all_aliases() {
        if release_at_or_past(CURRENT_RELEASE, entry.remove_in_release) {
            assert!(
                !routed.contains(entry.legacy_name),
                "alias '{}' should have been removed in {} but is still routed at {}",
                entry.legacy_name,
                entry.remove_in_release,
                CURRENT_RELEASE
            );
        }
    }
}

/// Every routed tool whose description is a `DEPRECATED:` alias must be a
/// registered manifest entry, and its description must carry that entry's exact
/// deprecation prefix — no undocumented zombie aliases, no drifted wording.
#[test]
fn every_deprecated_route_is_a_registered_alias() {
    for (name, description) in router_descriptions() {
        if !description.starts_with("DEPRECATED:") {
            continue;
        }
        let entry = find_alias(&name).unwrap_or_else(|| {
            panic!(
                "routed tool '{name}' advertises DEPRECATED but is not in the alias manifest \
                 (unregistered zombie alias): {description}"
            )
        });
        assert!(
            description.starts_with(&entry.deprecation_prefix()),
            "alias '{name}' description must start with its manifest deprecation prefix \
             '{}' — got: {description}",
            entry.deprecation_prefix()
        );
    }
}

/// Forward direction: every manifest forwarding alias must actually be routed
/// with its deprecation prefix (the manifest can't claim an alias that isn't
/// exposed).
#[test]
fn every_manifest_alias_is_routed_with_prefix() {
    let descriptions = router_descriptions();
    for entry in all_aliases() {
        if entry.alias_kind != AliasKind::ForwardingAlias {
            continue;
        }
        let description = descriptions.get(entry.legacy_name).unwrap_or_else(|| {
            panic!(
                "manifest alias '{}' is not registered on the router",
                entry.legacy_name
            )
        });
        assert!(
            description.starts_with(&entry.deprecation_prefix()),
            "manifest alias '{}' must be routed with its deprecation prefix",
            entry.legacy_name
        );
    }
}
