use crate::cli::{Cli, Commands, HubAction};
use crate::hub_helpers::should_expose_skill_tool;
use crate::kanban::{gc_expired_kanban_cards, DEFAULT_KANBAN_GC_MAX_AGE_DAYS};
use crate::mcp_proxy::filter_mcp_tools_by_permissions;
use crate::server_state::MemoryServer;
use crate::tool_params::{
    ExtractFactsParams, GetMemoryParams, ListMemoriesParams, PackProjectParams, PackRegisterParams,
    RememberParams, SearchMemoryParams, TachiTaskParams, WikiSearchParams, WikiWriteParams,
};
use crate::utils::{
    find_project_git_root, is_trusted_mcp_command, lock_or_recover, parse_env_bool, parse_env_u64,
};
use chrono::Utc;
use memory_core::{HubCapability, MemoryStore};
use rmcp::handler::server::wrapper::Parameters;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{stdin, stdout};

mod backfill;
mod clean_cli;
mod cli_tool;
mod env_cmd;
mod harness_cli;
mod manifest_cli;
mod poke_cli;
mod rescue_cli;
mod serve;
mod setup;
mod setup_wizard;
mod skill_surface_cli;
mod tidy;
mod vault_sync;

pub(crate) use setup_wizard::mcp_server_instructions;
mod vault_cli;

// Re-exports preserving the legacy public surface so external callers
// (`main.rs`, `tests.rs`) keep resolving symbols via `crate::bootstrap::<name>`.
#[cfg(test)]
pub(crate) use setup::build_setup_report;
#[cfg(test)]
pub(crate) use tidy::{
    build_migration_plan, build_tidy_report, execute_tidy_apply, execute_tidy_migrations,
    update_manifest_after_migration, MigrationConfig,
};

// Phase 2 (LLM 3-layer consolidation): only `SILICONFLOW_API_KEY` +
// `VOYAGE_API_KEY` are required going forward. Background skill/foundry
// lanes now go through the Claude CLI pool with SiliconFlow/Qwen as the
// raw-API fallback, so `MINIMAX_*`, `DISTILL_*` and `REASONING_*` env vars
// are deprecated. We keep them recognised here (with a `deprecated` flag)
// so `tachi setup` surfaces a soft warning instead of silently ignoring
// existing user configs.
pub(super) struct SetupApiKey {
    pub key: &'static str,
    pub label: &'static str,
    pub deprecated: bool,
}

pub(super) const SETUP_API_KEYS: [SetupApiKey; 5] = [
    SetupApiKey {
        key: "VOYAGE_API_KEY",
        label: "Voyage embeddings (voyage-4)",
        deprecated: false,
    },
    SetupApiKey {
        key: "VOYAGE_RERANK_API_KEY",
        label: "Voyage reranking (rerank-2.5) — optional",
        deprecated: false,
    },
    SetupApiKey {
        key: "SILICONFLOW_API_KEY",
        label: "SiliconFlow extraction (raw_api fallback for all background lanes)",
        deprecated: false,
    },
    SetupApiKey {
        key: "MINIMAX_API_KEY",
        label:
            "MiniMax distill/summary — DEPRECATED (Phase 2: routed via Claude pool + SiliconFlow)",
        deprecated: true,
    },
    SetupApiKey {
        key: "REASONING_API_KEY",
        label: "GLM-5.1 reasoning lane — DEPRECATED (Phase 2: skill-evolve uses Claude pool)",
        deprecated: true,
    },
];

