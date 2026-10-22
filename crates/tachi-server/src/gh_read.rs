//! Hardened, bounded `gh` subprocess transport shared by every in-daemon
//! GitHub read (token resolution, `env_clear` command building, output
//! redaction, repo validation and the bounded executor).

use crate::vault_ops::read_unlocked_vault_secret;
use crate::MemoryServer;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

mod bounded;
mod bounded_json;
use bounded::GhCall;
use bounded_json::run_gh_json_bounded;

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

pub(crate) fn validate_repo(repo: &str) -> Result<(), String> {
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
pub(crate) fn resolve_gh_path() -> Result<String, String> {
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
pub(crate) fn sanitize_output(text: &str, token: &str) -> String {
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

pub(crate) fn vault_secret_unavailable(err: &str) -> bool {
    crate::vault_ops::is_env_fallback_eligible(crate::vault_ops::classify_vault_read_error(err))
}

pub(crate) fn env_gh_token() -> Option<String> {
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

pub(crate) fn resolve_gh_token(server: &MemoryServer) -> Result<Option<String>, String> {
    match read_unlocked_vault_secret(server, "GH_TOKEN", Some(GH_AGENT_ID), false) {
        Ok(token) => Ok(Some(token)),
        Err(err) if vault_secret_unavailable(&err) => Ok(env_gh_token()),
        Err(err) => Err(err),
    }
}

pub(crate) fn preserve_gh_env(cmd: &mut Command) {
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

/// Build a sanitized Command for `gh` with env_clear + vault token injection
pub(crate) fn build_gh_command(server: &MemoryServer) -> Result<(Command, String), String> {
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
pub(crate) fn build_gh_command_for_resolved_credential(
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

/// #1071: bound for read-only hot paths (the `tachi_memory(action='ask')`
/// current-work anchor) that must never hold their caller for long. A `gh`
/// invocation that stalls on network is killed and reaped at this bound and
/// the caller gets a plain `Err`, never a hang.
pub(crate) const ANCHOR_GH_TIMEOUT: Duration = Duration::from_secs(6);

/// Bounded single-issue read for the current-work anchor. Credential and
/// `gh` resolution run inside the same [`ANCHOR_GH_TIMEOUT`] bound.
pub(crate) async fn read_issue_snapshot_bounded(
    server: &MemoryServer,
    repo: &str,
    issue_number: u64,
) -> Result<Value, String> {
    validate_repo(repo)?;
    run_gh_json_bounded(
        server,
        vec![
            "issue".to_string(),
            "view".to_string(),
            issue_number.to_string(),
            "--repo".to_string(),
            repo.to_string(),
            "--json".to_string(),
            "number,title,state,body,labels,milestone,updatedAt,comments".to_string(),
        ],
        ANCHOR_GH_TIMEOUT,
        "gh issue view",
    )
    .await
}

/// `gh issue view` with the default read bound. Non-JSON stdout is returned
/// as a JSON string so callers fall back to their defaults.
pub(crate) async fn read_issue(
    server: &MemoryServer,
    repo: &str,
    issue_number: u64,
) -> Result<Value, String> {
    read_view(
        server,
        "issue",
        repo,
        issue_number,
        "number,title,state,body,author,labels,assignees,createdAt,updatedAt,comments,milestone",
    )
    .await
}

/// `gh pr view` with the default read bound.
pub(crate) async fn read_pr(
    server: &MemoryServer,
    repo: &str,
    pr_number: u64,
) -> Result<Value, String> {
    read_view(
        server,
        "pr",
        repo,
        pr_number,
        "number,title,state,body,author,labels,reviewDecision,mergeable,additions,deletions,changedFiles,headRefName,baseRefName,createdAt,updatedAt",
    )
    .await
}

async fn read_view(
    server: &MemoryServer,
    kind: &str,
    repo: &str,
    number: u64,
    fields: &str,
) -> Result<Value, String> {
    validate_repo(repo)?;
    let mut call = GhCall::read();
    call.args([kind, "view", &number.to_string()])
        .args(["--repo", repo])
        .args(["--json", fields]);
    let output = call.run(server).await?;
    Ok(serde_json::from_str::<Value>(&output).unwrap_or(Value::String(output)))
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
}
