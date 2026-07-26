use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_links_eval_to_flow_dispatch_card_and_ux_matrix() {
    let (server, _temp_home) = make_server_with_temp_home();
    let flow_id = "flow_20260609T000002Z_complete_link_test";
    let dispatch_id = "20260609T000002Z-custom-complete-link";
    seed_dispatch_run(&server, dispatch_id);

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "glm_impl",
            "task": "implementation",
        }),
    )
    .expect("mark dispatch");

    let mut complete_params = task_params("complete");
    complete_params.task = Some("Implement dispatch completion linkage".to_string());
    complete_params.agent = Some("glm".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-link-002".to_string());
    complete_params.task_type = Some("fix_request".to_string());
    complete_params.profile = Some("glm_impl".to_string());
    complete_params.risk = Some("medium".to_string());
    complete_params.duration_ms = Some(1200);
    complete_params.skills_used = vec!["skill:superpowers-executing-plans".to_string()];
    complete_params.cost_tokens = Some(123);
    complete_params.quality_score = Some(0.88);
    complete_params.notes = Some("Linked eval back to dispatch card.".to_string());
    complete_params.diff = Some("diff --git a/x b/x\n+y\n".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.flow_id = Some(flow_id.to_string());
    complete_params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    complete_params.evidence_refs = vec!["crates/tachi-server/src/complete_ops.rs".to_string()];
    complete_params.tests_run = vec!["cargo test -p tachi-server dispatch_tests".to_string()];
    complete_params.scope = Some("project".to_string());
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should succeed");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");
    assert_eq!(
        bundle["pipeline"]["dispatch_completion_link"],
        json!(true),
        "{bundle:#}"
    );

    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["completed_dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("eval"));
    assert_eq!(status["state"], json!("dispatch_completed"));
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["completion"]["task_id"], json!("eval-link-002"));
    assert_eq!(card["completion"]["outcome"], json!("success"));
    assert_eq!(
        card["completion"]["verification_present"],
        json!(true),
        "{card:#}"
    );

    let mut ux_params = task_params("ux_matrix");
    ux_params.flow_id = Some(flow_id.to_string());
    ux_params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    let ux_raw = server
        .tachi_task(Parameters(ux_params))
        .await
        .expect("ux_matrix should succeed");
    let ux: Value = serde_json::from_str(&ux_raw).expect("ux JSON");
    assert!(
        ux["matrix"].as_array().is_some_and(|steps| {
            steps.iter().any(|step| {
                step["id"] == json!("complete_eval")
                    && step["status"] == json!("passed")
                    && step["tool"]
                        == json!("tachi_task(action='complete', dispatch_id=..., flow_id=...)")
            })
        }),
        "{ux:#}"
    );
}

