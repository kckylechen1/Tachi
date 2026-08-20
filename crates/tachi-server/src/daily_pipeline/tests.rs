use super::*;
use crate::server_state::DbScope;
use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

/// Serialize PATH mutation for Claude-CLI skip across parallel daily tests.
static REASONING_HTTP_FALLBACK_LOCK: Mutex<()> = Mutex::new(());
use tachi_llm::{
    llm::{ChatLaneConfig, ProviderRuntimeConfig},
    LlmClient, PersistedModelInvocationReceiptV1, ProviderSecret, RerankConfig, RerankProviderKind,
};

fn manifest_target(role: &str, path: PathBuf) -> ManifestDbTarget {
    ManifestDbTarget {
        name: "target".to_string(),
        label: "target".to_string(),
        path,
        role: role.to_string(),
        owner: "tachi".to_string(),
        schema_kind: "tachi".to_string(),
        allow_write: true,
        last_classification: "healthy".to_string(),
    }
}

#[tokio::test]
async fn collect_database_stats_for_targets_preserves_manifest_order() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut first = manifest_target("global", tmp.path().join("first.db"));
    first.name = "first".to_string();
    let mut second = manifest_target("project", tmp.path().join("second.db"));
    second.name = "second".to_string();

    let stats = collect_database_stats_for_targets(vec![first, second])
        .await
        .expect("stats");

    assert_eq!(
        stats
            .iter()
            .map(|stat| stat.name.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert!(stats.iter().all(|stat| stat.error.is_some()));
}

#[test]
fn truth_maintenance_routes_external_project_target_by_path() {
    let tmp = crate::test_support::non_skipped_fixture_tempdir("daily-pipeline-");
    let global = tmp.path().join("global").join("memory.db");
    let current_project = tmp
        .path()
        .join("workspace")
        .join(".tachi")
        .join("memory.db");
    crate::test_support::assert_repo_local_db_fixture_not_skipped(&current_project);
    let external = tmp.path().join("agent").join("memory.db");
    let target = manifest_target("agent", external.clone());

    let route = resolve_truth_maintenance_route_for_paths_in_home(
        tmp.path(),
        &global,
        Some(&current_project),
        &target,
    );

    assert_eq!(route.target_db, DbScope::Project);
    assert_eq!(route.named_project, None);
    assert_eq!(route.db_path.as_deref(), Some(external.as_path()));
}

#[test]
fn truth_maintenance_routes_external_global_target_as_global_path() {
    let tmp = crate::test_support::non_skipped_fixture_tempdir("daily-pipeline-");
    let global = tmp.path().join("global").join("memory.db");
    let current_project = tmp
        .path()
        .join("workspace")
        .join(".tachi")
        .join("memory.db");
    crate::test_support::assert_repo_local_db_fixture_not_skipped(&current_project);
    let external_global = tmp.path().join("archive").join("global-memory.db");
    let target = manifest_target("global", external_global.clone());

    let route = resolve_truth_maintenance_route_for_paths_in_home(
        tmp.path(),
        &global,
        Some(&current_project),
        &target,
    );

    assert_eq!(route.target_db, DbScope::Global);
    assert_eq!(route.named_project, None);
    assert_eq!(route.db_path.as_deref(), Some(external_global.as_path()));
}

#[test]
fn truth_maintenance_routes_plan_c_project_by_name() {
    let tmp = crate::test_support::non_skipped_fixture_tempdir("daily-pipeline-");

    let global = tmp.path().join("global").join("memory.db");
    let current_project = tmp
        .path()
        .join("workspace")
        .join(".tachi")
        .join("memory.db");
    crate::test_support::assert_repo_local_db_fixture_not_skipped(&current_project);
    let named = tmp.path().join("projects").join("sigil").join("memory.db");
    let target = manifest_target("project", named);

    let route = resolve_truth_maintenance_route_for_paths_in_home(
        tmp.path(),
        &global,
        Some(&current_project),
        &target,
    );

    assert_eq!(route.target_db, DbScope::Project);
    assert_eq!(route.named_project.as_deref(), Some("sigil"));
    assert_eq!(route.db_path, None);
}

struct MockReasoningBody {
    content: String,
    model: String,
    version: String,
    finish_reason: String,
}

struct MockReasoningState {
    calls: Arc<AtomicUsize>,
    bodies: Vec<MockReasoningBody>,
}

struct MockReasoningProvider {
    llm: LlmClient,
    calls: Arc<AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

struct ReasoningHttpFallbackGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    _empty_bin: tempfile::TempDir,
    _path_guard: crate::test_support::EnvRestore,
}

/// Hide `claude` from PATH so reasoning falls through to the mock HTTP lane.
/// Apply this AFTER `make_server()` — fixture setup may rewrite PATH.
fn force_reasoning_http_fallback() -> ReasoningHttpFallbackGuard {
    let lock = REASONING_HTTP_FALLBACK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let empty_bin = tempfile::tempdir().expect("empty PATH bin");
    let path_guard = crate::test_support::EnvRestore::set_path("PATH", empty_bin.path());
    ReasoningHttpFallbackGuard {
        _lock: lock,
        _empty_bin: empty_bin,
        _path_guard: path_guard,
    }
}

async fn mock_reasoning_response(State(state): State<Arc<MockReasoningState>>) -> Response {
    let call = state.calls.fetch_add(1, Ordering::SeqCst);
    let body = state
        .bodies
        .get(call)
        .or_else(|| state.bodies.last())
        .expect("mock body");
    Json(json!({
        "choices": [{
            "message": {"role": "assistant", "content": body.content},
            "finish_reason": body.finish_reason,
        }],
        "usage": {
            "prompt_tokens": 3,
            "completion_tokens": 5,
            "total_tokens": 8
        },
        "model": body.model,
        "system_fingerprint": body.version,
    }))
    .into_response()
}

impl MockReasoningProvider {
    async fn start_with(bodies: Vec<MockReasoningBody>) -> Self {
        assert!(!bodies.is_empty());
        let calls = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(MockReasoningState {
            calls: Arc::clone(&calls),
            bodies,
        });
        let app = Router::new()
            .route("/chat/completions", post(mock_reasoning_response))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock reasoning");
        let port = listener.local_addr().expect("addr").port();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve mock");
        });

        let unused = || ChatLaneConfig {
            base_url: "https://unused.test/v1/chat/completions".to_string(),
            model: "configured-primary-must-not-leak".to_string(),
            api_key_envs: vec!["UNUSED_API_KEY"],
        };
        let llm = LlmClient::new_with_config(
            ProviderRuntimeConfig {
                extract: unused(),
                summary: unused(),
                distill: unused(),
                reasoning: ChatLaneConfig {
                    base_url: format!("http://127.0.0.1:{port}/chat/completions"),
                    model: "configured-primary-must-not-leak".to_string(),
                    api_key_envs: vec!["DAILY_REASONING_TEST_KEY"],
                },
                rerank: RerankConfig {
                    provider: RerankProviderKind::Voyage,
                    local_endpoint: None,
                },
            },
            None,
        )
        .expect("llm");
        assert!(llm.set_provider_secret_pool(
            "DAILY_REASONING_TEST_KEY",
            vec![ProviderSecret {
                key_id: "daily-reasoning-test".to_string(),
                value: "test-key".to_string(),
            }],
        ));

        Self {
            llm,
            calls,
            _task: task,
        }
    }

    async fn start(bodies: Vec<(String, &'static str, &'static str)>) -> Self {
        Self::start_with(
            bodies
                .into_iter()
                .map(|(content, model, version)| MockReasoningBody {
                    content,
                    model: model.to_string(),
                    version: version.to_string(),
                    finish_reason: "stop".to_string(),
                })
                .collect(),
        )
        .await
    }

    async fn start_truncated(content: &str) -> Self {
        Self::start_with(vec![MockReasoningBody {
            content: content.to_string(),
            model: "truncated-served-model".to_string(),
            version: "truncated-served-version".to_string(),
            finish_reason: "length".to_string(),
        }])
        .await
    }
}

