use super::super::{make_entry, make_server, make_server_with_temp_home};
use super::{dispatch_params, task_params, EnvVarGuard};
use crate::tool_params::{GetMemoryParams, TaskBriefParams};
use chrono::Utc;
use memory_core::MemoryEntry;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
#[tokio::test]
async fn tachi_task_brief_uses_wiki_hits_for_debug_checklist() {
    let server = make_server();

    server
        .with_global_store(|store| {
            store
                .upsert(&MemoryEntry {
                    id: "wiki-debug-mcp-args".to_string(),
                    path: "/wiki/debug/mcp-args".to_string(),
                    summary: "Debug MCP argument serialization bug".to_string(),
                    text: "Debug MCP argument serialization bug checklist:\n- Verify schema -> client serialization -> server deserialization before editing transport.\n- Add a failing boundary test at the API boundary before retrying the same layer.\n- Stop after two failed patches in the same layer and ask another agent.\n\nThis note exists specifically for an MCP argument serialization bug that looks tempting to misdiagnose as a transport issue.".to_string(),
                    importance: 0.9,
                    timestamp: Utc::now().to_rfc3339(),
                    valid_from: String::new(),
                    valid_until: None,
                    category: "experience".to_string(),
                    topic: "mcp_args".to_string(),
                    keywords: vec!["mcp".to_string(), "debugging".to_string()],
                    persons: vec![],
                    entities: vec![],
                    location: String::new(),
                    source: "test".to_string(),
                    scope: "global".to_string(),
                    archived: false,
                    access_count: 0,
                    last_access: None,
                    revision: 1,
                    metadata: json!({}),
                    vector: None,
                    retention_policy: Some("permanent".to_string()),
                    domain: Some("coding".to_string()),
                    recall_count: 0,
                    query_diversity: 0,
                    tier: "raw".to_string(),
                })
                .map_err(|e| e.to_string())
        })
        .expect("seed wiki debugging note");

    let response = server
        .tachi_task_brief(Parameters(TaskBriefParams {
            task: "Debug MCP argument serialization bug".to_string(),
            agent_id: Some("copilot".to_string()),
            project: None,
            path_prefix: None,
            domain: Some("coding".to_string()),
            top_k: 3,
        }))
        .await
        .expect("tachi_task_brief should succeed");

    let json: Value = serde_json::from_str(&response).expect("task brief response json");
    assert!(
        json["wiki_hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty()),
        "expected wiki hits for matching task, got: {json}"
    );
    assert!(json["debug_checklist"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| {
            item.as_str().is_some_and(|text| {
                text.contains("schema -> client serialization -> server deserialization")
            })
        })));
    assert_eq!(
        json["suggested_next_tools"],
        json!([
            "tachi_wiki(action='search')",
            "tachi_skill(action='discover')",
            "tachi_task(action='plan')",
            "tachi_task(action='board')"
        ])
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_briefing_returns_feature_scoped_handoff_board() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000000Z_feature_briefing_test";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("valid flow id");
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
    assert!(briefing["wiki_hits"]
        .as_array()
        .is_some_and(|hits| hits.iter().any(|hit| {
            hit["path"] == json!("/wiki/agent/tachi/feature-briefing")
                && hit["layer"] == json!("wiki")
                && hit["authority"] == json!("advisory")
        })));
    assert!(briefing["guide_hits"]
        .as_array()
        .is_some_and(|hits| hits.iter().any(|hit| {
            hit["path"] == json!("/guide/global/workflows/agent-review")
                && hit["authority"] == json!("playbook")
                && hit["applies_to"]["profiles"] == json!(["codex_55_review"])
        })));
    assert!(briefing["wiki_hits"].as_array().is_some_and(|hits| {
        hits.iter()
            .all(|hit| hit["path"] != json!("/guide/global/workflows/agent-review"))
    }));
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
        briefing["suggested_dispatch"]["tool"],
        json!("tachi_task"),
        "feature briefing should tell leaders which facade to call next: {briefing:#}"
    );
    assert_eq!(
        briefing["suggested_dispatch"]["arguments"]["action"],
        json!("dispatch")
    );
    assert_eq!(
        briefing["suggested_dispatch"]["arguments"]["issue_ref"],
        json!("kckylechen1/tachi#194")
    );
    assert_eq!(
        briefing["suggested_dispatch"]["arguments"]["flow_id"],
        json!(flow_id)
    );
    assert!(briefing["suggested_dispatch"]["arguments"]["profile"]
        .as_str()
        .is_some_and(|profile| !profile.is_empty()));
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

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_briefing_supports_markdown_layered_sections() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();

    let mut params = task_params("briefing");
    params.format = Some("markdown".to_string());
    params.task = Some("Prepare feature handoff".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()];

    let body = server
        .tachi_task(Parameters(params))
        .await
        .expect("markdown briefing should succeed");

    assert!(body.starts_with("# Feature Briefing"), "{body}");
    for section in [
        "## Project Work Record",
        "## Canonical Docs / Specs",
        "## Run Artifacts",
        "## Board State",
        "## Guide / SOP",
        "## Feedback Rules",
        "## Recommended Dispatch",
        "## Relevant Skills / Profiles",
        "## Wiki Decisions / Lessons",
        "## Memory Fragments / Checkpoints",
        "## Eval Evidence",
        "## Next Action",
    ] {
        assert!(body.contains(section), "missing {section}: {body}");
    }
    assert!(body.contains("Dispatch args:"), "{body}");
}

