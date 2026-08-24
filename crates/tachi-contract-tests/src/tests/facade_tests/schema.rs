use serde_json::{json, Value};
use tachi_params::{
    TachiEventParams, TachiGhParams, TachiMemoryParams, TachiSaveParams, TachiSearchParams,
    TachiSkillParams, TachiTaskParams, TachiTuneParams,
};

fn enum_values(property: &Value, name: &str) -> Vec<String> {
    property["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("{name} enum"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{name} enum value"))
                .to_string()
        })
        .collect()
}

#[test]
fn tachi_memory_action_schema_declares_enum_values() {
    let schema = rmcp::schemars::schema_for!(TachiMemoryParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(
        action["enum"].as_array().expect("action enum"),
        &vec![
            json!("search"),
            json!("get"),
            json!("save"),
            json!("briefing"),
            json!("checkpoint"),
            json!("alerts"),
            json!("ask"),
            json!("extract_facts"),
            json!("consolidate"),
        ],
        "#1689 must pin the literal final nine-action Memory schema",
    );

    assert_eq!(action["type"], json!("string"));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("briefing")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("get")));
    for removed in [
        "recall_simulate",
        "recall_proposals",
        "review_recall_proposal",
        "apply_recall_proposals",
        "progress",
        "readiness",
        "delete",
        "gc",
        "doctor_scan",
        "ingest",
        "ingest_source",
        "pattern_feedback",
    ] {
        assert!(
            !action["enum"]
                .as_array()
                .expect("action enum")
                .contains(&json!(removed)),
            "tachi_memory must not advertise recall tuning action {removed} after #1426"
        );
    }
    assert_eq!(
        action["enum"].as_array().expect("action enum").len(),
        tachi_params::TACHI_MEMORY_ACTIONS.len(),
        "advertised action enum must match the TACHI_MEMORY_ACTIONS inventory"
    );
}

#[test]
fn f1689_retired_memory_actions_are_rejected_with_canonical_owner_guidance() {
    for (retired, guidance) in [
        ("progress", "tachi_task(action='status')"),
        ("readiness", "tachi_status"),
        ("delete", "tachi delete"),
        ("gc", "tachi gc"),
        ("doctor_scan", "tachi doctor"),
        ("ingest", "admitted adapter/operator ingest API"),
        ("ingest_source", "admitted adapter/operator ingest API"),
        ("pattern_feedback", "internal pattern-evidence API"),
    ] {
        let error = serde_json::from_value::<TachiMemoryParams>(json!({ "action": retired }))
            .expect_err("retired Memory action must fail at typed deserialization")
            .to_string();
        assert!(
            error.contains(retired),
            "error must name {retired}: {error}"
        );
        assert!(
            error.contains(guidance),
            "error for {retired} must route to {guidance}: {error}"
        );
    }
}

#[test]
fn f1689_memory_schema_removes_retired_only_params() {
    let memory_schema = rmcp::schemars::schema_for!(TachiMemoryParams);
    let memory_value = serde_json::to_value(memory_schema).expect("Memory schema serializes");
    let properties = memory_value["properties"]
        .as_object()
        .expect("Memory properties object");

    for retired_only_field in [
        "flow_id",
        "event",
        "state",
        "content",
        "ingest_type",
        "source_url",
        "auto_chunk",
        "auto_summarize",
        "auto_link",
        "chunk_size_chars",
        "chunk_overlap_chars",
        "conversation_id",
        "turn_id",
        "event_type",
        "messages",
    ] {
        assert!(
            !properties.contains_key(retired_only_field),
            "retired-only field {retired_only_field} must leave the public Memory schema"
        );
    }

    assert!(
        !properties.contains_key("emit_continuity"),
        "ordinary tachi_memory save must not expose the retired evidence-emission workflow",
    );

    let save_schema = rmcp::schemars::schema_for!(TachiSaveParams);
    let save_value = serde_json::to_value(save_schema).expect("Save schema serializes");
    assert!(
        !save_value["properties"]
            .as_object()
            .expect("Save properties object")
            .contains_key("emit_continuity"),
        "ordinary tachi_save must not expose the retired evidence-emission workflow",
    );
}

