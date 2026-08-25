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
            query: Some("check verify skill".to_string()),
            cap_type: Some("skill".to_string()),
            enabled_only: Some(true),
            limit: Some(24),
            skill_id: None,
            args: None,
        }))
        .await?;
    let discover: Value =
        serde_json::from_str(&discover_raw).map_err(|e| format!("parse skill discover: {e}"))?;
    let results = discover
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("skill discover missing results: {discover}"))?;
    if results.is_empty() {
        return Err(format!(
            "skill surface discover returned no callable skills: {discover}"
        ));
    }
    // #1690 C3: the loadout surface (static profile skill map) is retired; the
    // discover surface is the surviving static reviewed-skill list. The probe
    // asserts every listed row is a callable approved skill — no
    // recommendation/evolution intelligence behind it.
    for row in results {
        if row.get("callable").and_then(Value::as_bool) != Some(true) {
            return Err(format!("non-callable row in discover: {row}"));
        }
        if row.get("review_status").and_then(Value::as_str) != Some("approved") {
            return Err(format!("non-approved row in discover: {row}"));
        }
    }
    let skills = collect_strings(&discover);
    Ok(json!({
        "name": "skill_surface",
        "status": "passed",
        "expected": "discover lists the reviewed static skill surface (callable approved skills only)",
        "observed": {
            "discover_count": results.len(),
            "first_skills": skills.iter().take(5).collect::<Vec<_>>(),
        },
        "repro_steps": [
            "tachi_skill discover query='check verify skill'"
        ],
    }))
}
