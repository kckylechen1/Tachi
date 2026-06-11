use clap::{Parser, Subcommand};
use std::path::PathBuf;

// ─── CLI Arguments ────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "tachi", version, about = "Tachi — memory + Hub MCP server")]
pub(crate) struct Cli {
    /// Run as HTTP daemon instead of stdio transport
    #[arg(long)]
    pub daemon: bool,

    /// Port for HTTP daemon (default: 6919)
    #[arg(long, default_value_t = 6919)]
    pub port: u16,

    /// Override global memory DB path (equivalent to MEMORY_DB_PATH)
    #[arg(long, value_name = "PATH")]
    pub global_db: Option<PathBuf>,

    /// Override project memory DB path
    #[arg(long, value_name = "PATH")]
    pub project_db: Option<PathBuf>,

    /// Disable project DB entirely (force single-DB mode)
    #[arg(long)]
    pub no_project_db: bool,

    /// Built-in tool surface bundles or host alias, e.g. remember, observe+coordinate, openclaw, admin
    #[arg(long)]
    pub profile: Option<String>,

    /// Enable/disable background database GC (overrides MEMORY_GC_ENABLED)
    #[arg(long)]
    pub gc_enabled: Option<bool>,

    /// Delay before first background GC run in seconds (overrides MEMORY_GC_INITIAL_DELAY_SECS)
    #[arg(long)]
    pub gc_initial_delay_secs: Option<u64>,

    /// Interval between background GC runs in seconds (overrides MEMORY_GC_INTERVAL_SECS)
    #[arg(long)]
    pub gc_interval_secs: Option<u64>,

