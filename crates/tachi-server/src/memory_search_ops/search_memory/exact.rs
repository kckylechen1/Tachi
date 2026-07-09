use serde_json::json;

fn row_text_for_exact_match(row: &serde_json::Value) -> String {
    ["id", "path", "topic", "summary", "excerpt"]
        .into_iter()
        .filter_map(|key| row.get(key).and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase()
}

fn row_has_exact_token_match(query: &str, row: &serde_json::Value) -> bool {
    if row.get("match_type").and_then(serde_json::Value::as_str) == Some("exact_token") {
        return true;
    }
    if !memcore::scorer::is_id_like_exact_query(query) {
        return false;
    }
    row_text_for_exact_match(row).contains(&query.trim().to_ascii_lowercase())
}

pub(super) fn mark_exact_token_match(row: &mut serde_json::Value) {
    if let Some(obj) = row.as_object_mut() {
        obj.insert("match_type".into(), json!("exact_token"));
    }
}

pub(super) fn annotate_exact_token_matches(rows: &mut [serde_json::Value], query: &str) {
    for row in rows {
        if !row_has_exact_token_match(query, row) {
            continue;
        }
        mark_exact_token_match(row);
    }
}

pub(crate) fn has_high_confidence_exact_token_top(rows: &[serde_json::Value], query: &str) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    if !row_has_exact_token_match(query, first) {
        return false;
    }
    let fts = first
        .get("score")
        .and_then(|score| score.get("fts"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let symbolic = first
        .get("score")
        .and_then(|score| score.get("symbolic"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    fts >= 0.95 || symbolic >= 0.95
}
