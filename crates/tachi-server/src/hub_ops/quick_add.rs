//! `hub_quick_add` — composite tool: `hub_register` + (optional) `hub_review`.
//!
//! Registration already activates approved skills, so `auto_approve` only
//! runs the approve+enable review step for a registration that came back
//! disabled.

use crate::hub_ops::register::handle_hub_register;
use crate::hub_ops::review::handle_hub_review;
use crate::tool_params::{HubQuickAddParams, HubRegisterParams, HubReviewParams};
use crate::MemoryServer;
use serde_json::json;

pub(crate) async fn handle_hub_quick_add(
    server: &MemoryServer,
    params: HubQuickAddParams,
) -> Result<String, String> {
    // Step 1 — register, exactly as `hub_register` would.
    let register_params = HubRegisterParams {
        id: params.id.clone(),
        cap_type: params.cap_type.clone(),
        name: params.name.clone(),
        description: params.description.clone(),
        definition: params.definition.clone(),
        version: params.version,
        scope: params.scope.clone(),
    };
    let register_body = handle_hub_register(server, register_params).await?;
    let register_json: serde_json::Value = serde_json::from_str(&register_body)
        .map_err(|e| format!("parse register response: {e}"))?;

    let mut resp = serde_json::Map::new();
    resp.insert("register".into(), register_json.clone());

    // Step 2 — decide whether to auto-approve.
    let already_enabled = register_json
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let requested = params.auto_approve;

    if !requested {
        resp.insert("auto_approve".into(), json!("not_requested"));
    } else if already_enabled {
        // Skill / plugin — register already activated it.
        resp.insert("auto_approve".into(), json!("already_enabled"));
    } else {
        let review_params = HubReviewParams {
            id: params.id.clone(),
            review_status: "approved".to_string(),
            enabled: Some(true),
        };
        let review_body = handle_hub_review(server, review_params).await?;
        let review_json: serde_json::Value = serde_json::from_str(&review_body)
            .map_err(|e| format!("parse review response: {e}"))?;
        resp.insert("review".into(), review_json);
        resp.insert("auto_approve".into(), json!("applied"));
    }

    serde_json::to_string(&serde_json::Value::Object(resp)).map_err(|e| format!("serialize: {e}"))
}
