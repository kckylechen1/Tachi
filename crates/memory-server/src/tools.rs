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

use crate::capability_ops::{
    handle_prepare_capability_bundle, handle_recommend_capability, handle_recommend_skill,
    handle_recommend_toolchain,
};
use crate::copilot_ops::{
    handle_tachi_progress_check, handle_tachi_task_brief, handle_tachi_wiki_search,
    handle_tachi_wiki_write,
};
use crate::dlq_ops::{handle_dlq_list, handle_dlq_retry};
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
use crate::hub_ops::{
    handle_distill_trajectory, handle_export_skills, handle_hub_call, handle_hub_disconnect,
    handle_hub_discover, handle_hub_feedback, handle_hub_get, handle_hub_quick_add,
    handle_hub_register, handle_hub_review, handle_hub_set_active_version, handle_hub_set_enabled,
    handle_hub_stats, handle_run_skill, handle_skill_evolve, handle_tachi_audit_log,
    handle_vc_bind, handle_vc_list, handle_vc_register, handle_vc_resolve,
};
use crate::kanban::{
    handle_check_inbox, handle_post_card, handle_update_card, CheckInboxParams, PostCardParams,
    UpdateCardParams,
};
use crate::memory_ops::{
    handle_archive_memory, handle_delete_domain, handle_delete_memory, handle_get_domain,
    handle_get_memory, handle_list_domains, handle_list_memories, handle_memory_gc,
    handle_memory_stats, handle_register_domain, handle_runtime_info,
};
use crate::memory_search_ops::{
    handle_find_similar_memory, handle_remember, handle_save_memory, handle_search_memory,
};
use crate::pack_ops::{
    handle_pack_get, handle_pack_list, handle_pack_project, handle_pack_register,
    handle_pack_remove, handle_projection_list,
};
use crate::pipeline_ops::{
    handle_extract_facts, handle_get_pipeline_status, handle_ingest, handle_ingest_event,
    handle_ingest_source, handle_sync_memories,
};
use crate::project_db_ops::handle_tachi_init_project_db;
use crate::sandbox_ops::{
    handle_sandbox_check, handle_sandbox_exec_audit, handle_sandbox_get_policy,
    handle_sandbox_list_policies, handle_sandbox_set_policy, handle_sandbox_set_rule,
};
use crate::skill_chain_ops::handle_chain_skills;
use crate::tool_params::*;
use crate::vault_ops::{
    handle_vault_get, handle_vault_init, handle_vault_list, handle_vault_lock, handle_vault_remove,
    handle_vault_set, handle_vault_setup_rotation, handle_vault_status, handle_vault_unlock,
    VaultGetParams, VaultInitParams, VaultListParams, VaultRemoveParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams,
};
use crate::wiki_ops::{
    handle_wiki_browse, handle_wiki_ingest, handle_wiki_lint, handle_wiki_search,
};
use crate::{AgentProfile, MemoryServer};

// ─── Tool Implementations ────────────────────────────────────────────────────────

