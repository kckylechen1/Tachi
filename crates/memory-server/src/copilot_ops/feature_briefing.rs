use super::*;

pub(crate) async fn handle_tachi_task_brief(
    server: &MemoryServer,
    params: TaskBriefParams,
) -> Result<String, String> {
    let top_k = crate::clamp_facade_top_k(params.top_k);
    let mut wiki_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.task.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some("/wiki".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await?;
    crate::wiki_ops::filter_user_facing_wiki_rows(&mut wiki_rows);
    let memory_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.task.clone(),
            query_vec: None,
            top_k,
            path_prefix: params.path_prefix.clone(),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await?;
    let skills = recommend_skills_light(server, &params.task, 5).unwrap_or_default();
    let debug_checklist = build_debug_checklist(&wiki_rows);
    let routing = build_task_brief_routing(&params.task, &skills);

    let route_rec =
        build_route_recommendation(server, &params.task, params.project.as_deref()).await;
    let intent = routing.intent;
    let selected_sops = routing.selected_sops;
    let tool_plan = routing.tool_plan;

    serde_json::to_string(&json!({
        "status": "ok",
        "task": params.task,
        "agent_id": params.agent_id,
        "project": params.project,
        "wiki_hits": compact_rows(wiki_rows, top_k),
        "memory_hits": compact_rows(memory_rows, top_k),
        "intent": intent,
        "selected_sops": selected_sops,
        "tool_plan": tool_plan,
        "recommended_skills": skills,
        "debug_checklist": debug_checklist,
        "route_recommendation": route_rec,
        "suggested_next_tools": [
            "tachi_wiki(action='search')",
            "tachi_skill(action='discover')",
            "tachi_task(action='plan')",
            "tachi_task(action='board')"
        ],
    }))
    .map_err(|e| format!("serialize task_brief: {e}"))
}

pub(crate) async fn handle_tachi_feature_briefing(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let top_k = if params.compact.unwrap_or(false) {
        params.top_k.unwrap_or(4).clamp(1, 4)
    } else {
        crate::clamp_facade_top_k(params.top_k.unwrap_or(6))
    };
    let query = feature_briefing_query(params);
    let board = feature_board(server, params, top_k).await;
    let project_work_record = project_work_records(params);
    let canonical_docs = canonical_doc_refs(params);
    let run_artifacts = feature_run_artifacts(params.flow_id.as_deref())?;

    let wiki_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some("/wiki".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: None,
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
        },
        false,
    )
    .await
    .unwrap_or_default();

    let memory_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k,
            path_prefix: params.path_prefix.clone(),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
        },
        !params.include_global,
    )
    .await
    .unwrap_or_default();

    let eval_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k: top_k.min(5),
            path_prefix: Some("/eval".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
        },
        !params.include_global,
    )
    .await
    .unwrap_or_default();

    let skills = recommend_skills_light(server, &query, 5).unwrap_or_default();
    let routing = build_task_brief_routing(&query, &skills);
    let route_recommendation = feature_dispatch_recommendation(server, params, &query);
    let current_stage = infer_feature_stage(&run_artifacts, &board);
    let guide_hits = feature_guide_hits(
        server,
        params,
        &query,
        &current_stage,
        &route_recommendation,
        top_k,
    );
    let feedback_profile = params.profile.as_deref().or_else(|| {
        route_recommendation
            .get("recommended_profile")
            .and_then(Value::as_str)
    });
    let feedback_stage = params
        .stage
        .clone()
        .unwrap_or_else(|| current_stage.clone());
    let feedback_rules = crate::feedback_rule_ops::applicable_feedback_rules(
        server,
        crate::feedback_rule_ops::FeedbackRuleQuery {
            task: query.clone(),
            task_type: params.task_type.clone(),
            profile: feedback_profile.map(str::to_string),
            stage: Some(feedback_stage),
            keywords: feature_needles(params),
            project: params.project.clone(),
        },
    )
    .await;
    let feedback_rules_trace = crate::feedback_rule_ops::feedback_rules_trace(&feedback_rules);
    let suggested_dispatch = suggested_feature_dispatch(params, &query, &route_recommendation);
    let relevant_profiles = relevant_feature_profiles(&route_recommendation);
    let next_action = feature_next_action(&canonical_docs, &run_artifacts, &board, &memory_rows);
    let open_loops = crate::shell_ops::scan_open_loops(8);
    let wiki_hits = compact_layer_rows(wiki_rows, top_k, Some("wiki"), Some("advisory"));
    let memory_fragments = compact_layer_rows(memory_rows, top_k, Some("memory"), Some("context"));
    let eval_evidence = compact_layer_rows(eval_rows, top_k.min(5), Some("eval"), Some("evidence"));
    let doc_index = build_feature_doc_index(
        &project_work_record,
        &canonical_docs,
        &wiki_hits,
        &guide_hits,
        &feedback_rules_trace,
        &eval_evidence,
        &run_artifacts,
    );
    let kind = if params.action.eq_ignore_ascii_case("doc_index") {
        "doc_index"
    } else {
        "feature_briefing"
    };
    let response = json!({
        "status": "ok",
        "kind": kind,
        "objective": params.task.clone().unwrap_or_else(|| query.clone()),
        "scope": {
            "project": params.project,
            "flow_id": params.flow_id,
            "issue_ref": params.issue_ref,
            "pr_ref": params.pr_ref,
            "cwd": params.cwd,
            "include_global": params.include_global,
        },
        "current_stage": current_stage,
        "project_work_record": project_work_record,
        "board_state": board,
        "canonical_docs": canonical_docs,
        "run_artifacts": run_artifacts,
        "guide_sop": {
            "intent": routing.intent,
            "selected_sops": routing.selected_sops,
            "tool_plan": routing.tool_plan,
            "recommended_skills": skills,
        },
        "route_recommendation": route_recommendation,
        "relevant_profiles": relevant_profiles,
        "suggested_dispatch": suggested_dispatch,
        "guide_hits": guide_hits,
        "wiki_hits": wiki_hits,
        "feedback_rules": feedback_rules_trace,
        "memory_fragments": memory_fragments,
        "eval_evidence": eval_evidence,
        "doc_index": doc_index,
        "next_action": next_action,
        "open_loops": open_loops,
        "layering": {
            "project_work_record": "GitHub issues/PRs and linked flow state; source of truth for active work",
            "docs": "canonical repo specs/design docs; source of truth for feature/API truth",
            "wiki": "project-specific durable decisions and lessons; advisory unless promoted back to docs/issues",
            "guide": "global workflow/SOP and skill loadout guidance; playbook authority",
            "feedback_rules": "behavior patches that shape future agent prompts",
            "eval": "verification and reviewer usefulness evidence",
            "runtime_artifacts": "arena/dispatch/run files; runtime state, not canonical product truth",
            "principle": "Project facts first. Global playbook second. Feedback rules and eval pitfalls as behavior patches."
        },
    });

    if crate::facade_memory_ops::wants_json(params.format.as_deref()) {
        serde_json::to_string(&response).map_err(|e| format!("serialize feature briefing: {e}"))
    } else {
        Ok(format_feature_briefing_markdown(&response))
    }
}

