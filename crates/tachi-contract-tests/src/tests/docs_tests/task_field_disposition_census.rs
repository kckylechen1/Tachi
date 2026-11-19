//! Census validation for `TachiTaskParams` field dispositions (#1319 C1).
//!
//! Validates `docs/engineering/architecture/task-field-disposition-census-v1.fixture.json`
//! against the live compiled `TachiTaskParams` schema (`rmcp::schemars::schema_for!`),
//! typed `#[schemars(skip)]` back-compat fields, and `TachiTaskAction::PRIMARY`
//! inventory — without reading or text-parsing `.rs` source files.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use tachi_params::{TachiTaskAction, TachiTaskParams};

const FIXTURE: &str = include_str!(
    "../../../../../docs/engineering/architecture/task-field-disposition-census-v1.fixture.json"
);

const EXPECTED_SCHEMA_VERSION: &str = "task_field_disposition.v1";

const ALLOWED_DISPOSITIONS: &[&str] = &[
    "delete",
    "move_to_staff",
    "operator_only_internal",
    "keep_task_shared",
    "keep_task_ledger",
    "defer_1467",
];

const ALLOWED_OWNERS: &[&str] = &["Task", "Staff", "CLI/internal", "deleted"];

const REMOVED_TASK_ACTIONS: &[&str] = &[
    "dispatch",
    "wait",
    "cancel",
    "plan",
    "cycle_plan",
    "recommend",
    "refine_issues",
    "merge",
    "ux_matrix",
    "briefing",
    "doc_index",
    "cycle_status",
];

const OPERATOR_ONLY_INTERNAL_FIELDS: &[&str] = &[
    "env_id",
    "unmanaged_cwd",
    "skills",
    "context_query",
    "model",
    "permission_profile",
    "allowed_tools",
    "completion_predicate",
    "max_turns",
    "sandbox",
    "inject_tachi_mcp",
    "command",
    "harness_transport",
    "harness_server_url",
    "credential_profiles",
    "tool_profile",
    "mcp_access",
    "allowed_mcp_servers",
    "proposal_id",
    "review_status",
];

/// Map a live `schemars` property key on `TachiTaskParams` to its canonical
/// Rust field name in the census fixture.
fn schema_key_to_rust_field(key: &str) -> &str {
    match key {
        "__tachi_project_explicit" => "project_explicit",
        other => other,
    }
}

fn live_public_schema_fields() -> BTreeSet<String> {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("TachiTaskParams schema serializes");
    let properties = value["properties"]
        .as_object()
        .expect("TachiTaskParams schema has properties object");
    properties
        .keys()
        .map(|key| schema_key_to_rust_field(key).to_string())
        .collect()
}

fn live_primary_actions() -> BTreeSet<&'static str> {
    TachiTaskAction::PRIMARY
        .iter()
        .map(|action| action.as_str())
        .collect()
}

fn load_fixture() -> Value {
    serde_json::from_str(FIXTURE)
        .expect("task-field-disposition-census-v1.fixture.json must be valid JSON")
}