#[tokio::test]
async fn tachi_task_doc_index_returns_layered_authority_groups() {
    let server = make_server();
    let mut params = task_params("doc_index");
    params.format = Some("json".to_string());
    params.task = Some(
        "Implement issue-driven docs flow docs/engineering/architecture/subagent-eval-system.md"
            .to_string(),
    );
    params.issue_ref = Some("kckylechen1/tachi#363".to_string());
    params.pr_ref = Some("kckylechen1/tachi#364".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()];

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("doc_index should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("doc_index JSON");

    assert_eq!(parsed["kind"], json!("doc_index"));
    assert!(parsed["project_work_record"]
        .as_array()
        .is_some_and(|records| records.iter().any(|record| {
            record["kind"] == json!("github_issue")
                && record["ref"] == json!("kckylechen1/tachi#363")
        })));
    assert!(parsed["doc_index"]["groups"]
        .as_array()
        .is_some_and(|groups| groups.iter().any(|group| {
            group["name"] == json!("canonical_docs")
                && group["authority"] == json!("canonical")
                && group["count"].as_u64().unwrap_or(0) > 0
        })));
}

#[test]
fn tachi_task_intake_parses_issue_refs_without_accepting_pr_urls() {
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref("kckylechen1/tachi#194", None),
        Some(crate::task_lifecycle::GithubTarget {
            repo: "kckylechen1/tachi".to_string(),
            number: 194,
        })
    );
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref(
            "https://github.com/kckylechen1/tachi/issues/194/",
            None
        ),
        Some(crate::task_lifecycle::GithubTarget {
            repo: "kckylechen1/tachi".to_string(),
            number: 194,
        })
    );
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref("#194", Some("kckylechen1/tachi")),
        Some(crate::task_lifecycle::GithubTarget {
            repo: "kckylechen1/tachi".to_string(),
            number: 194,
        })
    );
    assert_eq!(
        crate::task_lifecycle::parse_issue_ref(
            "https://github.com/kckylechen1/tachi/pull/194",
            None
        ),
        None
    );
}

#[tokio::test]
async fn tachi_task_intake_requires_issue_target_before_github_access() {
    let server = make_server();
    let params = task_params("intake");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing issue target should fail before GitHub access");
    assert_eq!(
        err,
        "intake requires either repo+number or issue_ref='owner/repo#123' / GitHub issue URL"
    );
}

