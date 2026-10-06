use super::*;

#[tool_router(router = workflow_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    // ─── Facade: skill (discover / run) ─────────────────────────────────────

    #[tool(
        description = "Skill library for pre-built agent workflows. action='discover': search for a skill BEFORE solving a complex problem; action='run': execute a named skill by ID. Delegate tool profiles may use discover/run. Always discover before writing custom multi-step logic."
    )]
    pub(crate) async fn tachi_skill(
        &self,
        Parameters(params): Parameters<TachiSkillParams>,
    ) -> Result<String, String> {
        handle_tachi_skill_facade(self, params).await
    }

    // ─── Facade: task (briefing / complete / status / board / lifecycle)

    #[tool(
        description = "Task memory, policy, and ledger facade. The host harness's native subagent is the DEFAULT for ordinary local delegation; worker launch is tachi_staff(action='start'), not tachi_task. action='brief': feature-scoped handoff board with layered docs/specs, run artifacts, board state, wiki, memory fragments, eval evidence, and next action; action='status'/'board': read existing worker state, with status also returning a nested read-only cycle view when flow_id/issue_ref/pr_ref is supplied (Task is a unified work read model, not a worker-status authority); action='complete': record evaluated completion evidence; action='intake': bind lifecycle inputs. Operator-only dispatch diagnostics remain on a local CLI command and are not a model-facing Task action. GitHub lifecycle is tachi_gh only (#1713). Route-policy tuning is tachi_tune only (admin/operator). Harness-native subagent outcomes can be mirrored into the existing eval/ledger surfaces without Tachi owning their process lifecycle."
    )]
    pub(crate) async fn tachi_task(
        &self,
        Parameters(params): Parameters<TachiTaskParams>,
    ) -> Result<String, String> {
        handle_tachi_task_facade(self, params).await
    }

    // ─── GitHub MCP Proxy Tools ─────────────────────────────────────────────

    #[tool(
        description = "GitHub. PRs take repo+number or pr_ref. current_truth_refresh(repo+number) appends CurrentTruth; missing/malformed/incomplete/stale GitHub data fails closed. close_loop writes a referenced wiki closure, then comments/pattern feedback and a terminal marker only after successful wiki write; dry_run=true makes no writes. issue_comment needs number+body; pr_comment a PR target+body; both support dry_run previews. pr_review_digest: author_filter defaults to gemini; writes .tachi/reviews by default; returns memory/handbook candidates and a leader-verdict plan. safe_merge: requested_mode=preview unless confirm=true and dry_run!=true; merge_attempted/merge_executed are separate; tests_run populates a missing verification ledger with flow_id. ship: mechanical (files+commit_message) stages exact files, commits/pushes, PR only with pr_title+pr_body; contract (empty files, no commit_message) derives zero-prose PR title/body from git log/issue_ref/tests_run, pushes and opens PR. link_pr/pr_status/pr_handoff/release_note own lifecycle. With flow_id, safe_merge/pr_status read .tachi/runs/<flow_id>/verification.json; standard/strict wait on missing and block on failed/stale. Requires GH_TOKEN in Vault or environment."
    )]
    pub(crate) async fn tachi_gh(
        &self,
        Parameters(params): Parameters<TachiGhParams>,
    ) -> Result<String, String> {
        handle_tachi_gh(self, params).await
    }

    // ─── Tachi Verify: background verification evidence ledger ─────────────

    #[tool(
        description = "Background verification ledger. action='start' seeds pending checks; action='record' stores results from external runners (gitleaks, cargo check, clippy, tests); action='run' executes a closed-set check (fmt/clippy/nextest/doc/audit) in the flow's claimed worktree with server-observed head_sha and source='server_run:<kind>' — the only evidence that can satisfy the safe-merge authority gate; action='status'/'board' reads .tachi/runs/<flow_id>/verification.json. Safe-merge consumes required checks for the matching flow/head SHA: failed or stale required checks block merges, and missing required checks wait in standard/strict mode."
    )]
    pub(crate) async fn tachi_verify(
        &self,
        Parameters(params): Parameters<TachiVerifyParams>,
    ) -> Result<String, String> {
        handle_tachi_verify(self, params).await
    }

    // ─── Tachi Staff: external staffing facade ──────────────────────────────

    #[tool(
        description = "External staffing. start launches via the canonical dispatch kernel and requires a typed staffing_reason (native-first exception); execution is resolved by profile/policy, not the caller. status reads the canonical receipt by dispatch_id. result adds the full UTF-8 result.md (maximum 64 KiB; larger files are rejected) without the Task status preview cap. cancel requests managed-custom cancellation (dispatch_id + expected_status_revision). preflight is a read-only backend probe; it never launches, admits, authenticates, or cancels."
    )]
    pub(crate) async fn tachi_staff(
        &self,
        Parameters(params): Parameters<TachiStaffParams>,
    ) -> Result<String, String> {
        // `TachiStaffParams` is a flat, MCP-compatible struct (a `#[serde(tag)]`
        // enum would emit a schema without the root `type: object` the MCP spec
        // requires). Per-action admission is enforced HERE, before any run
        // artifact, so each action requires only its own fields:
        //   - start: REQUIRES a typed staffing_reason (native-first gate) +
        //     non-empty task; rejected with zero artifacts if missing.
        //   - status/result: REQUIRES dispatch_id; staffing_reason is IGNORED and
        //     MUST NOT be required (a read-only probe is never forced to
        //     fabricate a reason).
        let action = params.action.trim().to_ascii_lowercase();
        let format = params.format.clone();
        let raw = match action.as_str() {
            "preflight" => crate::staffing_ops::staff_preflight(&params).await?,
            "start" => {
                if params.expected_status_revision.is_some() {
                    return Err("tachi_staff: action='start' rejects cancel-only field `expected_status_revision`".to_string());
                }
                let request = params.to_assignment_request().map_err(|e| {
                    if e.contains("staffing_reason") {
                        "tachi_staff: action='start' requires a typed staffing_reason (the native-first exception); use the host harness's native subagent for ordinary delegation, or set staffing_reason to explicit_user_request / durable_cross_session / cross_device_remote / native_subagent_unavailable for an admitted exception; zero staffing or dispatch artifacts were created.".to_string()
                    } else if e.contains("task") {
                        "tachi_staff: action='start' requires a non-empty `task`".to_string()
                    } else {
                        e
                    }
                })?;
                crate::staffing_ops::staff_start(self, request).await?
            }
            "status" | "result" => {
                // status/result ignore staffing_reason entirely — a read-only probe
                // never needs a reason and is not pressured to fabricate one.
                let dispatch_id = params.dispatch_id.ok_or_else(|| {
                    format!("tachi_staff: action='{action}' requires a `dispatch_id`")
                })?;
                let raw = crate::staffing_ops::staff_status(
                    self,
                    crate::staffing_ops::StaffStatusRequest {
                        dispatch_id: dispatch_id.clone(),
                    },
                )
                .await?;
                // Read-surface projection (S1): for managed runs carrying a
                // durable identity record, expose execution state, control
                // state, controller epoch, reconciliation state, and artifact
                // availability as SEPARATE response facts. Response-only: the
                // canonical receipt bytes are never rewritten by a read, and
                // the projection is computed from THIS response's own parsed
                // snapshot — the reply can never mix two receipt revisions.
                match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(mut receipt) => {
                        let projection = crate::staffing_ops::staff_status_projection_from_receipt(
                            self,
                            &dispatch_id,
                            &receipt,
                        );
                        let enriched = projection.is_some() || action == "result";
                        if let Some(projection) = projection {
                            if let Some(object) = receipt.as_object_mut() {
                                object.insert("read_projection".to_string(), projection);
                            }
                        }
                        if action == "result" {
                            let run_dir =
                                crate::dispatch_ops::dispatch_runs_root().join(&dispatch_id);
                            let object = receipt.as_object_mut().ok_or_else(|| {
                                "tachi_staff: result receipt must be an object".to_string()
                            })?;
                            object.insert(
                                "result".to_string(),
                                read_dispatch_result(&run_dir, None)?,
                            );
                        }
                        if enriched {
                            serde_json::to_string_pretty(&receipt)
                                .map_err(|err| format!("tachi_staff: serialize {action}: {err}"))?
                        } else {
                            raw
                        }
                    }
                    Err(err) if action == "result" => {
                        return Err(format!("tachi_staff: decode result receipt: {err}"));
                    }
                    Err(_) => raw,
                }
            }
            "cancel" => {
                let (dispatch_id, expected_status_revision) = params.cancel_request()?;
                crate::staffing_ops::staff_cancel(
                    self,
                    crate::staffing_ops::StaffCancelRequest {
                        dispatch_id,
                        expected_status_revision,
                    },
                )
                .await?
            }
            other => {
                return Err(format!(
                    "tachi_staff: unknown action '{other}' (start|status|result|cancel|preflight)"
                ))
            }
        };
        if action == "cancel" {
            // The cancel result is itself the committed canonical receipt.
            // Do not wrap it in a facade action/status projection.
            return Ok(raw);
        }
        format_facade_response(
            &format!("Tachi staff {}", action),
            &action,
            &raw,
            format.as_deref(),
            false,
        )
    }
}
