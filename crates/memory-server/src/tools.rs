//! Tool implementations for the Memory MCP server.
//!
//! Extracted from `main.rs` (Phase 4 of v1.0 cleanup) to keep the crate root
//! focused on bootstrap/state and delegate the ~120 `#[tool]` wrappers here.
//! Every method in this file is a thin shim that delegates to a `handle_*`
//! function in one of the `*_ops` siblings — no business logic lives here.

use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use crate::agent_profile_ops::handle_tachi_profile;
use crate::arena_ops::handle_tachi_arena;
use crate::capability_ops::handle_prepare_capability_bundle;
use crate::copilot_ops::{
    handle_tachi_feature_briefing, handle_tachi_progress_check, handle_tachi_task_brief,
    handle_tachi_wiki_search, handle_tachi_wiki_write,
};
use crate::dlq_ops::{handle_dlq_list, handle_dlq_retry};
use crate::event_ops::handle_tachi_event;
use crate::foundry_ops::{
    handle_list_agent_evolution_proposals, handle_project_agent_profile,
    handle_queue_agent_evolution, handle_review_agent_evolution_proposal,
    handle_synthesize_agent_evolution,
};
use crate::foundry_runtime_ops::{
    handle_capture_session, handle_compact_context, handle_compact_rollup,
    handle_compact_session_memory, handle_recall_context, handle_section_build,
};
use crate::gh_ops::handle_tachi_gh;
use crate::graph_state_ops::{
    handle_add_edge, handle_get_edges, handle_get_state, handle_memory_graph, handle_set_state,
};
use crate::handoff_ops::{
    handle_handoff_check, handle_handoff_leave, handle_handoff_promote_issue,
};
use crate::hub_ops::{handle_hub_discover, handle_run_skill, handle_skill_from_pattern};
use crate::kanban::{
    handle_check_inbox, handle_post_card, handle_update_card, CheckInboxParams, PostCardParams,
    UpdateCardParams,
};
use crate::pipeline_ops::{
    handle_extract_facts, handle_get_pipeline_status, handle_ingest, handle_ingest_event,
    handle_ingest_source, handle_sync_memories,
};
use crate::project_db_ops::handle_tachi_init_project_db;
use crate::skill_chain_ops::handle_chain_skills;
use crate::tool_params::*;
use crate::verify_ops::handle_tachi_verify;
use crate::wiki_ops::{
    collect_wiki_browse_value, collect_wiki_read_value, collect_wiki_search_value,
    handle_wiki_browse, handle_wiki_ingest, handle_wiki_lint, handle_wiki_read, handle_wiki_search,
};
use crate::{AgentProfile, MemoryServer};

const TASK_WAIT_INITIAL_POLL_DELAY: StdDuration = StdDuration::from_millis(250);
const TASK_WAIT_MAX_POLL_DELAY: StdDuration = StdDuration::from_secs(2);

mod dispatch_complete_defaults;
mod domain_facade;
mod formatting;
mod hub_facade;
mod memory_facade;
mod pack_facade;
mod sandbox_facade;
mod skill_discovery;
mod skill_facade;
mod task_facade;
mod task_router;
mod vault_facade;
mod wiki_facade;

#[cfg(test)]
mod tests;

use self::dispatch_complete_defaults::*;
use self::formatting::*;
use self::skill_discovery::*;
use self::skill_facade::*;
use self::task_facade::*;
use self::task_router::*;
use self::wiki_facade::*;

pub(crate) use self::task_facade::build_task_pr_status_gh_params;
#[cfg(test)]
pub(crate) use self::task_facade::resolve_task_pr_status_target;

// ─── Tool Implementations ────────────────────────────────────────────────────────

