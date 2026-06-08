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

use crate::arena_ops::handle_tachi_arena;
use crate::capability_ops::{
    handle_prepare_capability_bundle, handle_recommend_capability, handle_recommend_skill,
    handle_recommend_toolchain,
};
use crate::copilot_ops::{
    handle_tachi_feature_briefing, handle_tachi_progress_check, handle_tachi_task_brief,
    handle_tachi_wiki_search, handle_tachi_wiki_write,
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
use crate::verify_ops::handle_tachi_verify;
use crate::wiki_ops::{
    handle_wiki_browse, handle_wiki_ingest, handle_wiki_lint, handle_wiki_read, handle_wiki_search,
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
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "save_memory", &params).await
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
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "save_memory", &params).await
        {
            return Ok(body);
        }
        handle_save_memory(self, params).await
    }

    #[tool(
        description = "Low-friction shortcut to save a note. Only `text` is required; path defaults to /notes/{YYYY-MM-DD}, category to \"fact\", importance to 0.6, scope to \"project\". Use save_memory directly when you need full control over path, importance, retention, vector, or auto-link."
    )]
    pub(crate) async fn remember(
        &self,
        Parameters(params): Parameters<RememberParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "remember", &params).await
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
        handle_search_memory(self, params, false).await
    }

    #[tool(
        description = "Ghost-in-the-Shell style alias for search_memory. Query memories from cyberbrain."
    )]
    pub(crate) async fn cyberbrain_search(
        &self,
        Parameters(params): Parameters<SearchMemoryParams>,
    ) -> Result<String, String> {
        handle_search_memory(self, params, false).await
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
        description = "List the native Tachi tools visible to the current profile. Use before calling unfamiliar tools instead of guessing tool names."
    )]
    pub(crate) async fn tachi_tools(&self) -> Result<String, String> {
        let env_patterns = std::env::var("TACHI_EXPOSED_TOOLS")
            .ok()
            .map(|raw| crate::profiles::parse_tool_patterns_csv(&raw))
            .filter(|patterns| !patterns.is_empty());
        let profile = self.active_tool_profile();
        let mut tools = crate::profiles::filter_tool_defs(
            self.tool_router.list_all(),
            profile,
            env_patterns.as_deref(),
        );
        tools.sort_by(|a, b| a.name.as_ref().cmp(b.name.as_ref()));
        let names = tools
            .iter()
            .map(|tool| tool.name.as_ref().to_string())
            .collect::<Vec<_>>();
        let rows = tools
            .iter()
            .map(|tool| {
                let description = tool
                    .description
                    .as_ref()
                    .map(|text| crate::utils::compact_text_line(text.as_ref(), 96))
                    .unwrap_or_default();
                format!("- `{}` — {}", tool.name, description)
            })
            .collect::<Vec<_>>();
        Ok(format!(
            "## Tachi tools\nprofile: `{}`\ncount: {}\n\n{}\n\nUse exact names from this list; unknown tool names are treated as not connected/unsupported by some MCP hosts.",
            profile
                .map(|p| p.as_str())
                .unwrap_or_else(|| crate::profiles::default_tool_profile().as_str()),
            names.len(),
            rows.join("\n")
        ))
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
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "tachi_wiki_write", &params).await
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
            crate::cli_client::maybe_forward_server_write(self, "extract_facts", &params).await
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
        description = "Prepare a host-aware capability bundle for a task query. Returns the primary skill, supporting capabilities, relevant packs, suggested host-native tools, and a ready-to-inject bundle section. Standard agents may also use tachi_skill(action='bundle')."
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
        description = "Unified memory facade. Actions: search (hybrid recall), get (fetch one memory by id), save (persist entry; prefer tachi_save for decisions), extract_facts (LLM atomize logs), briefing (session start), checkpoint (handoff), alerts (warnings when stuck), ask (Q&A over evidence), consolidate (merge duplicates), progress (long-running flow), readiness (health/tools). Use tachi_briefing for zero-arg briefing alias."
    )]
    pub(crate) async fn tachi_memory(
        &self,
        Parameters(params): Parameters<TachiMemoryParams>,
    ) -> Result<String, String> {
        crate::facade_memory_ops::handle_tachi_memory(self, params).await
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
            format: None,
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
            files: Vec::new(),
            compact: true,
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
        description = "Agent eval harness: load JSONL fixture rows and aggregate success/verification rates by agent+task_type (#158)."
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
            "read" => {
                let path = params
                    .path
                    .clone()
                    .ok_or_else(|| "path is required when action='read'".to_string())?;
                let project = params.project.clone().unwrap_or_else(|| "wiki".to_string());
                handle_wiki_read(self, &path, &project)
            }
            "write" => {
                if let Some(body) =
                    crate::cli_client::maybe_forward_server_write(self, "tachi_wiki", &params).await
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
                    references: params.references.clone(),
                };
                handle_tachi_wiki_write(self, wiki_params).await
            }
            _ => Err(format!(
                "Invalid action '{}'. Use 'search', 'browse', 'read', or 'write'.",
                params.action
            )),
        }
    }

    // ─── Facade: skill (discover / run / bundle / loadout) ──────────────────

    #[tool(
        description = "Skill library for pre-built agent workflows. action='discover': search for a skill BEFORE solving a complex problem; action='bundle': prepare a host-aware capability bundle for a task query; action='loadout': resolve a DispatchProfile's sparse skill loadout plus capability bundle; action='run': execute a named skill by ID. Always discover/bundle before writing custom multi-step logic."
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
                if params.enabled_only.unwrap_or(true) {
                    capabilities.retain(skill_discover_result_is_callable);
                }
                let limit = params.limit.unwrap_or(10).max(1);
                capabilities.truncate(limit);
                let mut results = capabilities
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
                            "source": "local_approved_cache",
                        })
                    })
                    .collect::<Vec<_>>();
                if results.len() < limit {
                    let mut local = discover_local_host_skills(
                        params.query.as_deref().unwrap_or_default(),
                        limit - results.len(),
                    );
                    let seen = results
                        .iter()
                        .filter_map(|cap| cap.get("id").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect::<std::collections::HashSet<_>>();
                    let hub_skill_names = results
                        .iter()
                        .filter_map(canonical_skill_name)
                        .collect::<std::collections::HashSet<_>>();
                    local.retain(|cap| {
                        let id_unseen = cap
                            .get("id")
                            .and_then(Value::as_str)
                            .is_none_or(|id| !seen.contains(id));
                        let name_unseen = canonical_skill_name(cap)
                            .is_none_or(|name| !hub_skill_names.contains(&name));
                        id_unseen && name_unseen
                    });
                    results.extend(local);
                }
                serde_json::to_string(&json!({
                    "query": params.query,
                    "search_backend": if params.query.is_some() { "hub_search+local_skill_index" } else { "hub_list+local_skill_index" },
                    "online_search": false,
                    "source": "local_approved_cache+host_skill_dirs",
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
            "bundle" => {
                let query = required_skill_query(&params, "bundle")?;
                let bundle_params = skill_bundle_params(&params, query, params.host.clone());
                handle_prepare_capability_bundle(self, bundle_params).await
            }
            "loadout" => {
                let profile_name = params
                    .profile
                    .as_deref()
                    .map(str::trim)
                    .filter(|profile| !profile.is_empty())
                    .ok_or_else(|| "profile is required when action='loadout'".to_string())?;
                let profile = crate::dispatch_profile::resolve_dispatch_profile(profile_name)
                    .ok_or_else(|| {
                        format!(
                            "Unknown dispatch profile '{}'. Supported: {}",
                            profile_name,
                            crate::dispatch_profile::DISPATCH_PROFILES
                                .iter()
                                .map(|profile| profile.name)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?;
                let query = params.query.clone().unwrap_or_else(|| {
                    format!(
                        "{} {} {}",
                        profile.role,
                        profile.common_skills.join(" "),
                        profile.signature_skills.join(" ")
                    )
                });
                let host = params
                    .host
                    .clone()
                    .or_else(|| Some(profile.backend.to_string()));
                let bundle_raw = handle_prepare_capability_bundle(
                    self,
                    skill_bundle_params(&params, query.clone(), host.clone()),
                )
                .await?;
                let bundle_value: Value = serde_json::from_str(&bundle_raw)
                    .map_err(|e| format!("parse capability bundle: {e}"))?;
                serde_json::to_string(&json!({
                    "action": "loadout",
                    "profile": profile.name,
                    "display_name": profile.display_name,
                    "role": profile.role,
                    "stage": profile.stage,
                    "backend": profile.backend,
                    "host": host,
                    "resolved_skills": crate::dispatch_profile::profile_required_skill_ids(profile),
                    "skill_loadout": crate::dispatch_profile::profile_skill_loadout_json(profile),
                    "evidence_required": profile.evidence_required,
                    "strong_against": profile.strong_against,
                    "weak_against": profile.weak_against,
                    "auto_capability_bundle": profile.auto_capability_bundle,
                    "capability_bundle": bundle_value.get("bundle").cloned().unwrap_or(Value::Null),
                    "mbit_card": crate::dispatch_profile::profile_json(profile).get("mbit_card").cloned().unwrap_or(Value::Null),
                }))
                .map_err(|e| format!("serialize skill loadout: {e}"))
            }
            _ => Err(format!(
                "Invalid action '{}'. Use 'discover', 'bundle', 'loadout', or 'run'.",
                params.action
            )),
        }
    }

    // ─── Facade: task (plan / recommend / dispatch / board / merge / lifecycle)

    #[tool(
        description = "Task management facade for agent work. action='briefing': feature-scoped handoff board with docs/specs, run artifacts, board state, wiki, memory fragments, eval evidence, and next action; action='plan': search memory/wiki and produce a todo list before complex work; action='recommend': choose a dispatch profile/agent/tool surface from the task, risk, and live eval evidence before assigning external workers; action='route_simulate': replay recent /eval rows across current, cost_sensitive, and quality_first policies without mutating routing; action='proposals': generate/list route-policy proposals from replay evidence; action='review_proposal': approve/reject a route-policy proposal; action='apply_proposals': persist an approved route-policy rule, requiring confirm=true; action='profiles'/'profile'/'card': inspect built-in dispatch profiles; action='dispatch': spawn a delegate agent from either agent or profile; action='complete': record evaluated completion evidence and link flow_id+dispatch_id back to the dispatch card; action='board': view task status; action='intake': bind/read a GitHub issue and create/refresh a flow; action='link_pr': attach a GitHub PR to a flow; action='pr_status': preview GitHub PR safe-merge status without merging, optionally persisting flow status; action='release_note': synthesize release notes; action='ux_matrix': write/read a feature workflow UX checklist; action='build_references': preview issue/doc/related refs; action='close_loop': write durable issue/doc/wiki closure; action='merge': local dispatched worktree git merge only. To execute GitHub PR merges use tachi_gh(action='safe_merge'). Typical worker flow: intake → briefing → ux_matrix → plan/recommend/route_simulate/proposals → dispatch → board → complete/eval → link_pr → pr_status → release_note → close_loop → merge."
    )]
    pub(crate) async fn tachi_task(
        &self,
        Parameters(params): Parameters<TachiTaskParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
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
                    top_k: params.top_k.unwrap_or(6),
                };
                return handle_tachi_task_brief(self, brief_params).await;
            }
            "briefing" => return handle_tachi_feature_briefing(self, &params).await,
            "dispatch" => {
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
                    max_turns: params.max_turns,
                    sandbox: params.sandbox.clone(),
                    inject_tachi_mcp: params.inject_tachi_mcp,
                    inject_hub_mcps: params.inject_hub_mcps,
                    command: params.command.clone(),
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
                crate::dispatch_ops::handle_tachi_dispatch(self, dispatch_params).await
            }
            "complete" => {
                let task = params
                    .task
                    .clone()
                    .ok_or_else(|| "task is required when action='complete'".to_string())?;
                let agent = params
                    .agent
                    .clone()
                    .ok_or_else(|| "agent is required when action='complete'".to_string())?;
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
                    profile: params.profile.clone(),
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
                    dispatch_id: params.dispatch_id.clone(),
                    flow_id: params.flow_id.clone(),
                    issue_ref: params.issue_ref.clone(),
                    pr_ref: params.pr_ref.clone(),
                    evidence_refs: params.evidence_refs.clone(),
                    tests_run: params.tests_run.clone(),
                    diff_present: params.diff_present,
                    scope: params.scope.clone(),
                    project: params.project.clone(),
                };
                crate::complete_ops::handle_tachi_complete(self, complete_params).await
            }
            "board" => {
                let board_params = TachiBoardParams {
                    state_filter: params.state_filter.clone(),
                    limit: params.limit,
                    project: params.project.clone(),
                };
                crate::dispatch_ops::handle_tachi_board(self, board_params).await
            }
            "profiles" | "profile" | "card" => serde_json::to_string(
                &crate::dispatch_profile::dispatch_profiles_json(),
            )
            .map_err(|e| format!("serialize dispatch profiles: {e}")),
            "intake" => crate::task_lifecycle::handle_task_intake(self, &params).await,
            "link_pr" => crate::task_lifecycle::handle_task_link_pr(self, &params).await,
            "recommend" => {
                let task = params
                    .task
                    .clone()
                    .ok_or_else(|| "task is required when action='recommend'".to_string())?;
                let mut file_paths = params.doc_paths.clone();
                file_paths.extend(params.spec_paths.clone());
                crate::dispatch_profile::handle_dispatch_recommendation(
                    self,
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
                    self,
                    params.limit.unwrap_or(500),
                    params.task.as_deref(),
                    params.risk.as_deref(),
                    &file_paths,
                )
            }
            "proposals" => crate::dispatch_profile::handle_route_policy_proposals(
                self,
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
                    self,
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
                    self,
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
                crate::dispatch_ops::handle_approve_merge(merge_params).await
            }
            "pr_status" => {
                let gh_params = build_task_pr_status_gh_params(&params)?;
                crate::gh_ops::handle_tachi_gh(self, gh_params).await
            }
            "release_note" => crate::task_lifecycle::handle_task_release_note(self, &params).await,
            "ux_matrix" => crate::task_lifecycle::handle_task_ux_matrix(&params),
            "build_references" | "close_loop" => {
                let workflow_params = TachiWorkflowParams {
                    action: action.clone(),
                    issue_ref: params.issue_ref.clone(),
                    doc_paths: params.doc_paths.clone(),
                    related_issues: params.related_issues.clone(),
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
                let result = crate::workflow_closure::handle_workflow(self, workflow_params).await?;
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
                "Invalid action '{}'. Use 'briefing', 'plan', 'dispatch', 'complete', 'board', 'profiles', 'profile', 'card', 'recommend', 'route_simulate', 'proposals', 'review_proposal', 'apply_proposals', 'intake', 'link_pr', 'pr_status', 'release_note', 'ux_matrix', 'build_references', 'close_loop', or 'merge'.",
                params.action
            )),
        }?;
        Ok(format_facade_response(
            &format!("Tachi task {}", action),
            &action,
            &raw,
            params.format.as_deref(),
        ))
    }

    // ─── GitHub MCP Proxy Tools ─────────────────────────────────────────────

    #[tool(
        description = "GitHub operations: repo_view, issue_list, issue_read, issue_create, pr_list, pr_read, pr_comments, pr_review_digest, safe_merge. pr_comments returns review submissions plus inline review comments. pr_review_digest filters bot/reviewer comments (author_filter defaults to gemini), writes .tachi/reviews digest artifacts by default, and returns memory/handbook candidates that require leader verdict before promotion. safe_merge is for GitHub PR merges, returns requested_mode=preview unless confirm=true and dry_run!=true, and reports merge_attempted/merge_executed separately. When flow_id is supplied, safe_merge consumes .tachi/runs/<flow_id>/verification.json from tachi_verify; standard/strict wait on missing required verification and block on failed/stale verification. Use approve_merge/tachi_task for local dispatched worktree merges. Requires GH_TOKEN in Vault or environment."
    )]
    pub(crate) async fn tachi_gh(
        &self,
        Parameters(params): Parameters<TachiGhParams>,
    ) -> Result<String, String> {
        handle_tachi_gh(self, params).await
    }

    // ─── Tachi Arena: tracked worker mission document ledger ────────────────

    #[tool(
        description = "Tracked worker mission ledger. action='open' creates .tachi/arena/<arena_id>/; action='spawn' writes mission prompt.md/status.json and returns a tracked prompt for a harness; action='board' lists arenas or missions; action='collect' reads worker result.md; action='abort' marks a mission stopped; action='reap' marks stale ready/running missions; action='close' closes and summarizes the arena. Arena owns run documents; memory owns distilled knowledge."
    )]
    pub(crate) async fn tachi_arena(
        &self,
        Parameters(params): Parameters<TachiArenaParams>,
    ) -> Result<String, String> {
        let action = params.action.to_ascii_lowercase();
        let format = params.format.clone();
        let raw = handle_tachi_arena(self, params).await?;
        Ok(format_facade_response(
            &format!("Tachi arena {}", action),
            &action,
            &raw,
            format.as_deref(),
        ))
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
        Ok(format_facade_response(
            &format!("Tachi shell {}", action),
            &action,
            &raw,
            format.as_deref(),
        ))
    }
}

