use crate::utils::is_trusted_mcp_command;
use memcore::MemoryStore;
use serde::Serialize;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitStatus;
use tachi_bootstrap::cli::Cli;

mod backfill;
mod build_cli;
mod clean_cli;
mod cli_tool;
mod env_cmd;
mod eval_cli;
mod harness_cli;
mod injection_surface_cli;
mod instruction_drift_sentinel;
mod instruction_manifest;
mod manifest_cli;
mod migrate_cli;
mod poke_cli;
mod recall_coverage_cli;
mod rescue_cli;
mod serve;
mod setup;
pub(crate) mod setup_wizard;
mod skill_surface_cli;
mod tidy;
mod vault_sync;
// Crate-visible so the Wiki search integration tests can drive
// `--adopt-legacy` end to end (tachi#1624): the discriminator is that a
// federated Wiki search flips from a zero-store refusal to a real hit, and
// that can only be observed from a `MemoryServer`, not from inside this
// module. Every item in it is already `pub(crate)` or private.
pub(crate) mod wiki_corpus;

mod vault_cli;

#[derive(Debug)]
pub(crate) struct VaultExecExit {
    code: i32,
    status: ExitStatus,
}

impl VaultExecExit {
    pub(crate) fn from_status(status: ExitStatus) -> Self {
        Self {
            // A signal has no portable process exit code to re-emit. Keep the
            // conventional non-zero failure code while preserving every real
            // child exit code unchanged.
            code: status.code().unwrap_or(1),
            status,
        }
    }

    pub(crate) fn code(&self) -> i32 {
        self.code
    }
}

impl std::fmt::Display for VaultExecExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vault exec child exited with {}", self.status)
    }
}

impl std::error::Error for VaultExecExit {}

// Feature-gated friend surface for the external bootstrap test crate. Product
// builds keep these implementation functions private to their defining modules.
#[cfg(feature = "bootstrap-test-api")]
pub(crate) use setup::build_setup_report;
#[cfg(feature = "bootstrap-test-api")]
pub use tidy::MigrationConfig;
#[cfg(feature = "bootstrap-test-api")]
pub(crate) use tidy::{
    authorized_migration_sources, build_migration_plan, build_tidy_report, execute_tidy_apply,
    execute_tidy_migrations, force_boundary_failure_after_archive_stage,
    update_manifest_after_migration,
};

// Phase 2 (LLM 3-layer consolidation): Voyage covers vectors, SiliconFlow
// covers front-line extraction, and DeepSeek is the preferred low-friction
// foundry distill/reasoning lane. Legacy `MINIMAX_*`, `DISTILL_*` and
// `REASONING_*` env vars remain recognised so `tachi setup` surfaces a soft
// warning instead of silently ignoring existing user configs.
pub(super) struct SetupApiKey {
    pub key: &'static str,
    pub label: &'static str,
    pub deprecated: bool,
}

pub(super) const SETUP_API_KEYS: [SetupApiKey; 6] = [
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
        label: "SiliconFlow extraction (front-line extract/summary)",
        deprecated: false,
    },
    SetupApiKey {
        key: "DEEPSEEK_API_KEY",
        label: "DeepSeek distill/reasoning (optional foundry lane)",
        deprecated: false,
    },
    SetupApiKey {
        key: "MINIMAX_API_KEY",
        label: "MiniMax distill/summary — DEPRECATED compatibility key",
        deprecated: true,
    },
    SetupApiKey {
        key: "REASONING_API_KEY",
        label: "Reasoning API fallback (optional)",
        deprecated: false,
    },
];

#[cfg(test)]
mod setup_api_key_tests {
    use super::SETUP_API_KEYS;

    #[test]
    fn deprecated_compatibility_keys_do_not_claim_active_routing() {
        let entry = SETUP_API_KEYS
            .iter()
            .find(|entry| entry.key == "MINIMAX_API_KEY")
            .expect("deprecated setup key must remain listed");
        assert!(entry.deprecated, "MINIMAX_API_KEY must remain deprecated");
        assert!(
            entry.label.contains("DEPRECATED compatibility key"),
            "MINIMAX_API_KEY must be described only as a compatibility key: {}",
            entry.label
        );
        assert!(!entry.label.contains("Claude pool"));
    }

    #[test]
    fn live_reasoning_key_remains_available_to_setup() {
        let entry = SETUP_API_KEYS
            .iter()
            .find(|entry| entry.key == "REASONING_API_KEY")
            .expect("live reasoning key must remain listed");
        assert!(!entry.deprecated, "live reasoning key must be prompted");
        assert!(!entry.label.contains("DEPRECATED"));
    }
}

pub(super) const DEFAULT_STANDARD_PROFILE_NOTICE: &str =
    "No profile specified; defaulting to 'standard'. Set TACHI_PROFILE=admin to restore legacy full surface (148 tools).";

#[derive(Debug, Clone, Serialize)]
pub struct SetupItem {
    pub id: String,
    pub label: String,
    pub status: String,
    pub details: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupReport {
    pub app_home: String,
    pub config_env_path: String,
    pub global_db_path: String,
    pub project_db_path: Option<String>,
    pub git_root: Option<String>,
    pub items: Vec<SetupItem>,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TidyFinding {
    pub path: String,
    pub entry_count: Option<usize>,
    pub vec_available: bool,
    pub scope_suggestion: String,
    pub status: String,
    pub recommended_action: String,
    pub is_symlink: bool,
    pub symlink_target: Option<String>,
    pub target_exists: Option<bool>,
    pub physical_id: Option<String>,
    pub canonical_path: Option<String>,
    /// Read-only probe path selected for SQLite/WAL visibility. It is
    /// inventory evidence only and never authorizes a mutation.
    pub inventory_open_path: Option<String>,
    pub is_primary_alias: bool,
    pub open_failure_kind: Option<crate::physical_db_identity::InventoryFailureKind>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TidyGroupSummary {
    pub group: String,
    pub database_count: usize,
    pub memory_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TidyPlanStep {
    pub order: usize,
    pub scope: String,
    pub action: String,
    pub source_paths: Vec<String>,
    pub target_label: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TidyAppliedStep {
    pub order: usize,
    pub scope: String,
    pub action: String,
    pub outcome: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TidyApplySummary {
    pub report_path: String,
    pub applied_steps: Vec<TidyAppliedStep>,
    pub applied_count: usize,
    pub skipped_count: usize,
}

/// One source DB scheduled for fragment-consolidation migration.
#[derive(Debug, Clone, Serialize)]
pub struct TidyMigration {
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
pub struct TidyMigrationOutcome {
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
pub struct TidyExecuteSummary {
    pub target_db: String,
    pub planned: Vec<TidyMigration>,
    pub outcomes: Vec<TidyMigrationOutcome>,
    pub migrated_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TidyReport {
    pub scanned_roots: Vec<String>,
    pub databases: Vec<TidyFinding>,
    pub groups: Vec<TidyGroupSummary>,
    pub dry_run_plan: Vec<TidyPlanStep>,
    pub total_databases: usize,
    /// Resolved discovered aliases belonging to a physical store. Unresolved
    /// paths are deliberately excluded. Retained as a compatibility field;
    /// new consumers should prefer `resolved_aliases`.
    pub total_aliases: usize,
    pub resolved_aliases: usize,
    pub unresolved_paths: usize,
    /// All discovered path appearances: resolved aliases plus unresolved
    /// paths.
    pub path_appearances: usize,
    pub total_memories: usize,
    pub physical_stores: Vec<crate::physical_db_identity::PhysicalDbStore>,
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
            .map(memcore::is_memory_db_filename)
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