fn stage_report(summary: &str, details: Value) -> DailyStageReport {
    DailyStageReport {
        status: "ok".to_string(),
        summary: summary.to_string(),
        details,
    }
}

fn sample_pipeline_report(date: &str) -> DailyPipelineReport {
    DailyPipelineReport {
        date: date.to_string(),
        report_path: None,
        health_check: stage_report("health", json!({"overall_health":"good","marker":"health"})),
        truth_maintenance: stage_report("truth", json!({"ok":true})),
        skill_evolution: stage_report("skill", json!({"ok":true})),
        routing_analysis: stage_report(
            "routing",
            json!({
                "routing_proposals":[],
                "disposition":"non_model_skip",
                "marker":"routing"
            }),
        ),
    }
}

fn sample_report_artifacts(
    date: &str,
    health_marker: &str,
    routing_details: Value,
) -> (DailyPipelineReport, String, String, String) {
    let mut report = sample_pipeline_report(date);
    report.health_check.details = json!({
        "overall_health": "good",
        "marker": health_marker,
    });
    report.routing_analysis.details = routing_details;
    let health_section = serialize_daily_json_section_for_tests(&report.health_check.details)
        .expect("health section");
    let routing_section = serialize_daily_json_section_for_tests(&report.routing_analysis.details)
        .expect("routing section");
    let markdown = render_daily_report_markdown_for_tests(
        &report,
        &health_section,
        "{}",
        "{}",
        &routing_section,
    );
    (report, health_section, routing_section, markdown)
}

async fn mint_receipt(
    provider: &MockReasoningProvider,
    label: &str,
) -> PersistedModelInvocationReceiptV1 {
    let _cli_skip = force_reasoning_http_fallback();
    let outcome = provider
        .llm
        .call_reasoning_llm_with_receipt("system", label, None, 0.0, 32)
        .await
        .expect("mint receipt");
    assert!(!outcome.truncated);
    outcome.invocation
}

