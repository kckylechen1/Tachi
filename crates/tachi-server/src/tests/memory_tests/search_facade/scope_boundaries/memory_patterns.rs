use super::*;
use crate::{MemoryServer, TachiEventParams};

fn event_params(action: &str) -> TachiEventParams {
    TachiEventParams {
        action: action.to_string(),
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

async fn seed_projected_pattern(server: &MemoryServer, key: &str, text: &str) -> String {
    let mut emit = event_params("emit");
    emit.id = Some(format!("pattern-seed-{key}"));
    emit.source_repo = Some("sigil".to_string());
    emit.adapter = Some("pattern-feedback-test".to_string());
    emit.domain = Some("agent_os".to_string());
    emit.session_id = Some("session-pattern-feedback".to_string());
    emit.actor = Some("codex".to_string());
    emit.event_type = Some("pattern.candidate".to_string());
    emit.authority = Some("collect_only".to_string());
    emit.projection_hints = vec!["pattern".to_string()];
    emit.payload = Some(json!({
        "pattern_key": key,
        "summary": format!("Pattern {key}"),
        "text": text,
    }));
    crate::event_ops::handle_tachi_event(server, emit)
        .await
        .expect("emit pattern seed");

    let mut project = event_params("project");
    project.projection_hints = vec!["pattern".to_string()];
    let projected = crate::event_ops::handle_tachi_event(server, project)
        .await
        .expect("project pattern seed");
    let projected_json: serde_json::Value = serde_json::from_str(&projected).expect("project JSON");
    projected_json["projections"][0]["memory_id"]
        .as_str()
        .expect("pattern memory id")
        .to_string()
}

struct FlowRecordFixture(std::path::PathBuf);

impl Drop for FlowRecordFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn seed_flow_record(flow_id: &str, issue_ref: Option<&str>) -> FlowRecordFixture {
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("valid flow id");
    std::fs::create_dir_all(&run_dir).expect("create flow record directory");
    std::fs::write(
        run_dir.join("status.json"),
        serde_json::to_vec_pretty(&json!({
            "flow_id": flow_id,
            "issue_ref": issue_ref,
            "created_at": "2026-08-13T00:00:00Z",
            "state": "flow_bound",
        }))
        .expect("serialize flow record"),
    )
    .expect("write flow record");
    FlowRecordFixture(run_dir)
}

fn seed_completion_dispatch_owner(server: &MemoryServer, flow_id: &str, dispatch_id: &str) {
    let dispatch_run = server.tachi_home_dir().join("runs").join(dispatch_id);
    std::fs::create_dir_all(&dispatch_run).expect("create dispatch run");
    std::fs::write(
        dispatch_run.join("status.json"),
        json!({ "dispatch_id": dispatch_id, "flow_id": flow_id }).to_string(),
    )
    .expect("write dispatch run status");
    crate::task_lifecycle::mark_task_dispatch(
        flow_id,
        dispatch_id,
        json!({ "task": "pattern evidence completion", "agent": "codex" }),
    )
    .expect("bind dispatch owner to flow");
}

fn memory_params(action: &str) -> TachiMemoryParams {
    TachiMemoryParams {
        action: action.to_string(),
        issue_ref: None,
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
        files: Vec::new(),
        references: Vec::new(),
        project: None,
        project_explicit: false,
        domain: None,
        compact: false,
        proposal_id: None,
        review_status: None,
        notes: None,
        confirm: false,
        state_filter: None,
    }
}

#[tokio::test]
async fn tachi_search_memory_scope_excludes_wiki_rows() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("plain-memory-row");
            memory.path = "/facts/plain".to_string();
            memory.text = "UniqueBoundaryNeedle belongs in plain memory.".to_string();
            memory.summary = "Plain memory row".to_string();
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut wiki = make_entry("wiki-row-should-not-appear");
            wiki.path = "/wiki/general/boundary".to_string();
            wiki.text = "UniqueBoundaryNeedle belongs in wiki.".to_string();
            wiki.summary = "Wiki row".to_string();
            wiki.domain = Some("wiki".to_string());
            wiki.metadata = json!({"wiki": true});
            store.upsert(&wiki).map_err(|e| e.to_string())
        })
        .expect("seed boundary entries");

    let response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniqueBoundaryNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            agent_role: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(response.contains("plain-memory-row"));
    assert!(!response.contains("wiki-row-should-not-appear"));
}

