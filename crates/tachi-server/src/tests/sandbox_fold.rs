//! #757 Cut3-S1 + v2 retirement: contract tests for the folded sandbox verb.
//! The six pre-fold sandbox tools were folded into `tachi_sandbox` (S1) and
//! their forwarding aliases expired at 1.10.0 — the routes were deleted at
//! v2.0.0 and their manifest entries survive as tombstones. This module covers:
//!
//! - the retirement authorization matrix (canonical 6 actions × all 7 tool
//!   profiles, admin-only; retired names unrouted under every profile,
//!   admin included);
//! - absent-from-router/tools/list and exact unknown-tool-error pins for the
//!   six retired names;
//! - output goldens: the canonical `tachi_sandbox` facade vs the **pre-fold
//!   `sandbox_ops` handler oracle** (the unchanged handlers every retired
//!   alias used to forward to) — byte-identical payloads for the pure-read
//!   actions, and for the two write actions identical responses AND identical
//!   persisted state, before and after the write;
//! - the alias-manifest tripwires (version-past-removal, zombie-alias, and the
//!   lifecycle-aware routed-prefix/absence gate).

use super::fold_contract::{
    assert_legacy_names_unrouted_everywhere, assert_verb_actions_admin_only, FoldPair,
};
use super::{call_tool_on_server, call_tool_via_server, make_server};
use crate::sandbox_ops::{
    handle_sandbox_check, handle_sandbox_exec_audit, handle_sandbox_get_policy,
    handle_sandbox_list_policies, handle_sandbox_set_policy, handle_sandbox_set_rule,
};
use crate::tool_params::{
    SandboxCheckParams, SandboxExecAuditParams, SandboxGetPolicyParams, SandboxListPoliciesParams,
    SandboxSetPolicyParams, SandboxSetRuleParams,
};
use crate::tools::alias_manifest::{
    all_aliases, find_alias, parse_release, release_at_or_past, AliasKind, CURRENT_RELEASE,
};
use std::collections::BTreeMap;

/// FoldPairs for the sandbox fold, derived from the manifest so the harness and
/// the manifest can never drift. After the v2 retirement these entries are
/// tombstones: `legacy_name` must be unrouted, `canonical_tool`/`canonical_action`
/// must stay admin-only.
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

/// Run one admin `tachi_sandbox` call through the real MCP choke point and
/// return its text payload.
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

// ── Retirement authorization matrix (#975-proof, v2 shape) ──────────────────

#[tokio::test]
async fn canonical_sandbox_actions_are_admin_only_across_all_seven_profiles() {
    // The six verb actions keep the exact pre-fold reachability: callable only
    // under admin, hidden for the other six profiles. This is the "non-admin
    // profile calling any action is refused" discrimination, pinned absolutely
    // (not merely vs another surface) across the full 6×7 matrix.
    let pairs = sandbox_fold_pairs();
    assert_eq!(
        pairs.len(),
        6,
        "the sandbox fold must still map exactly six canonical actions"
    );
    assert_verb_actions_admin_only(&pairs).await;
}

#[tokio::test]
async fn retired_sandbox_names_are_unrouted_across_all_seven_profiles_even_admin() {
    // The retired aliases must be rejected EVERYWHERE — including the admin
    // profile that used to own them. A retirement that only hid the names from
    // non-admin bundles (but left an admin-only route) fails here.
    let pairs = sandbox_fold_pairs();
    assert_eq!(
        pairs.len(),
        6,
        "the sandbox fold must still map exactly six retired legacy names"
    );
    assert_legacy_names_unrouted_everywhere(&pairs).await;
}

// ── Router census / tools-list absence + exact unknown-tool error ───────────

/// Registry absence is the strongest tools/list pin: a name missing from the
/// router inventory cannot appear in ANY profile's projected tools/list.
#[test]
fn retired_sandbox_names_are_absent_from_the_router_census() {
    let descriptions = router_descriptions();

    for entry in all_aliases() {
        if entry.canonical_tool != "tachi_sandbox" {
            continue;
        }
        assert!(
            !descriptions.contains_key(entry.legacy_name),
            "retired sandbox alias '{}' must not be registered on the router (absent from \
             tools/list for every profile)",
            entry.legacy_name
        );
    }
    let canonical = descriptions
        .get("tachi_sandbox")
        .expect("canonical tachi_sandbox verb must stay registered");
    assert!(
        !canonical.to_ascii_lowercase().contains("deprecated"),
        "tachi_sandbox is the canonical surface now; its description must not self-identify \
         as deprecated: {canonical}"
    );
}

