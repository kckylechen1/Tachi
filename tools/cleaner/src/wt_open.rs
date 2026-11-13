//! Managed worktree open (#484 / #894 slice).
//!
//! Tachi-owned entrypoint for creating linked git worktrees:
//! - Default root: `$TACHI_WORKTREES_ROOT` or `~/.cache/tachi/worktrees/<repo-slug>/`
//! - Refuses Desktop / iCloud / inside-primary-repo placement (TCC + residue)
//! - Registers via `wt-register` (marker + `~/.tachi/worktrees.json`)

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::registry::{self, RegisterOptions, RegisterOutputFormat};
use crate::wt_clean::OutputFormat;

#[derive(Debug)]
pub struct OpenOptions {
    pub repo_root: PathBuf,
    /// Explicit worktree path. When omitted, planned under the managed root.
    pub path: Option<PathBuf>,
    /// Branch to create or attach. Generated when omitted.
    pub branch: Option<String>,
    /// Base ref/SHA (default: `HEAD` of the primary checkout).
    pub base: Option<String>,
    pub task: Option<String>,
    pub role: Option<String>,
    pub dispatch_id: Option<String>,
    /// Directory leaf name under the managed root (generated when omitted).
    pub name: Option<String>,
    /// What build target this worktree is wired to (#894 S2c). The env class
    /// decides this; `Unallocated` is the `edit-only` default.
    pub cargo_target: CargoTargetPolicy,
    pub dry_run: bool,
    pub output: OutputFormat,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OpenReport {
    pub action: &'static str,
    pub dry_run: bool,
    pub opened: bool,
    pub registered: bool,
    pub repo_root: String,
    pub path: String,
    pub branch: String,
    pub base_ref: String,
    pub base_sha: String,
    pub managed_root: String,
    pub marker_path: Option<String>,
    #[serde(skip)]
    pub worktree_authority: Option<memcore::anchored_fs::AnchoredDirectory>,
    /// Shared `CARGO_TARGET_DIR` written to `<worktree>/.cargo/config.toml`
    /// (#484 slice 2), when this is a Rust repo and provisioning happened.
    /// `None` when the worktree is not a Rust repo (no root `Cargo.toml`) or
    /// provisioning was skipped/failed (see `warnings`).
    pub cargo_target_dir: Option<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

/// Default managed worktrees root (not under Desktop / primary repo).
///
/// Fails cleanly (`Err`) rather than falling back to the current directory
/// when neither `TACHI_WORKTREES_ROOT` nor `HOME`/`USERPROFILE` is set: a
/// cwd fallback here would make the managed root (and therefore the sweep
/// GC root and the path-boundary fence) silently resolve to wherever the
/// caller happens to be running from.
///
/// A *relative* `TACHI_WORKTREES_ROOT` is rejected outright (CP3) rather
/// than resolved against some implicit base: the managed root doubles as
/// the sweep GC root and the path-boundary fence, and a relative value
/// would make both silently float with whatever cwd the caller happens to
/// be running from — the same failure mode the cwd-fallback comment above
/// already refuses. There's no non-arbitrary base to resolve a relative
/// root against, so refusing is the unambiguous fix.
pub fn default_worktrees_root() -> Result<PathBuf, String> {
    if let Some(raw) = std::env::var_os("TACHI_WORKTREES_ROOT") {
        if !raw.is_empty() {
            let path = PathBuf::from(&raw);
            if !path.is_absolute() {
                return Err(format!(
                    "TACHI_WORKTREES_ROOT must be an absolute path, got relative path \
                     '{}': a relative managed root would resolve against the current \
                     working directory, letting the GC/fence root float with cwd",
                    path.display()
                ));
            }
            warn_if_ephemeral_root(&path, "TACHI_WORKTREES_ROOT");
            return Ok(path);
        }
    }
    match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)
            .join(".cache")
            .join("tachi")
            .join("worktrees")),
        _ => Err(
            "cannot determine managed worktrees root: HOME (and USERPROFILE) is unset and \
             TACHI_WORKTREES_ROOT is not set; refusing to fall back to the current directory"
                .to_string(),
        ),
    }
}

/// Known-ephemeral scratch roots (tachi#1184 item 3 — the worktree-location
/// law): a worktree meant to outlive a session belongs under a durable cache
/// dir (`~/.cache/tachi/worktrees/…`, the default above), never one of
/// these. `--path`/explicit-path escapes of the managed root are already a
/// HARD refusal ([`path_outside_managed_root_reason`]) — this list exists
/// for the one path that check does not cover: an operator pointing
/// `TACHI_WORKTREES_ROOT` itself at an ephemeral volume, which makes that
/// volume the "managed root" and therefore compliant by that check's own
/// definition. A hard fail here would be the wrong shape for a
/// self-configured root (a deliberate throwaway/test root is a legitimate
/// use), so this is a loud warning, not a refusal.
fn ephemeral_root_prefixes() -> Vec<PathBuf> {
    let mut prefixes = vec![PathBuf::from("/tmp"), PathBuf::from("/private/tmp")];
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        if !tmpdir.is_empty() {
            prefixes.push(PathBuf::from(tmpdir));
        }
    }
    prefixes
}

