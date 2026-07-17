use super::*;

fn obsidian_file_stem(entry: &MemoryEntry) -> String {
    let topic = entry.topic.trim();
    if !topic.is_empty() {
        return sanitize_safe_path_name(topic);
    }
    let summary = entry.summary.trim();
    if !summary.is_empty() {
        return sanitize_safe_path_name(summary);
    }
    sanitize_safe_path_name(&entry.id)
}

fn yaml_string_list(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".to_string();
    }
    let items = values
        .iter()
        .map(|value| format!("\"{}\"", value.replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

fn obsidian_link_entities(text: &str, entities: &[String]) -> String {
    let mut sorted = entities
        .iter()
        .map(|entity| entity.trim())
        .filter(|entity| !entity.is_empty())
        .collect::<Vec<_>>();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    sorted.dedup();

    let mut out = String::with_capacity(text.len());
    let mut idx = 0usize;
    while idx < text.len() {
        if text[idx..].starts_with("[[") {
            if let Some(end) = text[idx + 2..].find("]]") {
                let end_idx = idx + 2 + end + 2;
                out.push_str(&text[idx..end_idx]);
                idx = end_idx;
                continue;
            }
        }

        let mut matched: Option<&str> = None;
        for entity in &sorted {
            if text[idx..].starts_with(*entity) && is_entity_boundary(text, idx, idx + entity.len())
            {
                matched = Some(entity);
                break;
            }
        }
        if let Some(entity) = matched {
            out.push_str("[[");
            out.push_str(entity);
            out.push_str("]]");
            idx += entity.len();
        } else if let Some(ch) = text[idx..].chars().next() {
            out.push(ch);
            idx += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

fn is_entity_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = if start == 0 {
        None
    } else {
        text[..start].chars().next_back()
    };
    let after = if end >= text.len() {
        None
    } else {
        text[end..].chars().next()
    };
    !before.is_some_and(is_entity_word_char) && !after.is_some_and(is_entity_word_char)
}

fn is_entity_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '-'
}

fn reference_line(ref_str: &str) -> String {
    if ref_str.starts_with("http://")
        || ref_str.starts_with("https://")
        || ref_str.starts_with("file://")
    {
        format!("- [{ref_str}]({ref_str})\n")
    } else {
        format!("- `{ref_str}`\n")
    }
}

/// #1072 RED case 7: Obsidian export must preserve BOTH the legacy
/// `metadata.source_refs: string[]` (unchanged, rendered exactly as before
/// this leaf) AND the new typed `metadata.evidence_refs_v1` refs (canon doc
/// §7.1: "exporters render the typed `ref` field when present") during the
/// dual-write migration window — never one in place of the other.
fn append_references_section(body: &mut String, metadata: &serde_json::Value) {
    if let Some(refs) = metadata.get("source_refs").and_then(|v| v.as_array()) {
        if !refs.is_empty() {
            body.push_str("\n\n## References\n\n");
            for ref_val in refs {
                let Some(ref_str) = ref_val.as_str() else {
                    continue;
                };
                body.push_str(&reference_line(ref_str));
            }
        }
    }
    if let Some(typed_refs) = metadata.get("evidence_refs_v1").and_then(|v| v.as_array()) {
        if !typed_refs.is_empty() {
            body.push_str("\n\n## Evidence Refs (typed)\n\n");
            for typed_ref in typed_refs {
                let Some(ref_str) = typed_ref.get("ref").and_then(|v| v.as_str()) else {
                    continue;
                };
                let kind = typed_ref
                    .get("target_kind")
                    .and_then(|v| v.as_str())
                    .map(|kind| format!(" ({kind})"))
                    .unwrap_or_default();
                let mut line = reference_line(ref_str);
                line.truncate(line.trim_end_matches('\n').len());
                body.push_str(&line);
                body.push_str(&kind);
                body.push('\n');
            }
        }
    }
}

fn markdown_for_obsidian(entry: &MemoryEntry) -> String {
    let mut body = String::new();
    body.push_str("---\n");
    body.push_str(&format!("id: \"{}\"\n", entry.id.replace('"', "\\\"")));
    body.push_str(&format!("importance: {}\n", entry.importance));
    body.push_str(&format!(
        "keywords: {}\n",
        yaml_string_list(&entry.keywords)
    ));
    body.push_str(&format!(
        "entities: {}\n",
        yaml_string_list(&entry.entities)
    ));
    body.push_str(&format!("tags: {}\n", yaml_string_list(&entry.keywords)));
    body.push_str(&format!(
        "timestamp: \"{}\"\n",
        entry.timestamp.replace('"', "\\\"")
    ));
    body.push_str(&format!(
        "category: \"{}\"\n",
        entry.category.replace('"', "\\\"")
    ));
    body.push_str("---\n\n");
    body.push_str(&obsidian_link_entities(&entry.text, &entry.entities));
    if !entry.entities.is_empty() {
        body.push_str("\n\n## See Also\n");
        for entity in &entry.entities {
            body.push_str(&format!("- [[{}]]\n", entity));
        }
    }
    append_references_section(&mut body, &entry.metadata);
    body
}

pub(crate) fn export_wiki_obsidian(
    server: &MemoryServer,
    project: &str,
    output: &Path,
) -> Result<Value, String> {
    let (entries, _) = list_wiki_entries(server, project, 100_000)?;

    std::fs::create_dir_all(output).map_err(|e| format!("create export dir: {e}"))?;

    let mut index: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut exported = 0usize;
    for entry in entries {
        if entry.path == "/wiki/_log" {
            continue;
        }
        if !is_user_facing_wiki_entry(&entry) {
            continue;
        }
        let relative_dir = entry
            .path
            .trim_start_matches("/wiki")
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(sanitize_safe_path_name)
            .collect::<PathBuf>();
        let target_dir = output.join(&relative_dir);
        std::fs::create_dir_all(&target_dir).map_err(|e| format!("create entry dir: {e}"))?;
        let file_stem = obsidian_file_stem(&entry);
        let file_name = format!("{file_stem}.md");
        let file_path = target_dir.join(&file_name);
        std::fs::write(&file_path, markdown_for_obsidian(&entry))
            .map_err(|e| format!("write export file {}: {e}", file_path.display()))?;
        index
            .entry(entry.path.clone())
            .or_default()
            .push((file_name, entry.summary.clone()));
        exported += 1;
    }

    let mut index_md = String::from("# Wiki Index\n\n");
    for (path, files) in &index {
        index_md.push_str(&format!("## {path}\n"));
        for (file, summary) in files {
            let link = file.trim_end_matches(".md");
            if summary.is_empty() {
                index_md.push_str(&format!("- [[{link}]]\n"));
            } else {
                index_md.push_str(&format!("- [[{link}]] - {summary}\n"));
            }
        }
        index_md.push('\n');
    }
    let index_path = output.join("_index.md");
    std::fs::write(&index_path, index_md)
        .map_err(|e| format!("write index {}: {e}", index_path.display()))?;

    append_wiki_log(
        server,
        "export",
        &format!("obsidian | {} entry(s) -> {}", exported, output.display()),
    );

    Ok(json!({
        "status": "completed",
        "format": "obsidian",
        "output": output,
        "count": exported,
        "index": index_path,
    }))
}