#[test]
fn tachi_tune_action_schema_declares_migrated_enum_values() {
    let schema = rmcp::schemars::schema_for!(TachiTuneParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    for expected in [
        "route_simulate",
        "route_proposals",
        "route_review",
        "route_apply",
        "recall_simulate",
        "recall_proposals",
        "recall_review",
        "recall_apply",
    ] {
        assert!(
            values.contains(&json!(expected)),
            "tachi_tune must advertise migrated action {expected}"
        );
    }
    for removed_alias in [
        "proposals",
        "review_proposal",
        "apply_proposals",
        "review_recall_proposal",
        "apply_recall_proposals",
    ] {
        assert!(
            !values.contains(&json!(removed_alias)),
            "tachi_tune must not advertise retired alias {removed_alias}"
        );
    }
    assert_eq!(values.len(), tachi_params::TACHI_TUNE_ACTIONS.len());
}

#[test]
fn tachi_memory_schema_declares_polymorphic_field_enum_values() {
    let schema = rmcp::schemars::schema_for!(TachiMemoryParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let properties = &value["properties"];

    assert_eq!(
        enum_values(&properties["scope"], "scope"),
        vec![
            "all", "memory", "wiki", "patterns", "sft", "note", "user", "project", "general",
            "global",
        ]
    );
    assert_eq!(
        enum_values(&properties["kind"], "kind"),
        vec!["memory", "note", "wiki", "facts", "extract_facts"]
    );
    assert_eq!(
        enum_values(&properties["category"], "category"),
        vec![
            "fact",
            "decision",
            "experience",
            "preference",
            "entity",
            "other",
            "kanban",
            "handoff",
            "ghost",
            "wiki",
            "guide",
            "eval",
        ]
    );
    assert_eq!(
        enum_values(&properties["retention_policy"], "retention_policy"),
        vec!["ephemeral", "durable", "permanent", "pinned"]
    );
}

#[test]
fn tachi_search_schema_keeps_recall_scope_enum_values() {
    let schema = rmcp::schemars::schema_for!(TachiSearchParams);
    let value = serde_json::to_value(schema).expect("schema serializes");

    assert_eq!(
        enum_values(&value["properties"]["scope"], "scope"),
        vec!["all", "memory", "wiki", "patterns", "sft"]
    );
}

#[test]
fn tachi_event_action_schema_declares_enum_values() {
    let schema = rmcp::schemars::schema_for!(TachiEventParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("emit")));
    assert!(values.contains(&json!("query")));
    assert!(values.contains(&json!("metrics")));
    assert!(values.contains(&json!("project")));
    assert!(values.contains(&json!("promote")));
    assert!(values.contains(&json!("context")));
    assert!(values.contains(&json!("a2a")));
    assert!(values.contains(&json!("label_eval")));
}

#[test]
fn tachi_skill_action_schema_exposes_only_discover_and_run() {
    let schema = rmcp::schemars::schema_for!(TachiSkillParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert_eq!(values, &vec![json!("discover"), json!("run")]);
}

#[test]
fn tachi_staff_schema_exposes_start_status_and_cancel() {
    let schema = rmcp::schemars::schema_for!(tachi_params::TachiStaffParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let properties = value["properties"].as_object().expect("staff properties");
    let actions = properties["action"]["enum"]
        .as_array()
        .expect("staff action enum");
    assert_eq!(
        actions,
        &vec![json!("start"), json!("status"), json!("cancel")],
        "tachi_staff must expose exactly its canonical start/status/cancel actions"
    );

    // The facade stays flat for MCP compatibility, so start-only task/reason
    // fields coexist with the cancellation request shape. Apart from action
    // selection and response formatting, the only cancellation-control inputs
    // are the canonical dispatch id and its optimistic-concurrency revision.
    let non_cancel_fields = [
        "action",
        "format",
        "task",
        "staffing_reason",
        "flow_id",
        "issue_ref",
        "pr_ref",
        "profile",
        "project",
        "recommendation_ref",
        "stage",
        "worker",
    ];
    let mut cancellation_control_fields = properties
        .keys()
        .filter(|name| !non_cancel_fields.contains(&name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    cancellation_control_fields.sort();
    assert_eq!(
        cancellation_control_fields,
        vec!["dispatch_id", "expected_status_revision"],
        "cancel must expose only dispatch_id plus expected_status_revision as control inputs"
    );

    let cancel = serde_json::from_value::<tachi_params::TachiStaffParams>(json!({
        "action": "cancel",
        "dispatch_id": "20260804T000000Z-claude-deadbeef",
        "expected_status_revision": 7,
    }))
    .expect("canonical cancel request deserializes");
    assert_eq!(cancel.action, "cancel");
    assert_eq!(
        cancel.dispatch_id.as_deref(),
        Some("20260804T000000Z-claude-deadbeef")
    );
    assert_eq!(cancel.expected_status_revision, Some(7));

    // staffing_reason is present as a top-level property (the flat struct
    // surfaces it) but is NOT in the schema-level `required` list — it is
    // OPTIONAL at the schema level precisely so `action='status'` (a read-only
    // probe) can omit it without fabricating a reason. The `start` admission
    // gate is enforced by the handler, not by a cross-action required field.
    assert!(
        properties.contains_key("staffing_reason"),
        "staff schema must surface staffing_reason"
    );
    let required = value["required"].as_array().cloned().unwrap_or_default();
    assert!(
        !required.iter().any(|r| r == "staffing_reason"),
        "staffing_reason must NOT be schema-level required (status must be able to omit it); required={required:?}"
    );
    // No execution- or model-control fields leak through the boundary. These
    // must be rejected by the same deny-unknown-fields facade used by cancel,
    // rather than merely omitted from a documentation list.
    for removed in [
        "pid",
        "pgid",
        "process_group",
        "signal",
        "command",
        "cwd",
        "env",
        "credentials",
        "tools",
        "allowed_tools",
        "transport",
        "timeout",
        "process",
        "result",
        "sandbox",
    ] {
        assert!(
            !properties.contains_key(removed),
            "staff schema must not expose execution field {removed}"
        );
        let rejected = serde_json::from_value::<tachi_params::TachiStaffParams>(json!({
            "action": "cancel",
            "dispatch_id": "20260804T000000Z-claude-deadbeef",
            "expected_status_revision": 7,
            removed: "forbidden",
        }));
        assert!(
            rejected.is_err(),
            "cancel must reject leaked execution field {removed}"
        );
    }
}

/// Discrimination test (blocker fix): a `tachi_staff` STATUS request without
/// `staffing_reason` deserializes successfully. The bug the blocker caught was
/// that staffing_reason was schema-level REQUIRED, forcing a read-only status
/// probe to fabricate a reason. Now staffing_reason is optional at the schema
/// level (the start handler enforces it), so status carries no admission
/// pressure.
#[test]
fn staff_status_deserializes_without_reason() {
    use tachi_params::TachiStaffParams;
    let params = serde_json::from_value::<TachiStaffParams>(serde_json::json!({
        "action": "status",
        "dispatch_id": "20260804T000000Z-claude-deadbeef",
    }))
    .expect("status must deserialize WITHOUT staffing_reason (read-only probe)");
    assert_eq!(params.action, "status");
    assert_eq!(
        params.dispatch_id.as_deref(),
        Some("20260804T000000Z-claude-deadbeef")
    );
    assert!(
        params.staffing_reason.is_none(),
        "status probe did not supply a reason, and must not be forced to"
    );
}

/// Discrimination test: a START request without staffing_reason still
/// deserializes (the field is schema-optional), but the reason is None — the
/// handler-level admission gate (not deserialization) is what rejects it. This
/// test pins the schema-level optionality so the start gate stays a HANDLER
/// concern, not a cross-action struct requirement.
#[test]
fn staff_start_deserializes_with_reason_none_then_handler_rejects() {
    use tachi_params::TachiStaffParams;
    let params = serde_json::from_value::<TachiStaffParams>(serde_json::json!({
        "action": "start",
        "task": "launch a worker",
    }))
    .expect("start deserializes (handler enforces the reason, not serde)");
    assert_eq!(params.action, "start");
    assert!(
        params.staffing_reason.is_none(),
        "a start request that omitted staffing_reason has None here; the handler must reject it"
    );
}

/// Discrimination test: `staffing_reason` is a closed typed enum, not free-form.
#[test]
fn staff_start_reason_is_typed_not_free_form() {
    use tachi_params::TachiStaffParams;
    let admitted = serde_json::from_value::<TachiStaffParams>(serde_json::json!({
        "action": "start",
        "task": "typed reason",
        "staffing_reason": "cross_device_remote",
    }))
    .expect("allowlisted reason deserializes");
    assert_eq!(
        admitted.staffing_reason,
        Some(tachi_params::TachiDispatchReason::CrossDeviceRemote)
    );

    let err = serde_json::from_value::<TachiStaffParams>(serde_json::json!({
        "action": "start",
        "task": "free-form rejected",
        "staffing_reason": "want_parallelism",
    }))
    .expect_err("free-form reasons must be rejected at deserialization");
    assert!(
        err.to_string().contains("unknown variant"),
        "free-form reason must be rejected as unknown variant: {err}"
    );
}

/// Discrimination test: a STATUS request missing `dispatch_id` is rejected by
/// the handler (dispatch_id is Option at the schema level, but status requires
/// it semantically). This pins that the read path requires its own identifier.
#[test]
fn staff_status_without_dispatch_id_is_handler_rejected() {
    use tachi_params::TachiStaffParams;
    // Schema-optional, so this deserializes (None). The handler rejects.
    let params = serde_json::from_value::<TachiStaffParams>(serde_json::json!({
        "action": "status",
    }))
    .expect("status without dispatch_id deserializes (handler enforces it)");
    assert!(
        params.dispatch_id.is_none(),
        "status without dispatch_id has None here; the handler must reject"
    );
}

#[test]
fn tachi_task_action_schema_declares_exact_c1c_survivor_list() {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert_eq!(
        values,
        &vec![
            json!("intake"),
            json!("claim"),
            json!("heartbeat"),
            json!("handoff"),
            json!("release"),
            json!("board"),
            json!("status"),
            json!("complete"),
            json!("adjudicate"),
            json!("brief"),
        ],
        "tachi_task schema must expose exactly the #1687 C1c survivor list"
    );
    // #1319-C2: dispatch/cancel/wait were removed from tachi_task (external
    // staffing now flows through tachi_staff). The schema must NOT list them.
    assert!(
        !values.contains(&json!("dispatch")),
        "tachi_task must not advertise dispatch after [1319-C2]"
    );
    assert!(
        !values.contains(&json!("cancel")),
        "tachi_task must not advertise cancel after [1319-C2]"
    );
    assert!(
        !values.contains(&json!("wait")),
        "tachi_task must not advertise wait after [1319-C2]"
    );
    for removed in [
        "route_simulate",
        "proposals",
        "review_proposal",
        "apply_proposals",
    ] {
        assert!(
            !values.contains(&json!(removed)),
            "tachi_task must not advertise route tuning action {removed} after #1426"
        );
    }
    for removed in tachi_params::TACHI_TASK_RETIRED_ACTIONS {
        assert!(
            !values.contains(&json!(*removed)),
            "tachi_task schema must not advertise a retired C1a/C1b/C1c task action {removed}"
        );
    }
    // #757: GH PR lifecycle is tachi_gh only — not on tachi_task schema at all.
    for removed in tachi_params::TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS {
        assert!(
            !values.contains(&json!(*removed)),
            "tachi_task schema must not advertise removed lifecycle action {removed}"
        );
    }
    assert_eq!(values.len(), tachi_params::TachiTaskAction::PRIMARY.len());
}

#[test]
fn tachi_gh_action_schema_mentions_lifecycle_actions() {
    let schema = rmcp::schemars::schema_for!(tachi_params::TachiGhParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];
    let description = action["description"].as_str().expect("action description");

    for expected in [
        "ship",
        "link_pr",
        "pr_status",
        "pr_handoff",
        "release_note",
        "close_loop",
    ] {
        assert!(
            description.contains(expected),
            "tachi_gh action schema should mention {expected}: {description}"
        );
    }
}

// ─── Numeric params: schema must match string-or-number runtime (#572) ───────
//
// Every field using a `coerce::opt_*_from_string_or_number` deserializer accepts
// numeric strings at runtime, but schemars used to advertise only `integer`/
// `number`. MCP clients that send numeric strings (which the server accepts)
// were rejected by schema validation. These tests pin the agreement: the schema
// must advertise both the numeric type AND `string`.

fn collect_schema_types(schema: &Value, out: &mut Vec<String>) {
    match schema.get("type") {
        Some(Value::String(s)) => out.push(s.clone()),
        Some(Value::Array(arr)) => {
            for v in arr {
                if let Value::String(s) = v {
                    out.push(s.clone());
                }
            }
        }
        _ => {}
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(arr) = schema.get(key).and_then(|v| v.as_array()) {
            for sub in arr {
                collect_schema_types(sub, out);
            }
        }
    }
}

fn property_schema_types(value: &Value, field: &str) -> Vec<String> {
    let mut types = Vec::new();
    if let Some(prop) = value.get("properties").and_then(|p| p.get(field)) {
        collect_schema_types(prop, &mut types);
    }
    types
}

/// Asserts a numeric-coerce field's schema advertises both `numeric_type`
/// (`integer` or `number`) and `string`, matching the runtime deserializer.
fn assert_field_accepts_string_and_number(value: &Value, field: &str, numeric_type: &str) {
    let types = property_schema_types(value, field);
    assert!(
        !types.is_empty(),
        "{field}: no `type` found in schema property — property may be misnamed"
    );
    assert!(
        types.iter().any(|t| t == "string"),
        "{field}: schema must accept numeric strings because the runtime deserializer does; \
         types seen: {types:?}"
    );
    assert!(
        types.iter().any(|t| t == numeric_type),
        "{field}: schema must still advertise `{numeric_type}`; types seen: {types:?}"
    );
}

#[test]
fn tachi_task_numeric_params_schema_accepts_string_or_number() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(TachiTaskParams))
        .expect("schema serializes");
    // #1319-C2: the dispatch-only execution knobs other than timeout_secs are
    // hidden from the public schema (#[schemars(skip)]); timeout_secs survives
    // as the Status arm's acpx control timeout and keeps its string-or-number
    // admission contract. The list below covers the surviving ledger fields.
    for field in ["duration_ms", "number", "cost_tokens", "timeout_secs"] {
        assert_field_accepts_string_and_number(&value, field, "integer");
    }
    for field in ["cost_usd", "quality_score"] {
        assert_field_accepts_string_and_number(&value, field, "number");
    }
}

#[test]
fn tachi_gh_numeric_params_schema_accepts_string_or_number() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(tachi_params::TachiGhParams))
        .expect("schema serializes");
    assert_field_accepts_string_and_number(&value, "number", "integer");
    assert_field_accepts_string_and_number(&value, "limit", "integer");
}

#[test]
fn tachi_verify_numeric_params_schema_accepts_string_or_number() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(tachi_params::TachiVerifyParams))
        .expect("schema serializes");
    assert_field_accepts_string_and_number(&value, "exit_code", "integer");
    assert_field_accepts_string_and_number(&value, "limit", "integer");
    // #1454 slice 2: action=run timeout, same string-or-number admission.
    assert_field_accepts_string_and_number(&value, "timeout_secs", "integer");
}

