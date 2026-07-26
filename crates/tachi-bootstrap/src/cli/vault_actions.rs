use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand, Debug, Clone)]
pub enum EnvAction {
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
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        /// Write the generated exports file. Without this flag, sync is preview-only.
        #[arg(long)]
        apply: bool,
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
pub enum VaultIntakeAction {
    /// Discover local credential candidates without unlocking or writing Vault.
    Discover {
        /// Source host to scan: env or codex. Other hosts are reported as unsupported for this slice.
        #[arg(long)]
        host: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Plan intake actions for discovered candidates. Read-only: never unlocks the
    /// Vault, never reads a secret value, and writes nothing unless --write is set.
    Plan {
        /// Source host to scan: env or codex. Other hosts are reported as unsupported for this slice.
        #[arg(long)]
        host: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Write a redacted plan artifact to <cwd>/.tachi/intake-plan.json.
        /// Default is stdout only (fully read-only).
        #[arg(long)]
        write: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum VaultAction {
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
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
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
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
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
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
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
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
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
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
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
        /// Print the decrypted secret value. Without this flag, get only confirms the entry exists.
        #[arg(long)]
        reveal: bool,
        /// Emit JSON. Without --reveal the value is redacted.
        #[arg(long)]
        json: bool,
        /// Read password from stdin.
        #[arg(long)]
        stdin_password: bool,
        /// Read password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
    },
    /// Run a command with Vault-backed environment variables injected.
    Exec {
        /// Read password from stdin.
        #[arg(long)]
        stdin_password: bool,
        /// Read password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
        /// Optional Vault consumer identity for project-bound secret ACL checks.
        #[arg(long, value_name = "ID")]
        consumer: Option<String>,
        /// Comma-separated environment variable names that must be present before spawning.
        #[arg(long, value_name = "NAME,NAME", value_delimiter = ',')]
        require: Vec<String>,
        /// Allow spawning the child with the inherited environment when the Vault
        /// cannot be unlocked (or its environment cannot be loaded) AND no
        /// `--require` names were set. Without this flag (the default) `vault
        /// exec` refuses to spawn a credential-less child and exits nonzero
        /// before exec — so a caller that only checks exit status never silently
        /// gets a Vault-less run (#1413 concern 4). `--require` always wins: a
        /// required name that cannot be satisfied fails before spawn regardless
        /// of this flag.
        #[arg(long)]
        allow_unauthenticated: bool,
        /// Command and arguments to execute. Use `--` before the command.
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
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
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
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
        /// Legacy no-op; kept for CLI compatibility.
        #[arg(long, hide = true)]
        insecure_password_file: bool,
    },
    /// Lock the vault (clear cached key).
    Lock,
    /// Show vault status (initialized, locked/unlocked, entry count).
    Status,
    /// Discover local credential candidates without decrypting or writing secrets.
    Intake {
        #[command(subcommand)]
        action: VaultIntakeAction,
    },
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
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
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
    /// Diagnose a credential profile or provider custody without writing.
    ///
    /// With `--providers`, report OpenCode provider apiKey shapes and Vault
    /// metadata. Value comparison is opt-in through an explicit password
    /// source; without one, comparison remains UNKNOWN.
    Doctor {
        /// Report-only OpenCode providers and daemon effective-source view.
        #[arg(long)]
        providers: bool,
        /// OpenCode config path. Defaults to ~/.config/opencode/opencode.json.
        /// Env `TACHI_OPENCODE_CONFIG` is also honored (CLI flag wins when both set).
        #[arg(long, value_name = "PATH")]
        opencode_config: Option<PathBuf>,
        /// Credential profile name to diagnose.
        #[arg(long, required_unless_present = "providers")]
        profile: Option<String>,
        /// Agent or dispatch-profile consumer id requesting the credential.
        #[arg(long, required_unless_present = "providers")]
        consumer: Option<String>,
        /// JSON profile config file. Defaults to searching .tachi/credentials/*.json.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Read Vault password from stdin for report-only value comparison.
        #[arg(
            long,
            requires = "providers",
            conflicts_with_all = ["keychain", "password_file"]
        )]
        stdin_password: bool,
        /// Read Vault password from macOS Keychain for report-only value comparison.
        #[arg(
            long,
            requires = "providers",
            conflicts_with_all = ["stdin_password", "password_file"]
        )]
        keychain: bool,
        /// Read Vault password from a local file for report-only value comparison.
        #[arg(
            long,
            value_name = "PATH",
            requires = "providers",
            conflicts_with_all = ["stdin_password", "keychain"]
        )]
        password_file: Option<PathBuf>,
        /// Allow a provider-doctor password file readable by group/other.
        #[arg(long, requires = "password_file")]
        insecure_password_file: bool,
    },
    /// Export encrypted Vault rows to a signed sync bundle.
    ///
    /// If this file is stolen, its verifier/ciphertext material permits offline
    /// password guessing. Keep it local, use a strong Vault password, and treat
    /// --allow-cloud as an explicit risk acceptance. Use --entries-only to omit
    /// the verifier (reduces but does not eliminate the guessing surface).
    SyncExport {
        /// Output bundle path. Defaults to ~/.tachi/sync/vault/vault.bundle.json.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Allow writing the bundle to a cloud-sync path such as iCloud Drive,
        /// accepting offline password guessing risk if the synced file leaks.
        #[arg(long)]
        allow_cloud: bool,
        /// Omit vault_config (salt + verifier) from the bundle. The import side
        /// must already have a matching vault initialized. Reduces the offline
        /// guessing surface but does not eliminate it (entry AEAD ciphertext
        /// remains a verification oracle). Recommended for cloud-synced bundles.
        #[arg(long)]
        entries_only: bool,
        /// Read Vault password from stdin.
        #[arg(long)]
        stdin_password: bool,
        /// Read Vault password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read Vault password from the first line of a file.
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
    },
    /// Import encrypted Vault rows from a sync bundle.
    ///
    /// Possession of a signed or legacy unsigned bundle can permit offline
    /// password guessing against the Vault password; import only from trusted
    /// storage and prefer locally kept bundles.
    SyncImport {
        /// Input bundle path. Defaults to ~/.tachi/sync/vault/vault.bundle.json.
        #[arg(long, value_name = "PATH")]
        input: Option<PathBuf>,
        /// Import a legacy unsigned bundle. Signed bundles are still verified.
        #[arg(long)]
        allow_unsigned: bool,
        /// Read Vault password from stdin.
        #[arg(long)]
        stdin_password: bool,
        /// Read Vault password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read Vault password from the first line of a file.
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
    },
    /// Show the default Vault sync bundle path and whether it exists.
    SyncStatus {
        /// Bundle path to inspect. Defaults to ~/.tachi/sync/vault/vault.bundle.json.
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Bulk-enter the canonical provider API keys into the encrypted vault.
    /// Iterates the known provider keys, prompts for each (blank skips), and
    /// upserts non-empty values encrypted. Initializes the vault first if needed.
    SetupKeys {
        /// Read the vault password from stdin (first line) instead of prompting.
        #[arg(long)]
        stdin_password: bool,
        /// Read the vault password from macOS Keychain.
        #[arg(long)]
        keychain: bool,
        /// Read the vault password from a local file (first line only).
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,
        /// Read the vault password confirmation from a local file (init only).
        #[arg(long, value_name = "PATH")]
        confirm_password_file: Option<PathBuf>,
        /// Allow password files readable by group/other (insecure).
        #[arg(long)]
        insecure_password_file: bool,
        /// Include keys flagged deprecated in the canonical list.
        #[arg(long)]
        include_deprecated: bool,
    },
}
