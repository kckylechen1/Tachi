use super::*;
use crate::copilot_ops::handle_tachi_task_brief;

pub(super) async fn handle_tachi_task_facade(
    server: &MemoryServer,
    params: TachiTaskParams,
) -> Result<String, String> {
    // F4: typed action enum; match on wire string for stable arm labels.
    let action = params.action.as_str().to_string();
    let raw = match action.as_str() {
        "plan" => {
            let task = params
                .task
                .clone()
                .ok_or_else(|| "task is required when action='plan'".to_string())?;
            let brief_params = TaskBriefParams {
                task,
                agent_id: params.agent_id.clone(),
                project: params.project.clone(),
                path_prefix: params.path_prefix.clone(),
                domain: params.domain.clone(),
                top_k: crate::clamp_facade_top_k(
                    params.top_k.unwrap_or(6),
                ),
            };
            return handle_tachi_task_brief(server, brief_params).await;
        }
        "briefing" | "doc_index" => return handle_tachi_feature_briefing(server, &params).await,
        "dispatch" => {
            crate::task_lifecycle::guard_issue_flow_dispatch(&params)?;
            if params.agent.is_none() && params.profile.is_none() {
                return Err("agent or profile is required when action='dispatch'".to_string());
            }
            let task = params
                .task
                .clone()
                .ok_or_else(|| "task is required when action='dispatch'".to_string())?;
            let dispatch_params = TachiDispatchParams {
                agent: params.agent.clone(),
                profile: params.profile.clone(),
                task,
                cwd: params.cwd.clone(),
                skills: params.skills.clone(),
                context_query: params.context_query.clone(),
                model: params.model.clone(),
                timeout_secs: params.timeout_secs.unwrap_or(600),
                permission_profile: params.permission_profile.clone(),
                allowed_tools: params.allowed_tools.clone(),
                completion_predicate: params.completion_predicate.clone(),
                max_turns: params.max_turns,
                sandbox: params.sandbox.clone(),
                inject_tachi_mcp: params.inject_tachi_mcp,
                inject_hub_mcps: params.inject_hub_mcps,
                command: params.command.clone(),
                harness_transport: params.harness_transport.clone(),
                harness_server_url: params.harness_server_url.clone(),
                project: params.project.clone(),
                stage: params.stage.clone(),
                credential_profiles: params.credential_profiles.clone(),
                issue_ref: params.issue_ref.clone(),
                pr_ref: params.pr_ref.clone(),
                flow_id: params.flow_id.clone(),
                tool_profile: params.tool_profile.clone(),
                auto_capability_bundle: params.auto_capability_bundle,
                mcp_access: params.mcp_access.clone(),
                allowed_mcp_servers: params.allowed_mcp_servers.clone(),
            };
            crate::dispatch_ops::handle_tachi_dispatch(server, dispatch_params).await
        }
        "complete" => {
            let dispatch_defaults = params
                .dispatch_id
                .as_deref()
                .filter(|dispatch_id| !dispatch_id.trim().is_empty())
                .and_then(|dispatch_id| {
                    read_dispatch_defaults_for_complete_with_flow(
                        params.flow_id.as_deref(),
                        dispatch_id,
                    )
                });
            let task = params
                .task
                .clone()
                .or_else(|| {
                    dispatch_defaults
                        .as_ref()
                        .and_then(|defaults| defaults.task.clone())
                })
                .ok_or_else(|| {
                    "task is required when action='complete' (or provide a dispatch_id with a readable run status/card)".to_string()
                })?;
            let agent = params
                .agent
                .clone()
                .or_else(|| {
                    dispatch_defaults
                        .as_ref()
                        .and_then(|defaults| defaults.agent.clone())
                })
                .ok_or_else(|| {
                    "agent is required when action='complete' (or provide a dispatch_id with a readable run status/card)".to_string()
                })?;
            let outcome = params
                .outcome
                .clone()
                .ok_or_else(|| "outcome is required when action='complete'".to_string())?;
            let complete_params = TachiCompleteParams {
                task_id: params.task_id.clone(),
                task,
                agent,
                outcome,
                task_type: params.task_type.clone(),
                profile: params.profile.clone().or_else(|| {
                    dispatch_defaults
                        .as_ref()
                        .and_then(|defaults| defaults.profile.clone())
                }),
                risk: params.risk.clone(),
                duration_ms: params.duration_ms,
                skills_used: params.skills_used.clone(),
                cost_tokens: params.cost_tokens,
                cost_usd: params.cost_usd,
                quality_score: params.quality_score,
                notes: params.notes.clone(),
                trajectory: params.trajectory.clone(),
                diff: params.diff.clone(),
                worktree: params.worktree.clone(),
                subagents: params.subagents.clone(),
                feedback_rules_applied: params.feedback_rules_applied.clone(),
                dispatch_id: params.dispatch_id.clone(),
                flow_id: params.flow_id.clone(),
                issue_ref: params.issue_ref.clone(),
                pr_ref: params.pr_ref.clone(),
                evidence_refs: params.evidence_refs.clone(),
                tests_run: params.tests_run.clone(),
                diff_present: params.diff_present,
                scope: params.scope.clone(),
                project: params.project.clone(),
                format: params.format.clone(),
                signatures: params.signatures.clone(),
            };
            crate::complete_ops::handle_tachi_complete(server, complete_params).await
        }
        "board" => {
            let board_params = TachiBoardParams {
                state_filter: params.state_filter.clone(),
                limit: params.limit,
                project: params.project.clone(),
                flow_id: params.flow_id.clone(),
            };
            crate::dispatch_ops::handle_tachi_board(server, board_params).await
        }
        "status" => handle_tachi_task_status(server, &params).await,
        "cancel" => handle_tachi_task_cancel(server, &params).await,
        "wait" => handle_tachi_task_wait(server, &params).await,
        "profiles" | "profile" | "card" => serde_json::to_string(
            &crate::dispatch_profile::dispatch_profiles_json_for_server(server)?,
        )
        .map_err(|e| format!("serialize dispatch profiles: {e}")),
        "intake" => crate::task_lifecycle::handle_task_intake(server, &params).await,
        "link_pr" => Ok(with_gh_lifecycle_deprecation(
            "link_pr",
            crate::task_lifecycle::handle_task_link_pr(server, &params).await?,
        )),
        "cycle_status" => crate::task_lifecycle::handle_task_cycle_status(server, &params).await,
        "cycle_plan" => crate::task_lifecycle::handle_task_cycle_plan(server, &params).await,
        "recommend" => {
            let task = params
                .task
                .clone()
                .ok_or_else(|| "task is required when action='recommend'".to_string())?;
            let mut file_paths = params.doc_paths.clone();
            file_paths.extend(params.spec_paths.clone());
            crate::dispatch_profile::handle_dispatch_recommendation(
                server,
                &task,
                params.risk.as_deref(),
                params.limit.unwrap_or(500),
                &file_paths,
            )
        }
        "route_simulate" => {
            let mut file_paths = params.doc_paths.clone();
            file_paths.extend(params.spec_paths.clone());
            crate::dispatch_profile::handle_route_simulation(
                server,
                params.limit.unwrap_or(500),
                params.task.as_deref(),
                params.risk.as_deref(),
                &file_paths,
            )
        }
        "proposals" => crate::dispatch_profile::handle_route_policy_proposals(
            server,
            params.limit.unwrap_or(500),
            params.state_filter.as_deref(),
        ),
        "review_proposal" => {
            let proposal_id = params.proposal_id.as_deref().ok_or_else(|| {
                "proposal_id is required when action='review_proposal'".to_string()
            })?;
            let review_status = params.review_status.as_deref().ok_or_else(|| {
                "review_status is required when action='review_proposal'".to_string()
            })?;
            crate::dispatch_profile::handle_route_policy_review(
                server,
                proposal_id,
                review_status,
                params.notes.as_deref(),
            )
        }
        "apply_proposals" => {
            let proposal_id = params.proposal_id.as_deref().ok_or_else(|| {
                "proposal_id is required when action='apply_proposals'".to_string()
            })?;
            crate::dispatch_profile::handle_route_policy_apply(
                server,
                proposal_id,
                params.confirm,
            )
        }
        "merge" => {
            if params.pr_ref.is_some() || params.issue_ref.is_some() {
                return Err(
                    "tachi_task(action='merge') only merges local dispatched worktrees. Use tachi_gh(action='safe_merge', repo=..., number=...) for GitHub PR gates or PR merges."
                        .to_string(),
                );
            }
            let worktree = params
                .worktree
                .clone()
                .ok_or_else(|| "worktree is required when action='merge'".to_string())?;
            let merge_params = TachiApproveMergeParams {
                worktree,
                branch: params.branch.clone(),
                strategy: params.strategy.clone(),
                delete_worktree: params.delete_worktree,
                confirm: params.confirm,
            };
            tachi_merge_ops::handle_approve_merge(merge_params).await
        }
        "pr_status" => {
            let gh_params = build_task_pr_status_gh_params(&params)?;
            Ok(with_gh_lifecycle_deprecation(
                "pr_status",
                crate::gh_ops::handle_tachi_gh(server, gh_params).await?,
            ))
        }
        "pr_handoff" => Ok(with_gh_lifecycle_deprecation(
            "pr_handoff",
            crate::task_lifecycle::handle_task_pr_handoff(&params)?,
        )),
        "release_note" => Ok(with_gh_lifecycle_deprecation(
            "release_note",
            crate::task_lifecycle::handle_task_release_note(server, &params).await?,
        )),
        "ux_matrix" => crate::task_lifecycle::handle_task_ux_matrix(&params),
        "build_references" | "close_loop" => {
            let workflow_params = TachiWorkflowParams {
                action: action.clone(),
                issue_ref: params.issue_ref.clone(),
                pr_ref: params.pr_ref.clone(),
                doc_paths: params.doc_paths.clone(),
                spec_paths: params.spec_paths.clone(),
                related_issues: params.related_issues.clone(),
                post_comment: None,
                flow_id: params.flow_id.clone(),
                wiki_title: params.wiki_title.clone(),
                wiki_text: params.wiki_text.clone(),
                wiki_path: params.wiki_path.clone(),
                wiki_topic: params.wiki_topic.clone(),
                wiki_summary: params.wiki_summary.clone(),
                wiki_category: params.wiki_category.clone(),
                wiki_keywords: params.wiki_keywords.clone(),
                wiki_entities: params.wiki_entities.clone(),
                wiki_importance: params.wiki_importance,
                wiki_scope: params.wiki_scope.clone(),
                wiki_domain: params.wiki_domain.clone(),
                project: params.project.clone(),
                force: params.force,
            };
            let result = crate::workflow_closure::handle_workflow(server, workflow_params).await?;
            if action == "close_loop" {
                if let Some(flow_id) = params
                    .flow_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                {
                    crate::task_lifecycle::mark_task_close_loop(flow_id, &result)?;
                }
            }
            Ok(result)
        }
        _ => Err(format!(
            "Invalid action '{}'. Primary task actions: briefing/doc_index/plan/dispatch/complete/status/cancel/board/wait/profiles/profile/card/recommend/route_simulate/proposals/review_proposal/apply_proposals/intake/cycle_status/cycle_plan/ux_matrix/build_references/close_loop/merge. GitHub PR lifecycle (link_pr/pr_status/pr_handoff/release_note) is accepted for compatibility — prefer tachi_gh.",
            params.action
        )),
    }?;
    if action == "complete" && crate::facade_memory_ops::wants_full_format(params.format.as_deref())
    {
        return Ok(raw);
    }
    format_facade_response(
        &format!("Tachi task {}", action),
        &action,
        &raw,
        params.format.as_deref(),
    )
}

/// F2 (#495/#913): keep compat execution on `tachi_task` but mark GH lifecycle
/// as non-primary. Canonical surface is `tachi_gh(action=...)`.
fn with_gh_lifecycle_deprecation(action: &str, body: String) -> String {
    let notice = format!(
        "tachi_task(action='{action}') is a compatibility alias; prefer tachi_gh(action='{action}')"
    );
    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("deprecated_surface".to_string(), serde_json::json!(true));
            obj.insert(
                "canonical_tool".to_string(),
                serde_json::json!("tachi_gh"),
            );
            obj.insert(
                "deprecation_notice".to_string(),
                serde_json::json!(notice),
            );
            if let Ok(serialized) = serde_json::to_string(&value) {
                return serialized;
            }
        }
    }
    // Non-JSON responses: prefix a one-line notice.
    format!("DEPRECATION: {notice}\n{body}")
}
