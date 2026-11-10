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
                as_of: None,
            },
        )
        .await
        .ok()?;
        let full: serde_json::Value = serde_json::from_str(&raw).ok()?;
        if full
            .get("text")
            .and_then(serde_json::Value::as_str)
            .is_some()
        {
            let full: memcore::MemoryEntry = serde_json::from_value(full).ok()?;
            if memcore::is_test_memory_excluded(&full, None, false) {
                return None;
            }
            if !full.text.trim().is_empty() {
                return Some(full.text);
            }
        }
    }

    row.get("summary")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn hydration_rechecks_a_row_marked_synthetic_after_recall() {
        let (server, _home) = crate::tests::make_server_with_temp_home();
        let mut entry = crate::tests::make_entry("hydrated-synthetic-fixture");
        entry.path = "/notes/hydration".to_string();
        entry.text = "Current production lesson".to_string();
        let row = json!({"id": entry.id, "summary": "Previously recalled summary"});
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|error| error.to_string()))
            .unwrap();
        assert_eq!(
            prompt_row_text(&server, &row, None).await.as_deref(),
            Some("Current production lesson")
        );

        entry.metadata = json!({"synthetic_fixture": true});
        entry.text = "Synthetic replacement must not enter the prompt".to_string();
        server
            .with_global_store(|store| store.upsert(&entry).map_err(|error| error.to_string()))
            .unwrap();
        assert_eq!(prompt_row_text(&server, &row, None).await, None);
    }
}