pub(super) fn project_work_records(params: &TachiTaskParams) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    push_project_work_record(
        &mut out,
        &mut seen,
        "github_issue",
        params.issue_ref.as_deref(),
    );
    push_project_work_record(&mut out, &mut seen, "github_pr", params.pr_ref.as_deref());
    out
}

pub(super) fn push_project_work_record(
    out: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    kind: &str,
    raw_ref: Option<&str>,
) {
    let Some(reference) = raw_ref.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if !seen.insert(format!("{kind}:{reference}")) {
        return;
    }
    out.push(json!({
        "kind": kind,
        "ref": reference,
        "layer": "github_ref",
        "authority": "project_work_record",
        "source_of_truth": true,
        "status": "ref_only",
        "retrieval": "Call tachi_task(action='intake') for issue snapshots or tachi_task(action='link_pr'/'pr_status') for PR state.",
    }));
}

pub(super) fn build_feature_doc_index(
    project_work_record: &[Value],
    canonical_docs: &[Value],
    wiki_hits: &[Value],
    guide_hits: &[Value],
    feedback_rules: &Value,
    eval_evidence: &[Value],
    run_artifacts: &[Value],
) -> Value {
    let feedback_items = feedback_rules
        .get("rules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "authority_order": [
            "project_work_record",
            "canonical",
            "project_wiki",
            "global_guide",
            "feedback_rule",
            "eval",
            "runtime_artifact"
        ],
        "groups": [
            doc_index_group(
                "project_work_record",
                "github_ref",
                "project_work_record",
                "GitHub Issues/PRs remain the source of truth for active project work.",
                project_work_record,
            ),
            doc_index_group(
                "canonical_docs",
                "repo_doc_ref",
                "canonical",
                "Repo docs/specs define accepted design and API truth.",
                canonical_docs,
            ),
            doc_index_group(
                "project_wiki",
                "wiki",
                "advisory",
                "Project decisions and lessons are durable but do not override GitHub or repo docs.",
                wiki_hits,
            ),
            doc_index_group(
                "global_guide",
                "guide",
                "playbook",
                "Global guide entries apply by task_type/profile/stage as reusable workflow playbooks.",
                guide_hits,
            ),
            doc_index_group(
                "feedback_rules",
                "feedback_rule",
                "behavior_patch",
                "Feedback rules patch future agent behavior; they are not project facts.",
                &feedback_items,
            ),
            doc_index_group(
                "eval_evidence",
                "eval",
                "evidence",
                "Eval rows and reviewer findings are evidence for routing and verification.",
                eval_evidence,
            ),
            doc_index_group(
                "runtime_artifacts",
                "runtime_artifact",
                "runtime_state",
                "Arena/dispatch/run artifacts describe execution state and handoffs.",
                run_artifacts,
            ),
        ],
    })
}

