//! Human-readable Markdown formatting for agent-facing MCP tools.

use serde_json::Value;

/// Escape characters that have special meaning in Markdown bold/code contexts.
fn md_escape(s: &str) -> String {
    s.replace('*', "\\*")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('_', "\\_")
}

fn compact_text(s: &str, limit: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= limit {
        one_line
    } else {
        format!("{}...", one_line.chars().take(limit).collect::<String>())
    }
}

pub(crate) fn format_briefing(
    query: &str,
    memories: &Value,
    wiki: &Value,
    health_summary: &Value,
    kanban: &Value,
    checkpoints: &Value,
) -> String {
    let mut out = vec!["## Tachi briefing".to_string(), format!("Query: {query}")];

    out.push("\n### Memories".to_string());
    out.push(format_section_rows(memories, 12));

    if wiki.as_array().is_some_and(|rows| !rows.is_empty()) {
        out.push("\n### Wiki".to_string());
        out.push(format_section_rows(wiki, 5));
    }

    if let Some(score) = health_summary.get("health_score") {
        out.push(format!("\n### Health snapshot (score {score})"));
        if let Some(warnings) = health_summary.get("warnings").and_then(Value::as_array) {
            if warnings.is_empty() {
                out.push("- No active warnings".to_string());
            } else {
                for warning in warnings.iter().take(6) {
                    if let Some(text) = warning.as_str() {
                        out.push(format!("- {text}"));
                    }
                }
            }
        }
        if let Some(wiki_h) = health_summary.get("wiki") {
            out.push(format!(
                "- Wiki hygiene: {} orphan(s), {} stale, {} duplicate(s)",
                wiki_h.get("orphans").and_then(Value::as_u64).unwrap_or(0),
                wiki_h
                    .get("stale_nodes")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                wiki_h
                    .get("duplicates")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ));
        }
    }

    if let Some(tasks) = kanban.get("tasks").and_then(Value::as_array) {
        if !tasks.is_empty() {
            out.push("\n### Kanban".to_string());
            for task in tasks.iter().take(5) {
                let summary = task
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(task)");
                let state = task
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                out.push(format!("- [{state}] {summary}"));
            }
        }
    }

    if let Some(cps) = checkpoints.as_array() {
        if !cps.is_empty() {
            out.push("\n### Recent checkpoints".to_string());
            for cp in cps.iter().take(3) {
                let title = cp
                    .get("title")
                    .or_else(|| cp.get("summary"))
                    .and_then(Value::as_str)
                    .unwrap_or("(checkpoint)");
                out.push(format!("- {title}"));
            }
        }
    }

    out.push(
        "\n> **Session end**: save decisions/outcomes → `tachi_memory(action='save', text=…, keywords=[…], project='…')`. Windsurf/Cursor have no auto-capture."
            .to_string(),
    );

    out.join("\n")
}

