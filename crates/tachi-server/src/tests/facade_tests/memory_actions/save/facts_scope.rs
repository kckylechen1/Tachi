use super::*;
use tachi_llm::{
    llm::{ChatLaneConfig, ProviderRuntimeConfig},
    LlmClient, ProviderSecret, RerankConfig, RerankProviderKind,
};

struct ExtractProvider {
    llm: LlmClient,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ExtractProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ExtractProvider {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let router = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "choices": [{"message": {"role": "assistant", "content": json!([{
                        "text": "The shared retention policy keeps durable incident lessons for future workspaces.",
                        "topic": "workspace storage", "importance": 0.8
                    }]).to_string()}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                    "model": "scope-routing-extract"
                }))
            }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let unused_lane = || ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        };
        let llm = LlmClient::new_with_config(
            ProviderRuntimeConfig {
                extract: ChatLaneConfig {
                    base_url: endpoint,
                    model: "scope-routing-extract".to_string(),
                    api_key_envs: vec!["SCOPE_TEST_EXTRACT_API_KEY"],
                },
                summary: unused_lane(),
                reasoning: unused_lane(),
                distill: unused_lane(),
                rerank: RerankConfig {
                    provider: RerankProviderKind::Voyage,
                    local_endpoint: None,
                },
            },
            None,
        )
        .unwrap();
        assert!(llm.set_provider_secret_pool(
            "SCOPE_TEST_EXTRACT_API_KEY",
            vec![ProviderSecret {
                key_id: "fixture".to_string(),
                value: "test-key".to_string()
            }],
        ));
        Self { llm, task }
    }
}

async fn save_facts(server: &crate::MemoryServer, mut payload: Value) -> Value {
    payload["text"] = json!("Extract the shared retention convention into a durable fact.");
    payload["format"] = json!("json");
    let raw = if payload.get("action").is_some() {
        crate::facade_memory_ops::handle_tachi_memory(
            server,
            serde_json::from_value(payload).unwrap(),
        )
        .await
    } else {
        crate::facade_save_ops::handle_tachi_save(server, serde_json::from_value(payload).unwrap())
            .await
    }
    .unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn assert_fact_destination(server: &crate::MemoryServer, response: &Value, expected: &str) {
    assert_eq!(response["facts_saved"], json!(1), "{response}");
    assert_eq!(response["db"], json!(expected), "{response}");
    let id = response["facts"][0]["id"].as_str().unwrap();
    let global = server
        .with_global_store_read(|store| store.get(id).map_err(|e| e.to_string()))
        .unwrap();
    let project = server
        .with_project_store_read(|store| store.get(id).map_err(|e| e.to_string()))
        .unwrap();
    assert_eq!(global.is_some(), expected == "global");
    assert_eq!(project.is_some(), expected == "project");
}

#[tokio::test]
async fn explicit_global_fact_facades_never_use_the_daemons_static_project() {
    let provider = ExtractProvider::start().await;
    for payload in [
        json!({"kind": "facts", "storage_scope": "global"}),
        json!({"kind": "extract_facts", "storage_scope": "GLOBAL"}),
        json!({"action": "save", "kind": "facts", "storage_scope": "global"}),
        json!({"action": "save", "kind": "extract_facts", "storage_scope": "global"}),
        json!({"action": "extract_facts", "storage_scope": "global"}),
        json!({"action": "extract_facts", "scope": "GLOBAL"}),
    ] {
        let (mut server, _) = make_server_with_project_fixture("static-project");
        server.replace_llm(provider.llm.clone());
        let response = save_facts(&server, payload).await;
        assert_fact_destination(&server, &response, "global");
        assert!(response["warning"].is_null());
    }
}

#[tokio::test]
async fn fact_facades_keep_default_project_destination() {
    let provider = ExtractProvider::start().await;
    for payload in [
        json!({"kind": "facts"}),
        json!({"action": "save", "kind": "facts"}),
        json!({"action": "extract_facts"}),
    ] {
        let (mut server, _) = make_server_with_project_fixture("current-project");
        server.replace_llm(provider.llm.clone());
        let response = save_facts(&server, payload).await;
        assert_fact_destination(&server, &response, "project");
    }
}

#[tokio::test]
async fn extract_facts_refuses_an_unbound_implicit_project_before_llm() {
    let server = make_server();
    let params = serde_json::from_value(json!({
        "action": "extract_facts", "text": "An unbound fact must not silently become global memory."
    }))
    .unwrap();
    let error = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .unwrap_err();
    assert!(error.contains("No project DB is bound"), "{error}");
}
