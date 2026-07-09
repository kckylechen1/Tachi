//! Managed worktree open (#484 / #894 slice).
//!
//! Tachi-owned entrypoint for creating linked git worktrees:
//! - Default root: `$TACHI_WORKTREES_ROOT` or `~/.cache/tachi/worktrees/<repo-slug>/`
//! - Refuses Desktop / iCloud / inside-primary-repo placement (TCC + residue)
//! - Registers via `wt-register` (marker + `~/.tachi/worktrees.json`)

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

/// Default managed worktrees root (not under Desktop / primary repo).
pub fn default_worktrees_root() -> PathBuf {
    if let Some(raw) = std::env::var_os("TACHI_WORKTREES_ROOT") {
        if !raw.is_empty() {
            return PathBuf::from(raw);
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("tachi").join("worktrees")
}

/// Stable short slug for a repository path.
pub fn repo_slug(repo_root: &Path) -> String {
    let name = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    sanitize_segment(name)
}

pub fn plan_managed_worktree_path(repo_root: &Path, leaf_name: &str) -> PathBuf {
    default_worktrees_root()
        .join(repo_slug(repo_root))
        .join(sanitize_segment(leaf_name))
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
    let base_sha = git_stdout(&repo_root, &["rev-parse", "--verify", &base_ref])?;

    let (branch, leaf) = resolve_names(&options)?;
    let managed_root = default_worktrees_root();
    let path = match options.path {
        Some(p) => p,
        None => plan_managed_worktree_path(&repo_root, &leaf),
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
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    if let Some(reason) = forbidden_location_reason(&path, &repo_root) {
        report.errors.push(reason);
        return Ok(report);
    }

    if path.exists() {
        report.errors.push(format!(
            "worktree path already exists: {}",
            path.display()
        ));
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

    let branch = match options.branch.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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

fn short_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = nanos ^ ((std::process::id() as u128) << 32);
    format!("{:06x}", mixed & 0x00ff_ffff)
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
    // Prefer real path when it exists; otherwise keep lexical form.
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
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

        let icloud = PathBuf::from(
            "/Users/me/Library/Mobile Documents/com~apple~CloudDocs/worktrees/x",
        );
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

        let planned = plan_managed_worktree_path(Path::new("/tmp/my-repo"), "484-executor-aa");
        assert_eq!(
            planned,
            root.join("my-repo").join("484-executor-aa")
        );

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
            dry_run: false,
            output: OutputFormat::Json,
        })
        .unwrap();

        assert!(
            report.errors.is_empty(),
            "open errors: {:?}",
            report.errors
        );
        assert!(report.opened);
        assert!(report.registered);
        let path = PathBuf::from(&report.path);
        assert!(path.starts_with(&cache), "path={} cache={}", path.display(), cache.display());
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
}