#[tokio::test]
async fn admin_calls_to_retired_sandbox_names_return_the_exact_unknown_tool_error() {
    // Pin the exact rejection text an admin receives for a retired name — the
    // same `tool_not_found_result` wording the choke point emits for any
    // unknown tool. Exact-string (not `contains`) so a wording drift that
    // silently changed what callers see fails loudly.
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("admin").expect("admin profile parses"),
    ));

    for entry in all_aliases() {
        if entry.canonical_tool != "tachi_sandbox" {
            continue;
        }
        let result = call_tool_on_server(
            server.clone(),
            entry.legacy_name,
            Some(serde_json::Map::new()),
        )
        .await
        .unwrap_or_else(|e| {
            panic!(
                "admin call to '{}' should yield a result: {e}",
                entry.legacy_name
            )
        });
        assert_eq!(
            result.is_error,
            Some(true),
            "admin call to retired '{}' must be an error result",
            entry.legacy_name
        );
        let message = result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|text| text.text.clone())
            .unwrap_or_default();
        assert_eq!(
            message,
            format!(
                "tool not found: '{}'. Call tools/list and use an exact visible tool name for \
                 the current TACHI_PROFILE.",
                entry.legacy_name
            ),
            "admin tools/call to retired '{}' must return the exact unknown-tool error",
            entry.legacy_name
        );
    }
}

// ── Output goldens: canonical facade vs pre-fold handler oracle ─────────────
//
// The retired aliases no longer exist as routes, so pre-fold equivalence is
// proven against the *handlers themselves*: `sandbox_ops::handle_sandbox_*`
// are unchanged since the fold, and the canonical facade must produce
// byte-identical output to them. This keeps the fold's core guarantee (same
// bytes, not merely same semantics) testable after the alias routes are gone.

#[tokio::test]
async fn check_payload_is_byte_identical_to_pre_fold_handler_oracle() {
    let server = make_server();
    let oracle = handle_sandbox_check(
        &server,
        SandboxCheckParams {
            agent_role: "code-review".to_string(),
            path: "/project/secrets".to_string(),
            operation: "read".to_string(),
        },
    )
    .await
    .expect("handler-oracle check should succeed");

    let mut verb_args = serde_json::Map::new();
    verb_args.insert("action".to_string(), serde_json::json!("check"));
    verb_args.insert("agent_role".to_string(), serde_json::json!("code-review"));
    verb_args.insert("path".to_string(), serde_json::json!("/project/secrets"));
    verb_args.insert("operation".to_string(), serde_json::json!("read"));
    let verb = admin_call_text("tachi_sandbox", verb_args).await;

    assert!(
        !oracle.is_empty(),
        "handler-oracle check produced no payload"
    );
    assert_eq!(
        oracle, verb,
        "tachi_sandbox(action='check') must return byte-identical output to the unchanged \
         pre-fold handler"
    );
}

#[tokio::test]
async fn get_policy_payload_is_byte_identical_to_pre_fold_handler_oracle() {
    let server = make_server();
    let oracle = handle_sandbox_get_policy(
        &server,
        SandboxGetPolicyParams {
            capability_id: "mcp:exa".to_string(),
        },
    )
    .await
    .expect("handler-oracle get_policy should succeed");

    let mut verb_args = serde_json::Map::new();
    verb_args.insert("action".to_string(), serde_json::json!("get_policy"));
    verb_args.insert("capability_id".to_string(), serde_json::json!("mcp:exa"));
    let verb = admin_call_text("tachi_sandbox", verb_args).await;

    assert!(
        !oracle.is_empty(),
        "handler-oracle get_policy produced no payload"
    );
    assert_eq!(
        oracle, verb,
        "tachi_sandbox(action='get_policy') must return byte-identical output to the unchanged \
         pre-fold handler"
    );
}

#[tokio::test]
async fn list_policies_payload_is_byte_identical_to_pre_fold_handler_oracle() {
    // Fresh server, no policies seeded — deterministic empty-list response on
    // both sides. Oracle uses the serde defaults (enabled_only=false, limit=100)
    // a bare legacy call used to deserialize.
    let server = make_server();
    let oracle = handle_sandbox_list_policies(
        &server,
        SandboxListPoliciesParams {
            enabled_only: false,
            limit: 100,
        },
    )
    .await
    .expect("handler-oracle list_policies should succeed");

    let mut verb_args = serde_json::Map::new();
    verb_args.insert("action".to_string(), serde_json::json!("list_policies"));
    let verb = admin_call_text("tachi_sandbox", verb_args).await;

    assert!(
        !oracle.is_empty(),
        "handler-oracle list_policies produced no payload"
    );
    assert_eq!(
        oracle, verb,
        "tachi_sandbox(action='list_policies') must return byte-identical output to the \
         unchanged pre-fold handler"
    );
}

