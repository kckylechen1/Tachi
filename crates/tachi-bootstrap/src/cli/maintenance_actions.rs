use clap::Subcommand;
use std::path::PathBuf;

pub const DEFAULT_WORKTREE_SWEEP_MAX_AGE_DAYS: u64 = 7;

/// Managed worktree lifecycle (#484 disk governor open/close).
#[derive(Subcommand, Debug, Clone)]
pub enum WorktreeAction {
    /// Open a linked git worktree under the managed cache root (not Desktop/repo).
    Open {
        /// Primary repository root (main worktree).
        #[arg(long, value_name = "PATH")]
        repo: PathBuf,
        /// Explicit worktree path. Defaults to $TACHI_WORKTREES_ROOT/<repo-slug>/...
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
        /// Branch to create (or attach if it already exists and is free).
        #[arg(long)]
        branch: Option<String>,
        /// Base ref/SHA for the new branch (default: HEAD of --repo).
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Task / issue / flow id used in generated names.
        #[arg(long)]
        task: Option<String>,
        /// Role label (executor, reviewer, ...).
        #[arg(long)]
        role: Option<String>,
        /// Optional dispatch id stored on the registry record.
        #[arg(long, value_name = "ID")]
        dispatch_id: Option<String>,
        /// Directory leaf name under the managed root.
        #[arg(long)]
        name: Option<String>,
        /// Plan only; do not create the worktree.
        #[arg(long)]
        dry_run: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Close (remove) a Tachi-managed worktree after safety checks.
    Close {
        /// Worktree path to remove.
        path: PathBuf,
        /// Actually remove. Without this flag, only prints the plan.
        #[arg(long, conflicts_with = "dry_run")]
        force: bool,
        /// Preview only (default).
        #[arg(long)]
        dry_run: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// List registered managed worktrees.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum CleanAction {
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
        #[arg(long, default_value_t = DEFAULT_WORKTREE_SWEEP_MAX_AGE_DAYS)]
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
pub enum EvalAction {
    /// Run the local /eval recall corpus and publish aggregate-only health.
    Recall {
        /// Optional JSON cases file. Defaults to labeled rows under /eval in the target DB.
        #[arg(long, value_name = "PATH")]
        cases: Option<PathBuf>,
        /// Maximum search results per case.
        #[arg(long, default_value_t = 10)]
        top_k: usize,
        /// Minimum current recall@k required for a passing run.
        #[arg(long, default_value_t = 1.0)]
        min_recall: f64,
        /// Minimum current MRR required for a passing run.
        #[arg(long, default_value_t = 0.0)]
        min_mrr: f64,
        /// Include adaptive rerank in the replay.
        #[arg(long)]
        enable_rerank: bool,
        /// Emit machine-readable JSON instead of the human summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum RepairAction {
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
pub enum QuarantineAction {
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
pub enum DaemonAction {
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
    /// Sweep stale tachi processes machine-wide: orphaned stdio servers (the
    /// launching host died) and daemons whose backing global DB no longer
    /// exists, plus stale daemon lock files. Previews by default; pass --apply
    /// to actually SIGTERM / unlink. Self and healthy live processes are never
    /// touched.
    Reap {
        /// Actually terminate / unlink. Without this it is a dry-run preview.
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum WatcherAction {
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
pub enum FoundryAction {
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
pub enum DistillAction {
    /// Run one daily batch distill pass against the project DB.
    Run {
        /// Project DB path (defaults to `--project-db` when set).
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum RescueAction {
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
pub enum ManifestAction {
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
    /// Audit centralized `~/.tachi/projects/*/memory.db` and print a relocation
    /// plan. DRY-RUN by default: classifies each project DB as
    /// {symlink-alias, symlink-broken, real-file-with-owning-repo,
    /// real-file-home-resident, uuid-smoke-test-garbage} and proposes actions
    /// WITHOUT moving or deleting anything. `--apply` is reserved for a future
    /// guarded mutation pass and currently refuses to act.
    AuditProjects {
        /// Output JSON instead of human text.
        #[arg(long)]
        json: bool,
        /// Reserved: actually perform relocations/GC. Currently refuses; the
        /// audit is plan-only by design. (Takes a backup and refuses on
        /// ambiguity once implemented.)
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum WikiAction {
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
pub enum HarnessAction {
    /// Read-only inventory of host AgentMD / workflow files managed by Tachi
    Status {
        /// Limit the scan to one or more hosts. Repeat or comma-separate values.
        #[arg(long = "host", value_delimiter = ',')]
        hosts: Vec<String>,
        /// Override home directory for testing or dry-run inventory
        #[arg(long, value_name = "PATH")]
        home: Option<PathBuf>,
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum SkillSurfaceAction {
    /// Read-only inventory of local skill stores, hashes, symlinks, and host projections
    Status {
        /// Limit host projection checks. Repeat or comma-separate values.
        #[arg(long = "host", value_delimiter = ',')]
        hosts: Vec<String>,
        /// Override home directory for testing or dry-run inventory
        #[arg(long, value_name = "PATH")]
        home: Option<PathBuf>,
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
    },
    /// Read-only pinned upstream source status for builtin Superpowers and Waza skills
    Sources {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
    },
    /// Read-only reviewed-sync plan for upstream Superpowers and Waza changes
    SyncPlan {
        /// Emit machine-readable JSON instead of the human summary
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum CardAction {
    /// List Cards available to the current project/runtime.
    List {
        /// Emit machine-readable JSON instead of the human table.
        #[arg(long)]
        json: bool,
    },
    /// Show one Card by dispatch profile id.
    Show {
        /// Card/profile id, e.g. codex_55_review.
        id: String,
        /// Emit machine-readable JSON instead of the human summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum PokeAction {
    /// Run a local Poke probe suite.
    Run {
        /// Probe suite to run. Currently only "smoke" is supported.
        #[arg(long, default_value = "smoke")]
        suite: String,
        /// Emit machine-readable JSON instead of the human summary.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum HubAction {
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
pub enum McpAction {
    /// Register an upstream MCP server in the Tachi Hub.
    Add {
        /// Human name for the MCP server. Stored as capability id mcp:<normalized-name>.
        name: String,
        /// Remote URL for http/sse transports, or command path for stdio.
        url: String,
        /// Upstream transport.
        #[arg(long, value_parser = ["http", "sse", "stdio"], default_value = "http")]
        transport: String,
        /// Header in "Name: Value" form. Use ${vault:KEY} placeholders for secrets.
        #[arg(long = "header", value_name = "NAME: VALUE")]
        headers: Vec<String>,
        /// Store this API key in Vault and reference it from the MCP definition.
        #[arg(long, value_name = "VALUE")]
        key: Option<String>,
        /// Read vault password from stdin when --key is supplied.
        #[arg(long)]
        stdin_password: bool,
        /// Read vault password from macOS Keychain when --key is supplied.
        #[arg(long)]
        keychain: bool,
        /// Read vault password from a local file when --key is supplied.
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Allow password files readable by group/other when --key is supplied.
        #[arg(long)]
        insecure_password_file: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}