fn validate_census(
    fixture: &Value,
    live_public_fields: &BTreeSet<String>,
    internal_fields: &BTreeSet<&str>,
    live_actions: &BTreeSet<&str>,
) -> Vec<String> {
    let mut errors = Vec::new();

    match fixture.get("schema_version").and_then(Value::as_str) {
        Some(EXPECTED_SCHEMA_VERSION) => {}
        other => errors.push(format!(
            "fixture schema_version drifted: expected {EXPECTED_SCHEMA_VERSION:?}, got {other:?}"
        )),
    }

    let Some(fields_obj) = fixture.get("fields").and_then(Value::as_object) else {
        errors.push("fixture missing `fields` object".to_string());
        return errors;
    };

    let expected_total = fields_obj.len();
    match fixture.get("total_fields").and_then(Value::as_u64) {
        Some(claimed) if claimed as usize == expected_total => {}
        other => errors.push(format!(
            "fixture total_fields ({other:?}) != fixture fields count ({expected_total})"
        )),
    }

    let allowed_dispositions: BTreeSet<&str> = ALLOWED_DISPOSITIONS.iter().copied().collect();
    let allowed_owners: BTreeSet<&str> = ALLOWED_OWNERS.iter().copied().collect();
    let removed_actions: BTreeSet<&str> = REMOVED_TASK_ACTIONS.iter().copied().collect();

    let mut public_fixture_fields = BTreeSet::new();
    let mut internal_fixture_fields = BTreeSet::new();
    let mut actual_by_disposition: BTreeMap<&str, u64> = BTreeMap::new();

    for (name, entry) in fields_obj {
        let Some(disposition) = entry.get("disposition").and_then(Value::as_str) else {
            errors.push(format!("field {name} missing disposition"));
            continue;
        };
        if !allowed_dispositions.contains(disposition) {
            errors.push(format!(
                "field {name} has unknown disposition {disposition:?}"
            ));
        } else {
            *actual_by_disposition.entry(disposition).or_insert(0) += 1;
        }

        match entry.get("canonical_future_owner").and_then(Value::as_str) {
            Some(owner) if allowed_owners.contains(owner) => {}
            Some(owner) => errors.push(format!(
                "field {name} has unknown canonical_future_owner {owner:?}"
            )),
            None => errors.push(format!("field {name} missing canonical_future_owner")),
        }

        let Some(actions) = entry.get("used_by_actions").and_then(Value::as_array) else {
            errors.push(format!("field {name} missing used_by_actions"));
            continue;
        };
        let mut action_names = Vec::new();
        for action in actions {
            match action.as_str() {
                Some(token) => {
                    action_names.push(token);
                    if !live_actions.contains(token) {
                        errors.push(format!(
                            "{name}: used_by_actions contains '{token}' (not a live PRIMARY action)"
                        ));
                    }
                    if removed_actions.contains(token) {
                        errors.push(format!(
                            "{name}: used_by_actions contains REMOVED action '{token}'"
                        ));
                    }
                }
                None => errors.push(format!(
                    "field {name} has non-string action entry: {action:?}"
                )),
            }
        }

        match entry.get("rationale").and_then(Value::as_str) {
            Some(rationale) if !rationale.trim().is_empty() => {}
            _ => errors.push(format!("field {name} has empty or missing rationale")),
        }

        if disposition == "operator_only_internal" {
            internal_fixture_fields.insert(name.as_str());
            if !action_names.is_empty() {
                errors.push(format!(
                    "operator_only_internal field {name} must not name live actions: {action_names:?}"
                ));
            }
        } else {
            public_fixture_fields.insert(name.clone());
            if action_names.is_empty() {
                errors.push(format!(
                    "public schema field {name} ({disposition}) must list at least one surviving action"
                ));
            }
        }
    }

    // Live public schema properties <-> non-internal fixture entries must match exactly.
    for live in live_public_fields {
        if !public_fixture_fields.contains(live) {
            if internal_fixture_fields.contains(live.as_str()) {
                errors.push(format!(
                    "operator_only_internal field {live} leaked into live public TachiTaskParams schema"
                ));
            } else {
                errors.push(format!(
                    "live public TachiTaskParams field {live} missing from census fixture"
                ));
            }
        }
    }
    for claimed in &public_fixture_fields {
        if !live_public_fields.contains(claimed) {
            errors.push(format!(
                "census fixture claims public field {claimed} absent from live TachiTaskParams schema"
            ));
        }
    }

    // Internal (#[schemars(skip)]) fields <-> operator_only_internal fixture entries must match.
    for internal in internal_fields {
        if !internal_fixture_fields.contains(*internal) {
            errors.push(format!(
                "typed internal TachiTaskParams field {internal} missing operator_only_internal census entry"
            ));
        }
    }
    for claimed in &internal_fixture_fields {
        if !internal_fields.contains(claimed) {
            errors.push(format!(
                "census fixture claims operator_only_internal field {claimed} not in typed internal inventory"
            ));
        }
    }

    // disposition_summary counts must sum to total_fields and match per-disposition totals.
    if let Some(summary) = fixture
        .get("disposition_summary")
        .and_then(Value::as_object)
    {
        let summary_sum: u64 = summary.values().filter_map(Value::as_u64).sum();
        if summary_sum != expected_total as u64 {
            errors.push(format!(
                "disposition_summary counts sum to {summary_sum} but fields count is {expected_total}"
            ));
        }
        for (disposition, actual_count) in &actual_by_disposition {
            let claimed = summary
                .get(*disposition)
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if *actual_count != claimed {
                errors.push(format!(
                    "disposition {disposition}: fixture has {actual_count} entries but summary claims {claimed}"
                ));
            }
        }
        for (disposition, claimed) in summary {
            let actual = actual_by_disposition
                .get(disposition.as_str())
                .copied()
                .unwrap_or(0);
            let claimed_val = claimed.as_u64().unwrap_or(0);
            if actual != claimed_val {
                errors.push(format!(
                    "disposition {disposition}: summary claims {claimed_val} but fixture has {actual} entries"
                ));
            }
        }
    } else {
        errors.push("fixture missing `disposition_summary` object".to_string());
    }

    errors
}

#[test]
fn task_field_disposition_census_matches_live_schema_and_typed_contract() {
    let live_public = live_public_schema_fields();
    assert!(
        !live_public.is_empty(),
        "live TachiTaskParams schema must expose properties"
    );
    let internal: BTreeSet<&str> = OPERATOR_ONLY_INTERNAL_FIELDS.iter().copied().collect();
    let live_actions = live_primary_actions();
    let fixture = load_fixture();

    let errors = validate_census(&fixture, &live_public, &internal, &live_actions);
    assert!(
        errors.is_empty(),
        "task field disposition census drifted:\n{}",
        errors.join("\n")
    );
}

