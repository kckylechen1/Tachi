use super::*;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod bounded;
mod bounded_json;
pub(in crate::gh_ops) use bounded::{is_outcome_unknown_message, GhCall, GhRunError};
pub(in crate::gh_ops) use bounded_json::run_gh_json_observed_bounded;

const GH_AGENT_ID: &str = "tachi_gh_ops";
const MAX_GH_OUTPUT_CHARS: usize = 50_000;
#[cfg(test)]
std::thread_local! {
    static GITHUB_COMMAND_RUNNER_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
const GH_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "SSH_AUTH_SOCK",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
];

pub(in crate::gh_ops) fn validate_repo(repo: &str) -> Result<(), String> {
    let invalid =
        || "Invalid repo format. Expected canonical GitHub 'owner/repo' syntax.".to_string();
    let (owner, name) = repo.split_once('/').ok_or_else(&invalid)?;
    if name.contains('/')
        || owner.is_empty()
        || owner.len() > 39
        || name.is_empty()
        || name.len() > 100
    {
        return Err(invalid());
    }
    let owner_valid = owner
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && !owner.contains("--");
    let name_valid = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !name.starts_with('-')
        && name != "."
        && name != "..";
    if !owner_valid || !name_valid {
        return Err(invalid());
    }
    Ok(())
}

/// Resolve the path of the `gh` binary by scanning `PATH` in-process.
///
/// This used to shell out to `which gh` on every call. The scan honors the
/// same `PATH` (including test shims prepended to it) and the same "first
/// executable regular file wins" rule, but spawns nothing, so a bounded call
/// whose deadline fires during preparation cannot leave a process behind.
/// It is recomputed per call: no cross-operation caching of the path.
pub(in crate::gh_ops) fn resolve_gh_path() -> Result<String, String> {
    let path = std::env::var_os("PATH").and_then(|paths| find_gh_in_path(&paths));
    let Some(path) = path else {
        return Err("GitHub CLI (`gh`) not found. Install it: https://cli.github.com".into());
    };
    let path = path.to_string_lossy().to_string();
    if path.is_empty() {
        return Err("`gh` CLI path resolved to empty string".into());
    }
    Ok(path)
}

fn find_gh_in_path(paths: &std::ffi::OsStr) -> Option<PathBuf> {
    const NAMES: &[&str] = if cfg!(windows) {
        &["gh.exe", "gh"]
    } else {
        &["gh"]
    };
    for dir in std::env::split_paths(paths) {
        // An empty PATH entry means the current directory, as for `which`.
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        for name in NAMES {
            let candidate = dir.join(name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

static AUTH_HEADER_PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"(?i)(?:authorization:[ \t]*(?:bearer|token|basic)(?:[ \t]+[^ \t\r\n,;"<>]+|[ \t]*)|\b(?:x-github-token|x-access-token)[ \t]*[:=](?:[ \t]*[^ \t\r\n,;"<>]+|[ \t]*)|bearer[ \t]+(?:gh[opusr]_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+)[^\r\n]*|token[ \t]+(?:gh[opusr]_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+)[^\r\n]*)"#,
    )
    .expect("valid auth header pattern")
});

static GITHUB_TOKEN_PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\b(?:gh[opusr]_[A-Za-z0-9_]{10,}|github_pat_[A-Za-z0-9_]{20,})\b")
        .expect("valid github token pattern")
});

/// Strip sensitive tokens from output text
pub(in crate::gh_ops) fn sanitize_output(text: &str, token: &str) -> String {
    let mut sanitized = text.to_string();
    if !token.is_empty() {
        sanitized = sanitized.replace(token, "[REDACTED]");
    }
    // Redact all occurrences of auth headers and bearer/token patterns
    let sanitized = AUTH_HEADER_PATTERN.replace_all(&sanitized, "[REDACTED]");
    // Redact any raw GitHub credentials remaining in output
    let sanitized = GITHUB_TOKEN_PATTERN.replace_all(&sanitized, "[REDACTED]");
    sanitized.into_owned()
}

