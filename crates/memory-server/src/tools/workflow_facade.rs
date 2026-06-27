use super::*;

#[tool_router(router = workflow_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    // ─── Facade: skill (discover / run / bundle / loadout / from_pattern) ───

    #[tool(
        description = "Skill library for pre-built agent workflows. action='discover': search for a skill BEFORE solving a complex problem; action='bundle': prepare a host-aware capability bundle for a task query; action='loadout': resolve a DispatchProfile's sparse skill loadout plus capability bundle; action='from_pattern': create a disabled/pending skill candidate from a projected continuity pattern; action='run': execute a named skill by ID. Always discover/bundle before writing custom multi-step logic."
    )]
    pub(crate) async fn tachi_skill(
        &self,
        Parameters(params): Parameters<TachiSkillParams>,
    ) -> Result<String, String> {
        handle_tachi_skill_facade(self, params).await
    }

    // ─── Facade: task (plan / recommend / dispatch / board / merge / lifecycle)

    #[tool(
        description = "Task management facade for agent work. action='briefing': feature-scoped handoff board with docs/specs, run artifacts, board state, wiki, memory fragments, eval evidence, and next action; action='doc_index': project-first layered source index across GitHub issues/PRs, repo docs/specs, project wiki, global guide, feedback rules, eval, and runtime artifacts; action='plan': search memory/wiki and produce a todo list before complex work; action='recommend': choose a dispatch profile/agent/tool surface from the task, risk, and live eval evidence before assigning external workers; action='route_simulate': replay recent /eval rows across current, cost_sensitive, and quality_first policies without mutating routing; action='proposals': generate/list route-policy and loadout-evolution proposals from replay/eval evidence; action='review_proposal': approve/reject a proposal; action='apply_proposals': persist an approved route-policy rule or project an approved loadout-evolution proposal into a profile/card overlay, requiring confirm=true; action='profiles'/'profile'/'card': inspect built-in dispatch profiles plus reviewed overlays; action='dispatch': spawn a delegate agent from either agent or profile; action='status': read one dispatch ledger and query backend-local status when supported; action='cancel': request cooperative cancellation for a dispatch backend that supports it; action='wait': block on a dispatch_id until terminal state or timeout; action='complete': record evaluated completion evidence and link flow_id+dispatch_id back to the dispatch card; action='board': view task status; action='intake': bind/read a GitHub issue and create/refresh a flow; action='link_pr': attach a PR to a flow; action='pr_status': preview GitHub PR safe-merge status without merging, optionally persisting flow status; action='pr_handoff': write a PR body/branch handoff with verification evidence and known gaps; action='release_note': synthesize release notes; action='ux_matrix': write/read a feature workflow UX checklist; action='build_references': preview issue/doc/related refs; action='close_loop': write durable issue/doc/wiki closure; action='merge': local dispatched worktree git merge only. To execute GitHub PR merges use tachi_gh(action='safe_merge'). Typical worker flow: intake → briefing/doc_index → ux_matrix → plan/recommend/route_simulate/proposals → dispatch → status/wait/board → complete/eval → pr_handoff → link_pr → pr_status → release_note → close_loop → merge."
    )]
    pub(crate) async fn tachi_task(
        &self,
        Parameters(params): Parameters<TachiTaskParams>,
    ) -> Result<String, String> {
        handle_tachi_task_facade(self, params).await
    }

    // ─── GitHub MCP Proxy Tools ─────────────────────────────────────────────

    #[tool(
        description = "GitHub operations: repo_view, issue_list, issue_read, issue_create, issue_comment, pr_list, pr_read, pr_comments, pr_comment, pr_review_digest, safe_merge. issue_comment/pr_comment post a comment to an existing issue/PR (the closure-loop write-back); both require number+body and support dry_run=true for a preview without posting. pr_comments returns review submissions plus inline review comments. pr_review_digest filters bot/reviewer comments (author_filter defaults to gemini), writes .tachi/reviews digest artifacts by default, and returns memory/handbook candidates plus a leader-verdict routing plan for PR comments, GitHub issues, feedback rules, guide/wiki promotion, repo docs/specs, and eval evidence. safe_merge is for GitHub PR merges, returns requested_mode=preview unless confirm=true and dry_run!=true, and reports merge_attempted/merge_executed separately. When flow_id is supplied, safe_merge consumes .tachi/runs/<flow_id>/verification.json from tachi_verify; standard/strict wait on missing required verification and block on failed/stale verification. Use approve_merge/tachi_task for local dispatched worktree merges. Requires GH_TOKEN in Vault or environment."
    )]
    pub(crate) async fn tachi_gh(
        &self,
        Parameters(params): Parameters<TachiGhParams>,
    ) -> Result<String, String> {
        handle_tachi_gh(self, params).await
    }

    // ─── Tachi Arena: tracked worker mission document ledger ────────────────

    #[tool(
        description = "Tracked worker mission ledger. action='open' creates .tachi/arena/<arena_id>/; action='spawn' writes mission prompt.md/status.json and returns a tracked prompt, or launch=true bridges supported harnesses through tachi_task dispatch; action='board' lists arenas/missions plus linked dispatch state; action='collect' reads worker result.md or linked dispatch result.md and returns a completion draft; action='abort' marks a mission stopped; action='reap' marks stale ready/running missions; action='close' closes and summarizes the arena. Arena owns run documents; memory owns distilled knowledge."
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
        description = "Skill-gated flow orchestration for multi-step projects. WHEN: use tachi_shell when a task needs a full lifecycle with formal skill SOPs or multiple coordinated agents. Use tachi_task for single-agent work. action='brainstorm': explore options before committing to a design; action='plan': produce a decision-complete plan with validated structure; action='dispatch': assign work slices to agents; action='kanban': check progress; action='review': gate before merge; action='ship': release. Each stage-bearing action injects the corresponding Superpowers skill SOP and writes an instruction.md packet under .tachi/runs/<flow_id>/."
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
        )
    }
}
