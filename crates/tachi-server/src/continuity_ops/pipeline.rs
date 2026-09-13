use memcore::{AuthorityLevel, EffectScope, ProjectionKind, TachiEventRecord};
use serde_json::{json, Value};

use crate::tool_params::Message;
use crate::MemoryServer;

use super::storage::write_event;
use super::{
    now_rfc3339, parse_continuity_candidate_batch, parse_continuity_outcome_label,
    stable_event_payload_id, ContinuityEventTarget,
};

fn projection_candidate_event_type(projection: ProjectionKind) -> &'static str {
    match projection {
        ProjectionKind::Pattern => "pattern.candidate",
        ProjectionKind::Timeline => "timeline.candidate",
        ProjectionKind::Outcome => "outcome.candidate",
        ProjectionKind::Affect => "affect.candidate",
        ProjectionKind::Bonding => "bonding.candidate",
        ProjectionKind::WorldBook => "world_book.candidate",
        ProjectionKind::ProjectCycle => "project_cycle.candidate",
        ProjectionKind::DomainProfile => "domain_profile.candidate",
        ProjectionKind::EvidenceGate => "evidence_gate.candidate",
    }
}

fn continuity_pipeline_enabled() -> bool {
    std::env::var("TACHI_CONTINUITY_PIPELINE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn maybe_spawn_session_continuity_pipeline(
    server: &MemoryServer,
    target: ContinuityEventTarget,
    operation_key: &str,
    source_ref_id: &str,
    source_revision: &str,
    source_event_id: &str,
    conversation_id: String,
    turn_id: String,
    agent_id: String,
    project: Option<String>,
    messages: Vec<Message>,
) -> Value {
    if !continuity_pipeline_enabled() {
        return json!({
            "status": "disabled",
            "reason": "set TACHI_CONTINUITY_PIPELINE=1 to run distill/reasoning continuity labelers",
        });
    }

    if [source_ref_id, source_revision, source_event_id]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        return json!({"status": "failed", "reason": "source_binding_missing"});
    }

    match target.claim_pipeline_schedule(server, operation_key) {
        Ok(false) => return json!({"status": "already_scheduled", "operation_key": operation_key}),
        Err(error) => {
            return json!({"status": "failed", "reason": "schedule_claim_failed", "error": error})
        }
        Ok(true) => {}
    }

    let source_ref_id = source_ref_id.to_string();
    let source_revision = source_revision.to_string();
    let source_event_id = source_event_id.to_string();
    let server_clone = server.clone();
    let event_count_hint = messages.len();
    tokio::spawn(async move {
        let payload = json!({
            "conversation_id": conversation_id,
            "turn_id": turn_id,
            "agent_id": agent_id,
            "messages": messages,
        });
        let request = match serde_json::to_string_pretty(&payload) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!("[continuity] serialize session payload failed: {error}");
                return;
            }
        };

        match server_clone
            .llm
            .call_distill_llm_with_receipt(
                crate::prompts::CONTINUITY_CANDIDATE_PROMPT,
                &request,
                None,
                0.2,
                1800,
            )
            .await
        {
            Ok(raw)
                if raw.invocation.completion_status()
                    == tachi_llm::CompletionStatusV1::Truncated =>
            {
                tracing::warn!("[continuity] candidate distill rejected: llm_output_truncated");
            }
            Ok(raw) => match parse_continuity_candidate_batch(&raw.value) {
                Ok(batch) => {
                    for candidate in batch.candidates {
                        let event_type = candidate
                            .event_type
                            .clone()
                            .filter(|value| !value.trim().is_empty())
                            .unwrap_or_else(|| {
                                projection_candidate_event_type(candidate.projection).to_string()
                            });
                        let provenance = match crate::provenance::attach_event_model_invocation(
                            json!({
                                "source": "continuity_distill",
                                "lane": "distill",
                            }),
                            &raw.invocation,
                        ) {
                            Ok(provenance) => provenance,
                            Err(error) => {
                                tracing::warn!(
                                    "[continuity] candidate receipt attachment failed: {error}"
                                );
                                continue;
                            }
                        };
                        let event = TachiEventRecord {
                            id: stable_event_payload_id(&[
                                event_type.as_str(),
                                payload["conversation_id"].as_str().unwrap_or_default(),
                                candidate.summary.as_str(),
                                &uuid::Uuid::new_v4().to_string(),
                            ]),
                            source_repo: "tachi".to_string(),
                            adapter: "continuity_distill".to_string(),
                            project: target.project_label(project.as_deref()),
                            domain: "session".to_string(),
                            session_id: payload["conversation_id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            actor: payload["agent_id"].as_str().unwrap_or_default().to_string(),
                            event_type,
                            authority: AuthorityLevel::CollectOnly,
                            effects: vec![EffectScope::None],
                            projection_hints: vec![candidate.projection],
                            payload: json!({
                                "candidate": candidate,
                                "auto_applied": false,
                            }),
                            provenance,
                            created_at: now_rfc3339(),
                        };
                        if let Err(error) = write_event(&server_clone, &target, &event) {
                            tracing::warn!("[continuity] candidate event write failed: {error}");
                        }
                    }
                }
                Err(error) => tracing::warn!("[continuity] candidate parse failed: {error}"),
            },
            Err(error) => tracing::warn!("[continuity] candidate distill failed: {error}"),
        }

        match server_clone
            .llm
            .call_reasoning_llm_with_receipt(
                crate::prompts::SESSION_OUTCOME_LABEL_PROMPT,
                &request,
                None,
                0.0,
                900,
            )
            .await
        {
            Ok(raw) if raw.truncated => {
                tracing::warn!("[continuity] outcome label rejected: llm_output_truncated");
            }
            Ok(raw) => match parse_continuity_outcome_label(&raw.text) {
                Ok(label) => {
                    let provenance = match crate::provenance::attach_event_model_invocation(
                        json!({
                            "source": "continuity_labeler",
                            "source_refs": [{
                                "ref_type": "turn",
                                "ref_id": source_ref_id,
                                "revision": source_revision,
                            }],
                            "source_event_id": source_event_id,
                            "lane": "reasoning",
                            "note": "read-only signal; projectors must calibrate before automatic counter updates",
                        }),
                        &raw.invocation,
                    ) {
                        Ok(provenance) => provenance,
                        Err(error) => {
                            tracing::warn!(
                                "[continuity] outcome receipt attachment failed: {error}"
                            );
                            return;
                        }
                    };
                    let event = TachiEventRecord {
                        id: stable_event_payload_id(&[
                            "session.outcome",
                            payload["conversation_id"].as_str().unwrap_or_default(),
                            payload["turn_id"].as_str().unwrap_or_default(),
                            &uuid::Uuid::new_v4().to_string(),
                        ]),
                        source_repo: "tachi".to_string(),
                        adapter: "continuity_labeler".to_string(),
                        project: target.project_label(project.as_deref()),
                        domain: "session".to_string(),
                        session_id: payload["conversation_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        actor: payload["agent_id"].as_str().unwrap_or_default().to_string(),
                        event_type: "session.outcome".to_string(),
                        authority: AuthorityLevel::ReviewSignalOnly,
                        effects: vec![EffectScope::Scoring],
                        projection_hints: vec![
                            ProjectionKind::Outcome,
                            ProjectionKind::EvidenceGate,
                        ],
                        payload: json!({
                            "outcome": label.outcome.as_str(),
                            "evidence_basis": label.evidence_basis.as_str(),
                            "confidence": label.confidence,
                            "rationale": label.rationale,
                            "evidence_refs": label.evidence_refs,
                            "claims": label.claims,
                            "open_questions": label.open_questions,
                        }),
                        provenance,
                        created_at: now_rfc3339(),
                    };
                    if let Err(error) = write_event(&server_clone, &target, &event) {
                        tracing::warn!("[continuity] outcome event write failed: {error}");
                    }
                }
                Err(error) => tracing::warn!("[continuity] outcome parse failed: {error}"),
            },
            Err(error) => tracing::warn!("[continuity] outcome labeler failed: {error}"),
        }
    });

    json!({
        "status": "scheduled_best_effort",
        "delivery": "at_most_once",
        "operation_key": operation_key,
        "messages": event_count_hint,
        "lanes": ["distill", "reasoning"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tachi_llm::{
        llm::{ChatLaneConfig, ProviderRuntimeConfig},
        LlmClient, ProviderSecret, RerankConfig, RerankProviderKind,
    };

    struct MockContinuityProvider {
        llm: LlmClient,
        calls: Arc<AtomicUsize>,
        responses: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for MockContinuityProvider {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl MockContinuityProvider {
        async fn start(truncated: bool) -> Self {
            let calls = Arc::new(AtomicUsize::new(0));
            let responses = Arc::new(AtomicUsize::new(0));
            let app = Router::new()
                .route("/chat/completions", post(mock_chat_completion))
                .with_state(MockContinuityState {
                    calls: Arc::clone(&calls),
                    responses: Arc::clone(&responses),
                    truncated,
                });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind mock continuity provider");
            let port = listener
                .local_addr()
                .expect("mock continuity provider address")
                .port();
            let task = tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve mock continuity provider");
            });
            let llm = continuity_llm(format!("http://127.0.0.1:{port}/chat/completions"));
            for key in ["CONTINUITY_DISTILL_API_KEY", "CONTINUITY_REASONING_API_KEY"] {
                assert!(llm.set_provider_secret_pool(
                    key,
                    vec![ProviderSecret {
                        key_id: key.to_string(),
                        value: "test-key".to_string(),
                    }],
                ));
            }
            Self {
                llm,
                calls,
                responses,
                task,
            }
        }
    }

    #[derive(Clone)]
    struct MockContinuityState {
        calls: Arc<AtomicUsize>,
        responses: Arc<AtomicUsize>,
        truncated: bool,
    }

    async fn mock_chat_completion(State(state): State<MockContinuityState>) -> impl IntoResponse {
        let call = state.calls.fetch_add(1, Ordering::SeqCst);
        let finish_reason = if state.truncated { "length" } else { "stop" };
        let content = if state.truncated {
            "truncated continuity output".to_string()
        } else if call == 0 {
            r#"{
                "candidates": [{
                    "projection": "pattern",
                    "summary": "Continuity candidate receipt",
                    "text": "Candidate provenance must carry a top-level model_invocation.",
                    "confidence": 0.82,
                    "evidence_refs": ["message:1"]
                }]
            }"#
            .to_string()
        } else {
            r#"{
                "outcome": "success",
                "evidence_basis": "external_evidence",
                "confidence": 0.91,
                "rationale": "Outcome provenance must carry a top-level model_invocation.",
                "evidence_refs": ["message:1"],
                "claims": ["receipt attached"],
                "open_questions": []
            }"#
            .to_string()
        };
        state.responses.fetch_add(1, Ordering::SeqCst);
        Json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": content},
                "finish_reason": finish_reason,
            }],
            "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
            "model": if call == 0 { "continuity-distill-model" } else { "continuity-reasoning-model" },
        }))
    }

    fn continuity_llm(base_url: String) -> LlmClient {
        let lane = |key| ChatLaneConfig {
            base_url: base_url.clone(),
            model: "continuity-mock".to_string(),
            api_key_envs: vec![key],
        };
        LlmClient::new_with_config(
            ProviderRuntimeConfig {
                extract: lane("UNUSED_CONTINUITY_API_KEY"),
                summary: lane("UNUSED_CONTINUITY_API_KEY"),
                reasoning: lane("CONTINUITY_REASONING_API_KEY"),
                distill: lane("CONTINUITY_DISTILL_API_KEY"),
                rerank: RerankConfig {
                    provider: RerankProviderKind::Voyage,
                    local_endpoint: None,
                },
            },
            None,
        )
        .expect("initialize continuity LLM")
    }

    async fn wait_for_calls(provider: &MockContinuityProvider, expected: usize) {
        for _ in 0..50 {
            if provider.calls.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!(
            "mock continuity provider saw {} calls, expected at least {expected}",
            provider.calls.load(Ordering::SeqCst)
        );
    }

    async fn wait_for_responses(provider: &MockContinuityProvider, expected: usize) {
        for _ in 0..50 {
            if provider.responses.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!(
            "mock continuity provider completed {} responses, expected at least {expected}",
            provider.responses.load(Ordering::SeqCst)
        );
    }

    async fn wait_for_event_count_to_quiesce(server: &MemoryServer, session_id: &str) -> usize {
        let mut last = usize::MAX;
        let mut stable_samples = 0;
        for _ in 0..25 {
            let count = list_events(server, session_id).len();
            if count == last {
                stable_samples += 1;
                if stable_samples >= 3 {
                    return count;
                }
            } else {
                last = count;
                stable_samples = 0;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        list_events(server, session_id).len()
    }

    async fn wait_for_events(
        server: &MemoryServer,
        session_id: &str,
        expected: usize,
    ) -> Vec<TachiEventRecord> {
        for _ in 0..50 {
            let events = list_events(server, session_id);
            if events.len() >= expected {
                return events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let events = list_events(server, session_id);
        panic!(
            "continuity pipeline wrote {} events, expected at least {expected}: {events:#?}",
            events.len()
        );
    }

    fn list_events(server: &MemoryServer, session_id: &str) -> Vec<TachiEventRecord> {
        server
            .with_global_store_read(|store| {
                store
                    .list_tachi_events(&memcore::TachiEventQuery {
                        session_id: Some(session_id.to_string()),
                        limit: 10,
                        ..memcore::TachiEventQuery::default()
                    })
                    .map_err(|error| error.to_string())
            })
            .expect("list continuity events")
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn continuity_pipeline_persists_top_level_event_receipts_for_candidate_and_outcome() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _enabled = crate::test_support::EnvRestore::set("TACHI_CONTINUITY_PIPELINE", "1");
        let path_without_claude = tempfile::tempdir().expect("empty PATH fixture");
        let _path = crate::test_support::EnvRestore::set_path("PATH", path_without_claude.path());

        let provider = MockContinuityProvider::start(false).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let mut server =
            MemoryServer::new(dir.path().join("memory.db"), None).expect("test server");
        server.llm = Arc::new(provider.llm.clone());

        let status = maybe_spawn_session_continuity_pipeline(
            &server,
            ContinuityEventTarget::new(crate::DbScope::Global, None, None),
            "continuity-receipt-shape",
            "fixture-session:turn-1",
            "fixture-source-revision",
            "fixture-captured-event",
            "continuity-session-1".to_string(),
            "turn-1".to_string(),
            "codex".to_string(),
            Some("sigil".to_string()),
            vec![Message {
                role: "assistant".to_string(),
                content: "Continuity should persist event receipts.".to_string(),
            }],
        );
        assert_eq!(status["status"], json!("scheduled_best_effort"));

        let events = wait_for_events(&server, "continuity-session-1", 2).await;
        let candidate = events
            .iter()
            .find(|event| event.adapter == "continuity_distill")
            .expect("candidate event");
        assert_eq!(
            candidate
                .provenance
                .pointer("/model_invocation/effective_model"),
            Some(&json!("continuity-distill-model"))
        );
        assert!(
            candidate
                .provenance
                .pointer("/provenance/model_invocation")
                .is_none(),
            "event ledger provenance must not double-nest metadata provenance: {candidate:#?}"
        );

        let outcome = events
            .iter()
            .find(|event| event.adapter == "continuity_labeler")
            .expect("outcome event");
        assert_eq!(
            outcome
                .provenance
                .pointer("/model_invocation/effective_model"),
            Some(&json!("continuity-reasoning-model"))
        );
        assert!(
            outcome
                .provenance
                .pointer("/provenance/model_invocation")
                .is_none(),
            "event ledger provenance must not double-nest metadata provenance: {outcome:#?}"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn continuity_pipeline_truncated_outputs_write_zero_events() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _enabled = crate::test_support::EnvRestore::set("TACHI_CONTINUITY_PIPELINE", "1");
        let path_without_claude = tempfile::tempdir().expect("empty PATH fixture");
        let _path = crate::test_support::EnvRestore::set_path("PATH", path_without_claude.path());

        let provider = MockContinuityProvider::start(true).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let mut server =
            MemoryServer::new(dir.path().join("memory.db"), None).expect("test server");
        server.llm = Arc::new(provider.llm.clone());

        let status = maybe_spawn_session_continuity_pipeline(
            &server,
            ContinuityEventTarget::new(crate::DbScope::Global, None, None),
            "continuity-truncated-zero-events",
            "fixture-session:turn-1",
            "fixture-source-revision",
            "fixture-captured-event",
            "continuity-session-truncated".to_string(),
            "turn-1".to_string(),
            "codex".to_string(),
            Some("sigil".to_string()),
            vec![Message {
                role: "assistant".to_string(),
                content: "Truncated continuity output must write no events.".to_string(),
            }],
        );
        assert_eq!(status["status"], json!("scheduled_best_effort"));
        wait_for_calls(&provider, 2).await;
        wait_for_responses(&provider, 2).await;
        let event_count =
            wait_for_event_count_to_quiesce(&server, "continuity-session-truncated").await;

        let events = list_events(&server, "continuity-session-truncated");
        assert_eq!(
            events.len(),
            event_count,
            "event count changed after quiescence witness"
        );
        assert!(
            events.is_empty(),
            "truncated candidate and outcome outputs must write zero events: {events:#?}"
        );
    }

    struct SourceFixtureChild(std::process::Child);

    impl Drop for SourceFixtureChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }

    fn run_source_fixture(test: &str, body: impl FnOnce()) {
        const SELECTOR: &str = "TACHI_SOURCE_BINDING_FIXTURE_SELECTOR";
        const ROOT: &str = "TACHI_SOURCE_BINDING_FIXTURE_ROOT";
        if std::env::var(SELECTOR).ok().as_deref() == Some(test) {
            let root =
                std::path::PathBuf::from(std::env::var_os(ROOT).expect("private fixture root"));
            assert_eq!(std::env::current_dir().expect("child cwd"), root);
            body();
            std::fs::write(root.join("completed"), test).expect("child completion witness");
            return;
        }

        // Only filesystem/process setup occurs in the ambient parent; no server or LLM exists here.
        let root = tempfile::tempdir().expect("private source fixture process root");
        let root_path = root.path().canonicalize().expect("canonical private root");
        let home = root_path.join("home");
        let temp = root_path.join("tmp");
        let empty_path = root_path.join("empty-path");
        for directory in [&home, &temp, &empty_path] {
            std::fs::create_dir(directory).expect("private child directory");
        }
        let log_path = root_path.join("child.log");
        let output = std::fs::File::create(&log_path).expect("private child log");
        let exact = format!("continuity_ops::pipeline::tests::{test}");
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command
            .env_clear()
            .current_dir(&root_path)
            .args(["--exact", exact.as_str(), "--nocapture"])
            .env(SELECTOR, test)
            .env(ROOT, &root_path)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("TACHI_HOME", &home)
            .env("TMPDIR", &temp)
            .env("TMP", &temp)
            .env("TEMP", &temp)
            .env("PATH", &empty_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(
                output.try_clone().expect("clone child log"),
            ))
            .stderr(std::process::Stdio::from(output));
        #[cfg(windows)]
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        let mut child = SourceFixtureChild(command.spawn().expect("spawn private test child"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        loop {
            if let Some(status) = child.0.try_wait().expect("poll owned child") {
                assert!(
                    status.success(),
                    "private fixture failed: {status}\n{}",
                    std::fs::read_to_string(&log_path).expect("read private child log")
                );
                assert_eq!(
                    std::fs::read_to_string(root_path.join("completed"))
                        .expect("selected child actually completed"),
                    test
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "private source fixture timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    async fn wait_for_source_outcomes(
        server: &MemoryServer,
        expected: usize,
    ) -> Vec<TachiEventRecord> {
        for _ in 0..100 {
            let events = list_events(server, "source-session");
            if events
                .iter()
                .filter(|event| event.event_type == "session.outcome")
                .count()
                == expected
            {
                return events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("private capture outcome did not arrive");
    }

    /// Capture lineage survives exact replay and distinguishes changed same-turn evidence.
    #[test]
    fn capture_outcome_source_binding_survives_restart_and_changed_evidence() {
        run_source_fixture(
            "capture_outcome_source_binding_survives_restart_and_changed_evidence",
            || {
                crate::test_support::with_tachi_home(|home| {
                    let _enabled =
                        crate::test_support::EnvRestore::set("TACHI_CONTINUITY_PIPELINE", "1");
                    let _health = crate::test_support::EnvRestore::set(
                        "TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST",
                        "1",
                    );
                    let _voyage = crate::test_support::EnvRestore::remove("VOYAGE_API_KEY");
                    let empty_path = tempfile::tempdir().expect("private empty PATH");
                    let _path =
                        crate::test_support::EnvRestore::set_path("PATH", empty_path.path());
                    let runtime = tokio::runtime::Runtime::new().expect("private capture runtime");
                    runtime.block_on(async {
                let calls = Arc::new(AtomicUsize::new(0));
                let handler_calls = calls.clone();
                let app = Router::new().route("/chat/completions", post(move |Json(request): Json<Value>| {
                    let calls = handler_calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let content = match request["model"].as_str().expect("fixture model") {
                            "source-extract" => json!([{
                                "text": "A captured source observation.", "summary": "Source observation",
                                "topic": "session", "category": "fact", "scope": "global", "importance": 0.7
                            }]).to_string(),
                            "source-distill" => json!({"candidates": []}).to_string(),
                            "source-reasoning" => json!({
                                "outcome": "success", "evidence_basis": "external_evidence",
                                "confidence": 0.9, "rationale": "Fixture outcome",
                                "evidence_refs": [], "claims": [], "open_questions": []
                            }).to_string(),
                            other => panic!("unexpected private fixture model {other}"),
                        };
                        Json(json!({"choices": [{"message": {"role": "assistant", "content": content},
                            "finish_reason": "stop"}], "model": request["model"],
                            "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}}))
                    }
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("private loopback");
                let url = format!("http://{}/chat/completions", listener.local_addr().expect("loopback address"));
                let task = tokio::spawn(async move { axum::serve(listener, app).await.expect("private provider"); });
                let lane = |model: &str| ChatLaneConfig {
                    base_url: url.clone(), model: model.to_string(), api_key_envs: vec!["SOURCE_FIXTURE_KEY"],
                };
                let llm = LlmClient::new_with_config(ProviderRuntimeConfig {
                    extract: lane("source-extract"), summary: lane("source-summary"),
                    reasoning: lane("source-reasoning"), distill: lane("source-distill"),
                    rerank: RerankConfig { provider: RerankProviderKind::Voyage, local_endpoint: None },
                }, None).expect("private LLM");
                assert!(llm.set_provider_secret_pool("SOURCE_FIXTURE_KEY", vec![ProviderSecret {
                    key_id: "source-fixture".into(), value: "synthetic-provider-key".into(),
                }]));
                // Existing RAII fixture guard aborts only this owned loopback task on every exit.
                let provider = MockContinuityProvider { llm, calls, responses: Arc::new(AtomicUsize::new(0)), task };
                let db = home.join("global").join("memory.db");
                std::fs::create_dir_all(db.parent().expect("private DB parent")).expect("private DB directory");
                let open = || {
                    let mut server = MemoryServer::new_with_background_workers_for_test(db.clone(), None, true)
                        .expect("private capture server");
                    server.llm = Arc::new(provider.llm.clone());
                    server
                };
                let params = crate::tool_params::CaptureSessionParams {
                    conversation_id: "source-session".into(), turn_id: "same-turn".into(), agent_id: "source-fixture".into(),
                    messages: vec![Message { role: "user".into(), content: "synthetic-source-secret-A".into() }],
                    path_prefix: Some("/capture/source-binding".into()), scope: "global".into(), project: None,
                    project_explicit: false, min_chars: 1, force: true,
                };
                let server = open();
                crate::foundry_runtime_ops::handle_capture_session(&server, params.clone())
                    .await.expect("capture A");
                let events = wait_for_source_outcomes(&server, 1).await;
                let first = events.iter().find(|event| event.event_type == "session.outcome").expect("outcome A").clone();
                let capture_id = first.provenance["source_event_id"].as_str().expect("actual source event ID");
                assert!(events.iter().any(|event| event.id == capture_id && event.event_type == "session.captured"));
                let revision = crate::tool_params::canonical_json_sha256(&json!({"messages": params.messages})).expect("canonical source hash");
                assert_eq!(first.provenance["source_refs"], json!([{"ref_type": "turn", "ref_id": "source-session:same-turn", "revision": revision}]));
                assert_eq!(first.provenance.pointer("/model_invocation/effective_model"), Some(&json!("source-reasoning")));
                let first_bytes = serde_json::to_value(&first).expect("first event bytes");
                let count = events.len();
                let calls = provider.calls.load(Ordering::SeqCst);
                assert_eq!(calls, 3, "one extraction, distill and outcome call");
                drop(server);
                let server = open();
                crate::foundry_runtime_ops::handle_capture_session(&server, params.clone())
                    .await.expect("exact A replay after reopen");
                assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
                assert_eq!(list_events(&server, "source-session").len(), count);
                let mut changed = params;
                changed.messages[0].content = "synthetic-source-secret-B".into();
                let changed_revision = crate::tool_params::canonical_json_sha256(&json!({"messages": changed.messages})).expect("changed hash");
                crate::foundry_runtime_ops::handle_capture_session(&server, changed)
                    .await.expect("same-turn changed B");
                let events = wait_for_source_outcomes(&server, 2).await;
                assert_eq!(provider.calls.load(Ordering::SeqCst), calls + 3);
                assert_eq!(serde_json::to_value(events.iter().find(|event| event.id == first.id).expect("A retained")).expect("A bytes"), first_bytes);
                let second = events.iter().find(|event| event.event_type == "session.outcome" && event.id != first.id).expect("outcome B");
                assert_eq!(second.provenance["source_refs"][0]["revision"], changed_revision);
                assert_ne!(second.provenance["source_event_id"], first.provenance["source_event_id"]);
                assert!(events.iter().any(|event| event.event_type == "session.captured" && Some(event.id.as_str()) == second.provenance["source_event_id"].as_str()));
                for event in [first, second.clone()] {
                    let provenance = event.provenance.to_string();
                    assert!(!provenance.contains("synthetic-source-secret"));
                    assert!(!provenance.contains("synthetic-provider-key"));
                    assert!(!provenance.contains("messages"));
                }
            });
                });
            },
        );
    }

    #[test]
    fn outcome_source_binding_refuses_missing_values_before_schedule_claim() {
        run_source_fixture(
            "outcome_source_binding_refuses_missing_values_before_schedule_claim",
            || {
                crate::test_support::with_tachi_home(|home| {
                    let _enabled =
                        crate::test_support::EnvRestore::set("TACHI_CONTINUITY_PIPELINE", "1");
                    let server =
                        MemoryServer::new(home.join("memory.db"), None).expect("private server");
                    for (index, values) in [
                        ["", "revision", "event"],
                        ["turn", " ", "event"],
                        ["turn", "revision", ""],
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let key = format!("missing-source-{index}");
                        let target = ContinuityEventTarget::new(crate::DbScope::Global, None, None);
                        let result = maybe_spawn_session_continuity_pipeline(
                            &server,
                            target.clone(),
                            &key,
                            values[0],
                            values[1],
                            values[2],
                            "session".into(),
                            "turn".into(),
                            "actor".into(),
                            None,
                            vec![],
                        );
                        assert_eq!(result["reason"], "source_binding_missing");
                        assert!(target
                            .claim_pipeline_schedule(&server, &key)
                            .expect("refusal must not consume schedule key"));
                        assert!(list_events(&server, "session").is_empty());
                    }
                });
            },
        );
    }
}