pub(in crate::gh_ops) fn vault_secret_unavailable(err: &str) -> bool {
    crate::vault_ops::is_env_fallback_eligible(crate::vault_ops::classify_vault_read_error(err))
}

pub(in crate::gh_ops) fn env_gh_token() -> Option<String> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub(in crate::gh_ops) fn resolve_gh_token(server: &MemoryServer) -> Result<Option<String>, String> {
    match read_unlocked_vault_secret(server, "GH_TOKEN", Some(GH_AGENT_ID), false) {
        Ok(token) => Ok(Some(token)),
        Err(err) if vault_secret_unavailable(&err) => Ok(env_gh_token()),
        Err(err) => Err(err),
    }
}

pub(in crate::gh_ops) fn preserve_gh_env(cmd: &mut Command) {
    for var in GH_ENV_ALLOWLIST {
        if let Ok(val) = std::env::var(var) {
            cmd.env(var, val);
        }
    }
    if std::env::var_os("GITHUB_TOKEN").is_some() && std::env::var_os("GH_TOKEN").is_none() {
        if let Ok(val) = std::env::var("GITHUB_TOKEN") {
            cmd.env("GITHUB_TOKEN", val);
        }
    }
}

#[derive(Debug)]
#[cfg(unix)]
pub(in crate::gh_ops) struct GhBodyFileGuard(
    pub(in crate::gh_ops) PathBuf,
    std::os::unix::io::OwnedFd,
    std::ffi::CString,
);

#[derive(Debug)]
#[cfg(not(unix))]
pub(in crate::gh_ops) struct GhBodyFileGuard(pub(in crate::gh_ops) PathBuf);

impl Drop for GhBodyFileGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe {
                libc::unlinkat(self.1.as_raw_fd(), self.2.as_ptr(), 0);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = fs::remove_file(&self.0);
        }
    }
}

#[cfg(unix)]
fn open_private_gh_dir_in(
    base: &std::path::Path,
) -> Result<(PathBuf, std::os::unix::io::OwnedFd), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let uid = unsafe { libc::geteuid() };
    let dir_path = base.join(format!("tachi-gh-body-{}", uid));
    let c_dir = CString::new(dir_path.as_os_str().as_bytes())
        .map_err(|e| format!("dir path has interior nul: {e}"))?;

    // Attempt to open directory with O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC
    let mut fd = unsafe {
        libc::open(
            c_dir.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };

    if fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::NotFound {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            if let Err(create_err) = builder.create(&dir_path) {
                if create_err.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(format!(
                        "create private gh body dir {}: {create_err}",
                        dir_path.display()
                    ));
                }
            }
            fd = unsafe {
                libc::open(
                    c_dir.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(format!(
                    "open private gh body dir {}: {}",
                    dir_path.display(),
                    std::io::Error::last_os_error()
                ));
            }
        } else {
            return Err(format!("open gh body dir {}: {err}", dir_path.display()));
        }
    }

    let owned_dir_fd = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(fd) };
    validate_and_harden_gh_dir_fd(owned_dir_fd.as_raw_fd(), &dir_path, uid)?;
    Ok((dir_path, owned_dir_fd))
}