pub(super) fn doc_index_group(
    name: &str,
    layer: &str,
    authority: &str,
    rule: &str,
    items: &[Value],
) -> Value {
    json!({
        "name": name,
        "layer": layer,
        "authority": authority,
        "rule": rule,
        "count": items.len(),
        "items": items,
    })
}

pub(super) fn feature_briefing_query(params: &TachiTaskParams) -> String {
    params
        .task
        .as_deref()
        .or(params.issue_ref.as_deref())
        .or(params.pr_ref.as_deref())
        .or(params.flow_id.as_deref())
        .unwrap_or("current feature handoff")
        .to_string()
}

pub(super) fn canonical_doc_refs(params: &TachiTaskParams) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if let Ok((flow_docs, flow_specs)) =
        crate::task_lifecycle::flow_status_doc_refs(params.flow_id.as_deref())
    {
        for path in flow_specs {
            push_doc_ref(
                &mut out,
                &mut seen,
                "flow_spec",
                &path,
                params.cwd.as_deref(),
            );
        }
        for path in flow_docs {
            push_doc_ref(
                &mut out,
                &mut seen,
                "flow_doc",
                &path,
                params.cwd.as_deref(),
            );
        }
    }
    for path in params
        .spec_paths
        .iter()
        .map(|path| ("spec", path))
        .chain(params.doc_paths.iter().map(|path| ("doc", path)))
    {
        push_doc_ref(&mut out, &mut seen, path.0, path.1, params.cwd.as_deref());
    }
    if let Some(task) = params.task.as_deref() {
        for path in extract_markdown_paths(task) {
            push_doc_ref(
                &mut out,
                &mut seen,
                "mentioned_doc",
                &path,
                params.cwd.as_deref(),
            );
        }
    }
    out
}

