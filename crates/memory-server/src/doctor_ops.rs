//! MCP handlers for doctor v2. Read-only only — all mutations are CLI-only.

use crate::doctor::{default_scan_roots, scan, ScanOptions};
use serde_json::{json, to_string_pretty};

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
    let global_db_path = app_home.join("global").join("memory.db");
    let mut value =
        serde_json::to_value(&report).map_err(|e| format!("serialize report failed: {e}"))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "provider_keys".to_string(),
            json!({
                "keys": crate::status_ops::provider_key_status_json(&global_db_path),
                "probes": [],
                "probe_note": "MCP doctor scan is read-only; run `tachi doctor --probe-keys` for live provider probes."
            }),
        );
        obj.insert(
            "models".to_string(),
            json!({
                "lanes": crate::status_ops::model_lanes_json(),
                "probe_note": "run `tachi doctor --probe-keys` or `tachi status --probe-keys` to live-test lanes"
            }),
        );
    }
    to_string_pretty(&value).map_err(|e| format!("serialize report failed: {e}"))
}
