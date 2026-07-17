use crate::server_state::MemoryServer;
use crate::tool_params::{TachiDispatchParams, TachiMemoryParams, TachiTaskParams};
use chrono::Utc;
use serde_json::{json, Value};

mod acp_transport;
mod board_first;
mod completion_eval;
mod prompt_credentials_board;
mod recommend_policy;
mod signature_evidence;
mod workflow_artifacts;

fn dispatch_params(agent: Option<&str>, task: &str) -> TachiDispatchParams {
    TachiDispatchParams {
        agent: agent.map(str::to_string),
        profile: None,
        task: task.to_string(),
        execution_level: None,
        cwd: None,
        env_id: None,
        unmanaged_cwd: None,
        skills: Vec::new(),
        context_query: None,
        model: None,
        timeout_secs: 5,
        permission_profile: None,
        allowed_tools: Vec::new(),
        completion_predicate: None,
        max_turns: None,
        sandbox: None,
        inject_tachi_mcp: None,
        inject_hub_mcps: None,
        command: Vec::new(),
        harness_transport: None,
        harness_server_url: None,
        project: None,
        stage: None,
        credential_profiles: Vec::new(),
        issue_ref: None,
        pr_ref: None,
        flow_id: None,
        tool_profile: None,
        auto_capability_bundle: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        verbose: None,
    }
}

fn task_params(action: &str) -> TachiTaskParams {
    TachiTaskParams {
        action: action
            .parse()
            .unwrap_or_else(|e| panic!("valid tachi_task action '{action}': {e}")),
        format: Some("json".to_string()),
        task: None,
        execution_level: None,
        agent_id: None,
        domain: None,
        path_prefix: None,
        top_k: None,
        doc_paths: Vec::new(),
        related_issues: Vec::new(),
        spec_paths: Vec::new(),
        include_global: false,
        compact: None,
        agent: None,
        outcome: None,
        task_id: None,
        task_type: None,
        duration_ms: None,
        skills_used: Vec::new(),
        cost_tokens: None,
        cost_usd: None,
        quality_score: None,
        notes: None,
        trajectory: None,
        diff: None,
        subagents: Vec::new(),
        feedback_rules_applied: Vec::new(),
        signatures: Vec::new(),
        rulings: Vec::new(),
        adjudication: None,
        evidence_refs: Vec::new(),
        tests_run: Vec::new(),
        diff_present: None,
        scope: None,
        cwd: None,
        env_id: None,
        unmanaged_cwd: None,
        skills: Vec::new(),
        context_query: None,
        model: None,
        timeout_secs: None,
        permission_profile: None,
        allowed_tools: Vec::new(),
        completion_predicate: None,
        max_turns: None,
        sandbox: None,
        inject_tachi_mcp: None,
        inject_hub_mcps: None,
        command: Vec::new(),
        harness_transport: None,
        harness_server_url: None,
        project: None,
        project_explicit: false,
        stage: None,
        profile: None,
        credential_profiles: Vec::new(),
        repo: None,
        number: None,
        issue_ref: None,
        pr_ref: None,
        flow_id: None,
        dispatch_id: None,
        outcome_id: None,
        include_result: false,
        risk: None,
        tool_profile: None,
        auto_capability_bundle: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        verbose: None,
        state_filter: None,
        limit: None,
        proposal_id: None,
        review_status: None,
        worktree: None,
        branch: None,
        strategy: None,
        merge_policy: None,
        allow_umbrella_close: false,
        delete_worktree: true,
        confirm: false,
        wiki_title: None,
        wiki_text: None,
        wiki_path: None,
        wiki_topic: None,
        wiki_summary: None,
        wiki_category: None,
        wiki_keywords: Vec::new(),
        wiki_entities: Vec::new(),
        wiki_importance: None,
        wiki_scope: None,
        wiki_domain: None,
        force: false,
    }
}