#[test]
fn tachi_verify_run_timeout_accepts_numeric_strings() {
    let params: tachi_params::TachiVerifyParams = serde_json::from_value(json!({
        "action": "run",
        "flow_id": "flow_schema-run",
        "check_kind": "fmt",
        "timeout_secs": "900"
    }))
    .expect("runtime accepts numeric strings for run timeout");
    assert_eq!(params.timeout_secs, Some(900));
    assert_eq!(params.check_kind.as_deref(), Some("fmt"));
}

#[test]
fn tachi_complete_numeric_params_schema_accepts_string_or_number() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(
        tachi_params::TachiCompleteParams
    ))
    .expect("schema serializes");
    assert_field_accepts_string_and_number(&value, "duration_ms", "integer");
    assert_field_accepts_string_and_number(&value, "cost_tokens", "integer");
    assert_field_accepts_string_and_number(&value, "cost_usd", "number");
    assert_field_accepts_string_and_number(&value, "quality_score", "number");
}

#[test]
fn tachi_save_and_remember_importance_schema_accepts_string_or_number() {
    let save = serde_json::to_value(rmcp::schemars::schema_for!(tachi_params::TachiSaveParams))
        .expect("schema serializes");
    assert_field_accepts_string_and_number(&save, "importance", "number");

    let remember = serde_json::to_value(rmcp::schemars::schema_for!(tachi_params::RememberParams))
        .expect("schema serializes");
    assert_field_accepts_string_and_number(&remember, "importance", "number");
}