/// A successful replay must resume missing completion derives without
/// duplicating the canonical outcome, continuity events, or the flow marker.
/// The GitHub runner is deliberately reset here: completion reconciliation is
/// local-only and must never replay delivery commands.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn repeated_task_complete_reuses_outcome_and_completion_events() {
    let (server, _temp_home) = make_server_with_temp_home();
    let flow_id = "flow_20260726T000001Z-completion-replay";
    let dispatch_id = "20260726T000001Z-completion-replay";
    seed_dispatch_run(&server, dispatch_id);
    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({"agent": "codex", "profile": "codex_builder", "task": "reconcile completion"}),
    )
    .expect("mark dispatch");

    let mut params = task_params("complete");
    params.format = Some("full".to_string());
    params.task = Some("Reconcile a previously interrupted completion".to_string());
    params.agent = Some("codex".to_string());
    params.outcome = Some("success".to_string());
    params.task_id = None;
    params.task_type = Some("fix_request".to_string());
    params.profile = Some("codex_builder".to_string());
    params.dispatch_id = Some(dispatch_id.to_string());
    params.flow_id = Some(flow_id.to_string());
    params.scope = Some("global".to_string());
    params.evidence_refs = vec!["crates/tachi-server/src/complete_ops/handler.rs".to_string()];
    params.subagents = vec![crate::tool_params::TachiSubagentEvalParams {
        role: "reviewer".to_string(),
        agent: "codex-reviewer".to_string(),
        ..Default::default()
    }];

    crate::gh_ops::reset_github_command_runner_call_count();
    let first_raw = server
        .tachi_task(rmcp::handler::server::wrapper::Parameters(params.clone()))
        .await
        .expect("first task completion");
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let second_raw = server
        .tachi_task(rmcp::handler::server::wrapper::Parameters(params))
        .await
        .expect("replayed task completion");
    let first: Value = serde_json::from_str(&first_raw).expect("first completion JSON");
    let second: Value = serde_json::from_str(&second_raw).expect("second completion JSON");
    assert_ne!(
        first["task_id"], second["task_id"],
        "the fixture must cross the wall-clock fallback task-id boundary"
    );
    assert_eq!(
        second["pipeline"]["continuity_events"]["status"],
        json!("saved"),
        "an idempotent canonical replay must not surface an event collision: {second:#}"
    );

    let outcome_rows: i64 = server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM dispatch_outcomes WHERE dispatch_id = ?1",
                    [dispatch_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())
        })
        .expect("count canonical completion outcomes");
    assert_eq!(
        outcome_rows, 1,
        "a replay must reconcile one canonical outcome row"
    );

    for event_type in ["task.outcome", "subagent.evaluated"] {
        let events = server
            .with_global_store_read(|store| {
                store
                    .list_tachi_events(&memcore::TachiEventQuery {
                        event_type: Some(event_type.to_string()),
                        limit: 10,
                        ..memcore::TachiEventQuery::default()
                    })
                    .map_err(|error| error.to_string())
            })
            .expect("list replayed completion events");
        assert_eq!(
            events.len(),
            1,
            "replaying the same completion must retain exactly one {event_type} event"
        );
    }

    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("flow events");
    assert_eq!(
        events
            .lines()
            .filter(|line| line.contains("\"event\":\"dispatch_completed\""))
            .count(),
        1,
        "replaying a completion must not append another flow completion marker: {events}"
    );
    assert_eq!(
        crate::gh_ops::github_command_runner_call_count(),
        0,
        "completion replay must not invoke the GitHub command runner"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_infers_task_agent_and_profile_from_dispatch_card() {
    let (server, _temp_home) = make_server_with_temp_home();
    let flow_id = "flow_20260609T000004Z_complete_defaults_test";
    let dispatch_id = "20260609T000004Z-custom-defaults";
    seed_dispatch_run(&server, dispatch_id);

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "glm_impl",
            "task": "implementation from card",
        }),
    )
    .expect("mark dispatch");

    let mut complete_params = task_params("complete");
    complete_params.format = Some("full".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-link-004".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.flow_id = Some(flow_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should infer dispatch defaults");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");
    assert_eq!(bundle["recorded"], json!(true), "{bundle:#}");
    assert_eq!(bundle["agent"], json!("custom"), "{bundle:#}");
    assert_eq!(
        bundle["task"],
        json!("implementation from card"),
        "{bundle:#}"
    );
    assert_eq!(bundle["dispatch_id"], json!(dispatch_id), "{bundle:#}");
    assert_eq!(bundle["profile"], json!("glm_impl"), "{bundle:#}");
    assert_eq!(
        bundle["pipeline"]["dispatch_completion_link"]["recorded"],
        json!(true),
        "{bundle:#}"
    );

    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["agent"],
        json!("custom"),
        "{status:#}"
    );
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["task"],
        json!("implementation from card"),
        "{status:#}"
    );
}