pub(super) const DEFAULT_STANDARD_PROFILE_NOTICE: &str =
    "No profile specified; defaulting to 'standard'. Set TACHI_PROFILE=admin to restore legacy full surface (148 tools).";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SetupItem {
    pub id: String,
    pub label: String,
    pub status: String,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SetupReport {
    pub app_home: String,
    pub config_env_path: String,
    pub global_db_path: String,
    pub project_db_path: Option<String>,
    pub git_root: Option<String>,
    pub items: Vec<SetupItem>,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyFinding {
    pub path: String,
    pub entry_count: Option<usize>,
    pub vec_available: bool,
    pub scope_suggestion: String,
    pub status: String,
    pub recommended_action: String,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
    pub target_exists: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyGroupSummary {
    pub group: String,
    pub database_count: usize,
    pub memory_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyPlanStep {
    pub order: usize,
    pub scope: String,
    pub action: String,
    pub source_paths: Vec<String>,
    pub target_label: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyAppliedStep {
    pub order: usize,
    pub scope: String,
    pub action: String,
    pub outcome: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyApplySummary {
    pub report_path: String,
    pub applied_steps: Vec<TidyAppliedStep>,
    pub applied_count: usize,
    pub skipped_count: usize,
}

/// One source DB scheduled for fragment-consolidation migration.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyMigration {
    pub source_path: String,
    pub target_path: String,
    pub archive_path: String,
    pub scope_suggestion: String,
    pub action: String,
    pub source_row_count: usize,
    pub reason: String,
}

/// Result of a single migration execution.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyMigrationOutcome {
    pub source_path: String,
    pub target_path: String,
    pub archive_path: Option<String>,
    pub status: String, // "migrated" | "skipped" | "failed" | "dry_run"
    pub rows_before_target: usize,
    pub rows_after_target: usize,
    pub rows_copied: usize,
    pub message: String,
}

/// Aggregate result of the `--execute` path.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyExecuteSummary {
    pub target_db: String,
    pub planned: Vec<TidyMigration>,
    pub outcomes: Vec<TidyMigrationOutcome>,
    pub migrated_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TidyReport {
    pub scanned_roots: Vec<String>,
    pub databases: Vec<TidyFinding>,
    pub groups: Vec<TidyGroupSummary>,
    pub dry_run_plan: Vec<TidyPlanStep>,
    pub total_databases: usize,
    pub total_memories: usize,
    pub next_steps: Vec<String>,
}

pub(super) fn open_cli_store(db_path: &PathBuf) -> Result<MemoryStore, Box<dyn std::error::Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;
    Ok(MemoryStore::open(db_str)?)
}

pub(super) fn open_cli_store_read_only(
    db_path: &PathBuf,
) -> Result<MemoryStore, Box<dyn std::error::Error>> {
    let db_str = db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("DB path contains invalid UTF-8: {}", db_path.display()),
        )
    })?;
    Ok(MemoryStore::open_read_only(db_str)?)
}

pub(super) fn print_pretty_json(
    value: &serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

// `build_cli_memory_entry` was removed alongside the legacy `Commands::Save`
// arm. `tachi save` now aliases to `tachi remember`, which routes through
// `crate::memory_search_ops::handle_remember` and constructs its `MemoryEntry`
// from the richer `RememberParams` (tags, scope, project, category, topic,
// domain, retention-policy, summary, force).

pub(super) fn evaluate_cli_capability_enabled(
    cap_type: &str,
    definition: &str,
) -> Result<(bool, Option<String>), Box<dyn std::error::Error>> {
    if cap_type != "mcp" {
        return Ok((true, None));
    }

    let def: serde_json::Value = serde_json::from_str(definition)?;
    let transport_type = def["transport"].as_str().unwrap_or("stdio");
    if transport_type != "stdio" {
        return Ok((true, None));
    }

    match def["command"].as_str() {
        Some(cmd) if is_trusted_mcp_command(cmd) => Ok((true, None)),
        Some(cmd) => Ok((
            false,
            Some(format!(
                "Command '{}' is not in the trusted allowlist. Capability registered but disabled.",
                cmd
            )),
        )),
        None => Ok((
            false,
            Some(
                "mcp definition missing 'command' for stdio transport. Capability registered but disabled."
                    .to_string(),
            ),
        )),
    }
}

pub(super) fn count_matching_entries(
    root: &std::path::Path,
    matcher: &dyn Fn(&std::path::Path) -> bool,
) -> usize {
    std::fs::read_dir(root)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter(|entry| matcher(&entry.path()))
        .count()
}

pub(super) fn collect_memory_db_files(
    root: &std::path::Path,
    out: &mut Vec<PathBuf>,
    max_depth: usize,
) {
    if !root.exists() {
        return;
    }

    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let is_memory_db = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == "memory.db")
            .unwrap_or(false);
        let is_symlink = std::fs::symlink_metadata(&path)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false);

        if path.is_file() || (is_memory_db && is_symlink) {
            if is_memory_db {
                out.push(path);
            }
            continue;
        }

        if path.is_dir() && max_depth > 0 {
            collect_memory_db_files(&path, out, max_depth.saturating_sub(1));
        }
    }
}

pub(super) fn atty_stdout() -> bool {
    std::io::stdout().is_terminal()
}

pub(super) fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    serve::tokio_main(cli)
}
