use super::*;
use axum::{routing::post, Json, Router};
use tachi_llm::{
    llm::{ChatLaneConfig, ProviderRuntimeConfig},
    LlmClient, ProviderSecret, RerankConfig, RerankProviderKind,
};

struct MockTrajectoryProvider {
    llm: LlmClient,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockTrajectoryProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockTrajectoryProvider {
    async fn start(output: &str, finish_reason: &str) -> Self {
        let output = output.to_string();
        let finish_reason = finish_reason.to_string();
        let app = Router::new().route(
            "/chat/completions",
            post(move || {
                let output = output.clone();
                let finish_reason = finish_reason.clone();
                async move {
                    Json(json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": output},
                            "finish_reason": finish_reason,
                        }],
                        "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7},
                        "model": "mock-trajectory-distiller",
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock trajectory provider");
        let port = listener
            .local_addr()
            .expect("trajectory provider address")
            .port();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock trajectory provider");
        });
        let lane = || ChatLaneConfig {
            base_url: format!("http://127.0.0.1:{port}/chat/completions"),
            model: "mock-trajectory-distiller".to_string(),
            api_key_envs: vec!["TRAJECTORY_TEST_API_KEY"],
        };
        let llm = LlmClient::new_with_config(
            ProviderRuntimeConfig {
                extract: lane(),
                summary: lane(),
                reasoning: lane(),
                distill: lane(),
                rerank: RerankConfig {
                    provider: RerankProviderKind::Voyage,
                    local_endpoint: None,
                },
            },
            None,
        )
        .expect("initialize mock trajectory LLM");
        assert!(llm.set_provider_secret_pool(
            "TRAJECTORY_TEST_API_KEY",
            vec![ProviderSecret {
                key_id: "trajectory-test-key".to_string(),
                value: "test-key".to_string(),
            }],
        ));
        Self { llm, task }
    }
}

#[tokio::test]
async fn distill_trajectory_creates_permanent_snapshot_and_skill() {
    let mut server = make_server();
    const DISTILLED_MARKDOWN: &str =
        "# 适用场景\n- recurring task\n\n# 核心步骤\n- step\n\n# 踩坑记录\n- none\n\n# 验证标准\n- tests pass\n\n# 适用域标签\n- coding";
    let provider = MockTrajectoryProvider::start(DISTILLED_MARKDOWN, "stop").await;
    server.replace_llm(provider.llm.clone());

    let response = server
        .distill_trajectory(Parameters(DistillTrajectoryParams {
            task_description: "Fix a flaky test".to_string(),
            execution_trace: vec![json!({"step":"reproduced"}), json!({"step":"fixed"})],
            final_outcome: json!({"success": true, "score": 0.92}),
            agent_id: "codex".to_string(),
            skill_path: "/skills/coding/flaky-test-fix".to_string(),
            skill_id: Some("skill:flaky-test-fix".to_string()),
            importance: Some(0.9),
            domain: Some("coding".to_string()),
            project: None,
            scope: "global".to_string(),
        }))
        .await
        .expect("distill_trajectory should succeed");
    let response_json: Value = serde_json::from_str(&response).expect("distill response json");

    let snapshot_id = response_json["snapshot_id"]
        .as_str()
        .expect("snapshot id")
        .to_string();
    let snapshot = server
        .with_global_store_read(|store| store.get(&snapshot_id).map_err(|e| e.to_string()))
        .expect("load distilled snapshot")
        .expect("snapshot should exist");
    assert_eq!(snapshot.retention_policy.as_deref(), Some("permanent"));
    assert_eq!(snapshot.text, DISTILLED_MARKDOWN);
    let snapshot_receipt = snapshot
        .metadata
        .pointer("/provenance/model_invocation")
        .expect("trajectory snapshot invocation receipt");
    assert_eq!(snapshot_receipt["schema"], "model-invocation-v1");
    assert_eq!(snapshot_receipt["lane"], "extract");
    assert!(!snapshot
        .text
        .contains(crate::hub_ops::SIMULATED_SKILL_OUTPUT_MARKER));
    assert!(
        serde_json::from_str::<Value>(&snapshot.text).is_err(),
        "distilled snapshot should remain raw markdown, not a JSON envelope"
    );

    let distilled_cap = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:flaky-test-fix")
                .map_err(|e| e.to_string())
        })
        .expect("load distilled cap")
        .expect("distilled skill should exist");
    let distilled_def: Value =
        serde_json::from_str(&distilled_cap.definition).expect("distilled definition json");
    assert_eq!(distilled_def["retention_policy"], "permanent");
    assert_eq!(distilled_def["content"], json!(DISTILLED_MARKDOWN));
    assert_eq!(
        &distilled_def["provenance"]["model_invocation"], snapshot_receipt,
        "snapshot and capability must identify the same actual invocation"
    );
    assert!(!distilled_def["content"]
        .as_str()
        .expect("distilled content")
        .contains(crate::hub_ops::SIMULATED_SKILL_OUTPUT_MARKER));
    assert_eq!(distilled_cap.avg_rating, 0.5);
}

#[tokio::test]
async fn distill_trajectory_truncated_output_writes_no_snapshot_or_capability() {
    let mut server = make_server();
    let provider = MockTrajectoryProvider::start("# apparently valid skill", "length").await;
    server.replace_llm(provider.llm.clone());

    let error = server
        .distill_trajectory(Parameters(DistillTrajectoryParams {
            task_description: "Reject truncated trajectory output".to_string(),
            execution_trace: vec![json!({"step":"model truncated"})],
            final_outcome: json!({"success": false}),
            agent_id: "codex".to_string(),
            skill_path: "/skills/coding/truncated-trajectory".to_string(),
            skill_id: Some("skill:truncated-trajectory".to_string()),
            importance: Some(0.9),
            domain: Some("coding".to_string()),
            project: None,
            scope: "global".to_string(),
        }))
        .await
        .expect_err("truncated trajectory output must be rejected");
    assert!(
        error.contains(tachi_llm::LLM_OUTPUT_TRUNCATED),
        "unexpected error: {error}"
    );

    let snapshots = server
        .with_global_store_read(|store| {
            store
                .list_by_path("/skills/coding/truncated-trajectory/distilled", 8, false)
                .map_err(|error| error.to_string())
        })
        .expect("list truncated trajectory snapshots");
    assert!(snapshots.is_empty(), "truncated output wrote a snapshot");
    let capability = server
        .with_global_store_read(|store| {
            store
                .hub_get("skill:truncated-trajectory")
                .map_err(|error| error.to_string())
        })
        .expect("query truncated trajectory capability");
    assert!(
        capability.is_none(),
        "truncated output registered a capability"
    );
}