/// #1756 owner adjudication 5281071956: merely retrieving a pattern through
/// the public search facade is not admitted pattern evidence. The search
/// payload stays stable, while the append-only event ledger and projection
/// counters remain untouched.
#[tokio::test]
async fn tachi_search_patterns_scope_is_read_only_pattern_evidence() {
    let server = make_server();
    let memory_id = seed_projected_pattern(
        &server,
        "search-read-only-pattern",
        "UniquePatternReadOnlyNeedle must not become evidence merely because it was retrieved.",
    )
    .await;

    let before = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read pattern before")
        .expect("pattern exists before");
    let before_events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    limit: 500,
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("list events before search");

    let search = || async {
        server
            .tachi_search(Parameters(TachiSearchParams {
                query: "UniquePatternReadOnlyNeedle".to_string(),
                scope: "patterns".to_string(),
                top_k: 5,
                path_prefix: None,
                project: None,
                domain: None,
                file_context: None,
                error_context: None,
                context_symbols: Vec::new(),
                agent_role: None,
                category: None,
                include_archived: false,
                include_training: false,
                enable_rerank: false,
                as_of: None,
            }))
            .await
            .expect("patterns scoped search")
    };
    let first = search().await;
    let second = search().await;

    assert_eq!(
        first, second,
        "removing the write must not alter the payload"
    );
    assert!(first.contains(&memory_id));
    let after = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read pattern after")
        .expect("pattern exists after");
    assert_eq!(
        after.metadata.get("counters"),
        before.metadata.get("counters"),
        "retrieval must not increment pattern feedback counters"
    );
    let after_events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    limit: 500,
                    ..Default::default()
                })
                .map_err(|e| e.to_string())
        })
        .expect("list events after search");
    assert_eq!(
        after_events, before_events,
        "retrieval must not append a feedback/evidence event"
    );
}

#[tokio::test]
async fn tachi_search_patterns_scope_is_explicit() {
    let server = make_server();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("plain-pattern-word-memory");
            memory.path = "/scratch/patterns/plain".to_string();
            memory.text = "UniquePatternScopeNeedle belongs in plain memory.".to_string();
            memory.summary = "Plain pattern word memory".to_string();
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut pattern = make_entry("projected-pattern-row");
            pattern.path = "/user/patterns/agent_os/continuity-first".to_string();
            pattern.text = "UniquePatternScopeNeedle belongs in projected patterns.".to_string();
            pattern.summary = "Projected continuity pattern".to_string();
            pattern.metadata = json!({
                "projection_kind": "pattern",
                "projection_key": "continuity-first",
            });
            store.upsert(&pattern).map_err(|e| e.to_string())
        })
        .expect("seed pattern boundary entries");

    let memory_response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniquePatternScopeNeedle".to_string(),
            scope: "memory".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            agent_role: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("memory scoped search");

    assert!(memory_response.contains("plain-pattern-word-memory"));
    assert!(!memory_response.contains("projected-pattern-row"));

    let pattern_response = server
        .tachi_search(Parameters(TachiSearchParams {
            query: "UniquePatternScopeNeedle".to_string(),
            scope: "patterns".to_string(),
            top_k: 5,
            path_prefix: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            context_symbols: Vec::new(),
            agent_role: None,
            category: None,
            include_archived: false,
            include_training: false,
            enable_rerank: false,
            as_of: None,
        }))
        .await
        .expect("patterns scoped search");

    assert!(pattern_response.contains("projected-pattern-row"));
    assert!(pattern_response.contains("pattern_ref:"));
    assert!(!pattern_response.contains("plain-pattern-word-memory"));
}

