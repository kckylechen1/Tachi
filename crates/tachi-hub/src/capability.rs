use memcore::HubCapability;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityVisibility {
    Listed,
    Discoverable,
    Hidden,
}

impl CapabilityVisibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Listed => "listed",
            Self::Discoverable => "discoverable",
            Self::Hidden => "hidden",
        }
    }
}

impl std::str::FromStr for CapabilityVisibility {
    type Err = ();

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "listed" | "public" | "show" => Ok(Self::Listed),
            "discoverable" | "on_demand" | "on-demand" | "call_only" | "call-only" => {
                Ok(Self::Discoverable)
            }
            "hidden" | "private" | "off" => Ok(Self::Hidden),
            _ => Err(()),
        }
    }
}

pub fn capability_visibility_from_definition(def: &Value) -> CapabilityVisibility {
    let raw = def
        .get("policy")
        .and_then(|p| p.get("visibility"))
        .and_then(|v| v.as_str())
        .or_else(|| def.get("visibility").and_then(|v| v.as_str()));

    raw.and_then(|value| value.parse::<CapabilityVisibility>().ok())
        .unwrap_or(CapabilityVisibility::Listed)
}

pub fn capability_visibility_for_cap(cap: &HubCapability) -> CapabilityVisibility {
    match serde_json::from_str::<Value>(&cap.definition) {
        Ok(def) => capability_visibility_from_definition(&def),
        Err(e) => {
            eprintln!(
                "[hub] invalid capability definition JSON for '{}': {e}; defaulting visibility=listed",
                cap.id
            );
            CapabilityVisibility::Listed
        }
    }
}

pub fn should_expose_skill_tool(cap: &HubCapability) -> bool {
    cap.enabled
        && cap.cap_type.eq_ignore_ascii_case("skill")
        && capability_visibility_for_cap(cap) == CapabilityVisibility::Listed
}

pub fn should_expose_mcp_tools(cap: &HubCapability) -> bool {
    cap.enabled
        && cap.cap_type.eq_ignore_ascii_case("mcp")
        && capability_visibility_for_cap(cap) == CapabilityVisibility::Listed
}

pub fn review_status_allows_call(review_status: &str) -> bool {
    review_status.eq_ignore_ascii_case("approved")
}

pub fn health_status_allows_call(health_status: &str) -> bool {
    !health_status.eq_ignore_ascii_case("open")
}

pub fn capability_callable(cap: &HubCapability) -> bool {
    if !cap.enabled {
        return false;
    }
    if !review_status_allows_call(&cap.review_status) {
        return false;
    }
    if !health_status_allows_call(&cap.health_status) {
        return false;
    }
    if !cap.cap_type.eq_ignore_ascii_case("mcp") {
        return true;
    }
    match serde_json::from_str::<serde_json::Value>(&cap.definition) {
        // Backward-compat (owner-ratified option B, #968 regression fix): a
        // capability that already passed enabled + approved + healthy is trusted
        // even without a discovery_status stamp (existing pre-#968 caps + caps
        // registered outside the review-discovery flow). Only an EXPLICIT
        // non-"ready" discovery_status, a non-string value, or malformed JSON
        // fails closed.
        Ok(def) => match def.get("discovery_status") {
            None => true,
            Some(v) => v.as_str() == Some("ready"),
        },
        Err(_) => false,
    }
}

pub fn sanitize_skill_tool_name(skill_id: &str) -> Option<String> {
    let raw = skill_id.strip_prefix("skill:")?;
    let mut output = String::from("tachi_skill_");
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            output.push(c.to_ascii_lowercase());
        } else {
            output.push('_');
        }
    }
    while output.contains("__") {
        output = output.replace("__", "_");
    }
    Some(output.trim_end_matches('_').to_string())
}

pub fn make_text_tool_result(
    payload: &Value,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    let text = serde_json::to_string(payload)
        .map_err(|e| rmcp::ErrorData::internal_error(format!("serialize response: {e}"), None))?;
    serde_json::from_value(json!({
        "content": [{"type": "text", "text": text}],
        "isError": false
    }))
    .map_err(|e| rmcp::ErrorData::internal_error(format!("build MCP response: {e}"), None))
}

