use std::path::{Path, PathBuf};

use serde_json::json;

use crate::cli::WatcherAction;

pub(crate) async fn run_watcher(
    action: WatcherAction,
    global_db_path: &Path,
    project_db_path: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = crate::MemoryServer::new(global_db_path.to_path_buf(), project_db_path)?;
    match action {
        WatcherAction::Status { json: json_out } => {
            let watcher = crate::facade_memory_ops::claude_jsonl_passive_watcher_status();
            if json_out {
                println!("{}", serde_json::to_string_pretty(&watcher)?);
            } else {
                println!(
                    "passive watcher: {}",
                    watcher["status"].as_str().unwrap_or("unknown")
                );
                if let Some(path) = watcher.get("latest_jsonl").and_then(|v| v.as_str()) {
                    println!("  latest_jsonl: {path}");
                }
                if let Some(note) = watcher.get("note").and_then(|v| v.as_str()) {
                    println!("  note: {note}");
                }
            }
            Ok(())
        }
        WatcherAction::CaptureLatest { json: json_out } => {
            let captured =
                crate::facade_memory_ops::capture_latest_claude_jsonl_checkpoint(&server)
                    .await?
                    .unwrap_or_else(|| json!({"status":"not_detected"}));
            if json_out {
                println!("{}", serde_json::to_string_pretty(&captured)?);
            } else {
                println!(
                    "passive watcher capture: {}",
                    captured["status"].as_str().unwrap_or("unknown")
                );
                if let Some(path) = captured.get("path").and_then(|v| v.as_str()) {
                    println!("  source: {path}");
                }
            }
            Ok(())
        }
    }
}
