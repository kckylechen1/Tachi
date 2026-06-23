use super::*;
use crate::tool_params::AgentEvolutionMemoryQueryParams;

#[test]
fn agent_evolution_job_id_is_deterministic_for_same_inputs() {
    let params = SynthesizeAgentEvolutionParams {
        agent_id: "codex".to_string(),
        display_name: Some("Codex".to_string()),
        documents: vec![AgentEvolutionDocumentParams {
            kind: "identity".to_string(),
            path: None,
            content: "hello".to_string(),
        }],
        document_paths: Vec::new(),
        evidence: Vec::new(),
        evidence_paths: Vec::new(),
        memory_queries: vec![AgentEvolutionMemoryQueryParams {
            query: "routing".to_string(),
            title: None,
            path_prefix: None,
            project: None,
            weight: 1.0,
            top_k: 5,
        }],
        goals: vec!["tighten routing".to_string()],
        dry_run: false,
    };
    let first = agent_evolution_job_id(&params);
    let second = agent_evolution_job_id(&params);
    assert_eq!(first, second);
    assert!(first.starts_with("foundry-job:agent-evolution:"));
}

#[test]
fn agent_evolution_job_id_changes_when_inputs_change() {
    let mut params = SynthesizeAgentEvolutionParams {
        agent_id: "codex".to_string(),
        display_name: None,
        documents: Vec::new(),
        document_paths: Vec::new(),
        evidence: Vec::new(),
        evidence_paths: Vec::new(),
        memory_queries: Vec::new(),
        goals: vec!["one".to_string()],
        dry_run: false,
    };
    let first = agent_evolution_job_id(&params);
    params.goals = vec!["two".to_string()];
    let second = agent_evolution_job_id(&params);
    assert_ne!(first, second);
}

#[test]
fn foundry_job_is_active_for_queued_and_fresh_running() {
    let queued = json!({ "status": "queued" });
    assert!(foundry_job_is_active(&queued));

    let running = json!({
        "status": "running",
        "updated_at": Utc::now().to_rfc3339(),
    });
    assert!(foundry_job_is_active(&running));
}

#[test]
fn foundry_job_is_inactive_for_stale_running_and_terminal_states() {
    let stale = json!({
        "status": "running",
        "updated_at": (Utc::now() - chrono::Duration::minutes(31)).to_rfc3339(),
    });
    assert!(!foundry_job_is_active(&stale));
    assert!(foundry_running_job_is_stale(&stale));

    assert!(!foundry_job_is_active(&json!({ "status": "completed" })));
    assert!(!foundry_job_is_active(&json!({ "status": "failed" })));
}

#[test]
fn try_claim_foundry_job_rejects_completed_jobs() {
    let temp = tempfile::NamedTempFile::new().expect("temp db");
    let server = crate::MemoryServer::new(temp.path().to_path_buf(), None).expect("server");
    let params = SynthesizeAgentEvolutionParams {
        agent_id: "codex".to_string(),
        display_name: None,
        documents: vec![AgentEvolutionDocumentParams {
            kind: "identity".to_string(),
            path: None,
            content: "hello".to_string(),
        }],
        document_paths: Vec::new(),
        evidence: Vec::new(),
        evidence_paths: Vec::new(),
        memory_queries: Vec::new(),
        goals: Vec::new(),
        dry_run: false,
    };
    let job = build_foundry_job(&server, &params);
    save_foundry_job_state(&server, &job, "completed", json!({})).expect("seed completed");
    assert!(!try_claim_foundry_job(&server, &job));
}

#[test]
fn try_claim_foundry_job_reclaims_stale_running_jobs() {
    let temp = tempfile::NamedTempFile::new().expect("temp db");
    let server = crate::MemoryServer::new(temp.path().to_path_buf(), None).expect("server");
    let params = SynthesizeAgentEvolutionParams {
        agent_id: "codex".to_string(),
        display_name: None,
        documents: vec![AgentEvolutionDocumentParams {
            kind: "identity".to_string(),
            path: None,
            content: "hello".to_string(),
        }],
        document_paths: Vec::new(),
        evidence: Vec::new(),
        evidence_paths: Vec::new(),
        memory_queries: Vec::new(),
        goals: Vec::new(),
        dry_run: false,
    };
    let job = build_foundry_job(&server, &params);
    let stale_at = (Utc::now() - chrono::Duration::minutes(31)).to_rfc3339();
    let mut payload = serde_json::Map::new();
    payload.insert("job".into(), json!(job));
    payload.insert("status".into(), json!("running"));
    payload.insert("updated_at".into(), json!(stale_at));
    let value_json = serde_json::to_string(&serde_json::Value::Object(payload)).expect("json");
    server
        .with_global_store(|store| {
            store
                .set_state(FOUNDRY_JOB_NAMESPACE, &job.id, &value_json)
                .map_err(|e| format!("seed stale running: {e}"))
        })
        .expect("seed stale running");

    assert!(try_claim_foundry_job(&server, &job));
    let state = load_foundry_job_state(&server, &job.id)
        .expect("load state")
        .expect("state exists");
    assert_eq!(foundry_job_status(&state), "running");
}

