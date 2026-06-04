//! Human-readable Markdown formatting for agent-facing MCP tools.

use crate::utils::compact_text_line;
use serde_json::Value;

/// Escape characters that have special meaning in Markdown bold/code contexts.
fn md_escape(s: &str) -> String {
    s.replace('*', "\\*")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('_', "\\_")
}

pub(crate) fn format_briefing(
    query: &str,
    project_label: Option<&str>,
    memories: &Value,
    wiki: &Value,
    cross_project: &Value,
    health_summary: &Value,
    kanban: &Value,
    checkpoints: &Value,
    compact: bool,
) -> String {
    let memory_cap = if compact { 6 } else { 12 };
    let wiki_cap = if compact { 3 } else { 5 };
    let kanban_cap = if compact { 3 } else { 5 };
    let checkpoint_cap = if compact { 2 } else { 3 };
    let cross_cap = if compact { 3 } else { 5 };

    let mut out = vec!["## Tachi briefing".to_string(), format!("Query: {query}")];
    if let Some(project) = project_label.filter(|p| !p.is_empty()) {
        out.push(format!(
            "Project focus: `{project}` (memories/wiki from this repo)"
        ));
    }

    if let Some(handoffs) = cross_project.as_array() {
        if !handoffs.is_empty() {
            out.push("\n### Cross-project (global handoffs)".to_string());
            out.push(
                "_Pending memos from other repos/agents. Ack with `tachi_handoff(action='check')` or leave via `tachi_handoff(action='leave')`._".to_string(),
            );
            for row in handoffs.iter().take(cross_cap) {
                let from = row.get("from_agent").and_then(Value::as_str).unwrap_or("?");
                let summary = row
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(handoff)");
                let path = row
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("/handoff");
                out.push(format!(
                    "- [handoff] `{path}` from **{from}**: {}",
                    md_escape(&compact_text_line(summary, 120))
                ));
            }
        }
    }

    out.push("\n### Memories (this project)".to_string());
    out.push(format_section_rows(memories, memory_cap));

    if wiki.as_array().is_some_and(|rows| !rows.is_empty()) {
        out.push("\n### Wiki".to_string());
        out.push(format_section_rows(wiki, wiki_cap));
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
            for task in tasks.iter().take(kanban_cap) {
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
            for cp in cps.iter().take(checkpoint_cap) {
                let raw_title = cp
                    .get("title")
                    .or_else(|| cp.get("summary"))
                    .and_then(Value::as_str)
                    .unwrap_or("(checkpoint)");
                out.push(format!("- {}", compact_text_line(raw_title, 140)));
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
                md_escape(&compact_text_line(summary, 120)),
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
            md_escape(&compact_text_line(summary, 120)),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn long_title() -> String {
        // 250 chars of 'a' with newlines
        let body = "a".repeat(220);
        format!("{body}\n\nNext paragraph that should be truncated by the cap.")
    }

    #[test]
    fn format_briefing_truncates_long_checkpoint_titles() {
        let checkpoints = serde_json::json!([
            {"id": "c1", "title": long_title(), "summary": "fallback"}
        ]);
        let memories = serde_json::json!([]);
        let wiki = serde_json::json!([]);
        let health = serde_json::json!({"health_score": 95, "warnings": [], "wiki": {}});
        let kanban = serde_json::json!({"tasks": []});

        let cross = serde_json::json!([]);
        let out = format_briefing(
            "q",
            Some("sigil"),
            &memories,
            &wiki,
            &cross,
            &health,
            &kanban,
            &checkpoints,
            false,
        );

        // Compact-style truncation kicks in for checkpoints: must be capped.
        // We can't assert an exact char count because compact_text_line adds "…",
        // but the raw newline must not survive, and the line must be short.
        let line = out
            .lines()
            .find(|l| l.starts_with("- "))
            .expect("at least one bullet");
        assert!(!line.contains('\n'), "checkpoint title must be single-line");
        assert!(
            line.len() < 200,
            "checkpoint title should be truncated; got len {}",
            line.len()
        );
    }

    #[test]
    fn format_briefing_compact_caps_section_rows() {
        let memories: Vec<serde_json::Value> = (0..20)
            .map(|i| {
                serde_json::json!({
                    "id": format!("m{i}"),
                    "summary": format!("row {i}"),
                    "topic": "t",
                    "path": "/p",
                })
            })
            .collect();
        let wiki: Vec<serde_json::Value> = (0..20)
            .map(|i| {
                serde_json::json!({
                    "id": format!("w{i}"),
                    "summary": format!("wiki {i}"),
                    "topic": "t",
                    "path": "/wiki/x",
                })
            })
            .collect();
        let checkpoints: Vec<serde_json::Value> = (0..5)
            .map(|i| serde_json::json!({"id": format!("c{i}"), "title": format!("cp {i}")}))
            .collect();
        let health = serde_json::json!({"health_score": 95, "warnings": [], "wiki": {}});
        let kanban = serde_json::json!({"tasks": []});

        let cross = serde_json::json!([]);
        let compact = format_briefing(
            "q",
            None,
            &serde_json::json!(memories),
            &serde_json::json!(wiki),
            &cross,
            &health,
            &kanban,
            &serde_json::json!(checkpoints),
            true,
        );
        let full = format_briefing(
            "q",
            None,
            &serde_json::json!(memories),
            &serde_json::json!(wiki),
            &cross,
            &health,
            &kanban,
            &serde_json::json!(checkpoints),
            false,
        );

        // `format_section_rows` emits lines like "1. **topic** ..." — count
        // numbered rows under a section header.
        fn numbered_rows(s: &str, section_header: &str) -> usize {
            let mut in_section = false;
            let mut n = 0;
            for line in s.lines() {
                if line.starts_with("### ") {
                    in_section = line.contains(section_header);
                    continue;
                }
                if in_section
                    && !line.starts_with("> ")
                    && !line.trim().is_empty()
                    && line
                        .trim_start()
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit())
                {
                    n += 1;
                }
            }
            n
        }

        let compact_mem = numbered_rows(&compact, "Memories");
        let full_mem = numbered_rows(&full, "Memories");
        assert_eq!(
            compact_mem, 6,
            "compact must cap memories at 6, got {compact_mem}"
        );
        assert!(
            full_mem >= compact_mem,
            "full must show at least as many memories as compact (compact={compact_mem} full={full_mem})"
        );

        let compact_cp = numbered_rows(&compact, "Recent checkpoints");
        assert!(
            compact_cp <= 2,
            "compact must cap checkpoints at 2, got {compact_cp}"
        );

        // Compact output should be substantially shorter than full output.
        assert!(
            compact.len() < full.len(),
            "compact ({} bytes) should be smaller than full ({} bytes)",
            compact.len(),
            full.len()
        );
    }
}