/// Does `path` sit at or under one of `ephemeral_roots`? Two passes, neither
/// one fatal:
///
/// 1. **Literal component comparison** first — `path` commonly does not
///    exist yet (nobody has provisioned into this root), so `/tmp` /
///    `/private/tmp` are both checked literally by
///    [`ephemeral_root_prefixes`] specifically so the well-known macOS
///    `/tmp` -> `/private/tmp` symlink is covered without needing anything
///    to exist on disk first.
/// 2. **Canonicalized comparison** as a fallback (tachi#1184 review C4) —
///    catches the case the literal pass cannot: a root whose *given* spelling
///    differs from a listed root only through a symlink or case variant it
///    resolves to on disk (e.g. `$TMPDIR` reporting `/var/folders/xy/T` while
///    the filesystem's real path is `/private/var/folders/xy/T`).
///    `std::fs::canonicalize` fails (`ENOENT`, permission) whenever either
///    side does not exist — that failure is swallowed here and simply drops
///    this pass, never turning "cannot canonicalize" into a hard error; the
///    literal pass above already covers the common not-yet-provisioned case.
///
/// **Residual, accepted gap:** a root that does not exist yet AND differs
/// from every listed root only by case on a case-insensitive filesystem
/// evades both passes — canonicalize has nothing on disk to resolve the case
/// against, and the literal component comparison is case-sensitive by
/// design (it must be, to avoid false-positiving on genuinely distinct
/// paths elsewhere in this file). This is a loud *warning* surface, not a
/// security fence, so the gap is accepted rather than chased with a
/// case-folding heuristic that would risk false positives of its own.
fn is_under_any_root(path: &Path, ephemeral_roots: &[PathBuf]) -> bool {
    ephemeral_roots.iter().any(|root| {
        if path == root || path_is_within_components(path, root) {
            return true;
        }
        match (std::fs::canonicalize(path), std::fs::canonicalize(root)) {
            (Ok(canon_path), Ok(canon_root)) => {
                canon_path == canon_root || path_is_within_components(&canon_path, &canon_root)
            }
            _ => false,
        }
    })
}

/// Loud (never fatal) warning when an explicitly configured managed
/// worktrees root sits under a known-ephemeral scratch volume.
fn warn_if_ephemeral_root(path: &Path, source: &str) {
    if is_under_any_root(path, &ephemeral_root_prefixes()) {
        eprintln!(
            "[tachi worktree] WARNING: {source}={} points at an ephemeral scratch volume; a \
             worktree meant to outlive this session should live under a durable cache dir (the \
             default is ~/.cache/tachi/worktrees/<repo>), not /private/tmp, /tmp, or $TMPDIR — \
             those are wiped on reboot and orphan the tree's gitdir (tachi#1184)",
            path.display()
        );
    }
}

/// Env var overriding the shared `cargo` `target-dir` written into every
/// managed worktree's `.cargo/config.toml` (#484 slice 2).
pub const SHARED_CARGO_TARGET_DIR_ENV: &str = "TACHI_SHARED_CARGO_TARGET_DIR";

/// Default shared cargo target-dir (`~/.cache/sigil-shared-target`), used
/// when `TACHI_SHARED_CARGO_TARGET_DIR` is unset. Same fail-closed shape as
/// [`default_worktrees_root`]: refuses to fall back to the current working
/// directory when neither the env override nor `HOME`/`USERPROFILE` is set.
///
/// A *relative* `TACHI_SHARED_CARGO_TARGET_DIR` is rejected outright, same
/// discipline as `TACHI_WORKTREES_ROOT` above: `cargo` resolves a relative
/// `target-dir` in `.cargo/config.toml` against whatever cwd the build was
/// invoked from, not the worktree root — a relative override would silently
/// defeat the entire point of a *shared* target dir (each invocation cwd
/// would resolve to its own directory instead of the one shared cache).
pub fn default_shared_cargo_target_dir() -> Result<PathBuf, String> {
    if let Some(raw) = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV) {
        if !raw.is_empty() {
            let path = PathBuf::from(&raw);
            if !path.is_absolute() {
                return Err(format!(
                    "{SHARED_CARGO_TARGET_DIR_ENV} must be an absolute path, got relative \
                     path '{}': cargo resolves a relative target-dir against the \
                     per-invocation cwd, not the worktree root, which would silently \
                     defeat the shared-target-dir purpose",
                    path.display()
                ));
            }
            return Ok(path);
        }
    }
    match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)
            .join(".cache")
            .join("sigil-shared-target")),
        _ => Err(
            "cannot determine shared cargo target dir: HOME (and USERPROFILE) is unset and \
             TACHI_SHARED_CARGO_TARGET_DIR is not set"
                .to_string(),
        ),
    }
}

/// What build target, if any, a freshly opened worktree is wired to (#894 S2c).
///
/// Pre-S2c there was one behavior: every managed worktree got a
/// `.cargo/config.toml` pointing at the machine-shared `CARGO_TARGET_DIR`
/// (#484 slice 2, to stop each tree growing its own multi-GB `target/`). That
/// solved disk and created a correctness bug: N *diverged* worktrees driving
/// one target dir produced phantom compile errors (a symbol you can grep in the
/// source reported as `not found` by rustc — twice in one night, 2026-07-13).
///
/// So target allocation is now a per-env-class decision:
///
/// - [`CargoTargetPolicy::Unallocated`] — the `edit-only` default. No
///   `.cargo/config.toml` at all: this tree is not wired to any shared target,
///   and builds belong in the broker's serialized executor seat. This is a
///   *disk + routing* policy, NOT a sandbox: nothing stops a worker with a
///   shell from running cargo here anyway. What it does guarantee is that if
///   they do, the damage is a local `target/` dir the sweep can reclaim — they
///   cannot poison the seat's shared target from a diverged tree.
/// - [`CargoTargetPolicy::Shared`] — the pre-S2c behavior, kept for callers
///   that genuinely want the machine-shared target (the executor seat itself).
/// - [`CargoTargetPolicy::Private`] — an explicitly approved private target dir
///   (`build-private`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoTargetPolicy {
    /// No build target dir is wired to this worktree.
    Unallocated,
    /// Point `.cargo/config.toml` at the machine-shared `CARGO_TARGET_DIR`.
    Shared,
    /// Point `.cargo/config.toml` at this specific (private) target dir.
    Private(PathBuf),
}