#[tokio::test]
async fn exec_audit_payload_is_byte_identical_to_pre_fold_handler_oracle() {
    // Fresh server, no exec-audit rows seeded — deterministic empty-list
    // response on both sides. Oracle uses the serde defaults
    // (no filters, limit=100) a bare legacy call used to deserialize.
    let server = make_server();
    let oracle = handle_sandbox_exec_audit(
        &server,
        SandboxExecAuditParams {
            capability_id: None,
            stage: None,
            decision: None,
            limit: 100,
        },
    )
    .await
    .expect("handler-oracle exec_audit should succeed");

    let mut verb_args = serde_json::Map::new();
    verb_args.insert("action".to_string(), serde_json::json!("exec_audit"));
    let verb = admin_call_text("tachi_sandbox", verb_args).await;

    assert!(
        !oracle.is_empty(),
        "handler-oracle exec_audit produced no payload"
    );
    assert_eq!(
        oracle, verb,
        "tachi_sandbox(action='exec_audit') must return byte-identical output to the unchanged \
         pre-fold handler"
    );
}

// ── Write-action goldens: response AND persisted state, before and after ────
//
// The read goldens above only prove the *response* is byte-identical. For a
// write action (`set_rule`, `set_policy`) that alone isn't enough — the oracle
// path (direct pre-fold handler) and the canonical facade path could echo an
// identical acknowledgement while persisting different state (e.g. a
// serialization bug in one path only). These two tests thread a single
// `MemoryServer` (cloned over its shared `Arc<Mutex<MemoryStore>>`, see
// `call_tool_on_server`) through before-write-probe → write → after-write-probe
// — once driving the write through the handler oracle, once through the
// canonical facade route — and probe state with the SAME unchanged handler
// oracle on both paths, asserting every stage matches pairwise.

/// Extract the first text content block from a tool call result.
fn text_of(result: rmcp::model::CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.clone())
        .unwrap_or_default()
}

/// Probe `check` through the pre-fold handler oracle on `server` and return
/// its text payload.
async fn oracle_check_text(
    server: &crate::MemoryServer,
    agent_role: &str,
    path: &str,
    operation: &str,
) -> String {
    handle_sandbox_check(
        server,
        SandboxCheckParams {
            agent_role: agent_role.to_string(),
            path: path.to_string(),
            operation: operation.to_string(),
        },
    )
    .await
    .expect("handler-oracle check probe should succeed")
}

/// Drive one `set_rule` write — either through the pre-fold handler oracle or
/// through the canonical `tachi_sandbox` facade route — and probe `check` via
/// the handler oracle before and after, returning `(before, write_response,
/// after)`.
async fn drive_set_rule(via_canonical_facade: bool) -> (String, String, String) {
    let harness = make_server();
    harness.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("admin").expect("admin profile parses"),
    ));

    let agent_role = "code-review";
    let path_pattern = "/project/golden-rule/*";
    let probe_path = "/project/golden-rule/secret.txt";
    let operation = "write";

    let before = oracle_check_text(&harness, agent_role, probe_path, operation).await;

    let write_response = if via_canonical_facade {
        let mut write_args = serde_json::Map::new();
        write_args.insert("action".to_string(), serde_json::json!("set_rule"));
        write_args.insert("agent_role".to_string(), serde_json::json!(agent_role));
        write_args.insert("path_pattern".to_string(), serde_json::json!(path_pattern));
        write_args.insert("access_level".to_string(), serde_json::json!("deny"));
        let write_result = call_tool_on_server(harness.clone(), "tachi_sandbox", Some(write_args))
            .await
            .expect("canonical set_rule write call should yield a tool result");
        text_of(write_result)
    } else {
        handle_sandbox_set_rule(
            &harness,
            SandboxSetRuleParams {
                agent_role: agent_role.to_string(),
                path_pattern: path_pattern.to_string(),
                access_level: "deny".to_string(),
            },
        )
        .await
        .expect("handler-oracle set_rule write should succeed")
    };

    let after = oracle_check_text(&harness, agent_role, probe_path, operation).await;

    (before, write_response, after)
}

