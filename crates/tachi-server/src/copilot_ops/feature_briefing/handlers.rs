use super::super::*;
use super::board::{feature_board, feature_needles, feature_run_artifacts};
use super::dispatch::{
    feature_dispatch_recommendation, relevant_feature_profiles, suggested_feature_handoff,
};
use super::docs::{
    build_feature_doc_index, canonical_doc_refs, feature_briefing_query, project_work_records,
};
use super::markdown::format_feature_briefing_markdown;
use super::stage::{feature_next_action, infer_feature_stage};

const A2A_BRIEFING_RESPONSE_LIMIT: usize = 20;

fn current_briefing_actor(server: &MemoryServer) -> Option<memcore::A2aTransitionActor> {
    server
        .work_claim_connection()
        .and_then(|(identity, connection_id, admission)| {
            (admission == "self_asserted").then_some((identity, connection_id))
        })
        .and_then(|(identity, connection_id)| {
            Some(memcore::A2aTransitionActor {
                agent_identity_id: identity?,
                connection_id: (!connection_id.trim().is_empty()).then_some(connection_id)?,
            })
        })
}

fn consume_a2a_responses_for_briefing(server: &MemoryServer) -> Result<Vec<Value>, String> {
    let Some(actor) = current_briefing_actor(server) else {
        return Ok(Vec::new());
    };
    let envelopes = server.with_global_store(|store| {
        memcore::consume_a2a_for_recipient(
            store.connection_mut(),
            &actor,
            A2A_BRIEFING_RESPONSE_LIMIT,
            &Utc::now().to_rfc3339(),
        )
        .map_err(|error| error.to_string())
    })?;
    envelopes
        .into_iter()
        .map(|envelope| {
            let body = envelope.body.ok_or_else(|| {
                format!(
                    "A2A invariant violation: pending envelope '{}' has no body",
                    envelope.envelope_id
                )
            })?;
            Ok(json!({
                "envelope_id": envelope.envelope_id,
                "kind": envelope.kind,
                "issuer_agent_identity_id": envelope.issuer_agent_identity_id,
                "recipient_agent_identity_id": envelope.recipient_agent_identity_id,
                "subject_ref": envelope.subject_ref,
                "body": crate::memory_search_ops::scrub_generated_memory_text(&body),
                "body_digest": envelope.body_digest,
                "identity_assurance": {
                    "issuer": envelope.issuer_identity_assurance,
                    "recipient": envelope.recipient_identity_assurance,
                },
                "created_at": envelope.created_at,
                "expires_at": envelope.expires_at,
            }))
        })
        .collect::<Result<Vec<_>, String>>()
}

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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn handle_tachi_task_brief(
    server: &MemoryServer,
    params: TaskBriefParams,
) -> Result<String, String> {
    let top_k = crate::clamp_facade_top_k(params.top_k);
    let wiki_plan = WikiReadPlan::from_project(params.project.as_deref())?;
    let (wiki_rows, wiki_warning) = match crate::wiki_ops::search_wiki_rows_for_plan(
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
            format: None,
        },
        &wiki_plan,
        None,
        false,
    )
    .await
    {
        Ok(result) => (result.rows, None),
        Err(err) => (Vec::new(), Some(format!("wiki recall unavailable: {err}"))),
    };
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
            format: None,
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
        "wiki_warning": wiki_warning,
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
            "tachi_task(action='brief')",
            "tachi_task(action='board')"
        ],
    }))
    .map_err(|e| format!("serialize task_brief: {e}"))
}