pub(super) fn push_doc_ref(
    out: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    kind: &str,
    raw_path: &str,
    cwd: Option<&str>,
) {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() || !seen.insert(raw_path.to_string()) {
        return;
    }
    let resolved = resolve_workspace_path(raw_path, cwd);
    out.push(json!({
        "kind": kind,
        "path": raw_path,
        "exists": resolved.as_ref().is_some_and(|path| path.exists()),
        "resolved_path": resolved.map(|path| path.to_string_lossy().to_string()),
        "layer": "repo_doc_ref",
        "authority": "canonical",
        "source_of_truth": true,
    }));
}

pub(super) fn extract_markdown_paths(text: &str) -> Vec<String> {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ')' | '(' | '[' | ']'))
        .map(|token| token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | ':' | ';')))
        .filter(|token| {
            token.ends_with(".md") && (token.starts_with("docs/") || token.contains("/docs/"))
        })
        .map(str::to_string)
        .collect()
}

pub(super) fn resolve_workspace_path(raw_path: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let path = PathBuf::from(raw_path);
    if path.is_absolute() {
        return Some(path);
    }
    if let Some(cwd) = cwd {
        let cwd = Path::new(cwd);
        for ancestor in cwd.ancestors() {
            let candidate = ancestor.join(raw_path);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        return Some(cwd.join(raw_path));
    }
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join(raw_path);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    Some(cwd.join(raw_path))
}

pub(super) fn feature_dispatch_recommendation(
    server: &MemoryServer,
    params: &TachiTaskParams,
    query: &str,
) -> Value {
    let mut file_paths = params.doc_paths.clone();
    file_paths.extend(params.spec_paths.clone());
    match crate::dispatch_profile::handle_dispatch_recommendation(
        server,
        query,
        params.risk.as_deref(),
        params.limit.unwrap_or(500),
        &file_paths,
    ) {
        Ok(raw) => serde_json::from_str(&raw)
            .unwrap_or_else(|err| json!({"available": false, "error": err.to_string()})),
        Err(err) => json!({"available": false, "error": err}),
    }
}

pub(super) fn suggested_feature_dispatch(
    params: &TachiTaskParams,
    query: &str,
    recommendation: &Value,
) -> Value {
    let profile = params.profile.as_deref().map(str::to_string).or_else(|| {
        recommendation
            .get("recommended_profile")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let mut arguments = serde_json::Map::new();
    arguments.insert("action".to_string(), json!("dispatch"));
    arguments.insert(
        "task".to_string(),
        json!(params.task.as_deref().unwrap_or(query)),
    );
    if let Some(profile) = profile {
        arguments.insert("profile".to_string(), json!(profile));
    }
    if let Some(cwd) = params
        .cwd
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("cwd".to_string(), json!(cwd));
    }
    if let Some(project) = params
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("project".to_string(), json!(project));
    }
    if let Some(issue_ref) = params
        .issue_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("issue_ref".to_string(), json!(issue_ref));
    }
    if let Some(pr_ref) = params
        .pr_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("pr_ref".to_string(), json!(pr_ref));
    }
    if let Some(flow_id) = params
        .flow_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("flow_id".to_string(), json!(flow_id));
    }
    if let Some(risk) = params
        .risk
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("risk".to_string(), json!(risk));
    }
    if let Some(true) = params.auto_capability_bundle {
        arguments.insert("auto_capability_bundle".to_string(), json!(true));
    }
    json!({
        "tool": "tachi_task",
        "arguments": arguments,
        "evidence_required": recommendation
            .get("evidence_required")
            .cloned()
            .unwrap_or(Value::Null),
        "fallback_chain": recommendation
            .get("fallback_chain")
            .cloned()
            .unwrap_or_else(|| json!([])),
    })
}

