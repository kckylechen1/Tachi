use super::*;

#[test]
fn compact_layer_rows_preserves_typed_wiki_store_identity() {
    let store = json!({"kind": "bound_project"});
    let compact = compact_layer_rows(
        vec![json!({
            "id": "wiki-row",
            "path": "/wiki/example",
            "store": store,
        })],
        1,
        Some("wiki"),
        None,
    );

    assert_eq!(compact[0]["store"], store);
}

#[test]
fn compact_layer_rows_prefers_normalized_typed_references() {
    let rows = vec![json!({
        "id": "typed-row",
        "references": ["#1296", "docs/spec.md"],
        "metadata": {
            "source_refs": ["#legacy"],
            "evidence_refs_v1": [{"ref": "#typed-metadata"}]
        }
    })];
    let compact = compact_layer_rows(rows, 1, Some("wiki"), None);
    assert_eq!(compact[0]["references"], json!(["#1296", "docs/spec.md"]));
}

#[test]
fn compact_layer_rows_prefers_typed_metadata_when_both_channels_exist() {
    let rows = vec![json!({
        "id": "both-row",
        "metadata": {
            "source_refs": ["#legacy"],
            "evidence_refs_v1": [{"ref": "#typed"}, {"ref": "docs/typed.md"}]
        }
    })];
    let compact = compact_layer_rows(rows, 1, Some("wiki"), None);
    assert_eq!(compact[0]["references"], json!(["#typed", "docs/typed.md"]));
}

#[test]
fn compact_layer_rows_ignores_null_or_empty_normalized_references() {
    for references in [Value::Null, json!([])] {
        let rows = vec![json!({
            "id": "empty-normalized-row",
            "references": references,
            "metadata": {
                "source_refs": ["#legacy"],
                "evidence_refs_v1": [{"ref": "#typed"}, {"ref": "docs/typed.md"}]
            }
        })];
        let compact = compact_layer_rows(rows, 1, Some("wiki"), None);
        assert_eq!(compact[0]["references"], json!(["#typed", "docs/typed.md"]));
    }
}

#[test]
fn compact_layer_rows_falls_back_to_legacy_references() {
    let rows = vec![json!({
        "id": "legacy-row",
        "metadata": {"source_refs": ["#legacy", "docs/legacy.md"]}
    })];
    let compact = compact_layer_rows(rows, 1, Some("wiki"), None);
    assert_eq!(
        compact[0]["references"],
        json!(["#legacy", "docs/legacy.md"])
    );
}

#[test]
fn compact_layer_rows_skips_invalid_reference_values() {
    let rows = vec![
        json!({
            "id": "mixed-row",
            "references": [null, "  "],
            "metadata": {
                "evidence_refs_v1": [{"ref": ""}, {"ref": 42}],
                "source_refs": [null, "", "#legacy", 7]
            }
        }),
        json!({
            "id": "invalid-row",
            "metadata": {
                "evidence_refs_v1": [{"ref": "  "}],
                "source_refs": null
            }
        }),
    ];
    let compact = compact_layer_rows(rows, 2, Some("wiki"), None);

    assert_eq!(compact[0]["references"], json!(["#legacy"]));
    assert!(
        compact[1].get("references").is_none(),
        "invalid legacy metadata must not leak a null/scalar references field: {compact:#?}"
    );
}

#[test]
fn build_debug_checklist_prefers_wiki_guidance() {
    let checklist = build_debug_checklist(&[json!({
        "path": "/wiki/debug/mcp-args",
        "text": "Checklist:\n- Verify schema -> client serialization -> server deserialization before editing transport.\n- Add a failing boundary test at the API boundary before retrying the same layer.\n- Stop after two failed patches in the same layer and ask another agent.",
        "summary": "MCP argument debugging"
    })]);

    assert!(checklist[0].contains("schema -> client serialization -> server deserialization"));
    assert!(checklist
        .iter()
        .any(|item| item.contains("failing boundary test at the API boundary")));
}

#[test]
fn build_debug_checklist_falls_back_without_wiki_hits() {
    let checklist = build_debug_checklist(&[json!({
        "path": "/behavior/global_rules/retry-policy",
        "text": "This is not a wiki entry and should not override the fallback checklist.",
    })]);

    assert_eq!(checklist.len(), DEBUG_CHECKLIST_LIMIT);
    assert_eq!(checklist[0], FALLBACK_DEBUG_CHECKLIST[0]);
}

