use crate::mcp_proxy::filter_mcp_tools_by_permissions;
use crate::MemoryServer;
use memcore::{HubCapability, MemoryStore};
use rmcp::model::Tool;
use std::collections::BTreeMap;
use tachi_hub::{should_expose_mcp_tools, ToolBundle, ToolProfile};

pub(super) struct McpToolRoute {
    pub server_name: String,
    pub upstream_name: String,
    pub tool: Tool,
}

impl MemoryServer {
    /// Read the persisted discovery snapshot on each request. Owner CLI updates
    /// become visible without restarting the daemon or maintaining a second cache.
    /// Project definitions, including disabled ones, shadow global definitions.
    pub(super) fn visible_mcp_tools(
        &self,
        profile: Option<ToolProfile>,
        patterns: Option<&[String]>,
    ) -> Result<Vec<McpToolRoute>, rmcp::ErrorData> {
        if !profile
            .unwrap_or_else(ToolProfile::standard)
            .allows(ToolBundle::Coordinate)
        {
            return Ok(Vec::new());
        }
        let read = |store: &mut MemoryStore| {
            store
                .hub_list(Some("mcp"), false)
                .map_err(|e| e.to_string())
        };
        let mut caps: BTreeMap<String, HubCapability> = self
            .with_global_store_read(read)
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?
            .into_iter()
            .map(|cap| (cap.id.clone(), cap))
            .collect();
        if self.has_project_db() {
            for cap in self
                .with_project_store_read(read)
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?
            {
                caps.insert(cap.id.clone(), cap);
            }
        }
        let mut routes = Vec::new();
        for cap in caps.into_values().filter(should_expose_mcp_tools) {
            let Some(server_name) = cap.id.strip_prefix("mcp:") else {
                continue;
            };
            let Ok(def) = serde_json::from_str::<serde_json::Value>(&cap.definition) else {
                continue;
            };
            let Some(tools) = def.get("discovered_tools").cloned() else {
                continue;
            };
            let Ok(tools) = serde_json::from_value::<Vec<Tool>>(tools) else {
                continue;
            };
            for mut tool in filter_mcp_tools_by_permissions(&def, tools) {
                let upstream_name = tool.name.to_string();
                tool.name = format!("{server_name}__{upstream_name}").into();
                if self.tool_router.has_route(tool.name.as_ref())
                    || patterns.is_some_and(|patterns| {
                        !patterns.iter().any(|pattern| {
                            tachi_hub::tool_name_matches_pattern(tool.name.as_ref(), pattern)
                        })
                    })
                {
                    continue;
                }
                routes.push(McpToolRoute {
                    server_name: server_name.to_owned(),
                    upstream_name,
                    tool,
                });
            }
        }
        // A server/tool pair containing "__" can collide with another pair.
        // Omit ambiguous names from both listing and dispatch, never guess.
        let mut counts = BTreeMap::new();
        for route in &routes {
            *counts.entry(route.tool.name.to_string()).or_insert(0) += 1;
        }
        routes.retain(|route| counts[route.tool.name.as_ref()] == 1);
        routes.sort_by(|a, b| a.tool.name.cmp(&b.tool.name));
        Ok(routes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn capability(id: &str, name: &str) -> HubCapability {
        let mut cap = crate::tests::make_mcp_capability(id, 1);
        cap.definition = json!({"transport":"stdio", "command":"fixture",
            "tool_exposure":"gateway", "policy":{"visibility":"discoverable"},
            "discovered_tools":[{"name":name,"inputSchema":{"type":"object"}}]
        })
        .to_string();
        cap
    }

    #[tokio::test]
    async fn catalog_uses_current_persisted_config_and_project_shadowing() {
        let (server, _) = crate::tests::make_server_with_project_fixture("mcp-catalog");
        let cap = capability("mcp:fixture", "tool__name");
        server
            .with_global_store(|store| store.hub_register(&cap).map_err(|e| e.to_string()))
            .unwrap();
        let patterns = vec!["fixture__tool*".to_string()];
        assert_eq!(
            server
                .visible_mcp_tools(Some(ToolProfile::delegate()), Some(&patterns))
                .unwrap()
                .len(),
            1
        );
        assert!(server
            .visible_mcp_tools(None, Some(&["other__*".to_string()]))
            .unwrap()
            .is_empty());
        assert!(server
            .visible_mcp_tools(Some(ToolProfile::observe()), None)
            .unwrap()
            .is_empty());
        let mut project_cap = cap.clone();
        project_cap.enabled = false;
        server
            .with_project_store(|store| store.hub_register(&project_cap).map_err(|e| e.to_string()))
            .unwrap();
        assert!(
            server.visible_mcp_tools(None, None).unwrap().is_empty(),
            "disabled project override must shadow enabled global connection"
        );
        project_cap.enabled = true;
        let mut def: serde_json::Value = serde_json::from_str(&project_cap.definition).unwrap();
        def["permissions"] = json!({"deny":["tool__name"]});
        project_cap.definition = def.to_string();
        server
            .with_project_store(|store| store.hub_register(&project_cap).map_err(|e| e.to_string()))
            .unwrap();
        assert!(
            server.visible_mcp_tools(None, None).unwrap().is_empty(),
            "changed permissions take effect without cache invalidation"
        );
    }

    #[tokio::test]
    async fn colliding_names_do_not_choose_an_arbitrary_upstream() {
        let server = crate::tests::make_server();
        for cap in [capability("mcp:a", "b__c"), capability("mcp:a__b", "c")] {
            server
                .with_global_store(|store| store.hub_register(&cap).map_err(|e| e.to_string()))
                .unwrap();
        }
        assert!(server.visible_mcp_tools(None, None).unwrap().is_empty());
    }
}
