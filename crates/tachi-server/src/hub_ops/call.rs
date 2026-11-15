mod hub;

use crate::MemoryServer;
use serde_json::{json, Value};
use tachi_hub::{capability_not_callable_reason, skill_document_content};

pub(crate) use hub::handle_tachi_audit_log;

pub(crate) fn handle_run_skill(server: &MemoryServer, skill_id: &str) -> Result<String, String> {
    serde_json::to_string(&read_registered_skill_document(server, skill_id)?)
        .map_err(|e| format!("serialize skill document: {e}"))
}

/// The compatibility `run` action reads an explicitly declared workflow document.
/// Every call reloads the project-first definition, including dynamic tool calls.
pub(crate) fn read_registered_skill_document(
    server: &MemoryServer,
    skill_id: &str,
) -> Result<Value, String> {
    if crate::builtins::is_retired_builtin_capability_id(skill_id) {
        return Err(format!("Skill '{skill_id}' is retired and cannot be run."));
    }
    let cap = {
        let mut found = None;
        if server.has_project_db() {
            found = server.with_project_store(|store| {
                store
                    .hub_get(skill_id)
                    .map_err(|e| format!("hub get project: {e}"))
            })?;
        }
        if found.is_none() {
            found = server.with_global_store(|store| {
                store
                    .hub_get(skill_id)
                    .map_err(|e| format!("hub get global: {e}"))
            })?;
        }
        found.ok_or_else(|| format!("Skill '{skill_id}' not found in Hub"))?
    };

    if cap.cap_type != "skill" {
        return Err(format!(
            "'{}' is type '{}', not 'skill'",
            cap.id, cap.cap_type
        ));
    }
    if let Some(reason) = capability_not_callable_reason(&cap) {
        return Err(format!("Skill '{}' is not callable ({reason}).", cap.id));
    }

    let def: serde_json::Value = serde_json::from_str(&cap.definition)
        .map_err(|e| format!("invalid skill definition JSON: {e}"))?;

    let output = skill_document_content(&def).map_err(str::to_string)?;
    let _ = server.record_capability_call_outcome(&cap.id, true, None);
    Ok(json!({
        "status": "completed",
        "action": "run",
        "skill_id": skill_id,
        "execution": "document",
        "simulated": false,
        "output": output,
        "instruction": "This is a workflow document. Follow its steps yourself using your own tools; do not treat it as executed output.",
    }))
}
