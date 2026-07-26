use super::{
    BuildAction, CardAction, CardsAction, CleanAction, DaemonAction, DistillAction, EnvAction,
    EvalAction, FoundryAction, HarnessAction, HostAction, HubAction, InjectionSurfaceAction,
    ManifestAction, McpAction, PokeAction, RepairAction, RescueAction, SkillSurfaceAction,
    VaultAction, WatcherAction, WikiAction, WorktreeAction,
};
use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
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
        /// Override the migration target DB (defaults to ~/.tachi/global/tachi-memory.db)
        #[arg(long, value_name = "PATH")]
        target_db: Option<PathBuf>,
    },
    /// Clean build artifacts, Tachi-managed worktrees, and Tachi runtime leftovers.
    /// Dry-run by default; pass --force on a subcommand to delete.
    Clean {
        #[command(subcommand)]
        action: CleanAction,
    },
    /// Managed worktree lifecycle: open under ~/.cache/tachi/worktrees, close, list (#484).
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },
    /// Build broker (#894 S2c): submit immutable build tickets and drain them
    /// through this machine's single serialized executor seat.
    Build {
        #[command(subcommand)]
        action: BuildAction,
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
        /// Run the daily pipeline remediation: distill batch + provider key probe
        /// refresh. Unifies the two stale-marker warnings into one verb.
        #[arg(long)]
        run_daily: bool,
    },
    /// Manifest v1 — show/init/refresh ~/.tachi/manifest.json
    Manifest {
        #[command(subcommand)]
        action: ManifestAction,
    },
    /// Enumerate every known memory DB (global, manifest-addressed named
    /// projects resolved to their real backing file, and the current
    /// workspace's `.tachi/tachi-memory.db`) and report each one's schema-version
    /// gap against this binary's `EXPECTED_SCHEMA_VERSION` (kckylechen1/tachi#1223).
    ///
    /// Plan-only (read-only, zero writes) by default. `--apply` authorizes
    /// an in-process migration of exactly the libraries that are behind,
    /// via a typed `MigrationAuthority::Allow` scoped to this invocation
    /// (#1119's call-site-authority pattern — never a process env var, and
    /// never the shared top-level `--allow-schema-migration` flag). A
    /// library currently held by a live daemon is skipped and listed, not
    /// aborted; the rest of the sweep continues.
    Migrate {
        /// Emit machine-readable JSON instead of the human summary table
        #[arg(long)]
        json: bool,
        /// Actually migrate every library reported behind. Default is
        /// plan-only: report the gap, make zero writes.
        #[arg(long)]
        apply: bool,
    },
    /// Run garbage collection
    Gc,
    /// Hub registry (list/show/bindings/stats/doctor) and capability management
    Hub {
        #[command(subcommand)]
        action: HubAction,
    },
    /// Register upstream MCP servers in the Tachi Hub.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Inspect host instruction files projected from Tachi harness guidance
    Harness {
        #[command(subcommand)]
        action: HarnessAction,
    },
    /// Show or set this machine's local execution profile.
    Host {
        #[command(subcommand)]
        action: HostAction,
    },
    /// Inspect local skill stores and host-specific skill projections
    SkillSurface {
        #[command(subcommand)]
        action: SkillSurfaceAction,
    },
    /// Cross-harness injection-surface doctor (MCP/plugin/credential/env planes)
    InjectionSurface {
        #[command(subcommand)]
        action: InjectionSurfaceAction,
    },
    /// Inspect Tachikoma Cards projected from dispatch profiles.
    Card {
        #[command(subcommand)]
        action: CardAction,
    },
    /// Sync/list dispatch-ledger LANE cards (`~/.agents/dispatch-ledger/cards/*.md`,
    /// leader-authored model/vendor playbooks — see `tachi#1202`) as read-only
    /// `/cards/<seat>` mirror rows in the GLOBAL memory DB. Distinct from the
    /// singular `Card` command above, which projects Tachikoma dispatch-profile
    /// cards (`tachi_task(action='profiles')`) and is unrelated data.
    Cards {
        #[command(subcommand)]
        action: CardsAction,
    },
    /// Run Poke product-probe smoke suites against isolated local Tachi surfaces.
    Poke {
        #[command(subcommand)]
        action: PokeAction,
    },
    /// Local eval corpus automation.
    Eval {
        #[command(subcommand)]
        action: EvalAction,
    },
    /// Backfill missing vector embeddings using Voyage API
    BackfillVectors {
        /// Target DB path (defaults to global DB)
        #[arg(long, value_name = "PATH", conflicts_with_all = ["project", "all_projects"])]
        db: Option<PathBuf>,
        /// Target named project DB under ~/.tachi/projects/<name>/tachi-memory.db
        #[arg(long, conflicts_with = "all_projects")]
        project: Option<String>,
        /// Run against every manifest-owned writable Tachi DB.
        #[arg(long)]
        all_projects: bool,
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
    /// Backfill missing recall keywords using the configured extract LLM
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
    /// Run batch memory distill through the configured API lane. The legacy
    /// backend selector remains accepted but never launches Claude CLI.
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
    /// `--project` binds the (one-time) daemon session to that project at
    /// initialize, so the write lands in that project's DB — it is not a
    /// project param on an otherwise-unbound session.
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
        /// Optional named project DB target (e.g. "hyperion", "wiki"). When
        /// forwarding to a daemon, this binds that one-time session to the
        /// named project at initialize (X-Tachi-Project) rather than sending
        /// an unbound project= arg.
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
        /// Optional domain (e.g. "domain-pack", "code-review").
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
    /// `--project` binds the (one-time) daemon session to that project at
    /// initialize, so the write lands in that project's DB — it is not a
    /// project param on an otherwise-unbound session.
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
        /// Optional named project DB target (e.g. "hyperion", "wiki"). When
        /// forwarding to a daemon, this binds that one-time session to the
        /// named project at initialize (X-Tachi-Project) rather than sending
        /// an unbound project= arg.
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
        /// Probe every manifest DB (the full fleet), not just the global DB
        /// and the current project DB. This is the pre-#coldpath-perf-pack
        /// behavior; it opens and queries every registered DB read-only
        /// (job histogram, vector health, namespace counts, continuity
        /// metrics) and is measurably slower on hosts with many registered
        /// DBs. Omit for the fast default (global + current-project only).
        #[arg(long)]
        all_dbs: bool,
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
    /// VACUUM, orphan reference cleanup, opt-in memory hygiene). Default is
    /// dry-run; pass --apply to actually mutate. Per-DB backup auto-taken
    /// before any mutation.
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
        /// Allow password files readable by group/other (insecure).
        #[arg(long, global = true)]
        insecure_password_file: bool,
    },
}