fn wants_json_format(format: Option<&str>) -> bool {
    format
        .map(|format| format.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

pub(crate) fn resolve_task_pr_status_target(
    params: &TachiTaskParams,
) -> Result<(String, u64), String> {
    crate::task_lifecycle::resolve_task_pr_target(params)
        .map(|target| (target.repo, target.number))
        .map_err(|_| {
            "pr_status requires either repo+number or pr_ref='owner/repo#123' / GitHub PR URL"
                .to_string()
        })
}

pub(crate) fn build_task_pr_status_gh_params(
    params: &TachiTaskParams,
) -> Result<TachiGhParams, String> {
    let (repo, number) = resolve_task_pr_status_target(params)?;
    Ok(TachiGhParams {
        action: "safe_merge".to_string(),
        repo,
        number: Some(number),
        title: None,
        body: None,
        labels: Vec::new(),
        state: None,
        limit: None,
        merge_strategy: None,
        dry_run: Some(true),
        confirm: false,
        flow_id: params.flow_id.clone(),
        merge_policy: params.merge_policy.clone(),
        author_filter: None,
        write_digest: None,
    })
}

fn format_facade_response(title: &str, action: &str, raw: &str, format: Option<&str>) -> String {
    if wants_json_format(format) {
        return raw.to_string();
    }
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };
    let mut lines = vec![format!("## {title}")];
    lines.push(format!("action: `{action}`"));
    append_known_field(&mut lines, &value, "arena_id");
    append_known_field(&mut lines, &value, "mission_id");
    append_known_field(&mut lines, &value, "flow_id");
    append_known_field(&mut lines, &value, "dispatch_id");
    append_known_field(&mut lines, &value, "stage");
    append_known_field(&mut lines, &value, "state");
    append_known_field(&mut lines, &value, "run_dir");
    append_known_field(&mut lines, &value, "arena_dir");
    append_known_field(&mut lines, &value, "mission_dir");
    append_known_field(&mut lines, &value, "instruction_path");
    append_known_field(&mut lines, &value, "prompt_file");
    append_known_field(&mut lines, &value, "trajectory_file");
    append_known_field(&mut lines, &value, "context_file");
    append_known_field(&mut lines, &value, "message");
    append_known_field(&mut lines, &value, "dispatch_error");

    if let Some(tasks) = value.get("tasks").and_then(Value::as_array) {
        lines.push(format!("tasks: {}", tasks.len()));
        for task in tasks.iter().take(10) {
            let id = task
                .get("dispatch_id")
                .and_then(Value::as_str)
                .or_else(|| task.get("id").and_then(Value::as_str))
                .unwrap_or("(task)");
            let state = task
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let agent = task
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let summary = task
                .get("task")
                .and_then(Value::as_str)
                .or_else(|| task.get("title").and_then(Value::as_str))
                .or_else(|| task.get("summary").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .map(|text| format!(" - {text}"))
                .unwrap_or_default();
            lines.push(format!("- `{id}` {state} agent={agent}{summary}"));
        }
    }

    if let Some(flows) = value.get("flows").and_then(Value::as_array) {
        lines.push(format!("flows: {}", flows.len()));
        for flow in flows.iter().take(10) {
            let id = flow
                .get("flow_id")
                .and_then(Value::as_str)
                .unwrap_or("(flow)");
            let stage = flow.get("stage").and_then(Value::as_str).unwrap_or("?");
            let state = flow.get("state").and_then(Value::as_str).unwrap_or("?");
            let summary = flow
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| flow.get("task").and_then(Value::as_str))
                .or_else(|| flow.get("summary").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .map(|text| format!(" - {text}"))
                .unwrap_or_default();
            lines.push(format!("- `{id}` stage={stage} state={state}{summary}"));
        }
    }

    if lines.len() <= 2 {
        lines.push(format!("```json\n{}\n```", value));
    }
    lines.join("\n")
}

fn append_known_field(lines: &mut Vec<String>, value: &Value, field: &str) {
    let Some(raw) = value.get(field) else {
        return;
    };
    if raw.is_null() {
        return;
    }
    if let Some(text) = raw.as_str() {
        if !text.is_empty() {
            lines.push(format!("{field}: `{text}`"));
        }
    } else {
        lines.push(format!("{field}: `{raw}`"));
    }
}

fn skill_discover_result_is_callable(cap: &Value) -> bool {
    cap.get("callable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && cap
            .get("review_status")
            .and_then(|v| v.as_str())
            .is_some_and(|status| status.eq_ignore_ascii_case("approved"))
        && cap
            .get("health_status")
            .and_then(|v| v.as_str())
            .map(|status| {
                !matches!(
                    status.to_ascii_lowercase().as_str(),
                    "open" | "unhealthy" | "failing" | "broken" | "error"
                )
            })
            .unwrap_or(true)
}

fn canonical_skill_name(cap: &Value) -> Option<String> {
    let raw = cap
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| cap.get("name").and_then(Value::as_str))?;
    let without_kind = raw
        .strip_prefix("host-skill:")
        .or_else(|| raw.strip_prefix("skill:waza-"))
        .or_else(|| raw.strip_prefix("skill:"))
        .unwrap_or(raw);
    let trimmed = without_kind.strip_prefix("waza/").unwrap_or(without_kind);
    let normalized = trimmed.trim().to_ascii_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

fn required_skill_query(params: &TachiSkillParams, action: &str) -> Result<String, String> {
    params
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("query is required when action='{action}'"))
}

fn skill_bundle_params(
    params: &TachiSkillParams,
    query: String,
    host: Option<String>,
) -> PrepareCapabilityBundleParams {
    PrepareCapabilityBundleParams {
        query,
        host,
        skill_limit: params.skill_limit.unwrap_or(3).max(1),
        capability_limit: params.capability_limit.unwrap_or(3).max(1),
        pack_limit: params.pack_limit.unwrap_or(3).max(1),
        include_section: params.include_section.unwrap_or(true),
    }
}

fn discover_local_host_skills(query: &str, limit: usize) -> Vec<Value> {
    if limit == 0 {
        return Vec::new();
    }
    let query_tokens = skill_query_tokens(query);
    let mut candidates = Vec::new();
    for root in local_skill_roots() {
        collect_local_skills(&root, &query_tokens, &mut candidates);
    }
    candidates.sort_by(|a, b| {
        let score_b = b.get("_score").and_then(Value::as_i64).unwrap_or(0);
        let score_a = a.get("_score").and_then(Value::as_i64).unwrap_or(0);
        score_b.cmp(&score_a).then_with(|| {
            a.get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
        })
    });
    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|cap| {
            cap.get("id")
                .and_then(Value::as_str)
                .map(|id| seen.insert(id.to_string()))
                .unwrap_or(true)
        })
        .filter(|cap| {
            query_tokens.is_empty() || cap.get("_score").and_then(Value::as_i64) > Some(0)
        })
        .take(limit)
        .map(|mut cap| {
            if let Some(obj) = cap.as_object_mut() {
                obj.remove("_score");
            }
            cap
        })
        .collect()
}

fn local_skill_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        roots.push(home.join(".agents/skills"));
        roots.push(home.join(".codex/skills"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("skill"));
    }
    roots
}