#[tokio::test]
async fn provider_fallback_persists_actual_serving_identity_not_configured_primary() {
    let provider = MockReasoningProvider::start(vec![(
        r#"{"overall_health":"good","databases":[]}"#.to_string(),
        "actual-served-model-v9",
        "actual-served-version-v9",
    )])
    .await;
    let mut server = crate::tests::make_server();
    server.replace_llm(provider.llm.clone());
    let app_home = server.tachi_home_dir();
    let _cli_skip = force_reasoning_http_fallback();

    let (_stage, health_json, report_path, health_invocation) =
        run_health_check_for_tests(&server, &app_home, "2026-08-06")
            .await
            .expect("health");

    assert_eq!(
        health_invocation.effective_model(),
        Some("actual-served-model-v9")
    );
    assert_eq!(
        health_invocation.effective_version(),
        Some("actual-served-version-v9")
    );
    assert_ne!(
        health_invocation.effective_model(),
        Some("configured-primary-must-not-leak")
    );
    assert!(health_invocation.degraded());
    assert!(health_invocation
        .fallback_chain()
        .iter()
        .any(|marker| marker == "claude_cli_to_provider_http"));

    let health_section = serialize_daily_json_section_for_tests(&health_json).unwrap();
    let report = sample_pipeline_report("2026-08-06");
    let routing_section =
        serialize_daily_json_section_for_tests(&report.routing_analysis.details).unwrap();
    let markdown = render_daily_report_markdown_for_tests(
        &report,
        &health_section,
        "{}",
        "{}",
        &routing_section,
    );
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
    let sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &markdown,
        &health_section,
        &routing_section,
        &health_invocation,
        None,
    );
    publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
        .expect("publish");

    let sidecar_raw = std::fs::read_to_string(&sidecar_path).expect("sidecar");
    assert!(sidecar_raw.contains("actual-served-model-v9"));
    assert!(!sidecar_raw.contains("configured-primary-must-not-leak"));
    assert!(provider.calls.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn health_and_routing_receipts_are_independently_named_in_sidecar() {
    let provider = MockReasoningProvider::start(vec![
        (
            r#"{"overall_health":"good","databases":[]}"#.to_string(),
            "health-served-model",
            "health-served-version",
        ),
        (
            r#"{"routing_proposals":[{"agent":"a"}]}"#.to_string(),
            "routing-served-model",
            "routing-served-version",
        ),
    ])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let routing = mint_receipt(&provider, "routing").await;
    assert_ne!(health.effective_model(), routing.effective_model());

    let tmp = tempfile::tempdir().expect("tmp");
    let report_path = tmp.path().join("2026-08-06.md");
    let report = sample_pipeline_report("2026-08-06");
    let health_section =
        serialize_daily_json_section_for_tests(&report.health_check.details).unwrap();
    let routing_section =
        serialize_daily_json_section_for_tests(&report.routing_analysis.details).unwrap();
    let markdown = render_daily_report_markdown_for_tests(
        &report,
        &health_section,
        "{}",
        "{}",
        &routing_section,
    );
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
    let sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &markdown,
        &health_section,
        &routing_section,
        &health,
        Some(&routing),
    );
    publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
        .expect("publish");

    let sidecar_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).unwrap()).unwrap();
    assert_eq!(sidecar_json["schema"], "daily-model-invocations-v1");
    assert_eq!(
        sidecar_json["health"]["effective_model"],
        "health-served-model"
    );
    assert_eq!(
        sidecar_json["routing"]["effective_model"],
        "routing-served-model"
    );
    assert_eq!(
        sidecar_json["health"]["memory_id"],
        "daily-report:2026-08-06:health"
    );
    assert_eq!(
        sidecar_json["routing"]["memory_id"],
        "daily-report:2026-08-06:routing"
    );
    assert_ne!(
        sidecar_json["health"]["content_hash"],
        sidecar_json["routing"]["content_hash"]
    );
}

#[tokio::test]
async fn injected_failure_between_payload_and_sidecar_leaves_neither_success_pair() {
    let provider =
        MockReasoningProvider::start(vec![("ok".to_string(), "served-model", "served-version")])
            .await;
    let health = mint_receipt(&provider, "health").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let reports_dir = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("reports dir");
    let report_path = reports_dir.join("2026-08-06.md");
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
    let report = sample_pipeline_report("2026-08-06");
    let health_section =
        serialize_daily_json_section_for_tests(&report.health_check.details).unwrap();
    let routing_section =
        serialize_daily_json_section_for_tests(&report.routing_analysis.details).unwrap();
    let markdown = render_daily_report_markdown_for_tests(
        &report,
        &health_section,
        "{}",
        "{}",
        &routing_section,
    );
    let sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &markdown,
        &health_section,
        &routing_section,
        &health,
        None,
    );

    let err = publish_daily_report_pair_with_failure_for_tests(
        &report_path,
        &markdown,
        &sidecar_path,
        &sidecar,
        DailyPublishFailurePoint::P3AfterSidecarTempFsyncBeforeHardLink,
    )
    .expect_err("injected failure");

    assert!(err.contains("injected"));
    assert!(
        !report_path.exists(),
        "legacy payload path must not be used after mid-failure"
    );
    assert!(report_path
        .parent()
        .unwrap()
        .join("2026-08-06.r1.md")
        .exists());
    assert!(
        !sidecar_path.exists(),
        "sidecar must not remain success-shaped after mid-failure"
    );
}

#[tokio::test]
async fn truncated_model_output_refuses_clean_report_and_wiki() {
    let provider = MockReasoningProvider::start_truncated(r#"{"overall_health":"good"}"#).await;
    let mut server = crate::tests::make_server();
    server.replace_llm(provider.llm.clone());
    let app_home = server.tachi_home_dir();
    let _cli_skip = force_reasoning_http_fallback();
    let report_path = app_home.join("reports").join("daily").join("2026-08-06.md");
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);

    let err = run_health_check_for_tests(&server, &app_home, "2026-08-06")
        .await
        .expect_err("truncated must fail");
    assert!(err.contains("truncated"), "{err}");
    assert!(!report_path.exists());
    assert!(!sidecar_path.exists());
}

#[tokio::test]
async fn parse_failed_model_output_refuses_clean_report() {
    let provider = MockReasoningProvider::start(vec![(
        "not-json-at-all".to_string(),
        "parse-fail-model",
        "parse-fail-version",
    )])
    .await;
    let mut server = crate::tests::make_server();
    server.replace_llm(provider.llm.clone());
    let app_home = server.tachi_home_dir();
    let _cli_skip = force_reasoning_http_fallback();
    let report_path = app_home.join("reports").join("daily").join("2026-08-06.md");
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);

    let err = run_health_check_for_tests(&server, &app_home, "2026-08-06")
        .await
        .expect_err("parse failure must fail closed");
    assert!(err.contains("parse"), "{err}");
    assert!(!report_path.exists());
    assert!(!sidecar_path.exists());
}