#[cfg(unix)]
fn validate_and_harden_gh_dir_fd(
    dir_fd: std::os::unix::io::RawFd,
    dir_path: &std::path::Path,
    expected_uid: libc::uid_t,
) -> Result<(), String> {
    let mut stat_buf: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(dir_fd, &mut stat_buf) };
    if rc != 0 {
        return Err(format!(
            "fstat gh body dir {}: {}",
            dir_path.display(),
            std::io::Error::last_os_error()
        ));
    }

    if stat_buf.st_uid != expected_uid {
        return Err(format!(
            "gh body dir {} is owned by uid {}, expected current uid {}",
            dir_path.display(),
            stat_buf.st_uid,
            expected_uid
        ));
    }

    let mode = (stat_buf.st_mode & 0o777) as u32;
    if mode != 0o700 {
        let chmod_rc = unsafe { libc::fchmod(dir_fd, 0o700) };
        if chmod_rc != 0 {
            return Err(format!(
                "restrict gh body dir permissions {}: {}",
                dir_path.display(),
                std::io::Error::last_os_error()
            ));
        }
        let rc = unsafe { libc::fstat(dir_fd, &mut stat_buf) };
        if rc != 0 || (stat_buf.st_mode & 0o777) != 0o700 {
            return Err(format!(
                "failed to enforce 0700 mode on gh body dir {}",
                dir_path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(any(test, not(unix)))]
fn gh_body_temp_dir() -> Result<PathBuf, String> {
    gh_body_temp_dir_in(&std::env::temp_dir())
}

#[cfg(any(test, not(unix)))]
fn gh_body_temp_dir_in(base: &std::path::Path) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        let (dir_path, _dir_fd) = open_private_gh_dir_in(base)?;
        Ok(dir_path)
    }
    #[cfg(not(unix))]
    {
        let dir = base.join("tachi-gh-body");
        fs::create_dir_all(&dir)
            .map_err(|err| format!("create gh body dir {}: {err}", dir.display()))?;
        Ok(dir)
    }
}

/// Mint a unique per-call leaf filename for the body tempfile.
fn mint_gh_body_file_name() -> Result<String, String> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system clock before UNIX_EPOCH: {err}"))?
        .as_nanos();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(format!(
        "tachi-gh-body-{}-{nanos}-{seq}.txt",
        std::process::id()
    ))
}

/// A per-call unique path for the `gh --body-file` tempfile.
#[cfg(any(test, not(unix)))]
fn gh_body_temp_path() -> Result<PathBuf, String> {
    let base_dir = gh_body_temp_dir()?;
    let leaf_name = mint_gh_body_file_name()?;
    Ok(base_dir.join(leaf_name))
}

#[cfg(not(unix))]
fn gh_body_temp_path_in(base: &std::path::Path) -> Result<PathBuf, String> {
    let base_dir = gh_body_temp_dir_in(base)?;
    let leaf_name = mint_gh_body_file_name()?;
    Ok(base_dir.join(leaf_name))
}

/// Write `body` to a fresh temp file for `gh --body-file`. The returned
/// guard removes the file when dropped (see [`GhCall::attach_body_file`]).
fn write_gh_body_file(body: &str) -> Result<GhBodyFileGuard, String> {
    write_gh_body_file_in(&std::env::temp_dir(), body, |file, body| {
        file.write_all(body.as_bytes())
            .map_err(|err| format!("write gh body tempfile: {err}"))?;
        file.flush()
            .map_err(|err| format!("flush gh body tempfile: {err}"))?;
        Ok(())
    })
}

fn write_gh_body_file_in<F>(
    base: &std::path::Path,
    body: &str,
    writer: F,
) -> Result<GhBodyFileGuard, String>
where
    F: FnOnce(&mut fs::File, &str) -> Result<(), String>,
{
    #[cfg(unix)]
    let (mut file, guard) = {
        use std::ffi::CString;
        use std::os::unix::io::{AsRawFd, FromRawFd};

        let (dir_path, owned_dir_fd) = open_private_gh_dir_in(base)?;
        let leaf_name = mint_gh_body_file_name()?;
        let path = dir_path.join(&leaf_name);
        let c_leaf = CString::new(leaf_name.as_bytes())
            .map_err(|e| format!("leaf name has interior nul: {e}"))?;

        let fd = unsafe {
            libc::openat(
                owned_dir_fd.as_raw_fd(),
                c_leaf.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(format!(
                "create gh body tempfile: {}",
                std::io::Error::last_os_error()
            ));
        }

        let file = unsafe { fs::File::from_raw_fd(fd) };
        // Immediately wrap path and directory descriptor in guard so any subsequent failure unlinks the file!
        let guard = GhBodyFileGuard(path, owned_dir_fd, c_leaf);
        let chmod_rc = unsafe { libc::fchmod(file.as_raw_fd(), 0o600) };
        if chmod_rc != 0 {
            return Err(format!(
                "set permissions on gh body tempfile: {}",
                std::io::Error::last_os_error()
            ));
        }
        (file, guard)
    };

    #[cfg(not(unix))]
    let (mut file, guard) = {
        let path = gh_body_temp_path_in(base)?;
        let f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|err| format!("create gh body tempfile: {err}"))?;
        let guard = GhBodyFileGuard(path);
        (f, guard)
    };

    let write_res = writer(&mut file, body);
    drop(file);

    match write_res {
        Ok(()) => Ok(guard),
        Err(err) => {
            drop(guard);
            Err(err)
        }
    }
}