#[test]
fn wiki_slug_preserves_cjk_and_readable_separators() {
    assert_eq!(
        wiki_slug("MCP hub_call arguments 丢失：从 schema 层排查"),
        "MCP-hub_call-arguments-丢失-从-schema-层排查"
    );
}

#[test]
fn task_brief_router_selects_review_sop_for_pr_review() {
    let intent = classify_task_intent("看看这几个 PR 下面 Gemini 的回复");
    let sops = build_selected_sops(intent);
    let plan = build_tool_plan(intent);

    assert_eq!(intent, "review_request");
    assert!(sops
        .iter()
        .any(|sop| sop.get("id").and_then(|v| v.as_str()) == Some("skill:waza-check")));
    assert!(plan.iter().any(|step| {
        step.get("tool").and_then(|v| v.as_str()) == Some("tachi_task")
            && step.get("action").and_then(|v| v.as_str()) == Some("board")
    }));
}

#[test]
fn task_brief_router_selects_targeted_verification_for_build_run() {
    let intent = classify_task_intent("帮我编译二进制并且跑起来验证功能");
    let sops = build_selected_sops(intent);

    assert_eq!(intent, "test_request");
    assert!(sops.iter().any(|sop| {
        sop.get("id").and_then(|v| v.as_str()) == Some("skill:coding-test-strategy")
    }));
}

#[test]
fn task_brief_router_maps_waza_capability_intents() {
    let cases = [
        (
            "帮我做一个前端页面截图视觉检查",
            "design_request",
            "skill:waza-design",
        ),
        (
            "润色这段 release notes",
            "write_request",
            "skill:waza-write",
        ),
        (
            "读一下 https://example.com/report.pdf",
            "read_request",
            "skill:waza-read",
        ),
        (
            "检查 agent MCP 配置健康度",
            "health_request",
            "skill:waza-health",
        ),
    ];
    for (task, expected_intent, expected_skill) in cases {
        let intent = classify_task_intent(task);
        let sops = build_selected_sops(intent);
        assert_eq!(intent, expected_intent, "task: {task}");
        assert!(
            sops.iter()
                .any(|sop| { sop.get("id").and_then(|v| v.as_str()) == Some(expected_skill) }),
            "expected {expected_skill} for {task}, got {sops:?}"
        );
    }
}

#[test]
fn task_brief_router_avoids_ascii_substring_false_positives() {
    assert_eq!(
        classify_task_intent("explain why this failed"),
        "explain_request"
    );
    assert_eq!(
        classify_task_intent("decide whether this is specific enough"),
        "other"
    );
    assert_eq!(classify_task_intent("run ci checks"), "test_request");
}

#[test]
fn task_brief_router_generic_kankan_is_not_always_review() {
    assert_eq!(classify_task_intent("看看这个报错"), "fix_request");
    assert_eq!(
        classify_task_intent("看看这几个 PR 下面 Gemini 的回复"),
        "review_request"
    );
    assert_eq!(classify_task_intent("看一下 PRs"), "review_request");
}

#[test]
fn task_brief_router_classifies_exploration_as_research() {
    assert_eq!(
        classify_task_intent(
            "List the .rs files under crates/memcore/src and produce a one-line summary of each."
        ),
        "research_request"
    );
    assert_eq!(
        classify_task_intent("explore the codebase and give an overview"),
        "research_request"
    );
    assert_eq!(
        classify_task_intent("梳理一下这个模块的结构"),
        "research_request"
    );
    assert_eq!(
        classify_task_intent("map out the module dependency graph"),
        "research_request"
    );
    // Gemini guard: a coding task phrased with "map the ..." must NOT be
    // misread as research (the reason "map the" was narrowed to "map out").
    assert_ne!(
        classify_task_intent("map the array values into the new struct fields"),
        "research_request"
    );
}

#[test]
fn feature_board_filter_matches_stable_fields_only() {
    let needles = vec!["flow_20260608t000000z_feature".to_string()];

    assert!(value_contains_any(
        &json!({
            "dispatch_id": "dispatch-1",
            "summary": "work for flow_20260608T000000Z_feature",
            "metadata": {
                "debug_note": "unrelated"
            }
        }),
        &needles
    ));
    assert!(!value_contains_any(
        &json!({
            "dispatch_id": "dispatch-2",
            "summary": "unrelated work",
            "metadata": {
                "debug_note": "flow_20260608T000000Z_feature"
            }
        }),
        &needles
    ));
}