#[test]
fn parse_synthesis_response_accepts_json_object() {
    let parsed = parse_synthesis_response(
        r#"{
          "summary":"stable overall",
          "stable_signals":["keeps verifying changes"],
          "drift_signals":["routing is inconsistent"],
          "proposals":[
            {
              "title":"Tighten routing policy",
              "target":"AGENTS.md",
              "target_section":"Runtime 分流（硬规则）",
              "current_value":"old",
              "suggested_value":"new",
              "rationale":"eval shows repeated mismatch",
              "risk":"medium",
              "evidence_refs":["eval:2026-04-01"]
            }
          ],
          "no_change_reason":null
        }"#,
    )
    .expect("response should parse");

    assert_eq!(parsed.proposals.len(), 1);
    assert_eq!(parsed.proposals[0].target, "AGENTS.md");
}

#[test]
fn parse_review_status_accepts_expected_values() {
    assert_eq!(parse_review_status("approved").unwrap(), "approved");
    assert_eq!(parse_review_status("rejected").unwrap(), "rejected");
    assert_eq!(parse_review_status("applied").unwrap(), "applied");
    assert!(parse_review_status("queued").is_err());
}

#[test]
fn agent_evolution_proposal_identity_is_stable_per_job() {
    let params = SynthesizeAgentEvolutionParams {
        agent_id: "codex executor".to_string(),
        display_name: None,
        documents: Vec::new(),
        document_paths: Vec::new(),
        evidence: Vec::new(),
        evidence_paths: Vec::new(),
        memory_queries: Vec::new(),
        goals: Vec::new(),
        dry_run: false,
    };
    let job = memory_core::FoundryJobSpec {
        id: "foundry-job:abc/123".to_string(),
        kind: memory_core::FoundryJobKind::AgentEvolution,
        lane: memory_core::FoundryModelLane::Reasoning,
        status: memory_core::FoundryJobStatus::Queued,
        target_agent_id: Some(params.agent_id.clone()),
        requested_by: None,
        created_at: "2026-06-10T00:00:00Z".to_string(),
        evidence_count: 1,
        goal_count: 1,
        metadata: json!({}),
    };

    let first = agent_evolution_proposal_identity(&params, &job);
    let second = agent_evolution_proposal_identity(&params, &job);

    assert_eq!(first, second);
    assert_eq!(first.0, "agent-evolution-foundry-job_abc_123");
    assert_eq!(
        first.1,
        "/foundry/agents/codex_executor/proposals/foundry-job_abc_123"
    );
}

#[tokio::test]
async fn agent_evolution_proposal_persist_is_idempotent_per_job() {
    let temp = tempfile::NamedTempFile::new().expect("temp db");
    let server = crate::MemoryServer::new(temp.path().to_path_buf(), None).expect("server");
    let params = SynthesizeAgentEvolutionParams {
        agent_id: "codex".to_string(),
        display_name: None,
        documents: Vec::new(),
        document_paths: Vec::new(),
        evidence: Vec::new(),
        evidence_paths: Vec::new(),
        memory_queries: Vec::new(),
        goals: vec!["tighten routing".to_string()],
        dry_run: false,
    };
    let job = memory_core::FoundryJobSpec {
        id: "foundry-job:stable".to_string(),
        kind: memory_core::FoundryJobKind::AgentEvolution,
        lane: memory_core::FoundryModelLane::Reasoning,
        status: memory_core::FoundryJobStatus::Queued,
        target_agent_id: Some(params.agent_id.clone()),
        requested_by: None,
        created_at: "2026-06-10T00:00:00Z".to_string(),
        evidence_count: 1,
        goal_count: 1,
        metadata: json!({}),
    };

    let first_synthesis = memory_core::AgentEvolutionSynthesis {
        summary: "first".to_string(),
        stable_signals: Vec::new(),
        drift_signals: Vec::new(),
        proposals: Vec::new(),
        no_change_reason: None,
    };
    let second_synthesis = memory_core::AgentEvolutionSynthesis {
        summary: "second".to_string(),
        stable_signals: Vec::new(),
        drift_signals: Vec::new(),
        proposals: Vec::new(),
        no_change_reason: None,
    };

    let (first_id, first_path, target_db) =
        persist_agent_evolution_proposal(&server, &params, &job, &first_synthesis)
            .expect("first persist");
    let (second_id, second_path, second_target_db) =
        persist_agent_evolution_proposal(&server, &params, &job, &second_synthesis)
            .expect("second persist");

    assert_eq!(first_id, second_id);
    assert_eq!(first_path, second_path);
    assert_eq!(target_db, second_target_db);

    let root = proposal_root(&params.agent_id);
    let rows = server
        .with_store_for_scope_read(target_db, |store| {
            store
                .list_derived_by_source(AGENT_EVOLUTION_PROPOSAL_SOURCE, &root, 10)
                .map_err(|e| format!("list proposals: {e}"))
        })
        .expect("list proposals");
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0]["id"], first_id);
    assert_eq!(rows[0]["path"], first_path);
    assert_eq!(rows[0]["summary"], "second");
}

