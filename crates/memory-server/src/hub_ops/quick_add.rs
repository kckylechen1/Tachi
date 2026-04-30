//! `hub_quick_add` — composite tool: `hub_register` + (optional) `hub_review`.
//!
//! Motivation: agents commonly need to register a capability AND immediately
//! activate it. Doing this with two separate tool calls is awkward and races
//! against the trusted-command allowlist gate (an agent might forget the
//! follow-up `hub_review` call, leaving the capability stranded as `pending`).
//!
//! Safety boundary (PR6, non-negotiable):
//!   * `auto_approve` is honored ONLY when the underlying `hub_register` reports
//!     `auto_approval_eligible: true`. That flag is set true by `hub_register`
//!     only when:
//!       - cap_type != "mcp" (skills/plugins are governance-approved by default), OR
//!       - cap_type == "mcp" AND transport != "stdio" (no local exec risk), OR
//!       - cap_type == "mcp" AND transport == "stdio" AND `is_trusted_command(cmd)`
//!         returns true (i.e. the command is in the explicit allowlist).
//!   * For an UNTRUSTED stdio MCP command, `auto_approve` is silently ignored
//!     and a warning is appended to the response. The capability is left in
//!     `pending` review state exactly as plain `hub_register` would leave it.
//!     This preserves the allowlist as the single source of trust authority.
//!
//! For skills (always `auto_approval_eligible=true`), `auto_approve` is a no-op
//! because `hub_register` already marks them `approved`+`enabled`.

use super::*;
use crate::hub_ops::register::handle_hub_register;
use crate::hub_ops::review::handle_hub_review;
use crate::tool_params::{HubQuickAddParams, HubRegisterParams, HubReviewParams};

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
    let eligible = register_json
        .get("auto_approval_eligible")
        .and_then(|v| v.as_bool())
        .unwrap_or(true); // non-MCP types omit this field → eligible
    let requested = params.auto_approve;

    if !requested {
        resp.insert("auto_approve".into(), json!("not_requested"));
    } else if already_enabled {
        // Skill / plugin — register already activated it.
        resp.insert("auto_approve".into(), json!("already_enabled"));
    } else if !eligible {
        // Untrusted stdio MCP command — refuse to escalate.
        resp.insert("auto_approve".into(), json!("refused_untrusted"));
        append_warning(
            &mut resp,
            "auto_approve was requested but the underlying command is not in the trusted allowlist. \
             Capability remains in pending review. Use hub_review explicitly after manual inspection.",
        );
    } else {
        // Trusted MCP (or non-stdio MCP): run the review/approve step.
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
