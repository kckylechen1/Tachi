use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_briefing_returns_feature_scoped_handoff_board() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let runs_root = crate::dispatch_ops::runs_dir_for_server(&server);
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", &runs_root);
    let flow_id = "flow_20260608T000000Z_feature_briefing_test";
    let run_dir = runs_root.join(flow_id);
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    std::fs::write(
        run_dir.join("instruction.md"),
        format!("# Instruction\n\nflow_id: {flow_id}\n"),
    )
    .expect("write instruction");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string(&json!({
            "flow_id": flow_id,
            "stage": "dispatch",
            "updated_at": Utc::now().to_rfc3339(),
        }))
        .expect("status json"),
    )
    .expect("write status");
    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        "dispatch-feature-briefing",
        json!({
            "agent": "custom",
            "profile": "codex_55_review",
            "task": "feature briefing handoff",
        }),
    )
    .expect("link feature briefing dispatch to its flow descriptor");

    server
        .with_global_store(|store| {
            let mut wiki = make_entry("wiki-feature-briefing");
            wiki.path = "/wiki/agent/tachi/feature-briefing".to_string();
            wiki.summary = "Feature briefing should separate docs wiki and memory".to_string();
            wiki.text =
                "FeatureBriefingNeedle durable wiki lesson for handoff board layering.".to_string();
            wiki.category = "experience".to_string();
            wiki.topic = "feature_briefing".to_string();
            wiki.scope = "global".to_string();
            wiki.retention_policy = Some("permanent".to_string());
            store.upsert(&wiki).map_err(|e| e.to_string())?;

            let mut guide = make_entry("guide-feature-briefing-agent-review");
            guide.path = "/guide/global/workflows/agent-review".to_string();
            guide.summary = "AgentReview guide for review dispatch".to_string();
            guide.text = "FeatureBriefingNeedle AgentReview outputs should be routed by destination layer and promotion intent.".to_string();
            guide.category = "guide".to_string();
            guide.topic = "agent-review-guide".to_string();
            guide.scope = "global".to_string();
            guide.retention_policy = Some("permanent".to_string());
            guide.metadata = json!({
                "layer": "guide",
                "scope": "global",
                "authority": "playbook",
                "status": "active",
                "applies_to": {
                    "task_type": ["agent_review"],
                    "profiles": ["codex_55_review"],
                    "stage": ["review"]
                },
                "keywords": ["FeatureBriefingNeedle", "agent-review"]
            });
            store.upsert(&guide).map_err(|e| e.to_string())?;

            let mut feedback = make_entry("feedback-feature-briefing-agent-review");
            feedback.path = "/feedback/global/agent-review/promotion-routing".to_string();
            feedback.summary = "AgentReview findings need explicit promotion evidence".to_string();
            feedback.text = "FeatureBriefingNeedle review findings should list target destination and leader verdict before promotion.".to_string();
            feedback.category = "prompt_rule".to_string();
            feedback.topic = "agent-review-promotion-routing".to_string();
            feedback.scope = "global".to_string();
            feedback.metadata = json!({
                "kind": "feedback_rule",
                "layer": "feedback_rule",
                "scope": "global",
                "authority": "behavior_patch",
                "status": "active",
                "applies_to": {
                    "task_type": ["agent_review"],
                    "profiles": ["codex_55_review"],
                    "stage": ["review"]
                },
                "trigger_keywords": ["FeatureBriefingNeedle", "promotion", "routing"],
                "prompt_patch": "Before promoting a review finding, name the destination layer and cite the leader verdict."
            });
            store.upsert(&feedback).map_err(|e| e.to_string())?;

            let mut unrelated = make_entry("global-unrelated-feature-briefing");
            unrelated.path = "/scratch/other/global-memory-dump".to_string();
            unrelated.summary = "FeatureBriefingNeedle unrelated global fragment".to_string();
            unrelated.text = "This global memory would appear in a broad memory dump.".to_string();
            unrelated.scope = "global".to_string();
            store.upsert(&unrelated).map_err(|e| e.to_string())?;

            let mut kanban = make_entry("kanban-feature-briefing");
            kanban.path = "/kanban/tasks/feature-briefing".to_string();
            kanban.summary = format!("{flow_id} worker is running");
            kanban.text = "kanban dispatch task feature briefing".to_string();
            kanban.category = "kanban".to_string();
            kanban.metadata = json!({
                "dispatch_id": "dispatch-feature-briefing",
                "agent": "custom",
                "a2a_state": "TASK_STATE_WORKING",
                "updated_at": Utc::now().to_rfc3339(),
            });
            store.upsert(&kanban).map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .expect("seed briefing fixtures");

    let mut params = task_params("briefing");
    params.format = Some("json".to_string());
    params.task = Some(
        "FeatureBriefingNeedle implement docs/engineering/architecture/subagent-eval-system.md"
            .to_string(),
    );
    params.flow_id = Some(flow_id.to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()];
    params.top_k = Some(5);
    params.task_type = Some("agent_review".to_string());
    params.profile = Some("codex_55_review".to_string());
    params.stage = Some("review".to_string());

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("feature briefing should succeed");
    let briefing: Value = serde_json::from_str(&raw).expect("briefing JSON");

    assert_eq!(briefing["kind"], json!("feature_briefing"));
    assert_eq!(briefing["scope"]["flow_id"], json!(flow_id));
    assert!(briefing["project_work_record"]
        .as_array()
        .is_some_and(|records| records.iter().any(|record| {
            record["kind"] == json!("github_issue")
                && record["ref"] == json!("kckylechen1/tachi#194")
                && record["authority"] == json!("project_work_record")
        })));
    assert!(briefing["canonical_docs"]
        .as_array()
        .is_some_and(|docs| docs.iter().any(|doc| {
            doc["path"] == json!("docs/engineering/architecture/subagent-eval-system.md")
                && doc["exists"] == json!(true)
                && doc["authority"] == json!("canonical")
        })));
    assert!(briefing["run_artifacts"]
        .as_array()
        .is_some_and(|artifacts| artifacts.iter().any(|artifact| {
            artifact["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("instruction.md"))
                && artifact["exists"] == json!(true)
                && artifact["authority"] == json!("runtime_state")
        })));
    assert!(briefing["board_state"]["tasks"]
        .as_array()
        .is_some_and(|tasks| tasks.iter().any(|task| {
            task["dispatch_id"] == json!("dispatch-feature-briefing")
                && task["state"] == json!("TASK_STATE_WORKING")
        })));
    assert!(briefing["guide_hits"]
        .as_array()
        .is_some_and(|hits| hits.iter().any(|hit| {
            hit["path"] == json!("/guide/global/workflows/agent-review")
                && hit["authority"] == json!("playbook")
                && hit["applies_to"]["profiles"] == json!(["codex_55_review"])
        })));
    if let Some(hits) = briefing.get("wiki_hits").and_then(Value::as_array) {
        assert!(hits
            .iter()
            .all(|hit| hit["path"] != json!("/guide/global/workflows/agent-review")));
    }
    assert!(briefing["feedback_rules"]["rules"]
        .as_array()
        .is_some_and(|rules| rules.iter().any(|rule| {
            rule["path"] == json!("/feedback/global/agent-review/promotion-routing")
                && rule["layer"] == json!("feedback_rule")
                && rule["authority"] == json!("behavior_patch")
        })));
    let groups = briefing["doc_index"]["groups"]
        .as_array()
        .expect("doc index groups");
    let project_wiki_group = groups
        .iter()
        .find(|group| group["name"] == json!("project_wiki"))
        .expect("project wiki group");
    assert!(
        project_wiki_group["items"]
            .as_array()
            .is_some_and(|hits| hits.iter().any(|hit| {
                hit["path"] == json!("/wiki/agent/tachi/feature-briefing")
                    && hit["layer"] == json!("wiki")
                    && hit["authority"] == json!("advisory")
            })),
        "project wiki rows should stay in doc_index: {briefing:#}"
    );
    assert!(
        !briefing
            .as_object()
            .expect("feature briefing object")
            .contains_key("wiki_hits"),
        "top-level wiki_hits should be omitted when doc_index already carries every wiki hit: {briefing:#}"
    );
    let doc_index_ids = groups
        .iter()
        .flat_map(|group| {
            group["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| item["id"].as_str())
        })
        .collect::<std::collections::HashSet<_>>();
    if let Some(wiki_hits) = briefing.get("wiki_hits").and_then(Value::as_array) {
        for hit in wiki_hits {
            if let Some(id) = hit["id"].as_str() {
                assert!(
                    !doc_index_ids.contains(id),
                    "wiki id {id} appears in both top-level wiki_hits and doc_index: {briefing:#}"
                );
            }
        }
    }
    for expected in [
        "project_work_record",
        "canonical_docs",
        "project_wiki",
        "global_guide",
        "feedback_rules",
        "eval_evidence",
        "runtime_artifacts",
    ] {
        assert!(
            groups.iter().any(|group| group["name"] == json!(expected)),
            "missing doc_index group {expected}: {briefing:#}"
        );
    }
    assert_eq!(
        briefing["doc_index"]["authority_order"][0],
        json!("project_work_record")
    );
    assert!(briefing["route_recommendation"]["recommended_profile"]
        .as_str()
        .is_some_and(|profile| !profile.is_empty()));
    assert!(briefing["relevant_profiles"]
        .as_array()
        .is_some_and(|profiles| {
            profiles.iter().any(|profile| {
                profile["profile"] == briefing["route_recommendation"]["recommended_profile"]
            })
        }));
    assert_eq!(
        briefing["suggested_handoff"]["mechanism"],
        json!("harness_native_subagent"),
        "feature briefing should preserve the host harness's worker lifecycle: {briefing:#}"
    );
    assert_eq!(
        briefing["suggested_handoff"]["context"]["issue_ref"],
        json!("kckylechen1/tachi#194")
    );
    assert_eq!(
        briefing["suggested_handoff"]["context"]["flow_id"],
        json!(flow_id)
    );
    assert!(briefing["suggested_handoff"]["context"]["advisory_profile"]
        .as_str()
        .is_some_and(|profile| !profile.is_empty()));
    assert!(
        briefing.get("suggested_dispatch").is_none(),
        "advisory routing must not be converted into a Tachi dispatch call: {briefing:#}"
    );
    assert!(
        briefing["memory_fragments"]
            .as_array()
            .is_some_and(|hits| hits.is_empty()),
        "feature briefing should not include broad global memory fragments by default: {briefing:#}"
    );
    assert!(
        briefing["next_action"]
            .as_str()
            .is_some_and(|action| action.contains("Poll tachi_task(action='board')")),
        "working board task should drive next action: {briefing:#}"
    );
}