/// Outcome of attempting to provision a `cargo` `target-dir` for a freshly
/// opened managed worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoTargetProvision {
    /// `.cargo/config.toml` was written pointing at this target dir.
    Written(PathBuf),
    /// The policy allocates no build target ([`CargoTargetPolicy::Unallocated`]).
    SkippedUnallocated,
    /// Not a Rust repo (no root `Cargo.toml`) — skipped silently.
    SkippedNotRustRepo,
    /// `.cargo/config.toml` already existed — never overwritten.
    SkippedExisting,
}

/// Write `<worktree_path>/.cargo/config.toml` with `[build] target-dir =
/// "<dir>"` per `policy` (#484 slice 2, generalized by #894 S2c).
///
/// File-based (not env-based) so it survives any child process/lane that
/// forgets to export `CARGO_TARGET_DIR`. Only applies to Rust repos (a root
/// `Cargo.toml` must exist in the worktree already, since `git worktree add`
/// checks out tracked files before this runs) and never overwrites an existing
/// `.cargo/config.toml`.
#[allow(dead_code)] // public library seam; the CLI target uses descriptor-bound provisioning
pub fn provision_cargo_target_config(
    worktree_path: &Path,
    policy: &CargoTargetPolicy,
) -> Result<CargoTargetProvision, String> {
    provision_cargo_target_config_inner(worktree_path, policy, None)
}

pub fn provision_cargo_target_config_with_authority(
    worktree_path: &Path,
    policy: &CargoTargetPolicy,
    authority: &memcore::anchored_fs::AnchoredDirectory,
) -> Result<CargoTargetProvision, String> {
    provision_cargo_target_config_inner(worktree_path, policy, Some(authority))
}

fn provision_cargo_target_config_inner(
    worktree_path: &Path,
    policy: &CargoTargetPolicy,
    authority: Option<&memcore::anchored_fs::AnchoredDirectory>,
) -> Result<CargoTargetProvision, String> {
    #[cfg(unix)]
    if let Some(authority) = authority {
        let anchored_path = canonical_non_symlink_directory(worktree_path)?;
        if !authority
            .matches_absolute_path(&anchored_path)
            .map_err(|error| format!("verify descriptor authority: {error}"))?
        {
            return Err(format!(
                "worktree identity changed after open: {}",
                worktree_path.display()
            ));
        }
    }
    // Checked before the Rust-repo probe: "this env gets no target" is a policy
    // statement, not a fact about the repo, and it holds either way.
    if matches!(policy, CargoTargetPolicy::Unallocated) {
        return Ok(CargoTargetProvision::SkippedUnallocated);
    }
    if !worktree_path.join("Cargo.toml").exists() {
        return Ok(CargoTargetProvision::SkippedNotRustRepo);
    }
    let cargo_dir = worktree_path.join(".cargo");
    let config_path = cargo_dir.join("config.toml");
    let (target_dir, provenance) = match policy {
        CargoTargetPolicy::Unallocated => unreachable!("handled above"),
        CargoTargetPolicy::Shared => (
            default_shared_cargo_target_dir()?,
            format!(
                "# Written by tachi wt-open (#484): shared cargo target-dir so managed\n\
                 # worktrees don't each grow their own multi-GB local target/.\n\
                 # Override via {SHARED_CARGO_TARGET_DIR_ENV} at worktree-open time.\n"
            ),
        ),
        CargoTargetPolicy::Private(dir) => {
            if !dir.is_absolute() {
                return Err(format!(
                    "private cargo target-dir must be an absolute path, got '{}': cargo \
                     resolves a relative target-dir against the per-invocation cwd, not the \
                     worktree root",
                    dir.display()
                ));
            }
            (
                dir.clone(),
                "# Written by tachi wt-open (#894 S2c): PRIVATE cargo target-dir for an\n\
                 # explicitly approved build-private env. Not shared with any other tree.\n"
                    .to_string(),
            )
        }
    };
    let contents = format!(
        "{provenance}[build]\ntarget-dir = \"{}\"\n",
        escape_toml_string(&target_dir.display().to_string())
    );
    #[cfg(not(unix))]
    {
        let _ = (cargo_dir, config_path, contents, target_dir, authority);
        return Err(
            "cargo target provisioning requires descriptor-anchored, no-follow filesystem operations on this platform"
                .to_string(),
        );
    }
    #[cfg(unix)]
    {
        let anchored_path = canonical_non_symlink_directory(worktree_path)?;
        let opened_authority;
        let worktree = match authority {
            Some(authority) => {
                let matches = authority
                    .matches_absolute_path(&anchored_path)
                    .map_err(|error| {
                        format!(
                            "verify descriptor authority for {}: {error}",
                            worktree_path.display()
                        )
                    })?;
                if !matches {
                    return Err(format!(
                        "worktree identity changed after open: {}",
                        worktree_path.display()
                    ));
                }
                authority
            }
            None => {
                opened_authority =
                    memcore::anchored_fs::AnchoredDirectory::open_absolute(&anchored_path)
                        .map_err(|err| {
                            format!("anchor {} without symlinks: {err}", worktree_path.display())
                        })?;
                &opened_authority
            }
        };
        let cargo = worktree
            .open_or_create_directory(std::ffi::OsStr::new(".cargo"))
            .map_err(|err| format!("open anchored {}: {err}", cargo_dir.display()))?;
        match cargo
            .create_file_exclusive(std::ffi::OsStr::new("config.toml"), contents.as_bytes())
            .map_err(|err| format!("write anchored {}: {err}", config_path.display()))?
        {
            memcore::anchored_fs::CreateFileOutcome::Created => {
                Ok(CargoTargetProvision::Written(target_dir))
            }
            memcore::anchored_fs::CreateFileOutcome::Exists => {
                Ok(CargoTargetProvision::SkippedExisting)
            }
        }
    }
}