#[tool_router(vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Save a memory entry to the store. Creates a new entry or updates an existing one if id is provided."
    )]
    pub(crate) async fn save_memory(
        &self,
        Parameters(params): Parameters<SaveMemoryParams>,
    ) -> Result<String, String> {
        if let Some(body) = crate::cli_client::maybe_forward_write(
            self.global_db_path.as_path(),
            "save_memory",
            &params,
        )
        .await
        {
            return Ok(body);
        }
        handle_save_memory(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for save_memory. Write memory into cyberbrain."
    )]
    pub(crate) async fn cyberbrain_write(
        &self,
        Parameters(params): Parameters<SaveMemoryParams>,
    ) -> Result<String, String> {
        handle_save_memory(self, params).await
    }

    #[tool(
        description = "Low-friction shortcut to save a note. Only `text` is required; path defaults to /notes/{YYYY-MM-DD}, category to \"fact\", importance to 0.6, scope to \"project\". Use save_memory directly when you need full control over path, importance, retention, vector, or auto-link."
    )]
    pub(crate) async fn remember(
        &self,
        Parameters(params): Parameters<RememberParams>,
    ) -> Result<String, String> {
        if let Some(body) = crate::cli_client::maybe_forward_write(
            self.global_db_path.as_path(),
            "remember",
            &params,
        )
        .await
        {
            return Ok(body);
        }
        handle_remember(self, params).await
    }

    #[tool(
        description = "Search memory entries using hybrid search (vector + FTS + symbolic). Returns ranked results with scores."
    )]
    pub(crate) async fn search_memory(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<String, String> {
        handle_search_memory(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for search_memory. Query memories from cyberbrain."
    )]
    pub(crate) async fn cyberbrain_search(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<String, String> {
        handle_search_memory(self, params).await
    }

    #[tool(
        description = "Find memory entries similar to a provided vector. Uses vector similarity only (no FTS/symbolic/decay weighting)."
    )]
    pub(crate) async fn find_similar_memory(
        &self,
        Parameters(params): Parameters<FindSimilarMemoryParams>,
    ) -> Result<String, String> {
        handle_find_similar_memory(self, params).await
    }

    #[tool(description = "Get a single memory entry by ID.")]
    pub(crate) async fn get_memory(
        &self,
        Parameters(params): Parameters<GetMemoryParams>,
    ) -> Result<String, String> {
        handle_get_memory(self, params).await
    }

    #[tool(description = "List memory entries under a path prefix.")]
    pub(crate) async fn list_memories(
        &self,
        Parameters(params): Parameters<ListMemoriesParams>,
    ) -> Result<String, String> {
        handle_list_memories(self, params).await
    }

    #[tool(description = "Get aggregate statistics about the memory store.")]
    pub(crate) async fn memory_stats(&self) -> Result<String, String> {
        handle_memory_stats(self).await
    }

    #[tool(
        description = "Return Tachi runtime identity and DB routing metadata. Clients should verify this before writing through embedded or derivative Tachi deployments."
    )]
    pub(crate) async fn runtime_info(&self) -> Result<String, String> {
        handle_runtime_info(self).await
    }

    #[tool(
        description = "Cheap health check: daemon status, vector coverage, foundry queue depth, provider key drift/auth-failure inference, model lane config, and agent readiness warnings. Call at session start; use `tachi status --probe-keys` or `tachi doctor --probe-keys` for live provider calls."
    )]
    pub(crate) async fn tachi_status(&self) -> Result<String, String> {
        crate::status_ops::handle_tachi_status_agent(self).await
    }

    #[tool(
        description = "Doctor v2 — scan known memory.db roots, classify each (healthy / vec_extension_missing / wal_orphan / corrupt / legacy_schema / placeholder / backup), return JSON report. Read-only; no mutations."
    )]
    pub(crate) async fn tachi_doctor_scan(&self) -> Result<String, String> {
        crate::doctor_ops::handle_tachi_doctor_scan().await
    }

    #[tool(
        description = "Delete a memory entry permanently. Removes from main table, FTS, vectors, graph edges, and access history."
    )]
    pub(crate) async fn delete_memory(
        &self,
        Parameters(params): Parameters<DeleteMemoryParams>,
    ) -> Result<String, String> {
        handle_delete_memory(self, params).await
    }

    #[tool(
        description = "Archive a memory entry (soft-delete, set archived=1). Entry is hidden from default searches but can be retrieved with include_archived=true."
    )]
    pub(crate) async fn archive_memory(
        &self,
        Parameters(params): Parameters<ArchiveMemoryParams>,
    ) -> Result<String, String> {
        handle_archive_memory(self, params).await
    }

    #[tool(
        description = "Run garbage collection on growing tables. Prunes old access_history (keep latest 256 per memory), processed_events (30d), audit_log (30d + 100k cap), and agent_known_state (90d)."
    )]
    pub(crate) async fn memory_gc(&self) -> Result<String, String> {
        handle_memory_gc(self).await
    }

    #[tool(description = "Enable or disable a Hub capability by ID.")]
    pub(crate) async fn hub_set_enabled(
        &self,
        Parameters(params): Parameters<HubSetEnabledParams>,
    ) -> Result<String, String> {
        handle_hub_set_enabled(self, params).await
    }

    #[tool(description = "Set governance review status for a Hub capability.")]
    pub(crate) async fn hub_review(
        &self,
        Parameters(params): Parameters<HubReviewParams>,
    ) -> Result<String, String> {
        handle_hub_review(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for hub_review (Section 9 governance review)."
    )]
    pub(crate) async fn section9_review(
        &self,
        Parameters(params): Parameters<HubReviewParams>,
    ) -> Result<String, String> {
        handle_hub_review(self, params).await
    }

    #[tool(description = "Route an alias capability ID to a concrete active capability version.")]
    pub(crate) async fn hub_set_active_version(
        &self,
        Parameters(params): Parameters<HubSetActiveVersionParams>,
    ) -> Result<String, String> {
        handle_hub_set_active_version(self, params).await
    }

    #[tool(
        description = "Export Hub skills to agent-specific file formats. Targets: claude (SKILL.md + symlinks), openclaw (plugin manifest), cursor (.mdc rules), generic (raw files)."
    )]
    pub(crate) async fn hub_export_skills(
        &self,
        Parameters(params): Parameters<ExportSkillsParams>,
    ) -> Result<String, String> {
        handle_export_skills(self, params).await
    }

    #[tool(
        description = "Register a Virtual Capability (logical capability layer) on top of concrete backends."
    )]
    pub(crate) async fn vc_register(
        &self,
        Parameters(params): Parameters<VirtualCapabilityRegisterParams>,
    ) -> Result<String, String> {
        handle_vc_register(self, params).await
    }

    #[tool(
        description = "Bind a Virtual Capability to a concrete capability with deterministic priority and optional version pin."
    )]
    pub(crate) async fn vc_bind(
        &self,
        Parameters(params): Parameters<VirtualCapabilityBindParams>,
    ) -> Result<String, String> {
        handle_vc_bind(self, params).await
    }

    #[tool(description = "List Virtual Capabilities together with their current bindings.")]
    pub(crate) async fn vc_list(
        &self,
        Parameters(params): Parameters<HubDiscoverParams>,
    ) -> Result<String, String> {
        handle_vc_list(self, params).await
    }

    #[tool(
        description = "Resolve a Virtual Capability to the concrete capability currently selected for routing."
    )]
    pub(crate) async fn vc_resolve(
        &self,
        Parameters(params): Parameters<VirtualCapabilityResolveParams>,
    ) -> Result<String, String> {
        handle_vc_resolve(self, params).await
    }

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
        if let Some(body) = crate::cli_client::maybe_forward_write(
            self.global_db_path.as_path(),
            "tachi_wiki_write",
            &params,
        )
        .await
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
        description = "Prepare a task brief before non-trivial work: relevant wiki lessons, memory hits, lightweight skill suggestions, and debugging checklist."
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
        if let Some(body) = crate::cli_client::maybe_forward_write(
            self.global_db_path.as_path(),
            "extract_facts",
            &params,
        )
        .await
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

    #[tool(description = "Register a capability (skill, plugin, or MCP server) in the Hub.")]
    pub(crate) async fn hub_register(
        &self,
        Parameters(params): Parameters<HubRegisterParams>,
    ) -> Result<String, String> {
        handle_hub_register(self, params).await
    }

    #[tool(
        description = "Composite: hub_register followed by an optional hub_review approve+enable. \
                       Honors the trusted-command allowlist — auto_approve is silently dropped \
                       (with a warning) for untrusted stdio MCP commands."
    )]
    pub(crate) async fn hub_quick_add(
        &self,
        Parameters(params): Parameters<HubQuickAddParams>,
    ) -> Result<String, String> {
        handle_hub_quick_add(self, params).await
    }

    #[tool(
        description = "Discover available capabilities (skills, plugins, MCP servers) in the Hub."
    )]
    pub(crate) async fn hub_discover(
        &self,
        Parameters(params): Parameters<HubDiscoverParams>,
    ) -> Result<String, String> {
        handle_hub_discover(self, params).await
    }

    #[tool(description = "Get a specific capability from the Hub by ID.")]
    pub(crate) async fn hub_get(
        &self,
        Parameters(params): Parameters<HubGetParams>,
    ) -> Result<String, String> {
        handle_hub_get(self, params).await
    }

    #[tool(description = "Record feedback for a Hub capability invocation.")]
    pub(crate) async fn hub_feedback(
        &self,
        Parameters(params): Parameters<HubFeedbackParams>,
    ) -> Result<String, String> {
        handle_hub_feedback(self, params).await
    }

    #[tool(description = "Get Hub capability statistics and metrics.")]
    pub(crate) async fn hub_stats(&self) -> Result<String, String> {
        handle_hub_stats(self).await
    }

    #[tool(
        description = "Execute a registered Skill from the Hub using the internal LLM pipeline."
    )]
    pub(crate) async fn run_skill(
        &self,
        Parameters(params): Parameters<RunSkillParams>,
    ) -> Result<String, String> {
        handle_run_skill(self, params).await
    }

    #[tool(
        description = "Distill a completed task trajectory into a reusable Skill, persist a permanent skill snapshot, and register/update the distilled Hub Skill."
    )]
    pub(crate) async fn distill_trajectory(
        &self,
        Parameters(params): Parameters<DistillTrajectoryParams>,
    ) -> Result<String, String> {
        handle_distill_trajectory(self, params).await
    }

    #[tool(
        description = "Evolve a skill by analyzing its telemetry and using LLM to produce an improved prompt. Creates a new versioned capability."
    )]
    pub(crate) async fn skill_evolve(
        &self,
        Parameters(params): Parameters<SkillEvolveParams>,
    ) -> Result<String, String> {
        handle_skill_evolve(self, params).await
    }

    #[tool(
        description = "Recommend the best Tachi capability for a task query. Uses Hub metadata, visibility, host constraints, and telemetry to rank candidate capabilities."
    )]
    pub(crate) async fn recommend_capability(
        &self,
        Parameters(params): Parameters<RecommendCapabilityParams>,
    ) -> Result<String, String> {
        handle_recommend_capability(self, params).await
    }

    #[tool(
        description = "Recommend the most relevant skills for a task query. Returns ranked skill candidates plus callable tool aliases when available."
    )]
    pub(crate) async fn recommend_skill(
        &self,
        Parameters(params): Parameters<RecommendSkillParams>,
    ) -> Result<String, String> {
        handle_recommend_skill(self, params).await
    }

    #[tool(
        description = "Recommend a host-aware toolchain for a task query, including skills, supporting capabilities, projected packs, and suggested host-native execution tools."
    )]
    pub(crate) async fn recommend_toolchain(
        &self,
        Parameters(params): Parameters<RecommendToolchainParams>,
    ) -> Result<String, String> {
        handle_recommend_toolchain(self, params).await
    }

    #[tool(
        description = "Prepare a host-aware capability bundle for a task query. Returns the primary skill, supporting capabilities, relevant packs, suggested host-native tools, and a ready-to-inject bundle section."
    )]
    pub(crate) async fn prepare_capability_bundle(
        &self,
        Parameters(params): Parameters<PrepareCapabilityBundleParams>,
    ) -> Result<String, String> {
        handle_prepare_capability_bundle(self, params).await
    }

    #[tool(
        description = "Recall structured memory context for an active agent turn. Returns ranked results plus a ready-to-inject prepend_context block."
    )]
    pub(crate) async fn recall_context(
        &self,
        Parameters(params): Parameters<RecallContextParams>,
    ) -> Result<String, String> {
        handle_recall_context(self, params).await
    }

    #[tool(
        description = "Capture durable memories from a recent session window. Extracts structured memories, embeds them inside Tachi, and writes them to the configured store."
    )]
    pub(crate) async fn capture_session(
        &self,
        Parameters(params): Parameters<CaptureSessionParams>,
    ) -> Result<String, String> {
        handle_capture_session(self, params).await
    }

    #[tool(
        description = "Compact a soon-to-be-evicted session window into a ready-to-inject context block. Designed for host runtimes that know when token pressure requires compaction."
    )]
    pub(crate) async fn compact_context(
        &self,
        Parameters(params): Parameters<CompactContextParams>,
    ) -> Result<String, String> {
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

    #[tool(description = "View audit log of proxy tool calls through the Hub.")]
    pub(crate) async fn tachi_audit_log(
        &self,
        Parameters(params): Parameters<AuditLogParams>,
    ) -> Result<String, String> {
        handle_tachi_audit_log(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for tachi_audit_log (Section 9 audit view)."
    )]
    pub(crate) async fn section9_audit_log(
        &self,
        Parameters(params): Parameters<AuditLogParams>,
    ) -> Result<String, String> {
        handle_tachi_audit_log(self, params).await
    }

    #[tool(
        description = "Call a tool on a registered MCP server through the Hub using the shared connection pool."
    )]
    pub(crate) async fn hub_call(
        &self,
        Parameters(params): Parameters<HubCallParams>,
    ) -> Result<String, String> {
        handle_hub_call(self, params).await
    }

    #[tool(
        description = "Disconnect a cached MCP server connection from the pool. Forces a fresh reconnect (with updated env/config) on next hub_call."
    )]
    pub(crate) async fn hub_disconnect(
        &self,
        Parameters(params): Parameters<HubDisconnectParams>,
    ) -> Result<String, String> {
        handle_hub_disconnect(self, params).await
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

    // ─── Semantic Sandboxing Tools ───────────────────────────────────────────

    #[tool(
        description = "Set a sandbox access rule for an agent role + path pattern. Controls which memories a role can access. Access levels: read, write, deny."
    )]
    pub(crate) async fn sandbox_set_rule(
        &self,
        Parameters(params): Parameters<SandboxSetRuleParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_rule(self, params).await
    }

    #[tool(
        description = "Check if an agent role can access a given path for a specific operation. Advisory mode — not enforced in search_memory yet (TODO: future enforcement integration)."
    )]
    pub(crate) async fn sandbox_check(
        &self,
        Parameters(params): Parameters<SandboxCheckParams>,
    ) -> Result<String, String> {
        handle_sandbox_check(self, params).await
    }

    #[tool(
        description = "Set runtime sandbox policy for a capability (timeouts, concurrency, env allowlist, fs/cwd roots)."
    )]
    pub(crate) async fn sandbox_set_policy(
        &self,
        Parameters(params): Parameters<SandboxSetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_policy(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_set_policy. Configure shell execution policy."
    )]
    pub(crate) async fn shell_set_policy(
        &self,
        Parameters(params): Parameters<SandboxSetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_set_policy(self, params).await
    }

    #[tool(description = "Get runtime sandbox policy for a capability.")]
    pub(crate) async fn sandbox_get_policy(
        &self,
        Parameters(params): Parameters<SandboxGetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_get_policy(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_get_policy. Read shell execution policy."
    )]
    pub(crate) async fn shell_get_policy(
        &self,
        Parameters(params): Parameters<SandboxGetPolicyParams>,
    ) -> Result<String, String> {
        handle_sandbox_get_policy(self, params).await
    }

    #[tool(description = "List runtime sandbox policies.")]
    pub(crate) async fn sandbox_list_policies(
        &self,
        Parameters(params): Parameters<SandboxListPoliciesParams>,
    ) -> Result<String, String> {
        handle_sandbox_list_policies(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_list_policies. List shell policies."
    )]
    pub(crate) async fn shell_list_policies(
        &self,
        Parameters(params): Parameters<SandboxListPoliciesParams>,
    ) -> Result<String, String> {
        handle_sandbox_list_policies(self, params).await
    }

    #[tool(
        description = "List sandbox execution audit rows (policy decisions, startup, runtime outcomes)."
    )]
    pub(crate) async fn sandbox_exec_audit(
        &self,
        Parameters(params): Parameters<SandboxExecAuditParams>,
    ) -> Result<String, String> {
        handle_sandbox_exec_audit(self, params).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for sandbox_exec_audit. Inspect shell execution audit."
    )]
    pub(crate) async fn shell_exec_audit(
        &self,
        Parameters(params): Parameters<SandboxExecAuditParams>,
    ) -> Result<String, String> {
        handle_sandbox_exec_audit(self, params).await
    }

    // ─── Pack System Tools ────────────────────────────────────────────────────

    #[tool(description = "List installed skill packs. Optionally filter by enabled_only.")]
    pub(crate) async fn pack_list(
        &self,
        Parameters(params): Parameters<PackListParams>,
    ) -> Result<String, String> {
        handle_pack_list(self, params).await
    }

    #[tool(description = "Get details of a single installed skill pack by ID.")]
    pub(crate) async fn pack_get(
        &self,
        Parameters(params): Parameters<PackGetParams>,
    ) -> Result<String, String> {
        handle_pack_get(self, params).await
    }

    #[tool(
        description = "Register a skill pack after git clone / download. Records the pack in the registry with its metadata, source, and skill count."
    )]
    pub(crate) async fn pack_register(
        &self,
        Parameters(params): Parameters<PackRegisterParams>,
    ) -> Result<String, String> {
        handle_pack_register(self, params).await
    }

    #[tool(
        description = "Remove a skill pack from the registry. Also cleans up projected files in agent directories unless clean_files=false."
    )]
    pub(crate) async fn pack_remove(
        &self,
        Parameters(params): Parameters<PackRemoveParams>,
    ) -> Result<String, String> {
        handle_pack_remove(self, params).await
    }

    #[tool(
        description = "Project a pack's skills, workflows, and host overlays to one or more agents. Converts SKILL.md files to each agent's native format (e.g. .mdc rules for Cursor) and emits a tachi-projection manifest for adapters such as OpenClaw."
    )]
    pub(crate) async fn pack_project(
        &self,
        Parameters(params): Parameters<PackProjectParams>,
    ) -> Result<String, String> {
        handle_pack_project(self, params).await
    }

    #[tool(description = "List agent projections. Filter by agent and/or pack_id.")]
    pub(crate) async fn projection_list(
        &self,
        Parameters(params): Parameters<ProjectionListParams>,
    ) -> Result<String, String> {
        handle_projection_list(self, params).await
    }

    // ─── Domain Management ─────────────────────────────────────────────────

    #[tool(
        description = "Register a domain configuration for memory routing, GC thresholds, and default retention policies."
    )]
    pub(crate) async fn register_domain(
        &self,
        Parameters(params): Parameters<RegisterDomainParams>,
    ) -> Result<String, String> {
        handle_register_domain(self, params).await
    }

    #[tool(description = "Get a domain configuration by name.")]
    pub(crate) async fn get_domain(
        &self,
        Parameters(params): Parameters<GetDomainParams>,
    ) -> Result<String, String> {
        handle_get_domain(self, params).await
    }

    #[tool(description = "List all registered domain configurations.")]
    pub(crate) async fn list_domains(
        &self,
        Parameters(_params): Parameters<ListDomainsParams>,
    ) -> Result<String, String> {
        handle_list_domains(self).await
    }

    #[tool(description = "Delete a domain configuration by name.")]
    pub(crate) async fn delete_domain(
        &self,
        Parameters(params): Parameters<DeleteDomainParams>,
    ) -> Result<String, String> {
        handle_delete_domain(self, params).await
    }

    // ─── Vault (Encrypted Secret Storage) ────────────────────────────────────

    #[tool(description = "Initialize the vault with a master password. Can only be called once.")]
    pub(crate) async fn vault_init(
        &self,
        Parameters(params): Parameters<VaultInitParams>,
    ) -> Result<String, String> {
        handle_vault_init(self, params).await
    }

    #[tool(description = "Unlock the vault by verifying the master password.")]
    pub(crate) async fn vault_unlock(
        &self,
        Parameters(params): Parameters<VaultUnlockParams>,
    ) -> Result<String, String> {
        handle_vault_unlock(self, params).await
    }

    #[tool(description = "Lock the vault (clear encryption key from memory).")]
    pub(crate) async fn vault_lock(&self) -> Result<String, String> {
        handle_vault_lock(self).await
    }

    #[tool(
        description = "Store or update an encrypted secret in the vault. Supports multi-key rotation when name ends with _N."
    )]
    pub(crate) async fn vault_set(
        &self,
        Parameters(params): Parameters<VaultSetParams>,
    ) -> Result<String, String> {
        handle_vault_set(self, params).await
    }

    #[tool(
        description = "Retrieve and decrypt a secret from the vault. Supports auto-rotation for multi-key secrets."
    )]
    pub(crate) async fn vault_get(
        &self,
        Parameters(params): Parameters<VaultGetParams>,
    ) -> Result<String, String> {
        handle_vault_get(self, params).await
    }

    #[tool(
        description = "List all stored secrets (names and metadata only, not values). Does not require vault to be unlocked."
    )]
    pub(crate) async fn vault_list(
        &self,
        Parameters(params): Parameters<VaultListParams>,
    ) -> Result<String, String> {
        handle_vault_list(self, params).await
    }

    #[tool(description = "Delete a secret from the vault.")]
    pub(crate) async fn vault_remove(
        &self,
        Parameters(params): Parameters<VaultRemoveParams>,
    ) -> Result<String, String> {
        handle_vault_remove(self, params).await
    }

    #[tool(description = "Check vault status (initialized, locked/unlocked, entry count).")]
    pub(crate) async fn vault_status(&self) -> Result<String, String> {
        handle_vault_status(self).await
    }

    #[tool(
        description = "Setup key rotation for a prefix. Requires keys like PREFIX_1, PREFIX_2, etc. to already exist."
    )]
    pub(crate) async fn vault_setup_rotation(
        &self,
        Parameters(params): Parameters<VaultSetupRotationParams>,
    ) -> Result<String, String> {
        handle_vault_setup_rotation(self, params).await
    }

    // ─── Facade tools (consolidated surface for Antigravity minimal profile) ──────

    #[tool(
        description = "Unified memory facade for recall and session UX. Returns readable Markdown (not raw JSON). Actions: search, save, briefing, checkpoint, alerts, ask, etc. Pass `project` to target ~/.tachi/projects/<name>/memory.db explicitly; omit to use global + daemon-bound workspace DB (shown in response). Diagnostics belong in tachi_status / tachi_doctor — not here."
    )]
    pub(crate) async fn tachi_memory(
        &self,
        Parameters(params): Parameters<TachiMemoryParams>,
    ) -> Result<String, String> {
        crate::facade_memory_ops::handle_tachi_memory(self, params).await
    }

    #[tool(
        description = "Unified search across wiki and memory. Use scope to target 'wiki', 'memory', or 'all' (default)."
    )]
    pub(crate) async fn tachi_search(
        &self,
        Parameters(params): Parameters<TachiSearchParams>,
    ) -> Result<String, String> {
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
        description = "Unified save: writes a wiki entry, memory, or quick note. Set kind to 'wiki', 'note', or 'memory' — or omit for auto-detection."
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
        description = "Prepare a task brief before non-trivial work: relevant wiki lessons, memory hits, lightweight skill suggestions, and debugging checklist. (Alias: tachi_task_brief)"
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
        description = "Dispatch a task to a delegate agent (Claude Code CLI, Codex CLI, or custom). Assembles prompt with context from memory/wiki + injected skills, spawns agent subprocess, returns structured result. Call tachi_complete afterwards to record the eval."
    )]
    pub(crate) async fn tachi_dispatch(
        &self,
        Parameters(params): Parameters<TachiDispatchParams>,
    ) -> Result<String, String> {
        crate::dispatch_ops::handle_tachi_dispatch(self, params).await
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
        description = "Unified wiki facade: search, browse, or write wiki entries. Returns readable Markdown. Pass `project` to target a named library explicitly. Diagnostics belong in tachi_status / tachi_doctor."
    )]
    pub(crate) async fn tachi_wiki(
        &self,
        Parameters(params): Parameters<TachiWikiParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        match action.as_str() {
            "search" => {
                let query = params
                    .query
                    .clone()
                    .ok_or_else(|| "query is required when action='search'".to_string())?;
                let wiki_params = WikiSearchParams {
                    query,
                    path_prefix: None,
                    category: params.category.clone(),
                    top_k: params.top_k.unwrap_or(10),
                    include_archived: false,
                    agent_role: None,
                    project: params.project.clone(),
                    domain: params.domain.clone(),
                    file_context: None,
                    error_context: None,
                    weights: None,
                };
                handle_tachi_wiki_search(self, wiki_params).await
            }
            "browse" => {
                let browse_params = WikiBrowseParams {
                    category: params.category.clone(),
                    limit: params.limit.unwrap_or(50),
                    project: params.project.clone().unwrap_or_else(|| "wiki".to_string()),
                };
                handle_wiki_browse(self, browse_params)
            }
            "write" => {
                if let Some(body) = crate::cli_client::maybe_forward_write(
                    self.global_db_path.as_path(),
                    "tachi_wiki",
                    &params,
                )
                .await
                {
                    return Ok(body);
                }

                let title = params
                    .title
                    .clone()
                    .ok_or_else(|| "title is required when action='write'".to_string())?;
                let text = params
                    .text
                    .clone()
                    .ok_or_else(|| "text is required when action='write'".to_string())?;
                let wiki_params = WikiWriteParams {
                    title,
                    text,
                    path: params.path.clone(),
                    topic: params.topic.clone(),
                    summary: params.summary.clone(),
                    category: params
                        .category
                        .clone()
                        .unwrap_or_else(|| "experience".to_string()),
                    keywords: params.keywords.clone(),
                    entities: params.entities.clone(),
                    importance: params.importance.unwrap_or(0.85),
                    scope: params.scope.clone().unwrap_or_else(|| "global".to_string()),
                    retention_policy: "permanent".to_string(),
                    domain: params.domain.clone(),
                    project: params.project.clone(),
                    force: params.force,
                };
                handle_tachi_wiki_write(self, wiki_params).await
            }
            _ => Err(format!(
                "Invalid action '{}'. Use 'search', 'browse', or 'write'.",
                params.action
            )),
        }
    }

    // ─── Facade: skill (discover / run) ──────────────────────────────────────

    #[tool(
        description = "Unified skill facade: discover available skills or run a skill. Use action='discover' or 'run'."
    )]
    pub(crate) async fn tachi_skill(
        &self,
        Parameters(params): Parameters<TachiSkillParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        match action.as_str() {
            "discover" => {
                let discover_params = HubDiscoverParams {
                    query: params.query.clone(),
                    cap_type: params
                        .cap_type
                        .clone()
                        .or_else(|| Some("skill".to_string())),
                    enabled_only: params.enabled_only.unwrap_or(true),
                };
                let raw = handle_hub_discover(self, discover_params).await?;
                let mut capabilities: Vec<Value> =
                    serde_json::from_str(&raw).map_err(|e| format!("parse hub discover: {e}"))?;
                let limit = params.limit.unwrap_or(10).max(1);
                capabilities.truncate(limit);
                let results = capabilities
                    .into_iter()
                    .map(|cap| {
                        json!({
                            "id": cap.get("id").cloned().unwrap_or(Value::Null),
                            "name": cap.get("name").cloned().unwrap_or(Value::Null),
                            "description": cap.get("description").cloned().unwrap_or(Value::Null),
                            "cap_type": cap.get("cap_type").cloned().unwrap_or_else(|| cap.get("type").cloned().unwrap_or(Value::Null)),
                            "enabled": cap.get("enabled").cloned().unwrap_or(Value::Null),
                            "review_status": cap.get("review_status").cloned().unwrap_or(Value::Null),
                            "health_status": cap.get("health_status").cloned().unwrap_or(Value::Null),
                            "visibility": cap.get("visibility").cloned().unwrap_or(Value::Null),
                            "callable": cap.get("callable").cloned().unwrap_or(Value::Null),
                            "db": cap.get("db").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::to_string(&json!({
                    "query": params.query,
                    "count": results.len(),
                    "results": results,
                }))
                .map_err(|e| format!("serialize skill discover: {e}"))
            }
            "run" => {
                let skill_id = params
                    .skill_id
                    .clone()
                    .ok_or_else(|| "skill_id is required when action='run'".to_string())?;
                let run_params = RunSkillParams {
                    skill_id,
                    args: params.args.clone().unwrap_or(serde_json::Value::Null),
                };
                handle_run_skill(self, run_params).await
            }
            _ => Err(format!(
                "Invalid action '{}'. Use 'discover' or 'run'.",
                params.action
            )),
        }
    }

    // ─── Facade: task (plan / dispatch / board / merge) ─────────────────────

    #[tool(
        description = "Unified task facade: plan a task, dispatch to an agent, view the board, or merge a worktree. Use action='plan', 'dispatch', 'board', or 'merge'."
    )]
    pub(crate) async fn tachi_task(
        &self,
        Parameters(params): Parameters<TachiTaskParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        match action.as_str() {
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
                    top_k: params.top_k.unwrap_or(6),
                };
                handle_tachi_task_brief(self, brief_params).await
            }
            "dispatch" => {
                let agent = params
                    .agent
                    .clone()
                    .ok_or_else(|| "agent is required when action='dispatch'".to_string())?;
                let task = params
                    .task
                    .clone()
                    .ok_or_else(|| "task is required when action='dispatch'".to_string())?;
                let dispatch_params = TachiDispatchParams {
                    agent,
                    task,
                    cwd: params.cwd.clone(),
                    skills: params.skills.clone(),
                    context_query: params.context_query.clone(),
                    model: params.model.clone(),
                    timeout_secs: params.timeout_secs.unwrap_or(600),
                    permission_profile: params.permission_profile.clone(),
                    allowed_tools: params.allowed_tools.clone(),
                    max_turns: params.max_turns,
                    sandbox: params.sandbox.clone(),
                    inject_tachi_mcp: params.inject_tachi_mcp,
                    inject_hub_mcps: params.inject_hub_mcps,
                    command: params.command.clone(),
                    project: params.project.clone(),
                    stage: params.stage.clone(),
                };
                crate::dispatch_ops::handle_tachi_dispatch(self, dispatch_params).await
            }
            "board" => {
                let board_params = TachiBoardParams {
                    state_filter: params.state_filter.clone(),
                    limit: params.limit,
                    project: params.project.clone(),
                };
                crate::dispatch_ops::handle_tachi_board(self, board_params).await
            }
            "merge" => {
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
                crate::dispatch_ops::handle_approve_merge(merge_params).await
            }
            _ => Err(format!(
                "Invalid action '{}'. Use 'plan', 'dispatch', 'board', or 'merge'.",
                params.action
            )),
        }
    }

    // ─── GitHub MCP Proxy Tools ─────────────────────────────────────────────

    #[tool(
        description = "GitHub operations: repo_view, issue_list, issue_read, issue_create, pr_list, pr_read, safe_merge. safe_merge defaults to dry-run unless confirm=true. Requires GH_TOKEN in Vault or environment."
    )]
    pub(crate) async fn tachi_gh(
        &self,
        Parameters(params): Parameters<TachiGhParams>,
    ) -> Result<String, String> {
        handle_tachi_gh(self, params).await
    }

    // ─── Tachi Shell: skill-gated flow orchestration facade ─────────────────

    #[tool(
        description = "Tachi Shell: skill-gated flow orchestration. Actions: 'brainstorm' | 'plan' | 'dispatch' | 'kanban' | 'status' | 'review' | 'ship'. Each stage-bearing action injects the required Superpowers meta skill SOP and writes an instruction.md packet under .tachi/runs/<flow_id>/."
    )]
    pub(crate) async fn tachi_shell(
        &self,
        Parameters(params): Parameters<TachiShellParams>,
    ) -> Result<String, String> {
        crate::shell_ops::handle_tachi_shell(self, params).await
    }
}
