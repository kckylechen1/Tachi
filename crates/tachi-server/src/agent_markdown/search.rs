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

// tachi#1201 k3 hardening (Wizard/sonnet): pin `format_search_memory_markdown`
// / `format_section_rows` rendering behavior for CJK, super-long, and
// markdown-special-symbol content in the `summary` field (the field routed
// through `compact_text_line`). These lock the *actual* current contract —
// a numbered/bulleted list, not a pipe-delimited table — so assertions here
// describe real observed behavior, not an imagined GFM-table cell contract.
// All expected GREEN on the base SHA (regression locks, not bug fixes).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_search_memory_markdown_truncates_long_cjk_summary_at_char_boundary() {
        let rows = serde_json::json!([
            {"topic": "t", "summary": "价".repeat(300), "path": "/p", "id": "m1"}
        ]);
        let out = format_search_memory_markdown("q", &rows);

        // format_section_rows truncates `summary` at 120 chars (including
        // ellipsis) regardless of the caller's own limit param.
        let expected_summary = format!("{}...", "价".repeat(117));
        let expected =
            format!("## Tachi memory search: \"q\"\n\n1. **t** `m1` ? `/p` - {expected_summary}");
        assert_eq!(out, expected);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn format_search_memory_markdown_caps_long_mixed_ascii_cjk_summary_at_120() {
        // "超长单行(数千字符混中英)": cutoff engineered to land inside the
        // CJK region, a few chars past the ascii prefix.
        let summary = format!("{}{}", "A".repeat(100), "中".repeat(200));
        let rows = serde_json::json!([
            {"topic": "t4", "summary": summary, "path": "/p4", "id": "m4"}
        ]);
        let out = format_search_memory_markdown("q", &rows);

        let expected_summary = format!("{}{}...", "A".repeat(100), "中".repeat(17));
        let expected =
            format!("## Tachi memory search: \"q\"\n\n1. **t4** `m4` ? `/p4` - {expected_summary}");
        assert_eq!(out, expected);
    }

    #[test]
    fn format_search_memory_markdown_summary_embedded_newline_stays_single_line_row() {
        let rows = serde_json::json!([
            {"topic": "t2", "summary": "first part\nsecond part", "path": "/p2", "id": "m2"}
        ]);
        let out = format_search_memory_markdown("q2", &rows);

        let expected =
            "## Tachi memory search: \"q2\"\n\n1. **t2** `m2` ? `/p2` - first part second part";
        assert_eq!(out, expected);
        assert_eq!(
            out.lines().count(),
            3,
            "row must render as a single physical line"
        );
    }

    #[test]
    fn format_search_memory_markdown_summary_markdown_special_symbols_pass_through_literal() {
        // `|`, backtick, `#` are NOT in md_escape's escape set (only
        // `*[]_` are) and this renderer is a numbered list, not a
        // pipe-delimited table — pin the real current behavior: these
        // symbols survive literally in the summary field with no
        // corruption of the single-line-per-row contract.
        let rows = serde_json::json!([
            {"topic": "t3", "summary": "a | b ` c # d", "path": "/p3", "id": "m3"}
        ]);
        let out = format_search_memory_markdown("q3", &rows);

        let expected = "## Tachi memory search: \"q3\"\n\n1. **t3** `m3` ? `/p3` - a | b ` c # d";
        assert_eq!(out, expected);
    }

    #[test]
    fn format_search_memory_markdown_empty_rows_cjk_query_falls_back_to_no_results() {
        let rows = serde_json::json!([]);
        let out = format_search_memory_markdown("中文查询没有结果", &rows);

        assert_eq!(
            out,
            "## Tachi memory search: \"中文查询没有结果\"\n\n_No results._"
        );
    }
}