fn skill_query_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for raw in query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let token = raw.to_ascii_lowercase();
        if matches!(
            token.as_str(),
            "中文" | "英文" | "chinese" | "english" | "zh" | "en"
        ) {
            continue;
        }
        if token.len() >= 3 || matches!(token.as_str(), "pr" | "ui" | "ux") {
            tokens.push(token.clone());
        }
        append_skill_query_aliases(raw, &mut tokens);
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

fn append_skill_query_aliases(raw: &str, tokens: &mut Vec<String>) {
    let lower = raw.to_ascii_lowercase();
    let mut add = |aliases: &[&str]| {
        tokens.extend(aliases.iter().map(|alias| (*alias).to_string()));
    };

    if raw.contains("代码审查") || raw.contains("审查") || raw.contains("评审") {
        add(&["check", "code-review"]);
    }
    if raw.contains("修复") || raw.contains("修") || raw.contains("报错") {
        add(&["fix", "gh-fix-ci", "hunt", "repair"]);
    }
    if raw.contains("排查") || raw.contains("调试") || raw.contains("不工作") {
        add(&["debug", "hunt", "investigate"]);
    }
    if raw.contains("计划")
        || raw.contains("规划")
        || raw.contains("方案")
        || raw.contains("设计一下")
    {
        add(&["brainstorm", "plan", "think"]);
    }
    if raw.contains("设计") || raw.contains("前端") || raw.contains("页面") {
        add(&["design", "ui", "ux"]);
    }
    if raw.contains("合并") || raw.contains("提交") || raw.contains("推送") {
        add(&["check", "commit", "merge", "push"]);
    }
    if raw.contains("测试") || raw.contains("验证") {
        add(&["test", "verify"]);
    }
    if raw.contains("文档") || raw.contains("润色") {
        add(&["docs", "read", "write"]);
    }
    if lower == "ci" {
        add(&["fix", "gh-fix-ci"]);
    }
    if lower == "pr" {
        add(&["check", "pr", "review"]);
    }
}