#[tokio::test]
async fn tachi_memory_patterns_json_returns_pattern_refs() {
    let server = make_server();
    let memory_id = seed_projected_pattern(
        &server,
        "json-pattern-ref",
        "UniquePatternJsonRefNeedle should expose a machine-readable pattern_ref.",
    )
    .await;

    let response = server
        .tachi_memory(Parameters({
            let mut params = memory_params("search");
            params.query = Some("UniquePatternJsonRefNeedle".to_string());
            params.scope = Some("patterns".to_string());
            params
        }))
        .await
        .expect("pattern search json");
    let parsed: Value = serde_json::from_str(&response).expect("search response json");
    let rows = parsed["sections"][0]["rows"]
        .as_array()
        .expect("pattern rows");
    assert_eq!(rows[0]["id"], json!(memory_id));
    assert_eq!(rows[0]["pattern_ref"]["id"], json!(memory_id));
    assert_eq!(
        rows[0]["pattern_ref"]["projection_key"],
        json!("json-pattern-ref")
    );
}

#[tokio::test]
async fn tachi_complete_records_pattern_hit_from_evidence_ref() {
    let server = make_server();
    let _flow = seed_flow_record("flow_pattern_complete_001", None);
    let dispatch_id = "dispatch_pattern_complete_001";
    seed_completion_dispatch_owner(&server, "flow_pattern_complete_001", dispatch_id);
    let memory_id = seed_projected_pattern(
        &server,
        "complete-pattern-hit",
        "UniquePatternCompleteNeedle should become a hit when task completion cites it.",
    )
    .await;

    let params = TachiCompleteParams {
        task_id: None,
        task: "Use UniquePatternCompleteNeedle while completing a task".to_string(),
        agent: "codex".to_string(),
        outcome: "success".to_string(),
        task_type: Some("fix_request".to_string()),
        profile: None,
        risk: None,
        duration_ms: None,
        skills_used: Vec::new(),
        cost_tokens: None,
        cost_usd: None,
        quality_score: Some(0.9),
        notes: Some("The cited pattern matched the task.".to_string()),
        trajectory: None,
        diff: None,
        worktree: None,
        subagents: Vec::new(),
        feedback_rules_applied: Vec::new(),
        dispatch_id: Some(dispatch_id.to_string()),
        flow_id: Some("flow_pattern_complete_001".to_string()),
        issue_ref: None,
        pr_ref: None,
        evidence_refs: vec![format!("pattern:{memory_id}")],
        tests_run: Vec::new(),
        diff_present: Some(false),
        scope: Some("project".to_string()),
        project: None,
        format: None,
        signatures: Vec::new(),
        rulings: Vec::new(),
        adjudication: None,
        eval_run_ids: Vec::new(),
    };
    let response = server
        .tachi_complete(Parameters(params.clone()))
        .await
        .expect("complete with pattern ref");
    let parsed: Value = serde_json::from_str(&response).expect("complete response json");
    assert_eq!(
        parsed["pipeline"]["pattern_feedback"]["saved_count"],
        json!(1)
    );
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let replay = server
        .tachi_complete(Parameters(params))
        .await
        .expect("replay complete with pattern ref");
    let replay: Value = serde_json::from_str(&replay).expect("replay completion response json");
    assert_eq!(
        replay["pipeline"]["pattern_feedback"]["events"][0]["replayed"],
        json!(true),
        "exact completion replay must return the original pattern-evidence receipt: {replay:#}"
    );

    let events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    adapter: Some("tachi.pattern_evidence.v1".to_string()),
                    limit: 20,
                    ..Default::default()
                })
                .map_err(|error| error.to_string())
        })
        .expect("list pattern evidence events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].session_id, "flow_pattern_complete_001");
    assert_eq!(events[0].authority, memcore::AuthorityLevel::CollectOnly);
    assert_eq!(events[0].effects, vec![memcore::EffectScope::None]);
    assert!(events[0].projection_hints.is_empty());
    assert_eq!(events[0].payload["source"], json!("task_completion"));
    assert_eq!(events[0].payload["pattern_id"], json!(memory_id));
    assert_eq!(events[0].payload["outcome"], json!("hit"));

    let entry = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read pattern after complete")
        .expect("pattern exists after complete");
    assert_eq!(entry.metadata["counters"]["seen"], json!(1));
    assert_eq!(entry.metadata["counters"]["hit"], json!(0));
    assert_eq!(entry.metadata["counters"]["miss"], json!(0));
}