    /// CLI command (defaults to `serve` when omitted)
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum Commands {
    /// Start MCP Server (default when no subcommand is provided)
    Serve,
    /// Search memories
    Search {
        query: String,
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 5)]
        top_k: usize,
        /// Optional named project DB.
        #[arg(long)]
        project: Option<String>,
    },
    // Save is now an alias of `remember` — see `Remember` below, which carries
    // `#[command(alias = "save")]`.
    //
    // Historically `save` only accepted `--path` / `--importance`. We now
    // route it through the same `remember`-backed handler so the full flag
    // set (tags, scope, project, category, topic, domain, retention-policy,
    // summary, force) is available on both verbs and behavior is identical
    // (daemon-forwarding when a hub is up, in-process otherwise). The legacy
    // 3-flag invocation `tachi save TEXT --path X --importance Y` keeps
    // working unchanged because clap accepts the alias and all extra flags
    // are optional.
    /// Show database statistics
    Stats,
    /// Inspect onboarding readiness or run the interactive 5-step setup wizard
    Setup {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
        /// Force the interactive 5-step onboarding wizard (overrides TTY detection)
        #[arg(long, conflicts_with_all = ["json", "non_interactive"])]
        interactive: bool,
        /// Force report-only mode (no prompts) even when stdout is a TTY
        #[arg(long)]
        non_interactive: bool,
    },
    /// Scan for fragmented memory databases and report consolidation candidates
    Tidy {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
        /// Execute the conservative apply path for clearly safe actions (legacy alias)
        #[arg(long)]
        apply: bool,
        /// Show the migration plan without making any writes (default behavior)
        #[arg(long)]
        dry_run: bool,
        /// Execute fragment-DB consolidation: migrate rows, archive sources, update manifest
        #[arg(long)]
        execute: bool,
        /// Skip per-DB interactive confirmation prompts (requires --execute)
        #[arg(long)]
        yes: bool,
        /// Override the migration target DB (defaults to ~/.tachi/global/memory.db)
        #[arg(long, value_name = "PATH")]
        target_db: Option<PathBuf>,
    },
    /// Clean build artifacts, Tachi-managed worktrees, and Tachi runtime leftovers.
    /// Dry-run by default; pass --force on a subcommand to delete.
    Clean {
        #[command(subcommand)]
        action: CleanAction,
    },
    /// Doctor v2 — extension-aware DB classification (read-only by default)
    Doctor {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
        /// Execute the safe auto-fix pass (placeholder quarantine + WAL copy-aside)
        #[arg(long, alias = "apply")]
        fix: bool,
        /// Legacy no-op alias; doctor is read-only unless --fix is supplied
        #[arg(long, hide = true)]
        scan_only: bool,
        /// Override default scan roots (~/.tachi, ~/.openclaw, ~/.sigil, ~/.gemini/antigravity)
        #[arg(long, value_name = "PATH")]
        roots: Vec<PathBuf>,
        /// Branch #5: also report a foundry job-status histogram per manifest DB
        #[arg(long)]
        jobs: bool,
        /// Probe configured provider keys with live embedding/chat smoke calls
        #[arg(long)]
        probe_keys: bool,
    },
    /// Manifest v1 — show/init/refresh ~/.tachi/manifest.json
    Manifest {
        #[command(subcommand)]
        action: ManifestAction,
    },
    /// Run garbage collection
    Gc,
    /// Hub registry (list/show/packs/bindings/stats/doctor) and capability management
    Hub {
        #[command(subcommand)]
        action: HubAction,
    },
    /// Backfill missing vector embeddings using Voyage API
    BackfillVectors {
        /// Target DB path (defaults to global DB)
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        /// Batch size for Voyage API calls (max 128)
        #[arg(long, default_value_t = 64)]
        batch_size: usize,
        /// Only count missing entries, don't embed
        #[arg(long)]
        dry_run: bool,
        /// Include ephemeral recall-rerank cache rows. Defaults to false so
        /// output matches `tachi status` vector-health coverage.
        #[arg(long)]
        include_cache: bool,
    },
    /// Backfill missing summaries using the configured summary LLM
    BackfillSummaries {
        /// Target DB path (defaults to global DB)
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        /// Only count missing entries, don't generate summaries
        #[arg(long)]
        dry_run: bool,
    },
    /// Backfill missing keywords/entities using the configured extract LLM
    BackfillMetadata {
        /// Target DB path (defaults to global DB)
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        /// Only count missing entries, don't extract metadata
        #[arg(long)]
        dry_run: bool,
    },
    /// Rebuild or backfill FTS5 full-text search index
    BackfillFts {
        /// Target DB path (defaults to global DB)
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        /// Drop and fully rebuild the FTS table (useful after corruption)
        #[arg(long)]
        full: bool,
        /// Only show stats, don't modify
        #[arg(long)]
        dry_run: bool,
    },
    /// Run batch memory distill (raw API by default; Claude CLI when configured).
    Distill {
        #[command(subcommand)]
        action: DistillAction,
    },
    /// Branch #6 — Rescue: split a multi-project memory.db into per-project Tachi DBs.
    /// Plan-only by default; pass --apply to actually write into target DBs.
    Rescue {
        #[command(subcommand)]
        action: RescueAction,
    },
    /// Quick-capture a memory note. Mirrors the `remember` MCP tool.
    /// Forwards to a running daemon if one is detected; otherwise runs in-process.
    #[command(alias = "save")]
    Remember {
        /// Full text content to remember.
        text: String,
        /// Optional comma-separated tags (forwarded as keywords).
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// Scope: "user" | "project" | "general". Defaults to "project".
        #[arg(long)]
        scope: Option<String>,
        /// Optional named project DB target (e.g. "hyperion", "wiki").
        #[arg(long)]
        project: Option<String>,
        /// Optional path override. Defaults to /notes/YYYY-MM-DD.
        #[arg(long)]
        path: Option<String>,
        /// Importance score 0.0–1.0. Defaults to 0.6.
        #[arg(long)]
        importance: Option<f64>,
        /// Optional category override. Defaults to "fact".
        #[arg(long)]
        category: Option<String>,
        /// Optional topic / subject area.
        #[arg(long)]
        topic: Option<String>,
        /// Optional domain (e.g. "finance", "code-review").
        #[arg(long)]
        domain: Option<String>,
        /// Optional retention policy.
        #[arg(long)]
        retention_policy: Option<String>,
        /// Optional ≤100-char summary.
        #[arg(long)]
        summary: Option<String>,
        /// Bypass noise filter.
        #[arg(long)]
        force: bool,
    },
    /// Search the wiki bucket. Mirrors the `tachi_wiki_search` MCP tool.
    WikiSearch {
        /// Search query.
        query: String,
        /// Wiki category filter (e.g. "quant" or "/wiki/engineering").
        #[arg(long)]
        category: Option<String>,
        /// Number of results to return.
        #[arg(long, default_value_t = 10)]
        top_k: usize,
        /// Optional named project DB.
        #[arg(long)]
        project: Option<String>,
    },
    /// Write a wiki entry. Mirrors the `tachi_wiki_write` MCP tool.
    WikiWrite {
        /// Short title for the wiki entry.
        title: String,
        /// Full wiki entry body.
        text: String,
        /// Optional explicit /wiki path.
        #[arg(long)]
        path: Option<String>,
        /// Optional topic.
        #[arg(long)]
        topic: Option<String>,
        /// Short summary.
        #[arg(long)]
        summary: Option<String>,
        /// Comma-separated keyword tags.
        #[arg(long, value_delimiter = ',')]
        keywords: Vec<String>,
        /// Comma-separated entity names.
        #[arg(long, value_delimiter = ',')]
        entities: Vec<String>,
        /// Importance score (default: 0.85).
        #[arg(long)]
        importance: Option<f64>,
        /// Scope (default: "global" for wiki).
        #[arg(long)]
        scope: Option<String>,
        /// Optional named project DB.
        #[arg(long)]
        project: Option<String>,
        /// Optional domain.
        #[arg(long)]
        domain: Option<String>,
        /// Bypass noise filter.
        #[arg(long)]
        force: bool,
    },
    /// Wiki utilities.
    Wiki {
        #[command(subcommand)]
        action: WikiAction,
    },
    /// List memories under a path prefix. Mirrors the `list_memories` MCP tool.
    List {
        /// Path prefix filter. Defaults to "/".
        #[arg(long, default_value = "/")]
        path_prefix: String,
        /// Maximum number of entries to return.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Include archived entries.
        #[arg(long)]
        include_archived: bool,
    },
    /// Get a single memory by ID. Mirrors the `get_memory` MCP tool.
    Get {
        /// Memory entry ID.
        id: String,
        /// Optional named project DB to search first.
        #[arg(long)]
        project: Option<String>,
        /// Include archived entries.
        #[arg(long)]
        include_archived: bool,
    },
    /// Extract structured facts from text. Mirrors the `extract_facts` MCP tool.
    Extract {
        /// Text to extract facts from.
        text: String,
        /// Source identifier for the extraction.
        #[arg(long, default_value = "cli")]
        source: String,
    },
    /// Show daemon + scheduler + per-DB foundry status.
    ///
    /// Inspects ~/.tachi/daemon.lock + ~/.tachi/manifest.json and queries
    /// each manifest DB for foundry job status. Highlights orphan DBs
    /// (manifest entries the running daemon's scheduler cannot route to)
    /// and stuck in_progress jobs older than the configured threshold.
    Status {
        /// Re-render every 2 seconds (clear screen between frames).
        #[arg(long)]
        watch: bool,
        /// Emit machine-readable JSON instead of the human summary.
        #[arg(long)]
        json: bool,
        /// Suppress the per-row `[!] orphan` marker and exclude orphan DBs
        /// from the human-readable Manifest section. Orphans still count
        /// toward the Summary line so the total isn't silently misleading.
        ///
        /// "Orphan" here means: the manifest entry exists, but the running
        /// daemon's scheduler has no route that maps writes to that DB
        /// (typically agent-owned DBs from extensions whose hub plugin is
        /// not installed). It is NOT an error — just noise on hosts that
        /// keep many extension manifests around. `--json` output is
        /// unaffected so machine consumers always see the orphan flag.
        #[arg(long)]
        hide_orphans: bool,
        /// Also run live provider smoke probes. This can make network calls and should not be used in cheap polling loops.
        #[arg(long)]
        probe_keys: bool,
    },
    /// Inspect or terminate the running tachi daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Inspect or capture passive agent transcript sources such as Claude JSONL.
    Watcher {
        #[command(subcommand)]
        action: WatcherAction,
    },
    /// Per-DB foundry runtime configuration.
    Foundry {
        #[command(subcommand)]
        action: FoundryAction,
    },
    /// PR-5: data-fix operations across manifest DBs (FTS rebuild, retention
    /// backfill, quarantine resolution, foundry job purge, integrity check,
    /// VACUUM, orphan reference cleanup). Default is dry-run; pass --apply to
    /// actually mutate. Per-DB backup auto-taken before any mutation.
    Repair {
        #[command(subcommand)]
        action: Option<RepairAction>,
        /// Restrict to a single DB by manifest label / scope hint
        /// (e.g. "global", "project:hapi", or an absolute path).
        #[arg(long, value_name = "LABEL", global = true)]
        db: Option<String>,
        /// Comma-separated rule IDs to run, e.g. "R1,R2,R5". Default: all.
        #[arg(long, value_delimiter = ',', global = true)]
        rule: Vec<String>,
        /// Apply repairs (mutate). Without this flag, dry-run only.
        #[arg(long, global = true)]
        apply: bool,
        /// Skip the per-DB backup. DANGEROUS, not the default.
        #[arg(long, global = true)]
        no_backup: bool,
        /// Emit a machine-readable JSON report instead of the human summary.
        #[arg(long, global = true)]
        json: bool,
        /// Also purge `failed` foundry_jobs older than N days during R4
        /// (default cadence excludes them). Useful for clearing legacy
        /// per-capture MemoryDistill failures after the Phase 1 migration.
        #[arg(long, value_name = "DAYS", num_args = 0..=1, default_missing_value = "14", global = true)]
        purge_failed: Option<u64>,
    },
    /// Vault secret management (init, unlock, set, get, remove, list, lock, status).
    Vault {
        #[command(subcommand)]
        action: VaultAction,
    },
    /// Output vault secrets as `export KEY=VALUE` lines for shell injection.
    /// Replaces project .env files — pipe into shell with: eval "$(tachi env --keychain)"
    /// or on non-macOS hosts: eval "$(tachi env --password-file ~/.config/tachi/vault-password)".
    Env {
        #[command(subcommand)]
        action: Option<EnvAction>,
        /// Optional glob-style filter on secret names (e.g. "OPENAI*" or "*API_KEY").
        /// Without this flag, all unrestricted secrets are emitted.
        #[arg(long, global = true)]
        filter: Option<String>,
        /// Only emit secrets whose names match a prefix commonly used as env vars
        /// (all-uppercase with underscores, e.g. OPENAI_API_KEY).
        #[arg(long, global = true)]
        env_only: bool,
        /// Read master password from stdin instead of prompting interactively.
        /// Useful for scripts: echo "$PASS" | tachi env --stdin-password
        #[arg(long, global = true)]
        stdin_password: bool,
        /// Read master password from macOS Keychain instead of prompting.
        /// Uses service name "tachi-vault", account "default".
        #[arg(long, global = true)]
        keychain: bool,
        /// Read master password from a local file (first line only).
        /// Useful on Linux/Windows with OS/container secret mounts.
        #[arg(long, value_name = "PATH", global = true)]
        password_file: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum EnvAction {
    /// Inspect project .tachi/vault.env bindings without decrypting values.
    Plan {
        /// Project directory to inspect. Defaults to current directory.
        #[arg(long, value_name = "PATH")]
        cwd: Option<PathBuf>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Export project-bound Vault secrets as shell exports.
    Export {
        /// Project directory to inspect. Defaults to current directory.
        #[arg(long, value_name = "PATH")]
        cwd: Option<PathBuf>,
        /// Emit JSON instead of shell export syntax.
        #[arg(long)]
        json: bool,
    },
    /// Write generated shell exports to .tachi/env.generated for direnv-style sourcing.
    Sync {
        /// Project directory to inspect. Defaults to current directory.
        #[arg(long, value_name = "PATH")]
        cwd: Option<PathBuf>,
        /// Output file. Defaults to <project>/.tachi/env.generated.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Preview the target path and bindings without writing.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite an existing output file.
        #[arg(long)]
        force: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run a command with project-bound Vault secrets injected into its environment.
    Run {
        /// Project directory to inspect and use as command cwd. Defaults to current directory.
        #[arg(long, value_name = "PATH")]
        cwd: Option<PathBuf>,
        /// Command and arguments to execute. Use `--` before the command.
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum CleanAction {
    /// Clean Cargo target artifacts while preserving top-level release binaries.
    Target {
        /// Repository root or target directory. Defaults to the current directory.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
        /// Delete planned artifacts. Without this flag, only prints the plan.
        #[arg(long, conflicts_with = "dry_run")]
        force: bool,
        /// Preview only (default).
        #[arg(long)]
        dry_run: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove a Tachi-managed git worktree after safety checks.
    #[command(alias = "wt-remove")]
    Worktree {
        /// Worktree path to remove.
        path: PathBuf,
        /// Actually remove the worktree. Without this flag, only prints the plan.
        #[arg(long, conflicts_with = "dry_run")]
        force: bool,
        /// Preview only (default).
        #[arg(long)]
        dry_run: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Sweep stale Tachi-managed worktrees under temp roots.
    Sweep {
        /// Root to scan. Repeatable; defaults to TMPDIR, /private/tmp, and temp_dir.
        #[arg(long, value_name = "PATH")]
        root: Vec<PathBuf>,
        /// Minimum age in days before a marked worktree is considered stale.
        #[arg(long, default_value_t = tachi_clean::sweep::DEFAULT_SWEEP_MAX_AGE_DAYS)]
        max_age_days: u64,
        /// Remove candidates with git worktree remove --force.
        #[arg(long, conflicts_with = "dry_run")]
        force: bool,
        /// Preview only (default).
        #[arg(long)]
        dry_run: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Clean Tachi self-maintenance artifacts under TACHI_HOME or ~/.tachi.
    #[command(name = "tachi")]
    Tachi {
        /// Tachi home. Defaults to TACHI_HOME or ~/.tachi.
        #[arg(long, value_name = "PATH")]
        home: Option<PathBuf>,
        /// Delete planned artifacts. Without this flag, only prints the plan.
        #[arg(long, conflicts_with = "dry_run")]
        force: bool,
        /// Preview only (default).
        #[arg(long)]
        dry_run: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum RepairAction {
    /// Quarantine resolution helpers (PR-3 v4 migration aftermath).
    Quarantine {
        #[command(subcommand)]
        action: QuarantineAction,
    },
    /// VACUUM INTO a temp file then atomically swap. Requires daemon stopped.
    Vacuum {
        /// Manifest label or absolute DB path. Required.
        #[arg(long, value_name = "LABEL")]
        db: String,
        /// Apply (default: dry-run reports planned action only).
        #[arg(long)]
        apply: bool,
    },
    /// Convenience wrapper for R1 (FTS rebuild) on one DB.
    Fts {
        /// Manifest label or absolute DB path. Required.
        #[arg(long, value_name = "LABEL")]
        db: String,
        /// Apply (default: dry-run reports drift only).
        #[arg(long)]
        apply: bool,
    },
    /// Re-emit the most recent dry-run as JSON. Convenience.
    Report {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum QuarantineAction {
    /// List all rows under /_quarantine/cross-db/* across the manifest.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Restore one quarantined row to its original_path within the same DB.
    Restore {
        /// Memory ID to restore.
        #[arg(long)]
        id: String,
        /// Apply (default: dry-run).
        #[arg(long)]
        apply: bool,
    },
    /// Bulk cross-DB move: take all quarantined rows whose
    /// metadata.quarantine.expected_db matches `--to-db` and physically
    /// move them (INSERT into destination DB → verify → DELETE from source).
    RestoreAll {
        /// Destination DB (manifest label or absolute path).
        #[arg(long)]
        to_db: String,
        /// Apply (default: dry-run).
        #[arg(long)]
        apply: bool,
    },
    /// Delete quarantined rows older than N days.
    Purge {
        #[arg(long)]
        older_than: u64,
        /// Apply (default: dry-run).
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DaemonAction {
    /// Show the running daemon's PID, port, started_at, and lock state.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Send SIGTERM to the running daemon (no-op if no daemon is alive).
    Kill {
        /// Skip the live-process check and unlink the lock file regardless.
        /// Use only when a stale ~/.tachi/daemon.lock survived a hard crash
        /// and the PID inside is not actually a tachi process.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum WatcherAction {
    /// Report known passive transcript sources without writing memory.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Capture a concise checkpoint from the latest Claude JSONL transcript.
    CaptureLatest {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum FoundryAction {
    /// Per-DB runtime config: get current values for one DB.
    ConfigGet {
        /// Absolute path to the target DB. Defaults to the global DB.
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
    },
    /// Per-DB runtime config: set one or more values for one DB.
    /// Unset flags leave the existing value untouched.
    ConfigSet {
        /// Absolute path to the target DB. Defaults to the global DB.
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        /// Foundry execution master switch for this DB.
        #[arg(long)]
        enabled: Option<bool>,
        /// Throttle: maximum jobs the in-process worker may run per minute.
        #[arg(long)]
        max_jobs_per_minute: Option<u32>,
        /// Concurrency cap for `distill` lane jobs in this DB.
        #[arg(long)]
        distill_concurrency: Option<u32>,
        /// Concurrency cap for `enrichment` lane jobs in this DB.
        #[arg(long)]
        enrichment_concurrency: Option<u32>,
        /// Optional LLM provider override (e.g. "openai", "anthropic").
        /// Pass an empty string to clear.
        #[arg(long)]
        llm_provider_override: Option<String>,
    },
    /// Per-DB runtime config: list values for every manifest DB.
    ConfigList {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DistillAction {
    /// Run one daily batch distill pass against the project DB.
    Run {
        /// Project DB path (defaults to `--project-db` when set).
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum RescueAction {
    /// Split the legacy antigravity memory.db into per-project Tachi DBs.
    Antigravity {
        /// Source DB path (defaults to ~/.gemini/antigravity/memory.db)
        #[arg(long, value_name = "PATH")]
        source: Option<PathBuf>,
        /// Root directory containing per-project Tachi DBs (defaults to ~/.tachi/projects)
        #[arg(long, value_name = "PATH")]
        targets_root: Option<PathBuf>,
        /// Actually perform the rescue (default: dry-run plan only).
        #[arg(long)]
        apply: bool,
        /// Emit machine-readable JSON instead of the human summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum ManifestAction {
    /// Show the current manifest (human or --json)
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Create a new manifest by running a doctor scan (idempotent)
    Init,
    /// Re-run doctor scan and update the manifest in place
    Refresh,
    /// Resolve a path or scope hint against the manifest (diagnostic)
    Resolve {
        /// Path or scope hint (e.g. "global", "project:hyperion", "/abs/path/memory.db")
        target: String,
    },
    /// Plan or apply a sweep of unowned placeholder/backup DB files (dry-run by default)
    Sweep {
        /// Actually move files to quarantine (default: dry-run)
        #[arg(long)]
        apply: bool,
        /// Output JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Garbage-collect the manifest in place: drop missing files, drop test
    /// fixtures, dedup symlink aliases by canonical path, fix mis-classified
    /// `schema_kind`. Writes `~/.tachi/manifest.json.bak` before mutating.
    Gc {
        /// Output JSON instead of human text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum WikiAction {
    /// Export wiki entries to Markdown files.
    Export {
        /// Export format. Currently only "obsidian" is supported.
        #[arg(long, default_value = "obsidian")]
        format: String,
        /// Output directory.
        #[arg(long, value_name = "DIR")]
        output: PathBuf,
        /// Optional named project DB.
        #[arg(long, default_value = "wiki")]
        project: String,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum HubAction {
    /// List capabilities (table by default; pass --json for machine output)
    List {
        /// Filter by type: skill | plugin | mcp
        #[arg(long, value_name = "TYPE")]
        cap_type: Option<String>,
        /// Show disabled capabilities too
        #[arg(long)]
        all: bool,
        /// Emit JSON instead of the human table
        #[arg(long)]
        json: bool,
    },
    /// Show full detail for a single capability id
    Show {
        /// Capability id, e.g. "skill:code-review"
        id: String,
    },
    /// List installed skill packs
    Packs {
        #[arg(long)]
        all: bool,
    },
    /// Register a local skill pack directory in the Hub pack registry
    PackRegister {
        /// Pack id, e.g. "waza/skills"
        id: String,
        /// Local filesystem path containing SKILL.md files and optionally tachi-pack.json
        #[arg(long, value_name = "DIR")]
        local_path: PathBuf,
        /// Display name
        #[arg(long)]
        name: Option<String>,
        /// Source URI, e.g. "local:~/.agents/skills"
        #[arg(long)]
        source: Option<String>,
        /// Version string
        #[arg(long)]
        version: Option<String>,
        /// Short description
        #[arg(long)]
        description: Option<String>,
    },
    /// Project a registered skill pack to one or more agent host directories
    PackProject {
        /// Pack id to project
        pack_id: String,
        /// Agent kind to project to. Repeat for multiple agents.
        #[arg(long = "agent", value_name = "AGENT")]
        agents: Vec<String>,
    },
    /// List virtual capability bindings
    Bindings,
    Register {
        id: String,
        #[arg(long)]
        cap_type: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        definition: String,
        #[arg(long)]
        description: Option<String>,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    /// Aggregate stats (table by default; pass --json for machine output)
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// Lightweight multi-DB schema drift scan (not `tachi doctor` v2)
    Doctor {
        #[arg(long)]
        fix: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum VaultAction {
    /// Initialize the vault with a master password.
    Init {
        /// Read password from stdin instead of prompting.
        #[arg(long)]
        stdin_password: bool,
        /// Read password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Read password confirmation from a local file (first line only).
        /// Required for non-interactive init modes except --stdin-password,
        /// where the second stdin line is used when this flag is omitted.
        #[arg(long, value_name = "PATH")]
        confirm_password_file: Option<PathBuf>,
    },
    /// Unlock the vault for this session.
    Unlock {
        /// Read password from stdin instead of prompting.
        #[arg(long)]
        stdin_password: bool,
        /// Read password from macOS Keychain (service: tachi-vault, account: default).
        #[arg(long)]
        keychain: bool,
        /// Read password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
    },
    /// Store a secret in the vault. Prompts for the value interactively
    /// unless --value-stdin is set.
    Set {
        /// Secret name (e.g. GH_TOKEN, OPENAI_API_KEY).
        name: String,
        /// Secret type (default: api_key).
        #[arg(long, default_value = "api_key")]
        secret_type: String,
        /// Optional description.
        #[arg(long)]
        description: Option<String>,
        /// Read vault password from stdin.
        #[arg(long)]
        stdin_password: bool,
        /// Read vault password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read vault password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Read the secret value from stdin (first line) instead of prompting.
        /// Useful for piping: `gh auth token | tachi vault set GH_TOKEN --value-stdin --keychain`
        #[arg(long)]
        value_stdin: bool,
    },
    /// Store multiple API keys as one logical rotation pool.
    /// Values are read from stdin, one key per line.
    #[command(alias = "pool-set")]
    SetPool {
        /// Logical provider env name (e.g. OPENAI_API_KEY, ROUTER_API_KEY).
        prefix: String,
        /// Rotation strategy.
        #[arg(long, default_value = "round_robin")]
        strategy: String,
        /// Optional description applied to each concrete key.
        #[arg(long)]
        description: Option<String>,
        /// Read vault password from stdin.
        #[arg(long)]
        stdin_password: bool,
        /// Read vault password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read vault password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Read API key values from stdin, one per line.
        #[arg(long)]
        values_stdin: bool,
    },
    /// Lease one usable API key from a Vault pool and print shell export syntax.
    Lease {
        /// Logical provider env name or standalone API key name.
        name: String,
        /// Override output env var name. Defaults to name.
        #[arg(long)]
        env_name: Option<String>,
        /// Read password from stdin.
        #[arg(long)]
        stdin_password: bool,
        /// Read password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Emit JSON instead of shell export syntax.
        #[arg(long)]
        json: bool,
    },
    /// Record a provider key result into the Vault health ledger.
    RecordKeyResult {
        /// Logical provider/env name, e.g. DEEPSEEK_API_KEY.
        logical_name: String,
        /// Concrete leased key id, e.g. DEEPSEEK_API_KEY_2.
        key_id: String,
        /// HTTP status code observed by the consumer.
        #[arg(long)]
        status_code: Option<u16>,
        /// Outcome override: success | rate_limited | auth_failed | exhausted | error.
        #[arg(long)]
        outcome: Option<String>,
        /// Retry-After seconds for 429/cooldown responses.
        #[arg(long)]
        retry_after_secs: Option<u64>,
        /// Short non-secret reason or provider error class.
        #[arg(long)]
        reason: Option<String>,
        /// Emit JSON.
        #[arg(long)]
        json: bool,
    },
    /// Get a secret value from the vault.
    Get {
        /// Secret name.
        name: String,
        /// Read password from stdin.
        #[arg(long)]
        stdin_password: bool,
        /// Read password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
    },
    /// Remove a secret from the vault.
    Remove {
        /// Secret name.
        name: String,
        /// Read password from stdin.
        #[arg(long)]
        stdin_password: bool,
        /// Read password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
    },
    /// List vault secret metadata only (names/types/descriptions; does not show values).
    List {
        /// Legacy no-op; listing metadata does not require unlocking.
        #[arg(long, hide = true)]
        stdin_password: bool,
        /// Legacy no-op; listing metadata does not require unlocking.
        #[arg(long, hide = true)]
        keychain: bool,
        /// Legacy no-op; listing metadata does not require unlocking.
        #[arg(long, value_name = "PATH", hide = true)]
        password_file: Option<PathBuf>,
    },
    /// Lock the vault (clear cached key).
    Lock,
    /// Show vault status (initialized, locked/unlocked, entry count).
    Status,
    /// Plan or apply credential profile materialization without exposing secret values.
    Materialize {
        /// Credential profile name to materialize.
        #[arg(long)]
        profile: String,
        /// Agent or dispatch-profile consumer id requesting the credential.
        #[arg(long)]
        consumer: String,
        /// JSON profile config file. Defaults to searching .tachi/credentials/*.json.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Preview only. This is the default when --apply is not passed.
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        /// Apply supported materializers. Requires vault password input.
        #[arg(long)]
        apply: bool,
        /// Allow overwriting existing file/config targets after creating a backup.
        #[arg(long)]
        allow_existing: bool,
        /// Read vault password from stdin when --apply needs decrypted values.
        #[arg(long)]
        stdin_password: bool,
        /// Read vault password from macOS Keychain when --apply needs decrypted values.
        #[arg(long)]
        keychain: bool,
        /// Read vault password from a local file when --apply needs decrypted values.
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
    },
    /// Cleanup or mark Tachi-managed credential materializations.
    Cleanup {
        /// Limit cleanup to materializations under this run directory.
        #[arg(long, value_name = "PATH")]
        run_dir: Option<PathBuf>,
        /// Limit cleanup to one credential profile.
        #[arg(long)]
        profile: Option<String>,
        /// Limit cleanup to one agent or dispatch-profile consumer.
        #[arg(long)]
        consumer: Option<String>,
        /// Preview only. This is the default when --apply is not passed.
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        /// Apply cleanup. Without this flag the command only reports candidates.
        #[arg(long)]
        apply: bool,
        /// Mark matching metadata cleaned without deleting target files.
        #[arg(long)]
        mark_only: bool,
    },
    /// Diagnose a credential profile without decrypting or writing secrets.
    Doctor {
        /// Credential profile name to diagnose.
        #[arg(long)]
        profile: String,
        /// Agent or dispatch-profile consumer id requesting the credential.
        #[arg(long)]
        consumer: String,
        /// JSON profile config file. Defaults to searching .tachi/credentials/*.json.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
    /// Export encrypted Vault rows to an iCloud-compatible sync bundle.
    SyncExport {
        /// Output bundle path. Defaults to iCloud Drive/Tachi/vault/vault.bundle.json on macOS.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Allow writing the encrypted bundle to a cloud-sync path such as iCloud Drive.
        #[arg(long)]
        allow_cloud: bool,
    },
    /// Import encrypted Vault rows from a sync bundle.
    SyncImport {
        /// Input bundle path. Defaults to iCloud Drive/Tachi/vault/vault.bundle.json on macOS.
        #[arg(long, value_name = "PATH")]
        input: Option<PathBuf>,
    },
    /// Show the default Vault sync bundle path and whether it exists.
    SyncStatus {
        /// Bundle path to inspect. Defaults to iCloud Drive/Tachi/vault/vault.bundle.json on macOS.
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
    },
}