/// Remove only the exact private Cargo config this provisioning code writes.
/// A missing or different file is not ours and is left for the ordinary dirty
/// worktree fence to adjudicate. This narrow rollback exists so a later atomic
/// ledger failure can restore the freshly opened worktree to its clean checkout
/// before certified `wt-remove`; it does not weaken general dirty cleanup.
#[allow(dead_code)] // library API; the CLI target compiles this module without the server caller
pub fn rollback_private_cargo_target_config(
    worktree_path: &Path,
    target_dir: &Path,
) -> Result<bool, String> {
    #[cfg(not(unix))]
    return Err(
        "private cargo target rollback requires descriptor-anchored, no-follow filesystem operations on this platform"
            .to_string(),
    );
    #[cfg(unix)]
    {
        if !target_dir.is_absolute() {
            return Err(format!(
                "private cargo target-dir rollback requires an absolute target, got '{}'",
                target_dir.display()
            ));
        }
        let config_path = worktree_path.join(".cargo").join("config.toml");
        let expected = format!(
            "# Written by tachi wt-open (#894 S2c): PRIVATE cargo target-dir for an\n\
         # explicitly approved build-private env. Not shared with any other tree.\n\
         [build]\ntarget-dir = \"{}\"\n",
            escape_toml_string(&target_dir.display().to_string())
        );
        let anchored_path = canonical_non_symlink_directory(worktree_path)?;
        let worktree = memcore::anchored_fs::AnchoredDirectory::open_absolute(&anchored_path)
            .map_err(|error| {
                format!(
                    "anchor {} before rollback: {error}",
                    worktree_path.display()
                )
            })?;
        let cargo = match worktree.open_directory(std::ffi::OsStr::new(".cargo")) {
            Ok(cargo) => cargo,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(format!(
                    "anchor {} before rollback: {error}",
                    config_path.display()
                ))
            }
        };
        cargo
            .remove_file_if_exact(std::ffi::OsStr::new("config.toml"), expected.as_bytes())
            .map_err(|error| {
                format!(
                    "remove exact owned config {}: {error}",
                    config_path.display()
                )
            })
    }
}

#[allow(dead_code)] // used by the server library; the cleaner binary compiles this module separately
pub fn rollback_private_cargo_target_config_with_authority(
    worktree_path: &Path,
    target_dir: &Path,
    authority: &memcore::anchored_fs::AnchoredDirectory,
) -> Result<bool, String> {
    #[cfg(not(unix))]
    return Err(
        "private cargo target rollback requires descriptor-anchored, no-follow filesystem operations on this platform"
            .to_string(),
    );
    #[cfg(unix)]
    {
        if !authority
            .matches_absolute_path(worktree_path)
            .map_err(|error| format!("verify rollback descriptor authority: {error}"))?
        {
            return Err("worktree identity changed before private Cargo rollback".to_string());
        }
        if !target_dir.is_absolute() {
            return Err(format!(
                "private cargo target-dir rollback requires an absolute target, got '{}'",
                target_dir.display()
            ));
        }
        let expected = format!(
            "# Written by tachi wt-open (#894 S2c): PRIVATE cargo target-dir for an\n\
             # explicitly approved build-private env. Not shared with any other tree.\n\
             [build]\ntarget-dir = \"{}\"\n",
            escape_toml_string(&target_dir.display().to_string())
        );
        let cargo = match authority.open_directory(std::ffi::OsStr::new(".cargo")) {
            Ok(cargo) => cargo,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("open anchored .cargo for rollback: {error}")),
        };
        cargo
            .remove_file_if_exact(std::ffi::OsStr::new("config.toml"), expected.as_bytes())
            .map_err(|error| format!("remove exact owned private Cargo config: {error}"))
    }
}

fn canonical_non_symlink_directory(path: &Path) -> Result<PathBuf, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect directory {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "refusing non-directory or symlink worktree authority: {}",
            path.display()
        ));
    }
    std::fs::canonicalize(path)
        .map_err(|error| format!("canonicalize directory {}: {error}", path.display()))
}