fn collect_local_skills(root: &std::path::Path, query_tokens: &[String], out: &mut Vec<Value>) {
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.exists() {
                if let Some(skill) = local_skill_from_file(&skill_md, query_tokens) {
                    out.push(skill);
                }
            } else {
                collect_local_skills(&path, query_tokens, out);
            }
        }
    }
}

fn local_skill_from_file(path: &std::path::Path, query_tokens: &[String]) -> Option<Value> {
    let content = std::fs::read_to_string(path).ok()?;
    let name = front_matter_value(&content, "name").unwrap_or_else(|| {
        path.parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("skill")
            .to_string()
    });
    let description = front_matter_value(&content, "description")
        .or_else(|| front_matter_value(&content, "when_to_use"))
        .unwrap_or_else(|| first_markdown_heading(&content).unwrap_or_default());
    let haystack = format!(
        "{} {} {}",
        name,
        description,
        front_matter_value(&content, "when_to_use").unwrap_or_default()
    )
    .to_ascii_lowercase();
    let score = if query_tokens.is_empty() {
        1
    } else {
        query_tokens
            .iter()
            .filter(|token| haystack.contains(token.as_str()))
            .count() as i64
    };
    if !query_tokens.is_empty() && score == 0 {
        return None;
    }
    Some(json!({
        "id": format!("host-skill:{}", name),
        "name": name,
        "description": description,
        "cap_type": "skill",
        "enabled": true,
        "review_status": "approved",
        "health_status": "healthy",
        "visibility": "host-local",
        "callable": true,
        "db": "host",
        "source": "host_skill_dir",
        "path": path.display().to_string(),
        "_score": score,
    }))
}

