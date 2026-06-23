use crate::tool_params::GetMemoryParams;
use crate::MemoryServer;

pub(super) fn compact_example_text(text: &str, max_chars: usize) -> String {
    let mut out = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect::<String>();
        out.push_str("...");
    }
    out
}

pub(super) async fn prompt_row_text(
    server: &MemoryServer,
    row: &serde_json::Value,
    project: Option<&str>,
) -> Option<String> {
    if let Some(text) = row.get("text").and_then(|v| v.as_str()) {
        if !text.trim().is_empty() {
            return Some(text.to_string());
        }
    }

    if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
        let raw = crate::memory_ops::handle_get_memory(
            server,
            GetMemoryParams {
                id: id.to_string(),
                include_archived: false,
                project: project.map(str::to_string),
            },
        )
        .await
        .ok()?;
        let full: serde_json::Value = serde_json::from_str(&raw).ok()?;
        if let Some(text) = full.get("text").and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        }
    }

    row.get("summary")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}