/// Minimal TOML basic-string escaping (backslash + double-quote) — paths on
/// this platform never legitimately need more than that.
fn escape_toml_string(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

const MAX_TASK_SEGMENT_LEN: usize = 48;
const MAX_ROLE_SEGMENT_LEN: usize = 48;
const MAX_LEAF_SEGMENT_LEN: usize = 128;

/// Stable short slug for a repository path.
pub fn repo_slug(repo_root: &Path) -> String {
    let name = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    sanitize_bounded_segment(name, MAX_LEAF_SEGMENT_LEN).unwrap_or_else(|| "repo".to_string())
}

pub fn plan_managed_worktree_path(repo_root: &Path, leaf_name: &str) -> Result<PathBuf, String> {
    let leaf = sanitize_bounded_segment(leaf_name, MAX_LEAF_SEGMENT_LEN).ok_or_else(|| {
        "refusing empty worktree leaf name (would alias parent worktree directory)".to_string()
    })?;
    Ok(default_worktrees_root()?
        .join(repo_slug(repo_root))
        .join(leaf))
}

/// Return a human reason when `path` must not host a managed worktree.
pub fn forbidden_location_reason(path: &Path, repo_root: &Path) -> Option<String> {
    let path_norm = normalize_for_policy(path);
    let repo_norm = normalize_for_policy(repo_root);

    if path_component_match(&path_norm, "Desktop") {
        return Some(
            "refusing worktree under Desktop (TCC/iCloud risk; use ~/.cache/tachi/worktrees)"
                .to_string(),
        );
    }
    if path_norm.contains("Mobile Documents")
        || path_norm.contains("com~apple~CloudDocs")
        || path_component_match(&path_norm, "iCloud Drive")
    {
        return Some(
            "refusing worktree under iCloud-synced path (use ~/.cache/tachi/worktrees)".to_string(),
        );
    }
    if path_is_within(&path_norm, &repo_norm) {
        return Some(format!(
            "refusing worktree inside primary repo '{repo_norm}' (use managed cache root outside the checkout)"
        ));
    }
    None
}

pub fn run_wt_open(options: OpenOptions) -> Result<(), String> {
    run_wt_open_with_emit(options)
}

pub fn open_worktree(options: OpenOptions) -> Result<OpenReport, String> {
    #[cfg(not(unix))]
    if !matches!(&options.cargo_target, CargoTargetPolicy::Unallocated) {
        return Err(
            "refusing worktree open: cargo target provisioning requires descriptor-anchored, no-follow filesystem operations on this platform"
                .to_string(),
        );
    }
    let repo_root = canonicalize_existing(&options.repo_root, "repo root")?;
    let base_ref = options
        .base
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("HEAD")
        .to_string();
    validate_ref(&base_ref)?;
    // `--end-of-options` (documented git-rev-parse idiom) stops a
    // maliciously option-shaped ref (e.g. "--upload-pack=...") from being
    // interpreted as a rev-parse flag instead of a literal revision.
    let base_sha = git_stdout(
        &repo_root,
        &["rev-parse", "--verify", "--end-of-options", &base_ref],
    )?;

    let (branch, leaf) = resolve_names(&options)?;
    let managed_root = default_worktrees_root()?;
    let mut path = match options.path {
        Some(p) => p,
        None => plan_managed_worktree_path(&repo_root, &leaf)?,
    };

    let mut report = OpenReport {
        action: "wt-open",
        dry_run: options.dry_run,
        opened: false,
        registered: false,
        repo_root: repo_root.display().to_string(),
        path: path.display().to_string(),
        branch: branch.clone(),
        base_ref: base_ref.clone(),
        base_sha: base_sha.clone(),
        managed_root: managed_root.display().to_string(),
        marker_path: None,
        worktree_authority: None,
        cargo_target_dir: None,
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    // Path governor boundary (CP3/CP6): every managed worktree — explicit
    // `--path` included — must resolve inside the managed root. Rejects
    // lexical `..` traversal outright and boundary-checks the
    // symlink-resolved (ancestor-canonicalized) form by path components, not
    // by string prefix, so `~/.cache/tachi/worktrees-evil` can't pass a
    // naive `starts_with` check against `~/.cache/tachi/worktrees`.
    if let Some(reason) = path_outside_managed_root_reason(&path, &managed_root) {
        report.errors.push(reason);
        return Ok(report);
    }

    if let Some(reason) = forbidden_location_reason(&path, &repo_root) {
        report.errors.push(reason);
        return Ok(report);
    }

    if path.exists() {
        report
            .errors
            .push(format!("worktree path already exists: {}", path.display()));
        return Ok(report);
    }

    // Same-path re-entry gate (tachi#1118 freeze boundary 3): a worktree
    // that was scrapped (removed via `wt-remove`/`sweep`) must reopen only
    // under a NEW branch + NEW path. The exact same path is a documented
    // re-entry route for a surviving writer that still holds a reference
    // to it (2026-07-15 incident: two lanes rebuilt at the same path after
    // removal and interleaved writes into the new checkout). This check
    // fires even in dry-run so a caller sees the refusal before creating
    // anything.
    //
    // The scrap ledger records the FULLY canonicalized (symlink-resolved)
    // path (`wt_clean::plan_wt_remove` canonicalizes an existing directory
    // before recording it); `path` here does not exist yet, so it is
    // normalized with the same ancestor-canonicalize-then-lexically-append
    // strategy already used for the managed-root boundary check just above
    // (`canonicalize_prefix`) rather than compared raw — otherwise a
    // symlinked temp/cache root (e.g. macOS `/tmp` -> `/private/tmp`) would
    // silently defeat the match.
    let scrap_lookup_path = canonicalize_prefix(&path);
    match crate::scrap_ledger::find_scrap_by_path(&scrap_lookup_path) {
        Ok(Some(scrap)) => {
            report.errors.push(format!(
                "refusing to reopen scrapped path {} (branch '{}' scrapped at {}); a scrapped tree must reopen under a NEW branch + NEW path, never the same path a surviving writer might still hold (tachi#1118)",
                path.display(),
                scrap.branch,
                scrap.scrapped_at,
            ));
            return Ok(report);
        }
        Ok(None) => {}
        Err(err) => {
            // Fail-closed (tachi#1212 fix-round, codex checkpoint 4): a
            // ledger we cannot consult is inconclusive, not evidence of "no
            // scrap on record" — proceeding here is exactly the fail-open
            // the cross-vendor review flagged on this safety boundary.
            report.errors.push(format!(
                "refusing to open: could not consult the scrap ledger for same-path re-entry ({err}); fail-closed rather than open on inconclusive evidence (tachi#1118)"
            ));
            return Ok(report);
        }
    }

    // Same-BRANCH re-entry gate, the other half of "NEW branch + NEW path"
    // (tachi#1212 fix-round, codex checkpoint 1): the path-only check above
    // stops the same path from reopening under a different branch, but on
    // its own it still let an OLD branch name reopen at a brand-new path
    // (scrap branch_A@/path1, then open branch_A@/path2 sailed through).
    // Either half reused alone is a re-entry route for a surviving writer
    // that cached a reference by path OR by branch name, so both must be
    // new.
    match crate::scrap_ledger::find_scrap_by_branch(&branch) {
        Ok(Some(scrap)) => {
            report.errors.push(format!(
                "refusing to reopen scrapped branch '{}' (previously scrapped at path {} on {}); a scrapped tree must reopen under a NEW branch + NEW path, never a branch name a surviving writer might still reference (tachi#1118)",
                branch,
                scrap.path,
                scrap.scrapped_at,
            ));
            return Ok(report);
        }
        Ok(None) => {}
        Err(err) => {
            report.errors.push(format!(
                "refusing to open: could not consult the scrap ledger for same-branch re-entry ({err}); fail-closed rather than open on inconclusive evidence (tachi#1118)"
            ));
            return Ok(report);
        }
    }

    if branch_exists_locally(&repo_root, &branch)? {
        // Allow reusing only if not already checked out in another worktree.
        if let Some(other) = branch_checkout_path(&repo_root, &branch)? {
            report.errors.push(format!(
                "branch '{branch}' is already checked out at {other}"
            ));
            return Ok(report);
        }
    }

    if options.dry_run {
        report.warnings.push(
            "dry-run: would run git worktree add and register the managed worktree".to_string(),
        );
        return Ok(report);
    }

    // CP1/CP2/CP6 defense-in-depth: re-run the boundary check immediately
    // before we touch the filesystem, shrinking (not closing) the window
    // between the earlier check and this create/add — a symlink or other
    // filesystem change to an ancestor directory in between could otherwise
    // let a since-altered path resolve outside the managed root. A
    // same-user check-then-act race in that shrunk window is accepted
    // residual risk for this single-user local tool; full TOCTOU-safety
    // (locking / openat / O_NOFOLLOW) is deliberately out of scope.
    if let Some(reason) = path_outside_managed_root_reason(&path, &managed_root) {
        report
            .errors
            .push(format!("re-check before create: {reason}"));
        return Ok(report);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create worktree parent {}: {err}", parent.display()))?;
    }

    let add_status = if branch_exists_locally(&repo_root, &branch)? {
        Command::new("git")
            .args([
                "-C",
                repo_root.to_str().ok_or("repo path is not valid UTF-8")?,
                "worktree",
                "add",
                path.to_str().ok_or("worktree path is not valid UTF-8")?,
                &branch,
            ])
            .status()
            .map_err(|err| format!("git worktree add failed to start: {err}"))?
    } else {
        Command::new("git")
            .args([
                "-C",
                repo_root.to_str().ok_or("repo path is not valid UTF-8")?,
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_str().ok_or("worktree path is not valid UTF-8")?,
                &base_sha,
            ])
            .status()
            .map_err(|err| format!("git worktree add failed to start: {err}"))?
    };

    if !add_status.success() {
        report.errors.push(format!(
            "git worktree add failed with status {add_status} (repo={}, path={}, branch={}, base={})",
            repo_root.display(),
            path.display(),
            branch,
            base_sha
        ));
        return Ok(report);
    }

    let opened_path = match canonical_non_symlink_directory(&path) {
        Ok(opened_path) => opened_path,
        Err(error) => {
            report.errors.push(format!(
                "write-lane entry gate refused: cannot anchor the newly opened worktree: {error}"
            ));
            return Ok(report);
        }
    };
    if let Some(reason) = path_outside_managed_root_reason(&opened_path, &managed_root) {
        report.errors.push(format!(
            "write-lane entry gate refused after worktree creation: {reason}"
        ));
        return Ok(report);
    }
    let authority = match memcore::anchored_fs::AnchoredDirectory::open_absolute(&opened_path) {
        Ok(authority) => authority,
        Err(error) => {
            report.errors.push(format!(
                "write-lane entry gate refused: cannot open descriptor authority for {}: {error}",
                opened_path.display()
            ));
            return Ok(report);
        }
    };
    path = opened_path;
    report.path = path.display().to_string();
    report.worktree_authority = Some(authority);

    // Write-lane entry gate (tachi#1118 freeze boundary 3, second half):
    // `git status --porcelain` must read empty the moment a freshly
    // created worktree is handed to a write lane. A non-empty status here
    // — despite `git worktree add` having just succeeded on a path that
    // didn't exist a moment ago — is exactly the 2026-07-15 smoking-gun
    // signature (a surviving writer from a scrapped predecessor bleeding
    // writes into the rebuilt tree). This never auto-deletes the path
    // (detection only, tachi#1062 stays sealed): it refuses to hand the
    // tree off as `opened`, leaving it in place for investigation.
    match crate::wt_clean::dirty_entries_excluding_marker(&path) {
        Ok(entries) if entries.is_empty() => {}
        Ok(entries) => {
            report.errors.push(format!(
                "write-lane entry gate refused: worktree is dirty immediately after creation ({}); a surviving writer may have interleaved with this open (tachi#1118). The path was NOT registered; inspect it before reuse.",
                entries.join(" | ")
            ));
            return Ok(report);
        }
        Err(err) => {
            report.errors.push(format!(
                "write-lane entry gate refused: could not verify the freshly created worktree is clean ({err}); fail-closed rather than hand off an unverified tree (tachi#1118). The path was NOT registered; inspect it before reuse."
            ));
            return Ok(report);
        }
    }

    report.opened = true;

    // Cargo target-dir per the env class's policy (#484 slice 2, #894 S2c):
    // file-based so it survives any child process/lane that forgets to export
    // CARGO_TARGET_DIR. Never fatal — a failure here does not undo the worktree
    // open. An `Unallocated` (edit-only) worktree gets no config at all.
    let authority = report
        .worktree_authority
        .as_ref()
        .expect("opened worktree has descriptor authority");
    match provision_cargo_target_config_with_authority(&path, &options.cargo_target, authority) {
        Ok(CargoTargetProvision::Written(dir)) => {
            report.cargo_target_dir = Some(dir.display().to_string());
        }
        Ok(CargoTargetProvision::SkippedUnallocated) => {}
        Ok(CargoTargetProvision::SkippedNotRustRepo) => {}
        Ok(CargoTargetProvision::SkippedExisting) => {
            report.warnings.push(format!(
                "skipped cargo target-dir provisioning: {} already exists",
                path.join(".cargo").join("config.toml").display()
            ));
        }
        Err(err) => {
            report
                .warnings
                .push(format!("cargo target-dir provisioning failed: {err}"));
        }
    }

    // Register marker + global registry so wt-remove / sweep can reclaim later.
    let dispatch_id = options.dispatch_id.or_else(|| options.task.clone());
    match registry::register_worktree_anchored(
        RegisterOptions {
            path: path.clone(),
            repo_root: repo_root.clone(),
            branch: branch.clone(),
            dispatch_id,
            pr: None,
            // Emit suppressed: open report is the single user-facing surface.
            output: RegisterOutputFormat::Json,
        },
        path.clone(),
        authority,
    ) {
        Ok(reg) => {
            report.registered = true;
            report.marker_path = Some(reg.marker_path);
        }
        Err(err) => {
            report.opened = false;
            report.errors.push(format!(
                "worktree opened but descriptor-bound registration failed: {err}; refusing handoff because the managed path has no trustworthy cleanup identity"
            ));
        }
    }

    Ok(report)
}

fn resolve_names(options: &OpenOptions) -> Result<(String, String), String> {
    let short = short_id();
    let task = options
        .task
        .as_deref()
        .and_then(|s| sanitize_bounded_segment(s, MAX_TASK_SEGMENT_LEN))
        .unwrap_or_else(|| "task".to_string());
    let role = options
        .role
        .as_deref()
        .and_then(|s| sanitize_bounded_segment(s, MAX_ROLE_SEGMENT_LEN))
        .unwrap_or_else(|| "worker".to_string());

    let leaf = options
        .name
        .as_deref()
        .and_then(|s| sanitize_bounded_segment(s, MAX_LEAF_SEGMENT_LEN))
        .unwrap_or_else(|| format!("{task}-{role}-{short}"));

    let branch = match options
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(b) => {
            if b.starts_with('-') {
                return Err(format!(
                    "refusing branch '{b}': refs beginning with '-' can be interpreted as git options"
                ));
            }
            b.to_string()
        }
        None => format!("tachi/{task}/{role}-{short}"),
    };

    Ok((branch, leaf))
}

/// Reject refs that could be interpreted as git options instead of
/// revisions (defense in depth alongside `--end-of-options`).
fn validate_ref(raw: &str) -> Result<(), String> {
    if raw.starts_with('-') {
        return Err(format!(
            "refusing base ref '{raw}': refs beginning with '-' can be interpreted as git options"
        ));
    }
    Ok(())
}

/// Generate a short random id for default branch/path leaves. A uuid v4 is
/// used precisely because it carries no timestamp/pid dependence: the prior
/// `nanos ^ (pid << 32)` scheme masked down to 24 bits and dropped the pid
/// entirely (pid lived at bit 32+, past the mask), so two default-name
/// provisions within ~16.8ms collided on path/branch (#1026 sibling, found
/// by codex review scanning the same generator class fixed in
/// `exec_env_ops::generate_env_id`). 12 hex chars keeps names short while
/// giving 48 bits of real randomness; the `:299` path-exists check remains
/// the backstop against the residual collision probability.
fn short_id() -> String {
    let full = uuid::Uuid::new_v4().simple().to_string();
    full[..12].to_string()
}

fn fnv1a_32_fold(bytes: &[u8]) -> u32 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    ((hash >> 32) ^ hash) as u32
}

