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

    // ─── Facade: task (plan / recommend / dispatch / board / merge / lifecycle)

    #[tool(
        description = "Task memory, policy, and ledger facade. The host harness's native subagent is the DEFAULT for ordinary local delegation; merely having Tachi, wanting parallelism/tracking, or selecting a vendor never justifies Tachi dispatch. action='dispatch' is legacy durable/remote execution and requires dispatch_reason=explicit_user_request|durable_cross_session|cross_device_remote|native_subagent_unavailable. action='briefing': feature-scoped handoff board with docs/specs, run artifacts, board state, wiki, memory fragments, eval evidence, and next action; action='doc_index': project-first layered source index; action='plan': search memory/wiki and produce a todo list; action='recommend': advisory profile/card evidence, not permission or an instruction to dispatch; action='route_simulate'/'proposals'/'review_proposal'/'apply_proposals': routing-policy evaluation; action='profiles'/'profile'/'card': inspect cards; action='status'/'cancel'/'wait': manage an already explicit Tachi dispatch; action='complete': record evaluated completion evidence; action='board': view ledger state; action='intake'/'cycle_status'/'cycle_plan': lifecycle read models; action='ux_matrix'/'build_references'/'close_loop': closure records; action='merge': local Tachi-dispatched worktree merge only. GitHub PR lifecycle is tachi_gh only. Harness-native subagent outcomes can be mirrored into the existing eval/ledger surfaces without Tachi owning their process lifecycle."
    )]
    pub(crate) async fn tachi_task(
        &self,
        Parameters(params): Parameters<TachiTaskParams>,
    ) -> Result<String, String> {
        handle_tachi_task_facade(self, params).await
    }

    // ─── GitHub MCP Proxy Tools ─────────────────────────────────────────────

    #[tool(
        description = "GitHub operations: repo_view, issue_list, issue_read, issue_create, issue_comment, pr_list, pr_read, pr_comments, pr_comment, pr_review_digest, safe_merge, ship, link_pr, pr_status, pr_handoff, release_note. PR actions accept repo+number or pr_ref='owner/repo#123' / GitHub PR URL. issue_comment/pr_comment post a comment to an existing issue/PR (the closure-loop write-back); issue_comment requires number+body, and pr_comment requires a PR target+body; both support dry_run=true for a preview without posting. pr_comments returns review submissions plus inline review comments. pr_review_digest filters bot/reviewer comments (author_filter defaults to gemini), writes .tachi/reviews digest artifacts by default, and returns memory/handbook candidates plus a leader-verdict routing plan for PR comments, GitHub issues, feedback rules, guide/wiki promotion, repo docs/specs, and eval evidence. safe_merge is for GitHub PR merges, returns requested_mode=preview unless confirm=true and dry_run!=true, reports merge_attempted/merge_executed separately, and records caller-supplied tests_run into the verification ledger when flow_id is supplied and no ledger exists. ship has two modes: mechanical (non-empty files + commit_message) stages an exact file list, commits with a caller-authored message, and creates a PR only when pr_title and pr_body are both supplied; contract (empty files, no commit_message) builds a zero-prose PR title/body from git log, issue_ref, and tests_run, then pushes and opens the PR. link_pr/pr_status/pr_handoff/release_note are the canonical GitHub lifecycle surface for issue→PR→release flow artifacts. When flow_id is supplied, safe_merge/pr_status consume .tachi/runs/<flow_id>/verification.json from tachi_verify; standard/strict wait on missing required verification and block on failed/stale verification. Use approve_merge/tachi_task for local dispatched worktree merges. Requires GH_TOKEN in Vault or environment."
    )]
    pub(crate) async fn tachi_gh(
        &self,
        Parameters(params): Parameters<TachiGhParams>,
    ) -> Result<String, String> {
        handle_tachi_gh(self, params).await
    }

    // ─── Tachi Arena: tracked worker mission document ledger ────────────────

    #[tool(
        description = "Tracked worker mission ledger. The default action='spawn' path uses launch=false: it writes mission prompt.md/status.json and returns a tracked prompt for the host harness's native subagent. launch=true is only for explicit durable/remote/native-unavailable exceptions, requires dispatch_reason for launch-capable lanes, and bridges supported harnesses through Tachi dispatch. action='open' creates an arena; action='board' lists compact arena/mission state plus linked dispatch summaries; action='collect' reads worker result.md or linked dispatch result.md and returns a completion draft; action='abort' marks a mission stopped; action='reap' marks stale ready/running missions using timeout_secs when supplied; action='close' closes and summarizes the arena. Arena owns run documents; memory owns distilled knowledge."
    )]
    pub(crate) async fn tachi_arena(
        &self,
        Parameters(params): Parameters<TachiArenaParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        let format = params.format.clone();
        let raw = handle_tachi_arena(self, params).await?;
        format_facade_response(
            &format!("Tachi arena {}", action),
            &action,
            &raw,
            format.as_deref(),
            false,
            false,
        )
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

    // ─── Tachi Shell: skill-gated flow orchestration facade ─────────────────

    #[tool(
        description = "Skill-gated flow packet and ledger orchestration for multi-step projects. This does not replace the host harness's native subagents. action='brainstorm': explore options; action='plan': produce a decision-complete plan; action='dispatch': prepare bounded handoff packets, using native workers by default (legacy async Tachi execution is only for an explicit durable/remote exception); action='kanban': check progress; action='review': gate before merge; action='ship': release. Each stage-bearing action injects the corresponding skill SOP and writes an instruction.md packet under .tachi/runs/<flow_id>/."
    )]
    pub(crate) async fn tachi_shell(
        &self,
        Parameters(params): Parameters<TachiShellParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        let format = params.format.clone();
        let raw = crate::shell_ops::handle_tachi_shell(self, params).await?;
        format_facade_response(
            &format!("Tachi shell {}", action),
            &action,
            &raw,
            format.as_deref(),
            false,
            false,
        )
    }
}
