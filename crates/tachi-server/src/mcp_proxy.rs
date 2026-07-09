use chrono::Utc;
use rmcp::model::Tool;
use serde_json::json;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolExposureMode {
    Flatten,
    Gateway,
}

impl McpToolExposureMode {
    pub fn from_str(raw: &str) -> Option<Self> {
        let normalized = raw.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "flatten" | "direct" | "expanded" | "server-tools" | "server_tools" => {
                Some(Self::Flatten)
            }
            "gateway" | "hub-call" | "hub_call" | "hub_call_only" | "proxy-only" | "proxy_only"
            | "compact" => Some(Self::Gateway),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Flatten => "flatten",
            Self::Gateway => "gateway",
        }
    }
}

pub fn clear_mcp_discovery_metadata(def: &mut serde_json::Value) {
    if let Some(obj) = def.as_object_mut() {
        obj.remove("discovered_tools");
        obj.remove("discovered_at");
        obj.remove("tools_count");
        obj.remove("discovery_status");
        obj.remove("discovery_checked_at");
        obj.remove("last_discovery_error");
    }
}

pub fn set_mcp_discovery_success(def: &mut serde_json::Value, tools: &[Tool]) {
    if !def.is_object() {
        *def = json!({});
    }
    clear_mcp_discovery_metadata(def);
    def["discovery_status"] = json!("ready");
    def["discovered_tools"] = serde_json::to_value(tools).unwrap_or_default();
    def["discovered_at"] = json!(Utc::now().to_rfc3339());
    def["tools_count"] = json!(tools.len());
}

pub fn set_mcp_discovery_failure(def: &mut serde_json::Value, error: &str) {
    if !def.is_object() {
        *def = json!({});
    }
    clear_mcp_discovery_metadata(def);
    def["discovery_status"] = json!("failed");
    def["discovery_checked_at"] = json!(Utc::now().to_rfc3339());
    def["last_discovery_error"] = json!(error);
}

pub fn append_warning(
    resp: &mut serde_json::Map<String, serde_json::Value>,
    warning: impl Into<String>,
) {
    let warning = warning.into();
    if warning.is_empty() {
        return;
    }

    match resp.remove("warning") {
        Some(serde_json::Value::String(existing)) if !existing.is_empty() => {
            resp.insert("warning".into(), json!(format!("{existing}; {warning}")));
        }
        _ => {
            resp.insert("warning".into(), json!(warning));
        }
    }
}

fn parse_tool_name_set(def: &serde_json::Value, key: &str) -> Option<HashSet<String>> {
    let names: HashSet<String> = def
        .get("permissions")
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(|name| name.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

pub fn filter_mcp_tools_by_permissions(def: &serde_json::Value, tools: Vec<Tool>) -> Vec<Tool> {
    let allow_list = parse_tool_name_set(def, "allow");
    let deny_list = parse_tool_name_set(def, "deny").unwrap_or_default();

    tools
        .into_iter()
        .filter(|tool| {
            let tool_name = tool.name.as_ref();
            let allowed = allow_list
                .as_ref()
                .map(|set| set.contains(tool_name))
                .unwrap_or(true);
            allowed && !deny_list.contains(tool_name)
        })
        .collect()
}

pub fn resolve_mcp_tool_exposure(
    def: &serde_json::Value,
    default_mode: McpToolExposureMode,
) -> McpToolExposureMode {
    if let Some(raw_mode) = def.get("tool_exposure").and_then(|v| v.as_str()) {
        if let Some(mode) = McpToolExposureMode::from_str(raw_mode) {
            return mode;
        }
    }

    if let Some(expose_tools) = def.get("expose_tools").and_then(|v| v.as_bool()) {
        return if expose_tools {
            McpToolExposureMode::Flatten
        } else {
            McpToolExposureMode::Gateway
        };
    }

    default_mode
}
