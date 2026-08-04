use super::*;

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

    assert_eq!(action["type"], json!("string"));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("briefing")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("get")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("readiness")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("recall_simulate")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("recall_proposals")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("review_recall_proposal")));
    assert!(action["enum"]
        .as_array()
        .expect("action enum")
        .contains(&json!("apply_recall_proposals")));
    // #757 fold: standalone tools re-fronted as tachi_memory actions.
    for folded in ["delete", "gc", "doctor_scan", "ingest", "ingest_source"] {
        assert!(
            action["enum"]
                .as_array()
                .expect("action enum")
                .contains(&json!(folded)),
            "folded action '{folded}' must be advertised in the tachi_memory action schema"
        );
    }
    assert_eq!(
        action["enum"].as_array().expect("action enum").len(),
        crate::tool_params::TACHI_MEMORY_ACTIONS.len(),
        "advertised action enum must match the TACHI_MEMORY_ACTIONS inventory"
    );
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
fn tachi_skill_action_schema_declares_bundle_and_loadout() {
    let schema = rmcp::schemars::schema_for!(TachiSkillParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("discover")));
    assert!(values.contains(&json!("run")));
    assert!(values.contains(&json!("bundle")));
    assert!(values.contains(&json!("from_pattern")));
    assert!(values.contains(&json!("loadout")));
}

#[test]
fn tachi_staff_schema_exposes_only_start_and_status() {
    let schema = rmcp::schemars::schema_for!(crate::tool_params::TachiStaffParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let properties = value["properties"].as_object().expect("staff properties");
    let actions = properties["action"]["enum"]
        .as_array()
        .expect("staff action enum");
    assert_eq!(actions, &vec![json!("start"), json!("status")]);
    // staffing_reason is present as a top-level property (the flat struct
    // surfaces it) but is NOT in the schema-level `required` list — it is
    // OPTIONAL at the schema level precisely so `action='status'` (a read-only
    // probe) can omit it without fabricating a reason. The `start` admission
    // gate is enforced by the handler, not by a cross-action required field.
    assert!(
        properties.contains_key("staffing_reason"),
        "staff schema must surface staffing_reason"
    );
    let required = value["required"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !required.iter().any(|r| r == "staffing_reason"),
        "staffing_reason must NOT be schema-level required (status must be able to omit it); required={required:?}"
    );
    // No execution-shaped fields leak through the boundary.
    for removed in ["cwd", "command", "transport", "sandbox", "allowed_tools"] {
        assert!(
            !properties.contains_key(removed),
            "staff schema must not expose execution field {removed}"
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
    use crate::tool_params::TachiStaffParams;
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
    use crate::tool_params::TachiStaffParams;
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
    use crate::tool_params::TachiStaffParams;
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
    use crate::tool_params::TachiStaffParams;
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
fn tachi_task_action_schema_declares_feature_briefing() {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("briefing")));
    assert!(values.contains(&json!("doc_index")));
    assert!(values.contains(&json!("plan")));
    assert!(values.contains(&json!("dispatch")));
    assert!(values.contains(&json!("complete")));
    assert!(values.contains(&json!("recommend")));
    assert!(values.contains(&json!("route_simulate")));
    assert!(values.contains(&json!("proposals")));
    assert!(values.contains(&json!("review_proposal")));
    assert!(values.contains(&json!("apply_proposals")));
    assert!(values.contains(&json!("status")));
    assert!(values.contains(&json!("cancel")));
    assert!(values.contains(&json!("intake")));
    assert!(values.contains(&json!("cycle_plan")));
    assert!(values.contains(&json!("ux_matrix")));
    assert!(values.contains(&json!("build_references")));
    assert!(values.contains(&json!("close_loop")));
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
    let schema = rmcp::schemars::schema_for!(crate::tool_params::TachiGhParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];
    let description = action["description"].as_str().expect("action description");

    for expected in ["ship", "link_pr", "pr_status", "pr_handoff", "release_note"] {
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
    for field in [
        "duration_ms",
        "timeout_secs",
        "max_turns",
        "number",
        "cost_tokens",
    ] {
        assert_field_accepts_string_and_number(&value, field, "integer");
    }
    for field in ["cost_usd", "quality_score"] {
        assert_field_accepts_string_and_number(&value, field, "number");
    }
}

#[test]
fn tachi_gh_numeric_params_schema_accepts_string_or_number() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(
        crate::tool_params::TachiGhParams
    ))
    .expect("schema serializes");
    assert_field_accepts_string_and_number(&value, "number", "integer");
    assert_field_accepts_string_and_number(&value, "limit", "integer");
}

#[test]
fn tachi_verify_numeric_params_schema_accepts_string_or_number() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(
        crate::tool_params::TachiVerifyParams
    ))
    .expect("schema serializes");
    assert_field_accepts_string_and_number(&value, "exit_code", "integer");
    assert_field_accepts_string_and_number(&value, "limit", "integer");
}

#[test]
fn tachi_complete_numeric_params_schema_accepts_string_or_number() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(
        crate::tool_params::TachiCompleteParams
    ))
    .expect("schema serializes");
    assert_field_accepts_string_and_number(&value, "duration_ms", "integer");
    assert_field_accepts_string_and_number(&value, "cost_tokens", "integer");
    assert_field_accepts_string_and_number(&value, "cost_usd", "number");
    assert_field_accepts_string_and_number(&value, "quality_score", "number");
}

#[test]
fn tachi_save_and_remember_importance_schema_accepts_string_or_number() {
    let save = serde_json::to_value(rmcp::schemars::schema_for!(
        crate::tool_params::TachiSaveParams
    ))
    .expect("schema serializes");
    assert_field_accepts_string_and_number(&save, "importance", "number");

    let remember = serde_json::to_value(rmcp::schemars::schema_for!(
        crate::tool_params::RememberParams
    ))
    .expect("schema serializes");
    assert_field_accepts_string_and_number(&remember, "importance", "number");
}

#[test]
fn tachi_save_schema_advertises_both_fact_extraction_kinds() {
    let value = serde_json::to_value(rmcp::schemars::schema_for!(
        crate::tool_params::TachiSaveParams
    ))
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
        crate::tool_params::SearchMemoryParams
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
    let params: crate::tool_params::TachiGhParams = serde_json::from_value(json!({
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
    let params: crate::tool_params::TachiVerifyParams = serde_json::from_value(json!({
        "action": "record",
        "exit_code": "0",
        "limit": "3"
    }))
    .expect("runtime accepts numeric strings");
    assert_eq!(params.exit_code, Some(0));
    assert_eq!(params.limit, Some(3));
}