/// Build a sanitized Command for `gh` with env_clear + vault token injection
pub(in crate::gh_ops) fn build_gh_command(
    server: &MemoryServer,
) -> Result<(Command, String), String> {
    let gh_path = resolve_gh_path()?;
    let token = resolve_gh_token(server)?;
    let cmd = build_gh_command_for_resolved_credential(&gh_path, token.as_deref());

    Ok((cmd, token.unwrap_or_default()))
}

/// Rebuild the hardened `gh` command from an already-resolved executable and
/// credential, keeping the command hardening in one place so a caller that
/// pins one credential context cannot grow a second, weaker environment
/// builder. (The approver-authority probe that motivated this split was
/// deleted at #1583.)
pub(in crate::gh_ops) fn build_gh_command_for_resolved_credential(
    gh_path: &str,
    token: Option<&str>,
) -> Command {
    let mut cmd = Command::new(gh_path);
    cmd.env_clear();

    preserve_gh_env(&mut cmd);
    if let Some(token) = token {
        cmd.env("GH_TOKEN", token);
    }
    cmd.env("GH_PROMPT_DISABLED", "1");
    cmd.env("NO_COLOR", "1");

    cmd
}

/// Strict JSON reads retain the existing success/exit-code contract. Callers
/// that must preserve restrictions from partial failures opt into the typed
/// observed-error path; neither path treats failed execution as successful.
pub(in crate::gh_ops) async fn run_gh_json_bounded(
    server: &MemoryServer,
    args: Vec<String>,
    timeout: Duration,
    context: &str,
) -> Result<Value, String> {
    run_gh_json_observed_bounded(server, args, timeout, context)
        .await
        .map_err(|failure| failure.into_reason())
}