#[tokio::test]
async fn tachi_task_link_pr_requires_flow_id_before_github_access() {
    let server = make_server();
    let mut params = task_params("link_pr");
    params.pr_ref = Some("kckylechen1/tachi#229".to_string());
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing flow_id should fail before GitHub access");
    assert_eq!(err, "flow_id is required for link_pr");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tachi_task_intake_and_link_pr_artifacts_feed_briefing() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000001Z_intake_link_pr_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some("## Acceptance criteria\n- Flow artifacts feed briefing.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()],
        spec_paths: Vec::new(),
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");

    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 229,
        title: "Add task PR status preview".to_string(),
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/229".to_string(),
        head_ref: Some("feat/task-pr-status".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("status json");
    assert_eq!(status["issue_ref"], json!("kckylechen1/tachi#194"));
    assert_eq!(status["pr_ref"], json!("kckylechen1/tachi#229"));
    assert_eq!(status["github"]["issue_number"], json!(194));
    assert_eq!(status["github"]["pr_number"], json!(229));
    assert_eq!(status["github"]["merge_state"], json!("merged"));
    assert!(run_dir.join("instruction.md").exists());
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(events.contains("github_issue_linked"), "{events}");
    assert!(events.contains("github_pr_updated"), "{events}");
    assert_eq!(
        crate::task_lifecycle::resolve_link_pr_issue_ref(flow_id, None).expect("inherited issue"),
        Some("kckylechen1/tachi#194".to_string())
    );
    assert!(
        crate::task_lifecycle::resolve_link_pr_issue_ref(flow_id, Some("other/repo#999"))
            .expect_err("mismatched issue_ref should be rejected")
            .contains("link_pr issue_ref mismatch")
    );

    let mut params = task_params("briefing");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("briefing should read flow docs");
    let briefing: Value = serde_json::from_str(&raw).expect("briefing JSON");
    assert!(briefing["canonical_docs"]
        .as_array()
        .is_some_and(|docs| docs.iter().any(|doc| {
            doc["kind"] == json!("flow_doc")
                && doc["path"] == json!("docs/engineering/architecture/subagent-eval-system.md")
        })));
    assert!(briefing["run_artifacts"]
        .as_array()
        .is_some_and(|artifacts| artifacts.iter().any(|artifact| {
            artifact["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("instruction.md"))
                && artifact["exists"] == json!(true)
        })));
}

#[tokio::test]
async fn tachi_task_build_references_reuses_workflow_closure() {
    let server = make_server();
    let mut params = task_params("build_references");
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/agent-flow.md".to_string()];
    params.related_issues = vec!["#153".to_string(), "kckylechen1/tachi#194".to_string()];

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("task build_references should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("task response JSON");
    assert_eq!(
        parsed["references"],
        json!([
            "kckylechen1/tachi#194",
            "docs/engineering/architecture/agent-flow.md",
            "#153"
        ])
    );
    assert_eq!(
        parsed["promotion_plan"]["requires_explicit_invocation"],
        json!(true)
    );
    assert_eq!(
        parsed["promotion_plan"]["automatic_double_write"],
        json!(false)
    );
    assert!(parsed["promotion_plan"]["destinations"]
        .as_array()
        .is_some_and(|destinations| destinations
            .iter()
            .any(|dest| dest["destination"] == json!("feedback_rule"))));
}

#[tokio::test]
async fn tachi_task_close_loop_writes_wiki_with_references() {
    let server = make_server();
    let mut params = task_params("close_loop");
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    params.doc_paths = vec!["docs/engineering/architecture/agent-flow.md".to_string()];
    params.related_issues = vec!["#153".to_string()];
    params.wiki_title = Some("Task closure facade smoke".to_string());
    params.wiki_text = Some("Closed loop lesson through tachi_task facade.".to_string());
    params.wiki_topic = Some("task-closure-facade".to_string());
    params.force = true;

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("task close_loop should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("task response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("close_loop"));
    assert_eq!(
        parsed["promotion_plan"]["automatic_double_write"],
        json!(false)
    );
    let wiki_id = parsed["wiki"]["id"].as_str().expect("wiki id");
    let fetched = server
        .get_memory(Parameters(GetMemoryParams {
            id: wiki_id.to_string(),
            include_archived: false,
            project: None,
        }))
        .await
        .expect("get wiki entry");
    let entry: Value = serde_json::from_str(&fetched).expect("entry JSON");
    assert_eq!(
        entry["metadata"]["source_refs"],
        json!([
            "kckylechen1/tachi#194",
            "docs/engineering/architecture/agent-flow.md",
            "#153"
        ])
    );
    assert_eq!(
        entry["metadata"]["promotion"]["decision_mode"],
        json!("explicit_invocation")
    );
    assert_eq!(
        entry["metadata"]["promotion"]["destination_layer"],
        json!("wiki")
    );
    assert_eq!(
        entry["metadata"]["promotion"]["automatic_double_write"],
        json!(false)
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_close_loop_marks_flow_complete_for_ux_matrix() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000006Z_close_loop_marker_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 239,
        title: "Persist close_loop marker".to_string(),
        body: Some("## Acceptance criteria\n- close_loop marks the flow complete.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/239".to_string(),
        doc_paths: vec!["docs/engineering/architecture/credential-adapters-cleanup.md".to_string()],
        spec_paths: Vec::new(),
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Persist close_loop marker",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");

    let mut close_params = task_params("close_loop");
    close_params.flow_id = Some(flow_id.to_string());
    close_params.issue_ref = Some("kckylechen1/tachi#239".to_string());
    close_params.wiki_title = Some("Close loop marker smoke".to_string());
    close_params.wiki_text = Some("Close loop should mark the flow complete.".to_string());
    close_params.wiki_topic = Some("close-loop-marker".to_string());
    close_params.force = true;
    let raw = server
        .tachi_task(Parameters(close_params))
        .await
        .expect("close_loop should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("close_loop response JSON");
    assert_eq!(parsed["ok"], json!(true));

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    assert!(run_dir.join("close_loop.json").exists());
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert_eq!(status["state"], json!("closed_loop"));
    assert!(status["artifacts"]["close_loop"]
        .as_str()
        .is_some_and(|path| path.ends_with("close_loop.json")));

    let mut ux_params = task_params("ux_matrix");
    ux_params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(ux_params))
        .await
        .expect("ux_matrix should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["overall"], json!("complete"));
    let matrix = parsed["matrix"].as_array().expect("matrix array");
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("close_loop") && step["status"] == json!("passed") }));
}

#[test]
#[allow(clippy::await_holding_lock)]
fn tachi_task_dispatch_marker_updates_flow_status_idempotently() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260608T000007Z_dispatch_marker_test";
    let dispatch_id = "20260608T000007Z-custom-marker";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "deepseek_explore",
            "task": "read-only review",
        }),
    )
    .expect("mark dispatch");
    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "deepseek_explore",
            "task": "read-only review",
        }),
    )
    .expect("mark dispatch idempotently");
    assert!(
        crate::task_lifecycle::mark_task_dispatch(flow_id, "../bad", json!({})).is_err(),
        "dispatch marker ids must stay filename-safe"
    );

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("dispatch"));
    assert_eq!(status["state"], json!("dispatched"));
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    assert!(
        std::path::Path::new(card_path).exists(),
        "dispatch card should exist: {status:#}"
    );
    assert_eq!(
        status["dispatch_cards"].as_array().map(Vec::len),
        Some(1),
        "dispatch card list should not duplicate entries: {status:#}"
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(events.contains("\"event\":\"dispatch_linked\""), "{events}");
}

