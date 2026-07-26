use crate::{DbScope, MemoryServer};

/// Format a save-path error string. When the underlying SQLite error indicates
/// a readonly database, attach the resolved DB path, the active scope/profile,
/// and a concrete remediation hint. Non-readonly errors fall through to the
/// previous one-line format so existing callers (and tests) keep working.
pub(in crate::memory_search_ops::save_memory) fn format_save_error(
    server: &MemoryServer,
    target_db: DbScope,
    named_project: Option<&str>,
    err: &dyn std::fmt::Display,
) -> String {
    let err_str = err.to_string();
    let lower = err_str.to_ascii_lowercase();
    let is_readonly = lower.contains("readonly")
        || lower.contains("read-only")
        || lower.contains("read only")
        || lower.contains("attempt to write a readonly database");

    if !is_readonly {
        return match named_project {
            Some(name) => format!("Failed to save memory to '{}': {}", name, err_str),
            None => format!("Failed to save memory: {}", err_str),
        };
    }

    let db_path = match named_project {
        Some(name) => server
            .resolve_server_named_project_db_path(name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| format!("<named project: {name}>")),
        None => match target_db {
            DbScope::Global => server.global_db_path_buf().display().to_string(),
            DbScope::Project => server
                .project_db_path_buf()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<no project DB configured>".to_string()),
        },
    };

    let profile_label = server
        .active_tool_profile()
        .map(|p| p.as_str())
        .unwrap_or_else(|| "admin".to_string());

    format!(
        "Failed to save memory: database is read-only.\n  \
         db_path: {db_path}\n  \
         scope: {scope}\n  \
         profile: {profile_label}\n  \
         hints:\n    \
         - Another process may hold an exclusive lock; check for stale `tachi` daemons.\n    \
         - File permissions may be wrong; ensure the user owns the DB file and parent dir.\n    \
         - The DB may have been opened read-only by an earlier CLI command — restart the daemon.\n    \
         - If targeting the wrong DB, pass --global-db / --project-db (or `project=` on the call).\n  \
         underlying: {err_str}",
        scope = target_db.as_str(),
    )
}
