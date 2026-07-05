//! Business logic for the `tachi_web_search` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_web_search`].

use serde_json::{json, Value};

use crate::tool_params::*;
use crate::MemoryServer;
use tachi_hub::capability_callable;

fn first_text_blocks(result: &rmcp::model::CallToolResult) -> Vec<String> {
    result
        .content
        .iter()
        .filter_map(|item| {
            serde_json::to_value(item).ok().and_then(|value| {
                value
                    .get("text")
                    .and_then(|text| text.as_str())
                    .map(String::from)
            })
        })
        .collect()
}

fn matches_web_search_backend(capability_id: &str, selector: Option<&str>) -> bool {
    let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    if selector.eq_ignore_ascii_case("auto") {
        return true;
    }
    let normalized = selector
        .strip_prefix("mcp:")
        .unwrap_or(selector)
        .to_ascii_lowercase();
    let cap = capability_id
        .strip_prefix("mcp:")
        .unwrap_or(capability_id)
        .to_ascii_lowercase();
    cap == normalized || cap.contains(&normalized)
}

fn discovered_tool_schema(definition: &Value, tool_name: &str) -> Option<Value> {
    definition
        .get("discovered_tools")
        .and_then(|tools| tools.as_array())
        .and_then(|tools| {
            tools.iter().find(|tool| {
                tool.get("name")
                    .and_then(|name| name.as_str())
                    .map(|name| name == tool_name)
                    .unwrap_or(false)
            })
        })
        .and_then(|tool| {
            tool.get("inputSchema")
                .or_else(|| tool.get("input_schema"))
                .cloned()
        })
}

fn choose_web_search_tool(
    capability_id: &str,
    definition: &Value,
    explicit_tool_name: Option<&str>,
) -> String {
    if let Some(name) = explicit_tool_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return name.to_string();
    }

    if let Some(name) = definition
        .get("discovered_tools")
        .and_then(|tools| tools.as_array())
        .and_then(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.get("name").and_then(|name| name.as_str()))
                .find(|name| name.to_ascii_lowercase().contains("search"))
        })
    {
        return name.to_string();
    }

    match capability_id {
        "mcp:exa" => "web_search_exa".to_string(),
        "mcp:tavily" => "tavily_search".to_string(),
        "mcp:web-search" => "web_search".to_string(),
        _ => "search".to_string(),
    }
}

fn web_search_arguments(
    params: &TachiWebSearchParams,
    input_schema: Option<&Value>,
) -> serde_json::Map<String, Value> {
    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), json!(params.query));

    let has_property = |name: &str| {
        input_schema
            .and_then(|schema| schema.get("properties"))
            .and_then(|properties| properties.as_object())
            .map(|properties| properties.contains_key(name))
            .unwrap_or(false)
    };

    for key in ["numResults", "max_results", "maxResults", "limit", "top_k"] {
        if has_property(key) {
            args.insert(key.to_string(), json!(params.top_k));
            break;
        }
    }

    if !params.include_domains.is_empty() {
        for key in ["include_domains", "includeDomains"] {
            if has_property(key) {
                args.insert(key.to_string(), json!(params.include_domains));
                break;
            }
        }
    }

    if !params.exclude_domains.is_empty() {
        for key in ["exclude_domains", "excludeDomains"] {
            if has_property(key) {
                args.insert(key.to_string(), json!(params.exclude_domains));
                break;
            }
        }
    }

    args
}

pub(crate) async fn handle_tachi_web_search(
    server: &MemoryServer,
    params: TachiWebSearchParams,
) -> Result<String, String> {
    let (bindings, binding_db) = server.get_virtual_capability_bindings("vc:web_search")?;
    let mut candidates: Vec<String> = bindings
        .into_iter()
        .filter(|binding| binding.enabled)
        .map(|binding| binding.capability_id)
        .collect();

    for fallback in ["mcp:web-search", "mcp:exa", "mcp:tavily"] {
        if !candidates.iter().any(|candidate| candidate == fallback) {
            candidates.push(fallback.to_string());
        }
    }

    let backend_selector = params.backend.as_deref();
    let mut attempts = Vec::new();

    for capability_id in candidates {
        if !matches_web_search_backend(&capability_id, backend_selector) {
            continue;
        }

        let cap = match server.get_capability(&capability_id) {
            Ok(cap) => cap,
            Err(err) => {
                attempts.push(json!({
                    "capability_id": capability_id,
                    "status": "missing",
                    "error": format!("{err}"),
                }));
                continue;
            }
        };

        if !capability_callable(&cap) {
            attempts.push(json!({
                "capability_id": capability_id,
                "status": "not_callable",
                "enabled": cap.enabled,
                "review_status": cap.review_status,
                "health_status": cap.health_status,
            }));
            continue;
        }

        let def: Value = match serde_json::from_str(&cap.definition) {
            Ok(def) => def,
            Err(err) => {
                attempts.push(json!({
                    "capability_id": capability_id,
                    "status": "bad_definition",
                    "error": err.to_string(),
                }));
                continue;
            }
        };

        let tool_name = choose_web_search_tool(&capability_id, &def, params.tool_name.as_deref());
        let schema = discovered_tool_schema(&def, &tool_name);
        let arguments = web_search_arguments(&params, schema.as_ref());
        let result = server
            .proxy_call_capability_internal(
                &capability_id,
                Some("vc:web_search"),
                &tool_name,
                Some(arguments.clone()),
            )
            .await;

        match result {
            Ok(result) if !result.is_error.unwrap_or(false) => {
                let content = first_text_blocks(&result);
                let response = json!({
                    "status": "completed",
                    "action": "search",
                    "query": params.query,
                    "backend": capability_id,
                    "tool": tool_name,
                    "binding_db": binding_db,
                    "arguments": arguments,
                    "content": content,
                    "attempts": attempts,
                });
                return serde_json::to_string(&response)
                    .map_err(|err| format!("serialize web search response: {err}"));
            }
            Ok(result) => {
                attempts.push(json!({
                    "capability_id": capability_id,
                    "tool": tool_name,
                    "status": "tool_error",
                    "content": first_text_blocks(&result),
                }));
            }
            Err(err) => {
                attempts.push(json!({
                    "capability_id": capability_id,
                    "tool": tool_name,
                    "status": "call_failed",
                    "error": format!("{err}"),
                }));
            }
        }
    }

    Err(format!(
        "No web search backend succeeded for query '{}'. Attempts: {}",
        params.query,
        serde_json::to_string(&attempts).unwrap_or_else(|_| "[]".to_string())
    ))
}