#[test]
fn operator_only_internal_fields_remain_hidden_from_schema_and_deserializable() {
    let live_public = live_public_schema_fields();
    for field in OPERATOR_ONLY_INTERNAL_FIELDS {
        assert!(
            !live_public.contains(*field),
            "operator_only_internal field {field} must stay hidden (#[schemars(skip)]) from public MCP schema"
        );
    }

    // Stored-JSON back-compat: every #[schemars(skip)] field still exists on the
    // typed struct and deserializes cleanly when present in persisted JSON.
    let params: TachiTaskParams = serde_json::from_value(json!({
        "action": "status",
        "env_id": "env-1",
        "unmanaged_cwd": true,
        "skills": ["rust"],
        "context_query": "ctx",
        "model": "sonnet",
        "permission_profile": "read_only",
        "allowed_tools": ["read_file"],
        "completion_predicate": {"type": "artifact_non_empty", "path": "result.md"},
        "max_turns": 5,
        "sandbox": "workspace-write",
        "inject_tachi_mcp": false,
        "command": ["echo", "ok"],
        "harness_transport": "stdio",
        "harness_server_url": "http://127.0.0.1:7919",
        "credential_profiles": ["default"],
        "tool_profile": "observe",
        "mcp_access": {"inject_tachi_mcp": false},
        "allowed_mcp_servers": ["tachi"],
        "proposal_id": "prop-1",
        "review_status": "pending"
    }))
    .expect("all operator_only_internal back-compat fields deserialize");

    assert_eq!(params.env_id.as_deref(), Some("env-1"));
    assert_eq!(params.unmanaged_cwd, Some(true));
    assert_eq!(params.skills, vec!["rust"]);
    assert_eq!(params.context_query.as_deref(), Some("ctx"));
    assert_eq!(params.model.as_deref(), Some("sonnet"));
    assert_eq!(params.permission_profile.as_deref(), Some("read_only"));
    assert_eq!(params.allowed_tools, vec!["read_file"]);
    assert!(params.completion_predicate.is_some());
    assert_eq!(params.max_turns, Some(5));
    assert_eq!(params.sandbox.as_deref(), Some("workspace-write"));
    assert_eq!(params.inject_tachi_mcp, Some(false));
    assert_eq!(params.command, vec!["echo", "ok"]);
    assert_eq!(params.harness_transport.as_deref(), Some("stdio"));
    assert_eq!(
        params.harness_server_url.as_deref(),
        Some("http://127.0.0.1:7919")
    );
    assert_eq!(params.credential_profiles, vec!["default"]);
    assert_eq!(params.tool_profile.as_deref(), Some("observe"));
    assert!(params.mcp_access.is_some());
    assert_eq!(params.allowed_mcp_servers, vec!["tachi"]);
    assert_eq!(params.proposal_id.as_deref(), Some("prop-1"));
    assert_eq!(params.review_status.as_deref(), Some("pending"));
}

#[test]
fn census_rejects_missing_disposition_phantom_field_leak_and_summary_drift() {
    let fixture = load_fixture();
    let live_public = live_public_schema_fields();
    let internal: BTreeSet<&str> = OPERATOR_ONLY_INTERNAL_FIELDS.iter().copied().collect();
    let live_actions = live_primary_actions();

    // 1. Live schema gains a field not present in the fixture -> rejected.
    let mut with_new_public = live_public.clone();
    with_new_public.insert("undocumented_new_field".to_string());
    let missing_err = validate_census(&fixture, &with_new_public, &internal, &live_actions);
    assert!(
        missing_err
            .iter()
            .any(|msg| msg.contains("undocumented_new_field") && msg.contains("missing")),
        "must reject live schema field missing from fixture: {missing_err:?}"
    );

    // 2. Fixture claims a public field that left the live schema -> rejected.
    let mut without_action = live_public.clone();
    without_action.remove("release_reason");
    let phantom_err = validate_census(&fixture, &without_action, &internal, &live_actions);
    assert!(
        phantom_err
            .iter()
            .any(|msg| msg.contains("release_reason") && msg.contains("absent")),
        "must reject phantom fixture entry absent from live schema: {phantom_err:?}"
    );

    // 3. An operator_only_internal field leaks into the public schema -> rejected.
    let mut leaked_internal = live_public.clone();
    leaked_internal.insert("env_id".to_string());
    let leak_err = validate_census(&fixture, &leaked_internal, &internal, &live_actions);
    assert!(
        leak_err
            .iter()
            .any(|msg| msg.contains("env_id") && msg.contains("leaked")),
        "must reject operator_only_internal field leaking into public schema: {leak_err:?}"
    );

    // 4. Stale/removed action in used_by_actions -> rejected.
    let mut stale_action = fixture.clone();
    stale_action["fields"]["task"]["used_by_actions"] = json!(["dispatch"]);
    let action_err = validate_census(&stale_action, &live_public, &internal, &live_actions);
    assert!(
        action_err
            .iter()
            .any(|msg| msg.contains("dispatch") && msg.contains("REMOVED")),
        "must reject removed action in used_by_actions: {action_err:?}"
    );

    // 5. Summary count drift -> rejected.
    let mut drifted_summary = fixture;
    drifted_summary["disposition_summary"]["keep_task_shared"] = json!(999);
    let summary_err = validate_census(&drifted_summary, &live_public, &internal, &live_actions);
    assert!(
        summary_err
            .iter()
            .any(|msg| msg.contains("disposition_summary")),
        "must reject disposition_summary count drift: {summary_err:?}"
    );
}