#[tokio::test]
async fn replacement_cannot_pair_new_payload_with_old_receipt() {
    let provider = MockReasoningProvider::start(vec![
        ("first".to_string(), "model-a", "version-a"),
        ("second".to_string(), "model-b", "version-b"),
    ])
    .await;
    let first = mint_receipt(&provider, "first").await;
    let second = mint_receipt(&provider, "second").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let report_path = tmp.path().join("2026-08-06.md");
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);

    let old_health = json!({"overall_health":"old","marker":"old-health"});
    let old_routing = json!({"marker":"old-routing"});
    let old_report = DailyPipelineReport {
        date: "2026-08-06".to_string(),
        report_path: None,
        health_check: stage_report("old", old_health.clone()),
        truth_maintenance: stage_report("truth", json!({})),
        skill_evolution: stage_report("skill", json!({})),
        routing_analysis: stage_report("routing", old_routing.clone()),
    };
    let old_health_section = serialize_daily_json_section_for_tests(&old_health).unwrap();
    let old_routing_section = serialize_daily_json_section_for_tests(&old_routing).unwrap();
    let old_markdown = render_daily_report_markdown_for_tests(
        &old_report,
        &old_health_section,
        "{}",
        "{}",
        &old_routing_section,
    );
    let old_sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &old_markdown,
        &old_health_section,
        &old_routing_section,
        &first,
        Some(&first),
    );
    publish_daily_report_pair_for_tests(&report_path, &old_markdown, &sidecar_path, &old_sidecar)
        .expect("old publish");
    let old_sidecar_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).unwrap()).unwrap();
    let old_health_hash = old_sidecar_json["health"]["content_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let old_report_hash = old_sidecar_json["report_content_hash"]
        .as_str()
        .unwrap()
        .to_string();

    let new_health = json!({"overall_health":"new","marker":"new-health"});
    let new_routing = json!({"marker":"new-routing"});
    let new_report = DailyPipelineReport {
        date: "2026-08-06".to_string(),
        report_path: None,
        health_check: stage_report("new", new_health.clone()),
        truth_maintenance: stage_report("truth", json!({})),
        skill_evolution: stage_report("skill", json!({})),
        routing_analysis: stage_report("routing", new_routing.clone()),
    };
    let new_health_section = serialize_daily_json_section_for_tests(&new_health).unwrap();
    let new_routing_section = serialize_daily_json_section_for_tests(&new_routing).unwrap();
    let new_markdown = render_daily_report_markdown_for_tests(
        &new_report,
        &new_health_section,
        "{}",
        "{}",
        &new_routing_section,
    );
    let revision = next_daily_report_revision_for_tests(&sidecar_path);
    assert_eq!(revision, 2);
    let new_sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        revision,
        &new_markdown,
        &new_health_section,
        &new_routing_section,
        &second,
        Some(&second),
    );
    let new_sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, revision);
    publish_daily_report_pair_for_tests(
        &report_path,
        &new_markdown,
        &new_sidecar_path,
        &new_sidecar,
    )
    .expect("new publish");

    let new_sidecar_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&new_sidecar_path).unwrap()).unwrap();
    let new_markdown_on_disk =
        std::fs::read_to_string(tmp.path().join("2026-08-06.r2.md")).unwrap();
    assert_eq!(
        new_sidecar_json["report_content_hash"],
        PersistedModelInvocationReceiptV1::content_hash_for(&new_markdown_on_disk)
    );
    assert_ne!(new_sidecar_json["report_content_hash"], old_report_hash);
    assert_ne!(new_sidecar_json["health"]["content_hash"], old_health_hash);
    assert_eq!(new_sidecar_json["revision"], 2);
    assert_eq!(new_sidecar_json["health"]["effective_model"], "model-b");
    assert!(new_markdown_on_disk.contains("new-health"));
    assert!(!new_markdown_on_disk.contains("old-health"));
}

#[tokio::test]
async fn serialized_artifact_provenance_is_secret_negative() {
    let provider = MockReasoningProvider::start(vec![(
        r#"{"overall_health":"good"}"#.to_string(),
        "served-model",
        "served-version",
    )])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let routing = mint_receipt(&provider, "routing").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let report_path = tmp.path().join("2026-08-06.md");
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
    let report = sample_pipeline_report("2026-08-06");
    let health_section =
        serialize_daily_json_section_for_tests(&report.health_check.details).unwrap();
    let routing_section =
        serialize_daily_json_section_for_tests(&report.routing_analysis.details).unwrap();
    let markdown = render_daily_report_markdown_for_tests(
        &report,
        &health_section,
        "{}",
        "{}",
        &routing_section,
    );
    let sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &markdown,
        &health_section,
        &routing_section,
        &health,
        Some(&routing),
    );
    publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
        .expect("publish");

    let sidecar_raw = std::fs::read_to_string(&sidecar_path).unwrap();
    for forbidden in [
        "test-key",
        "DAILY_REASONING_TEST_KEY",
        "Bearer ",
        "Authorization",
        "prompt.md",
        "https://unused.test",
        "vault://",
        "api-key-id",
        "configured-primary-must-not-leak",
        "raw provider error",
    ] {
        assert!(
            !sidecar_raw.contains(forbidden),
            "sidecar leaked forbidden marker {forbidden}: {sidecar_raw}"
        );
    }
    let sidecar_json: Value = serde_json::from_str(&sidecar_raw).unwrap();
    for key in ["health", "routing"] {
        let receipt = sidecar_json[key].as_object().expect("receipt object");
        for forbidden_key in [
            "prompt",
            "content",
            "endpoint",
            "credential",
            "api_key",
            "vault",
            "error",
            "raw",
        ] {
            assert!(
                !receipt.contains_key(forbidden_key),
                "{key} receipt must not contain {forbidden_key}"
            );
        }
    }
}

