use super::super::*;
use super::board::{feature_board, feature_needles, feature_run_artifacts};
use super::dispatch::{
    feature_dispatch_recommendation, relevant_feature_profiles, suggested_feature_dispatch,
};
use super::docs::{
    build_feature_doc_index, canonical_doc_refs, feature_briefing_query, project_work_records,
};
use super::markdown::format_feature_briefing_markdown;
use super::stage::{feature_next_action, infer_feature_stage};

fn doc_index_item_ids(doc_index: &Value) -> std::collections::HashSet<String> {
    doc_index
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|group| {
            group
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn filter_wiki_hits_not_in_doc_index(wiki_hits: &[Value], doc_index: &Value) -> Vec<Value> {
    let indexed_ids = doc_index_item_ids(doc_index);
    wiki_hits
        .iter()
        .filter(|hit| {
            hit.get("id")
                .and_then(Value::as_str)
                .is_none_or(|id| !indexed_ids.contains(id))
        })
        .cloned()
        .collect()
}

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
            context_symbols: Vec::new(),
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
            context_symbols: Vec::new(),
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
    let project_work_record = project_work_records(params);
    let canonical_docs = canonical_doc_refs(params);
    let run_artifacts = feature_run_artifacts(params.flow_id.as_deref())?;

    let (board, wiki_rows, memory_rows, eval_rows) = tokio::join!(
        feature_board(server, params, top_k),
        search_memory_rows(
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
                context_symbols: Vec::new(),
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
        ),
        search_memory_rows(
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
                context_symbols: Vec::new(),
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
        ),
        search_memory_rows(
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
                context_symbols: Vec::new(),
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
        ),
    );
    let wiki_rows = wiki_rows.unwrap_or_default();
    let memory_rows = memory_rows.unwrap_or_default();
    let eval_rows = eval_rows.unwrap_or_default();

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
    let next_action = feature_next_action(
        params,
        &canonical_docs,
        &run_artifacts,
        &board,
        &memory_rows,
    );
    let open_loops = crate::shell_ops::scan_open_loops(8);
    let issue_freshness = crate::gh_ops::briefing_freshness_queues(server, 5);
    // #1001: presence 工位表 + advisory collision warnings. Read-only,
    // failure-safe (empty board on any storage error) — never fails briefing.
    // Single call point (Scope item 3) — see `claims_ops::presence_briefing_section`.
    let presence_section =
        crate::claims_ops::presence_briefing_section(server, params.issue_ref.as_deref());
    let presence_board = presence_section["board"].clone();
    let presence_warnings = presence_section["warnings"].clone();
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
    let top_level_wiki_hits = filter_wiki_hits_not_in_doc_index(&wiki_hits, &doc_index);
    let kind = if params.action.as_str() == "doc_index" {
        "doc_index"
    } else {
        "feature_briefing"
    };
    let mut response = json!({
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
        "feedback_rules": feedback_rules_trace,
        "memory_fragments": memory_fragments,
        "eval_evidence": eval_evidence,
        "doc_index": doc_index,
        "next_action": next_action,
        "open_loops": open_loops,
        "issue_freshness": issue_freshness,
        "presence": {
            "board": presence_board,
            "warnings": presence_warnings,
        },
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
    if !top_level_wiki_hits.is_empty() {
        response
            .as_object_mut()
            .expect("feature briefing response object")
            .insert("wiki_hits".to_string(), json!(top_level_wiki_hits));
    }

    if crate::facade_memory_ops::wants_json(params.format.as_deref()) {
        serde_json::to_string(&response).map_err(|e| format!("serialize feature briefing: {e}"))
    } else {
        Ok(format_feature_briefing_markdown(&response))
    }
}
