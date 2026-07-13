//! #757 Cut3-S1: contract tests for folding the six sandbox tools into the
//! `tachi_sandbox` verb. Covers the fold-invariant authorization matrix
//! (`fold_contract` harness), old↔new output goldens for the pure-read actions,
//! and the two alias-manifest tripwires (version-past-removal, zombie-alias).

use super::fold_contract::{assert_admin_only, assert_fold_matrix_equivalence, FoldPair};
use super::{call_tool_on_server, call_tool_via_server, make_server};
use crate::tools::alias_manifest::{
    all_aliases, find_alias, parse_release, release_at_or_past, AliasKind, CURRENT_RELEASE,
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
async fn admin_call_text(tool: &str, args: serde_json::Map<String, serde_json::Value>) -> String {
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

#[tokio::test]
async fn list_policies_golden_matches_between_alias_and_verb() {
    // Fresh server, no policies seeded — deterministic empty-list response on
    // both sides.
    let legacy_args = serde_json::Map::new();
    let mut verb_args = serde_json::Map::new();
    verb_args.insert("action".to_string(), serde_json::json!("list_policies"));

    let legacy = admin_call_text("sandbox_list_policies", legacy_args).await;
    let verb = admin_call_text("tachi_sandbox", verb_args).await;

    assert!(
        !legacy.is_empty(),
        "legacy list_policies produced no payload"
    );
    assert_eq!(
        legacy, verb,
        "sandbox_list_policies alias and tachi_sandbox(action='list_policies') must return identical output"
    );
}

#[tokio::test]
async fn exec_audit_golden_matches_between_alias_and_verb() {
    // Fresh server, no exec-audit rows seeded — deterministic empty-list
    // response on both sides.
    let legacy_args = serde_json::Map::new();
    let mut verb_args = serde_json::Map::new();
    verb_args.insert("action".to_string(), serde_json::json!("exec_audit"));

    let legacy = admin_call_text("sandbox_exec_audit", legacy_args).await;
    let verb = admin_call_text("tachi_sandbox", verb_args).await;

    assert!(!legacy.is_empty(), "legacy exec_audit produced no payload");
    assert_eq!(
        legacy, verb,
        "sandbox_exec_audit alias and tachi_sandbox(action='exec_audit') must return identical output"
    );
}

// ── Write-action goldens: response AND persisted state, before and after ───
//
// `check_golden`/`get_policy_golden`/`list_policies_golden`/`exec_audit_golden`
// above only prove the *response* is byte-identical for pure reads. For a
// write action (`set_rule`, `set_policy`) that alone isn't enough — an alias
// and its canonical verb could echo an identical acknowledgement while
// persisting different state (e.g. a serialization bug in one path only).
// These two tests thread a single `MemoryServer` (cloned over its shared
// `Arc<Mutex<MemoryStore>>`, see `call_tool_on_server`) through
// before-write-probe → write → after-write-probe, once per alias/verb path,
// and assert every stage matches pairwise.

/// Extract the first text content block from a tool call result.
fn text_of(result: rmcp::model::CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.clone())
        .unwrap_or_default()
}

/// Drive `sandbox_check` on `server` and return its text payload.
async fn check_text(
    server: crate::MemoryServer,
    agent_role: &str,
    path: &str,
    operation: &str,
) -> String {
    let mut args = serde_json::Map::new();
    args.insert("agent_role".to_string(), serde_json::json!(agent_role));
    args.insert("path".to_string(), serde_json::json!(path));
    args.insert("operation".to_string(), serde_json::json!(operation));
    let result = call_tool_on_server(server, "sandbox_check", Some(args))
        .await
        .expect("sandbox_check probe call should yield a tool result");
    text_of(result)
}

/// Set a sandbox rule via `set_rule_tool` (either `sandbox_set_rule` or
/// `tachi_sandbox` with `action='set_rule'`) and probe `sandbox_check` before
/// and after, returning `(before, write_response, after)`.
async fn drive_set_rule(set_rule_tool: &str) -> (String, String, String) {
    let harness = make_server();
    harness.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("admin").expect("admin profile parses"),
    ));

    let agent_role = "code-review";
    let path_pattern = "/project/golden-rule/*";
    let probe_path = "/project/golden-rule/secret.txt";
    let operation = "write";

    let before = check_text(harness.clone(), agent_role, probe_path, operation).await;

    let mut write_args = serde_json::Map::new();
    write_args.insert("agent_role".to_string(), serde_json::json!(agent_role));
    write_args.insert("path_pattern".to_string(), serde_json::json!(path_pattern));
    write_args.insert("access_level".to_string(), serde_json::json!("deny"));
    if set_rule_tool == "tachi_sandbox" {
        write_args.insert("action".to_string(), serde_json::json!("set_rule"));
    }
    let write_result = call_tool_on_server(harness.clone(), set_rule_tool, Some(write_args))
        .await
        .expect("set_rule write call should yield a tool result");
    let write_response = text_of(write_result);

    let after = check_text(harness.clone(), agent_role, probe_path, operation).await;

    (before, write_response, after)
}

