use super::*;
use crate::server_state::DbScope;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tachi_llm::PersistedModelInvocationReceiptV1;

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

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn daily_maintenance_and_statistics_run_without_reasoning_provider() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("home");
    let db_path = home.path().join("memory.db");
    let mut server = MemoryServer::new_isolated_with_home_for_test(
        db_path.clone(),
        None,
        home.path().to_path_buf(),
    )
    .expect("isolated server");
    let unavailable = || tachi_llm::llm::ChatLaneConfig {
        base_url: "http://127.0.0.1:9/unavailable".to_string(),
        model: "unavailable".to_string(),
        api_key_envs: vec!["DAILY_RETIRED_REASONING_KEY"],
    };
    server.llm = std::sync::Arc::new(
        tachi_llm::LlmClient::new_with_config(
            tachi_llm::llm::ProviderRuntimeConfig {
                extract: unavailable(),
                summary: unavailable(),
                distill: unavailable(),
                reasoning: unavailable(),
                rerank: tachi_llm::RerankConfig {
                    provider: tachi_llm::RerankProviderKind::Voyage,
                    local_endpoint: None,
                },
            },
            None,
        )
        .expect("unavailable provider configuration"),
    );
    let empty_bin = tempfile::tempdir().expect("empty PATH");
    let _path = crate::test_support::EnvRestore::set_path("PATH", empty_bin.path());
    server
        .with_global_store(|store| {
            for (id, retention) in [("stale", None), ("durable", Some("permanent"))] {
                let mut entry = crate::tests::make_entry(id);
                entry.path = format!("/notes/{id}");
                entry.importance = 0.5;
                entry.retention_policy = retention.map(str::to_string);
                store.upsert(&entry).map_err(|e| e.to_string())?;
            }
            for (id, outcome, quality) in [
                ("eval-success", "success", 8),
                ("eval-failure", "failure", 6),
            ] {
                let mut entry = crate::tests::make_entry(id);
                entry.path = format!("/eval/{outcome}");
                entry.metadata =
                    json!({"agent":"worker-a", "outcome":outcome, "quality_score":quality});
                store.upsert(&entry).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed maintenance and eval facts");
    crate::test_support::with_unrestricted_fixture_connection(&db_path, |connection| {
        connection
            .execute(
                "UPDATE memories SET created_at='2020-01-01T00:00:00Z' WHERE id IN ('stale','durable')",
                [],
            )
            .map(|_| ())
    })
    .expect("backdate only the private maintenance fixtures");

    let report = run_daily_pipeline(&server)
        .await
        .expect("daily maintenance");

    assert_eq!(report.truth_maintenance.status, "completed");
    assert_eq!(report.database_stats.status, "collected");
    assert_eq!(report.routing_stats.details["total_eval_records"], 2);
    assert_eq!(
        report.routing_stats.details["agents"][0]["success_rate"],
        0.5
    );
    assert_eq!(
        report.routing_stats.details["agents"][0]["avg_quality"],
        7.0
    );
    assert!(
        report.database_stats.details["databases"][0]["total_entries"]
            .as_i64()
            .is_some_and(|count| count >= 4)
    );
    assert!(
        report.truth_maintenance.details["scope_accounting"]["progressed"]
            .as_array()
            .is_some_and(|targets| !targets.is_empty())
    );
    server
        .with_global_store_read(|store| {
            assert!(
                store
                    .get_with_options("stale", true)
                    .unwrap()
                    .unwrap()
                    .archived
            );
            assert!(!store.get("durable").unwrap().unwrap().archived);
            let wiki_count: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE path LIKE '/wiki/tachi/daily-health%'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(wiki_count, 0);
            Ok::<(), String>(())
        })
        .expect("maintenance results");
    assert!(!home.path().join("reports").join("daily").exists());
}

// Historical on-disk fixtures exercise the retained reader, without restoring
// the retired publisher or making provider calls to fabricate old reports.
fn historical_report_fixture(
    home: &Path,
    date: &str,
    revision: i64,
    routing_details: Value,
    routing_receipt: bool,
) -> (PathBuf, PathBuf, Value) {
    let reports_dir = home.join("reports").join("daily");
    std::fs::create_dir_all(&reports_dir).expect("historical reports dir");
    let health = serde_json::to_string_pretty(&json!({
        "overall_health": "healthy", "marker": revision,
    }))
    .unwrap();
    let routing = serde_json::to_string_pretty(&routing_details).unwrap();
    let markdown = format!(
        "# Historical daily report\n\n## Health Check\n\n```json\n{health}\n```\n\n## Routing Analysis\n\n```json\n{routing}\n```\n"
    );
    let receipt = |content: &str, stage: &str| {
        json!({
            "schema": tachi_llm::MODEL_INVOCATION_SCHEMA_V1,
            "lane": "reasoning",
            "engine_kind": "provider_http",
            "effective_provider": "historical-provider",
            "effective_model": "historical-model",
            "effective_version": null,
            "fallback_chain": [],
            "degraded": false,
            "completion_status": "complete",
            "prompt_tokens": 3,
            "completion_tokens": 5,
            "total_tokens": 8,
            "latency_ms": 10,
            "content_hash": PersistedModelInvocationReceiptV1::content_hash_for(content),
            "memory_id": format!("daily-report:{date}:{stage}"),
            "revision": revision,
        })
    };
    let payload_path = reports_dir.join(format!("{date}.r{revision}.md"));
    let sidecar_path = reports_dir.join(format!("{date}.r{revision}.model-invocations-v1.json"));
    let sidecar = json!({
        "schema": "daily-model-invocations-v1",
        "date": date,
        "revision": revision,
        "payload_basename": format!("{date}.r{revision}.md"),
        "report_content_hash": PersistedModelInvocationReceiptV1::content_hash_for(&markdown),
        "health": receipt(&health, "health"),
        "routing": if routing_receipt { receipt(&routing, "routing") } else { Value::Null },
    });
    std::fs::write(&payload_path, markdown).expect("historical payload");
    std::fs::write(&sidecar_path, serde_json::to_vec(&sidecar).unwrap())
        .expect("historical sidecar");
    (payload_path, sidecar_path, sidecar)
}

#[test]
fn mismatched_sidecar_is_rejected_without_legacy_fallback() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (payload_path, sidecar_path, mut sidecar) = historical_report_fixture(
        tmp.path(),
        "2026-08-06",
        1,
        json!({"disposition":"non_model_skip"}),
        false,
    );
    assert_eq!(
        validated_latest_daily_report(tmp.path()),
        Some(payload_path.display().to_string())
    );
    std::fs::write(
        payload_path.with_file_name("2026-08-06.md"),
        "legacy fallback must not win",
    )
    .expect("legacy");
    sidecar["report_content_hash"] = json!("mismatch");
    std::fs::write(sidecar_path, serde_json::to_vec(&sidecar).unwrap()).expect("corrupt");
    assert_eq!(validated_latest_daily_report(tmp.path()), None);
}

#[test]
fn newest_stripped_health_identity_falls_back_to_prior_generation() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (prior, _, _) = historical_report_fixture(
        tmp.path(),
        "2026-08-06",
        1,
        json!({"disposition":"non_model_skip"}),
        false,
    );
    let (_, newest_path, mut newest) = historical_report_fixture(
        tmp.path(),
        "2026-08-06",
        2,
        json!({"disposition":"non_model_skip"}),
        false,
    );
    newest["health"]
        .as_object_mut()
        .expect("health receipt")
        .remove("effective_model");
    std::fs::write(newest_path, serde_json::to_vec(&newest).unwrap()).expect("strip identity");
    assert_eq!(
        validated_latest_daily_report(tmp.path()),
        Some(prior.display().to_string())
    );
}