#[tokio::test]
async fn sidecar_declares_immutable_revision_payload() {
    let provider = MockReasoningProvider::start(vec![(
        "health".to_string(),
        "served-model",
        "served-version",
    )])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let reports_dir = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("reports dir");
    let report_path = reports_dir.join("2026-08-06.md");
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
    let report = sample_pipeline_report("2026-08-06");
    let health_section =
        serialize_daily_json_section_for_tests(&report.health_check.details).unwrap();
    let routing_section =
        serialize_daily_json_section_for_tests(&report.routing_analysis.details).unwrap();
    let markdown = render_daily_report_markdown_for_tests(
        &report,
        &health_section,
        "{}",
        "{}",
        &routing_section,
    );
    let sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &markdown,
        &health_section,
        &routing_section,
        &health,
        None,
    );
    publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
        .expect("publish");

    let sidecar_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).expect("sidecar"))
            .expect("sidecar json");
    assert_eq!(sidecar_json["payload_basename"], "2026-08-06.r1.md");
    assert!(report_path
        .parent()
        .unwrap()
        .join("2026-08-06.r1.md")
        .exists());
    assert!(!report_path.exists());
}

#[tokio::test]
async fn mismatched_sidecar_is_rejected_without_legacy_fallback() {
    let provider = MockReasoningProvider::start(vec![(
        "health".to_string(),
        "served-model",
        "served-version",
    )])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let reports_dir = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("reports dir");
    let report_path = reports_dir.join("2026-08-06.md");
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
    let report = sample_pipeline_report("2026-08-06");
    let health_section =
        serialize_daily_json_section_for_tests(&report.health_check.details).unwrap();
    let routing_section =
        serialize_daily_json_section_for_tests(&report.routing_analysis.details).unwrap();
    let markdown = render_daily_report_markdown_for_tests(
        &report,
        &health_section,
        "{}",
        "{}",
        &routing_section,
    );
    let sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &markdown,
        &health_section,
        &routing_section,
        &health,
        None,
    );
    publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
        .expect("publish");
    std::fs::write(&report_path, "legacy fallback must not win").expect("legacy");
    let mut corrupt: Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).unwrap()).unwrap();
    corrupt["report_content_hash"] = json!("mismatch");
    std::fs::write(&sidecar_path, serde_json::to_vec(&corrupt).unwrap()).expect("corrupt");

    assert_eq!(
        crate::daily_pipeline::validated_latest_daily_report(tmp.path()),
        None
    );
}

#[tokio::test]
async fn newest_stripped_health_identity_falls_back_to_prior_generation() {
    let provider = MockReasoningProvider::start(vec![(
        "health".to_string(),
        "served-model",
        "served-version",
    )])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let reports_dir = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("reports dir");
    let report_path = reports_dir.join("2026-08-06.md");

    for revision in [1_i64, 2] {
        let (_report, health_section, routing_section, markdown) = sample_report_artifacts(
            "2026-08-06",
            &format!("health-{revision}"),
            json!({"disposition":"non_model_skip","marker":revision}),
        );
        let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, revision);
        let sidecar = build_daily_sidecar_for_tests(
            "2026-08-06",
            revision,
            &markdown,
            &health_section,
            &routing_section,
            &health,
            None,
        );
        publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
            .expect("generation publish");
    }

    let newest_sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 2);
    let mut newest: Value =
        serde_json::from_str(&std::fs::read_to_string(&newest_sidecar_path).unwrap()).unwrap();
    newest["health"]
        .as_object_mut()
        .expect("health receipt")
        .remove("effective_model");
    std::fs::write(&newest_sidecar_path, serde_json::to_vec(&newest).unwrap())
        .expect("strip newest identity");

    let latest = crate::daily_pipeline::validated_latest_daily_report(tmp.path())
        .expect("prior valid generation");
    assert!(latest.ends_with("2026-08-06.r1.md"), "latest={latest}");
}

#[tokio::test]
async fn newest_model_derived_routing_without_receipt_falls_back_to_prior_generation() {
    let provider = MockReasoningProvider::start(vec![
        ("health".to_string(), "health-model", "health-version"),
        ("routing".to_string(), "routing-model", "routing-version"),
    ])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let routing = mint_receipt(&provider, "routing").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let reports_dir = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("reports dir");
    let report_path = reports_dir.join("2026-08-06.md");
    let model_routing = json!({"routing_proposals":[],"marker":"model-derived"});

    for (revision, routing_invocation) in [(1_i64, Some(&routing)), (2_i64, None)] {
        let (_report, health_section, routing_section, markdown) = sample_report_artifacts(
            "2026-08-06",
            &format!("health-{revision}"),
            model_routing.clone(),
        );
        let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, revision);
        let sidecar = build_daily_sidecar_for_tests(
            "2026-08-06",
            revision,
            &markdown,
            &health_section,
            &routing_section,
            &health,
            routing_invocation,
        );
        publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
            .expect("generation publish");
    }

    let latest = crate::daily_pipeline::validated_latest_daily_report(tmp.path())
        .expect("prior valid generation");
    assert!(latest.ends_with("2026-08-06.r1.md"), "latest={latest}");
}

