use serde_json::json;

use crate::status_ops::DbStatus;

fn agent_mcp_config_paths(home: &std::path::Path) -> Vec<(&'static str, std::path::PathBuf)> {
    vec![
        ("claude-code", home.join(".claude").join(".mcp.json")),
        (
            "claude-desktop",
            home.join("Library/Application Support/Claude/claude_desktop_config.json"),
        ),
        ("cursor", home.join(".cursor").join("mcp.json")),
        ("gemini", home.join(".gemini").join("mcp.json")),
        (
            "gemini-settings",
            home.join(".gemini").join("settings.json"),
        ),
        (
            "gemini-config",
            home.join(".gemini").join("config").join("mcp_config.json"),
        ),
        (
            "antigravity",
            home.join(".gemini")
                .join("antigravity")
                .join("mcp_config.json"),
        ),
        (
            "antigravity-ide",
            home.join(".gemini")
                .join("antigravity-ide")
                .join("mcp_config.json"),
        ),
        ("codex", home.join(".codex").join("config.toml")),
        ("opencode", home.join(".config/opencode/opencode.json")),
        ("amp", home.join(".config/amp/settings.json")),
        (
            "amp-macos",
            home.join("Library/Application Support/Amp/settings.json"),
        ),
    ]
}

pub(crate) fn agent_readiness_json(
    app_home: &std::path::Path,
    snapshot: &crate::status_ops::StatusSnapshot,
) -> serde_json::Value {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let agent_rule_files = vec![
        ("claude", home.join(".claude").join("CLAUDE.md")),
        ("codex", home.join(".codex").join("AGENTS.md")),
        ("gemini", home.join(".gemini").join("GEMINI.md")),
    ];
    let rules: Vec<_> = agent_rule_files
        .into_iter()
        .map(|(agent, path)| {
            let installed = std::fs::read_to_string(&path)
                .ok()
                .is_some_and(|body| body.contains("BEGIN TACHI MEMORY RULES"));
            json!({
                "agent": agent,
                "path": path.display().to_string(),
                "exists": path.exists(),
                "tachi_rules_installed": installed,
            })
        })
        .collect();
    let mcp_configs = agent_mcp_config_paths(&home);
    let mcp: Vec<_> = mcp_configs
        .into_iter()
        .map(|(agent, path)| {
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            json!({
                "agent": agent,
                "path": path.display().to_string(),
                "exists": path.exists(),
                "mentions_tachi": raw.to_ascii_lowercase().contains("tachi") || raw.contains("tachi-server"),
                "uses_no_project_db": raw.contains("--no-project-db"),
                "pins_memory_db_path": raw.contains("MEMORY_DB_PATH"),
                "uses_local_http_daemon": raw.contains("127.0.0.1:6919/mcp"),
            })
        })
        .collect();
    json!({
        "rules": rules,
        "mcp_configs": mcp,
        "runs_root": crate::task_lifecycle::flow_runs_root().display().to_string(),
        "last_distill_marker": snapshot.distill_marker.as_ref().map(|m| m.path.clone()),
        "last_distill": snapshot.distill_marker.as_ref().and_then(|m| {
            // Only emit the quality block for new-format (JSON) markers; legacy
            // bare-timestamp markers leave every field None.
            (m.groups_distilled.is_some()
                || m.groups_skipped.is_some()
                || m.fallback_used.is_some()
                || m.errors.is_some())
            .then(|| json!({
                "groups_distilled": m.groups_distilled,
                "groups_skipped": m.groups_skipped,
                "fallback_used": m.fallback_used,
                "errors": m.errors,
            }))
        }),
        "doctor_hint": format!("tachi doctor --jobs --probe-keys --roots {}", shell_quote(&app_home.display().to_string())),
    })
}

pub(crate) fn format_backfill_command(db: &DbStatus) -> String {
    format!("tachi backfill-vectors --db {}", shell_quote(&db.path))
}

pub(crate) fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '='))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