#[test]
fn newest_model_derived_routing_without_receipt_falls_back_to_prior_generation() {
    let tmp = tempfile::tempdir().expect("tmp");
    let routing = json!({"routing_proposals":[],"marker":"model-derived"});
    let (prior, _, _) =
        historical_report_fixture(tmp.path(), "2026-08-06", 1, routing.clone(), true);
    historical_report_fixture(tmp.path(), "2026-08-06", 2, routing, false);
    assert_eq!(
        validated_latest_daily_report(tmp.path()),
        Some(prior.display().to_string())
    );
}

#[test]
fn non_model_skip_allows_null_routing_receipt() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (payload, sidecar_path, mut sidecar) = historical_report_fixture(
        tmp.path(),
        "2026-08-06",
        1,
        json!({"disposition":"non_model_skip"}),
        false,
    );
    assert_eq!(
        validated_latest_daily_report(tmp.path()),
        Some(payload.display().to_string())
    );
    sidecar["routing"] = sidecar["health"].clone();
    std::fs::write(sidecar_path, serde_json::to_vec(&sidecar).unwrap()).expect("inject receipt");
    assert!(
        validated_latest_daily_report(tmp.path()).is_none(),
        "a non-model skip cannot claim an invocation receipt"
    );
}

#[test]
fn late_generation_cannot_hide_newer_committed_generation() {
    let tmp = tempfile::tempdir().expect("tmp");
    for revision in [1, 3, 2] {
        historical_report_fixture(
            tmp.path(),
            "2026-08-06",
            revision,
            json!({"disposition":"non_model_skip"}),
            false,
        );
    }
    let latest = validated_latest_daily_report(tmp.path()).expect("latest generation");
    assert!(latest.ends_with("2026-08-06.r3.md"), "latest={latest}");
    let sidecar_path = tmp
        .path()
        .join("reports/daily/2026-08-06.r3.model-invocations-v1.json");
    let mut newest: Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).unwrap()).unwrap();
    newest["report_content_hash"] = json!("bad-digest");
    std::fs::write(sidecar_path, serde_json::to_vec(&newest).unwrap()).expect("corrupt latest");
    let fallback = validated_latest_daily_report(tmp.path()).expect("prior generation");
    assert!(
        fallback.ends_with("2026-08-06.r2.md"),
        "fallback={fallback}"
    );
}

#[test]
fn legacy_report_remains_readable_without_sidecar() {
    let tmp = tempfile::tempdir().expect("tmp");
    let reports = tmp.path().join("reports").join("daily");
    std::fs::create_dir_all(&reports).unwrap();
    let legacy = reports.join("2026-08-05.md");
    std::fs::write(&legacy, "historical daily report").unwrap();
    std::fs::write(reports.join("2026-08-06.r1.md"), "uncommitted payload").unwrap();
    assert_eq!(
        validated_latest_daily_report(tmp.path()),
        Some(legacy.display().to_string())
    );
}