#[tokio::test]
async fn tachi_complete_with_unverified_flow_id_skips_pattern_evidence() {
    let server = make_server();
    let memory_id = seed_projected_pattern(
        &server,
        "complete-pattern-missing-flow",
        "UniquePatternMissingFlowNeedle must not receive fabricated completion evidence.",
    )
    .await;

    let response = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("pattern-complete-no-flow".to_string()),
            task: "Completion without a real flow id".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: None,
            risk: None,
            duration_ms: None,
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.9),
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            eval_run_ids: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            flow_id: Some("flow_unverified_complete_001".to_string()),
            issue_ref: None,
            pr_ref: None,
            evidence_refs: vec![format!("pattern:{memory_id}")],
            tests_run: Vec::new(),
            diff_present: Some(false),
            scope: Some("project".to_string()),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
        }))
        .await
        .expect("complete without flow id");
    let parsed: Value = serde_json::from_str(&response).expect("complete response JSON");

    assert_eq!(
        parsed["pipeline"]["pattern_feedback"]["status"],
        json!("skipped")
    );
    assert_eq!(
        parsed["pipeline"]["pattern_feedback"]["reason"],
        json!("unverified_flow_identity")
    );
    assert_eq!(
        parsed["pipeline"]["pattern_feedback"]["saved_count"],
        json!(0)
    );
}

#[tokio::test]
async fn tachi_complete_without_flow_id_skips_pattern_evidence() {
    let server = make_server();
    let memory_id = seed_projected_pattern(
        &server,
        "complete-pattern-no-flow",
        "UniquePatternNoFlowNeedle must not receive completion evidence.",
    )
    .await;

    let response = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("pattern-complete-missing-flow".to_string()),
            task: "Completion with no flow identity".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            profile: None,
            risk: None,
            duration_ms: None,
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.9),
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            eval_run_ids: Vec::new(),
            feedback_rules_applied: Vec::new(),
            dispatch_id: None,
            evidence_refs: vec![format!("pattern:{memory_id}")],
            flow_id: None,
            issue_ref: None,
            pr_ref: None,
            tests_run: Vec::new(),
            scope: Some("project".to_string()),
            diff_present: Some(false),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
        }))
        .await
        .expect("complete without flow id");
    let parsed: Value = serde_json::from_str(&response).expect("complete response JSON");

    assert_eq!(
        parsed["pipeline"]["pattern_feedback"]["status"],
        json!("skipped")
    );
    assert_eq!(
        parsed["pipeline"]["pattern_feedback"]["reason"],
        json!("missing_real_flow_id")
    );
}

#[tokio::test]
async fn tachi_complete_with_foreign_dispatch_skips_pattern_evidence() {
    let server = make_server();
    let owned_flow = "flow_pattern_complete_owner_001";
    let foreign_flow = "flow_pattern_complete_foreign_001";
    let _owned = seed_flow_record(owned_flow, None);
    let _foreign = seed_flow_record(foreign_flow, None);
    let dispatch_id = "dispatch_pattern_complete_owner_001";
    seed_completion_dispatch_owner(&server, owned_flow, dispatch_id);
    let memory_id = seed_projected_pattern(
        &server,
        "complete-pattern-foreign-dispatch",
        "UniquePatternForeignDispatchNeedle must remain bound to its owning flow.",
    )
    .await;

    let response = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("pattern-complete-foreign-dispatch".to_string()),
            task: "Completion claims a foreign flow".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            dispatch_id: Some(dispatch_id.to_string()),
            flow_id: Some(foreign_flow.to_string()),
            evidence_refs: vec![format!("pattern:{memory_id}")],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            profile: None,
            risk: None,
            duration_ms: None,
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.9),
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            eval_run_ids: Vec::new(),
            feedback_rules_applied: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            tests_run: Vec::new(),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
        }))
        .await
        .expect("complete with foreign flow");
    let parsed: Value = serde_json::from_str(&response).expect("complete response JSON");

    assert_eq!(
        parsed["pipeline"]["pattern_feedback"]["status"],
        json!("skipped")
    );
    assert_eq!(
        parsed["pipeline"]["pattern_feedback"]["reason"],
        json!("unverified_flow_identity")
    );
}