pub(crate) async fn handle_tachi_feature_briefing(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    // #1575 fix-round: this handler serves the folded action='brief', and its
    // memory/eval searches below run `project_only=true` (via
    // `!params.include_global`, which defaults to `false`) — the same shape
    // `memory_search_ops::require_named_project_exists`'s doc comment
    // documents as needing this guard: `project_only` searches fall through
    // to workspace/bound-store resolution on a miss instead of erroring
    // (`search_memory/rows.rs`). Apply the same guard `facade_memory_ops::
    // briefing_ops::handle_memory_briefing` uses for `tachi_memory(action=
    // 'briefing')`, so an explicit nonexistent `project=` errors here too
    // instead of silently answering from whichever store the process is
    // bound to.
    //
    // Normalize ONCE at this seam — trim the caller's name, validate the
    // TRIMMED form (path-based project resolution builds a directory
    // component from the literal string, so an untrimmed lookup would
    // itself miss and silently re-trigger the fallback this guard exists to
    // close), then shadow `params` with a copy whose `.project` is the
    // trimmed name so every downstream consumer in this function (wiki
    // plan, the three project_only searches, board/doc-index scoping, the
    // `scope.project` response echo) resolves the SAME identity the guard
    // validated. Empty-after-trim is a loud typed error, not a silent
    // fallback.
    let normalized_project = match params.project.as_deref() {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err("project name cannot be empty".to_string());
            }
            crate::memory_search_ops::require_named_project_exists(server, trimmed)?;
            Some(trimmed.to_string())
        }
        None => None,
    };
    let params = &{
        let mut p = params.clone();
        p.project = normalized_project;
        p
    };
    // #527: omitted compact defaults to true (agent packet). Full board is
    // opt-in. `compact` is the only knob this call site honors for that —
    // `format` here is purely the JSON-vs-markdown response-shape selector
    // (see `TachiTaskParams::format`'s own doc comment), a separate concern.
    // kckylechen1/tachi#1058: it's the *outer* `intake` receipt's `format`
    // that needs reverse-pressuring into the inner briefing call's `compact`
    // — that's fixed at the construction site in
    // `task_lifecycle::flow_artifacts::github::intake_briefing_params`,
    // before the inner call's own `format` gets hardcoded to `"json"`. This
    // function must not treat `format="full"` as an implicit `compact=false`
    // — that would silently flip a JSON caller's response into markdown via
    // `wants_json` below (`format="full"` isn't a recognized JSON token).
    let top_k = if params.compact.unwrap_or(true) {
        params.top_k.unwrap_or(4).clamp(1, 4)
    } else {
        crate::clamp_facade_top_k(params.top_k.unwrap_or(6))
    };
    // Briefing is the only v1 delivery boundary. Selection and the
    // received→accepted→consumed receipts happen in one Memcore transaction;
    // JSON and Markdown below render this same immutable vector.
    let a2a_responses = consume_a2a_responses_for_briefing(server)?;
    let query = feature_briefing_query(params);
    let wiki_plan = WikiReadPlan::from_project(params.project.as_deref())?;
    let project_work_record = project_work_records(params);
    let canonical_docs = canonical_doc_refs(params);
    let run_artifacts = feature_run_artifacts(params.flow_id.as_deref())?;

    let (board, wiki_rows, memory_rows, eval_rows) = tokio::join!(
        feature_board(server, params, top_k),
        crate::wiki_ops::search_wiki_rows_for_plan(
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
                project: params.project.clone(),
                domain: params.domain.clone(),
                file_context: None,
                error_context: None,
                enable_rerank: false,
                as_of: None,
                include_metadata: true,
                format: None,
            },
            &wiki_plan,
            None,
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
                format: None,
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
                format: None,
            },
            !params.include_global,
        ),
    );
    // #1575: memory/eval search errors stay loud. Wiki federation is
    // best-effort (#1761): a leftover wiki-schema refuse must not kill the
    // rest of the brief. Explicit missing `project=` is already loud above.
    // Empty-without-warning is silent degradation — fold the refuse into
    // `wiki_warning` so the tray stays usable and the leftover is visible.
    let (wiki_rows, wiki_warning) = match wiki_rows {
        Ok(result) => (result.rows, None),
        Err(err) => (Vec::new(), Some(format!("wiki recall unavailable: {err}"))),
    };
    let memory_rows = memory_rows?;
    let eval_rows = eval_rows?;

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
    let suggested_handoff = suggested_feature_handoff(params, &query, &route_recommendation);
    let relevant_profiles = relevant_feature_profiles(&route_recommendation);
    let next_action = feature_next_action(
        params,
        &canonical_docs,
        &run_artifacts,
        &board,
        &memory_rows,
    );
    let open_loops = crate::task_lifecycle::scan_open_loops(8);
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
    // #1712 C1b-1: briefing and doc-index were one handler before the fold;
    // retain the established feature-briefing packet kind as the sole
    // canonical result discriminator instead of preserving an unreachable
    // retired-token fork.
    let kind = "feature_briefing";
    let mut response = json!({
        "status": "ok",
        "kind": kind,
        "a2a_responses": a2a_responses,
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
        "suggested_handoff": suggested_handoff,
        "guide_hits": guide_hits,
        "feedback_rules": feedback_rules_trace,
        "memory_fragments": memory_fragments,
        "eval_evidence": eval_evidence,
        "wiki_warning": wiki_warning,
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
            "runtime_artifacts": "dispatch/run files; runtime state, not canonical product truth",
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

#[cfg(test)]
mod a2a_response_tests {
    use super::*;
    use memcore::NewA2aEnvelope;

    fn briefing_params(format: &str) -> TachiTaskParams {
        serde_json::from_value(json!({
            "action": "brief",
            "task": "continue the reviewed slice",
            "format": format,
            "include_global": true,
        }))
        .expect("briefing params")
    }

    fn seed_response(
        server: &MemoryServer,
        envelope_id: &str,
        body: &str,
        created_at: &str,
        expires_at: &str,
    ) {
        crate::claims_ops::admit_agent_connection(
            server,
            Some("agent.recipient".to_string()),
            true,
        )
        .expect("historical recipient admission");
        crate::claims_ops::admit_agent_connection(server, Some("agent.issuer".to_string()), true)
            .expect("current issuer admission");
        let (_, issuer_connection_id, _) =
            server.work_claim_connection().expect("issuer connection");
        server
            .with_global_store(|store| {
                memcore::insert_a2a_envelope(
                    store.connection_mut(),
                    &NewA2aEnvelope {
                        envelope_id: envelope_id.to_string(),
                        issuer_agent_identity_id: "agent.issuer".to_string(),
                        issuer_connection_id,
                        recipient_agent_identity_id: "agent.recipient".to_string(),
                        subject_ref: "peer_publication:publication-parity".to_string(),
                        body: body.to_string(),
                        idempotency_key: envelope_id.to_string(),
                        created_at: created_at.to_string(),
                        expires_at: expires_at.to_string(),
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            })
            .expect("seed A2A response");
        crate::claims_ops::admit_agent_connection(
            server,
            Some("agent.recipient".to_string()),
            true,
        )
        .expect("current recipient admission");
    }

    /// Break caught: selecting responses separately per renderer, rendering
    /// them below Memory/Wiki, or returning different envelope ids/body.
    #[tokio::test]
    async fn json_and_markdown_render_the_same_consumed_response_first() {
        let json_server = crate::tests::make_server();
        let markdown_server = crate::tests::make_server();
        for server in [&*json_server, &*markdown_server] {
            seed_response(
                server,
                "envelope-parity",
                "review complete <think>private chain</think>; continue with slice four",
                "2026-08-12T00:00:00Z",
                "2099-08-19T00:00:00Z",
            );
        }

        let json_body = handle_tachi_feature_briefing(&json_server, &briefing_params("json"))
            .await
            .expect("JSON briefing");
        let json_value: Value = serde_json::from_str(&json_body).expect("JSON response");
        let selected = json_value["a2a_responses"]
            .as_array()
            .expect("A2A response array");
        assert_eq!(selected.len(), 1, "{json_value}");
        assert_eq!(selected[0]["envelope_id"], "envelope-parity");
        assert_eq!(
            selected[0]["body"],
            "review complete ; continue with slice four"
        );

        let markdown =
            handle_tachi_feature_briefing(&markdown_server, &briefing_params("markdown"))
                .await
                .expect("Markdown briefing");
        assert!(markdown.contains("envelope-parity"), "{markdown}");
        assert!(
            markdown.contains("review complete ; continue with slice four"),
            "{markdown}"
        );
        assert!(!json_body.contains("private chain"), "{json_body}");
        assert!(!markdown.contains("private chain"), "{markdown}");
        let a2a_at = markdown.find("## A2A Responses").expect("A2A section");
        let objective_at = markdown.find("## Objective").expect("objective section");
        let memory_at = markdown
            .find("## Memory Fragments / Checkpoints")
            .expect("memory section");
        assert!(a2a_at < objective_at && a2a_at < memory_at, "{markdown}");

        let status = json_server
            .with_global_store_read(|store| {
                memcore::list_a2a_status(store.connection(), "agent.recipient", 10)
                    .map_err(|error| error.to_string())
            })
            .expect("status after briefing");
        assert_eq!(status[0].current_state, "consumed");
        assert_eq!(
            status[0]
                .receipts
                .iter()
                .map(|receipt| receipt.state.as_str())
                .collect::<Vec<_>>(),
            ["received", "accepted", "consumed"]
        );

        let second = handle_tachi_feature_briefing(&json_server, &briefing_params("json"))
            .await
            .expect("second JSON briefing");
        let second: Value = serde_json::from_str(&second).unwrap();
        assert!(second["a2a_responses"].as_array().unwrap().is_empty());
    }

    /// Break caught: an expired body leaking into briefing or expiry being
    /// appended repeatedly on later briefing reads.
    #[tokio::test]
    async fn expired_response_is_terminal_and_never_projected() {
        let server = crate::tests::make_server();
        seed_response(
            &server,
            "envelope-expired",
            "must never render",
            "2026-08-01T00:00:00Z",
            "2026-08-02T00:00:00Z",
        );
        for _ in 0..2 {
            let body = handle_tachi_feature_briefing(&server, &briefing_params("json"))
                .await
                .expect("briefing");
            let value: Value = serde_json::from_str(&body).unwrap();
            assert!(value["a2a_responses"].as_array().unwrap().is_empty());
            assert!(!body.contains("must never render"), "{body}");
        }
        let status = server
            .with_global_store_read(|store| {
                memcore::list_a2a_status(store.connection(), "agent.recipient", 10)
                    .map_err(|error| error.to_string())
            })
            .expect("status");
        assert_eq!(status[0].current_state, "expired");
        assert_eq!(
            status[0]
                .receipts
                .iter()
                .filter(|receipt| receipt.state == "expired")
                .count(),
            1
        );
    }

    /// Break caught: using a seat/agent_id fallback when the current
    /// connection is remote/unavailable, thereby exposing or consuming a
    /// different identity's pending response.
    #[tokio::test]
    async fn unavailable_current_identity_neither_reads_nor_consumes() {
        let server = crate::tests::make_server();
        seed_response(
            &server,
            "envelope-unavailable",
            "recipient-only body",
            "2026-08-12T00:00:00Z",
            "2099-08-19T00:00:00Z",
        );
        crate::claims_ops::admit_agent_connection(&server, Some("agent.remote".to_string()), false)
            .expect("record unavailable current identity");
        let body = handle_tachi_feature_briefing(&server, &briefing_params("json"))
            .await
            .expect("briefing remains available without mailbox authority");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert!(value["a2a_responses"].as_array().unwrap().is_empty());
        assert!(!body.contains("recipient-only body"), "{body}");
        let status = server
            .with_global_store_read(|store| {
                memcore::list_a2a_status(store.connection(), "agent.recipient", 10)
                    .map_err(|error| error.to_string())
            })
            .expect("recipient status");
        assert_eq!(status[0].current_state, "received");
        assert_eq!(status[0].receipts.len(), 1);
    }

    #[tokio::test]
    async fn pending_envelope_without_body_fails_briefing_without_consuming_it() {
        let server = crate::tests::make_server();
        seed_response(
            &server,
            "envelope-missing-pending-body",
            "must remain pending",
            "2026-08-12T00:00:00Z",
            "2099-08-19T00:00:00Z",
        );
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .execute(
                        "UPDATE a2a_envelopes SET body=NULL
                         WHERE envelope_id='envelope-missing-pending-body'",
                        [],
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
            .expect("inject impossible pending-body state");

        let error = handle_tachi_feature_briefing(&server, &briefing_params("json"))
            .await
            .expect_err("missing pending body is a loud invariant violation");
        assert!(error.contains("pending envelope"), "{error}");
        let status = server
            .with_global_store_read(|store| {
                memcore::list_a2a_status(store.connection(), "agent.recipient", 10)
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        assert_eq!(status[0].current_state, "received");
        assert_eq!(status[0].receipts.len(), 1);
    }
}
