//! Human-readable Markdown formatting for agent-facing MCP tools.

use crate::db_context::{diagnostics_footer, format_db_context_markdown, DbContext};
use serde_json::Value;

pub(crate) fn format_memory_rows(title: &str, ctx: &DbContext, rows: &Value) -> String {
    let mut out = vec![format!("## {title}\n"), format_db_context_markdown(ctx)];
    let Some(items) = rows.as_array() else {
        out.push("\n_No results._".to_string());
        return out.join("\n");
    };
    if items.is_empty() {
        out.push("\n_No results._".to_string());
        return out.join("\n");
    }
    out.push(format!("\n### Results ({})", items.len()));
    for (idx, row) in items.iter().enumerate() {
        let topic = row
            .get("topic")
            .and_then(Value::as_str)
            .unwrap_or("untitled");
        let db = row.get("db").and_then(Value::as_str).unwrap_or("?");
        let relevance = row
            .get("relevance")
            .or_else(|| row.get("score"))
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        let summary = row
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("(no summary)");
        let path = row
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("/");
        out.push(format!(
            "\n{}. **{topic}** ({db}, relevance {relevance})\n   - {summary}\n   - Path: `{path}`"
        , idx + 1));
    }
    out.join("\n")
}

pub(crate) fn format_briefing(
    ctx: &DbContext,
    query: &str,
    memories: &Value,
    wiki: &Value,
    health_summary: &Value,
    kanban: &Value,
    checkpoints: &Value,
) -> String {
    let mut out = vec![
        "## Tachi briefing".to_string(),
        format!("**Query:** {query}\n"),
        format_db_context_markdown(ctx),
    ];

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
        let wiki_h = health_summary.get("wiki");
        if wiki_h.is_some() {
            out.push(format!(
                "- Wiki hygiene: {} orphan(s), {} stale, {} duplicate(s)",
                wiki_h.and_then(|v| v.get("orphans")).and_then(Value::as_u64).unwrap_or(0),
                wiki_h.and_then(|v| v.get("stale_nodes")).and_then(Value::as_u64).unwrap_or(0),
                wiki_h.and_then(|v| v.get("duplicates")).and_then(Value::as_u64).unwrap_or(0),
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

    out.push(diagnostics_footer().to_string());
    out.join("\n")
}

pub(crate) fn format_alerts(ctx: &DbContext, warnings: &[String], wiki_counts: &Value) -> String {
    let mut out = vec![
        "## Tachi alerts".to_string(),
        format_db_context_markdown(ctx),
        "\n### Warnings".to_string(),
    ];
    if warnings.is_empty() {
        out.push("- No active warnings".to_string());
    } else {
        for (idx, warning) in warnings.iter().enumerate().take(12) {
            out.push(format!("{}. {warning}", idx + 1));
        }
    }

    let orphans = wiki_counts.get("orphans").and_then(Value::as_u64).unwrap_or(0);
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

    out.push(diagnostics_footer().to_string());
    out.join("\n")
}

pub(crate) fn format_wiki_search(
    ctx: &DbContext,
    query: &str,
    count: usize,
    results: &Value,
) -> String {
    let mut out = vec![
        format!("## Wiki search: \"{query}\""),
        format_db_context_markdown(ctx),
        format!("\n### Results ({count})"),
    ];
    let rows = results
        .as_array()
        .or_else(|| results.get("results").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        out.push("_No wiki entries matched._".to_string());
    } else {
        for (idx, row) in rows.iter().enumerate() {
            let summary = row
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("(no summary)");
            let path = row
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or("/wiki");
            let relevance = row
                .get("relevance")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".to_string());
            out.push(format!(
                "\n{}. `{path}` (relevance {relevance})\n   {summary}",
                idx + 1
            ));
        }
    }
    out.join("\n")
}

pub(crate) fn format_search_sections(ctx: &DbContext, query: &str, sections: &[(String, Value)]) -> String {
    let mut out = vec![
        format!("## Tachi search: \"{query}\""),
        format_db_context_markdown(ctx),
    ];
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

fn format_section_rows(rows: &Value, limit: usize) -> String {
    let Some(items) = rows.as_array() else {
        return "_No results._".to_string();
    };
    if items.is_empty() {
        return "_No results._".to_string();
    }
    let mut out = Vec::new();
    for (idx, row) in items.iter().take(limit).enumerate() {
        let topic = row
            .get("topic")
            .and_then(Value::as_str)
            .unwrap_or("entry");
        let summary = row
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("(no summary)");
        let path = row
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("/");
        let id = row.get("id").and_then(Value::as_str);
        let relevance = row
            .get("relevance")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        let id_suffix = id.map(|value| format!(" `{value}`")).unwrap_or_default();
        out.push(format!(
            "{}. **{topic}**{id_suffix} (relevance {relevance})\n   - {summary}\n   - `{path}`",
            idx + 1
        ));
    }
    out.join("\n")
}