#[tokio::test]
async fn tachi_complete_with_foreign_issue_flow_skips_pattern_evidence() {
    let server = make_server();
    let flow_id = "flow_pattern_complete_foreign_issue_001";
    let dispatch_id = "dispatch_pattern_complete_foreign_issue_001";
    let _flow = seed_flow_record(flow_id, Some("kckylechen1/tachi#1756"));
    seed_completion_dispatch_owner(&server, flow_id, dispatch_id);
    let memory_id = seed_projected_pattern(
        &server,
        "complete-pattern-foreign-issue",
        "UniquePatternForeignIssueNeedle must remain bound to its owning issue.",
    )
    .await;

    let response = server
        .tachi_complete(Parameters(TachiCompleteParams {
            task_id: Some("pattern-complete-foreign-issue".to_string()),
            task: "Completion claims a foreign issue".to_string(),
            agent: "codex".to_string(),
            outcome: "success".to_string(),
            task_type: Some("fix_request".to_string()),
            dispatch_id: Some(dispatch_id.to_string()),
            flow_id: Some(flow_id.to_string()),
            issue_ref: Some("  kckylechen1/tachi#9999  ".to_string()),
            evidence_refs: vec![format!("pattern:{memory_id}")],
            diff_present: Some(false),
            scope: Some("project".to_string()),
            profile: None,
            risk: None,
            duration_ms: None,
            skills_used: Vec::new(),
            cost_tokens: None,
            cost_usd: None,
            quality_score: Some(0.9),
            notes: None,
            trajectory: None,
            diff: None,
            worktree: None,
            subagents: Vec::new(),
            eval_run_ids: Vec::new(),
            feedback_rules_applied: Vec::new(),
            pr_ref: None,
            tests_run: Vec::new(),
            project: None,
            format: None,
            signatures: Vec::new(),
            rulings: Vec::new(),
            adjudication: None,
        }))
        .await
        .expect("complete with foreign issue");
    let parsed: Value = serde_json::from_str(&response).expect("complete response JSON");
    let event_count = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    adapter: Some("tachi.pattern_evidence.v1".to_string()),
                    limit: 20,
                    ..Default::default()
                })
                .map(|events| events.len())
                .map_err(|error| error.to_string())
        })
        .expect("count pattern evidence events");

    assert_eq!(
        json!({
            "status": parsed["pipeline"]["pattern_feedback"]["status"],
            "reason": parsed["pipeline"]["pattern_feedback"]["reason"],
            "saved_count": parsed["pipeline"]["pattern_feedback"]["saved_count"],
            "event_count": event_count,
        }),
        json!({
            "status": "skipped",
            "reason": "unverified_flow_identity",
            "saved_count": 0,
            "event_count": 0,
        }),
        "a valid flow and dispatch cannot admit evidence for a foreign caller issue"
    );
}

