use super::super::helpers::{build_section_artifact, dedup_strings};
use crate::server_state::MemoryServer;
use crate::tool_params::SectionBuildParams;
use serde_json::json;

pub(crate) async fn handle_section_build(
    _server: &MemoryServer,
    params: SectionBuildParams,
) -> Result<String, String> {
    let content = params.content.unwrap_or_default();
    let items = dedup_strings(params.items);
    if content.trim().is_empty() && items.is_empty() {
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_section",
        }))
        .map_err(|e| format!("Failed to serialize section_build response: {e}"));
    }

    let section = build_section_artifact(
        &params.layer,
        &params.kind,
        params.title.as_deref(),
        &content,
        &items,
        &params.cache_boundary,
        &params.source_refs,
        params.target_tokens,
    );

    serde_json::to_string(&json!({
        "status": "completed",
        "section": section,
    }))
    .map_err(|e| format!("Failed to serialize section_build response: {e}"))
}
