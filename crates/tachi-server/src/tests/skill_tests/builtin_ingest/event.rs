use super::*;
use crate::server_state::MemoryServer;
use crate::tool_params::{IngestEventParams, Message};
use axum::{
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::Value;
use std::sync::Arc;
use tachi_llm::{
    llm::{ChatLaneConfig, ProviderRuntimeConfig},
    LlmClient, ProviderSecret, RerankConfig, RerankProviderKind,
};

const FACT_RESPONSE: &str = r#"[{"text":"The ingest durability test records one stable fact after reopening the database.","topic":"ingest durability","keywords":["ingest","durability"],"entities":["Tachi"],"scope":"general","importance":0.8}]"#;

struct MockExtractProvider {
    llm: LlmClient,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockExtractProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockExtractProvider {
    async fn start() -> Self {
        let app = Router::new().route("/chat/completions", post(mock_extract_response));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock extract provider");
        let port = listener
            .local_addr()
            .expect("mock extract provider address")
            .port();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock extract provider");
        });

        let unused_lane = || ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "unused".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        };
        let llm = LlmClient::new_with_config(
            ProviderRuntimeConfig {
                extract: ChatLaneConfig {
                    base_url: format!("http://127.0.0.1:{port}/chat/completions"),
                    model: "mock-ingest-extract".to_string(),
                    api_key_envs: vec!["INGEST_TEST_EXTRACT_API_KEY"],
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
        .expect("initialize mock LLM client");
        assert!(llm.set_provider_secret_pool(
            "INGEST_TEST_EXTRACT_API_KEY",
            vec![ProviderSecret {
                key_id: "ingest-test-key".to_string(),
                value: "test-key".to_string(),
            }],
        ));

        Self { llm, task }
    }
}

async fn mock_extract_response() -> Response {
    Json(json!({
        "choices": [{
            "message": {"role": "assistant", "content": FACT_RESPONSE},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        "model": "mock-ingest-extract"
    }))
    .into_response()
}

fn event_params() -> IngestEventParams {
    IngestEventParams {
        conversation_id: "durability-conversation".to_string(),
        turn_id: "durability-turn".to_string(),
        event_type: None,
        content: None,
        messages: vec![Message {
            role: "user".to_string(),
            content: "Persist this durability fixture as one stable fact.".to_string(),
        }],
        path_prefix: None,
        importance: None,
        scope: "global".to_string(),
        project: None,
        domain: None,
        metadata: None,
    }
}

fn test_server_at(path: std::path::PathBuf, llm: &LlmClient) -> MemoryServer {
    // make_server initializes the process-wide deterministic test environment.
    let bootstrap = make_server();
    drop(bootstrap);

    let mut server = MemoryServer::new(path, None).expect("open durable ingest test database");
    server.llm = Arc::new(llm.clone());
    server
}

#[tokio::test]
async fn abandoned_ingest_claim_reopens_retries_and_persists_once() {
    let provider = MockExtractProvider::start().await;
    let temp = tempfile::tempdir().expect("temp durable ingest database");
    let db_path = temp.path().join("memory.db");
    let params = event_params();
    let event_hash =
        crate::utils::stable_hash(&format!("{}:{}", params.conversation_id, params.turn_id));

    let server = test_server_at(db_path.clone(), &provider.llm);
    server
        .with_global_store(|store| {
            store
                .try_claim_event(
                    &event_hash,
                    &format!("{}:{}", params.conversation_id, params.turn_id),
                    "ingest",
                )
                .map_err(|error| format!("seed abandoned claim: {error}"))?;
            store
                .connection()
                .execute(
                    "UPDATE processed_events SET created_at = ?1 WHERE event_hash = ?2",
                    rusqlite::params![
                        (Utc::now() - ChronoDuration::minutes(10)).to_rfc3339(),
                        event_hash,
                    ],
                )
                .map_err(|error| format!("age abandoned claim: {error}"))?;
            Ok(())
        })
        .expect("seed a stale claim before simulated process abort");
    drop(server);

    let reopened = test_server_at(db_path, &provider.llm);
    let completed = crate::pipeline_ops::handle_ingest_event(&reopened, params.clone())
        .await
        .expect("stale claim must retry synchronously after reopen");
    let response: Value = serde_json::from_str(&completed).expect("completed JSON");
    assert_eq!(response["status"], "completed");
    let response_object = response.as_object().expect("response object");
    assert_eq!(
        response_object.len(),
        2,
        "synchronous completion must preserve the status/hash response shape"
    );
    assert!(response_object.contains_key("hash"));

    let duplicate = crate::pipeline_ops::handle_ingest_event(&reopened, params)
        .await
        .expect("completed event replay");
    let duplicate: Value = serde_json::from_str(&duplicate).expect("duplicate JSON");
    assert_eq!(duplicate["status"], "skipped");

    let (fact_count, success_audits) = reopened
        .with_global_store_read(|store| {
            let facts = store
                .list_by_path("/general/ingest_durability", 10, false)
                .map_err(|error| format!("list persisted facts: {error}"))?;
            let audits: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE server_id = 'ingest' AND tool_name = 'ingest_event' AND success = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| format!("count success audits: {error}"))?;
            Ok((facts.len(), audits))
        })
        .expect("read durable ingest state");
    assert_eq!(fact_count, 1, "retries must not duplicate the fact row");
    assert_eq!(
        success_audits, 1,
        "retries must not duplicate success audit"
    );
}

#[tokio::test]
async fn conversation_row_write_failure_is_loud_and_never_records_success() {
    let provider = MockExtractProvider::start().await;
    let temp = tempfile::tempdir().expect("temp row failure database");
    let server = test_server_at(temp.path().join("memory.db"), &provider.llm);
    crate::test_support::with_unrestricted_fixture_connection(
        &server.global_db_path_buf(),
        |connection| connection.execute_batch("DROP TABLE memories"),
    )
    .expect("force conversation row write failure");

    let error = crate::pipeline_ops::handle_ingest_event(&server, event_params())
        .await
        .expect_err("a failed conversation row write must reach the caller");
    assert!(
        error.contains("write"),
        "unexpected ingestion error: {error}"
    );

    let (success_audits, failure_audits) = server
        .with_global_store_read(|store| {
            let success: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE server_id = 'ingest' AND tool_name = 'ingest_event' AND success = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| format!("count success audits: {error}"))?;
            let failure: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE server_id = 'ingest' AND tool_name = 'ingest_event' AND success = 0",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| format!("count failure audits: {error}"))?;
            Ok((success, failure))
        })
        .expect("read ingest audit state");
    assert_eq!(success_audits, 0, "failed row writes cannot report success");
    assert_eq!(
        failure_audits, 1,
        "failed row writes require a durable failure audit"
    );
}
