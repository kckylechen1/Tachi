use crate::hub_ops::read_registered_skill_document;
use crate::server_state::MemoryServer;
use crate::shared_defs::dlq_replay_is_explicitly_safe;
use crate::utils::lock_or_recover;
use memcore::HubCapability;
use serde_json::Value;
use std::collections::HashMap;
use tachi_hub::{build_skill_tool_from_cap, make_text_tool_result};

fn register_skill_tool_in_registry(
    cap: &HubCapability,
    skill_tools: &mut HashMap<String, String>,
    skill_tool_defs: &mut HashMap<String, rmcp::model::Tool>,
) -> Result<String, String> {
    let (tool_name, tool) = build_skill_tool_from_cap(cap)?;
    if let Some(existing_id) = skill_tools.get(&tool_name) {
        if existing_id != &cap.id {
            return Err(format!(
                "Skill tool '{}' is already registered for capability '{}'.",
                tool_name, existing_id
            ));
        }
    }
    skill_tools.insert(tool_name.clone(), cap.id.clone());
    skill_tool_defs.insert(tool_name.clone(), tool);
    Ok(tool_name)
}

fn unregister_skill_tool_from_registry(
    skill_id: &str,
    skill_tools: &mut HashMap<String, String>,
    skill_tool_defs: &mut HashMap<String, rmcp::model::Tool>,
) -> Option<String> {
    let tool_name = skill_tools
        .iter()
        .find(|(_, id)| id.as_str() == skill_id)
        .map(|(name, _)| name.clone())?;
    skill_tools.remove(&tool_name);
    skill_tool_defs.remove(&tool_name);
    Some(tool_name)
}

impl MemoryServer {
    pub(crate) fn register_skill_tool(&self, cap: &HubCapability) -> Result<String, String> {
        let _ = self.unregister_skill_tool(&cap.id);
        if crate::builtins::is_retired_builtin_capability_id(&cap.id) {
            return Err(format!(
                "Skill '{}' is retired and cannot be exposed as a dynamic tool.",
                cap.id
            ));
        }
        let mut skill_tools = lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools");
        let mut skill_tool_defs =
            lock_or_recover(&self.tool_discovery.skill_tool_defs, "skill_tool_defs");
        register_skill_tool_in_registry(cap, &mut skill_tools, &mut skill_tool_defs)
    }

    pub(crate) fn unregister_skill_tool(&self, skill_id: &str) -> Result<Option<String>, String> {
        let removed_tool_name = {
            let mut skill_tools = lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools");
            let mut skill_tool_defs =
                lock_or_recover(&self.tool_discovery.skill_tool_defs, "skill_tool_defs");
            unregister_skill_tool_from_registry(skill_id, &mut skill_tools, &mut skill_tool_defs)
        };

        Ok(removed_tool_name)
    }

    pub(crate) async fn call_skill_tool(
        &self,
        tool_name: &str,
        _arguments: Option<rmcp::model::JsonObject>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        let skill_id = lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools")
            .get(tool_name)
            .cloned()
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    format!("Skill tool '{}' not found", tool_name),
                    None,
                )
            })?;

        let mut document = read_registered_skill_document(self, &skill_id)
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        document["tool_name"] = Value::String(tool_name.to_string());
        make_text_tool_result(&document)
    }

    pub(crate) async fn retry_dispatch(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        if !dlq_replay_is_explicitly_safe(tool_name, arguments.as_ref()) {
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Tool '{}' cannot be retried via DLQ because no explicit typed action-effect classification authorizes safe replay",
                    tool_name
                ),
                None,
            ));
        }

        if lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools").contains_key(tool_name)
        {
            let args_obj = arguments.map(|m| m.into_iter().collect::<rmcp::model::JsonObject>());
            return self.call_skill_tool(tool_name, args_obj).await;
        }

        if self.tool_router.has_route(tool_name) {
            return Err(rmcp::ErrorData::invalid_params(
                format!(
                    "Tool '{}' cannot be retried via DLQ because it is native or non-idempotent; retry the MCP call explicitly",
                    tool_name
                ),
                None,
            ));
        }

        Err(rmcp::ErrorData::invalid_params(
            format!(
                "Native tool '{}' cannot be retried via DLQ — retry the MCP call directly",
                tool_name
            ),
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_cap(id: &str, description: &str) -> HubCapability {
        HubCapability {
            id: id.to_string(),
            name: id.to_string(),
            cap_type: "skill".to_string(),
            version: 1,
            description: description.to_string(),
            definition: r#"{"execution":"document","content":"workflow"}"#.to_string(),
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

    fn tool_description(defs: &HashMap<String, rmcp::model::Tool>, tool_name: &str) -> String {
        let tool = serde_json::to_value(defs.get(tool_name).expect("registered tool definition"))
            .expect("serialize tool definition");
        tool["description"]
            .as_str()
            .expect("tool description")
            .to_string()
    }

    #[test]
    fn normalized_name_collision_preserves_winner_and_loser_unregister_is_noop() {
        let winner = document_cap("skill:Review/Fix", "original winner");
        let loser = document_cap("skill:review-fix", "colliding loser");
        let mut skill_tools = HashMap::new();
        let mut skill_tool_defs = HashMap::new();

        let tool_name =
            register_skill_tool_in_registry(&winner, &mut skill_tools, &mut skill_tool_defs)
                .expect("register winner");
        assert_eq!(tool_name, "tachi_skill_review_fix");
        let original_description = tool_description(&skill_tool_defs, &tool_name);

        let error = register_skill_tool_in_registry(&loser, &mut skill_tools, &mut skill_tool_defs)
            .expect_err("different capability must not take the normalized name");
        assert!(error.contains(&tool_name), "{error}");
        assert!(error.contains(&winner.id), "{error}");
        assert_eq!(skill_tools.get(&tool_name), Some(&winner.id));
        assert_eq!(skill_tools.len(), 1);
        assert_eq!(skill_tool_defs.len(), 1);
        assert_eq!(
            tool_description(&skill_tool_defs, &tool_name),
            original_description
        );

        assert_eq!(
            unregister_skill_tool_from_registry(&loser.id, &mut skill_tools, &mut skill_tool_defs,),
            None
        );
        assert_eq!(skill_tools.get(&tool_name), Some(&winner.id));
        assert_eq!(
            tool_description(&skill_tool_defs, &tool_name),
            original_description
        );

        let updated_winner = document_cap(&winner.id, "updated winner");
        assert_eq!(
            register_skill_tool_in_registry(
                &updated_winner,
                &mut skill_tools,
                &mut skill_tool_defs,
            )
            .expect("same-id update"),
            tool_name
        );
        assert_eq!(skill_tools.len(), 1);
        assert_eq!(skill_tool_defs.len(), 1);
        assert!(tool_description(&skill_tool_defs, &tool_name).contains("updated winner"));

        assert_eq!(
            unregister_skill_tool_from_registry(&winner.id, &mut skill_tools, &mut skill_tool_defs,),
            Some(tool_name)
        );
        assert!(skill_tools.is_empty());
        assert!(skill_tool_defs.is_empty());
    }
}