/// #773 (S2 prep, sol-terminal-review-certified): eval rows carry
/// dispatch_id but almost never issue_ref because the calling agent must
/// manually re-supply it and mostly doesn't. `tachi_complete` must
/// auto-inject issue_ref from the dispatch's own kanban card (populated at
/// launch by `init_kanban_task`) whenever the caller supplies dispatch_id
/// but omits issue_ref.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_auto_injects_issue_ref_from_kanban_card_when_missing() {
    let (server, _temp_home) = make_server_with_temp_home();
    let dispatch_id = "20260712T000001Z-custom-issueref-autoinject";
    seed_dispatch_run(&server, dispatch_id);
    let issue_ref = "kckylechen1/tachi#773";

    // Seed the kanban card the way a real dispatch launch would
    // (`dispatch_ops::kanban_helpers::init_kanban_task`), with issue_ref on
    // file from launch but no explicit flow_id — this exercises the
    // stand-alone kanban lookup, not the flow_id-mediated path.
    crate::memory_search_ops::handle_save_memory(
        &server,
        crate::tool_params::SaveMemoryParams {
            text: "Dispatch Task\nAgent: custom\nTask: issue_ref autoinject fixture".to_string(),
            summary: "Kanban: issue_ref autoinject fixture".to_string(),
            path: format!("/kanban/tasks/{dispatch_id}"),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "kanban".to_string(),
            keywords: vec!["kanban".to_string(), "dispatch".to_string()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            project_explicit: false,
            retention_policy: Some(memcore::RetentionPolicy::Pinned.as_str().to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({
                "type": "a2a_task",
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
                "agent": "custom",
                "issue_ref": issue_ref,
                "eval_ledger_id": null,
            })),
            emit_continuity: false,
        },
    )
    .await
    .expect("seed kanban card with issue_ref on file");

    let mut complete_params = task_params("complete");
    complete_params.format = Some("full".to_string());
    complete_params.task = Some("Auto-inject issue_ref at completion".to_string());
    complete_params.agent = Some("custom".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-issueref-autoinject".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    complete_params.scope = Some("project".to_string());
    // Deliberately no issue_ref supplied — this is the propagation gap.
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should succeed");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");

    assert_eq!(
        bundle["issue_ref"],
        json!(issue_ref),
        "review bundle should reflect the auto-injected issue_ref: {bundle:#}"
    );

    // Confirm the persisted eval memory row's metadata itself carries the
    // auto-injected issue_ref (not just the transient response bundle).
    let eval_id = bundle["eval_entry"]["id"]
        .as_str()
        .expect("eval entry should return memory id")
        .to_string();
    let fetched_str = server
        .get_memory(Parameters(GetMemoryParams {
            id: eval_id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched: Value = serde_json::from_str(&fetched_str).expect("memory JSON");
    assert_eq!(
        fetched["metadata"]["issue_ref"],
        json!(issue_ref),
        "eval record metadata should carry the auto-injected issue_ref: {fetched:#}"
    );
}

/// Fail-safe half of #773: when there is no dispatch record to look up (or
/// the record has no issue_ref on file), completion must proceed without
/// error and without an issue_ref — never fail the completion over a
/// missing provenance value.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_proceeds_without_issue_ref_when_no_dispatch_record_found() {
    let (server, _temp_home) = make_server_with_temp_home();
    let dispatch_id = "20260712T000002Z-custom-no-kanban-record";
    seed_dispatch_run(&server, dispatch_id);

    let mut complete_params = task_params("complete");
    complete_params.format = Some("full".to_string());
    complete_params.task = Some("Complete with dispatch_id but no kanban card".to_string());
    complete_params.agent = Some("custom".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-no-issueref".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    complete_params.scope = Some("project".to_string());
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should still succeed with no dispatch record on file");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");

    assert_eq!(bundle["recorded"], json!(true), "{bundle:#}");
    assert!(
        bundle["issue_ref"].is_null(),
        "no issue_ref should be present when there is nothing to look up: {bundle:#}"
    );

    let eval_id = bundle["eval_entry"]["id"]
        .as_str()
        .expect("eval entry should return memory id")
        .to_string();
    let fetched_str = server
        .get_memory(Parameters(GetMemoryParams {
            id: eval_id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched: Value = serde_json::from_str(&fetched_str).expect("memory JSON");
    assert!(
        fetched["metadata"].get("issue_ref").is_none(),
        "eval metadata must not gain a phantom issue_ref key: {fetched:#}"
    );
}

/// tachi#1200 item 1: eval rows written by `tachi_complete` carry
/// `dispatch_id` but almost never a leader `profile` id (agents omit it, and
/// only the `tachi_task(action='complete')` bridge had any inference at
/// all — from a filesystem run artifact that frequently isn't on file).
/// Live policy replay (`route_simulate`/`recommend`) matches eval rows on
/// `EvalRow.profile` against a known dispatch profile name and silently
/// DROPS a row with no profile from every replay computation. This
/// auto-injects `profile` from the dispatch's own kanban card (populated at
/// launch by `init_kanban_task`) the same way `issue_ref` is auto-injected,
/// closing the gap for BOTH callers — including the direct `tachi_complete`
/// tool, which had zero profile inference before this fix.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_auto_injects_profile_from_kanban_card_when_missing() {
    let (server, _temp_home) = make_server_with_temp_home();
    let dispatch_id = "20260717T000001Z-custom-profile-autoinject";
    seed_dispatch_run(&server, dispatch_id);
    let profile = "opencode_builder";

    // Seed the kanban card the way a real dispatch launch would
    // (`dispatch_ops::kanban_helpers::init_kanban_task`), with profile on
    // file from launch but no explicit flow_id — this exercises the
    // stand-alone kanban lookup, not any flow_id-mediated path.
    crate::memory_search_ops::handle_save_memory(
        &server,
        crate::tool_params::SaveMemoryParams {
            text: "Dispatch Task\nAgent: custom\nTask: profile autoinject fixture".to_string(),
            summary: "Kanban: profile autoinject fixture".to_string(),
            path: format!("/kanban/tasks/{dispatch_id}"),
            importance: 0.7,
            category: "fact".to_string(),
            topic: "kanban".to_string(),
            keywords: vec!["kanban".to_string(), "dispatch".to_string()],
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            scope: "project".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            project_explicit: false,
            retention_policy: Some(memcore::RetentionPolicy::Pinned.as_str().to_string()),
            domain: Some("system".to_string()),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(json!({
                "type": "a2a_task",
                "dispatch_id": dispatch_id,
                "a2a_state": "TASK_STATE_WORKING",
                "agent": "custom",
                "profile": profile,
                "eval_ledger_id": null,
            })),
            emit_continuity: false,
        },
    )
    .await
    .expect("seed kanban card with profile on file");

    let mut complete_params = task_params("complete");
    complete_params.format = Some("full".to_string());
    complete_params.task = Some("Auto-inject profile at completion".to_string());
    complete_params.agent = Some("custom".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-profile-autoinject".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    complete_params.scope = Some("project".to_string());
    // Deliberately no profile supplied — this is the propagation gap.
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should succeed");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");

    assert_eq!(
        bundle["profile"],
        json!(profile),
        "review bundle should reflect the auto-injected profile: {bundle:#}"
    );

    // Confirm the persisted eval memory row's metadata itself carries the
    // auto-injected profile (not just the transient response bundle) — this
    // is the exact field `eval_row_from_memory` reads for policy replay.
    let eval_id = bundle["eval_entry"]["id"]
        .as_str()
        .expect("eval entry should return memory id")
        .to_string();
    let fetched_str = server
        .get_memory(Parameters(GetMemoryParams {
            id: eval_id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched: Value = serde_json::from_str(&fetched_str).expect("memory JSON");
    assert_eq!(
        fetched["metadata"]["profile"],
        json!(profile),
        "eval record metadata should carry the auto-injected profile: {fetched:#}"
    );
}

/// Fail-safe half of #1200: when there is no dispatch record to look up (or
/// the record has no profile on file), completion must proceed without error
/// and without a profile — never fabricate linkage that was never recorded.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_does_not_fabricate_profile_without_dispatch_record() {
    let (server, _temp_home) = make_server_with_temp_home();
    let dispatch_id = "20260717T000002Z-custom-no-kanban-profile";
    seed_dispatch_run(&server, dispatch_id);

    let mut complete_params = task_params("complete");
    complete_params.format = Some("full".to_string());
    complete_params.task = Some("Complete with dispatch_id but no kanban card".to_string());
    complete_params.agent = Some("custom".to_string());
    complete_params.outcome = Some("success".to_string());
    complete_params.task_id = Some("eval-no-profile".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    complete_params.scope = Some("project".to_string());
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should still succeed with no dispatch record on file");
    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");

    assert_eq!(bundle["recorded"], json!(true), "{bundle:#}");
    assert!(
        bundle["profile"].is_null(),
        "no profile should be present when there is nothing to look up: {bundle:#}"
    );

    let eval_id = bundle["eval_entry"]["id"]
        .as_str()
        .expect("eval entry should return memory id")
        .to_string();
    let fetched_str = server
        .get_memory(Parameters(GetMemoryParams {
            id: eval_id,
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get_memory should succeed");
    let fetched: Value = serde_json::from_str(&fetched_str).expect("memory JSON");
    assert!(
        fetched["metadata"].get("profile").is_none(),
        "eval metadata must not gain a phantom profile key: {fetched:#}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_complete_surfaces_warning_when_kanban_card_is_missing() {
    let (server, _temp_home) = make_server_with_temp_home();
    let dispatch_id = "20260615T000008Z-kanban-warning";
    let run_dir = server.tachi_home_dir().join("runs").join(dispatch_id);
    std::fs::create_dir_all(&run_dir).expect("create dispatch run directory");
    std::fs::write(
        run_dir.join("status.json"),
        json!({ "dispatch_id": dispatch_id }).to_string(),
    )
    .expect("seed dispatch status");

    let mut complete_params = task_params("complete");
    complete_params.format = Some("full".to_string());
    complete_params.task = Some("Write completion while kanban is stale".to_string());
    complete_params.agent = Some("codex".to_string());
    complete_params.outcome = Some("partial".to_string());
    complete_params.task_id = Some("eval-kanban-warning".to_string());
    complete_params.dispatch_id = Some(dispatch_id.to_string());
    complete_params.evidence_refs = vec!["result.md".to_string()];
    let raw = server
        .tachi_task(Parameters(complete_params))
        .await
        .expect("complete should still succeed");

    let bundle: Value = serde_json::from_str(&raw).expect("complete bundle");
    let warning = bundle["warning"]
        .as_str()
        .expect("kanban update warning should be surfaced");
    assert!(warning.contains("kanban card missing"), "{warning}");
    assert!(warning.contains(dispatch_id), "{warning}");
    assert!(warning.contains("eval-kanban-warning"), "{warning}");
    assert!(
        warning.contains("Write completion while kanban is stale"),
        "{warning}"
    );
    assert_eq!(
        bundle["pipeline"]["kanban_update"]["status"],
        json!("missing")
    );
    assert_eq!(
        bundle["pipeline"]["kanban_update"]["dispatch_id"],
        json!(dispatch_id)
    );
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("read completion receipt"),
    )
    .expect("receipt JSON");
    assert_eq!(
        status["resolved_completion"]["state"],
        json!("TASK_STATE_INPUT_REQUIRED"),
        "missing kanban must not prevent the durable partial receipt: {status:#}"
    );
    assert_eq!(
        status["resolved_completion"]["closure_kind"],
        json!("partial"),
        "the durable receipt must distinguish partial from ordinary input: {status:#}"
    );
}
