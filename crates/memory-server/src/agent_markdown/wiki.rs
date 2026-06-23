use super::*;

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