pub(super) fn relevant_feature_profiles(recommendation: &Value) -> Vec<Value> {
    recommendation
        .get("candidates")
        .and_then(Value::as_array)
        .map(|candidates| {
            candidates
                .iter()
                .take(4)
                .map(|candidate| {
                    json!({
                        "profile": candidate.get("profile").cloned().unwrap_or(Value::Null),
                        "agent": candidate.get("agent").cloned().unwrap_or(Value::Null),
                        "role": candidate.get("role").cloned().unwrap_or(Value::Null),
                        "score": candidate.get("score").cloned().unwrap_or(Value::Null),
                        "reason": candidate.get("reasons").cloned().unwrap_or_else(|| json!([])),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn feature_run_artifacts(flow_id: Option<&str>) -> Result<Vec<Value>, String> {
    let Some(flow_id) = flow_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id)?;
    let mut out = vec![json!({
        "kind": "run_dir",
        "path": run_dir.to_string_lossy(),
        "exists": run_dir.exists(),
        "layer": "runtime_artifact",
        "authority": "runtime_state",
    })];
    for name in [
        "instruction.md",
        "plan.md",
        "result.md",
        "validation.md",
        "status.json",
        "close_loop.json",
        "events.jsonl",
        "progress.jsonl",
        "trajectory.jsonl",
    ] {
        let path = run_dir.join(name);
        out.push(json!({
            "kind": "run_artifact",
            "path": path.to_string_lossy(),
            "exists": path.exists(),
            "layer": "runtime_artifact",
            "authority": "runtime_state",
        }));
    }
    Ok(out)
}

pub(super) async fn feature_board(
    server: &MemoryServer,
    params: &TachiTaskParams,
    top_k: usize,
) -> serde_json::Value {
    let raw = feature_board_raw(server, params, params.flow_id.clone(), top_k).await;
    let mut used_fallback = false;
    let mut board: Value = match raw {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| json!({})),
        Err(_) => return json!({"available": false}),
    };
    if params
        .flow_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
        && board
            .get("tasks")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        if let Ok(raw) = feature_board_raw(server, params, None, top_k).await {
            board = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
            used_fallback = true;
        }
    }
    let Some(tasks) = board.get("tasks").and_then(Value::as_array).cloned() else {
        return board;
    };
    let needles = feature_needles(params);
    if needles.is_empty() {
        board["tasks"] = Value::Array(tasks.into_iter().take(top_k).collect());
        board["count"] = json!(board["tasks"].as_array().map(Vec::len).unwrap_or(0));
        return board;
    }
    let filtered = tasks
        .into_iter()
        .filter(|task| value_contains_any(task, &needles))
        .take(top_k)
        .collect::<Vec<_>>();
    board["tasks"] = Value::Array(filtered);
    board["count"] = json!(board["tasks"].as_array().map(Vec::len).unwrap_or(0));
    if used_fallback {
        board["flow_id"] = json!(params.flow_id);
        board["flow_filter_fallback"] = json!("needle_scan");
    }
    board
}

pub(super) async fn feature_board_raw(
    server: &MemoryServer,
    params: &TachiTaskParams,
    flow_id: Option<String>,
    top_k: usize,
) -> Result<String, String> {
    crate::dispatch_ops::handle_tachi_board(
        server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(top_k.max(10)),
            project: params.project.clone(),
            flow_id,
        },
    )
    .await
}

pub(super) fn feature_needles(params: &TachiTaskParams) -> Vec<String> {
    [
        params.flow_id.as_deref(),
        params.issue_ref.as_deref(),
        params.pr_ref.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_ascii_lowercase)
    .collect()
}

pub(super) fn value_contains_any(value: &Value, needles: &[String]) -> bool {
    if needles.is_empty() {
        return true;
    }
    [
        "dispatch_id",
        "summary",
        "run_dir",
        "eval_id",
        "agent",
        "state",
        "source",
    ]
    .into_iter()
    .filter_map(|field| value.get(field))
    .any(|field_value| field_value_contains_any(field_value, needles))
}

pub(super) fn field_value_contains_any(value: &Value, needles: &[String]) -> bool {
    match value {
        Value::String(text) => {
            let text = text.to_ascii_lowercase();
            needles.iter().any(|needle| text.contains(needle))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| field_value_contains_any(value, needles)),
        Value::Object(map) => map
            .values()
            .any(|value| field_value_contains_any(value, needles)),
        _ => false,
    }
}

pub(super) fn infer_feature_stage(run_artifacts: &[Value], board: &Value) -> String {
    if let Some(task) = board
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|tasks| tasks.first())
    {
        if let Some(state) = task.get("state").and_then(Value::as_str) {
            return state.to_string();
        }
    }
    let has_result = run_artifacts.iter().any(|artifact| {
        artifact
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("result.md"))
            && artifact.get("exists").and_then(Value::as_bool) == Some(true)
    });
    if has_result {
        "result_available".to_string()
    } else if !run_artifacts.is_empty() {
        "flow_started".to_string()
    } else {
        "intake".to_string()
    }
}

pub(super) fn feature_next_action(
    canonical_docs: &[Value],
    run_artifacts: &[Value],
    board: &Value,
    memory_rows: &[Value],
) -> String {
    if canonical_docs.is_empty() {
        return "Attach or create a canonical docs/spec reference before treating memory as feature truth.".to_string();
    }
    if board
        .get("tasks")
        .and_then(Value::as_array)
        .is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| task.get("state").and_then(Value::as_str) == Some("TASK_STATE_WORKING"))
        })
    {
        return "Poll tachi_task(action='board') and collect the active worker result before dispatching more work.".to_string();
    }
    if run_artifacts.iter().any(|artifact| {
        artifact
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("instruction.md"))
            && artifact.get("exists").and_then(Value::as_bool) == Some(true)
    }) && !run_artifacts.iter().any(|artifact| {
        artifact
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("result.md"))
            && artifact.get("exists").and_then(Value::as_bool) == Some(true)
    }) {
        return "Use the flow instruction packet as the worker handoff source and dispatch a bounded slice.".to_string();
    }
    if memory_rows.is_empty() {
        return "Start with tachi_task(action='plan') or save a checkpoint after the next concrete decision.".to_string();
    }
    "Run tachi_task(action='recommend') for the next worker profile, then dispatch or review with explicit verification.".to_string()
}