#[tokio::test]
async fn set_rule_golden_matches_response_and_state_before_and_after() {
    let (legacy_before, legacy_write, legacy_after) = drive_set_rule("sandbox_set_rule").await;
    let (verb_before, verb_write, verb_after) = drive_set_rule("tachi_sandbox").await;

    assert!(
        !legacy_before.is_empty(),
        "legacy pre-write check produced no payload"
    );
    assert_eq!(
        legacy_before, verb_before,
        "pre-write sandbox_check state must be identical on both fresh servers (no rule yet)"
    );
    assert!(
        legacy_before.contains("\"allowed\":true"),
        "sanity: default access (no rule) should be allowed=true before the write, got: {legacy_before}"
    );

    assert!(
        !legacy_write.is_empty(),
        "legacy set_rule produced no payload"
    );
    assert_eq!(
        legacy_write, verb_write,
        "sandbox_set_rule alias and tachi_sandbox(action='set_rule') must return identical output"
    );

    assert!(
        !legacy_after.is_empty(),
        "legacy post-write check produced no payload"
    );
    assert_eq!(
        legacy_after, verb_after,
        "post-write sandbox_check state must be identical between alias and verb paths — \
         a write-action golden must prove persisted state matches, not just the ack response"
    );
    assert!(
        legacy_after.contains("\"allowed\":false"),
        "sanity: the deny rule should be enforced after the write, got: {legacy_after}"
    );
}

/// Strip `created_at`/`updated_at` (wall-clock, not deterministic across two
/// independently-driven servers) before comparing `get_policy` payloads.
fn normalize_policy_state(text: &str) -> serde_json::Value {
    let mut value: serde_json::Value = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("get_policy response must be valid JSON: {e}; got: {text}"));
    if let Some(obj) = value.as_object_mut() {
        obj.remove("created_at");
        obj.remove("updated_at");
    }
    value
}

/// Drive `get_policy` on `server` and return its text payload.
async fn get_policy_text(server: crate::MemoryServer, capability_id: &str) -> String {
    let mut args = serde_json::Map::new();
    args.insert(
        "capability_id".to_string(),
        serde_json::json!(capability_id),
    );
    let result = call_tool_on_server(server, "sandbox_get_policy", Some(args))
        .await
        .expect("sandbox_get_policy probe call should yield a tool result");
    text_of(result)
}