#[tokio::test]
async fn close_loop_attaches_pattern_refs_and_records_append_only_hit_evidence() {
    let server = make_server();
    let _flow = seed_flow_record("flow_pattern_close_loop_001", Some("kckylechen1/tachi#250"));
    let memory_id = seed_projected_pattern(
        &server,
        "close-loop-pattern-hit",
        "UniquePatternCloseLoopNeedle should be attached to reviewed closure artifacts.",
    )
    .await;

    let close_params = TachiGhParams {
        action: "close_loop".to_string(),
        issue_ref: Some("kckylechen1/tachi#250".to_string()),
        pr_ref: None,
        doc_paths: vec![],
        spec_paths: vec![],
        related_issues: vec![],
        post_comment: Some(false),
        flow_id: Some("flow_pattern_close_loop_001".to_string()),
        notes: None,
        wiki_title: Some("UniquePatternCloseLoopNeedle closure".to_string()),
        wiki_text: Some("Reviewed closure should cite UniquePatternCloseLoopNeedle.".to_string()),
        wiki_path: None,
        wiki_topic: Some("pattern-close-loop".to_string()),
        wiki_summary: Some("UniquePatternCloseLoopNeedle closure".to_string()),
        wiki_category: None,
        wiki_keywords: Vec::new(),
        wiki_entities: Vec::new(),
        wiki_importance: Some(0.8),
        wiki_scope: None,
        wiki_domain: None,
        project: None,
        force: true,
        ..Default::default()
    };
    let response = server
        .tachi_gh(Parameters(close_params.clone()))
        .await
        .expect("close_loop with pattern refs");
    let parsed: Value = serde_json::from_str(&response).expect("close_loop response json");
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["wiki"]["pattern_refs"][0]["id"], json!(memory_id));
    assert_eq!(parsed["pattern_feedback"]["saved_count"], json!(1));

    let events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    adapter: Some("tachi.pattern_evidence.v1".to_string()),
                    limit: 20,
                    ..Default::default()
                })
                .map_err(|error| error.to_string())
        })
        .expect("list pattern evidence events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].session_id, "flow_pattern_close_loop_001");
    assert_eq!(events[0].authority, memcore::AuthorityLevel::CollectOnly);
    assert_eq!(events[0].effects, vec![memcore::EffectScope::None]);
    assert!(events[0].projection_hints.is_empty());
    assert_eq!(events[0].payload["source"], json!("workflow_closure"));
    assert_eq!(events[0].payload["pattern_id"], json!(memory_id));
    assert_eq!(events[0].payload["outcome"], json!("hit"));

    let replay_response = server
        .tachi_gh(Parameters(close_params))
        .await
        .expect("replay identical close_loop");
    let replay: Value =
        serde_json::from_str(&replay_response).expect("replay close_loop response json");
    assert_eq!(replay["pattern_feedback"]["saved_count"], json!(1));
    assert_eq!(
        replay["pattern_feedback"]["events"][0]["replayed"],
        json!(true)
    );
    let replayed_events = server
        .with_global_store_read(|store| {
            store
                .list_tachi_events(&memcore::TachiEventQuery {
                    adapter: Some("tachi.pattern_evidence.v1".to_string()),
                    limit: 20,
                    ..Default::default()
                })
                .map_err(|error| error.to_string())
        })
        .expect("list replayed workflow evidence events");
    assert_eq!(
        replayed_events.len(),
        1,
        "exact replay must not append another event"
    );

    let entry = server
        .with_global_store_read(|store| store.get(&memory_id).map_err(|e| e.to_string()))
        .expect("read pattern after close_loop")
        .expect("pattern exists after close_loop");
    assert_eq!(entry.metadata["counters"]["seen"], json!(1));
    assert_eq!(entry.metadata["counters"]["hit"], json!(0));
    assert_eq!(entry.metadata["counters"]["miss"], json!(0));
}

