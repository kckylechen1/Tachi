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
            limit: Some(24),
            skill_id: None,
            args: None,
        }))
        .await?;
    let discover: Value =
        serde_json::from_str(&discover_raw).map_err(|e| format!("parse skill discover: {e}"))?;
    let skills = collect_strings(&discover);
    let has_waza = skills.iter().any(|item| item.contains("skill:waza-"));
    let has_superpower = skills
        .iter()
        .any(|item| item.contains("skill:superpowers-"));
    if !has_waza || !has_superpower {
        return Err(format!(
            "skill surface missing builtin categories: has_waza={has_waza} has_superpower={has_superpower} discover={discover}"
        ));
    }
    Ok(json!({
        "name": "skill_surface",
        "status": "passed",
        "expected": "discover lists the reviewed static skill surface (builtin waza/superpowers categories)",
        "observed": {
            "discover_count": discover.get("results").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "has_waza": has_waza,
            "has_superpowers": has_superpower,
        },
        "repro_steps": [
            "tachi_skill discover query='waza check verification'"
        ],
    }))
}
