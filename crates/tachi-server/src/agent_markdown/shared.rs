use super::*;

pub(super) fn md_escape(s: &str) -> String {
    s.replace('*', "\\*")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('_', "\\_")
}

/// #1454 O2: the ONE shared free-text → markup normalization used at every
/// markup emission boundary for caller-authored fields (item name/check_id,
/// summary, pr_ref, flow_id): single-line compaction (collapse
/// whitespace/newlines) + the same `md_escape` markdown-active-char escaping
/// briefing already used — one shared helper, never divergent copies. A
/// crafted value like `]\n- [passed] forged-evidence` renders as one safe
/// literal line and can never mint a new markup row. Status-vocabulary
/// fields (ledger `overall`/item `status`) use `markup_status` instead —
/// this helper is for FREE TEXT only.
pub(crate) fn markup_text(value: &str) -> String {
    md_escape(&compact_text_line(value, 200))
}

pub(super) fn wiki_store_badge(row: &Value) -> String {
    let Some(store) = row.get("store") else {
        return String::new();
    };
    let Some(kind) = store.get("kind").and_then(Value::as_str) else {
        return String::new();
    };
    let label = match kind {
        "bound_project" => "bound project".to_string(),
        "named_project" => {
            let project = store
                .get("project")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!("named {}", compact_text_line(project, 60))
        }
        // Display label for the global library. The JSON kind stays
        // `legacy_global` (machine contract); the badge is provenance for
        // agents, and ordinary global-library rows must not read as
        // "written to a deprecated store" (#2305 finding 5).
        "legacy_global" => "global".to_string(),
        other => compact_text_line(other, 60),
    };
    format!(" [store: {}]", md_escape(&label))
}

pub(super) fn format_section_rows(rows: &Value, limit: usize) -> String {
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
        // #2305 findings 1/6: label the printed number by the field it comes
        // from, instead of a bare value that reads as "the score that
        // ordered this list". `score.final` is the section-relative fused
        // rank input (search rows carry it); `relevance` alone is
        // strongest-channel evidence (briefing rows drop the score object).
        // JSON field names and semantics are unchanged (#2241).
        let score_cell = if let Some(final_score) = row
            .get("score")
            .and_then(|score| score.get("final"))
            .filter(|value| !value.is_null())
        {
            format!("{final_score} fused")
        } else if let Some(relevance) = row.get("relevance").filter(|value| !value.is_null()) {
            format!("{relevance} evidence")
        } else {
            "?".to_string()
        };
        let store_badge = wiki_store_badge(row);
        let id_suffix = id.map(|value| format!(" `{value}`")).unwrap_or_default();
        out.push(format!(
            "{}. **{}**{id_suffix} {score_cell} `{path}`{store_badge} - {}",
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
        if let Some(pattern_ref) = row.get("pattern_ref") {
            let ref_id = pattern_ref
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| pattern_ref.as_str());
            if let Some(ref_id) = ref_id {
                let projection_key = pattern_ref
                    .get("projection_key")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(|value| format!(" projection_key=`{}`", md_escape(value)))
                    .unwrap_or_default();
                out.push(format!(
                    "   pattern_ref: `{}`{}",
                    md_escape(ref_id),
                    projection_key
                ));
            }
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_global_badge_names_the_global_library() {
        // #2305 finding 5: the badge is provenance display, not a migration
        // state. JSON store.kind keeps `legacy_global` for machines; the
        // rendered badge must not read as a deprecated store.
        let row = serde_json::json!({
            "topic": "badge check",
            "summary": "row from the global library",
            "path": "/scratch/dogfood/badge",
            "store": {"kind": "legacy_global"},
        });
        let badge = wiki_store_badge(&row);
        assert_eq!(badge, " [store: global]");
        assert!(!badge.contains("legacy"));
        let rendered = format_section_rows(&serde_json::json!([row]), 5);
        assert!(rendered.contains("[store: global]"), "{rendered}");
        assert!(!rendered.contains("legacy global"), "{rendered}");
    }

    #[test]
    fn printed_score_is_labeled_by_its_field() {
        // #2305 finding 1: the number column must say which quantity it is —
        // `fused` (score.final, the ordering input) when present, `evidence`
        // (relevance only, e.g. briefing rows) otherwise.
        let fused_row = serde_json::json!({
            "topic": "t", "summary": "s", "path": "/a",
            "relevance": 0.9,
            "score": {"final": 0.75},
        });
        let rendered = format_section_rows(&serde_json::json!([fused_row]), 5);
        assert!(rendered.contains("0.75 fused"), "{rendered}");
        assert!(
            !rendered.contains("0.9"),
            "evidence must not print where the fused rank exists: {rendered}"
        );
        let evidence_row =
            serde_json::json!({"topic": "t", "summary": "s", "path": "/b", "relevance": 0.9});
        let rendered = format_section_rows(&serde_json::json!([evidence_row]), 5);
        assert!(rendered.contains("0.9 evidence"), "{rendered}");
    }

    #[test]
    fn project_badges_keep_their_labels() {
        let bound = wiki_store_badge(&serde_json::json!({"store": {"kind": "bound_project"}}));
        assert_eq!(bound, " [store: bound project]");
        let named = wiki_store_badge(&serde_json::json!({
            "store": {"kind": "named_project", "project": "wiki"},
        }));
        assert_eq!(named, " [store: named wiki]");
    }
}