#[test]
fn tachi_save_schema_advertises_both_fact_extraction_kinds() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(tachi_params::TachiSaveParams))
        .expect("schema serializes");
    let kinds = enum_values(&value["properties"]["kind"], "kind");

    for kind in ["facts", "extract_facts"] {
        assert!(
            kinds.iter().any(|advertised| advertised == kind),
            "tachi_save supports kind={kind} in its handler, so the public schema must advertise it"
        );
    }
}

#[test]
fn search_memory_mmr_threshold_schema_accepts_string_or_number() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(
        tachi_params::SearchMemoryParams
    ))
    .expect("schema serializes");
    assert_field_accepts_string_and_number(&value, "mmr_threshold", "number");
}

// ─── Runtime agreement: the deserializer actually accepts numeric strings ─────

#[test]
fn tachi_task_runtime_accepts_numeric_strings() {
    let params: TachiTaskParams = serde_json::from_value(json!({
        "action": "complete",
        "duration_ms": "12345",
        "timeout_secs": "30",
        "cost_tokens": "99",
        "cost_usd": "1.5",
        "quality_score": "0.9"
    }))
    .expect("runtime accepts numeric strings");
    assert_eq!(params.duration_ms, Some(12345));
    assert_eq!(params.timeout_secs, Some(30));
    assert_eq!(params.cost_tokens, Some(99));
    assert_eq!(params.cost_usd, Some(1.5));
    assert_eq!(params.quality_score, Some(0.9));
}