#[test]
fn apply_markdown_section_update_replaces_existing_section() {
    let original = "# Identity\n\n## Routing\n\nold value\n\n## Other\n\nstay\n";
    let updated = apply_markdown_section_update(original, Some("Routing"), "new value");
    assert!(updated.contains("## Routing\n\nnew value"));
    assert!(updated.contains("## Other\n\nstay"));
    assert!(!updated.contains("old value"));
}

#[test]
fn apply_markdown_section_update_appends_missing_section() {
    let updated =
        apply_markdown_section_update("# Identity\n", Some("Memory Policy"), "write less");
    assert!(updated.contains("## Memory Policy"));
    assert!(updated.contains("write less"));
}

#[test]
fn apply_markdown_section_update_replaces_nested_subsections_with_parent() {
    let original = "# Identity\n\n## Routing\n\nold value\n\n### Detail\n\nkeep with section\n\n## Other\n\nstay\n";
    let updated = apply_markdown_section_update(original, Some("Routing"), "new value");
    assert!(updated.contains("## Routing\n\nnew value"));
    assert!(updated.contains("## Other\n\nstay"));
    assert!(!updated.contains("### Detail"));
    assert!(!updated.contains("old value"));
}

#[test]
fn resolve_projection_write_path_accepts_repo_relative_file() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("repo root")
        .to_path_buf();
    let target = repo_root.join("docs/neural-foundry-v1.md");
    let resolved = resolve_projection_write_path(target.to_string_lossy().as_ref())
        .expect("repo path allowed");
    assert!(resolved.ends_with("docs/neural-foundry-v1.md"));
}

#[test]
fn resolve_projection_write_path_rejects_outside_allowed_roots() {
    let outside = std::env::temp_dir().join("foundry-projection-outside.md");
    let err = resolve_projection_write_path(outside.to_string_lossy().as_ref())
        .expect_err("outside path should be rejected");
    assert!(err.contains("outside allowed roots"));
}

#[test]
fn proposal_targets_document_matches_policy_kind_without_md_suffix() {
    let doc = AgentEvolutionDocumentParams {
        kind: "routing_policy".to_string(),
        path: None,
        content: String::new(),
    };
    let proposal = memory_core::AgentEvolutionProposal {
        title: "Tighten routing".to_string(),
        target: "routing_policy".to_string(),
        target_section: Some("Rules".to_string()),
        current_value: None,
        suggested_value: "new rule".to_string(),
        rationale: "safer".to_string(),
        risk: "medium".to_string(),
        evidence_refs: vec![],
    };

    assert!(proposal_targets_document(&proposal, &doc));
}

#[test]
fn resolve_projection_write_path_rejects_symlink_target_outside_allowed_roots() {
    let root = std::env::temp_dir().join(format!("foundry-symlink-{}", uuid::Uuid::new_v4()));
    let allowed = root.join("allowed");
    let outside = root.join("outside");
    std::fs::create_dir_all(&allowed).expect("create allowed root");
    std::fs::create_dir_all(&outside).expect("create outside root");

    let original_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&allowed).expect("set cwd");

    let link_path = allowed.join("IDENTITY.md");
    let outside_target = outside.join("IDENTITY.md");
    std::fs::write(&outside_target, "outside").expect("seed outside target");
    std::os::unix::fs::symlink(&outside_target, &link_path).expect("create symlink");

    let err = resolve_projection_write_path(link_path.to_string_lossy().as_ref())
        .expect_err("symlink to outside root should be rejected");
    assert!(err.contains("resolves outside allowed roots"));

    std::env::set_current_dir(original_cwd).expect("restore cwd");
    let _ = std::fs::remove_file(&link_path);
    let _ = std::fs::remove_file(&outside_target);
    let _ = std::fs::remove_dir_all(&root);
}