#[cfg(test)]
pub(crate) fn reset_github_command_runner_call_count() {
    GITHUB_COMMAND_RUNNER_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
pub(crate) fn github_command_runner_call_count() -> u64 {
    GITHUB_COMMAND_RUNNER_CALLS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn note_github_command_runner_call() {
    GITHUB_COMMAND_RUNNER_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_repo_accepts_canonical_github_names() {
        for repo in [
            "owner/repo",
            "owner-2/repo_name.rs",
            "a/.github",
            "OWNER/Repo-123",
        ] {
            validate_repo(repo).unwrap_or_else(|error| panic!("{repo}: {error}"));
        }
    }

    #[test]
    fn validate_repo_rejects_option_control_and_path_shapes() {
        for repo in [
            "--repo/name",
            "owner/--help",
            "owner/repo\nnext",
            "owner/repo\0next",
            "owner/../repo",
            "owner/..",
            "./repo",
            "owner/repo/extra",
            "owner\\repo/name",
            "owner /repo",
            "owner/repo name",
            "owner_/repo",
            "owner--name/repo",
            "/repo",
            "owner/",
        ] {
            assert!(validate_repo(repo).is_err(), "must reject {repo:?}");
        }
    }

    #[test]
    fn attach_body_file_writes_verbatim_body() {
        let body = "line one\nline two\n\"quoted\"";
        let mut call = GhCall::mutation();
        call.args(["issue", "comment", "1"]);
        call.attach_body_file(body).expect("attach body file");
        let args: Vec<String> = call
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[3], "--body-file");
        let written = fs::read_to_string(&args[4]).expect("read body tempfile");
        assert_eq!(written, body);
        drop(call);
        assert!(
            !std::path::Path::new(&args[4]).exists(),
            "dropping the call removes its body file"
        );
    }

    /// The name must be unique per call, not per microsecond — a burst of `gh`
    /// calls on several threads used to mint identical `pid-nanos` names and
    /// lose the `create_new`.
    #[test]
    fn gh_body_temp_paths_are_unique_within_one_process() {
        const THREADS: usize = 4;
        const PER_THREAD: usize = 500;

        let mut workers = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            workers.push(std::thread::spawn(|| {
                (0..PER_THREAD)
                    .map(|_| gh_body_temp_path().expect("mint gh body temp path"))
                    .collect::<Vec<_>>()
            }));
        }

        let mut minted = Vec::with_capacity(THREADS * PER_THREAD);
        for worker in workers {
            minted.extend(worker.join().expect("join minting thread"));
        }
        let unique: std::collections::BTreeSet<_> = minted.iter().cloned().collect();

        assert_eq!(
            unique.len(),
            minted.len(),
            "gh body temp paths collided: {} unique out of {}",
            unique.len(),
            minted.len()
        );

        // Also test concurrent production file creation directly:
        // verify openat/fchmod/guard creation never collides and all files exist simultaneously
        const CONCURRENT_FILES_THREADS: usize = 4;
        const FILES_PER_THREAD: usize = 25;
        let mut file_workers = Vec::with_capacity(CONCURRENT_FILES_THREADS);
        for i in 0..CONCURRENT_FILES_THREADS {
            file_workers.push(std::thread::spawn(move || {
                let mut guards = Vec::with_capacity(FILES_PER_THREAD);
                for j in 0..FILES_PER_THREAD {
                    let guard = write_gh_body_file(&format!("payload-{i}-{j}"))
                        .expect("write concurrent gh body file");
                    assert!(guard.0.exists(), "tempfile exists while guard is held");
                    guards.push(guard);
                }
                guards
            }));
        }

        let mut all_guards = Vec::with_capacity(CONCURRENT_FILES_THREADS * FILES_PER_THREAD);
        for worker in file_workers {
            all_guards.extend(worker.join().expect("join file worker thread"));
        }
        let all_paths: std::collections::BTreeSet<_> =
            all_guards.iter().map(|g| g.0.clone()).collect();
        assert_eq!(
            all_paths.len(),
            all_guards.len(),
            "production body file paths collided: {} unique out of {}",
            all_paths.len(),
            all_guards.len()
        );
        for guard in &all_guards {
            assert!(
                guard.0.exists(),
                "all concurrent tempfiles exist simultaneously"
            );
        }
        let guard_paths: Vec<_> = all_guards.iter().map(|g| g.0.clone()).collect();
        drop(all_guards);
        for path in guard_paths {
            assert!(!path.exists(), "concurrent tempfile cleaned up after drop");
        }
    }

    #[test]
    fn sanitize_output_redacts_repeated_headers_and_trailing_unterminated_tokens() {
        let primary_token = "ghp_primary111111111111111111111111111111";
        let secondary_token = "ghp_secondary22222222222222222222222222222";
        let tertiary_token = "ghp_tertiary33333333333333333333333333333";

        // 1. Trailing unterminated header without \r or \n
        let unterminated = format!("error: Authorization: Bearer {secondary_token}");
        let sanitized_unterminated = sanitize_output(&unterminated, primary_token);
        assert!(
            !sanitized_unterminated.contains(secondary_token),
            "unterminated secondary token leaked: {sanitized_unterminated}"
        );

        // 2. Multiple headers with distinct tokens
        let multiple = format!(
            "request 1: Authorization: Bearer {secondary_token}\n\
             request 2: Authorization: Bearer {tertiary_token}\n\
             request 3: x-github-token: {primary_token}\n"
        );
        let sanitized_multiple = sanitize_output(&multiple, primary_token);
        assert!(
            !sanitized_multiple.contains(primary_token),
            "primary token leaked: {sanitized_multiple}"
        );
        assert!(
            !sanitized_multiple.contains(secondary_token),
            "first secondary token leaked: {sanitized_multiple}"
        );
        assert!(
            !sanitized_multiple.contains(tertiary_token),
            "second secondary token leaked: {sanitized_multiple}"
        );

        // 3. Whitespace variations and literal tabs
        let whitespace = format!("header: Authorization:   Bearer   {secondary_token}");
        let sanitized_whitespace = sanitize_output(&whitespace, primary_token);
        assert!(
            !sanitized_whitespace.contains(secondary_token),
            "token with whitespace leaked: {sanitized_whitespace}"
        );

        let tabbed = format!("header: Authorization:\tBearer\t{secondary_token}\trequest_id=42");
        assert_eq!(
            sanitize_output(&tabbed, primary_token),
            "header: [REDACTED]\trequest_id=42",
            "tabs and trailing request_id must be preserved"
        );

        // 4. Trailing ordinary text and punctuation preservation
        let trailing_bearer =
            format!("before Authorization: Bearer {secondary_token} request_id=42");
        assert_eq!(
            sanitize_output(&trailing_bearer, primary_token),
            "before [REDACTED] request_id=42",
            "trailing text after Bearer header must be preserved"
        );

        let basic_auth = "before Authorization: Basic dXNlcjpwYXNz request_id=42";
        assert_eq!(
            sanitize_output(basic_auth, primary_token),
            "before [REDACTED] request_id=42",
            "Basic authorization credentials must be redacted while preserving trailing text"
        );

        let x_header_trailing =
            format!("before x-github-token: {secondary_token}; public status: 403");
        assert_eq!(
            sanitize_output(&x_header_trailing, primary_token),
            "before [REDACTED]; public status: 403",
            "trailing text after x-github-token must be preserved"
        );

        // 5. Non-auth mentions of header names must not be falsely redacted
        let mention = "this explains x-github-token behavior without a credential";
        assert_eq!(
            sanitize_output(mention, primary_token),
            "this explains x-github-token behavior without a credential",
            "prose mentioning x-github-token without a credential must not be redacted"
        );

        // 6. Multiple distinct headers on a single line
        let single_line_multi = format!(
            "x-github-token: {secondary_token}; x-access-token: {tertiary_token}; done=true"
        );
        assert_eq!(
            sanitize_output(&single_line_multi, primary_token),
            "[REDACTED]; [REDACTED]; done=true",
            "multiple headers on one line must all be redacted without losing trailing syntax"
        );

        // 7. Non-UTF8 / lossy output with real non-utf8 byte sequence
        let lossy_from_raw = String::from_utf8_lossy(
            &[
                b"lossy bytes \xff\xfe Bearer ",
                secondary_token.as_bytes(),
                b"\nstatus: 200",
            ]
            .concat(),
        )
        .to_string();
        let sanitized_lossy = sanitize_output(&lossy_from_raw, primary_token);
        assert!(
            !sanitized_lossy.contains(secondary_token),
            "lossy token leaked: {sanitized_lossy}"
        );
        assert!(
            sanitized_lossy.contains("lossy bytes \u{FFFD}\u{FFFD} "),
            "surrounding lossy text must be preserved"
        );
        assert!(
            sanitized_lossy.contains("status: 200"),
            "subsequent lines after lossy token must be preserved"
        );

        // 8. Verbatim preservation of non-auth content across multiple lines
        let multiline = format!(
            "Line 1: init ok\n\
             Authorization: Bearer {secondary_token}\n\
             Line 3: processing data\n\
             Authorization: token {tertiary_token}\n\
             Line 5: finished"
        );
        let sanitized_multiline = sanitize_output(&multiline, primary_token);
        assert_eq!(
            sanitized_multiline,
            "Line 1: init ok\n\
             [REDACTED]\n\
             Line 3: processing data\n\
             [REDACTED]\n\
             Line 5: finished",
            "non-sensitive surrounding lines must be preserved without loss"
        );

        // 9. Empty header with LF and CRLF must not delete subsequent status lines
        let empty_bearer_lf = "before\nAuthorization: Bearer\npublic status: 403\nafter";
        assert_eq!(
            sanitize_output(empty_bearer_lf, primary_token),
            "before\n[REDACTED]\npublic status: 403\nafter",
            "empty Bearer header must not consume subsequent LF line"
        );

        let empty_x_header_crlf = "before\r\nx-github-token:\r\npublic status: 403\r\nafter";
        assert_eq!(
            sanitize_output(empty_x_header_crlf, primary_token),
            "before\r\n[REDACTED]\r\npublic status: 403\r\nafter",
            "empty x-github-token must not consume subsequent CRLF line"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_gh_body_file_creates_private_0600_mode_under_permissive_umask() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        struct TestUmaskGuard(libc::mode_t);
        impl TestUmaskGuard {
            fn set(mode: libc::mode_t) -> Self {
                let prev = unsafe { libc::umask(mode) };
                Self(prev)
            }
        }
        impl Drop for TestUmaskGuard {
            fn drop(&mut self) {
                unsafe { libc::umask(self.0) };
            }
        }

        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _umask = TestUmaskGuard::set(0o000);

        let mut call = GhCall::mutation();
        call.attach_body_file("secret body contents")
            .expect("attach body file");
        let args: Vec<String> = call
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(args[0], "--body-file");
        let path = std::path::Path::new(&args[1]);
        assert!(path.exists(), "tempfile exists while call is in-flight");

        let meta = std::fs::symlink_metadata(path).expect("stat tempfile");
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "body file must be mode 0600 even under permissive umask"
        );

        let parent = path.parent().expect("tempfile parent dir");
        let parent_meta = std::fs::symlink_metadata(parent).expect("stat parent dir");
        assert_eq!(
            parent_meta.uid(),
            unsafe { libc::geteuid() },
            "parent dir must be owned by current user"
        );
        assert_eq!(
            parent_meta.permissions().mode() & 0o777,
            0o700,
            "parent dir must be mode 0700"
        );

        drop(call);
        assert!(!path.exists(), "body file cleaned up after call drop");
    }

    #[cfg(unix)]
    #[test]
    fn gh_body_temp_dir_in_fails_closed_on_symlink_or_invalid_parent() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("create test sandbox");
        let uid = unsafe { libc::geteuid() };
        let target_dir_name = format!("tachi-gh-body-{uid}");
        let fake_target = temp.path().join(&target_dir_name);

        // 1. Regular file at expected directory path -> must fail closed
        std::fs::write(&fake_target, b"not a directory").expect("write fake file");
        let err = gh_body_temp_dir_in(temp.path()).expect_err("must reject regular file as dir");
        assert!(
            err.to_lowercase().contains("not a directory"),
            "expected 'not a directory' error, got: {err}"
        );
        std::fs::remove_file(&fake_target).expect("remove fake file");

        // 2. Symlink at expected directory path -> must fail closed
        let other_dir = temp.path().join("other_dir");
        std::fs::create_dir(&other_dir).expect("create other dir");
        std::os::unix::fs::symlink(&other_dir, &fake_target).expect("create symlink");
        let err = gh_body_temp_dir_in(temp.path()).expect_err("must reject symlink as dir");
        assert!(
            err.contains("is a symlink")
                || err.to_lowercase().contains("symbolic links")
                || err.contains("Too many levels")
                || err.to_lowercase().contains("not a directory"),
            "expected symlink rejection error, got: {err}"
        );
        std::fs::remove_file(&fake_target).expect("remove symlink");

        // 3. Permissive existing directory -> repaired to 0700
        std::fs::create_dir(&fake_target).expect("create dir");
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        std::fs::set_permissions(&fake_target, std::fs::Permissions::from_mode(0o777))
            .expect("chmod 777");
        let dir = gh_body_temp_dir_in(temp.path()).expect("repair permissive dir");
        let meta = std::fs::symlink_metadata(&dir).expect("stat repaired dir");
        assert_eq!(meta.uid(), uid);
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        std::fs::remove_dir(&fake_target).expect("clean up repaired dir");

        // 4. Normal creation -> creates 0700 dir
        let created = gh_body_temp_dir_in(temp.path()).expect("create private dir");
        let meta = std::fs::symlink_metadata(&created).expect("stat created dir");
        assert_eq!(meta.uid(), uid);
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);

        // 5. Foreign owner rejection on private directory descriptor
        let test_foreign_dir = temp.path().join("foreign_owner_dir");
        std::fs::create_dir(&test_foreign_dir).expect("create test foreign dir");
        let foreign_fd = unsafe {
            use std::os::unix::ffi::OsStrExt;
            let c_path =
                std::ffi::CString::new(test_foreign_dir.as_os_str().as_bytes()).expect("no nul");
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        assert!(foreign_fd >= 0, "open test foreign dir descriptor");
        let foreign_res =
            validate_and_harden_gh_dir_fd(foreign_fd, &test_foreign_dir, uid.wrapping_add(1));
        unsafe { libc::close(foreign_fd) };
        let err = foreign_res.expect_err("must reject foreign owner UID");
        assert!(
            err.contains("owned by uid") && err.contains("expected current uid"),
            "expected foreign owner error, got: {err}"
        );

        // 6. Descriptor-bound unlinkat cleans up even if parent directory path is renamed
        let sub_base = temp.path().join("sub_base");
        std::fs::create_dir(&sub_base).expect("create sub base");
        let (dir_path, dir_fd) = open_private_gh_dir_in(&sub_base).expect("open sub dir");
        let leaf = mint_gh_body_file_name().expect("mint leaf");
        let file_path = dir_path.join(&leaf);
        let c_leaf = std::ffi::CString::new(leaf.as_bytes()).expect("no nul");
        use std::os::unix::io::AsRawFd;
        let fd = unsafe {
            libc::openat(
                dir_fd.as_raw_fd(),
                c_leaf.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                0o600,
            )
        };
        assert!(fd >= 0, "create file inside dir fd");
        unsafe { libc::close(fd) };
        assert!(file_path.exists());
        let guard = GhBodyFileGuard(file_path.clone(), dir_fd, c_leaf);
        // Rename the parent directory path on the filesystem
        let renamed_dir = sub_base.join("renamed_parent");
        std::fs::rename(&dir_path, &renamed_dir).expect("rename parent directory");
        assert!(!dir_path.exists(), "original path no longer exists");
        drop(guard);
        // The file inside the renamed directory should be cleanly unlinked via the descriptor!
        assert!(
            !renamed_dir.join(&leaf).exists(),
            "file was unlinked via retained directory fd"
        );

        // 7. Write failure through write_gh_body_file_in: asserts Err and that the created file is removed
        let target_dir = temp.path().join(format!("tachi-gh-body-{}", uid));
        let err = write_gh_body_file_in(temp.path(), "partial payload", |file, _| {
            use std::io::Write;
            file.write_all(b"partial")
                .map_err(|e| format!("write: {e}"))?;
            Err("injected disk full error during write".to_string())
        })
        .expect_err("write_gh_body_file_in must return Err on writer failure");
        assert!(err.contains("injected disk full error"));

        // Verify that NO partial file was left behind in the directory!
        let remaining_files = std::fs::read_dir(&target_dir)
            .expect("read target dir")
            .filter_map(Result::ok)
            .count();
        assert_eq!(
            remaining_files, 0,
            "partial file must be removed when write fails"
        );
    }
}
