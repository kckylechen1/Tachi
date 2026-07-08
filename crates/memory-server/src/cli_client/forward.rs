//! Daemon-forwarding policy for read and write tool calls.

use std::path::Path;

use crate::MemoryServer;

use super::{
    app_home_from_global_db, call_daemon_tool, daemon_matches_requested_dbs,
    daemon_version_matches, detect_daemon_for_global_db, is_daemon_process, DaemonCallError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForwardFallback {
    /// Writes must not be replayed locally after the daemon might have handled
    /// them, or agents can create duplicate memories/jobs.
    Write,
    /// Read/result tools do not create user-authored content, so the stdio
    /// adapter can fall back to its local handler when daemon transport fails.
    /// Search may duplicate access accounting in this rare path; that is
    /// preferable to failing recall outright.
    Read,
}

impl ForwardFallback {
    fn allows_in_process_fallback(self, error: &DaemonCallError) -> bool {
        match self {
            Self::Write => error.allows_in_process_fallback(),
            Self::Read => true,
        }
    }

    fn fallback_label(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Read => "read",
        }
    }
}

/// Forward a write tool to the running daemon when available.
/// Returns `Ok(Some(body))` on success and `Ok(None)` when no compatible daemon
/// was reached before dispatch. If a compatible daemon was reached and the
/// request outcome is unknown, returns `Err` so callers do not repeat writes
/// in-process after a possibly successful daemon-side commit.
pub(crate) async fn maybe_forward_server_write<T: serde::Serialize>(
    server: &MemoryServer,
    tool_name: &str,
    params: &T,
) -> Result<Option<String>, String> {
    let global_db_path = server.global_db_path_buf();
    let project_db_path = server.project_db_path_buf();
    maybe_forward_tool(
        global_db_path.as_path(),
        project_db_path.as_deref(),
        tool_name,
        params,
        ForwardFallback::Write,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn maybe_forward_write<T: serde::Serialize>(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    tool_name: &str,
    params: &T,
) -> Result<Option<String>, String> {
    maybe_forward_tool(
        global_db_path,
        project_db_path,
        tool_name,
        params,
        ForwardFallback::Write,
    )
    .await
}

/// Forward a read-only tool to the running daemon when available.
///
/// Unlike writes, reads can safely fall back to the in-process handler if the
/// daemon call fails after dispatch. This keeps old stdio adapters aligned with
/// daemon-side search/routing fixes while preserving local resilience.
pub(crate) async fn maybe_forward_server_read<T: serde::Serialize>(
    server: &MemoryServer,
    tool_name: &str,
    params: &T,
) -> Result<Option<String>, String> {
    let global_db_path = server.global_db_path_buf();
    let project_db_path = server.project_db_path_buf();
    maybe_forward_read(
        global_db_path.as_path(),
        project_db_path.as_deref(),
        tool_name,
        params,
    )
    .await
}

pub(crate) async fn maybe_forward_read<T: serde::Serialize>(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    tool_name: &str,
    params: &T,
) -> Result<Option<String>, String> {
    maybe_forward_tool(
        global_db_path,
        project_db_path,
        tool_name,
        params,
        ForwardFallback::Read,
    )
    .await
}

async fn maybe_forward_tool<T: serde::Serialize>(
    global_db_path: &Path,
    project_db_path: Option<&Path>,
    tool_name: &str,
    params: &T,
    fallback: ForwardFallback,
) -> Result<Option<String>, String> {
    if is_daemon_process() {
        return Ok(None);
    }
    let Some(args) = serde_json::to_value(params)
        .ok()
        .and_then(|value| value.as_object().cloned())
    else {
        eprintln!(
            "[mcp] failed to serialize daemon forward args for '{tool_name}'; executing in-process"
        );
        return Ok(None);
    };
    let app_home = app_home_from_global_db(global_db_path);
    let Some(info) = detect_daemon_for_global_db(&app_home, global_db_path).await else {
        return Ok(None);
    };
    if !daemon_version_matches(&info) {
        eprintln!(
            "[mcp] daemon version mismatch (daemon {:?}, binary {}); executing in-process",
            info.version,
            env!("CARGO_PKG_VERSION")
        );
        return Ok(None);
    }
    if !daemon_matches_requested_dbs(&info, global_db_path, project_db_path) {
        return Ok(None);
    }
    match call_daemon_tool(&info, tool_name, args, None).await {
        Ok(body) => Ok(Some(body)),
        Err(error) if fallback.allows_in_process_fallback(&error) => {
            eprintln!(
                "[mcp] daemon {} forward '{tool_name}' failed ({}); executing in-process",
                fallback.fallback_label(),
                error.message()
            );
            Ok(None)
        }
        Err(error) => Err(format!(
            "daemon forward '{tool_name}' failed after dispatch; refusing in-process fallback to avoid duplicate writes: {}",
            error.message()
        )),
    }
}