fn neutralize_trailing_lock(mut seg: String) -> String {
    while seg.len() >= 5 && seg[seg.len() - 5..].eq_ignore_ascii_case(".lock") {
        let dot_idx = seg.len() - 5;
        if dot_idx > 0 && seg.as_bytes()[dot_idx - 1] == b'-' {
            seg.remove(dot_idx);
        } else {
            seg.replace_range(dot_idx..=dot_idx, "-");
        }
    }
    seg
}

/// Sanitize an arbitrary label/task string into a bounded ASCII path & git-ref
/// component (`<= max_len` bytes, no `..`, no leading/trailing `-` or `.`, and
/// no `.lock` suffix).
fn sanitize_bounded_segment(raw: &str, max_len: usize) -> Option<String> {
    let trimmed_raw = raw.trim();
    if trimmed_raw.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(trimmed_raw.len().min(max_len + 16));
    let mut last: Option<char> = None;
    for ch in trimmed_raw.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '_' {
            Some(ch)
        } else if ch == '.' {
            Some('.')
        } else if ch == '-' || ch == '/' || ch == '\\' || ch.is_whitespace() {
            Some('-')
        } else {
            None
        };
        if let Some(c) = mapped {
            if (c == '-' && last == Some('-')) || (c == '.' && last == Some('.')) {
                continue;
            }
            out.push(c);
            last = Some(c);
        }
    }
    let cleaned = out.trim_matches(['-', '.']).to_string();
    let fingerprint = fnv1a_32_fold(trimmed_raw.as_bytes());
    if cleaned.is_empty() {
        return Some(format!("s-{fingerprint:08x}"));
    }
    let cleaned = neutralize_trailing_lock(cleaned);
    if trimmed_raw.is_ascii() && cleaned.len() <= max_len {
        return Some(cleaned);
    }
    debug_assert!(max_len >= 10);
    let suffix = format!("-{fingerprint:08x}");
    let prefix_budget = max_len.saturating_sub(suffix.len());
    let limit = cleaned.len().min(prefix_budget);
    let prefix = cleaned[..limit].trim_end_matches(['-', '.']);
    if prefix.is_empty() {
        Some(format!("s-{fingerprint:08x}"))
    } else {
        Some(format!("{prefix}{suffix}"))
    }
}

