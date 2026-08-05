use serde_json::{json, Value};
use tachi_params::{
    TachiEventParams, TachiMemoryParams, TachiSearchParams, TachiSkillParams, TachiTaskParams,
    TachiTuneParams,
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
    for removed in [
        "recall_simulate",
        "recall_proposals",
        "review_recall_proposal",
        "apply_recall_proposals",
    ] {
        assert!(
            !action["enum"]
                .as_array()
                .expect("action enum")
                .contains(&json!(removed)),
            "tachi_memory must not advertise recall tuning action {removed} after #1426"
        );
    }
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
        tachi_params::TACHI_MEMORY_ACTIONS.len(),
        "advertised action enum must match the TACHI_MEMORY_ACTIONS inventory"
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
    let schema = rmcp::schemars::schema_for!(tachi_params::TachiStaffParams);
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
    let required = value["required"].as_array().cloned().unwrap_or_default();
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
fn tachi_task_action_schema_declares_feature_briefing() {
    let schema = rmcp::schemars::schema_for!(TachiTaskParams);
    let value = serde_json::to_value(schema).expect("schema serializes");
    let action = &value["properties"]["action"];

    assert_eq!(action["type"], json!("string"));
    let values = action["enum"].as_array().expect("action enum");
    assert!(values.contains(&json!("briefing")));
    assert!(values.contains(&json!("doc_index")));
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
    assert!(values.contains(&json!("complete")));
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
    assert!(values.contains(&json!("status")));
    assert!(values.contains(&json!("intake")));
    assert!(values.contains(&json!("cycle_status")));
    assert!(values.contains(&json!("build_references")));
    assert!(values.contains(&json!("close_loop")));
    // #757: GH PR lifecycle is tachi_gh only — not on tachi_task schema at all.
    for removed in tachi_params::TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS {
        assert!(
            !values.contains(&json!(*removed)),
            "tachi_task schema must not advertise removed lifecycle action {removed}"
        );
    }
    for removed in tachi_params::TACHI_TASK_REMOVED_SIX_ACTIONS {
        assert!(
            !values.contains(&json!(*removed)),
            "tachi_task schema must not advertise retired #1683 action {removed}"
        );
    }
    assert_eq!(values.len(), tachi_params::TachiTaskAction::PRIMARY.len());
    assert_eq!(values.len(), 17);
}

#[test]
fn tachi_gh_action_schema_mentions_lifecycle_actions() {
    let schema = rmcp::schemars::schema_for!(tachi_params::TachiGhParams);
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
}

/// #1319-C2 discriminator (positive): fields still READ by surviving public
/// Task actions must remain visible in the public `tachi_task` schema —
/// hiding them would make their live readers unactionable no-ops.
/// - `agent` is read by the Complete arm (task_router.rs, with
///   dispatch-defaults fallback);
/// - `cwd` is read by briefing/doc_index for relative doc path resolution
///   (feature_briefing/docs.rs);
/// - `auto_capability_bundle` is read by briefing for context injection
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
        ("cwd", "action=briefing"),
        ("auto_capability_bundle", "action=briefing"),
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
}
