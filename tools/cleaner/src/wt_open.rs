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
pub fn provision_cargo_target_config(
    worktree_path: &Path,
    policy: &CargoTargetPolicy,
) -> Result<CargoTargetProvision, String> {
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
    if config_path.exists() {
        return Ok(CargoTargetProvision::SkippedExisting);
    }
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
    std::fs::create_dir_all(&cargo_dir)
        .map_err(|err| format!("create {}: {err}", cargo_dir.display()))?;
    let contents = format!(
        "{provenance}[build]\ntarget-dir = \"{}\"\n",
        escape_toml_string(&target_dir.display().to_string())
    );
    std::fs::write(&config_path, contents)
        .map_err(|err| format!("write {}: {err}", config_path.display()))?;
    Ok(CargoTargetProvision::Written(target_dir))
}

/// Back-compat shim for the pre-#894 call shape: provision the machine-shared
/// target dir. Equivalent to [`provision_cargo_target_config`] with
/// [`CargoTargetPolicy::Shared`].
pub fn provision_shared_cargo_target_config(
    worktree_path: &Path,
) -> Result<CargoTargetProvision, String> {
    provision_cargo_target_config(worktree_path, &CargoTargetPolicy::Shared)
}

/// Minimal TOML basic-string escaping (backslash + double-quote) — paths on
/// this platform never legitimately need more than that.
fn escape_toml_string(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Stable short slug for a repository path.
pub fn repo_slug(repo_root: &Path) -> String {
    let name = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    sanitize_segment(name)
}

pub fn plan_managed_worktree_path(repo_root: &Path, leaf_name: &str) -> Result<PathBuf, String> {
    Ok(default_worktrees_root()?
        .join(repo_slug(repo_root))
        .join(sanitize_segment(leaf_name)))
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
    let path = match options.path {
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
    report.opened = true;

    // Cargo target-dir per the env class's policy (#484 slice 2, #894 S2c):
    // file-based so it survives any child process/lane that forgets to export
    // CARGO_TARGET_DIR. Never fatal — a failure here does not undo the worktree
    // open. An `Unallocated` (edit-only) worktree gets no config at all.
    match provision_cargo_target_config(&path, &options.cargo_target) {
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
    match registry::register_worktree(RegisterOptions {
        path: path.clone(),
        repo_root: repo_root.clone(),
        branch: branch.clone(),
        dispatch_id,
        pr: None,
        // Emit suppressed: open report is the single user-facing surface.
        output: RegisterOutputFormat::Json,
    }) {
        Ok(reg) => {
            report.registered = true;
            report.marker_path = Some(reg.marker_path);
        }
        Err(err) => {
            report.warnings.push(format!(
                "worktree opened but registration failed: {err}; run tachi-clean wt-register manually"
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
        .map(sanitize_segment)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "task".to_string());
    let role = options
        .role
        .as_deref()
        .map(sanitize_segment)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "worker".to_string());

    let leaf = options
        .name
        .as_deref()
        .map(sanitize_segment)
        .filter(|s| !s.is_empty())
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

fn sanitize_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else if ch == '/' || ch == '\\' || ch == ' ' {
            out.push('-');
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed
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
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn default_short_id_is_12_hex_and_unique() {
        // Discriminating test for the #1026 sibling fix: 12 chars pins the
        // truncation width (a regression to the old 6-hex mask would fail
        // here), hex-only pins the uuid `simple()` form, and two consecutive
        // calls differing pins the same-instant-collision fix itself.
        let a = short_id();
        let b = short_id();
        assert_eq!(a.len(), 12, "short_id must keep 48 bits (12 hex chars)");
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b, "consecutive short_ids must not collide");
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn unique_temp(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn init_git_repo(path: &Path) {
        assert!(Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(path)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.email", "tachi-test@example.com"])
            .current_dir(path)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.name", "tachi-test"])
            .current_dir(path)
            .status()
            .unwrap()
            .success());
        std::fs::write(path.join("README"), "hello").unwrap();
        assert!(Command::new("git")
            .args(["add", "README"])
            .current_dir(path)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(path)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn forbids_desktop_and_in_repo_paths() {
        let repo = PathBuf::from("/Users/me/Desktop/Sigil");
        let desktop_wt = PathBuf::from("/Users/me/Desktop/Sigil/.claude/worktrees/agent-1");
        let reason = forbidden_location_reason(&desktop_wt, &repo).expect("forbidden");
        assert!(
            reason.contains("Desktop") || reason.contains("primary repo"),
            "unexpected reason: {reason}"
        );

        let icloud =
            PathBuf::from("/Users/me/Library/Mobile Documents/com~apple~CloudDocs/worktrees/x");
        let reason = forbidden_location_reason(&icloud, Path::new("/tmp/repo")).expect("icloud");
        assert!(reason.contains("iCloud"), "{reason}");

        let ok = PathBuf::from("/Users/me/.cache/tachi/worktrees/Sigil/484-worker-abc");
        assert!(
            forbidden_location_reason(&ok, Path::new("/Users/me/Desktop/Sigil")).is_none(),
            "managed cache path must be allowed"
        );
    }

    #[test]
    fn plan_path_uses_managed_root_and_slug() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("TACHI_WORKTREES_ROOT");
        let root = unique_temp("tachi-wt-root");
        std::env::set_var("TACHI_WORKTREES_ROOT", &root);

        let planned = plan_managed_worktree_path(Path::new("/tmp/my-repo"), "484-executor-aa")
            .expect("managed root resolves when TACHI_WORKTREES_ROOT is set");
        assert_eq!(planned, root.join("my-repo").join("484-executor-aa"));

        match old {
            Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
            None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn open_refuses_desktop_path_even_when_explicit() {
        // Discrimination: pre-fix world allowed .claude/worktrees under Desktop;
        // governor must hard-refuse before git worktree add.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-wt-open-desktop");
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);

        let desktop_path = home.join("Desktop").join("fake-wt");
        let report = open_worktree(OpenOptions {
            repo_root: repo,
            path: Some(desktop_path),
            branch: Some("tachi/test/desktop-refuse".into()),
            base: Some("HEAD".into()),
            task: Some("484".into()),
            role: Some("executor".into()),
            dispatch_id: None,
            name: None,
            cargo_target: CargoTargetPolicy::Shared,
            dry_run: false,
            output: OutputFormat::Json,
        })
        .unwrap();

        assert!(!report.opened, "must not open under Desktop");
        assert!(
            report.errors.iter().any(|e| e.contains("Desktop")),
            "expected Desktop refusal, got: {:?}",
            report.errors
        );

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn open_creates_managed_worktree_and_marker() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-wt-open-ok");
        let home = root.join("home");
        let cache = root.join("cache-worktrees");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        let old_home = std::env::var_os("HOME");
        let old_root = std::env::var_os("TACHI_WORKTREES_ROOT");
        std::env::set_var("HOME", &home);
        std::env::set_var("TACHI_WORKTREES_ROOT", &cache);

        let report = open_worktree(OpenOptions {
            repo_root: repo.clone(),
            path: None,
            branch: Some("tachi/484/executor-test".into()),
            base: Some("HEAD".into()),
            task: Some("484".into()),
            role: Some("executor".into()),
            dispatch_id: Some("dispatch-484".into()),
            name: Some("484-executor-test".into()),
            cargo_target: CargoTargetPolicy::Shared,
            dry_run: false,
            output: OutputFormat::Json,
        })
        .unwrap();

        assert!(report.errors.is_empty(), "open errors: {:?}", report.errors);
        assert!(report.opened);
        assert!(report.registered);
        let path = PathBuf::from(&report.path);
        assert!(
            path.starts_with(&cache),
            "path={} cache={}",
            path.display(),
            cache.display()
        );
        assert!(path.join(".tachi-worktree.json").exists());
        assert!(path.join("README").exists());
        assert!(registry::registry_contains(&path));

        // Cleanup worktree from the temp repo so the test dir can be removed.
        let _ = Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "worktree",
                "remove",
                "--force",
                path.to_str().unwrap(),
            ])
            .status();

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_root {
            Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
            None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dry_run_does_not_create_path() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-wt-open-dry");
        let home = root.join("home");
        let cache = root.join("cache-worktrees");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        let old_home = std::env::var_os("HOME");
        let old_root = std::env::var_os("TACHI_WORKTREES_ROOT");
        std::env::set_var("HOME", &home);
        std::env::set_var("TACHI_WORKTREES_ROOT", &cache);

        let report = open_worktree(OpenOptions {
            repo_root: repo,
            path: None,
            branch: Some("tachi/484/dry".into()),
            base: Some("HEAD".into()),
            task: Some("484".into()),
            role: Some("worker".into()),
            dispatch_id: None,
            name: Some("dry-leaf".into()),
            cargo_target: CargoTargetPolicy::Shared,
            dry_run: true,
            output: OutputFormat::Json,
        })
        .unwrap();

        assert!(report.dry_run);
        assert!(!report.opened);
        assert!(!PathBuf::from(&report.path).exists());

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_root {
            Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
            None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn default_worktrees_root_rejects_relative_env_value() {
        // CP3 discrimination: a relative TACHI_WORKTREES_ROOT must never
        // silently resolve against cwd — the managed root doubles as the
        // sweep GC root and the path-boundary fence.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("TACHI_WORKTREES_ROOT");
        std::env::set_var("TACHI_WORKTREES_ROOT", "relative/worktrees");

        let result = default_worktrees_root();
        assert!(
            result.is_err(),
            "relative TACHI_WORKTREES_ROOT must be rejected, got {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("absolute"),
            "error should explain the absolute-path requirement: {err}"
        );

        match old {
            Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
            None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
        }
    }

    #[test]
    fn open_rejects_option_shaped_base_ref() {
        // CP6 discrimination: a base ref shaped like a git option must be
        // rejected before it ever reaches `git rev-parse`, not just relying
        // on `--end-of-options`. RED if `validate_ref`'s leading-`-` check
        // (or its call site) is removed.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-wt-open-ref-injection");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        let result = open_worktree(OpenOptions {
            repo_root: repo,
            path: None,
            branch: Some("tachi/test/ref-injection".into()),
            base: Some("--upload-pack=/bin/sh".into()),
            task: Some("484".into()),
            role: Some("executor".into()),
            dispatch_id: None,
            name: None,
            cargo_target: CargoTargetPolicy::Shared,
            dry_run: false,
            output: OutputFormat::Json,
        });

        assert!(
            result.is_err(),
            "option-shaped base ref must be rejected before git rev-parse, got {result:?}"
        );
        let err = result.unwrap_err();
        assert!(err.contains("refusing base ref"), "unexpected error: {err}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn open_rejects_leading_dash_base_ref() {
        // CP6 discrimination, second shape: a bare leading-dash ref (no
        // '=' payload) must also be rejected, not just the `--flag=value`
        // form.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-wt-open-ref-injection-dash");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        let result = open_worktree(OpenOptions {
            repo_root: repo,
            path: None,
            branch: Some("tachi/test/ref-injection-dash".into()),
            base: Some("-x".into()),
            task: Some("484".into()),
            role: Some("executor".into()),
            dispatch_id: None,
            name: None,
            cargo_target: CargoTargetPolicy::Shared,
            dry_run: false,
            output: OutputFormat::Json,
        });

        assert!(
            result.is_err(),
            "leading-dash base ref must be rejected, got {result:?}"
        );
        let err = result.unwrap_err();
        assert!(err.contains("refusing base ref"), "unexpected error: {err}");

        let _ = std::fs::remove_dir_all(root);
    }

    // --- #484 slice 2: shared cargo target-dir provisioning ---

    #[test]
    fn provision_writes_config_for_rust_repo() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-cargo-provision-rust");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let target = root.join("shared-target");

        let old = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
        std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, &target);

        let outcome = provision_shared_cargo_target_config(&root).expect("provision ok");
        match &outcome {
            CargoTargetProvision::Written(dir) => assert_eq!(dir, &target),
            other => panic!("expected Written, got {other:?}"),
        }

        let config_path = root.join(".cargo").join("config.toml");
        let contents = std::fs::read_to_string(&config_path).expect("config written");
        assert!(contents.contains("[build]"), "contents: {contents}");
        assert!(
            contents.contains(&format!("target-dir = \"{}\"", target.display())),
            "contents should point at the shared target dir, got: {contents}"
        );

        match old {
            Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
            None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn provision_skips_non_rust_repo_silently() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-cargo-provision-non-rust");
        // No Cargo.toml.

        let outcome = provision_shared_cargo_target_config(&root).expect("provision ok");
        assert_eq!(outcome, CargoTargetProvision::SkippedNotRustRepo);
        assert!(
            !root.join(".cargo").exists(),
            "non-Rust repo must not get a .cargo dir"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn provision_never_overwrites_existing_config() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-cargo-provision-existing");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        let custom = "# hand-written, do not touch\n[build]\ntarget-dir = \"/custom/target\"\n";
        std::fs::write(root.join(".cargo").join("config.toml"), custom).unwrap();

        let target = root.join("shared-target");
        let old = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
        std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, &target);

        let outcome = provision_shared_cargo_target_config(&root).expect("provision ok");
        assert_eq!(outcome, CargoTargetProvision::SkippedExisting);
        let contents = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
        assert_eq!(contents, custom, "existing config.toml must be untouched");

        match old {
            Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
            None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn provision_rejects_relative_override_and_writes_no_config() {
        // A relative TACHI_SHARED_CARGO_TARGET_DIR would make cargo resolve
        // target-dir against whatever cwd the build happens to run from,
        // defeating the whole point of a *shared* target dir. Must be
        // rejected outright rather than silently written relative.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-cargo-provision-relative");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

        let old = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
        std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, "relative/shared-target");

        let err = provision_shared_cargo_target_config(&root)
            .expect_err("relative override must be rejected");
        assert!(
            err.contains("must be an absolute path"),
            "error should explain the absolute-path requirement, got: {err}"
        );
        assert!(
            !root.join(".cargo").join("config.toml").exists(),
            "no config.toml should be written for a rejected relative override"
        );

        match old {
            Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
            None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn provision_writes_absolute_override_verbatim() {
        // Companion to the relative-rejection test above: an absolute
        // override must be written into config.toml exactly as given, not
        // normalized/canonicalized.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-cargo-provision-absolute-verbatim");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let target = root.join("nested").join("shared-target");
        assert!(target.is_absolute());

        let old = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
        std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, &target);

        let outcome = provision_shared_cargo_target_config(&root).expect("provision ok");
        assert_eq!(outcome, CargoTargetProvision::Written(target.clone()));
        let contents = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
        assert!(
            contents.contains(&format!("target-dir = \"{}\"", target.display())),
            "absolute override should be written verbatim, got: {contents}"
        );

        match old {
            Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
            None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn open_worktree_warns_and_skips_provisioning_for_relative_override() {
        // End-to-end: a relative TACHI_SHARED_CARGO_TARGET_DIR must not
        // fail the worktree open (non-fatal, same style as other
        // provisioning failures) but must surface a warning and leave no
        // config.toml behind.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-wt-open-cargo-relative");
        let home = root.join("home");
        let cache = root.join("cache-worktrees");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "Cargo.toml"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "add Cargo.toml"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());

        let old_home = std::env::var_os("HOME");
        let old_root = std::env::var_os("TACHI_WORKTREES_ROOT");
        let old_target = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
        std::env::set_var("HOME", &home);
        std::env::set_var("TACHI_WORKTREES_ROOT", &cache);
        std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, "relative/shared-target");

        let report = open_worktree(OpenOptions {
            repo_root: repo.clone(),
            path: None,
            branch: Some("tachi/484/executor-cargo-relative".into()),
            base: Some("HEAD".into()),
            task: Some("484".into()),
            role: Some("executor".into()),
            dispatch_id: Some("dispatch-484-cargo-relative".into()),
            name: Some("484-executor-cargo-relative".into()),
            cargo_target: CargoTargetPolicy::Shared,
            dry_run: false,
            output: OutputFormat::Json,
        })
        .unwrap();

        assert!(report.errors.is_empty(), "open errors: {:?}", report.errors);
        assert!(report.opened);
        assert!(
            report.cargo_target_dir.is_none(),
            "no cargo_target_dir should be reported for a rejected relative override"
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("must be an absolute path")),
            "expected a warning about the relative override, got: {:?}",
            report.warnings
        );

        let path = PathBuf::from(&report.path);
        assert!(
            !path.join(".cargo").join("config.toml").exists(),
            "no config.toml should be written when the override is relative"
        );

        // Cleanup worktree from the temp repo so the test dir can be removed.
        let _ = Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "worktree",
                "remove",
                "--force",
                path.to_str().unwrap(),
            ])
            .status();

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_root {
            Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
            None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
        }
        match old_target {
            Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
            None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn open_worktree_provisions_shared_cargo_target_for_rust_repo() {
        // End-to-end: git worktree add checks out a tracked root Cargo.toml,
        // so open_worktree must then write .cargo/config.toml pointing at
        // the shared target dir into the freshly created worktree.
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let root = unique_temp("tachi-wt-open-cargo");
        let home = root.join("home");
        let cache = root.join("cache-worktrees");
        let shared_target = root.join("shared-target");
        let repo = root.join("repo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "Cargo.toml"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "add Cargo.toml"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());

        let old_home = std::env::var_os("HOME");
        let old_root = std::env::var_os("TACHI_WORKTREES_ROOT");
        let old_target = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
        std::env::set_var("HOME", &home);
        std::env::set_var("TACHI_WORKTREES_ROOT", &cache);
        std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, &shared_target);

        let report = open_worktree(OpenOptions {
            repo_root: repo.clone(),
            path: None,
            branch: Some("tachi/484/executor-cargo".into()),
            base: Some("HEAD".into()),
            task: Some("484".into()),
            role: Some("executor".into()),
            dispatch_id: Some("dispatch-484-cargo".into()),
            name: Some("484-executor-cargo".into()),
            cargo_target: CargoTargetPolicy::Shared,
            dry_run: false,
            output: OutputFormat::Json,
        })
        .unwrap();

        assert!(report.errors.is_empty(), "open errors: {:?}", report.errors);
        assert!(report.opened);
        assert_eq!(
            report.cargo_target_dir.as_deref(),
            Some(shared_target.display().to_string().as_str())
        );

        let path = PathBuf::from(&report.path);
        let config = path.join(".cargo").join("config.toml");
        assert!(config.exists(), "expected {} to exist", config.display());
        let contents = std::fs::read_to_string(&config).unwrap();
        assert!(
            contents.contains(&shared_target.display().to_string()),
            "contents: {contents}"
        );

        // Cleanup worktree from the temp repo so the test dir can be removed.
        let _ = Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "worktree",
                "remove",
                "--force",
                path.to_str().unwrap(),
            ])
            .status();

        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match old_root {
            Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
            None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
        }
        match old_target {
            Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
            None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
