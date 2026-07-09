use super::super::helpers::{build_section_artifact, dedup_strings, SectionArtifactInput};
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

    let section = build_section_artifact(SectionArtifactInput {
        layer: &params.layer,
        kind: &params.kind,
        title: params.title.as_deref(),
        content: &content,
        items: &items,
        cache_boundary: &params.cache_boundary,
        source_refs: &params.source_refs,
        target_tokens: params.target_tokens,
    });

    serde_json::to_string(&json!({
        "status": "completed",
        "section": section,
    }))
    .map_err(|e| format!("Failed to serialize section_build response: {e}"))
}