pub(super) fn format_feature_briefing_markdown(value: &Value) -> String {
    let mut out = Vec::new();
    out.push("# Feature Briefing".to_string());
    out.push(format!(
        "\n## Objective\n{}",
        value
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or("(unspecified)")
    ));
    out.push(format!(
        "\n## Current Stage\n{}",
        value
            .get("current_stage")
            .and_then(Value::as_str)
            .unwrap_or("intake")
    ));
    out.push(markdown_section(
        "Project Work Record",
        value.get("project_work_record").and_then(Value::as_array),
        "No GitHub issue/PR reference attached.",
    ));
    out.push(markdown_section(
        "Canonical Docs / Specs",
        value.get("canonical_docs").and_then(Value::as_array),
        "No canonical docs/specs attached.",
    ));
    out.push(markdown_section(
        "Run Artifacts",
        value.get("run_artifacts").and_then(Value::as_array),
        "No flow run artifacts attached.",
    ));
    out.push(markdown_section(
        "Board State",
        value
            .get("board_state")
            .and_then(|board| board.get("tasks"))
            .and_then(Value::as_array),
        "No matching board tasks.",
    ));
    let mut guide_rows = Vec::new();
    if let Some(hits) = value.get("guide_hits").and_then(Value::as_array) {
        guide_rows.extend(hits.iter().cloned());
    }
    if let Some(sops) = value
        .get("guide_sop")
        .and_then(|guide| guide.get("selected_sops"))
        .and_then(Value::as_array)
    {
        guide_rows.extend(sops.iter().cloned());
    }
    out.push(markdown_section(
        "Guide / SOP",
        if guide_rows.is_empty() {
            None
        } else {
            Some(&guide_rows)
        },
        "No SOP selected.",
    ));
    out.push(markdown_section(
        "Feedback Rules",
        value
            .get("feedback_rules")
            .and_then(|rules| rules.get("rules"))
            .and_then(Value::as_array),
        "No applicable feedback rules.",
    ));
    out.push(markdown_dispatch_recommendation(value));
    out.push(markdown_section(
        "Relevant Skills / Profiles",
        value.get("relevant_profiles").and_then(Value::as_array),
        "No dispatch profiles ranked.",
    ));
    out.push(markdown_section(
        "Wiki Decisions / Lessons",
        value.get("wiki_hits").and_then(Value::as_array),
        "No wiki hits.",
    ));
    out.push(markdown_section(
        "Memory Fragments / Checkpoints",
        value.get("memory_fragments").and_then(Value::as_array),
        "No project-scoped memory fragments.",
    ));
    out.push(markdown_section(
        "Eval Evidence",
        value.get("eval_evidence").and_then(Value::as_array),
        "No matching eval evidence.",
    ));
    if let Some(loops) = value
        .get("open_loops")
        .and_then(Value::as_array)
        .filter(|loops| !loops.is_empty())
    {
        out.push("\n## ⚠️ Open Loops (closure debt)".to_string());
        for item in loops {
            let detail = item.get("detail").and_then(Value::as_str).unwrap_or("");
            let action = item.get("action").and_then(Value::as_str).unwrap_or("");
            out.push(format!("- {detail} → `{action}`"));
        }
    }
    out.push(format!(
        "\n## Next Action\n{}",
        value
            .get("next_action")
            .and_then(Value::as_str)
            .unwrap_or("Continue from the canonical docs/specs.")
    ));
    out.join("\n")
}