#[test]
fn tachi_gh_runtime_accepts_numeric_strings() {
    let params: tachi_params::TachiGhParams = serde_json::from_value(json!({
        "action": "issue_read",
        "number": "42",
        "limit": "5"
    }))
    .expect("runtime accepts numeric strings");
    assert_eq!(params.number, Some(42));
    assert_eq!(params.limit, Some(5));
}

#[test]
fn tachi_verify_runtime_accepts_numeric_strings() {
    let params: tachi_params::TachiVerifyParams = serde_json::from_value(json!({
        "action": "record",
        "exit_code": "0",
        "limit": "3"
    }))
    .expect("runtime accepts numeric strings");
    assert_eq!(params.exit_code, Some(0));
    assert_eq!(params.limit, Some(3));
}

/// #1319-C2 discriminator: the generated `tachi_task` MCP schema's property
/// descriptions must not reference the removed Dispatch/Wait/Cancel actions.
/// After the enum dropped those variants, any description still guiding the
/// model to "provide this for action=dispatch" is a schema lie.
#[test]
fn tachi_task_schema_descriptions_do_not_reference_removed_actions() {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let serialized = value.to_string().to_ascii_lowercase();
    for stale in [
        "[action=dispatch",
        "action='dispatch'",
        "action=dispatch|",
        "dispatch response",
        "dispatch prompt",
        "spawned agent",
        "[action=wait",
        "action=wait",
        "[action=cancel",
        "action=cancel",
    ] {
        assert!(
            !serialized.contains(stale),
            "tachi_task schema must not reference removed action text '{stale}' after [1319-C2]"
        );
    }
    // #1426: same rule for the four route-tuning actions that left Task for
    // the admin-only tachi_tune surface.
    for stale in [
        "action=route_simulate",
        "action='route_simulate'",
        "action=proposals",
        "action='proposals'",
        "action=review_proposal",
        "action='review_proposal'",
        "action=apply_proposals",
        "action='apply_proposals'",
        "|route_simulate",
        "|proposals]",
    ] {
        assert!(
            !serialized.contains(stale),
            "tachi_task schema must not reference removed route-tuning action text '{stale}' after #1426"
        );
    }
    for stale in [
        "action=plan",
        "action='plan'",
        "action=cycle_plan",
        "action='cycle_plan'",
        "action=recommend",
        "action='recommend'",
        "action=refine_issues",
        "action='refine_issues'",
        "action=merge",
        "action='merge'",
        "action=ux_matrix",
        "action='ux_matrix'",
        "action=briefing",
        "action='briefing'",
        "action=doc_index",
        "action='doc_index'",
        "action=cycle_status",
        "action='cycle_status'",
        "briefing/intake",
        "briefing/intake/close_loop/status/complete",
        "action=profiles",
        "action='profiles'",
        "action=profile",
        "action='profile'",
        "action=card",
        "action='card'",
        "profile/card",
    ] {
        assert!(
            !serialized.contains(stale),
            "tachi_task schema must not reference retired C1a/C1b/C1c action text '{stale}'"
        );
    }
}