fn memory_params(action: &str) -> TachiMemoryParams {
    TachiMemoryParams {
        action: action.to_string(),
        format: Some("json".to_string()),
        query: None,
        scope: None,
        top_k: 6,
        path_prefix: None,
        file_context: None,
        error_context: None,
        category: None,
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        as_of: None,
        synthesize: false,
        model: None,
        agent_role: None,
        text: None,
        title: None,
        summary: None,
        topic: None,
        keywords: Vec::new(),
        entities: Vec::new(),
        importance: None,
        retention_policy: None,
        kind: None,
        path: None,
        id: None,
        force: false,
        source: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
        files: Vec::new(),
        flow_id: None,
        event: None,
        state: None,
        project: None,
        project_explicit: false,
        domain: None,
        compact: false,
        proposal_id: None,
        review_status: None,
        notes: None,
        confirm: false,
        state_filter: None,
        content: None,
        ingest_type: "source".to_string(),
        source_url: None,
        auto_chunk: true,
        auto_summarize: true,
        auto_link: true,
        chunk_size_chars: 1200,
        chunk_overlap_chars: 120,
        conversation_id: None,
        turn_id: None,
        event_type: None,
        messages: Vec::new(),
        issue_ref: None,
        branch: None,
        declared_file_scope: Vec::new(),
        claim_id: None,
        dispatch_id: None,
        release_reason: None,
        to: None,
        ttl_days: None,
        include_read: false,
        agent_id: None,
    }
}

async fn save_grep_evidence_feedback_rule(server: &MemoryServer) -> String {
    let mut params = memory_params("save");
    params.kind = Some("feedback_rule".to_string());
    params.title = Some("Subagent audit prompts require explicit search evidence".to_string());
    params.topic = Some("Subagent audit prompts require explicit search evidence".to_string());
    params.path = Some("/feedback/subagent/code-audit/grep-evidence".to_string());
    params.category = Some("prompt_rule".to_string());
    params.text = Some(
        "When dispatching subagents for code audits or dead-code scans, give exact search patterns and require grep/ripgrep evidence in reports."
            .to_string(),
    );
    params.keywords = vec![
        "subagent".to_string(),
        "code_audit".to_string(),
        "dead_code".to_string(),
        "grep".to_string(),
        "false_positive".to_string(),
    ];
    params.force = true;
    params.metadata = Some(json!({
        "applies_to": {
            "task_type": ["review_request", "code_audit", "dead_code_scan"],
            "profiles": ["codex_55_review", "codex_53_fast", "deepseek_explore"],
            "stage": ["review", "explore"]
        },
        "trigger_keywords": ["unused", "no callers", "dead code", "grep", "search repo"],
        "prompt_patch": "For any 'unused', 'no callers', or dead-code claim: search both identifier form and call form; include exact grep/ripgrep commands; list paths searched; report uncertainty if the search scope is incomplete.",
        "evidence_contract": [
            "grep_commands",
            "paths_searched",
            "matching_files_or_none",
            "confidence",
            "uncertainty_notes"
        ]
    }));

    let raw = crate::facade_memory_ops::handle_tachi_memory(server, params)
        .await
        .expect("feedback rule save should succeed");
    serde_json::from_str::<Value>(&raw)
        .expect("save JSON")
        .get("id")
        .and_then(Value::as_str)
        .expect("saved rule id")
        .to_string()
}

use crate::test_support::EnvRestore;

/// Thin wrapper around [`EnvRestore`] so the ~60 call sites across the
/// dispatch-tests sub-modules (which import this as `n`) keep their
/// `n::set_path` / `n::set_value` syntax without each file needing its
/// own import. #1096 leaf-2a.
struct EnvVarGuard(#[allow(dead_code)] EnvRestore); // held for Drop only

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &std::path::Path) -> Self {
        Self(EnvRestore::set_path(key, value))
    }

    fn set_value(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        Self(EnvRestore::set_os(key, value.as_ref()))
    }
}

