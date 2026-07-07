use rmcp::model::JsonObject;

pub(crate) const HEADER_PROFILE: &str = "x-tachi-profile";
pub(crate) const HEADER_CLIENT: &str = "x-tachi-client";
pub(crate) const HEADER_PROJECT: &str = "x-tachi-project";

pub(crate) const META_PROFILE: &str = "tachiProfile";
pub(crate) const META_CLIENT: &str = "tachiClient";
pub(crate) const META_PROJECT: &str = "tachiProject";

pub(crate) fn enforce_session_project(
    tool_name: &str,
    arguments: &mut Option<JsonObject>,
    project: &str,
    transport_label: &str,
) -> Result<(), rmcp::ErrorData> {
    let args = arguments.get_or_insert_with(serde_json::Map::new);
    if let Some(explicit_project) = args.get("project") {
        if explicit_project.as_str() == Some(project) {
            return Ok(());
        }
        // Write isolation: reject routing into another project's DB. Cross-library
        // reads are safe because daemon-side project stores are opened read-only.
        if explicit_project.as_str().is_some()
            && explicit_project_can_cross_binding(tool_name, args)
        {
            return Ok(());
        }
        return Err(rmcp::ErrorData::invalid_params(
            format!(
                "{transport_label} project binding mismatch: session is bound to '{project}', but tool call requested project={explicit_project}"
            ),
            None,
        ));
    }
    if !project_defaults_to_bound_project(tool_name, args) {
        return Ok(());
    }
    if tool_name == "tachi_memory"
        && args
            .get("action")
            .and_then(|value| value.as_str())
            .map(|action| !tachi_memory_action_defaults_to_project(action))
            .unwrap_or(false)
    {
        return Ok(());
    }
    if args
        .get("scope")
        .and_then(|value| value.as_str())
        .is_some_and(|scope| scope.eq_ignore_ascii_case("global"))
    {
        return Ok(());
    }
    args.insert("project".to_string(), serde_json::json!(project));
    Ok(())
}

/// Returns true when an explicit `project` param may differ from the session
/// binding. Protects write isolation only: daemon-side reads use read-only opens
/// and do not threaten single-writer discipline.
pub(crate) fn explicit_project_can_cross_binding(tool_name: &str, args: &JsonObject) -> bool {
    match tool_name {
        "search_memory"
        | "find_similar_memory"
        | "get_memory"
        | "list_memories"
        | "memory_graph"
        | "get_edges"
        | "tachi_search" => true,
        "tachi_memory" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_memory_action_allows_cross_project_read),
        "tachi_wiki" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_wiki_action_allows_cross_project_read),
        "tachi_event" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_event_action_allows_cross_project_read),
        _ => false,
    }
}

pub(crate) fn project_defaults_to_bound_project(tool_name: &str, args: &JsonObject) -> bool {
    if tool_name == "tachi_memory" {
        return args
            .get("action")
            .and_then(|value| value.as_str())
            .is_none_or(tachi_memory_action_defaults_to_project);
    }
    matches!(
        tool_name,
        "search_memory"
            | "find_similar_memory"
            | "get_memory"
            | "list_memories"
            | "delete_memory"
            | "archive_memory"
            | "save_memory"
            | "remember"
            | "ingest"
            | "ingest_event"
            | "ingest_source"
            | "extract_facts"
            | "tachi_search"
            | "tachi_save"
            | "tachi_event"
            | "tachi_domain_adapter"
            | "tachi_task"
            | "tachi_verify"
            | "tachi_gh"
            | "tachi_wiki"
            | "wiki_write"
            | "tachi_wiki_write"
    ) || tool_name == "tachi_memory"
}

fn tachi_memory_action_defaults_to_project(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "alerts"
            | "ask"
            | "briefing"
            | "checkpoint"
            | "consolidate"
            | "extract_facts"
            | "get"
            | "apply_recall_proposals"
            | "pattern_feedback"
            | "progress"
            | "readiness"
            | "recall_proposals"
            | "recall_simulate"
            | "review_recall_proposal"
            | "save"
            | "search"
    )
}

fn tachi_memory_action_allows_cross_project_read(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "alerts"
            | "ask"
            | "briefing"
            | "consolidate"
            | "get"
            | "readiness"
            | "recall_simulate"
            | "search"
    )
}

fn tachi_event_action_allows_cross_project_read(action: &str) -> bool {
    matches!(action.to_ascii_lowercase().as_str(), "metrics" | "query")
}

fn tachi_wiki_action_allows_cross_project_read(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "browse" | "read" | "search"
    )
}

pub(crate) fn normalize_identity_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
