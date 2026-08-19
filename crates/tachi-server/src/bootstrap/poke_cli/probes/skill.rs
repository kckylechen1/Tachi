use crate::tool_params::TachiSkillParams;
use crate::MemoryServer;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

use super::util::collect_strings;

pub(in crate::bootstrap::poke_cli) async fn probe_skill_surface(
    server: &MemoryServer,
) -> Result<Value, String> {
    let discover_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "discover".to_string(),
            query: Some("waza check verification".to_string()),
            cap_type: Some("skill".to_string()),
            enabled_only: Some(true),
            limit: Some(12),
            skill_id: None,
            args: None,
        }))
        .await?;
    let discover: Value =
        serde_json::from_str(&discover_raw).map_err(|e| format!("parse skill discover: {e}"))?;
    let profile = crate::dispatch_profile::resolve_dispatch_profile("codex_55_review")
        .ok_or_else(|| "codex_55_review profile not found".to_string())?;
    let loadout = crate::dispatch_profile::profile_skill_loadout_json_for_server(server, profile)?;
    let skills = collect_strings(&loadout);
    let has_waza = skills.iter().any(|item| item.contains("skill:waza-"));
    let has_superpower = skills
        .iter()
        .any(|item| item.contains("skill:superpowers-"));
    if !has_waza || !has_superpower {
        return Err(format!(
            "skill surface missing builtin categories: has_waza={has_waza} has_superpower={has_superpower} loadout={loadout}"
        ));
    }
    Ok(json!({
        "name": "skill_surface",
        "status": "passed",
        "expected": "discover/list builtin skills and render a safe loadout contract",
        "observed": {
            "discover_count": discover.get("results").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "has_waza": has_waza,
            "has_superpowers": has_superpower,
            "loadout_profile": "codex_55_review",
        },
        "repro_steps": [
            "tachi_skill discover query='waza check verification'",
        ],
    }))
}
