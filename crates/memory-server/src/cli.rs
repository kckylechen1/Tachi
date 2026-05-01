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

    /// Enable clawdoctor: periodic OpenClaw health check + auto-restart
    /// (overrides CLAWDOCTOR_ENABLED)
    #[arg(long)]
    pub clawdoctor: Option<bool>,

    /// OpenClaw gateway URL for clawdoctor health checks
    /// (overrides CLAWDOCTOR_URL, default: http://127.0.0.1:18789)
    #[arg(long)]
    pub clawdoctor_url: Option<String>,

    /// Clawdoctor check interval in seconds (overrides CLAWDOCTOR_INTERVAL_SECS, default: 300)
    #[arg(long)]
    pub clawdoctor_interval_secs: Option<u64>,

    /// Consecutive failures before clawdoctor triggers a restart
    /// (overrides CLAWDOCTOR_FAIL_THRESHOLD, default: 3)
    #[arg(long)]
    pub clawdoctor_fail_threshold: Option<u32>,

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
    },
    /// Save a memory. Alias of `remember`.
    ///
    /// Historically `save` only accepted `--path` / `--importance`. We now
    /// route it through the same `remember`-backed handler so the full flag
    /// set (tags, scope, project, category, topic, domain, retention-policy,
    /// summary, force) is available on both verbs and behavior is identical
    /// (daemon-forwarding when a hub is up, in-process otherwise). The legacy
    /// 3-flag invocation `tachi save TEXT --path X --importance Y` keeps
    /// working unchanged because clap accepts the alias and all extra flags
    /// are optional.
    // Variant intentionally elided — see `Remember` below, which carries
    // `#[command(alias = "save")]`.
    /// Show database statistics
    Stats,
    /// Inspect onboarding readiness and current local Tachi setup
    Setup {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
    },
    /// Scan for fragmented memory databases and report consolidation candidates
    Tidy {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
        /// Execute the conservative apply path for clearly safe actions
        #[arg(long)]
        apply: bool,
    },
    /// Doctor v2 — extension-aware DB classification + safe auto-fix
    Doctor {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
        /// Skip the safe auto-fix pass (placeholder quarantine + WAL copy-aside)
        #[arg(long)]
        scan_only: bool,
        /// Override default scan roots (~/.tachi, ~/.openclaw, ~/.sigil, ~/.gemini/antigravity)
        #[arg(long, value_name = "PATH")]
        roots: Vec<PathBuf>,
        /// Branch #5: also report a foundry job-status histogram per manifest DB
        #[arg(long)]
        jobs: bool,
    },
    /// Manifest v1 — show/init/refresh ~/.tachi/manifest.json
    Manifest {
        #[command(subcommand)]
        action: ManifestAction,
    },
    /// Run garbage collection
    Gc,
    /// Hub management
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
    },
    /// Inspect or terminate the running tachi daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
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
pub(crate) enum HubAction {
    List {
        #[arg(long)]
        cap_type: Option<String>,
    },
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
    Stats,
}