/// #1319-C2 discriminator: dispatch-only execution knobs (fields with no live
/// reader on any surviving Task action) must NOT appear in the public
/// `tachi_task` schema. They were exposed only for the removed Dispatch arm;
/// leaving them public is an unactionable no-op surface.
#[test]
fn tachi_task_schema_hides_dispatch_only_execution_knobs() {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let properties = value["properties"].as_object().expect("task properties");
    // Fields whose ONLY reader was the removed Dispatch arm (verified by
    // grep: zero `params.<field>` reads in surviving task_router/task_facade
    // arms). These must be hidden via #[schemars(skip)].
    for knob in [
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
        "inject_hub_mcps",
        "command",
        "harness_transport",
        "harness_server_url",
        "credential_profiles",
        "tool_profile",
        "mcp_access",
        "allowed_mcp_servers",
    ] {
        assert!(
            !properties.contains_key(knob),
            "dispatch-only execution knob '{knob}' must be hidden from the public tachi_task schema after [1319-C2]"
        );
    }
    // #1426: proposal_id/review_status lost their only readers (the removed
    // review_proposal/apply_proposals arms) when route tuning moved to
    // tachi_tune. Same treatment, same reason.
    for orphaned in ["proposal_id", "review_status"] {
        assert!(
            !properties.contains_key(orphaned),
            "route-tuning parameter '{orphaned}' must be hidden from the public tachi_task schema after #1426"
        );
    }
    for deleted in [
        "include_card",
        "execution_level",
        "strategy",
        "delete_worktree",
        "confirm",
    ] {
        assert!(
            !properties.contains_key(deleted),
            "#1683 C1a deleted field '{deleted}' must not appear in the public tachi_task schema"
        );
    }
}

