use super::*;

#[tool_router(router = continuity_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Unified memory facade. Actions: search (hybrid recall), get (fetch one memory by id), save (persist entry; prefer tachi_save for decisions), extract_facts (LLM atomize logs), briefing (session start), checkpoint (handoff), alerts (warnings when stuck), ask (Q&A over evidence), consolidate (merge duplicates), progress (long-running flow), readiness (health/tools). Use tachi_briefing for zero-arg briefing alias."
    )]
    pub(crate) async fn tachi_memory(
        &self,
        Parameters(params): Parameters<TachiMemoryParams>,
    ) -> Result<String, String> {
        crate::facade_memory_ops::handle_tachi_memory(self, params).await
    }

    #[tool(
        description = "Append/query/project domain-neutral continuity events. action='emit' records pattern/outcome/affect/bonding/lorebook/evidence/project-cycle events with authority/effect metadata; action='query' lists recent events; action='metrics' returns read-only continuity metrics such as challenge_rate; action='project' idempotently materializes candidate events into stable memory projections; action='context' returns projected continuity memory for prompt/read-model use; action='label_eval' compares session.outcome labels to session.outcome.review gold labels. Affect/emotion projections are tone/reminder only and must not mutate facts, scores, routing, or execution."
    )]
    pub(crate) async fn tachi_event(
        &self,
        Parameters(params): Parameters<TachiEventParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        if matches!(
            action.as_str(),
            "query" | "metrics" | "context" | "label_eval"
        ) {
            if let Some(body) =
                crate::cli_client::maybe_forward_server_read(self, "tachi_event", &params).await?
            {
                return Ok(body);
            }
        } else if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "tachi_event", &params).await?
        {
            return Ok(body);
        }
        handle_tachi_event(self, params).await
    }

    #[tool(
        description = "Domain adapter facade for repo-derived continuity shapes. Actions: lorebook_import (import RomanBath/SillyTavern lorebook entries into tachi_event world_book projections). Keeps repo conventions out of generic memory core."
    )]
    pub(crate) async fn tachi_domain_adapter(
        &self,
        Parameters(params): Parameters<TachiDomainAdapterParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "tachi_domain_adapter", &params)
                .await?
        {
            return Ok(body);
        }
        crate::domain_adapter_ops::handle_tachi_domain_adapter(self, params).await
    }

    /// Zero-param briefing alias. Call at the start of any non-trivial task to
    /// load prior session context without having to remember the action name.
    #[tool(
        description = "Call at the START of any non-trivial task. Returns project-scoped memories, pending global handoffs (cross-project issue board), wiki, kanban, and checkpoints. Zero params — compact mode; uses current git repo. For all-global briefing use tachi_memory(action='briefing', scope='all')."
    )]
    pub(crate) async fn tachi_briefing(&self) -> Result<String, String> {
        let named = crate::memory_search_ops::resolve_workspace_named_project();
        let query = named
            .as_ref()
            .map(|name| format!("{name} current task recent decisions blockers next steps"));
        let params = TachiMemoryParams {
            action: "briefing".to_string(),
            format: Some("markdown".to_string()),
            query,
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
            keywords: vec![],
            entities: vec![],
            importance: None,
            retention_policy: None,
            kind: None,
            path: None,
            id: None,
            force: false,
            source: None,
            valid_from: None,
            valid_until: None,
            flow_id: None,
            event: None,
            state: None,
            project: named,
            domain: None,
            metadata: None,
            emit_continuity: false,
            files: Vec::new(),
            compact: true,
            proposal_id: None,
            review_status: None,
            notes: None,
            confirm: false,
            state_filter: None,
            content: None,
            ingest_type: "source".to_string(),
            source_url: None,
            auto_chunk: true,
            auto_summarize: true,
            auto_link: true,
            chunk_size_chars: 1200,
            chunk_overlap_chars: 120,
            conversation_id: None,
            turn_id: None,
            event_type: None,
            messages: Vec::new(),
        };
        crate::facade_memory_ops::handle_tachi_memory(self, params).await
    }

    #[tool(
        description = "Unified search across wiki and memory. Use scope to target 'wiki', 'memory', or 'all' (default)."
    )]
    pub(crate) async fn tachi_search(
        &self,
        Parameters(params): Parameters<TachiSearchParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "tachi_search", &params).await?
        {
            return Ok(body);
        }
        crate::facade_search_ops::handle_tachi_search(self, params).await
    }

    #[tool(
        description = "Search the live web through Tachi Hub. Uses vc:web_search routing and automatically falls back across available backends."
    )]
    pub(crate) async fn tachi_web_search(
        &self,
        Parameters(params): Parameters<TachiWebSearchParams>,
    ) -> Result<String, String> {
        crate::web_search_ops::handle_tachi_web_search(self, params).await
    }

    #[tool(
        description = "Research verb (tachi#530). P1 feed mode: action='feed' with a url → fetch the page (UNTRUSTED data), digest it, and emit an impact-routing PROPOSAL. Writes report artifacts to a run dir only; every routed finding is advisory and the leader/owner ratifies before anything lands (2-gate). No fan-out, no auto-escalation from ask."
    )]
    pub(crate) async fn tachi_research(
        &self,
        Parameters(params): Parameters<TachiResearchParams>,
    ) -> Result<String, String> {
        crate::research_ops::handle_tachi_research(self, params).await
    }

    #[tool(
        description = "Save a conclusion (preference, decision, or lesson) after any meaningful step — do NOT wait for session end. For raw logs or general text, use tachi_memory(action='extract_facts')."
    )]
    pub(crate) async fn tachi_save(
        &self,
        Parameters(params): Parameters<TachiSaveParams>,
    ) -> Result<String, String> {
        let raw = crate::facade_save_ops::handle_tachi_save(self, params.clone()).await?;
        crate::facade_save_ops::finalize_tachi_save_response(&params, &raw, None)
    }
}