#[tokio::test]
async fn non_model_skip_allows_null_routing_receipt() {
    let provider = MockReasoningProvider::start(vec![(
        "health".to_string(),
        "served-model",
        "served-version",
    )])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let reports_dir = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("reports dir");
    let report_path = reports_dir.join("2026-08-06.md");
    let (_report, health_section, routing_section, markdown) = sample_report_artifacts(
        "2026-08-06",
        "health",
        json!({"disposition":"non_model_skip","reason":"no eval rows"}),
    );
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
    let sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &markdown,
        &health_section,
        &routing_section,
        &health,
        None,
    );
    publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
        .expect("generation publish");

    let latest = crate::daily_pipeline::validated_latest_daily_report(tmp.path())
        .expect("non-model skip generation");
    assert!(latest.ends_with("2026-08-06.r1.md"), "latest={latest}");

    let mut mismatched: Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).expect("sidecar"))
            .expect("sidecar json");
    mismatched["routing"] = mismatched["health"].clone();
    std::fs::write(&sidecar_path, serde_json::to_vec(&mismatched).unwrap())
        .expect("inject receipt on non-model skip");
    assert!(
        crate::daily_pipeline::validated_latest_daily_report(tmp.path()).is_none(),
        "a non-model skip cannot claim an invocation receipt"
    );
}

#[test]
fn corrupt_sidecar_does_not_reset_immutable_revision_allocation() {
    let tmp = tempfile::tempdir().expect("tmp");
    let report_path = tmp.path().join("2026-08-06.md");
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 7);
    std::fs::write(tmp.path().join("2026-08-06.r7.md"), "prior").expect("payload");
    std::fs::write(&sidecar_path, "not-json").expect("corrupt sidecar");
    assert_eq!(next_daily_report_revision_for_tests(&sidecar_path), 8);
}

#[tokio::test]
async fn late_generation_cannot_hide_newer_committed_generation() {
    let provider = MockReasoningProvider::start(vec![(
        "health".to_string(),
        "served-model",
        "served-version",
    )])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let reports_dir = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("reports dir");
    let report_path = reports_dir.join("2026-08-06.md");
    for revision in [1_i64, 3, 2] {
        let report = sample_pipeline_report(&format!("2026-08-06-{revision}"));
        let health_section =
            serialize_daily_json_section_for_tests(&json!({"marker": revision})).unwrap();
        let routing_section =
            serialize_daily_json_section_for_tests(&report.routing_analysis.details).unwrap();
        let markdown = render_daily_report_markdown_for_tests(
            &report,
            &health_section,
            "{}",
            "{}",
            &routing_section,
        );
        let sidecar = build_daily_sidecar_for_tests(
            "2026-08-06",
            revision,
            &markdown,
            &health_section,
            &routing_section,
            &health,
            None,
        );
        let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, revision);
        publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
            .expect("generation publish");
    }

    let latest = crate::daily_pipeline::validated_latest_daily_report(tmp.path())
        .expect("valid latest generation");
    assert!(latest.ends_with("2026-08-06.r3.md"), "latest={latest}");
    let newest_sidecar = daily_report_generation_sidecar_path_for_tests(&report_path, 3);
    let mut corrupt: Value =
        serde_json::from_str(&std::fs::read_to_string(&newest_sidecar).expect("newest sidecar"))
            .expect("newest json");
    corrupt["report_content_hash"] = json!("bad-digest");
    std::fs::write(&newest_sidecar, serde_json::to_vec(&corrupt).unwrap()).expect("corrupt");
    let fallback = crate::daily_pipeline::validated_latest_daily_report(tmp.path())
        .expect("valid older generation");
    assert!(
        fallback.ends_with("2026-08-06.r2.md"),
        "fallback={fallback}"
    );
}

#[tokio::test]
async fn generation_collision_does_not_overwrite_existing_sidecar() {
    let provider = MockReasoningProvider::start(vec![(
        "health".to_string(),
        "served-model",
        "served-version",
    )])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let report_path = tmp.path().join("2026-08-06.md");
    let health_section = "{\"marker\":\"health\"}";
    let routing_section = "{\"routing_proposals\":[]}";
    let report = sample_pipeline_report("2026-08-06");
    let markdown = render_daily_report_markdown_for_tests(
        &report,
        health_section,
        "{}",
        "{}",
        routing_section,
    );
    let sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &markdown,
        health_section,
        routing_section,
        &health,
        None,
    );
    let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
    publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
        .expect("first publish");
    let err = publish_daily_report_pair_for_tests(&report_path, &markdown, &sidecar_path, &sidecar)
        .expect_err("same generation must collide");
    assert!(err.contains("collision"), "{err}");
    assert!(sidecar_path.exists());
}

#[tokio::test]
async fn precommit_failure_preserves_prior_valid_generation() {
    let provider = MockReasoningProvider::start(vec![(
        "health".to_string(),
        "served-model",
        "served-version",
    )])
    .await;
    let health = mint_receipt(&provider, "health").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let reports_dir = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("reports dir");
    let report_path = reports_dir.join("2026-08-06.md");
    let report = sample_pipeline_report("2026-08-06");
    let health_section =
        serialize_daily_json_section_for_tests(&report.health_check.details).unwrap();
    let routing_section =
        serialize_daily_json_section_for_tests(&report.routing_analysis.details).unwrap();
    let first_markdown = render_daily_report_markdown_for_tests(
        &report,
        &health_section,
        "{}",
        "{}",
        &routing_section,
    );
    let first_sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        1,
        &first_markdown,
        &health_section,
        &routing_section,
        &health,
        None,
    );
    let first_sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
    publish_daily_report_pair_for_tests(
        &report_path,
        &first_markdown,
        &first_sidecar_path,
        &first_sidecar,
    )
    .expect("first generation");

    let second_markdown = first_markdown.replace("2026-08-06", "2026-08-07");
    let second_sidecar = build_daily_sidecar_for_tests(
        "2026-08-06",
        2,
        &second_markdown,
        &health_section,
        &routing_section,
        &health,
        None,
    );
    let second_sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 2);
    let err = publish_daily_report_pair_with_failure_for_tests(
        &report_path,
        &second_markdown,
        &second_sidecar_path,
        &second_sidecar,
        DailyPublishFailurePoint::P3AfterSidecarTempFsyncBeforeHardLink,
    )
    .expect_err("injected precommit failure");
    assert!(err.contains("before hard-link"), "{err}");
    assert!(first_sidecar_path.exists());
    assert!(!second_sidecar_path.exists());
    assert!(
        crate::daily_pipeline::validated_latest_daily_report(tmp.path())
            .expect("prior generation")
            .ends_with("2026-08-06.r1.md")
    );
}