pub(super) fn markdown_dispatch_recommendation(value: &Value) -> String {
    let mut out = vec!["\n## Recommended Dispatch".to_string()];
    let recommendation = value.get("route_recommendation").unwrap_or(&Value::Null);
    let suggested = value.get("suggested_dispatch").unwrap_or(&Value::Null);
    let Some(profile) = recommendation
        .get("recommended_profile")
        .and_then(Value::as_str)
    else {
        out.push("- No dispatch profile recommendation available.".to_string());
        return out.join("\n");
    };
    let agent = recommendation
        .get("recommended_agent")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let risk = recommendation
        .get("risk")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    out.push(format!(
        "- Profile: `{profile}` via `{agent}` (risk={risk})"
    ));
    if let Some(reason) = recommendation.get("reason").and_then(Value::as_array) {
        let reason = reason
            .iter()
            .take(3)
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if !reason.is_empty() {
            out.push(format!("- Why: {}", reason.join("; ")));
        }
    }
    if let Some(arguments) = suggested.get("arguments") {
        out.push(format!("- Dispatch args: `{}`", compact_json(arguments)));
    }
    out.join("\n")
}

pub(super) fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

pub(super) fn markdown_section(title: &str, rows: Option<&Vec<Value>>, empty: &str) -> String {
    let mut out = vec![format!("\n## {title}")];
    let Some(rows) = rows.filter(|rows| !rows.is_empty()) else {
        out.push(format!("- {empty}"));
        return out.join("\n");
    };
    for row in rows.iter().take(8) {
        out.push(format!("- {}", compact_value_line(row)));
    }
    out.join("\n")
}

pub(super) fn compact_value_line(value: &Value) -> String {
    if let Some(path) = value.get("path").and_then(Value::as_str) {
        let summary = value
            .get("summary")
            .or_else(|| value.get("kind"))
            .or_else(|| value.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if summary.is_empty() {
            return format!("`{path}`");
        }
        return format!("`{path}` - {summary}");
    }
    if let Some(summary) = value.get("summary").and_then(Value::as_str) {
        return summary.to_string();
    }
    if let Some(id) = value.get("dispatch_id").and_then(Value::as_str) {
        let state = value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let summary = value.get("summary").and_then(Value::as_str).unwrap_or("");
        return format!("`{id}` [{state}] {summary}");
    }
    value.to_string()
}