/// Set a sandbox runtime policy via `set_policy_tool` and probe `get_policy`
/// before and after, returning `(before, write_response, after)`.
async fn drive_set_policy(set_policy_tool: &str) -> (String, String, String) {
    let harness = make_server();
    harness.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("admin").expect("admin profile parses"),
    ));

    let capability_id = "mcp:golden-policy";

    let before = get_policy_text(harness.clone(), capability_id).await;

    let mut write_args = serde_json::Map::new();
    write_args.insert(
        "capability_id".to_string(),
        serde_json::json!(capability_id),
    );
    write_args.insert("runtime_type".to_string(), serde_json::json!("wasm"));
    write_args.insert(
        "env_allowlist".to_string(),
        serde_json::json!(["PATH", "HOME"]),
    );
    write_args.insert("fs_read_roots".to_string(), serde_json::json!(["/project"]));
    write_args.insert(
        "fs_write_roots".to_string(),
        serde_json::json!(["/project/out"]),
    );
    write_args.insert("cwd_roots".to_string(), serde_json::json!(["/project"]));
    write_args.insert("max_startup_ms".to_string(), serde_json::json!(15_000));
    write_args.insert("max_tool_ms".to_string(), serde_json::json!(20_000));
    write_args.insert("max_concurrency".to_string(), serde_json::json!(2));
    write_args.insert("enabled".to_string(), serde_json::json!(true));
    if set_policy_tool == "tachi_sandbox" {
        write_args.insert("action".to_string(), serde_json::json!("set_policy"));
    }
    let write_result = call_tool_on_server(harness.clone(), set_policy_tool, Some(write_args))
        .await
        .expect("set_policy write call should yield a tool result");
    let write_response = text_of(write_result);

    let after = get_policy_text(harness.clone(), capability_id).await;

    (before, write_response, after)
}

#[tokio::test]
async fn set_policy_golden_matches_response_and_state_before_and_after() {
    let (legacy_before, legacy_write, legacy_after) = drive_set_policy("sandbox_set_policy").await;
    let (verb_before, verb_write, verb_after) = drive_set_policy("tachi_sandbox").await;

    assert!(
        !legacy_before.is_empty(),
        "legacy pre-write get_policy produced no payload"
    );
    assert_eq!(
        legacy_before, verb_before,
        "pre-write get_policy state must be identical on both fresh servers (no policy yet)"
    );
    assert!(
        legacy_before.contains("\"error\""),
        "sanity: get_policy should report not-found before the write, got: {legacy_before}"
    );

    assert!(
        !legacy_write.is_empty(),
        "legacy set_policy produced no payload"
    );
    assert_eq!(
        legacy_write, verb_write,
        "sandbox_set_policy alias and tachi_sandbox(action='set_policy') must return identical output"
    );

    assert!(
        !legacy_after.is_empty(),
        "legacy post-write get_policy produced no payload"
    );
    assert_eq!(
        normalize_policy_state(&legacy_after),
        normalize_policy_state(&verb_after),
        "post-write get_policy state (ignoring created_at/updated_at wall-clock timestamps) \
         must be identical between alias and verb paths — a write-action golden must prove \
         persisted state matches, not just the ack response. legacy={legacy_after} verb={verb_after}"
    );
}

// ── Alias-manifest tripwires ────────────────────────────────────────────────

