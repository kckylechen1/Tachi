use super::*;
use crate::copilot_ops::handle_tachi_task_brief;

pub(super) async fn handle_tachi_task_facade(
    server: &MemoryServer,
    params: TachiTaskParams,
) -> Result<String, String> {
    // F4 (#913): typed action enum. `action` (wire string) is kept for arm
    // labels/response formatting; the dispatch below matches on
    // `params.action` (the enum) directly and exhaustively — no `_ =>`
    // catch-all — so a new `TachiTaskAction` variant fails to COMPILE here
    // until it is explicitly routed, instead of silently round-tripping
    // through an "Invalid action" string match a new variant could slip past
    // (#919 concern).
    let action = params.action.as_str().to_string();
    reject_delegate_task_action(server, &action)?;
    let raw = match params.action {
        TachiTaskAction::Plan => {
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
                top_k: crate::clamp_facade_top_k(params.top_k.unwrap_or(6)),
            };
            return handle_tachi_task_brief(server, brief_params).await;
        }
        TachiTaskAction::Briefing | TachiTaskAction::DocIndex => {
            return handle_tachi_feature_briefing(server, &params).await
        }
        TachiTaskAction::Dispatch => {
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
                execution_level: params.execution_level,
                cwd: params.cwd.clone(),
                env_id: params.env_id.clone(),
                unmanaged_cwd: params.unmanaged_cwd,
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
                verbose: params.verbose,
                inject_card: params.inject_card,
            };
            crate::dispatch_ops::handle_tachi_dispatch(server, dispatch_params).await
        }
        TachiTaskAction::Complete => {
            let dispatch_defaults = params
                .dispatch_id
                .as_deref()
                .filter(|dispatch_id| !dispatch_id.trim().is_empty())
                .and_then(|dispatch_id| {
                    read_dispatch_defaults_for_complete_with_flow(
                        &server.tachi_home_dir(),
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
                eval_run_ids: params.eval_run_ids.clone(),
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
                rulings: params.rulings.clone(),
                adjudication: params.adjudication.clone(),
            };
            // #1041 B7: forward the REAL wire explicitness signal —
            // `TachiCompleteParams.project` here can be a transport-injected
            // default (`tachi_task` IS in
            // `session_identity::project_defaults_to_bound_project`'s list),
            // and `TachiCompleteParams` itself has no field to carry that
            // distinction across this bridge.
            crate::complete_ops::handle_tachi_complete(
                server,
                complete_params,
                params.project_explicit,
            )
            .await
        }
        TachiTaskAction::Board => {
            let board_params = TachiBoardParams {
                state_filter: params.state_filter.clone(),
                limit: params.limit,
                project: params.project.clone(),
                flow_id: params.flow_id.clone(),
            };
            crate::dispatch_ops::handle_tachi_board(server, board_params).await
        }
        TachiTaskAction::Status => handle_tachi_task_status(server, &params).await,
        TachiTaskAction::Cancel => handle_tachi_task_cancel(server, &params).await,
        TachiTaskAction::Wait => handle_tachi_task_wait(server, &params).await,
        // codex review round 2 (#1182 checkpoint 2): the issue #1173 escape
        // hatch is "verbose=true OR action='profile'" — two independent ways
        // to get the full card. `profiles` (the listing) is the one item 2
        // names as needing to slim; `profile`/`card` (singular-sounding
        // aliases of the same underlying listing call, pre-existing before
        // #1173) are the promised on-demand full-card fetch and must default
        // to full unless the caller explicitly asks for the slim shape via
        // verbose=false.
        TachiTaskAction::Profiles => {
            serde_json::to_string(&crate::dispatch_profile::dispatch_profiles_json_for_server(
                server,
                params.verbose.unwrap_or(false),
            )?)
            .map_err(|e| format!("serialize dispatch profiles: {e}"))
        }
        TachiTaskAction::Profile | TachiTaskAction::Card => {
            serde_json::to_string(&crate::dispatch_profile::dispatch_profiles_json_for_server(
                server,
                params.verbose.unwrap_or(true),
            )?)
            .map_err(|e| format!("serialize dispatch profiles: {e}"))
        }
        TachiTaskAction::Intake => crate::task_lifecycle::handle_task_intake(server, &params).await,
        TachiTaskAction::CycleStatus => {
            crate::task_lifecycle::handle_task_cycle_status(server, &params).await
        }
        TachiTaskAction::CyclePlan => {
            crate::task_lifecycle::handle_task_cycle_plan(server, &params).await
        }
        TachiTaskAction::Recommend => {
            let task = params
                .task
                .clone()
                .ok_or_else(|| "task is required when action='recommend'".to_string())?;
            let mut file_paths = params.doc_paths.clone();
            file_paths.extend(params.spec_paths.clone());
            let admission = crate::host_profile::admit_execution_level(params.execution_level);
            if !admission.allowed {
                serde_json::to_string(&serde_json::json!({
                    "host_admission": admission.to_json(),
                }))
                .map_err(|e| format!("serialize host admission decline: {e}"))
            } else {
                let raw = crate::dispatch_profile::handle_dispatch_recommendation(
                    server,
                    &task,
                    params.risk.as_deref(),
                    params.limit.unwrap_or(500),
                    &file_paths,
                )?;
                attach_host_admission(raw, &admission)
            }
        }
        TachiTaskAction::RouteSimulate => {
            let mut file_paths = params.doc_paths.clone();
            file_paths.extend(params.spec_paths.clone());
            let admission = crate::host_profile::admit_execution_level(params.execution_level);
            if !admission.allowed {
                serde_json::to_string(&serde_json::json!({
                    "action": "route_simulate",
                    "host_admission": admission.to_json(),
                }))
                .map_err(|e| format!("serialize host admission decline: {e}"))
            } else {
                let raw = crate::dispatch_profile::handle_route_simulation(
                    server,
                    params.limit.unwrap_or(500),
                    params.task.as_deref(),
                    params.risk.as_deref(),
                    &file_paths,
                )?;
                attach_host_admission(raw, &admission)
            }
        }
        TachiTaskAction::Proposals => crate::dispatch_profile::handle_route_policy_proposals(
            server,
            params.limit.unwrap_or(500),
            params.state_filter.as_deref(),
        ),
        TachiTaskAction::ReviewProposal => {
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
        TachiTaskAction::ApplyProposals => {
            let proposal_id = params.proposal_id.as_deref().ok_or_else(|| {
                "proposal_id is required when action='apply_proposals'".to_string()
            })?;
            crate::dispatch_profile::handle_route_policy_apply(server, proposal_id, params.confirm)
        }
        TachiTaskAction::Adjudicate => {
            let adjudication = params
                .adjudication
                .clone()
                .ok_or_else(|| "adjudication is required when action='adjudicate'".to_string())?;
            let result = crate::complete_ops::dispatch_outcome::record_posthoc_adjudication(
                server,
                params.outcome_id.as_deref(),
                params.dispatch_id.as_deref(),
                &adjudication,
                &params.signatures,
                params.project.as_deref(),
                params.scope.as_deref(),
            );
            serde_json::to_string(&result).map_err(|e| format!("serialize adjudicate result: {e}"))
        }
        TachiTaskAction::Merge => {
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
        TachiTaskAction::UxMatrix => crate::task_lifecycle::handle_task_ux_matrix(&params),
        TachiTaskAction::BuildReferences | TachiTaskAction::CloseLoop => {
            let workflow_params = TachiWorkflowParams {
                action: action.clone(),
                issue_ref: params.issue_ref.clone(),
                pr_ref: params.pr_ref.clone(),
                doc_paths: params.doc_paths.clone(),
                spec_paths: params.spec_paths.clone(),
                related_issues: params.related_issues.clone(),
                post_comment: None,
                flow_id: params.flow_id.clone(),
                notes: params.notes.clone(),
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
        TachiTaskAction::RefineIssues => {
            crate::refinery_ops::handle_refine_issues(server, &params).await
        } // No `_ =>` catch-all: `TachiTaskAction` is exhaustively matched
          // above (#919 concern) — a new variant fails to compile here until
          // it is explicitly routed, instead of silently returning "Invalid
          // action" for a value that already deserialized successfully.
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

fn attach_host_admission(
    raw: String,
    admission: &crate::host_profile::HostAdmission,
) -> Result<String, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse recommend/route payload: {e}"))?;
    let object = value.as_object_mut().ok_or_else(|| {
        "attach host admission: expected recommend/route payload to be a JSON object".to_string()
    })?;
    object.insert("host_admission".to_string(), admission.to_json());
    serde_json::to_string(&value).map_err(|e| format!("serialize host admission attach: {e}"))
}

/// Defense-in-depth for the task facade; primary gate is F3
/// `facade_action_allowed` in `call_tool` (covers the MCP path). Direct
/// internal calls to `handle_tachi_task_facade` still hit this — same
/// pattern as `skill_facade::reject_delegate_skill_action` (#919 concern:
/// `tachi_task` previously forwarded straight to the router with no
/// handler-level re-check).
fn reject_delegate_task_action(server: &MemoryServer, action: &str) -> Result<(), String> {
    if !tachi_hub::facade_action_allowed("tachi_task", Some(action), server.active_tool_profile()) {
        return Err(format!(
            "tachi_task(action='{action}') is not available to the active tool profile; delegate workers may use 'plan', 'complete', 'status', 'board', 'wait', 'briefing', or 'doc_index'."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod host_admission_tests {
    use super::attach_host_admission;

    #[test]
    fn attach_host_admission_rejects_non_object_producer_payloads() {
        let _profile = crate::host_profile::HostProfileTestOverride::set(Some("development"));
        let admission =
            crate::host_profile::admit_execution_level(Some(tachi_params::ExecutionLevel::L0));

        for raw in ["[]", "null", r#""scalar""#] {
            let error = attach_host_admission(raw.to_string(), &admission)
                .expect_err("non-object producer payload must fail closed");
            assert_eq!(
                error,
                "attach host admission: expected recommend/route payload to be a JSON object"
            );
        }
    }
}
