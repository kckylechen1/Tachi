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
            profile: None,
            host: None,
            skill_limit: None,
            capability_limit: None,
            pack_limit: None,
            include_section: None,
        }))
        .await?;
    let discover: Value =
        serde_json::from_str(&discover_raw).map_err(|e| format!("parse skill discover: {e}"))?;
    let loadout_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: None,
            cap_type: None,
            enabled_only: None,
            limit: None,
            skill_id: None,
            args: None,
            profile: Some("codex_55_review".to_string()),
            host: Some("codex".to_string()),
            skill_limit: Some(8),
            capability_limit: Some(8),
            pack_limit: Some(4),
            include_section: Some(true),
        }))
        .await?;
    let loadout: Value =
        serde_json::from_str(&loadout_raw).map_err(|e| format!("parse skill loadout: {e}"))?;
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
            "tachi_skill loadout profile=codex_55_review host=codex"
        ],
    }))
}
