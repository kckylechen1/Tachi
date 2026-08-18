use super::*;

/// #1690 C3 discriminator (5a): the retired `tachi_skill` actions —
/// bundle/loadout/from_pattern — must be typed-rejected with an error naming
/// the surviving action set {discover, run}. RED pre-fix: the arms exist and
/// succeed (or reject with the old message listing retired actions); GREEN
/// post-fix: the surviving-set error.
#[tokio::test]
async fn retired_skill_actions_are_typed_rejected_with_surviving_set() {
    let server = make_server();
    for action in ["bundle", "loadout", "from_pattern"] {
        let result = server
            .tachi_skill(Parameters(TachiSkillParams {
                action: action.to_string(),
                query: Some("plan a dispatch policy change".to_string()),
                cap_type: None,
                enabled_only: None,
                limit: None,
                skill_id: None,
                args: Some(json!({"skill_id": "skill:pattern-x", "name": "x"})),
            }))
            .await
            .expect_err("retired action must be typed-rejected");

        assert!(
            result.contains("Invalid action"),
            "retired action '{action}' must hit the invalid-action error, got: {result}"
        );
        assert!(
            result.contains("'discover' or 'run'"),
            "rejection must name the surviving set {{discover, run}} for '{action}', got: {result}"
        );
        assert!(
            !result.contains("bundle") || action == "bundle",
            "surviving-set error must not teach a retired action for '{action}', got: {result}"
        );
    }
}

/// Re-anchor of the deleted `tachi_skill(action='bundle')` query-matching guard:
/// a registered reviewed skill is still discoverable by query through the
/// surviving thin discover surface (the static reviewed list the leader
/// adjudicated in #1690 C3 gate outcome 1).
#[tokio::test]
async fn registered_skill_is_discoverable_by_query() {
    let server = make_server();
    server
        .with_global_store(|store| {
            store
                .hub_register(&make_skill_capability(
                    "skill:excel-automation",
                    "excel-automation",
                    "Build spreadsheet workflows and Excel reports from CSV data.",
                    "listed",
                ))
                .map_err(|e| e.to_string())
        })
        .expect("seed skill registry");

    let result = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "discover".to_string(),
            query: Some("excel csv spreadsheet".to_string()),
            cap_type: None,
            enabled_only: Some(true),
            limit: Some(5),
            skill_id: None,
            args: None,
        }))
        .await
        .expect("tachi_skill discover should succeed");
    let json: Value = serde_json::from_str(&result).expect("json");
    assert!(
        json["results"]
            .as_array()
            .expect("results array")
            .iter()
            .any(|row| row["id"] == json!("skill:excel-automation")),
        "registered reviewed skill must surface in discover: {json}"
    );
}

/// Re-anchor of the deleted `from_pattern` pattern-resolution guard: the
/// continuity pattern projection machinery (which from_pattern consumed) stays
/// live through `continuity_ops::list_active_patterns` — a projected pattern is
/// still resolvable by its projection_key.
#[tokio::test]
async fn continuity_pattern_projection_stays_resolvable_by_key() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut pattern = make_entry("skill-from-pattern-row");
            pattern.path = "/user/patterns/agent_os/continuity-first".to_string();
            pattern.summary = "Continuity-first project management".to_string();
            pattern.text =
                "Use continuity evidence before selecting the next project-management action."
                    .to_string();
            pattern.metadata = json!({
                "projection_kind": "pattern",
                "projection_key": "continuity-first",
                "source_event_id": "pattern-event-skill",
                "counters": {"seen": 4, "hit": 2}
            });
            store.upsert(&pattern).map_err(|e| e.to_string())
        })
        .expect("seed projected pattern");

    let patterns =
        crate::continuity_ops::list_active_patterns(&server, None, Some("continuity-first"), 10)
            .expect("list active patterns");
    assert_eq!(patterns.len(), 1, "pattern projection must stay resolvable");
    assert_eq!(
        patterns[0].metadata["projection_key"],
        json!("continuity-first")
    );
    assert_eq!(patterns[0].metadata["projection_kind"], json!("pattern"));
}

/// Re-anchor of the deleted delegate-gate tests: the delegate profile gate
/// survives and now admits exactly the surviving static surface — discover and
/// run are callable, retired actions are blocked by the gate before the action
/// switch.
#[tokio::test]
async fn delegate_profile_gate_admits_discover_run_and_blocks_retired_actions() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("delegate").expect("delegate profile should parse"),
    ));

    for action in ["bundle", "loadout", "from_pattern"] {
        let error = server
            .tachi_skill(Parameters(TachiSkillParams {
                action: action.to_string(),
                query: Some("plan a dispatch policy change".to_string()),
                cap_type: None,
                enabled_only: None,
                limit: None,
                skill_id: None,
                args: Some(json!({"skill_id": "skill:pattern-x", "name": "x"})),
            }))
            .await
            .expect_err("delegate gate must reject retired actions");
        assert!(
            error.contains("not available to the active tool profile"),
            "delegate gate must fire for '{action}', got: {error}"
        );
    }

    let discover = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "discover".to_string(),
            query: Some("plan a dispatch policy change".to_string()),
            cap_type: None,
            enabled_only: Some(true),
            limit: Some(5),
            skill_id: None,
            args: None,
        }))
        .await
        .expect("delegate discover must stay available");
    let discover_json: Value = serde_json::from_str(&discover).expect("discover JSON");
    assert_eq!(discover_json["status"], json!("completed"));

    // #1690 C5 repair (chosen option: extend, not narrow): the test name
    // claims the delegate gate ADMITS run — so run must actually be invoked
    // through the delegate profile, not merely assumed. A document skill
    // exercises the full `execute_skill_prompt_with_receipt` path without an
    // LLM call (deterministic, no provider dependency).
    let mut doc_skill = make_skill_capability(
        "skill:delegate-gate-doc",
        "delegate-gate-doc",
        "Document skill proving the delegate gate admits run.",
        "listed",
    );
    doc_skill.definition = json!({
        "execution": "document",
        "prompt": "document path; content returned verbatim",
        "content": "delegate run admitted",
        "policy": {"visibility": "listed"},
        "inputSchema": {"type": "object"}
    })
    .to_string();
    server
        .with_global_store(|store| {
            store
                .hub_register(&doc_skill)
                .map_err(|e| e.to_string())
        })
        .expect("seed delegate-gate document skill");

    let run = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "run".to_string(),
            query: None,
            cap_type: None,
            enabled_only: None,
            limit: None,
            skill_id: Some("skill:delegate-gate-doc".to_string()),
            args: Some(json!({})),
        }))
        .await
        .expect("delegate run must be admitted by the gate");
    let run_json: Value = serde_json::from_str(&run).expect("run JSON");
    assert_eq!(run_json["status"], json!("completed"));
    assert_eq!(run_json["action"], json!("run"));
    assert_eq!(run_json["execution"], json!("document"));
    assert_eq!(run_json["output"], json!("delegate run admitted"));
}