#[tokio::test]
async fn set_rule_golden_matches_response_and_state_before_and_after() {
    let (oracle_before, oracle_write, oracle_after) = drive_set_rule(false).await;
    let (verb_before, verb_write, verb_after) = drive_set_rule(true).await;

    assert!(
        !oracle_before.is_empty(),
        "oracle pre-write check produced no payload"
    );
    assert_eq!(
        oracle_before, verb_before,
        "pre-write check state must be identical on both fresh servers (no rule yet)"
    );
    assert!(
        oracle_before.contains("\"allowed\":true"),
        "sanity: default access (no rule) should be allowed=true before the write, got: {oracle_before}"
    );

    assert!(
        !oracle_write.is_empty(),
        "oracle set_rule produced no payload"
    );
    assert_eq!(
        oracle_write, verb_write,
        "handler-oracle set_rule and tachi_sandbox(action='set_rule') must return identical \
         output"
    );

    assert!(
        !oracle_after.is_empty(),
        "oracle post-write check produced no payload"
    );
    assert_eq!(
        oracle_after, verb_after,
        "post-write check state must be identical between the oracle path and the canonical \
         facade path — a write-action golden must prove persisted state matches, not just the \
         ack response"
    );
    assert!(
        oracle_after.contains("\"allowed\":false"),
        "sanity: the deny rule should be enforced after the write, got: {oracle_after}"
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

/// Probe `get_policy` through the pre-fold handler oracle on `server` and
/// return its text payload.
async fn oracle_get_policy_text(server: &crate::MemoryServer, capability_id: &str) -> String {
    handle_sandbox_get_policy(
        server,
        SandboxGetPolicyParams {
            capability_id: capability_id.to_string(),
        },
    )
    .await
    .expect("handler-oracle get_policy probe should succeed")
}

/// Drive one `set_policy` write — either through the pre-fold handler oracle
/// or through the canonical `tachi_sandbox` facade route — and probe
/// `get_policy` via the handler oracle before and after, returning `(before,
/// write_response, after)`.
async fn drive_set_policy(via_canonical_facade: bool) -> (String, String, String) {
    let harness = make_server();
    harness.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("admin").expect("admin profile parses"),
    ));

    let capability_id = "mcp:golden-policy";

    let before = oracle_get_policy_text(&harness, capability_id).await;

    let write_response = if via_canonical_facade {
        let mut write_args = serde_json::Map::new();
        write_args.insert("action".to_string(), serde_json::json!("set_policy"));
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
        let write_result = call_tool_on_server(harness.clone(), "tachi_sandbox", Some(write_args))
            .await
            .expect("canonical set_policy write call should yield a tool result");
        text_of(write_result)
    } else {
        handle_sandbox_set_policy(
            &harness,
            SandboxSetPolicyParams {
                capability_id: capability_id.to_string(),
                runtime_type: "wasm".to_string(),
                env_allowlist: vec!["PATH".to_string(), "HOME".to_string()],
                fs_read_roots: vec!["/project".to_string()],
                fs_write_roots: vec!["/project/out".to_string()],
                cwd_roots: vec!["/project".to_string()],
                max_startup_ms: 15_000,
                max_tool_ms: 20_000,
                max_concurrency: 2,
                enabled: true,
            },
        )
        .await
        .expect("handler-oracle set_policy write should succeed")
    };

    let after = oracle_get_policy_text(&harness, capability_id).await;

    (before, write_response, after)
}