#[test]
#[allow(clippy::await_holding_lock)]
fn tachi_task_dispatch_completion_marker_updates_card_and_status_idempotently() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260609T000001Z_dispatch_completion_marker_test";
    let dispatch_id = "20260609T000001Z-custom-complete";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "custom",
            "profile": "glm_51_impl",
            "task": "implementation",
        }),
    )
    .expect("mark dispatch");
    crate::task_lifecycle::mark_task_dispatch_completion(
        flow_id,
        dispatch_id,
        json!({
            "task_id": "eval-link-001",
            "outcome": "success",
            "eval_memory_id": "memory-eval-001",
            "eval_path": "/eval/2026-06-09/eval-link-001",
            "verification_present": true,
            "tests_run": ["cargo test -p memory-server dispatch_tests"],
        }),
    )
    .expect("mark completion");
    crate::task_lifecycle::mark_task_dispatch_completion(
        flow_id,
        dispatch_id,
        json!({
            "task_id": "eval-link-001",
            "outcome": "success",
            "eval_memory_id": "memory-eval-001",
            "eval_path": "/eval/2026-06-09/eval-link-001",
            "verification_present": true,
        }),
    )
    .expect("mark completion idempotently");

    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert_eq!(status["completed_dispatch_ids"], json!([dispatch_id]));
    assert_eq!(status["stage"], json!("eval"));
    assert_eq!(status["state"], json!("dispatch_completed"));
    assert_eq!(
        status["dispatch_eval"][dispatch_id]["eval_memory_id"],
        json!("memory-eval-001")
    );
    assert_eq!(
        status["artifacts"]["dispatch_completions"][dispatch_id]["outcome"],
        json!("success")
    );
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(
        card["completion"]["eval_path"],
        json!("/eval/2026-06-09/eval-link-001")
    );
    assert_eq!(
        card["completion_history"].as_array().map(Vec::len),
        Some(1),
        "same eval should not duplicate completion history: {card:#}"
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(
        events.contains("\"event\":\"dispatch_completed\""),
        "{events}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_dispatch_with_flow_id_records_dispatch_card() {
    let (server, _temp_home) = make_server_with_temp_home();
    let tmp = tempfile::tempdir().expect("temp dispatch cwd");
    let flow_id = "flow_20260608T000008Z_dispatch_card_test";
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("flow run dir");
    std::fs::create_dir_all(&run_dir).expect("create flow run dir");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow_id,
            "dispatch_ids": [],
        }))
        .expect("serialize status"),
    )
    .expect("seed status");

    let mut params = dispatch_params(Some("custom"), "smoke flow dispatch marker");
    params.command = vec![
        "python3".to_string(),
        "-c".to_string(),
        "print('ok')".to_string(),
    ];
    params.cwd = Some(tmp.path().to_string_lossy().to_string());
    params.profile = Some("glm_51_impl".to_string());
    params.flow_id = Some(flow_id.to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());

    let raw = crate::dispatch_ops::handle_tachi_dispatch(&server, params)
        .await
        .expect("custom dispatch should start");
    let response: Value = serde_json::from_str(&raw).expect("dispatch JSON");
    let dispatch_id = response["dispatch_id"].as_str().expect("dispatch id");

    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status JSON");
    assert!(
        status["dispatch_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(dispatch_id))),
        "flow status should include dispatch id {dispatch_id}: {status:#}"
    );
    assert!(
        status["artifacts"]["dispatches"][dispatch_id]
            .as_str()
            .is_some_and(|path| path.ends_with(".json")),
        "flow status should link compact dispatch card: {status:#}"
    );
    let card_path = status["artifacts"]["dispatches"][dispatch_id]
        .as_str()
        .expect("dispatch card path");
    let card: Value = serde_json::from_str(&std::fs::read_to_string(card_path).expect("card"))
        .expect("card JSON");
    assert_eq!(card["suggested_complete"]["tool"], json!("tachi_task"));
    assert_eq!(
        card["suggested_complete"]["arguments"]["action"],
        json!("complete")
    );
    assert_eq!(
        card["suggested_complete"]["arguments"]["dispatch_id"],
        json!(dispatch_id)
    );
    assert_eq!(
        card["suggested_complete"]["arguments"]["flow_id"],
        json!(flow_id)
    );
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(
        events.contains(dispatch_id) && events.contains("\"event\":\"dispatch_linked\""),
        "{events}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_board_filters_to_flow_dispatch_ids() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _tachi_home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let flow_id = "flow_20260609T000005Z_board_flow_filter";
    let dispatch_id = "20260609T000005Z-codex-flow";
    let other_dispatch_id = "20260609T000006Z-codex-other";

    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({
            "agent": "codex",
            "profile": "codex_53_fast",
            "task": "flow task",
        }),
    )
    .expect("mark dispatch");
    for (id, task) in [
        (dispatch_id, "flow task"),
        (other_dispatch_id, "other task"),
    ] {
        let run_dir = temp_home.path().join("runs").join(id);
        std::fs::create_dir_all(&run_dir).expect("create run dir");
        if id == dispatch_id {
            std::fs::write(run_dir.join("result.md"), "worker completed").expect("result");
        }
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::to_string_pretty(&json!({
                "dispatch_id": id,
                "agent": "codex",
                "task": task,
                "state": "TASK_STATE_COMPLETED",
                "exit_code": 0,
                "updated_at": Utc::now().to_rfc3339(),
            }))
            .expect("status json"),
        )
        .expect("write status");
    }

    let mut params = task_params("board");
    params.flow_id = Some(flow_id.to_string());
    params.limit = Some(20);
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("board should succeed");
    let board: Value = serde_json::from_str(&raw).expect("board JSON");
    assert_eq!(board["flow_id"], json!(flow_id), "{board:#}");
    assert_eq!(board["run_count"], json!(1), "{board:#}");
    let tasks = board["tasks"].as_array().expect("tasks");
    assert_eq!(tasks.len(), 1, "{board:#}");
    assert_eq!(tasks[0]["dispatch_id"], json!(dispatch_id), "{board:#}");
    assert_eq!(
        tasks[0]["state"],
        json!("TASK_STATE_COMPLETED"),
        "{board:#}"
    );
    assert_eq!(tasks[0]["exit_code"], json!(0), "{board:#}");
    assert_eq!(tasks[0]["result_written"], json!(true), "{board:#}");
    assert_eq!(tasks[0]["state_source"], json!("run"), "{board:#}");
    assert!(
        tasks[0]["run_dir"]
            .as_str()
            .is_some_and(|path| path.ends_with(dispatch_id)),
        "{board:#}"
    );
}

