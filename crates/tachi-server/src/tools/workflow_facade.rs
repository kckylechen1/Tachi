use super::*;

#[tool_router(router = workflow_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    // ─── Facade: skill (discover / run / bundle / loadout / from_pattern) ───

    #[tool(
        description = "Skill library for pre-built agent workflows. action='discover': search for a skill BEFORE solving a complex problem; action='bundle': prepare a host-aware capability bundle for a task query; action='loadout': resolve a DispatchProfile's sparse skill loadout plus capability bundle; action='from_pattern': create a disabled/pending skill candidate from a projected continuity pattern; action='run': execute a named skill by ID. Delegate tool profiles may use only discover/run/bundle. Always discover/bundle before writing custom multi-step logic."
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
        description = "GitHub operations: repo_view, issue_list, issue_read, issue_create, issue_comment, issue_label, issue_freshness_scan, pr_list, pr_read, pr_comments, pr_comment, pr_review_digest, safe_merge, ship, close_loop, link_pr, pr_status, pr_handoff, release_note. PR actions accept repo+number or pr_ref='owner/repo#123' / GitHub PR URL. close_loop writes a referenced wiki closure and, unless dry_run=true, performs the real comment/pattern-feedback fan-out and records the terminal flow marker only after the wiki write succeeds; dry_run=true returns only the references and promotion plan with no writes. issue_comment/pr_comment post a comment to an existing issue/PR (the closure-loop write-back); issue_comment requires number+body, and pr_comment requires a PR target+body; both support dry_run=true for a preview without posting. pr_comments returns review submissions plus inline review comments. pr_review_digest filters bot/reviewer comments (author_filter defaults to gemini), writes .tachi/reviews digest artifacts by default, and returns memory/handbook candidates plus a leader-verdict routing plan for PR comments, GitHub issues, feedback rules, guide/wiki promotion, repo docs/specs, and eval evidence. safe_merge is for GitHub PR merges, returns requested_mode=preview unless confirm=true and dry_run!=true, reports merge_attempted/merge_executed separately, and records caller-supplied tests_run into the verification ledger when flow_id is supplied and no ledger exists. ship has two modes: mechanical (non-empty files + commit_message) stages an exact file list, commits with a caller-authored message, and creates a PR only when pr_title and pr_body are both supplied; contract (empty files, no commit_message) builds a zero-prose PR title/body from git log, issue_ref, and tests_run, then pushes and opens the PR. link_pr/pr_status/pr_handoff/release_note are the canonical GitHub lifecycle surface for issue→PR→release flow artifacts. When flow_id is supplied, safe_merge/pr_status consume .tachi/runs/<flow_id>/verification.json from tachi_verify; standard/strict wait on missing required verification and block on failed/stale verification. Requires GH_TOKEN in Vault or environment."
    )]
    pub(crate) async fn tachi_gh(
        &self,
        Parameters(params): Parameters<TachiGhParams>,
    ) -> Result<String, String> {
        handle_tachi_gh(self, params).await
    }

    // ─── Tachi Verify: background verification evidence ledger ─────────────

    #[tool(
        description = "Background verification ledger. action='start' seeds pending checks; action='record' stores results from external runners (gitleaks, cargo check, clippy, tests); action='status'/'board' reads .tachi/runs/<flow_id>/verification.json. Safe-merge consumes required checks for the matching flow/head SHA: failed or stale required checks block merges, and missing required checks wait in standard/strict mode."
    )]
    pub(crate) async fn tachi_verify(
        &self,
        Parameters(params): Parameters<TachiVerifyParams>,
    ) -> Result<String, String> {
        handle_tachi_verify(self, params).await
    }

    // ─── Tachi Staff: external staffing facade ──────────────────────────────

    #[tool(
        description = "External staffing: start a worker via the canonical dispatch kernel, or read a worker's canonical status receipt. start requires a typed staffing_reason (native-first exception); execution detail is resolved by profile/policy, not the caller."
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
        //   - status: REQUIRES dispatch_id; staffing_reason is IGNORED and
        //     MUST NOT be required (a read-only probe is never forced to
        //     fabricate a reason).
        let action = params.action.trim().to_ascii_lowercase();
        let format = params.format.clone();
        let raw = match action.as_str() {
            "start" => {
                let task = params.task.unwrap_or_default();
                if task.trim().is_empty() {
                    return Err(
                        "tachi_staff: action='start' requires a non-empty `task`".to_string()
                    );
                }
                // #1319 admission gate: a start request MUST carry a typed
                // staffing_reason. None is rejected with zero artifacts (no
                // run dir, no status.json) — same fail-closed shape as the
                // retired require_tachi_dispatch_reason. This runs before
                // staff_start → handle_tachi_dispatch, so no receipt is seeded.
                let staffing_reason = params.staffing_reason.ok_or_else(|| {
                    "tachi_staff: action='start' requires a typed staffing_reason (the native-first exception); use the host harness's native subagent for ordinary delegation, or set staffing_reason to explicit_user_request / durable_cross_session / cross_device_remote / native_subagent_unavailable for an admitted exception; zero staffing or dispatch artifacts were created.".to_string()
                })?;
                let request = crate::staffing_ops::StaffStartRequest {
                    task,
                    staffing_reason,
                    profile: params.profile,
                    worker: params.worker,
                    project: params.project,
                    stage: params.stage,
                    issue_ref: params.issue_ref,
                    pr_ref: params.pr_ref,
                    flow_id: params.flow_id,
                    // tachi#1675 PR1 Seam B.
                    recommendation_ref: params.recommendation_ref,
                };
                crate::staffing_ops::staff_start(self, request).await?
            }
            "status" => {
                // status ignores staffing_reason entirely — a read-only probe
                // never needs a reason and is not pressured to fabricate one.
                let dispatch_id = params.dispatch_id.ok_or_else(|| {
                    "tachi_staff: action='status' requires a `dispatch_id`".to_string()
                })?;
                crate::staffing_ops::staff_status(
                    self,
                    crate::staffing_ops::StaffStatusRequest { dispatch_id },
                )
                .await?
            }
            other => {
                return Err(format!(
                    "tachi_staff: unknown action '{other}' (start|status)"
                ))
            }
        };
        format_facade_response(
            &format!("Tachi staff {}", action),
            &action,
            &raw,
            format.as_deref(),
            false,
        )
    }
}