fn normalize_for_policy(path: &Path) -> String {
    // Canonicalize the longest existing ancestor (resolving symlinks) and
    // lexically append whatever doesn't exist yet, rather than only
    // canonicalizing when the *entire* path already exists. A worktree
    // path is expected to not exist yet, so a whole-path-only canonicalize
    // silently degrades to pure lexical normalization for almost every
    // call, which does not defend against a symlinked intermediate
    // directory (e.g. an existing ancestor that is itself a symlink
    // pointing outside the managed root).
    let resolved = canonicalize_prefix(path);
    let mut parts = Vec::new();
    for component in resolved.components() {
        match component {
            Component::RootDir => parts.clear(),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::Prefix(p) => parts.push(p.as_os_str().to_string_lossy().into_owned()),
        }
    }
    if resolved.is_absolute() {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

fn path_component_match(normalized: &str, name: &str) -> bool {
    normalized
        .split('/')
        .any(|part| part.eq_ignore_ascii_case(name))
}

fn path_is_within(path_norm: &str, parent_norm: &str) -> bool {
    if path_norm == parent_norm {
        return true;
    }
    let prefix = if parent_norm.ends_with('/') {
        parent_norm.to_string()
    } else {
        format!("{parent_norm}/")
    };
    path_norm.starts_with(&prefix)
}

/// Canonicalize the longest existing ancestor of `path` (resolving
/// symlinks) and lexically re-append the remaining, not-yet-created
/// components. Falls back to the raw lexical path only when no ancestor
/// exists at all (e.g. a bare relative path with nothing on disk yet).
fn canonicalize_prefix(path: &Path) -> PathBuf {
    let mut trailing: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = path.to_path_buf();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&probe) {
            let mut result = canon;
            for part in trailing.iter().rev() {
                result.push(part);
            }
            return result;
        }
        let name = probe.file_name().map(|n| n.to_os_string());
        let parent = probe.parent().map(Path::to_path_buf);
        match (name, parent) {
            (Some(name), Some(parent)) if parent != probe => {
                trailing.push(name);
                probe = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// True if `path` contains a lexical `..` component anywhere. Explicit
/// `--path` values are never allowed to traverse, even if the traversal
/// would lexically stay inside the managed root — path fencing should not
/// have to reason about that; refuse it outright.
fn contains_parent_traversal(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

/// Component-wise "is `path` inside `boundary`" check (not a string
/// `starts_with`, which would wrongly accept a sibling like
/// `~/.cache/tachi/worktrees-evil` against boundary
/// `~/.cache/tachi/worktrees`).
fn path_is_within_components(path: &Path, boundary: &Path) -> bool {
    if path == boundary {
        return true;
    }
    let mut path_components = path.components();
    for boundary_component in boundary.components() {
        match path_components.next() {
            Some(component) if component == boundary_component => continue,
            _ => return false,
        }
    }
    path_components.next().is_some()
}

/// Return a human reason when `path` does not resolve inside the managed
/// worktree root `managed_root` — the CP3/CP6 governor boundary. Applies to
/// both explicit `--path` and (harmlessly, since it always passes) the
/// auto-planned path, so there is exactly one enforcement point.
fn path_outside_managed_root_reason(path: &Path, managed_root: &Path) -> Option<String> {
    if contains_parent_traversal(path) {
        return Some(format!(
            "refusing worktree path containing '..' (path traversal): {}",
            path.display()
        ));
    }
    let path_canon = canonicalize_prefix(path);
    let boundary_canon = canonicalize_prefix(managed_root);
    if !path_is_within_components(&path_canon, &boundary_canon) {
        return Some(format!(
            "refusing worktree path outside managed root '{}': {}",
            managed_root.display(),
            path.display()
        ));
    }
    None
}

fn canonicalize_existing(path: &Path, label: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|err| format!("cannot canonicalize {label}: {err}"))
}

fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| format!("git {:?} failed to start: {err}", args))?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn branch_exists_locally(repo: &Path, branch: &str) -> Result<bool, String> {
    let output = Command::new("git")
        .args([
            "-C",
            repo.to_str().ok_or("repo path is not valid UTF-8")?,
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .status()
        .map_err(|err| format!("git show-ref failed: {err}"))?;
    Ok(output.success())
}

fn branch_checkout_path(repo: &Path, branch: &str) -> Result<Option<String>, String> {
    let raw = git_stdout(repo, &["worktree", "list", "--porcelain"])?;
    let mut current_path: Option<String> = None;
    for line in raw.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.to_string());
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            if b == branch {
                return Ok(current_path);
            }
        }
    }
    Ok(None)
}

pub fn emit_open_report(report: &OpenReport, output: OutputFormat) -> Result<(), String> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|err| format!("serialize open report: {err}"))?
        ),
        OutputFormat::Text => {
            let mode = if report.dry_run { "dry-run" } else { "apply" };
            println!("tachi worktree open ({mode})");
            println!("  repo: {}", report.repo_root);
            println!("  path: {}", report.path);
            println!("  branch: {}", report.branch);
            println!("  base: {} ({})", report.base_ref, report.base_sha);
            println!("  managed_root: {}", report.managed_root);
            println!("  opened: {}", report.opened);
            println!("  registered: {}", report.registered);
            if let Some(dir) = &report.cargo_target_dir {
                println!("  cargo_target_dir: {dir}");
            }
            for warning in &report.warnings {
                println!("  warning: {warning}");
            }
            for error in &report.errors {
                println!("  error: {error}");
            }
        }
    }
    Ok(())
}

/// Re-run open with proper emit (used by CLI entrypoints).
pub fn run_wt_open_with_emit(options: OpenOptions) -> Result<(), String> {
    let output = options.output;
    let report = open_worktree(options)?;
    emit_open_report(&report, output)?;
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors.join("; "))
    }
}

#[cfg(test)]
#[path = "wt_open_tests.rs"]
mod tests;