#[tokio::test]
async fn publish_failure_points_preserve_or_commit_complete_generation() {
    let provider = MockReasoningProvider::start(vec![(
        "health".to_string(),
        "served-model",
        "served-version",
    )])
    .await;
    let health = mint_receipt(&provider, "health").await;

    for failure_point in [
        DailyPublishFailurePoint::P1AfterPayloadBytesBeforePayloadFsync,
        DailyPublishFailurePoint::P2AfterPayloadFsyncBeforeSidecarTemp,
        DailyPublishFailurePoint::P3AfterSidecarTempFsyncBeforeHardLink,
        DailyPublishFailurePoint::P4AfterHardLinkBeforeDirectoryFsync,
    ] {
        let tmp = tempfile::tempdir().expect("tmp");
        let reports_dir = tmp.path().join("reports").join("daily");
        std::fs::create_dir_all(&reports_dir).expect("reports dir");
        let report_path = reports_dir.join("2026-08-06.md");

        let (_first_report, first_health_section, first_routing_section, first_markdown) =
            sample_report_artifacts(
                "2026-08-06",
                "first",
                json!({"disposition":"non_model_skip","marker":"first"}),
            );
        let first_sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 1);
        let first_sidecar = build_daily_sidecar_for_tests(
            "2026-08-06",
            1,
            &first_markdown,
            &first_health_section,
            &first_routing_section,
            &health,
            None,
        );
        publish_daily_report_pair_for_tests(
            &report_path,
            &first_markdown,
            &first_sidecar_path,
            &first_sidecar,
        )
        .expect("first generation");
        assert!(
            crate::daily_pipeline::validated_latest_daily_report(tmp.path())
                .expect("first latest")
                .ends_with("2026-08-06.r1.md")
        );

        let (_second_report, second_health_section, second_routing_section, second_markdown) =
            sample_report_artifacts(
                "2026-08-06",
                "second",
                json!({"disposition":"non_model_skip","marker":"second"}),
            );
        let second_sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, 2);
        let second_sidecar = build_daily_sidecar_for_tests(
            "2026-08-06",
            2,
            &second_markdown,
            &second_health_section,
            &second_routing_section,
            &health,
            None,
        );
        let err = publish_daily_report_pair_with_failure_for_tests(
            &report_path,
            &second_markdown,
            &second_sidecar_path,
            &second_sidecar,
            failure_point,
        )
        .expect_err("injected publication failure");
        assert!(err.contains("injected"), "{err}");

        let expected_latest_revision =
            if failure_point == DailyPublishFailurePoint::P4AfterHardLinkBeforeDirectoryFsync {
                2
            } else {
                1
            };
        let latest = crate::daily_pipeline::validated_latest_daily_report(tmp.path())
            .expect("latest after injected failure");
        assert!(
            latest.ends_with(&format!("2026-08-06.r{expected_latest_revision}.md")),
            "failure_point={failure_point:?} latest={latest}"
        );

        if expected_latest_revision == 2 {
            let committed: Value = serde_json::from_str(
                &std::fs::read_to_string(&second_sidecar_path).expect("p4 sidecar"),
            )
            .expect("p4 sidecar json");
            let payload_path = reports_dir.join(
                committed["payload_basename"]
                    .as_str()
                    .expect("p4 payload basename"),
            );
            let payload = std::fs::read_to_string(&payload_path).expect("p4 payload");
            assert_eq!(
                committed["report_content_hash"],
                PersistedModelInvocationReceiptV1::content_hash_for(&payload)
            );
            assert_eq!(committed["revision"], 2);
            assert_eq!(committed["health"]["revision"], 2);
            assert_eq!(committed["health"]["completion_status"], "complete");
        } else {
            assert!(!second_sidecar_path.exists());
        }

        let (_clean_report, clean_health_section, clean_routing_section, clean_markdown) =
            sample_report_artifacts(
                "2026-08-06",
                "clean",
                json!({"disposition":"non_model_skip","marker":"clean"}),
            );
        let (clean_revision, clean_sidecar) = publish_daily_report_with_retry_for_tests(
            &report_path,
            "2026-08-06",
            &clean_markdown,
            &clean_health_section,
            &clean_routing_section,
            &health,
            None,
        )
        .expect("clean later publish");
        assert!(clean_revision > expected_latest_revision);
        assert_eq!(clean_sidecar.revision, clean_revision);
        let clean_latest =
            crate::daily_pipeline::validated_latest_daily_report(tmp.path()).expect("clean latest");
        assert!(
            clean_latest.ends_with(&format!("2026-08-06.r{clean_revision}.md")),
            "clean_latest={clean_latest}"
        );
    }
}