pub fn build_skill_tool_from_cap(
    cap: &HubCapability,
) -> Result<(String, rmcp::model::Tool), String> {
    let tool_name = sanitize_skill_tool_name(&cap.id)
        .ok_or_else(|| format!("Invalid skill id '{}'", cap.id))?;
    let def: Value = serde_json::from_str(&cap.definition)
        .map_err(|e| format!("Invalid skill definition JSON: {e}"))?;
    let input_schema = def
        .get("inputSchema")
        .cloned()
        .or_else(|| def.get("input_schema").cloned())
        .unwrap_or(json!({
            "type": "object",
            "properties": {
                "input": {"type": "string", "description": "Primary input for this skill"},
                "context": {"type": "string", "description": "Optional context"}
            },
            "additionalProperties": true
        }));
    let description = if cap.description.is_empty() {
        format!("Run skill {}", cap.name)
    } else {
        cap.description.clone()
    };
    let tool = serde_json::from_value::<rmcp::model::Tool>(json!({
        "name": tool_name,
        "description": description,
        "inputSchema": input_schema
    }))
    .map_err(|e| format!("Build skill tool failed: {e}"))?;
    Ok((tool_name, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(id: &str, cap_type: &str, definition: &str) -> HubCapability {
        HubCapability {
            id: id.to_string(),
            name: id.to_string(),
            cap_type: cap_type.to_string(),
            version: 1,
            description: String::new(),
            definition: definition.to_string(),
            enabled: true,
            review_status: "approved".to_string(),
            health_status: "healthy".to_string(),
            last_error: None,
            last_success_at: None,
            last_failure_at: None,
            fail_streak: 0,
            active_version: None,
            exposure_mode: "direct".to_string(),
            uses: 0,
            successes: 0,
            failures: 0,
            avg_rating: 0.0,
            last_used: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn visibility_accepts_policy_field_and_defaults_to_listed() {
        assert_eq!(
            capability_visibility_from_definition(&json!({"policy": {"visibility": "hidden"}})),
            CapabilityVisibility::Hidden
        );
        assert_eq!(
            capability_visibility_from_definition(&json!({"visibility": "on-demand"})),
            CapabilityVisibility::Discoverable
        );
        assert_eq!(
            capability_visibility_from_definition(&json!({})),
            CapabilityVisibility::Listed
        );
    }

    #[test]
    fn mcp_capability_requires_ready_discovery_to_be_callable() {
        let ready = cap("mcp:ready", "mcp", r#"{"discovery_status":"ready"}"#);
        let pending = cap("mcp:pending", "mcp", r#"{"discovery_status":"pending"}"#);

        assert!(capability_callable(&ready));
        assert!(!capability_callable(&pending));
    }

    #[test]
    fn mcp_capability_missing_discovery_status_is_callable_backward_compat() {
        // #968 option B (owner-ratified): a cap that already passed
        // enabled + approved + healthy but predates the discovery_status
        // stamp (or was registered outside the review-discovery flow) must
        // stay callable, not brick on upgrade.
        let missing = cap("mcp:missing", "mcp", r#"{"other_field":"value"}"#);

        assert!(capability_callable(&missing));
    }

    #[test]
    fn mcp_capability_malformed_definition_fails_closed() {
        let malformed = cap("mcp:malformed", "mcp", "not valid json{{{");

        assert!(!capability_callable(&malformed));
    }

    #[test]
    fn mcp_capability_non_string_discovery_status_fails_closed() {
        let non_string = cap("mcp:non-string", "mcp", r#"{"discovery_status":42}"#);

        assert!(!capability_callable(&non_string));
    }

    #[test]
    fn skill_tool_names_are_sanitized() {
        assert_eq!(
            sanitize_skill_tool_name("skill:Review/Fix-It"),
            Some("tachi_skill_review_fix_it".to_string())
        );
        assert_eq!(sanitize_skill_tool_name("mcp:web-search"), None);
    }
}
