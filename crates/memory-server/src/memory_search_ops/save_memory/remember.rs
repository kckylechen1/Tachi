use super::handler::handle_save_memory;
use crate::tool_params::{RememberParams, SaveMemoryParams};
use crate::MemoryServer;
use chrono::Utc;
use serde_json::json;

/// Low-friction shortcut over `handle_save_memory`. Infers `path`, `category`,
/// and `importance` so callers only need to pass `text` (and optionally
/// `tags`). Internally constructs `SaveMemoryParams` and delegates, so noise
/// filter, capture gate, provenance injection, auto-link, and the enrichment
/// batcher all run identically to a direct save_memory call.
pub(crate) async fn handle_remember(
    server: &MemoryServer,
    params: RememberParams,
) -> Result<String, String> {
    // Default path = /notes/{YYYY-MM-DD} so quick captures land in a
    // predictable, browsable bucket without forcing the caller to choose one.
    let inferred_path = params.path.unwrap_or_else(|| {
        let date = Utc::now().format("%Y-%m-%d");
        format!("/notes/{date}")
    });

    let save_params = SaveMemoryParams {
        text: params.text,
        summary: params.summary,
        path: inferred_path,
        importance: params.importance.unwrap_or(0.6).clamp(0.0, 1.0),
        category: params.category.unwrap_or_else(|| "fact".to_string()),
        topic: params.topic,
        keywords: params.tags,
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: params.scope.unwrap_or_else(|| "project".to_string()),
        vector: None,
        id: None,
        force: params.force,
        auto_link: true,
        project: params.project,
        retention_policy: params.retention_policy,
        domain: params.domain,
        timestamp: None,
        valid_from: params.valid_from,
        valid_until: params.valid_until,
        metadata: Some(json!({ "shortcut": "remember" })),
    };

    handle_save_memory(server, save_params).await
}
