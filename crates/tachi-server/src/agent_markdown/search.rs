use super::*;

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

/// Render raw `search_memory` MCP-tool rows as markdown (tachi#1201 k3's
/// default when `format` is omitted). Reuses the same row renderer as the
/// `tachi_search` facade's "Memory" section so `search_memory`'s markdown
/// output stays visually consistent with that sibling surface. `rows` is
/// already capped by `top_k` (max `MAX_SEARCH_TOP_K`), so this never
/// truncates further.
pub(crate) fn format_search_memory_markdown(query: &str, rows: &Value) -> String {
    format!(
        "## Tachi memory search: \"{query}\"\n\n{}",
        format_section_rows(rows, usize::MAX)
    )
}