#[tokio::test]
async fn concurrent_publishers_commit_distinct_bound_generations() {
    let provider = MockReasoningProvider::start(vec![
        ("health-a".to_string(), "model-a", "version-a"),
        ("health-b".to_string(), "model-b", "version-b"),
    ])
    .await;
    let first_health = mint_receipt(&provider, "health-a").await;
    let second_health = mint_receipt(&provider, "health-b").await;
    let tmp = tempfile::tempdir().expect("tmp");
    let reports_dir = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("reports dir");
    let report_path = reports_dir.join("2026-08-06.md");
    // Both OS threads allocate revision 1 before either may publish. That
    // forces one real create_new/hard-link collision through the production
    // retry loop; a single-thread Tokio executor would only serialize these
    // blocking publishers and would not discriminate the CAS path.
    let barrier = Arc::new(std::sync::Barrier::new(2));

    let (_first_report, first_health_section, first_routing_section, first_markdown) =
        sample_report_artifacts(
            "2026-08-06",
            "publisher-a",
            json!({"disposition":"non_model_skip","marker":"publisher-a"}),
        );
    let first_task = {
        let barrier = Arc::clone(&barrier);
        let report_path = report_path.clone();
        let health_section = first_health_section.clone();
        let routing_section = first_routing_section.clone();
        let markdown = first_markdown.clone();
        let health = first_health.clone();
        std::thread::spawn(move || {
            let (revision, sidecar) =
                publish_daily_report_with_retry_after_first_allocation_for_tests(
                    &report_path,
                    "2026-08-06",
                    &markdown,
                    &health_section,
                    &routing_section,
                    &health,
                    None,
                    &barrier,
                )
                .expect("publisher a");
            (revision, sidecar, markdown, health_section)
        })
    };

    let (_second_report, second_health_section, second_routing_section, second_markdown) =
        sample_report_artifacts(
            "2026-08-06",
            "publisher-b",
            json!({"disposition":"non_model_skip","marker":"publisher-b"}),
        );
    let second_task = {
        let barrier = Arc::clone(&barrier);
        let report_path = report_path.clone();
        let health_section = second_health_section.clone();
        let routing_section = second_routing_section.clone();
        let markdown = second_markdown.clone();
        let health = second_health.clone();
        std::thread::spawn(move || {
            let (revision, sidecar) =
                publish_daily_report_with_retry_after_first_allocation_for_tests(
                    &report_path,
                    "2026-08-06",
                    &markdown,
                    &health_section,
                    &routing_section,
                    &health,
                    None,
                    &barrier,
                )
                .expect("publisher b");
            (revision, sidecar, markdown, health_section)
        })
    };

    let first = first_task.join().expect("publisher a join");
    let second = second_task.join().expect("publisher b join");
    assert_ne!(first.0, second.0);
    let mut revisions = vec![first.0, second.0];
    revisions.sort_unstable();
    assert_eq!(revisions, vec![1, 2]);

    for (revision, sidecar, markdown, health_section) in [&first, &second] {
        assert_eq!(sidecar.revision, *revision);
        let sidecar_path = daily_report_generation_sidecar_path_for_tests(&report_path, *revision);
        let sidecar_json: Value = serde_json::from_str(
            &std::fs::read_to_string(&sidecar_path).expect("committed sidecar"),
        )
        .expect("sidecar json");
        let payload_path = reports_dir.join(
            sidecar_json["payload_basename"]
                .as_str()
                .expect("payload basename"),
        );
        let payload = std::fs::read_to_string(&payload_path).expect("committed payload");
        assert_eq!(payload, *markdown);
        assert_eq!(
            sidecar_json["report_content_hash"],
            PersistedModelInvocationReceiptV1::content_hash_for(&payload)
        );
        assert_eq!(
            sidecar_json["health"]["content_hash"],
            PersistedModelInvocationReceiptV1::content_hash_for(health_section)
        );
        assert_eq!(sidecar_json["health"]["revision"], *revision);
    }

    let latest = crate::daily_pipeline::validated_latest_daily_report(tmp.path())
        .expect("highest valid generation");
    assert!(latest.ends_with("2026-08-06.r2.md"), "latest={latest}");
}

#[tokio::test]
async fn skill_evolution_excludes_retired_global_and_project_tombstones() {
    let (server, _project_db) =
        crate::tests::make_server_with_project_fixture("retired-trajectory-evolution");
    let mut global = memcore::HubCapability {
        id: crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID.to_string(),
        cap_type: "skill".to_string(),
        name: "trajectory-distiller".to_string(),
        version: 1,
        description: "Historical global trajectory writer omitted from evolution".to_string(),
        definition: serde_json::json!({
            "prompt": "Run historical trajectory distiller",
            "content": "Historical retired skill",
            "policy": { "visibility": "listed" }
        })
        .to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "direct".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    global.health_status = "unhealthy".to_string();
    global.fail_streak = 99;
    let mut project = global.clone();
    project.description = "Historical project trajectory writer omitted from evolution".to_string();
    server
        .with_global_store(|store| {
            store
                .hub_register(&global)
                .map_err(|error| error.to_string())
        })
        .expect("inject retired global evolution row");
    server
        .with_project_store(|store| {
            store
                .hub_register(&project)
                .map_err(|error| error.to_string())
        })
        .expect("inject retired project evolution row");

    let report = run_skill_evolution_stage(&server).await;
    let output = serde_json::to_string(&report.details).expect("evolution details JSON");
    assert!(
        !output.contains(crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID),
        "retired tombstone must not appear in evolution output: {output}"
    );
    assert!(
        !report
            .summary
            .contains(crate::builtins::RETIRED_TRAJECTORY_DISTILLER_ID),
        "retired tombstone must not appear in evolution summary"
    );
}
