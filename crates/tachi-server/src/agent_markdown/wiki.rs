use super::*;

fn effective_artifact_badge(row: &Value) -> String {
    let Some(artifact) = row.get("effective_artifact") else {
        return String::new();
    };
    let scalar = |key: &str, fallback: &str| {
        artifact
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    };
    let list = |value: Option<&Value>| {
        value
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|value| compact_text_line(value, value.chars().count().saturating_add(1)))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
    };
    let mut parts = vec![
        format!("kind={}", scalar("artifact_kind", "wiki")),
        format!("scope={}", scalar("knowledge_scope", "unspecified")),
        format!("lifecycle={}", scalar("lifecycle", "pending_review")),
        format!("authority={}", scalar("authority", "advisory")),
        format!(
            "applicability={}",
            scalar("applicability_status", "unspecified")
        ),
    ];
    let origins = list(artifact.get("origin_projects"));
    parts.push(format!(
        "origin={}",
        if origins.is_empty() { "none" } else { &origins }
    ));
    let applies_to = artifact.get("applies_to");
    let mut restrictions = Vec::new();
    for axis in [
        "projects",
        "repos",
        "domains",
        "task_type",
        "profiles",
        "stage",
    ] {
        let values = list(applies_to.and_then(|value| value.get(axis)));
        if !values.is_empty() {
            restrictions.push(format!("{axis}={values}"));
        }
    }
    parts.push(format!(
        "applies={}",
        if restrictions.is_empty() {
            "none".to_string()
        } else {
            restrictions.join(",")
        }
    ));
    let exceptions = list(artifact.get("known_exceptions"));
    parts.push(format!(
        "exceptions={}",
        if exceptions.is_empty() {
            "none"
        } else {
            &exceptions
        }
    ));
    let issues = list(artifact.get("validation_issues"));
    if !issues.is_empty() {
        parts.push(format!("issues={issues}"));
    }
    format!(" [{}]", md_escape(&parts.join("; ")))
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
            // #1072: an explicit non-active lifecycle scope (see
            // `WikiSearchParams::lifecycle`) can still surface a draft row —
            // label it so it never reads as reviewed wiki truth.
            let lifecycle_badge = match row.get("lifecycle").and_then(Value::as_str) {
                Some(lifecycle) if lifecycle != "active" => format!(" [{lifecycle}]"),
                _ => String::new(),
            };
            let store_badge = wiki_store_badge(row);
            let artifact_badge = effective_artifact_badge(row);
            out.push(format!(
                "{}. {relevance} `{path}`{lifecycle_badge}{store_badge}{artifact_badge} - {}",
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
        // #1072 fix-round (#1215 BUG 6): Markdown provenance loss — browse
        // JSON carries `lifecycle`/`authority` but the rendered markdown
        // silently dropped them, so a non-active entry (reachable via an
        // explicit `lifecycle` scope) could read as plain reviewed content.
        // Mirror `format_wiki_search`'s badge convention.
        let lifecycle_badge = match entry.get("lifecycle").and_then(Value::as_str) {
            Some(lifecycle) if lifecycle != "active" => format!(" [{lifecycle}]"),
            _ => String::new(),
        };
        let store_badge = wiki_store_badge(entry);
        let artifact_badge = effective_artifact_badge(entry);
        out.push(format!(
            "{}. **`{entry_path}`**{lifecycle_badge}{store_badge}{artifact_badge} (importance {importance})\n   {}",
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

    let lifecycle = entry
        .get("lifecycle")
        .and_then(Value::as_str)
        .unwrap_or("active");

    let store_badge = wiki_store_badge(entry);
    let mut out = vec![format!("## Wiki: `{path}`{store_badge}")];
    // #1072 RED case 2/3: a non-`active` entry (e.g. a `pending_review`
    // draft reached by its exact path) must never render as plain reviewed
    // wiki content — the caller sees this before the body.
    if lifecycle != "active" {
        out.push(format!(
            "\n> ⚠️ lifecycle: **{lifecycle}** — not reviewed/active wiki truth."
        ));
    }
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
    let artifact_badge = effective_artifact_badge(entry);
    if !artifact_badge.is_empty() {
        out.push(format!("**artifact:**{}", artifact_badge));
    }
    out.push("\n---\n".to_string());
    out.push(text.to_string());
    out.join("\n")
}

pub(crate) fn format_wiki_read_ambiguity(path: &str, count: u64, candidates: &[Value]) -> String {
    let mut out = vec![
        "## Wiki read".to_string(),
        format!("\n_Ambiguous path `{path}` matched {count} entries._"),
    ];
    for candidate in candidates {
        let candidate_path = candidate
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(path);
        let id = candidate
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let store_badge = wiki_store_badge(candidate);
        let artifact_badge = effective_artifact_badge(candidate);
        out.push(format!(
            "- `{candidate_path}`{store_badge}{artifact_badge} (id `{id}`)"
        ));
    }
    out.push("\nUse `tachi_wiki(action=\"search\")` or read by a more specific path.".to_string());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shared_artifact_row() -> Value {
        json!({
            "id": "shared-guide",
            "path": "/guide/shared",
            "effective_artifact": {
                "artifact_kind": "guide",
                "knowledge_scope": "shared",
                "origin_projects": ["Quant_Analyzer_2026"],
                "applies_to": {
                    "projects": [],
                    "repos": ["kckylechen1/tachi"],
                    "domains": ["memory"],
                    "task_type": [],
                    "profiles": [],
                    "stage": ["implementation"]
                },
                "applicability_status": "bounded",
                "known_exceptions": ["legacy adapter"],
                "lifecycle": "active",
                "authority": "playbook"
            }
        })
    }

    #[test]
    fn markdown_exposes_complete_effective_artifact_semantics() {
        let row = shared_artifact_row();
        let badge = effective_artifact_badge(&row);
        for expected in [
            "kind=guide",
            "scope=shared",
            "authority=playbook",
            "origin=Quant\\_Analyzer\\_2026",
            "repos=kckylechen1/tachi",
            "domains=memory",
            "stage=implementation",
            "exceptions=legacy adapter",
        ] {
            assert!(badge.contains(expected), "missing {expected}: {badge}");
        }

        let ambiguity = format_wiki_read_ambiguity("/guide/shared", 1, &[row]);
        assert!(ambiguity.contains("kind=guide"));
        assert!(ambiguity.contains("repos=kckylechen1/tachi"));
    }
}