#[test]
fn tachi_task_pr_status_parses_repo_number_and_pr_ref() {
    let mut params = task_params("pr_status");
    params.repo = Some("kckylechen1/tachi".to_string());
    params.number = Some(228);
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("repo+number"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.repo = None;
    params.number = None;
    params.pr_ref = Some("kckylechen1/tachi#228".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("owner/repo#number"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.pr_ref = Some("https://github.com/kckylechen1/tachi/pull/228".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("github PR URL"),
        ("kckylechen1/tachi".to_string(), 228)
    );

    params.pr_ref = Some(" https://github.com/kckylechen1/tachi/pull/228/ ".to_string());
    assert_eq!(
        crate::tools::resolve_task_pr_status_target(&params).expect("trimmed github PR URL"),
        ("kckylechen1/tachi".to_string(), 228)
    );
}

#[test]
fn tachi_task_pr_status_rejects_ambiguous_pr_refs() {
    for pr_ref in [
        "",
        "kckylechen1/tachi#",
        "kckylechen1/tachi/extra#228",
        "https://github.com/kckylechen1/tachi/issues/228",
        "https://github.com/kckylechen1/tachi/pull/228/files",
    ] {
        let mut params = task_params("pr_status");
        params.pr_ref = Some(pr_ref.to_string());
        assert!(
            crate::tools::resolve_task_pr_status_target(&params).is_err(),
            "unexpectedly accepted pr_ref={pr_ref:?}"
        );
    }
}

#[test]
fn tachi_task_pr_status_builds_safe_merge_preview_params() {
    let mut params = task_params("pr_status");
    params.pr_ref = Some("kckylechen1/tachi#228".to_string());
    params.flow_id = Some("flow_pr_status".to_string());
    params.merge_policy = Some("strict".to_string());
    params.strategy = Some("squash".to_string());
    params.confirm = true;

    let gh_params =
        crate::tools::build_task_pr_status_gh_params(&params).expect("pr_status params");
    assert_eq!(gh_params.action, "safe_merge");
    assert_eq!(gh_params.repo, "kckylechen1/tachi");
    assert_eq!(gh_params.number, Some(228));
    assert_eq!(gh_params.dry_run, Some(true));
    assert!(!gh_params.confirm);
    assert_eq!(gh_params.flow_id.as_deref(), Some("flow_pr_status"));
    assert_eq!(gh_params.merge_policy.as_deref(), Some("strict"));
    assert_eq!(gh_params.merge_strategy, None);
}

#[tokio::test]
async fn tachi_task_pr_status_requires_repo_number_or_parseable_ref() {
    let server = make_server();
    let params = task_params("pr_status");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing target should fail before GitHub access");
    assert_eq!(
        err,
        "pr_status requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}

#[tokio::test]
async fn tachi_task_release_note_requires_flow_or_pr_ref_before_github_access() {
    let server = make_server();
    let params = task_params("release_note");
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("missing release note target should fail before GitHub access");
    assert_eq!(
        err,
        "release_note requires flow_id or pr_ref='owner/repo#123' / GitHub PR URL"
    );
}

#[test]
fn issue_automation_plan_blocks_missing_acceptance_and_high_risk() {
    let missing_acceptance = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 380,
        title: "Automate issue dispatch".to_string(),
        body: Some("Let Tachi read an issue and do the work.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/380".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let plan = crate::task_lifecycle::build_issue_automation_plan(&missing_acceptance, None);
    assert_eq!(plan["dispatch_allowed"], json!(false));
    assert_eq!(plan["requires_leader"], json!(true));
    assert!(plan["leader_gate_reasons"]
        .as_array()
        .expect("leader gate reasons")
        .contains(&json!("missing_acceptance_criteria")));

    let high_risk = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 381,
        title: "Rotate vault token handling".to_string(),
        body: Some("## Acceptance criteria\n- Secrets stay redacted.".to_string()),
        labels: vec!["security".to_string()],
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/381".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let plan = crate::task_lifecycle::build_issue_automation_plan(&high_risk, None);
    assert_eq!(plan["dispatch_allowed"], json!(false));
    assert!(plan["high_risk_reasons"]
        .as_array()
        .expect("high risk reasons")
        .contains(&json!("touches_security")));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_dispatch_requires_leader_confirmation_for_blocked_issue_flow() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260614T000001Z_blocked_dispatch_gate";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 380,
        title: "Automate issue dispatch".to_string(),
        body: Some("Do the automation.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/380".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Automate issue dispatch",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");

    let mut params = task_params("dispatch");
    params.flow_id = Some(flow_id.to_string());
    params.issue_ref = Some("kckylechen1/tachi#380".to_string());
    params.task = Some("Automate issue dispatch".to_string());
    params.agent = Some("codex".to_string());
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("dispatch should fail before spawning an agent");
    assert!(
        err.contains("dispatch requires leader confirmation"),
        "unexpected error: {err}"
    );
    assert!(
        err.contains("missing_acceptance_criteria"),
        "unexpected error: {err}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_pr_handoff_writes_pr_body_with_verification_and_gaps() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260614T000002Z_pr_handoff";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 380,
        title: "Automate issue dispatch".to_string(),
        body: Some("## Acceptance criteria\n- PR handoff contains required evidence.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/380".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Automate issue dispatch",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "overall": "passed",
            "items": [
                { "command": "cargo test -p memory-server tachi_task_pr_handoff", "status": "passed" }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");

    let mut params = task_params("pr_handoff");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("pr_handoff should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("pr_handoff JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["safe_to_open"], json!(true));
    let body = parsed["pr_body"].as_str().expect("pr body");
    assert!(body.contains("Linked issue: kckylechen1/tachi#380"));
    assert!(body.contains("Overall: `passed`"));
    assert!(body.contains("Known Gaps / Review Gates"));
    assert!(body.contains("None recorded by Tachi automation gate"));
    let handoff_path = parsed["pr_handoff_path"].as_str().expect("handoff path");
    assert!(handoff_path.ends_with("pr_handoff.md"), "{handoff_path}");
    assert!(run_dir.join("pr_handoff.md").exists());
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_release_note_writes_flow_artifact_with_refs() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000002Z_release_note_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some(
            "## Acceptance criteria\n- Release note includes issue, PR, and verification."
                .to_string(),
        ),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()],
        spec_paths: vec!["docs/engineering/specs/dispatch-policy.md".to_string()],
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");
    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 230,
        title: "Bind GitHub lifecycle into task flows".to_string(),
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: Some("feat/task-intake-link-pr".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "overall": "passed",
            "items": [
                { "command": "cargo test -p memory-server tachi_task_release_note", "status": "passed" },
                { "kind": "gitleaks", "status": "passed" }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");

    let mut params = task_params("release_note");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("release note should be generated");
    let parsed: Value = serde_json::from_str(&raw).expect("release_note response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("release_note"));
    assert_eq!(parsed["flow_id"], json!(flow_id));
    assert_eq!(parsed["issue_ref"], json!("kckylechen1/tachi#194"));
    assert_eq!(parsed["pr_ref"], json!("kckylechen1/tachi#230"));
    assert_eq!(parsed["inputs"]["verification_present"], json!(true));
    let note_path = parsed["release_note_path"]
        .as_str()
        .expect("release note path");
    assert!(note_path.ends_with("release_note.md"), "{note_path}");
    assert!(run_dir.join("release_note.md").exists());
    let note = std::fs::read_to_string(run_dir.join("release_note.md")).expect("release note");
    for expected in [
        "# Release Note",
        "Issue: `kckylechen1/tachi#194`",
        "PR: `kckylechen1/tachi#230`",
        "Merge state: `merged`",
        "spec: `docs/engineering/specs/dispatch-policy.md`",
        "doc: `docs/engineering/architecture/subagent-eval-system.md`",
        "Overall: `passed`",
        "`passed` cargo test -p memory-server tachi_task_release_note",
    ] {
        assert!(note.contains(expected), "missing {expected}: {note}");
    }
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert_eq!(status["state"], json!("release_note_generated"));
    assert!(status["release_note_path"]
        .as_str()
        .is_some_and(|path| path.ends_with("release_note.md")));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_release_note_rejects_mismatched_pr_ref_for_cached_flow_pr() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000003Z_release_note_mismatch";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some("## Acceptance criteria\n- Mismatched PR refs are rejected.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");
    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 230,
        title: "Bind GitHub lifecycle into task flows".to_string(),
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: None,
        base_ref: None,
        review_decision: None,
        mergeable: None,
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");

    let mut params = task_params("release_note");
    params.flow_id = Some(flow_id.to_string());
    params.pr_ref = Some("kckylechen1/tachi#999".to_string());
    let err = server
        .tachi_task(Parameters(params))
        .await
        .expect_err("mismatched pr_ref should be rejected");
    assert!(
        err.contains("release_note pr_ref mismatch"),
        "unexpected error: {err}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_release_note_skips_empty_optional_github_fields() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000004Z_release_note_empty_fields";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some(
            "## Acceptance criteria\n- Empty optional GitHub fields are skipped.".to_string(),
        ),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");
    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 230,
        title: "Bind GitHub lifecycle into task flows".to_string(),
        state: Some("MERGED".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: None,
        base_ref: None,
        review_decision: Some(String::new()),
        mergeable: Some(String::new()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");

    let mut params = task_params("release_note");
    params.format = Some("json".to_string());
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("release note should be generated");
    let parsed: Value = serde_json::from_str(&raw).expect("release_note response JSON");
    let note = parsed["release_note"].as_str().expect("release note text");
    assert!(!note.contains("Review: ``"), "{note}");
    assert!(!note.contains("Mergeable: ``"), "{note}");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_ux_matrix_writes_feature_workflow_artifact() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let temp_runs = tempfile::tempdir().expect("temp run root");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", temp_runs.path());
    let server = make_server();
    let flow_id = "flow_20260608T000005Z_ux_matrix_test";
    let issue = crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 194,
        title: "Policy-learning dispatch profiles".to_string(),
        body: Some("## Acceptance criteria\n- UX matrix includes workflow gates.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/issues/194".to_string(),
        doc_paths: vec!["docs/engineering/architecture/subagent-eval-system.md".to_string()],
        spec_paths: vec!["docs/engineering/specs/dispatch-policy.md".to_string()],
    };
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(&issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Policy-learning dispatch profiles",
        &issue,
        &automation_plan,
    )
    .expect("write intake artifacts");
    let pr = crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number: 230,
        title: "Bind GitHub lifecycle into task flows".to_string(),
        state: Some("OPEN".to_string()),
        url: "https://github.com/kckylechen1/tachi/pull/230".to_string(),
        head_ref: Some("feat/task-intake-link-pr".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    };
    crate::task_lifecycle::write_link_pr_artifacts(flow_id, &pr, None)
        .expect("write link_pr artifacts");
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
    crate::shell_ops::merge_github_status(
        &run_dir,
        json!({
            "merge_state": "ready",
            "policy": "standard",
            "requested_mode": "preview",
            "will_merge": false,
        }),
    )
    .expect("merge github status");
    let mut status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    status["dispatch_ids"] = json!(["dispatch-ux-matrix"]);
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&status).expect("status json"),
    )
    .expect("write status");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "overall": "passed",
            "items": [
                { "id": "gitleaks", "kind": "gitleaks", "status": "passed", "required": true }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");

    let mut release_params = task_params("release_note");
    release_params.flow_id = Some(flow_id.to_string());
    server
        .tachi_task(Parameters(release_params))
        .await
        .expect("release note should be generated");

    let mut params = task_params("ux_matrix");
    params.flow_id = Some(flow_id.to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should succeed");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("ux_matrix"));
    assert_eq!(parsed["overall"], json!("ready_for_close_loop"));
    let matrix = parsed["matrix"].as_array().expect("matrix array");
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("dispatch") && step["status"] == json!("passed") }));
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("verification") && step["status"] == json!("passed") }));
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("pr_status") && step["status"] == json!("passed") }));
    assert!(matrix
        .iter()
        .any(|step| { step["id"] == json!("release_note") && step["status"] == json!("passed") }));
    let ux_path = parsed["ux_matrix_path"]
        .as_str()
        .expect("ux matrix artifact path");
    assert!(ux_path.ends_with("ux_matrix.json"), "{ux_path}");
    assert!(run_dir.join("ux_matrix.json").exists());
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status"),
    )
    .expect("status json");
    assert!(status["artifacts"]["ux_matrix"]
        .as_str()
        .is_some_and(|path| path.ends_with("ux_matrix.json")));
}

#[tokio::test]
async fn tachi_task_ux_matrix_without_flow_is_read_only_starting_checklist() {
    let server = make_server();
    let mut params = task_params("ux_matrix");
    params.task = Some("Review a new Tachi feature request".to_string());
    params.issue_ref = Some("kckylechen1/tachi#194".to_string());
    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should work before intake flow exists");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["action"], json!("ux_matrix"));
    assert_eq!(parsed["flow_id"], Value::Null);
    assert_eq!(parsed["ux_matrix_path"], Value::Null);
    assert_eq!(parsed["overall"], json!("needs_action"));
    assert!(parsed["matrix"].as_array().is_some_and(|matrix| matrix
        .iter()
        .any(|step| step["id"] == json!("intake") && step["status"] == json!("ready"))));
    assert!(parsed["matrix"].as_array().is_some_and(|matrix| matrix
        .iter()
        .any(|step| step["id"] == json!("canonical_docs") && step["status"] == json!("pending"))));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn tachi_task_ux_matrix_creates_new_flow_directory() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_home = tempfile::tempdir().expect("temp tachi home");
    let _home = EnvVarGuard::set_path("TACHI_HOME", temp_home.path());
    let server = make_server();
    let flow_id = "flow_20260609T000007Z_ux_matrix_new_flow";
    let mut params = task_params("ux_matrix");
    params.flow_id = Some(flow_id.to_string());
    params.task = Some("Start a new UX matrix before intake".to_string());

    let raw = server
        .tachi_task(Parameters(params))
        .await
        .expect("ux_matrix should create a new flow dir");
    let parsed: Value = serde_json::from_str(&raw).expect("ux_matrix response JSON");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["flow_id"], json!(flow_id));
    let path = parsed["ux_matrix_path"].as_str().expect("ux matrix path");
    assert!(path.ends_with("ux_matrix.json"), "{path}");
    assert!(std::path::Path::new(path).exists());
}