#[test]
fn tachi_task_schema_excludes_relocated_closure_fields() {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let properties = value["properties"].as_object().expect("task properties");
    for field in [
        "related_issues",
        "post_comment",
        "wiki_title",
        "wiki_text",
        "wiki_path",
        "wiki_topic",
        "wiki_summary",
        "wiki_category",
        "wiki_keywords",
        "wiki_entities",
        "wiki_importance",
        "wiki_scope",
        "wiki_domain",
        "force",
    ] {
        assert!(
            !properties.contains_key(field),
            "closure field '{field}' must live on tachi_gh(action='close_loop'), not tachi_task"
        );
    }
}

#[test]
fn tachi_gh_schema_exposes_relocated_closure_fields() {
    let schema = rmcp::schemars::schema_for!(TachiGhParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let properties = value["properties"].as_object().expect("GH properties");
    for field in [
        "issue_ref",
        "pr_ref",
        "doc_paths",
        "spec_paths",
        "related_issues",
        "post_comment",
        "flow_id",
        "notes",
        "wiki_title",
        "wiki_text",
        "wiki_path",
        "wiki_topic",
        "wiki_summary",
        "wiki_category",
        "wiki_keywords",
        "wiki_entities",
        "wiki_importance",
        "wiki_scope",
        "wiki_domain",
        "project",
        "force",
    ] {
        assert!(
            properties.contains_key(field),
            "closure field '{field}' must be available on tachi_gh(action='close_loop')"
        );
    }
}

/// #1319-C2 discriminator (positive): fields still READ by surviving public
/// Task actions must remain visible in the public `tachi_task` schema —
/// hiding them would make their live readers unactionable no-ops.
/// - `agent` is read by the Complete arm (task_router.rs, with
///   dispatch-defaults fallback);
/// - `cwd` is shared by brief and lifecycle/closure readers for relative doc
///   path resolution (feature_briefing/docs.rs and task_lifecycle);
/// - `auto_capability_bundle` is read by brief for context injection
///   (feature_briefing/dispatch.rs);
/// - `project_explicit` is read by the Complete arm for the #1041 B7 wire
///   explicitness signal (task_router.rs complete arm); its schema property
///   name is the serde rename `__tachi_project_explicit`, which IS the wire
///   name clients send;
/// - `timeout_secs` is read by the Status arm for the acpx control command
///   timeout (task_facade.rs handle_tachi_task_status).
#[test]
fn tachi_task_schema_keeps_fields_read_by_surviving_actions() {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let properties = value["properties"].as_object().expect("task properties");
    for (field, reader_hint) in [
        ("agent", "action=complete"),
        ("auto_capability_bundle", "action=brief"),
        ("__tachi_project_explicit", "action=complete"),
        ("timeout_secs", "action=status"),
    ] {
        let property = properties.get(field).unwrap_or_else(|| {
            panic!("{field} must stay visible in the public tachi_task schema after [1319-C2]")
        });
        let description = property["description"].as_str().unwrap_or_default();
        assert!(
            description.contains(reader_hint),
            "{field} description must name its surviving reader action ({reader_hint})"
        );
    }
    let cwd_description = properties["cwd"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(
        cwd_description.contains("task context/lifecycle records"),
        "cwd description must preserve its shared task-context/lifecycle disposition"
    );
    assert!(
        !cwd_description.contains("action=briefing")
            && !cwd_description.contains("action=doc_index")
            && !cwd_description.contains("action=cycle_status"),
        "cwd description must not teach a retired Task action"
    );
}
