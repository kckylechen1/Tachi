use serde_json::{json, Value};

pub(in crate::copilot_ops) fn compact_rows(rows: Vec<Value>, limit: usize) -> Vec<Value> {
    compact_layer_rows(rows, limit, None, None)
}

pub(in crate::copilot_ops) fn compact_layer_rows(
    rows: Vec<Value>,
    limit: usize,
    default_layer: Option<&str>,
    default_authority: Option<&str>,
) -> Vec<Value> {
    rows.into_iter()
        .take(limit)
        .map(|row| {
            let metadata = row.get("metadata").unwrap_or(&Value::Null);
            let mut out = serde_json::Map::new();
            for key in ["id", "db", "path", "topic", "summary", "excerpt"] {
                out.insert(
                    key.to_string(),
                    row.get(key).cloned().unwrap_or(Value::Null),
                );
            }
            out.insert(
                "score".to_string(),
                row.get("score")
                    .or_else(|| row.get("relevance"))
                    .cloned()
                    .unwrap_or(Value::Null),
            );

            for key in ["layer", "scope", "authority", "status", "source_ref"] {
                let value = metadata
                    .get(key)
                    .or_else(|| row.get(key))
                    .cloned()
                    .unwrap_or_else(|| match key {
                        "layer" => default_layer.map_or(Value::Null, |value| json!(value)),
                        "authority" => default_authority.map_or(Value::Null, |value| json!(value)),
                        "scope" => row.get("db").cloned().unwrap_or(Value::Null),
                        _ => Value::Null,
                    });
                if !value.is_null() {
                    out.insert(key.to_string(), value);
                }
            }

            for key in ["source_refs", "applies_to"] {
                if let Some(value) = metadata.get(key).or_else(|| row.get(key)) {
                    out.insert(key.to_string(), value.clone());
                }
            }

            let references = row.get("references").cloned().or_else(|| {
                let typed = metadata
                    .get("evidence_refs_v1")
                    .and_then(Value::as_array)
                    .map(|refs| {
                        refs.iter()
                            .filter_map(|value| value.get("ref").and_then(Value::as_str))
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .filter(|refs| !refs.is_empty());
                typed
                    .map(|refs| json!(refs))
                    .or_else(|| metadata.get("source_refs").cloned())
            });
            if let Some(references) = references {
                out.insert("references".to_string(), references);
            }

            Value::Object(out)
        })
        .collect()
}
