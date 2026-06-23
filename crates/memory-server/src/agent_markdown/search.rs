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