#[tokio::test]
async fn close_loop_with_unverified_flow_id_skips_pattern_evidence() {
    let server = make_server();
    let memory_id = seed_projected_pattern(
        &server,
        "close-loop-pattern-missing-flow",
        "UniquePatternCloseLoopMissingFlowNeedle must not receive fabricated closure evidence.",
    )
    .await;

    let response = server
        .tachi_gh(Parameters(TachiGhParams {
            action: "close_loop".to_string(),
            issue_ref: Some("kckylechen1/tachi#251".to_string()),
            post_comment: Some(false),
            flow_id: Some("flow_unverified_close_loop_001".to_string()),
            wiki_title: Some("UniquePatternCloseLoopMissingFlowNeedle closure".to_string()),
            wiki_text: Some(
                "Reviewed closure cites UniquePatternCloseLoopMissingFlowNeedle.".to_string(),
            ),
            wiki_topic: Some("pattern-close-loop-missing-flow".to_string()),
            wiki_summary: Some("UniquePatternCloseLoopMissingFlowNeedle closure".to_string()),
            wiki_importance: Some(0.8),
            project: None,
            force: true,
            ..Default::default()
        }))
        .await
        .expect("close_loop without flow id");
    let parsed: Value = serde_json::from_str(&response).expect("close_loop response json");

    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["wiki"]["pattern_refs"][0]["id"], json!(memory_id));
    assert_eq!(parsed["pattern_feedback"]["status"], json!("skipped"));
    assert_eq!(
        parsed["pattern_feedback"]["reason"],
        json!("unverified_flow_identity")
    );
    assert_eq!(parsed["pattern_feedback"]["saved_count"], json!(0));
}

#[tokio::test]
async fn close_loop_with_foreign_issue_flow_skips_pattern_evidence() {
    let server = make_server();
    let flow_id = "flow_pattern_close_loop_foreign_issue_001";
    let _flow = seed_flow_record(flow_id, Some("kckylechen1/tachi#253"));
    let memory_id = seed_projected_pattern(
        &server,
        "close-loop-pattern-foreign-issue",
        "UniquePatternForeignIssueNeedle must remain bound to its owning issue.",
    )
    .await;

    let response = server
        .tachi_gh(Parameters(TachiGhParams {
            action: "close_loop".to_string(),
            issue_ref: Some("kckylechen1/tachi#254".to_string()),
            post_comment: Some(false),
            flow_id: Some(flow_id.to_string()),
            wiki_title: Some("UniquePatternForeignIssueNeedle closure".to_string()),
            wiki_text: Some("Reviewed closure cites UniquePatternForeignIssueNeedle.".to_string()),
            wiki_topic: Some("pattern-close-loop-foreign-issue".to_string()),
            wiki_summary: Some("UniquePatternForeignIssueNeedle closure".to_string()),
            wiki_importance: Some(0.8),
            force: true,
            ..Default::default()
        }))
        .await
        .expect("close_loop with foreign issue flow");
    let parsed: Value = serde_json::from_str(&response).expect("close_loop response json");

    assert_eq!(parsed["wiki"]["pattern_refs"][0]["id"], json!(memory_id));
    assert_eq!(parsed["pattern_feedback"]["status"], json!("skipped"));
    assert_eq!(
        parsed["pattern_feedback"]["reason"],
        json!("unverified_flow_identity")
    );
}

#[tokio::test]
async fn close_loop_without_flow_id_skips_pattern_evidence() {
    let server = make_server();
    let memory_id = seed_projected_pattern(
        &server,
        "close-loop-pattern-no-flow",
        "UniquePatternCloseLoopNoFlowNeedle must not receive closure evidence.",
    )
    .await;

    let response = server
        .tachi_gh(Parameters(TachiGhParams {
            action: "close_loop".to_string(),
            issue_ref: Some("kckylechen1/tachi#252".to_string()),
            post_comment: Some(false),
            flow_id: None,
            wiki_title: Some("UniquePatternCloseLoopNoFlowNeedle closure".to_string()),
            wiki_text: Some(
                "Reviewed closure cites UniquePatternCloseLoopNoFlowNeedle.".to_string(),
            ),
            wiki_topic: Some("pattern-close-loop-no-flow".to_string()),
            wiki_summary: Some("UniquePatternCloseLoopNoFlowNeedle closure".to_string()),
            wiki_importance: Some(0.8),
            force: true,
            ..Default::default()
        }))
        .await
        .expect("close_loop without flow id");
    let parsed: Value = serde_json::from_str(&response).expect("close_loop response json");

    assert_eq!(parsed["wiki"]["pattern_refs"][0]["id"], json!(memory_id));
    assert_eq!(parsed["pattern_feedback"]["status"], json!("skipped"));
    assert_eq!(
        parsed["pattern_feedback"]["reason"],
        json!("missing_real_flow_id")
    );
}