#[tokio::test]
async fn set_policy_golden_matches_response_and_state_before_and_after() {
    let (oracle_before, oracle_write, oracle_after) = drive_set_policy(false).await;
    let (verb_before, verb_write, verb_after) = drive_set_policy(true).await;

    assert!(
        !oracle_before.is_empty(),
        "oracle pre-write get_policy produced no payload"
    );
    assert_eq!(
        oracle_before, verb_before,
        "pre-write get_policy state must be identical on both fresh servers (no policy yet)"
    );
    assert!(
        oracle_before.contains("\"error\""),
        "sanity: get_policy should report not-found before the write, got: {oracle_before}"
    );

    assert!(
        !oracle_write.is_empty(),
        "oracle set_policy produced no payload"
    );
    assert_eq!(
        oracle_write, verb_write,
        "handler-oracle set_policy and tachi_sandbox(action='set_policy') must return identical \
         output"
    );

    assert!(
        !oracle_after.is_empty(),
        "oracle post-write get_policy produced no payload"
    );
    assert_eq!(
        normalize_policy_state(&oracle_after),
        normalize_policy_state(&verb_after),
        "post-write get_policy state (ignoring created_at/updated_at wall-clock timestamps) \
         must be identical between the oracle path and the canonical facade path — a \
         write-action golden must prove persisted state matches, not just the ack response. \
         oracle={oracle_after} verb={verb_after}"
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
/// #1099: the `#1016` exemption that used to live here (`handoff_leave`/
/// `handoff_check`/`tachi_handoff` said "DEPRECATED" but signaled it via a
/// response-embedded `deprecated` field instead of `ALIAS_MANIFEST`) is
/// gone. `handoff_leave`/`handoff_check` no longer exist as routes, and
/// `tachi_handoff`'s description no longer says "deprecated" (it now
/// documents its one surviving action, `promote_issue`, which was never
/// deprecated). The gate below is fully strict again — no exemptions.
///
/// v2: with the six sandbox aliases deleted, no routed tool self-identifies as
/// deprecated; the gate stays as a live drift tripwire for any future fold
/// that re-introduces one without registering it here.
#[test]
fn every_deprecated_route_is_a_registered_alias() {
    for (name, description) in router_descriptions() {
        if !description.to_ascii_lowercase().contains("deprecated") {
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

/// Lifecycle-aware routed-prefix gate. The pre-v2 form asserted every manifest
/// alias was routed with its deprecation prefix; after the v2 retirement that
/// would be exactly backwards for tombstones, so the gate now branches on
/// [`AliasKind`] and carries a MANDATORY assertion for the only remaining
/// kind:
///
/// - `RetiredAlias` (tombstone): MUST NOT be routed at all — the mandatory
///   absence guard. A resurrected zombie route fails here (and re-adding a
///   name requires a new manifest entry with a fresh lifecycle).
///
/// The `ForwardingAlias` (live) branch — routed with the exact manifest
/// deprecation prefix — existed while the sandbox aliases were live and was
/// removed with the variant: it had no constructor left once every entry
/// became a tombstone. An S2–S7 fold re-adds the variant and this branch in
/// the same change that registers its live entries. Until then,
/// `every_deprecated_route_is_a_registered_alias` below remains the live
/// drift gate that catches any route self-identifying as deprecated without a
/// manifest entry.
#[test]
fn manifest_aliases_match_their_router_lifecycle() {
    let descriptions = router_descriptions();
    for entry in all_aliases() {
        match entry.alias_kind {
            AliasKind::RetiredAlias => {
                assert!(
                    !descriptions.contains_key(entry.legacy_name),
                    "retired alias '{}' (removed in {}) must not be routed; re-introducing it \
                     requires a new manifest entry with a fresh lifecycle, not a kind flip",
                    entry.legacy_name,
                    entry.remove_in_release
                );
            }
        }
    }
}

// ── Manifest field consistency (round-2 review fixup) ───────────────────────

/// Every `AliasEntry` field must be consumed by a real assertion, not just
/// written at construction — a field only ever written and never read is
/// exactly the clippy `dead_code` shape, and `#[allow(dead_code)]` would hide
/// a manifest that could silently drift from the live router/handlers. This
/// test reads `introduced_release`, `admin_only`, `destructive`, and
/// `alias_kind` (the four fields no other test touches — `legacy_name`/
/// `canonical_tool`/`canonical_action`/`remove_in_release` are already
/// exercised above) and pins each to a concrete invariant.
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
        // `canonical_sandbox_actions_are_admin_only_across_all_seven_profiles`
        // above; this pins the manifest's own *declaration* so a future entry
        // can't silently flip this bit without both assertions catching it.
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

        // Lifecycle/kind consistency: a RetiredAlias tombstone may only exist
        // once its own deadline has actually been reached — a tombstone whose
        // remove_in_release is still in the future would record a removal
        // that hasn't been authorized by the lifecycle yet.
        match entry.alias_kind {
            AliasKind::RetiredAlias => {
                assert!(
                    release_at_or_past(CURRENT_RELEASE, entry.remove_in_release),
                    "manifest entry '{}' is a RetiredAlias tombstone but its remove_in_release \
                     '{}' has not been reached at {} — premature removal, restore a live \
                     alias kind until the deadline",
                    entry.legacy_name,
                    entry.remove_in_release,
                    CURRENT_RELEASE
                );
            }
        }
    }
}
