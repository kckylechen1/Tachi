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
    let job = memcore::FoundryJobSpec {
        id: "foundry-job:abc/123".to_string(),
        kind: memcore::FoundryJobKind::AgentEvolution,
        lane: memcore::FoundryModelLane::Reasoning,
        status: memcore::FoundryJobStatus::Queued,
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
    let job = memcore::FoundryJobSpec {
        id: "foundry-job:stable".to_string(),
        kind: memcore::FoundryJobKind::AgentEvolution,
        lane: memcore::FoundryModelLane::Reasoning,
        status: memcore::FoundryJobStatus::Queued,
        target_agent_id: Some(params.agent_id.clone()),
        requested_by: None,
        created_at: "2026-06-10T00:00:00Z".to_string(),
        evidence_count: 1,
        goal_count: 1,
        metadata: json!({}),
    };

    let first_synthesis = memcore::AgentEvolutionSynthesis {
        summary: "first".to_string(),
        stable_signals: Vec::new(),
        drift_signals: Vec::new(),
        proposals: Vec::new(),
        no_change_reason: None,
    };
    let second_synthesis = memcore::AgentEvolutionSynthesis {
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
