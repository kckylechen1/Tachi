use clap::{Args, Subcommand};
use std::path::PathBuf;

pub const DEFAULT_WORKTREE_SWEEP_MAX_AGE_DAYS: u64 = 7;

/// Staleness gate for the orphan build-artifact reaper (#894 S2b). Same 7 days
/// as the worktree sweep: a build target nothing has touched in a week is not
/// part of a live build.
pub const DEFAULT_ORPHAN_REAP_MAX_AGE_DAYS: u64 = 7;

/// Machine-local execution profile. This is separate from agent dispatch
/// profiles: it controls the highest side-effect level allowed on this host.
#[derive(Subcommand, Debug, Clone)]
pub enum HostAction {
    /// Show the active host profile and its maximum execution level.
    Show {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Persist a host profile in TACHI_HOME/config.env for future processes.
    Set {
        /// One of: development, home_data, release.
        profile: String,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

/// The build broker (#894 S2c): this machine runs exactly ONE cargo at a time,
/// in one fixed executor-seat checkout, and never against a target dir whose
/// generation has diverged from the source being built.
#[derive(Subcommand, Debug, Clone)]
pub enum BuildAction {
    /// Submit an immutable build ticket for the serialized executor seat.
    Submit {
        /// Repository root (the repo identity the ticket is scoped to).
        #[arg(long, value_name = "PATH")]
        repo: PathBuf,
        /// Commit to build. A ref (branch/tag/HEAD) is resolved to its object id
        /// here; the ticket itself only ever carries the object id.
        #[arg(long, value_name = "REF", default_value = "HEAD")]
        head: String,
        /// Base the source claims to branch from (default: same as --head).
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// The exec_env lease this build's result is attributed to.
        #[arg(long, value_name = "ID")]
        env_id: Option<String>,
        /// The dispatch this build belongs to.
        #[arg(long, value_name = "ID")]
        dispatch_id: Option<String>,
        /// Explicit ticket id (default: a fresh uuid). Tickets are immutable —
        /// reusing an id with a different payload is refused.
        #[arg(long, value_name = "ID")]
        ticket_id: Option<String>,
        /// The command to run in the seat, e.g. `-- cargo test -p memcore`.
        /// Defaults to `cargo build --workspace`.
        #[arg(last = true, value_name = "CMD")]
        command: Vec<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Drain the ticket queue through the single executor seat (FIFO, strictly
    /// serialized, and only tickets for THIS repo). Exits immediately if
    /// another build holds the slot.
    Run {
        /// Repository root (used to resolve the seat, filter the queue, and
        /// answer lineage questions).
        #[arg(long, value_name = "PATH")]
        repo: PathBuf,
        /// Run at most this many tickets (default: drain the queue).
        #[arg(long, value_name = "N")]
        max: Option<usize>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show the executor slot holder, this repo's pending queue, its dead
    /// letters, and each target dir's generation.
    Status {
        #[arg(long, value_name = "PATH")]
        repo: PathBuf,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Cancel a queued ticket: it leaves the queue for good (terminal state) and
    /// the executor never picks it up. Refused for a ticket that already has a
    /// receipt, or one whose build is currently holding the executor slot (that
    /// is `build abandon`'s job, and only once its process is actually dead).
    Cancel {
        /// The ticket to cancel.
        #[arg(long, value_name = "ID")]
        ticket_id: String,
        /// Why (recorded on the terminal record).
        #[arg(long, value_name = "REASON", default_value = "cancelled by operator")]
        reason: String,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Crash recovery: the slot is held by a build that is no longer running.
    /// Quarantines that build's target dir FIRST, then frees the slot. Never
    /// run this while the holder's cargo is actually alive.
    Abandon {
        /// Why the slot is being forced open (recorded on the quarantine).
        #[arg(long, value_name = "REASON")]
        reason: String,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Flags for `tachi worktree open`.
///
/// A struct rather than inline variant fields because this one variant carries
/// ~260 bytes of options while its siblings carry ~30: as inline fields it made
/// every `WorktreeAction` value (including a bare `List { json }`) pay for the
/// biggest one, which is `clippy::large_enum_variant`. The variant holds it
/// boxed, so the enum is pointer-sized again.
#[derive(Args, Debug, Clone)]
pub struct WorktreeOpenArgs {
    /// Primary repository root (main worktree).
    #[arg(long, value_name = "PATH")]
    pub repo: PathBuf,
    /// Explicit worktree path. Defaults to $TACHI_WORKTREES_ROOT/<repo-slug>/...
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Branch to create (or attach if it already exists and is free).
    #[arg(long)]
    pub branch: Option<String>,
    /// Base ref/SHA for the new branch (default: HEAD of --repo).
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    /// Task / issue / flow id used in generated names.
    #[arg(long)]
    pub task: Option<String>,
    /// Role label (executor, reviewer, ...).
    #[arg(long)]
    pub role: Option<String>,
    /// Optional dispatch id stored on the registry record.
    #[arg(long, value_name = "ID")]
    pub dispatch_id: Option<String>,
    /// Directory leaf name under the managed root.
    #[arg(long)]
    pub name: Option<String>,
    /// Provisioning class (#894 S2c): edit-only (default; no build target
    /// dir — builds go through the build broker) | build-ticketed (submits
    /// tickets to the machine-unique serialized executor seat; still gets no
    /// target dir of its own) | build-private (rare: a private target dir;
    /// requires --approve-private-target).
    #[arg(long, value_name = "CLASS", default_value = "edit-only")]
    pub env_class: String,
    /// Approval token for --env-class build-private (who approved the disk
    /// reservation). Refused without it.
    #[arg(long, value_name = "APPROVER")]
    pub approve_private_target: Option<String>,
    /// Disk to reserve, in bytes, for a build-private target dir.
    #[arg(long, value_name = "BYTES")]
    pub reserve_bytes: Option<i64>,
    /// Plan only; do not create the worktree.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

/// Managed worktree lifecycle (#484 disk governor open/close).
#[derive(Subcommand, Debug, Clone)]
pub enum WorktreeAction {
    /// Open a linked git worktree under the managed cache root (not Desktop/repo).
    Open(Box<WorktreeOpenArgs>),
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
    /// Reap orphaned build artifacts (dead cargo targets / cargo homes) that no
    /// process holds and no lease binds, booking every freed byte in the
    /// exec_env resource ledger (#894 S2b).
    Orphans {
        /// Root to scan. Repeatable; defaults to TMPDIR, /private/tmp, temp_dir
        /// and ~/.cache.
        #[arg(long, value_name = "PATH")]
        root: Vec<PathBuf>,
        /// Minimum age in days before an artifact is considered orphaned.
        #[arg(long, default_value_t = DEFAULT_ORPHAN_REAP_MAX_AGE_DAYS)]
        max_age_days: u64,
        /// Actually delete eligible artifacts. Without this flag, only prints
        /// the plan (and writes nothing to the ledger).
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
pub enum DedupeAction {
    /// Plan exact same-path byte-identical duplicates without writing the DB.
    Exact {
        #[arg(long, value_name = "LABEL")]
        db: String,
        #[arg(long)]
        output: std::path::PathBuf,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        path_prefix: Option<String>,
    },
    /// Apply a saved exact-dedupe plan atomically. Writes a durable receipt
    /// (audit + restore input) to `--receipt-out` before reporting success.
    Apply {
        #[arg(long, value_name = "LABEL")]
        db: String,
        #[arg(long)]
        plan: std::path::PathBuf,
        #[arg(long)]
        yes: bool,
        #[arg(long, value_name = "FILE")]
        receipt_out: std::path::PathBuf,
    },
    /// Restore every loser archived by one exact-dedupe apply receipt.
    Restore {
        #[arg(long, value_name = "LABEL")]
        db: String,
        #[arg(long)]
        receipt: std::path::PathBuf,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum LifecycleConsistencyAction {
    /// Plan safe lifecycle-state normalization and report ambiguous rows.
    Plan {
        #[arg(long, value_name = "LABEL")]
        db: String,
        #[arg(long)]
        output: std::path::PathBuf,
        /// Emit the complete machine-readable plan on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Apply a saved lifecycle-consistency plan atomically.
    Apply {
        #[arg(long, value_name = "LABEL")]
        db: String,
        #[arg(long)]
        plan: std::path::PathBuf,
        #[arg(long)]
        yes: bool,
        #[arg(long, value_name = "FILE")]
        receipt_out: std::path::PathBuf,
    },
    /// Restore every lifecycle mutation recorded by one apply receipt.
    Restore {
        #[arg(long, value_name = "LABEL")]
        db: String,
        #[arg(long)]
        receipt: std::path::PathBuf,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum RepairAction {
    /// Plan or apply deterministic duplicate repair.
    Dedupe {
        #[command(subcommand)]
        action: DedupeAction,
    },
    /// Plan, apply, or restore lifecycle-state consistency repair.
    Lifecycle {
        #[command(subcommand)]
        action: LifecycleConsistencyAction,
    },
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
    /// Plan a fail-closed archive sweep of expired auto-capture rows.
    CaptureArchivePlan {
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        #[arg(long, value_name = "RFC3339")]
        as_of: Option<String>,
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Apply a previously emitted capture archive plan.
    CaptureArchiveApply {
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        #[arg(long, value_name = "FILE")]
        plan: PathBuf,
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Restore rows archived by one capture archive receipt.
    CaptureArchiveRestore {
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        #[arg(long, value_name = "FILE")]
        receipt: PathBuf,
        #[arg(long, required = true)]
        confirm: bool,
    },
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
        /// #1043 D3 escape hatch: skip the pre-selection consolidate pass
        /// (byte-identical duplicate collapse) and select candidates
        /// straight from the raw, undeduplicated pool — pre-#1043 behavior.
        #[arg(long)]
        no_consolidate: bool,
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
    /// Audit the project, shared Wiki, and legacy global Wiki corpus.
    ///
    /// This is a read-only JSON preview unless `--apply` is supplied together
    /// with the exact confirmation token and an existing backup directory.
    Corpus {
        /// Apply the precomputed, backup-gated migration plan.
        #[arg(long)]
        apply: bool,
        /// Exact confirmation token required for apply.
        #[arg(long, value_name = "TOKEN")]
        confirm: Option<String>,
        /// Existing directory in which deterministic SQLite backups are stored.
        #[arg(long, value_name = "DIR")]
        backup_dir: Option<PathBuf>,
        /// Optional JSON preview plan to apply; source revisions are checked.
        #[arg(long, value_name = "PATH")]
        plan: Option<PathBuf>,
        /// Repair rows a sibling-worker race left archived and non-canonical.
        ///
        /// Its own mode: without `--confirm` it only reports the exact damage
        /// signature it found. It cannot be combined with `--apply`/`--plan`.
        #[arg(long)]
        repair_sibling_damage: bool,
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

/// Cross-harness injection-surface doctor (#1307): report-only inventory of
/// MCP / plugin / credential / environment / density planes. Never writes,
/// never chmods, never uninstalls, never prints credential values.
#[derive(Subcommand, Debug, Clone)]
pub enum InjectionSurfaceAction {
    /// Report-only doctor over a local fleet registry of harness injection planes
    Doctor {
        /// Path to the fleet registry JSON (local to this doctor; not #871)
        #[arg(long, value_name = "PATH")]
        registry: PathBuf,
        /// Optional root for resolving relative plane paths in fixtures
        #[arg(long, value_name = "PATH")]
        home: Option<PathBuf>,
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

/// `tachi cards` (plural) — dispatch-ledger LANE card ingest (tachi#1202
/// Phase-1 / tachi#992). Deliberately a separate enum from `CardAction`
/// above: that one projects Tachikoma dispatch-profile cards, this one
/// mirrors `~/.agents/dispatch-ledger/cards/*.md` (leader-authored
/// model/vendor playbooks) into GLOBAL-db `/cards/<seat>` rows.
#[derive(Subcommand, Debug, Clone)]
pub enum CardsAction {
    /// Produce a deterministic, card-write-free draft artifact from JSON.
    Draft {
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
    },
    /// Produce an independent review receipt from JSON.
    Review {
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
    },
    /// Bind accepted draft/review JSON to the current canonical card revision.
    Approve {
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
    },
    /// Apply approved bytes using a fresh evidence-state JSON bundle.
    Apply {
        #[arg(long, value_name = "FILE")]
        approval: PathBuf,
        #[arg(long, value_name = "FILE")]
        evidence: PathBuf,
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
    },
    /// Read every `*.md` lane card under `--dir` (default:
    /// `~/.agents/dispatch-ledger/cards`) and create/update/no-op/archive its
    /// `/cards/<seat>` mirror row accordingly. Writes go through the same
    /// daemon-forward-else-in-process channel as `tachi remember`. Never
    /// deletes: a mirror row whose source file disappeared is archived, not
    /// removed.
    Sync {
        /// Override the source directory of `*.md` lane cards.
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
        /// Emit machine-readable JSON instead of the human summary table.
        #[arg(long)]
        json: bool,
    },
    /// List current `/cards/<seat>` mirror rows (read-only; no filesystem access).
    List {
        /// Emit machine-readable JSON instead of the human table.
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