#[tool_router(vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Add or update an edge in the memory graph. Edges represent causal, temporal, or entity relationships between memories."
    )]
    pub(crate) async fn add_edge(
        &self,
        Parameters(params): Parameters<AddEdgeParams>,
    ) -> Result<String, String> {
        handle_add_edge(self, params).await
    }

    #[tool(
        description = "Get edges connected to a memory entry. Returns causal, temporal, and entity relationship edges."
    )]
    pub(crate) async fn get_edges(
        &self,
        Parameters(params): Parameters<GetEdgesParams>,
    ) -> Result<String, String> {
        handle_get_edges(self, params).await
    }

    #[tool(
        description = "Inspect a read-only neighborhood from the memory graph, seeded by memory id or a search query. Returns seed nodes, neighboring nodes, and connecting edges."
    )]
    pub(crate) async fn memory_graph(
        &self,
        Parameters(params): Parameters<MemoryGraphParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "memory_graph", &params).await?
        {
            return Ok(body);
        }
        handle_memory_graph(self, params).await
    }

    #[tool(
        description = "Run health checks over wiki memories and skill graph state. Returns orphan nodes, contradiction candidates, stale nodes, missing edge hints, and current skill quality guard status."
    )]
    pub(crate) async fn wiki_lint(
        &self,
        Parameters(params): Parameters<WikiLintParams>,
    ) -> Result<String, String> {
        handle_wiki_lint(self, params).await
    }

    #[tool(
        description = "Write a durable wiki entry under /wiki with sane defaults for path, retention, metadata, and auto-linking."
    )]
    pub(crate) async fn tachi_wiki_write(
        &self,
        Parameters(params): Parameters<WikiWriteParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "tachi_wiki_write", &params).await?
        {
            return Ok(body);
        }
        handle_tachi_wiki_write(self, params).await
    }

    #[tool(
        description = "Search wiki entries under /wiki. Use this before debugging from scratch or when a prior lesson may exist."
    )]
    pub(crate) async fn tachi_wiki_search(
        &self,
        Parameters(params): Parameters<WikiSearchParams>,
    ) -> Result<String, String> {
        handle_tachi_wiki_search(self, params).await
    }

    #[tool(
        description = "Ingest a URL or local file into the wiki project DB, extract metadata when possible, and link related wiki entries by shared entities."
    )]
    pub(crate) async fn tachi_wiki_ingest(
        &self,
        Parameters(params): Parameters<TachiWikiIngestParams>,
    ) -> Result<String, String> {
        handle_wiki_ingest(self, params).await
    }

    #[tool(
        description = "Organize workspace docs: automatically classify/move files, sync task checkmarks, and rebuild docs/_index.md tree. Pass dry_run=true to preview planned moves/frontmatter/task-sync changes without modifying any files."
    )]
    pub(crate) async fn tachi_wiki_organize(
        &self,
        Parameters(params): Parameters<TachiWikiOrganizeParams>,
    ) -> Result<String, String> {
        crate::docs_ops::handle_wiki_organize(self, &params.dir_path, params.dry_run).await
    }

    #[tool(
        description = "Prepare a task brief before non-trivial work: relevant wiki lessons, memory hits, intent, selected_sops, tool_plan, lightweight skill suggestions, and debugging checklist."
    )]
    pub(crate) async fn tachi_task_brief(
        &self,
        Parameters(params): Parameters<TaskBriefParams>,
    ) -> Result<String, String> {
        handle_tachi_task_brief(self, params).await
    }

    #[tool(
        description = "Check whether an agent is stuck after repeated attempts. Returns reframe advice, relevant wiki hits, and an ask-codex prompt when useful. Pass flow_id to append a progress_check event to .tachi/runs/<flow_id>/progress.jsonl."
    )]
    pub(crate) async fn tachi_progress_check(
        &self,
        Parameters(params): Parameters<ProgressCheckParams>,
    ) -> Result<String, String> {
        handle_tachi_progress_check(self, params).await
    }

    #[tool(
        description = "Search the wiki knowledge base for relevant entries. The wiki contains distilled knowledge from past development sessions organized by category (quant, engineering, agent, product). Supports short category aliases like 'quant', 'strategy', 'tachi', 'debugging', etc."
    )]
    pub(crate) async fn wiki_search(
        &self,
        Parameters(params): Parameters<WikiSearchParams>,
    ) -> Result<String, String> {
        handle_wiki_search(self, params).await
    }

    #[tool(
        description = "Browse wiki entries by category. Without a category, returns category stats (counts per path). With a category, lists entries under that path. Supports short aliases like 'quant', 'engineering', 'tachi', etc."
    )]
    pub(crate) async fn wiki_browse(
        &self,
        Parameters(params): Parameters<WikiBrowseParams>,
    ) -> Result<String, String> {
        handle_wiki_browse(self, params)
    }

    #[tool(description = "Set a key-value pair in server state (stored in hard_state table).")]
    pub(crate) async fn set_state(
        &self,
        Parameters(params): Parameters<SetStateParams>,
    ) -> Result<String, String> {
        handle_set_state(self, params).await
    }

    #[tool(description = "Get a value from server state by key.")]
    pub(crate) async fn get_state(
        &self,
        Parameters(params): Parameters<GetStateParams>,
    ) -> Result<String, String> {
        handle_get_state(self, params).await
    }

    #[tool(description = "Extract structured facts from text using LLM and save to memory.")]
    pub(crate) async fn extract_facts(
        &self,
        Parameters(params): Parameters<ExtractFactsParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "extract_facts", &params).await?
        {
            return Ok(body);
        }
        handle_extract_facts(self, params).await
    }

    #[tool(description = "Ingest a conversation event and extract facts from messages.")]
    pub(crate) async fn ingest_event(
        &self,
        Parameters(params): Parameters<IngestEventParams>,
    ) -> Result<String, String> {
        handle_ingest_event(self, params).await
    }

    #[tool(
        description = "Unified ingest entrypoint for conversation events and source documents. Use ingest_type to select event vs source behavior."
    )]
    pub(crate) async fn ingest(
        &self,
        Parameters(params): Parameters<IngestParams>,
    ) -> Result<String, String> {
        handle_ingest(self, params).await
    }

    #[tool(
        description = "Batch ingest source content with optional chunking, enrichment, and graph edge building."
    )]
    pub(crate) async fn ingest_source(
        &self,
        Parameters(params): Parameters<IngestSourceParams>,
    ) -> Result<String, String> {
        handle_ingest_source(self, params).await
    }

    #[tool(description = "Get pipeline status and statistics.")]
    pub(crate) async fn get_pipeline_status(&self) -> Result<String, String> {
        handle_get_pipeline_status(self).await
    }

    #[tool(
        description = "Get only new or changed memories since last sync for this agent. Returns incremental diff to save tokens. Use agent_id to identify your agent uniquely."
    )]
    pub(crate) async fn sync_memories(
        &self,
        Parameters(params): Parameters<SyncMemoriesParams>,
    ) -> Result<String, String> {
        handle_sync_memories(self, params).await
    }

    #[tool(description = "Post a kanban card from one agent to another.")]
    pub(crate) async fn post_card(
        &self,
        Parameters(params): Parameters<PostCardParams>,
    ) -> Result<String, String> {
        handle_post_card(self, params).await
    }

    #[tool(description = "Check kanban inbox for a target agent.")]
    pub(crate) async fn check_inbox(
        &self,
        Parameters(params): Parameters<CheckInboxParams>,
    ) -> Result<String, String> {
        handle_check_inbox(self, params).await
    }

    #[tool(description = "Update status of a kanban card.")]
    pub(crate) async fn update_card(
        &self,
        Parameters(params): Parameters<UpdateCardParams>,
    ) -> Result<String, String> {
        handle_update_card(self, params).await
    }

    #[tool(
        description = "Recall structured memory context for an active agent turn. Returns ranked results plus a ready-to-inject prepend_context block."
    )]
    pub(crate) async fn recall_context(
        &self,
        Parameters(params): Parameters<RecallContextParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "recall_context", &params).await?
        {
            return Ok(body);
        }
        handle_recall_context(self, params).await
    }

    #[tool(
        description = "Capture durable memories from a recent session window. Extracts structured memories, embeds them inside Tachi, and writes them to the configured store."
    )]
    pub(crate) async fn capture_session(
        &self,
        Parameters(params): Parameters<CaptureSessionParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "capture_session", &params).await?
        {
            return Ok(body);
        }
        handle_capture_session(self, params).await
    }

    #[tool(
        description = "Compact a soon-to-be-evicted session window into a ready-to-inject context block. Designed for host runtimes that know when token pressure requires compaction."
    )]
    pub(crate) async fn compact_context(
        &self,
        Parameters(params): Parameters<CompactContextParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "compact_context", &params).await?
        {
            return Ok(body);
        }
        handle_compact_context(self, params).await
    }

    #[tool(
        description = "Render a structured context section with explicit layer and cache-boundary markers. Useful for host runtimes assembling static/session/live prompt sections."
    )]
    pub(crate) async fn section_build(
        &self,
        Parameters(params): Parameters<SectionBuildParams>,
    ) -> Result<String, String> {
        handle_section_build(self, params).await
    }

    #[tool(
        description = "Roll up multiple compacted session artifacts into a new compact summary block, preserving salient topics and durable signals for later reinjection."
    )]
    pub(crate) async fn compact_rollup(
        &self,
        Parameters(params): Parameters<CompactRollupParams>,
    ) -> Result<String, String> {
        handle_compact_rollup(self, params).await
    }

    #[tool(
        description = "Persist a compacted session artifact and its durable signals into Tachi memory, then optionally queue Foundry maintenance jobs."
    )]
    pub(crate) async fn compact_session_memory(
        &self,
        Parameters(params): Parameters<CompactSessionMemoryParams>,
    ) -> Result<String, String> {
        handle_compact_session_memory(self, params).await
    }

    #[tool(
        description = "Synthesize agent evolution proposals from canonical profile documents and evidence. Returns structured JSON proposals; use dry_run=true to inspect the normalized request without calling the model."
    )]
    pub(crate) async fn synthesize_agent_evolution(
        &self,
        Parameters(params): Parameters<SynthesizeAgentEvolutionParams>,
    ) -> Result<String, String> {
        handle_synthesize_agent_evolution(self, params).await
    }

    #[tool(
        description = "Queue an agent evolution synthesis job. Persists job state and stores generated proposals for later review."
    )]
    pub(crate) async fn queue_agent_evolution(
        &self,
        Parameters(params): Parameters<SynthesizeAgentEvolutionParams>,
    ) -> Result<String, String> {
        handle_queue_agent_evolution(self, params).await
    }

    #[tool(
        description = "List persisted agent evolution proposals for a target agent. Optionally filter by review status."
    )]
    pub(crate) async fn list_agent_evolution_proposals(
        &self,
        Parameters(params): Parameters<ListAgentEvolutionProposalsParams>,
    ) -> Result<String, String> {
        handle_list_agent_evolution_proposals(self, params).await
    }

    #[tool(
        description = "Review a persisted agent evolution proposal by marking it approved, rejected, or applied."
    )]
    pub(crate) async fn review_agent_evolution_proposal(
        &self,
        Parameters(params): Parameters<ReviewAgentEvolutionProposalParams>,
    ) -> Result<String, String> {
        handle_review_agent_evolution_proposal(self, params).await
    }

    #[tool(
        description = "Project approved agent evolution proposals into host documents. Returns projected content and can optionally write back to disk paths."
    )]
    pub(crate) async fn project_agent_profile(
        &self,
        Parameters(params): Parameters<ProjectAgentProfileParams>,
    ) -> Result<String, String> {
        handle_project_agent_profile(self, params).await
    }

    #[tool(
        description = "Import and render canonical AgentProfilePack projections for user-agent alignment. Read-only: returns dry-run AGENTS.md / CLAUDE.md / GEMINI.md / Cursor/OpenClaw projections or a bounded runtime context block; it never writes files."
    )]
    pub(crate) async fn tachi_profile(
        &self,
        Parameters(params): Parameters<TachiProfileParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "tachi_profile", &params).await?
        {
            return Ok(body);
        }
        handle_tachi_profile(self, params).await
    }

    #[tool(
        description = "Initialize a project-scoped Tachi memory DB under the current or target git repository."
    )]
    pub(crate) async fn tachi_init_project_db(
        &self,
        Parameters(params): Parameters<InitProjectDbParams>,
    ) -> Result<String, String> {
        handle_tachi_init_project_db(self, params).await
    }

    // ─── Agent Profile ───────────────────────────────────────────────────────

    #[tool(
        description = "Register this agent session with an identity profile. Enables per-agent memory scoping, tool filtering, and rate limit customization."
    )]
    pub(crate) async fn agent_register(
        &self,
        Parameters(params): Parameters<AgentRegisterParams>,
    ) -> Result<String, String> {
        let profile = AgentProfile {
            agent_id: params.agent_id.clone(),
            display_name: params
                .display_name
                .unwrap_or_else(|| params.agent_id.clone()),
            capabilities: params.capabilities,
            tool_filter: params.tool_filter,
            rate_limit_rpm: params.rate_limit_rpm,
            rate_limit_burst: params.rate_limit_burst,
            registered_at: Utc::now().to_rfc3339(),
        };

        let response = serde_json::to_string(&serde_json::json!({
            "status": "registered",
            "agent_id": profile.agent_id,
            "display_name": profile.display_name,
            "capabilities": profile.capabilities,
            "tool_filter": profile.tool_filter,
            "rate_limit_rpm": profile.rate_limit_rpm,
            "rate_limit_burst": profile.rate_limit_burst,
            "registered_at": profile.registered_at,
        }))
        .map_err(|e| format!("serialize: {e}"))?;

        let mut guard = self.agent_runtime_write();
        guard.agent_profile = Some(profile);

        Ok(response)
    }

    #[tool(
        description = "Return the current agent profile for this session, or null if no agent has registered."
    )]
    pub(crate) async fn agent_whoami(
        &self,
        Parameters(_params): Parameters<AgentWhoamiParams>,
    ) -> Result<String, String> {
        let guard = self.agent_runtime_read();
        match guard.agent_profile.as_ref() {
            Some(profile) => serde_json::to_string(&profile).map_err(|e| format!("serialize: {e}")),
            None => Ok(r#"{"status":"unregistered","message":"No agent profile set. Call agent_register to identify this session."}"#.to_string()),
        }
    }

    // ─── Cross-Agent Handoff ─────────────────────────────────────────────────

    #[tool(
        description = "Leave a handoff memo for the next agent session. Contains session summary, next steps, and optional context."
    )]
    pub(crate) async fn handoff_leave(
        &self,
        Parameters(params): Parameters<HandoffLeaveParams>,
    ) -> Result<String, String> {
        handle_handoff_leave(self, params).await
    }

    #[tool(
        description = "Check for pending handoff memos from previous agent sessions. Call this at the start of a new session."
    )]
    pub(crate) async fn handoff_check(
        &self,
        Parameters(params): Parameters<HandoffCheckParams>,
    ) -> Result<String, String> {
        handle_handoff_check(self, params).await
    }

    // ─── Skill Chaining (Unix Pipe-Style Composition) ────────────────────────

    #[tool(
        description = "Execute a chain of skills in sequence (Unix pipe style). Output of each skill feeds as input to the next."
    )]
    pub(crate) async fn chain_skills(
        &self,
        Parameters(params): Parameters<ChainSkillsParams>,
    ) -> Result<String, String> {
        handle_chain_skills(self, params).await
    }

    // ─── Dead Letter Queue Tools ──────────────────────────────────────────────

    #[tool(
        description = "List dead letter queue entries (failed tool calls). Filter by status: pending, retrying, resolved, abandoned."
    )]
    pub(crate) async fn dlq_list(
        &self,
        Parameters(params): Parameters<DlqListParams>,
    ) -> Result<String, String> {
        handle_dlq_list(self, params).await
    }

    #[tool(
        description = "Manually retry a dead letter queue entry by its ID. Re-dispatches the failed tool call."
    )]
    pub(crate) async fn dlq_retry(
        &self,
        Parameters(params): Parameters<DlqRetryParams>,
    ) -> Result<String, String> {
        handle_dlq_retry(self, params).await
    }

    // ─── Facade tools (consolidated surface for Antigravity minimal profile) ──────

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
        description = "Save a conclusion (preference, decision, or lesson) after any meaningful step — do NOT wait for session end. For raw logs or general text, use tachi_memory(action='extract_facts')."
    )]
    pub(crate) async fn tachi_save(
        &self,
        Parameters(params): Parameters<TachiSaveParams>,
    ) -> Result<String, String> {
        crate::facade_save_ops::handle_tachi_save(self, params).await
    }

    #[tool(
        description = "Unified handoff: 'leave' a memo, 'check' pending memos, or 'promote_issue' to create/link a GitHub issue from a handoff memo."
    )]
    pub(crate) async fn tachi_handoff(
        &self,
        Parameters(params): Parameters<TachiHandoffParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        match action.as_str() {
            "leave" => {
                let summary = params
                    .summary
                    .clone()
                    .ok_or_else(|| "summary is required when action='leave'".to_string())?;
                let leave_params = HandoffLeaveParams {
                    summary,
                    next_steps: params.next_steps.clone(),
                    target_agent: params.target_agent.clone(),
                    context: params.context.clone(),
                };
                handle_handoff_leave(self, leave_params).await
            }
            "check" => {
                let check_params = HandoffCheckParams {
                    agent_id: params.agent_id.clone(),
                    acknowledge: params.acknowledge,
                };
                handle_handoff_check(self, check_params).await
            }
            "promote_issue" => {
                let memo_id = params
                    .memo_id
                    .clone()
                    .ok_or_else(|| "memo_id is required when action='promote_issue'".to_string())?;
                let repo = params
                    .repo
                    .clone()
                    .ok_or_else(|| "repo is required when action='promote_issue'".to_string())?;
                let promote_params = HandoffPromoteIssueParams {
                    memo_id,
                    repo,
                    title: params.title.clone(),
                    labels: params.labels.clone(),
                    flow_id: params.flow_id.clone(),
                    force: params.force,
                };
                handle_handoff_promote_issue(self, promote_params).await
            }
            _ => Err(format!(
                "Invalid action '{}'. Use 'leave', 'check', or 'promote_issue'.",
                params.action
            )),
        }
    }

    #[tool(
        description = "Issue→Doc→Memory closure: action=close_loop writes wiki with references[] (issue + docs + related issues); build_references previews the array. Replaces nightly wiki compile (#77)."
    )]
    pub(crate) async fn tachi_workflow(
        &self,
        Parameters(params): Parameters<TachiWorkflowParams>,
    ) -> Result<String, String> {
        crate::workflow_closure::handle_workflow(self, params).await
    }

    #[tool(
        description = "Persistent orchestrator state outside LLM context: todo_list, todo_update, handoff_write, handoff_read, recovery_briefing. Stored in hard_state (survives compaction). Use task_id = dispatch_id or issue id."
    )]
    pub(crate) async fn tachi_orchestrator(
        &self,
        Parameters(params): Parameters<TachiOrchestratorParams>,
    ) -> Result<String, String> {
        crate::orchestrator_ops::handle_orchestrator(self, params).await
    }

    #[tool(
        description = "Prepare a task brief before non-trivial work: relevant wiki lessons, memory hits, intent, selected_sops, tool_plan, lightweight skill suggestions, and debugging checklist. (Alias: tachi_task_brief)"
    )]
    pub(crate) async fn tachi_plan(
        &self,
        Parameters(params): Parameters<TaskBriefParams>,
    ) -> Result<String, String> {
        handle_tachi_task_brief(self, params).await
    }

    #[tool(
        description = "Check whether an agent is stuck after repeated attempts. Returns reframe advice, relevant wiki hits, and an ask-codex prompt when useful. Pass flow_id to append progress.jsonl. (Alias: tachi_progress_check)"
    )]
    pub(crate) async fn tachi_unstick(
        &self,
        Parameters(params): Parameters<ProgressCheckParams>,
    ) -> Result<String, String> {
        handle_tachi_progress_check(self, params).await
    }

    #[tool(
        description = "Browse wiki entries by category. Without a category, returns category stats. Supports short aliases like 'quant', 'engineering', 'tachi', etc. (Alias: wiki_browse)"
    )]
    pub(crate) async fn tachi_browse(
        &self,
        Parameters(params): Parameters<WikiBrowseParams>,
    ) -> Result<String, String> {
        handle_wiki_browse(self, params)
    }

    #[tool(
        description = "Agent fleet registry (#155): action=list shows claude/codex/grok/kimi; action=select returns heuristic agent + fallback chain for an intent label."
    )]
    pub(crate) async fn tachi_agents(
        &self,
        Parameters(params): Parameters<TachiAgentsParams>,
    ) -> Result<String, String> {
        crate::agent_registry::handle_agents(self, params).await
    }

    #[tool(
        description = "Dispatch a task to a delegate CLI agent (claude, codex, grok, kimi, or custom). Assembles prompt with context from memory/wiki + injected skills, spawns agent subprocess, returns structured result. Call tachi_complete afterwards to record the eval."
    )]
    pub(crate) async fn tachi_dispatch(
        &self,
        Parameters(params): Parameters<TachiDispatchParams>,
    ) -> Result<String, String> {
        crate::dispatch_ops::handle_tachi_dispatch(self, params).await
    }

    #[tool(
        description = "Agent eval scorecard: aggregate live eval success/verification rates by agent, profile, and task type. Fixture replay is local-only and requires TACHI_AGENT_EVAL_ALLOW_FIXTURE=1."
    )]
    pub(crate) async fn tachi_agent_eval(
        &self,
        Parameters(params): Parameters<TachiAgentEvalParams>,
    ) -> Result<String, String> {
        crate::agent_eval::handle_agent_eval(self, params).await
    }

    #[tool(
        description = "View the task board (kanban) showing all dispatched background tasks and their statuses. Returns a list of tasks with their A2A state (WORKING, COMPLETED, FAILED, etc)."
    )]
    pub(crate) async fn tachi_board(
        &self,
        Parameters(params): Parameters<TachiBoardParams>,
    ) -> Result<String, String> {
        crate::dispatch_ops::handle_tachi_board(self, params).await
    }

    #[tool(
        description = "Merge a git worktree branch back to the main branch and optionally remove the worktree. Use after reviewing tachi_dispatch results."
    )]
    pub(crate) async fn approve_merge(
        &self,
        Parameters(params): Parameters<TachiApproveMergeParams>,
    ) -> Result<String, String> {
        crate::dispatch_ops::handle_approve_merge(params).await
    }

    #[tool(
        description = "Declare task completion and write an entry to the eval ledger. Records agent, outcome, duration, cost, skills used, and (optionally) trajectory/diff for later distillation. Returns a review bundle. Does NOT auto-merge worktrees — use approve_merge for that."
    )]
    pub(crate) async fn tachi_complete(
        &self,
        Parameters(params): Parameters<TachiCompleteParams>,
    ) -> Result<String, String> {
        crate::complete_ops::handle_tachi_complete(self, params).await
    }

    // ─── Facade: wiki (search / browse / write) ─────────────────────────────

    #[tool(
        description = "Stable, reusable knowledge base. action='search': look up lessons, patterns, and how-tos BEFORE debugging from scratch or reaching for web search — a prior lesson may already exist. action='browse': explore available categories. action='read': load a specific entry by path. action='write': persist a reusable lesson, architecture decision, pattern, or how-to. WHEN: wiki for durable knowledge that helps future sessions (patterns, lessons, decisions, conventions). Use tachi_memory for session-specific facts (decisions, findings, commands for the current task). Pass project to target a named library."
    )]
    pub(crate) async fn tachi_wiki(
        &self,
        Parameters(params): Parameters<TachiWikiParams>,
    ) -> Result<String, String> {
        handle_tachi_wiki_facade(self, params).await
    }

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
