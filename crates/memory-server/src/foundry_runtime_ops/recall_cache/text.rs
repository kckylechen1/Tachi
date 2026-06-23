use super::super::recall::{value_id, value_path, value_relevance, value_topic};

fn row_string(row: &serde_json::Value, key: &str) -> String {
    row.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(in crate::foundry_runtime_ops::recall_cache) fn build_recall_cache_text(
    query: &str,
    rows: &[serde_json::Value],
) -> String {
    let mut lines = vec![format!("Recall rerank cache for query: {query}")];
    for (idx, row) in rows.iter().enumerate() {
        let id = value_id(row);
        let path = value_path(row);
        let topic = value_topic(row);
        let score = value_relevance(row);
        let summary = row_string(row, "summary");
        let text = row_string(row, "text");
        lines.push(format!(
            "{}. id={} score={:.3} topic={} path={}",
            idx + 1,
            if id.is_empty() { "unknown" } else { &id },
            score,
            if topic.is_empty() { "unknown" } else { &topic },
            if path.is_empty() { "unknown" } else { &path },
        ));
        lines.push(format!(
            "   {}",
            if summary.trim().is_empty() {
                text.chars().take(180).collect::<String>()
            } else {
                summary
            }
        ));
    }
    lines.join("\n")
}