/// A registered alias whose `remove_in_release` has been reached (current
/// package version >= that release) but that is STILL routed is a zombie: the
/// removal was forgotten. Turn it red at that point.
#[test]
fn no_alias_is_routed_past_its_removal_release() {
    let routed: std::collections::BTreeSet<String> = router_descriptions().into_keys().collect();

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

/// Every routed tool whose description flags itself as deprecated must be a
/// registered manifest entry, and its description must carry that entry's exact
/// deprecation prefix — no undocumented zombie aliases, no drifted wording.
///
/// Round-2 review fixup: the original gate only matched an exact
/// `starts_with("DEPRECATED:")` prefix, so a future alias whose description
/// says e.g. "Deprecated - use tachi_sandbox(...)" or buries "deprecated"
/// mid-sentence would silently skip this tripwire entirely (the zombie-alias
/// case this test exists to catch). Match case-insensitively anywhere in the
/// description instead, so any wording choice that self-identifies as
/// deprecated still gets held to the manifest.
///
/// One explicit exemption: `#1016`'s handoff routes (`handoff_leave`,
/// `handoff_check`, `tachi_handoff`) already say "DEPRECATED" in their
/// descriptions but signal deprecation through a DIFFERENT, pre-existing
/// mechanism — a `deprecated` field embedded in the JSON *response* (see
/// `handoff_tests.rs`), not `ALIAS_MANIFEST`/`AliasEntry`. They predate this
/// widened contains-match and were never routed through this manifest, so
/// without the exemption the widening (correctly) flags them as unregistered
/// — but registering them here would be speaking for #1016's own compat
/// contract, which is out of scope for the #757 Cut3-S1 sandbox fold. Keep
/// this list named and reviewable rather than silently narrowing the match.
const NON_MANIFEST_DEPRECATION_MECHANISMS: &[&str] =
    &["handoff_leave", "handoff_check", "tachi_handoff"];

#[test]
fn every_deprecated_route_is_a_registered_alias() {
    for (name, description) in router_descriptions() {
        if !description.to_ascii_lowercase().contains("deprecated") {
            continue;
        }
        if NON_MANIFEST_DEPRECATION_MECHANISMS.contains(&name.as_str()) {
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

// ── Manifest field consistency (round-2 review fixup) ───────────────────────

/// Every `AliasEntry` field must be consumed by a real assertion, not just
/// written at construction — a field only ever written and never read is
/// exactly the clippy `dead_code` shape, and `#[allow(dead_code)]` would hide
/// a manifest that could silently drift from the live router/handlers. This
/// test reads `introduced_release`, `admin_only`, and `destructive` (the
/// three fields no other test touches — `legacy_name`/`canonical_tool`/
/// `canonical_action`/`alias_kind`/`remove_in_release` are already exercised
/// above) and pins each to a concrete invariant.
#[test]
fn manifest_entries_have_consistent_lifecycle_and_destructive_fields() {
    for entry in all_aliases() {
        // introduced_release must parse and must not be later than
        // remove_in_release — a manifest bug (typo'd version) should fail
        // loud here rather than silently never tripping the tripwire.
        assert!(
            parse_release(entry.introduced_release).is_some(),
            "manifest entry '{}' has an unparseable introduced_release '{}'",
            entry.legacy_name,
            entry.introduced_release,
        );
        assert!(
            release_at_or_past(entry.remove_in_release, entry.introduced_release),
            "manifest entry '{}' claims remove_in_release '{}' before its own \
             introduced_release '{}' — an alias can't be removed before it was introduced",
            entry.legacy_name,
            entry.remove_in_release,
            entry.introduced_release,
        );

        // Every Cut3-S1 sandbox alias was admin-only pre-fold and the fold's
        // whole point is that this doesn't change. The behavioral proof is
        // `sandbox_fold_is_admin_only_both_sides` above; this pins the
        // manifest's own *declaration* so a future entry can't silently flip
        // this bit without both assertions catching it.
        assert!(
            entry.admin_only,
            "manifest entry '{}' declares admin_only=false, but every Cut3-S1 sandbox \
             alias must be admin-only",
            entry.legacy_name,
        );

        // The manifest's per-action destructive bit must match what the
        // pre-fold handler actually does: `set_rule`/`set_policy` mutate
        // state (write a sandbox rule / runtime policy); `check`/
        // `get_policy`/`list_policies`/`exec_audit` are pure reads. This is
        // the source the S2+ action-level destructive_hint direction (see
        // server_handler::annotate_tool) will eventually read from, so it
        // must stay accurate even though today's MCP hint is tool-level.
        let expected_destructive = matches!(entry.canonical_action, "set_rule" | "set_policy");
        assert_eq!(
            entry.destructive, expected_destructive,
            "manifest entry '{}' (canonical_action='{}') declares destructive={}, \
             expected {} — set_rule/set_policy mutate state, the other sandbox \
             actions are pure reads",
            entry.legacy_name, entry.canonical_action, entry.destructive, expected_destructive,
        );
    }
}