// Same worst-case ceiling as before (240 * 25ms = 1200 * 5ms = 6s), but a
// finer poll interval so the (overwhelmingly common) fast-resolving case
// returns ~5x sooner instead of always paying at least one 25ms tick.
// Widely shared (16 call sites across 10 files, issue #682 busy-wait sweep):
// shrinking typical latency here has outsized effect on suite wall clock
// without weakening the timeout safety margin (a genuine hang still trips
// at the same 6s ceiling).
const DISPATCH_TEST_WAIT_ATTEMPTS: usize = 1200;
const DISPATCH_TEST_WAIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

async fn wait_for_dispatch_result(run_dir: &std::path::Path) -> String {
    let result_path = run_dir.join("result.md");
    for _ in 0..DISPATCH_TEST_WAIT_ATTEMPTS {
        if let Ok(raw) = std::fs::read_to_string(&result_path) {
            return raw;
        }
        tokio::time::sleep(DISPATCH_TEST_WAIT_INTERVAL).await;
    }
    std::fs::read_to_string(&result_path).expect("dispatch result.md should be written")
}

async fn wait_for_dispatch_status(run_dir: &std::path::Path) -> Value {
    let status_path = run_dir.join("status.json");
    for _ in 0..DISPATCH_TEST_WAIT_ATTEMPTS {
        if let Ok(raw) = std::fs::read_to_string(&status_path) {
            let status: Value = serde_json::from_str(&raw).expect("status JSON");
            if dispatch_status_is_terminal(&status) {
                return status;
            }
        }
        tokio::time::sleep(DISPATCH_TEST_WAIT_INTERVAL).await;
    }
    serde_json::from_str(&std::fs::read_to_string(&status_path).expect("status.json"))
        .expect("status JSON")
}

fn dispatch_status_is_terminal(status: &Value) -> bool {
    if !status["exit_code"].is_null() {
        return true;
    }
    if matches!(
        status.get("state").and_then(Value::as_str),
        Some("TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED")
    ) {
        return true;
    }
    status.get("result_written").and_then(Value::as_bool) == Some(true)
}

fn write_acpx_control_fixture(run_dir: &std::path::Path, dispatch_id: &str) {
    std::fs::create_dir_all(run_dir).expect("create acpx control run dir");
    let argv_base = |action: &str| {
        json!([
            "python3",
            "-m",
            "fake_acpx_control",
            "--cwd",
            "/tmp/project",
            "--format",
            "json",
            "--json-strict",
            "--approve-reads",
            "codex",
            action,
            "-s",
            "raven"
        ])
    };
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_string_pretty(&json!({
            "dispatch_id": dispatch_id,
            "agent": "codex",
            "task": "exercise acpx controls",
            "state": "TASK_STATE_WORKING",
            "updated_at": Utc::now().to_rfc3339(),
            "exit_code": null,
            "result_written": false,
            "harness_transport": "acpx",
            "execution_backend": "acpx",
            "acpx": {
                "agent": "codex",
                "mode": "session",
                "session": "raven",
                "permissions": "approve-reads",
                "controls": {
                    "status": {
                        "supported": true,
                        "argv": argv_base("status")
                    },
                    "cancel": {
                        "supported": true,
                        "argv": argv_base("cancel")
                    }
                }
            }
        }))
        .expect("serialize acpx control status"),
    )
    .expect("write acpx control status");
    std::fs::write(run_dir.join("trajectory.jsonl"), "").expect("trajectory");
}

fn write_fake_acpx_control_module(module_dir: &std::path::Path) {
    std::fs::write(
        module_dir.join("fake_acpx_control.py"),
        r#"import json
import sys

action = "cancel" if "cancel" in sys.argv else "status"
print(json.dumps({
    "action": action,
    "state": "cancelled" if action == "cancel" else "running",
    "argv": sys.argv[1:],
}))
"#,
    )
    .expect("fake acpx control module");
}
