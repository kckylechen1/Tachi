use memcore::{AuthorityLevel, EffectScope, TachiEventQuery};

use super::pattern_evidence::{
    append_pattern_evidence, PatternEvidenceInput, PatternEvidenceOutcome, PatternEvidenceSource,
};
use super::storage::{list_projection_memories, ContinuityEventTarget};
use crate::tool_params::TachiEventParams;
use crate::MemoryServer;

fn production_callers(symbol: &str) -> Vec<String> {
    fn visit(dir: &std::path::Path, symbol: &str, callers: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                    visit(&path, symbol, callers);
                }
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
                || path.file_name().and_then(|name| name.to_str())
                    == Some("pattern_evidence_contract.rs")
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read Rust source");
            let needle = format!("{symbol}(");
            for line in source.lines().filter(|line| line.contains(&needle)) {
                let trimmed = line.trim_start();
                if !trimmed.starts_with("pub(crate) fn ") && !trimmed.starts_with("fn ") {
                    callers.push(
                        path.strip_prefix(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
                            .expect("source path under crate src")
                            .to_string_lossy()
                            .trim_start_matches('/')
                            .to_string(),
                    );
                }
            }
        }
    }

    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut callers = Vec::new();
    visit(&source_root, symbol, &mut callers);
    callers.sort();
    callers
}

fn input() -> PatternEvidenceInput {
    PatternEvidenceInput {
        source: PatternEvidenceSource::TaskCompletion,
        project: None,
        run_id: "flow-real-1".to_string(),
        source_revision: "eval-revision-7".to_string(),
        evidence_digest: "sha256:evidence-1".to_string(),
        pattern_id: "pattern-1".to_string(),
        outcome: PatternEvidenceOutcome::Hit,
        idempotency_key: "complete:flow-real-1:pattern-1:hit".to_string(),
    }
}

fn events(server: &MemoryServer) -> Vec<memcore::TachiEventRecord> {
    server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&TachiEventQuery {
                    limit: 20,
                    ..Default::default()
                })
                .map_err(|error| error.to_string())
        })
        .expect("list events")
}

fn project_params() -> TachiEventParams {
    TachiEventParams {
        action: "project".to_string(),
        format: None,
        id: None,
        source_repo: None,
        adapter: None,
        project: None,
        project_explicit: false,
        domain: None,
        session_id: None,
        actor: None,
        event_type: None,
        authority: None,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: None,
        provenance: None,
        created_at: None,
        limit: 20,
        path_prefix: None,
        dry_run: false,
    }
}

fn assert_pattern_evidence_was_not_projected(server: &MemoryServer, report: &serde_json::Value) {
    let target = ContinuityEventTarget::new(crate::DbScope::Global, None, None);
    let patterns = list_projection_memories(server, &target, "/user/patterns", 20)
        .expect("list projected pattern memories");
    let projection_state = patterns
        .iter()
        .map(|entry| {
            serde_json::json!({
                "id": entry.id,
                "tier": entry.tier,
                "counters": entry.metadata.get("counters"),
            })
        })
        .collect::<Vec<_>>();
    let active_memory_count = server
        .with_global_store_read(|store| {
            store
                .count_active_memories()
                .map_err(|error| error.to_string())
        })
        .expect("count active memories");

    assert_eq!(
        serde_json::json!({
            "projected_count": report["projected_count"],
            "skipped_count": report["skipped_count"],
            "promotion_candidate_count": report["promotion_candidate_count"],
            "active_memory_count": active_memory_count,
            "pattern_projection_state": projection_state,
        }),
        serde_json::json!({
            "projected_count": 0,
            "skipped_count": 1,
            "promotion_candidate_count": 0,
            "active_memory_count": 0,
            "pattern_projection_state": [],
        }),
        "append-only pattern evidence must never create projection memory, counters, tiers, or promotion candidates"
    );
}

