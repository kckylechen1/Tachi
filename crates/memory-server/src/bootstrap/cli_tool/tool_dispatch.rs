use crate::server_state::MemoryServer;
use serde_json::json;
use std::path::PathBuf;

use super::super::print_pretty_json;

/// Pretty-print a tool's JSON-string output. Falls back to raw text if the
/// result is not valid JSON (defensive: handlers always return JSON today).
pub(super) fn print_cli_tool_result(body: &str) -> Result<(), Box<dyn std::error::Error>> {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => print_pretty_json(&v),
        Err(_) => {
            println!("{body}");
            Ok(())
        }
    }
}

fn cli_tool_allows_read_fallback(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "search_memory"
            | "list_memories"
            | "get_memory"
            | "tachi_search"
            | "wiki_search"
            | "tachi_wiki_search"
            | "vault_status"
            | "vault_list"
    )
}

/// Dispatch a CLI tool invocation: try the running daemon first; on miss,
/// build a transient in-process MemoryServer and call the handler directly.
/// Either path returns the tool's JSON string body.
pub(super) async fn dispatch_cli_tool<F, Fut>(
    tool_name: &str,
    args: serde_json::Map<String, serde_json::Value>,
    global_db: &PathBuf,
    project_db: Option<&PathBuf>,
    app_home: &PathBuf,
    in_process: F,
) -> Result<String, Box<dyn std::error::Error>>
where
    F: FnOnce(MemoryServer, serde_json::Map<String, serde_json::Value>) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let read_fallback = cli_tool_allows_read_fallback(tool_name);
    if let Some(info) = crate::cli_client::detect_daemon_for_global_db(app_home, global_db).await {
        if crate::cli_client::daemon_matches_requested_dbs(
            &info,
            global_db,
            project_db.map(|path| path.as_path()),
        ) {
            match crate::cli_client::call_daemon_tool(&info, tool_name, args.clone()).await {
                Ok(body) => return Ok(body),
                Err(e) if read_fallback || e.allows_in_process_fallback() => {
                    eprintln!(
                        "[cli] daemon dispatch failed ({e}); falling back to in-process execution"
                    );
                }
                Err(e) => return Err(format!(
                    "daemon dispatch failed after dispatch; refusing in-process fallback to avoid duplicate writes: {e}"
                ).into()),
            }
        } else if crate::cli_client::daemon_global_db_matches(&info, global_db) {
            if let Some(named_project) =
                project_db.and_then(|path| crate::path_utils::named_project_for_db_path(path))
            {
                let mut daemon_args = args.clone();
                daemon_args
                    .entry("project".to_string())
                    .or_insert_with(|| json!(named_project.clone()));
                eprintln!(
                    "[cli] daemon project DB scope differs; forwarding via named project '{named_project}'"
                );
                match crate::cli_client::call_daemon_tool(&info, tool_name, daemon_args).await {
                    Ok(body) => return Ok(body),
                    Err(e) if read_fallback || e.allows_in_process_fallback() => {
                        eprintln!(
                            "[cli] daemon named-project dispatch failed ({e}); falling back to in-process execution"
                        );
                    }
                    Err(e) => return Err(format!(
                        "daemon named-project dispatch failed after dispatch; refusing in-process fallback to avoid duplicate writes: {e}"
                    ).into()),
                }
            } else {
                eprintln!(
                    "[cli] daemon DB scope differs from requested CLI scope; executing in-process"
                );
            }
        } else {
            eprintln!(
                "[cli] foreign daemon global_db={:?}; executing in-process",
                info.global_db
            );
        }
    }
    let server = crate::cli_client::build_in_process_server(global_db, project_db)?;
    let body = in_process(server, args)
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::cli_tool_allows_read_fallback;

    #[test]
    fn read_only_cli_wrappers_allow_daemon_fallback() {
        for tool_name in [
            "search_memory",
            "list_memories",
            "get_memory",
            "tachi_search",
            "wiki_search",
            "tachi_wiki_search",
            "vault_status",
            "vault_list",
        ] {
            assert!(
                cli_tool_allows_read_fallback(tool_name),
                "{tool_name} should be safe to run in-process when daemon forwarding misses"
            );
        }
        assert!(!cli_tool_allows_read_fallback("save_memory"));
        assert!(!cli_tool_allows_read_fallback("tachi_wiki_write"));
    }
}