pub(crate) fn format_alerts(warnings: &[String], wiki_counts: &Value) -> String {
    let mut out = vec!["## Tachi alerts".to_string(), "\n### Warnings".to_string()];
    if warnings.is_empty() {
        out.push("- No active warnings".to_string());
    } else {
        for (idx, warning) in warnings.iter().enumerate().take(12) {
            out.push(format!("{}. {warning}", idx + 1));
        }
    }

    let orphans = wiki_counts
        .get("orphans")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let stale = wiki_counts
        .get("stale_nodes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let duplicates = wiki_counts
        .get("duplicates")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if orphans + stale + duplicates > 0 {
        out.push(format!(
            "\n### Wiki hygiene\n- Orphans: {orphans} | Stale: {stale} | Duplicates: {duplicates}"
        ));
    }

    out.join("\n")
}

pub(crate) fn format_wiki_search(query: &str, count: usize, results: &Value) -> String {
    let mut out = vec![
        format!("## Wiki search: \"{query}\""),
        format!("Results: {count}"),
    ];
    let rows: &[Value] = results
        .as_array()
        .or_else(|| results.get("results").and_then(Value::as_array))
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    if rows.is_empty() {
        out.push("_No wiki entries matched._".to_string());
    } else {
        for (idx, row) in rows.iter().enumerate() {
            let summary = row
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("(no summary)");
            let path = row.get("path").and_then(Value::as_str).unwrap_or("/wiki");
            let relevance = row
                .get("relevance")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".to_string());
            out.push(format!(
                "{}. {relevance} `{path}` - {}",
                idx + 1,
                md_escape(&compact_text(summary, 120)),
            ));
        }
    }
    out.join("\n")
}

pub(crate) fn format_search_sections(query: &str, sections: &[(String, Value)]) -> String {
    let mut out = vec![format!("## Tachi search: \"{query}\"")];
    for (heading, rows) in sections {
        out.push(format!("\n### {heading}"));
        if let Some(text) = rows.as_str() {
            out.push(text.to_string());
        } else {
            out.push(format_section_rows(rows, 12));
        }
    }
    out.join("\n")
}

pub(crate) fn format_wiki_browse_stats(total: usize, categories: &[(String, usize)]) -> String {
    let mut out = vec!["## Wiki browse".to_string()];
    if categories.is_empty() {
        out.push("\n_No wiki categories found._".to_string());
        return out.join("\n");
    }
    out.push(format!(
        "\n**{total}** entries across **{}** categories:\n",
        categories.len()
    ));
    for (path, count) in categories {
        out.push(format!("- `{path}` ({count})"));
    }
    out.join("\n")
}

pub(crate) fn format_wiki_browse_category(path: &str, entries: &[Value]) -> String {
    let mut out = vec![format!("## Wiki browse: `{path}`")];
    if entries.is_empty() {
        out.push("\n_No entries in this category._".to_string());
        return out.join("\n");
    }
    out.push(format!("\n**{}** entries:\n", entries.len()));
    for (idx, entry) in entries.iter().enumerate() {
        let summary = entry
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("(no summary)");
        let entry_path = entry.get("path").and_then(Value::as_str).unwrap_or(path);
        let importance = entry
            .get("importance")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        out.push(format!(
            "{}. **`{entry_path}`** (importance {importance})\n   {}",
            idx + 1,
            md_escape(summary),
        ));
    }
    out.join("\n")
}

pub(crate) fn format_wiki_read(entry: &Value) -> String {
    let path = entry
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let text = entry
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("(empty)");
    let summary = entry.get("summary").and_then(Value::as_str).unwrap_or("");
    let importance = entry
        .get("importance")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "?".to_string());
    let keywords = entry
        .get("keywords")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let entities = entry
        .get("entities")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();

    let mut out = vec![format!("## Wiki: `{path}`")];
    if !summary.is_empty() {
        out.push(format!("\n> {summary}"));
    }
    out.push(format!(
        "\n**importance:** {importance}{}",
        if keywords.is_empty() {
            String::new()
        } else {
            format!(" | **keywords:** {keywords}")
        }
    ));
    if !entities.is_empty() {
        out.push(format!("**entities:** {entities}"));
    }
    out.push("\n---\n".to_string());
    out.push(text.to_string());
    out.join("\n")
}

fn format_section_rows(rows: &Value, limit: usize) -> String {
    let Some(items) = rows.as_array() else {
        return "_No results._".to_string();
    };
    if items.is_empty() {
        return "_No results._".to_string();
    }
    let mut out = Vec::new();
    for (idx, row) in items.iter().take(limit).enumerate() {
        let topic = row.get("topic").and_then(Value::as_str).unwrap_or("entry");
        let summary = row
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("(no summary)");
        let path = row.get("path").and_then(Value::as_str).unwrap_or("/");
        let id = row.get("id").and_then(Value::as_str);
        let relevance = row
            .get("relevance")
            .or_else(|| row.get("score"))
            .filter(|v| !v.is_null())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        let id_suffix = id.map(|value| format!(" `{value}`")).unwrap_or_default();
        out.push(format!(
            "{}. **{}**{id_suffix} {relevance} `{path}` - {}",
            idx + 1,
            md_escape(topic),
            md_escape(&compact_text(summary, 120)),
        ));
        if let Some(files) = row.get("files").and_then(Value::as_array) {
            let paths: Vec<String> = files
                .iter()
                .filter_map(Value::as_str)
                .take(5)
                .map(|p| format!("`{}`", md_escape(p)))
                .collect();
            if !paths.is_empty() {
                out.push(format!("   📎 {}", paths.join(", ")));
            }
        }
    }
    out.join("\n")
}