#[test]
fn pattern_evidence_replays_one_collect_only_event_without_projection() {
    let dir = tempfile::tempdir().expect("temp dir");
    let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");

    let first = append_pattern_evidence(&server, input()).expect("first append");
    let second = append_pattern_evidence(&server, input()).expect("exact replay");

    assert!(!first.replayed);
    assert!(second.replayed);
    assert_eq!(second.event_id, first.event_id);
    assert_eq!(second.idempotency_key, first.idempotency_key);
    let events = events(&server);
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.authority, AuthorityLevel::CollectOnly);
    assert_eq!(event.effects, vec![EffectScope::None]);
    assert!(event.projection_hints.is_empty());
    assert_eq!(event.actor, "tachi-internal");
    assert_eq!(event.adapter, "tachi.pattern_evidence.v1");
    assert_eq!(event.session_id, "flow-real-1");
    assert_eq!(event.payload["source"], "task_completion");
    assert_eq!(event.payload["source_revision"], "eval-revision-7");
    assert_eq!(event.payload["evidence_digest"], "sha256:evidence-1");
    assert_eq!(event.payload["pattern_id"], "pattern-1");
    assert_eq!(event.payload["outcome"], "hit");
    assert_eq!(
        server
            .with_global_store_read(|store| {
                store
                    .count_active_memories()
                    .map_err(|error| error.to_string())
            })
            .expect("count memories"),
        0,
        "append-only evidence must not project or mutate memory authority"
    );
}

#[test]
fn pattern_evidence_same_key_semantic_drift_refuses_before_second_write() {
    let dir = tempfile::tempdir().expect("temp dir");
    let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");
    append_pattern_evidence(&server, input()).expect("first append");
    let mut drifted = input();
    drifted.evidence_digest = "sha256:different-evidence".to_string();

    let error = append_pattern_evidence(&server, drifted).expect_err("collision must refuse");

    assert!(error.contains("collision"), "unexpected error: {error}");
    assert_eq!(events(&server).len(), 1);
}

#[test]
fn pattern_evidence_missing_real_identity_refuses_before_write() {
    let dir = tempfile::tempdir().expect("temp dir");
    let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");
    let mut missing = input();
    missing.run_id.clear();

    let error = append_pattern_evidence(&server, missing).expect_err("identity must refuse");

    assert!(error.contains("real run/session/flow id"));
    assert!(events(&server).is_empty());
}

#[test]
fn pattern_evidence_auto_projector_denies_materialization() {
    let dir = tempfile::tempdir().expect("temp dir");
    let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");
    append_pattern_evidence(&server, input()).expect("append admitted pattern evidence");

    let report = super::projection::project_auto_continuity_events_for_target(
        &server,
        ContinuityEventTarget::new(crate::DbScope::Global, None, None),
        20,
    )
    .expect("run actual auto projector");

    assert_pattern_evidence_was_not_projected(&server, &report);
}

#[tokio::test]
async fn pattern_evidence_explicit_project_route_denies_materialization() {
    let dir = tempfile::tempdir().expect("temp dir");
    let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");
    append_pattern_evidence(&server, input()).expect("append admitted pattern evidence");

    let response = crate::event_ops::handle_tachi_event(&server, project_params())
        .await
        .expect("run actual tachi_event project route");
    let report: serde_json::Value =
        serde_json::from_str(&response).expect("parse project route response");

    assert_pattern_evidence_was_not_projected(&server, &report);
}

#[test]
fn pattern_evidence_production_census_is_exact_and_search_is_retired() {
    assert_eq!(
        production_callers("append_pattern_evidence_for_refs"),
        vec!["complete_ops/handler.rs", "workflow_closure.rs",],
        "only the two operational flow-owning internal producers are admitted",
    );
    let legacy_pattern_feedback_emitter = concat!("emit_pattern_feedback_", "event");
    assert_eq!(
        production_callers(legacy_pattern_feedback_emitter),
        Vec::<String>::new(),
        "the mutating legacy pattern-feedback emitter is physically retired",
    );
    assert!(
        production_callers("emit_pattern_seen_events").is_empty(),
        "pattern search's identity-free auto-seen caller is retired",
    );
    let context = include_str!("context.rs");
    assert!(
        !context.contains("append_pattern_evidence_for_refs(")
            && !context.contains(&format!("{legacy_pattern_feedback_emitter}("))
            && !context.contains("emit_pattern_seen_events("),
        "model-facing context is explicitly retired from evidence admission",
    );
}
