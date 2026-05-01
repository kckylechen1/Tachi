//! MCP handlers for doctor v2. Read-only only — all mutations are CLI-only.

use crate::doctor::{default_scan_roots, scan, ScanOptions};
use serde_json::to_string_pretty;

/// `tachi_doctor_scan` — scan default roots, no auto-fix, return JSON report.
pub(crate) async fn handle_tachi_doctor_scan() -> Result<String, String> {
    let home = dirs::home_dir().ok_or_else(|| "home dir not found".to_string())?;
    let app_home = home.join(".tachi");
    let git_root = std::env::current_dir().ok();
    let roots = default_scan_roots(&home, git_root.as_deref());
    let quarantine_dir = app_home.join("quarantine");
    let opts = ScanOptions {
        auto_fix: false,
        max_depth: 10,
    };
    let report = scan(&roots, &quarantine_dir, opts);
    to_string_pretty(&report).map_err(|e| format!("serialize report failed: {e}"))
}