fn front_matter_value(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines {
        if line == "---" {
            return None;
        }
        let Some((raw_key, raw_value)) = line.split_once(':') else {
            continue;
        };
        if raw_key.trim() == key {
            let value = raw_value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn first_markdown_heading(content: &str) -> Option<String> {
    content
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_response_defaults_to_markdown_and_preserves_json_opt_in() {
        let raw = r#"{"flow_id":"flow_1","stage":"plan","state":"instruction_ready","tasks":[{"dispatch_id":"d1","state":"running","agent":"codex","task":"Fix search"}]}"#;
        let markdown = format_facade_response("Tachi shell plan", "plan", raw, None);
        assert!(markdown.starts_with("## Tachi shell plan"));
        assert!(markdown.contains("flow_id: `flow_1`"));
        assert!(markdown.contains("- `d1` running agent=codex - Fix search"));

        let json = format_facade_response("Tachi shell plan", "plan", raw, Some("json"));
        assert_eq!(json, raw);
    }

    #[test]
    fn local_skill_discovery_scans_host_skill_dirs() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_home = std::env::var_os("HOME");
        let temp_home =
            std::env::temp_dir().join(format!("tachi-local-skill-test-{}", uuid::Uuid::new_v4()));
        let skill_dir = temp_home.join(".agents/skills/agent-only-probe");
        let duplicate_skill_dir = temp_home.join(".codex/skills/agent-only-probe");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        std::fs::create_dir_all(&duplicate_skill_dir).expect("create duplicate skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: agent-only-probe\ndescription: Use for zhsearchprobe workflows\n---\n# Agent Only Probe\n",
        )
        .expect("write skill");
        std::fs::write(
            duplicate_skill_dir.join("SKILL.md"),
            "---\nname: agent-only-probe\ndescription: Use for zhsearchprobe workflows\n---\n# Agent Only Probe Duplicate\n",
        )
        .expect("write duplicate skill");
        std::env::set_var("HOME", &temp_home);

        let found = discover_local_host_skills("zhsearchprobe", 5);

        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = std::fs::remove_dir_all(&temp_home);

        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].get("name").and_then(Value::as_str),
            Some("agent-only-probe")
        );
        assert_eq!(
            found[0].get("source").and_then(Value::as_str),
            Some("host_skill_dir")
        );
    }

    #[test]
    fn local_skill_discovery_expands_common_chinese_queries() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_home = std::env::var_os("HOME");
        let temp_home = std::env::temp_dir().join(format!(
            "tachi-local-skill-zh-test-{}",
            uuid::Uuid::new_v4()
        ));
        let skill_dir = temp_home.join(".codex/skills/gh-fix-ci");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: gh-fix-ci\ndescription: Inspect GitHub PR checks and fix failing CI workflows\n---\n# GH Fix CI\n",
        )
        .expect("write skill");
        std::env::set_var("HOME", &temp_home);

        let found = discover_local_host_skills("中文 代码审查 修复 CI", 5);

        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = std::fs::remove_dir_all(&temp_home);

        assert!(
            found
                .iter()
                .any(|cap| cap.get("name").and_then(Value::as_str) == Some("gh-fix-ci")),
            "expected Chinese query aliases to find gh-fix-ci: {found:?}"
        );
    }
}
