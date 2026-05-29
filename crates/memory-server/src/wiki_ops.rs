use super::*;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use memory_core::scorer::local_pagerank;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

const WIKI_LOG_MAX_BYTES: usize = 256 * 1024;
const WIKI_LOG_MAX_ENTRIES: usize = 200;
const WIKI_LOG_ENTRY_MAX_BYTES: usize = 4096;

fn default_checks() -> Vec<String> {
    vec![
        "orphans".to_string(),
        "contradictions".to_string(),
        "stale".to_string(),
        "missing_edges".to_string(),
        "dirty_data".to_string(),
        "duplicates".to_string(),
    ]
}

fn parse_rfc3339_utc(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn tokenize_for_similarity(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn token_cosine_similarity(a: &str, b: &str) -> f64 {
    let mut freq_a: HashMap<String, f64> = HashMap::new();
    let mut freq_b: HashMap<String, f64> = HashMap::new();
    for token in tokenize_for_similarity(a) {
        *freq_a.entry(token).or_insert(0.0) += 1.0;
    }
    for token in tokenize_for_similarity(b) {
        *freq_b.entry(token).or_insert(0.0) += 1.0;
    }
    if freq_a.is_empty() || freq_b.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let norm_a = freq_a.values().map(|v| v * v).sum::<f64>().sqrt();
    let norm_b = freq_b.values().map(|v| v * v).sum::<f64>().sqrt();
    for (token, value_a) in &freq_a {
        if let Some(value_b) = freq_b.get(token) {
            dot += value_a * value_b;
        }
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

fn contradiction_score(a: &str, b: &str) -> f64 {
    let negations = [
        "never", "not", "avoid", "disable", "forbid", "against", "cannot",
    ];
    let affirmations = ["always", "use", "enable", "allow", "prefer", "should"];
    let a_lower = a.to_ascii_lowercase();
    let b_lower = b.to_ascii_lowercase();
    let a_neg = negations.iter().any(|token| a_lower.contains(token));
    let b_neg = negations.iter().any(|token| b_lower.contains(token));
    let a_aff = affirmations.iter().any(|token| a_lower.contains(token));
    let b_aff = affirmations.iter().any(|token| b_lower.contains(token));
    if (a_neg && b_aff) || (b_neg && a_aff) {
        token_cosine_similarity(a, b)
    } else {
        0.0
    }
}

fn extract_skill_content(cap: &HubCapability) -> Option<String> {
    let def: Value = serde_json::from_str(&cap.definition).ok()?;
    def.get("content")
        .and_then(|v| v.as_str())
        .or_else(|| def.get("prompt").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

fn extract_skill_path(cap: &HubCapability) -> Option<String> {
    let def: Value = serde_json::from_str(&cap.definition).ok()?;
    def.get("skill_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn set_skill_quality_metadata(def: &mut Value, patch: Value) {
    if !def.is_object() {
        *def = json!({});
    }
    let Some(obj) = def.as_object_mut() else {
        return;
    };
    let quality = obj.entry("quality_guard").or_insert_with(|| json!({}));
    if !quality.is_object() {
        *quality = json!({});
    }
    if let (Some(target), Some(source)) = (quality.as_object_mut(), patch.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn latest_snapshot_for_skill(store: &mut MemoryStore, skill_path: &str) -> Option<MemoryEntry> {
    let root = format!("{}/distilled", skill_path.trim_end_matches('/'));
    store
        .list_by_path(&root, 50, false)
        .ok()?
        .into_iter()
        .max_by(|a, b| a.timestamp.cmp(&b.timestamp))
}

fn relation_exists(
    edges: &[memory_core::MemoryEdge],
    a: &str,
    b: &str,
    relation: Option<&str>,
) -> bool {
    edges.iter().any(|edge| {
        let matches_nodes = (edge.source_id == a && edge.target_id == b)
            || (edge.source_id == b && edge.target_id == a);
        let matches_relation = relation.map(|rel| edge.relation == rel).unwrap_or(true);
        matches_nodes && matches_relation
    })
}

fn wiki_ingest_local_file_allowed(source_path: &Path) -> bool {
    if std::env::var("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    {
        return true;
    }

    let canonical_source = match std::fs::canonicalize(source_path) {
        Ok(path) => path,
        Err(_) => return false,
    };
    let cwd = std::env::current_dir().ok();
    let home = dirs::home_dir();
    let mut roots = Vec::new();
    if let Some(cwd) = cwd {
        roots.push(cwd);
    }
    for env_key in ["TACHI_HOME", "SIGIL_HOME"] {
        if let Ok(path) = std::env::var(env_key) {
            roots.push(PathBuf::from(path));
        }
    }
    if let Some(home) = home {
        roots.push(home.join(".tachi"));
    }

    roots
        .into_iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .any(|root| canonical_source.starts_with(root))
}

async fn source_for_path(source: &str) -> Result<String, String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let response = reqwest::get(source)
            .await
            .map_err(|e| format!("fetch source URL: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "fetch source URL failed with status {}",
                response.status()
            ));
        }
        response
            .text()
            .await
            .map_err(|e| format!("read source response: {e}"))
    } else {
        let path = Path::new(source);
        if !wiki_ingest_local_file_allowed(path) {
            return Err(
                "local wiki ingest is restricted to the current workspace or TACHI_HOME; set TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE=1 to override"
                    .to_string(),
            );
        }
        tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("read source file: {e}"))
    }
}

fn compact_entry(entry: &MemoryEntry) -> Value {
    json!({
        "id": entry.id,
        "path": entry.path,
        "summary": entry.summary,
    })
}

fn find_related_by_entities(
    server: &MemoryServer,
    project: &str,
    entities: &[String],
    exclude_id: &str,
    limit: usize,
) -> Vec<Value> {
    let entity_set: HashSet<String> = entities
        .iter()
        .map(|entity| entity.trim().to_ascii_lowercase())
        .filter(|entity| !entity.is_empty())
        .collect();
    if entity_set.is_empty() || limit == 0 {
        return Vec::new();
    }

    let entries = list_related_candidates(server, project, 5000).unwrap_or_default();

    let mut related = entries
        .into_iter()
        .filter(is_user_facing_wiki_entry)
        .filter(|entry| entry.id != exclude_id)
        .filter(|entry| {
            entry
                .entities
                .iter()
                .any(|entity| entity_set.contains(&entity.trim().to_ascii_lowercase()))
        })
        .collect::<Vec<_>>();
    related.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.timestamp.cmp(&a.timestamp))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut seen = HashSet::new();
    related
        .into_iter()
        .filter(|entry| seen.insert(entry.id.clone()))
        .take(limit)
        .map(|entry| compact_entry(&entry))
        .collect()
}

fn list_related_candidates(
    server: &MemoryServer,
    project: &str,
    limit: usize,
) -> Result<Vec<MemoryEntry>, String> {
    server
        .with_named_project_store_read(project, |store| {
            store
                .list_by_path("/wiki", limit, false)
                .map_err(|e| format!("wiki list: {e}"))
        })
        .map(|entries| {
            entries
                .into_iter()
                .filter(is_user_facing_wiki_entry)
                .collect()
        })
        .or_else(|_| {
            server
                .with_global_store_read(|store| {
                    store
                        .list_by_path("/wiki", limit, false)
                        .map_err(|e| format!("wiki fallback list: {e}"))
                })
                .map(|entries| {
                    entries
                        .into_iter()
                        .filter(is_user_facing_wiki_entry)
                        .collect()
                })
        })
}

fn is_user_facing_wiki_entry(entry: &MemoryEntry) -> bool {
    entry.path != "/wiki/_log"
        && !entry
            .metadata
            .get("wiki_log")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn list_wiki_entries(
    server: &MemoryServer,
    project: &str,
    limit: usize,
) -> Result<(Vec<MemoryEntry>, &'static str), String> {
    match server.with_named_project_store_read(project, |store| {
        store
            .list_by_path("/wiki", limit, false)
            .map_err(|e| format!("wiki list: {e}"))
    }) {
        Ok(entries) => Ok((
            entries
                .into_iter()
                .filter(is_user_facing_wiki_entry)
                .collect(),
            "named",
        )),
        Err(named_err) => server
            .with_global_store_read(|store| {
                store
                    .list_by_path("/wiki", limit, false)
                    .map_err(|e| format!("wiki fallback list: {e}"))
            })
            .map(|entries| {
                (
                    entries
                        .into_iter()
                        .filter(is_user_facing_wiki_entry)
                        .collect(),
                    "global",
                )
            })
            .map_err(|fallback_err| format!("{named_err}; {fallback_err}")),
    }
}

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
    body
}

fn string_list_from_value(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::trim))
                .filter(|item| !item.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn derive_ingest_fallback(source: &str, topic_hint: Option<&str>, content: &str) -> Value {
    let title = topic_hint
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            content
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(|line| {
                    line.trim()
                        .trim_start_matches('#')
                        .trim()
                        .chars()
                        .take(80)
                        .collect()
                })
        })
        .unwrap_or_else(|| {
            Path::new(source)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("ingested-source")
                .to_string()
        });
    let keywords = topic_hint
        .map(|topic| {
            topic
                .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let entities = topic_hint
        .filter(|topic| !topic.trim().is_empty())
        .map(|topic| vec![topic.trim().to_string()])
        .unwrap_or_default();
    json!({
        "title": title,
        "topic": topic_hint.unwrap_or("ingest"),
        "summary": content.chars().take(100).collect::<String>(),
        "keywords": keywords,
        "entities": entities,
    })
}

async fn extract_ingest_metadata(
    server: &MemoryServer,
    source: &str,
    topic_hint: Option<&str>,
    content: &str,
) -> Value {
    #[cfg(test)]
    {
        let _ = server;
        derive_ingest_fallback(source, topic_hint, content)
    }

    #[cfg(not(test))]
    {
        let system = "Extract wiki ingestion metadata. Return JSON only with keys: title, topic, summary, keywords, entities.";
        let user = format!(
            "Source: {source}\nTopic hint: {}\n\nContent:\n{}",
            topic_hint.unwrap_or(""),
            content.chars().take(8000).collect::<String>()
        );
        match server
            .llm
            .call_extract_llm(system, &user, None, 0.2, 800)
            .await
        {
            Ok(response) => match crate::llm::LlmClient::extract_json_payload(&response)
                .ok()
                .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
            {
                Some(value) => value,
                None => derive_ingest_fallback(source, topic_hint, content),
            },
            Err(_) => derive_ingest_fallback(source, topic_hint, content),
        }
    }
}

pub(crate) async fn handle_wiki_ingest(
    server: &MemoryServer,
    params: TachiWikiIngestParams,
) -> Result<String, String> {
    let content = source_for_path(&params.source).await?;
    if content.trim().is_empty() {
        append_wiki_log(
            server,
            "ingest",
            &format!("{} | skipped empty source", params.source),
        );
        return serde_json::to_string(&json!({
            "status": "skipped",
            "reason": "empty_source",
            "source": params.source,
        }))
        .map_err(|e| format!("serialize wiki_ingest: {e}"));
    }

    let metadata =
        extract_ingest_metadata(server, &params.source, params.topic.as_deref(), &content).await;
    let title = metadata
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Ingested Source")
        .to_string();
    let topic = metadata
        .get("topic")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or(params.topic.clone())
        .unwrap_or_else(|| "ingest".to_string());
    let summary = metadata
        .get("summary")
        .and_then(Value::as_str)
        .map(|value| value.chars().take(120).collect::<String>())
        .unwrap_or_else(|| content.chars().take(100).collect());
    let mut keywords = string_list_from_value(metadata.get("keywords"));
    if !keywords.iter().any(|keyword| keyword == "ingest") {
        keywords.push("ingest".to_string());
    }
    let entities = string_list_from_value(metadata.get("entities"));
    let path = format!("/wiki/general/{}", sanitize_safe_path_name(&topic));
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = Utc::now().to_rfc3339();

    let entry = MemoryEntry {
        id: id.clone(),
        path: path.clone(),
        summary: summary.clone(),
        text: content.clone(),
        importance: 0.8,
        timestamp: timestamp.clone(),
        valid_from: String::new(),
        valid_until: None,
        category: "experience".to_string(),
        topic: topic.clone(),
        keywords,
        persons: Vec::new(),
        entities: entities.clone(),
        location: String::new(),
        source: "wiki".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({
            "wiki": true,
            "wiki_title": title.clone(),
            "ingest_source": params.source.clone(),
            "allow_cross_project": true,
        }),
        vector: None,
        retention_policy: Some("permanent".to_string()),
        domain: Some("wiki".to_string()),
    };

    server.with_named_project_store("wiki", |store| {
        let old_id = {
            let mut stmt = store
                .connection()
                .prepare(
                    "SELECT id FROM memories 
                     WHERE (path = ?1 OR (domain = 'wiki' AND topic = ?2)) 
                       AND archived = 0 
                       AND superseded_by IS NULL 
                     LIMIT 1",
                )
                .map_err(|e| format!("prepare wiki duplicate query failed: {e}"))?;
            let mut rows = stmt
                .query_map((&path, &topic), |row| row.get::<_, String>(0))
                .map_err(|e| format!("query wiki duplicate failed: {e}"))?;
            if let Some(row) = rows.next() {
                Some(row.map_err(|e| format!("read wiki duplicate row failed: {e}"))?)
            } else {
                None
            }
        };

        store
            .upsert(&entry)
            .map_err(|e| format!("wiki ingest save: {e}"))?;

        if let Some(old_id) = old_id {
            store
                .supersede_memory(&old_id, &id)
                .map_err(|e| format!("supersede old wiki failed: {e}"))?;
            store
                .archive_memory(&old_id)
                .map_err(|e| format!("archive old wiki failed: {e}"))?;
        }
        Ok(())
    })?;

    let mut related = Vec::new();
    if params.update_related {
        related = find_related_by_entities(server, "wiki", &entities, &id, 10);
        for related_entry in &related {
            let Some(target_id) = related_entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            let edge = memory_core::MemoryEdge {
                source_id: id.clone(),
                target_id: target_id.to_string(),
                relation: "references".to_string(),
                weight: 0.6,
                metadata: json!({
                    "wiki_ingest": true,
                    "shared_entities": entities.clone(),
                }),
                created_at: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_to: None,
            };
            let _ = server.with_named_project_store("wiki", |store| {
                store
                    .add_edge(&edge)
                    .map_err(|e| format!("wiki ingest edge: {e}"))
            });
        }
    }

    server.enqueue_enrichment(crate::enrichment::build_enrichment_item(
        &entry,
        true,
        false,
        DbScope::Project,
        Some("wiki".to_string()),
        None,
        None,
        None,
        1,
    ));

    append_wiki_log(
        server,
        "ingest",
        &format!("{} | created {} at {}", params.source, id, path),
    );

    serde_json::to_string(&json!({
        "status": "created",
        "id": id,
        "path": path,
        "title": title,
        "summary": summary,
        "related_entries": related,
    }))
    .map_err(|e| format!("serialize wiki_ingest: {e}"))
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

pub(crate) fn append_wiki_log(server: &MemoryServer, operation: &str, details: &str) {
    let now = Utc::now().to_rfc3339();
    let log_line = format!(
        "## [{}] {} | {}",
        now,
        operation,
        compact_log_details(details.trim())
    );
    let entry = MemoryEntry {
        id: "wiki-operation-log".to_string(),
        path: "/wiki/_log".to_string(),
        summary: "Wiki operation log".to_string(),
        text: log_line.clone(),
        importance: 0.3,
        timestamp: now,
        valid_from: String::new(),
        valid_until: None,
        category: "other".to_string(),
        topic: "wiki_log".to_string(),
        keywords: vec!["wiki".to_string(), "log".to_string()],
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "mcp".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({"wiki_log": true}),
        vector: None,
        retention_policy: Some("durable".to_string()),
        domain: Some("wiki".to_string()),
    };

    let append_result = server.with_named_project_store("wiki", |store| {
        let mut entry = entry.clone();
        if let Some(existing) = store
            .get("wiki-operation-log")
            .map_err(|e| format!("wiki_log get: {e}"))?
        {
            entry.text = compact_wiki_log(&existing.text, &log_line);
            entry.revision = existing.revision;
        }
        store
            .upsert(&entry)
            .map_err(|e| format!("wiki_log upsert: {e}"))
    });

    if append_result.is_err() {
        let _ = server.with_global_store(|store| {
            let mut entry = entry;
            entry.metadata = json!({"wiki_log": true, "fallback_db": "global"});
            if let Some(existing) = store
                .get("wiki-operation-log")
                .map_err(|e| format!("wiki_log fallback get: {e}"))?
            {
                entry.text = compact_wiki_log(&existing.text, &log_line);
                entry.revision = existing.revision;
            }
            store
                .upsert(&entry)
                .map_err(|e| format!("wiki_log fallback upsert: {e}"))
        });
    }
}

fn compact_wiki_log(existing: &str, new_line: &str) -> String {
    let mut entries = existing
        .split("\n\n")
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    entries.push(new_line.trim().to_string());
    if entries.len() > WIKI_LOG_MAX_ENTRIES {
        entries.drain(0..entries.len() - WIKI_LOG_MAX_ENTRIES);
    }
    while entries.join("\n\n").len() > WIKI_LOG_MAX_BYTES && entries.len() > 1 {
        entries.remove(0);
    }
    entries.join("\n\n")
}

fn compact_log_details(details: &str) -> String {
    if details.len() <= WIKI_LOG_ENTRY_MAX_BYTES {
        return details.to_string();
    }
    let marker = "... [truncated]";
    let mut end = WIKI_LOG_ENTRY_MAX_BYTES.saturating_sub(marker.len());
    while end > 0 && !details.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &details[..end], marker)
}

#[derive(Clone)]
struct SkillQualitySnapshot {
    cap: HubCapability,
    def: Value,
    content: String,
    latest_snapshot: Option<MemoryEntry>,
}

fn run_skill_quality_guards_for_scope(
    server: &MemoryServer,
    scope: DbScope,
) -> Result<Value, String> {
    let mut snapshots: Vec<SkillQualitySnapshot> =
        server.with_store_for_scope_read(scope, |store| {
            let caps = store
                .hub_list(Some("skill"), false)
                .map_err(|e| format!("hub list skills: {e}"))?;
            let mut out = Vec::new();
            for cap in caps {
                let Some(content) = extract_skill_content(&cap) else {
                    continue;
                };
                let def: Value =
                    serde_json::from_str(&cap.definition).unwrap_or_else(|_| json!({}));
                let skill_path = extract_skill_path(&cap);
                let latest_snapshot = skill_path
                    .as_deref()
                    .and_then(|path| latest_snapshot_for_skill(store, path));
                out.push(SkillQualitySnapshot {
                    cap,
                    def,
                    content,
                    latest_snapshot,
                });
            }
            Ok(out)
        })?;

    let now = Utc::now();
    let mut merge_map: HashMap<String, Vec<Value>> = HashMap::new();
    let mut graph_edges = Vec::<memory_core::MemoryEdge>::new();

    for i in 0..snapshots.len() {
        for j in (i + 1)..snapshots.len() {
            let similarity = token_cosine_similarity(&snapshots[i].content, &snapshots[j].content);
            if similarity > 0.92 {
                merge_map
                    .entry(snapshots[i].cap.id.clone())
                    .or_default()
                    .push(json!({
                        "skill_id": snapshots[j].cap.id,
                        "similarity": similarity,
                    }));
                merge_map
                    .entry(snapshots[j].cap.id.clone())
                    .or_default()
                    .push(json!({
                        "skill_id": snapshots[i].cap.id,
                        "similarity": similarity,
                    }));

                if let (Some(left), Some(right)) = (
                    snapshots[i].latest_snapshot.as_ref(),
                    snapshots[j].latest_snapshot.as_ref(),
                ) {
                    graph_edges.push(memory_core::MemoryEdge {
                        source_id: left.id.clone(),
                        target_id: right.id.clone(),
                        relation: "merge_hint".to_string(),
                        weight: similarity.clamp(0.0, 1.0),
                        metadata: json!({
                            "source": "skill_quality_guard",
                            "type": "merge_hint",
                            "similarity": similarity,
                        }),
                        created_at: now.to_rfc3339(),
                        valid_from: String::new(),
                        valid_to: None,
                    });
                }
            }
        }
    }

    if !graph_edges.is_empty() {
        let edges = graph_edges.clone();
        let _ = server.with_store_for_scope(scope, |store| {
            for edge in &edges {
                store
                    .add_edge(edge)
                    .map_err(|e| format!("skill graph edge: {e}"))?;
            }
            Ok(())
        });
    }

    let pagerank = local_pagerank(&graph_edges, 0.85);
    let mut archived_skills = Vec::<String>::new();
    let mut changed_caps = Vec::<HubCapability>::new();

    for snapshot in &mut snapshots {
        let merge_hints = merge_map.get(&snapshot.cap.id).cloned().unwrap_or_default();
        let pagerank_score = snapshot
            .latest_snapshot
            .as_ref()
            .and_then(|memory| pagerank.get(&memory.id).copied())
            .unwrap_or(0.0);
        let mut new_def = snapshot.def.clone();
        set_skill_quality_metadata(
            &mut new_def,
            json!({
                "merge_hints": merge_hints,
                "pagerank": pagerank_score,
                "updated_at": now.to_rfc3339(),
            }),
        );

        let stale_cutoff = now - ChronoDuration::days(30);
        let should_archive = snapshot.cap.avg_rating < 0.3
            && snapshot
                .cap
                .last_used
                .as_deref()
                .and_then(parse_rfc3339_utc)
                .map(|ts| ts < stale_cutoff)
                .unwrap_or(false);
        if should_archive {
            if !new_def.is_object() {
                new_def = json!({});
            }
            if let Some(obj) = new_def.as_object_mut() {
                let policy = obj.entry("policy").or_insert_with(|| json!({}));
                if !policy.is_object() {
                    *policy = json!({});
                }
                if let Some(policy_obj) = policy.as_object_mut() {
                    policy_obj.insert("visibility".to_string(), json!("hidden"));
                }
            }
            set_skill_quality_metadata(
                &mut new_def,
                json!({
                    "status": "archived",
                    "archived_reason": "stale_low_rating",
                    "archived_at": now.to_rfc3339(),
                }),
            );
            archived_skills.push(snapshot.cap.id.clone());
        }

        let serialized = serde_json::to_string(&new_def)
            .map_err(|e| format!("serialize skill quality def: {e}"))?;
        if serialized != snapshot.cap.definition {
            let mut updated = snapshot.cap.clone();
            updated.definition = serialized;
            changed_caps.push(updated);
        }
    }

    if !changed_caps.is_empty() {
        let caps_to_store = changed_caps.clone();
        server.with_store_for_scope(scope, |store| {
            for cap in &caps_to_store {
                store
                    .hub_register(cap)
                    .map_err(|e| format!("hub register skill quality update: {e}"))?;
            }
            Ok(())
        })?;

        for cap in &changed_caps {
            if capability_callable(cap) && should_expose_skill_tool(cap) {
                let _ = server.register_skill_tool(cap);
            } else {
                let _ = server.unregister_skill_tool(&cap.id);
            }
        }
    }

    Ok(json!({
        "scope": scope.as_str(),
        "archived_skills": archived_skills,
        "merge_hints": merge_map,
        "pagerank": pagerank,
        "updated_caps": changed_caps.iter().map(|cap| cap.id.clone()).collect::<Vec<_>>(),
    }))
}

pub(crate) fn refresh_skill_quality_guards(server: &MemoryServer) -> Result<Value, String> {
    let global = run_skill_quality_guards_for_scope(server, DbScope::Global)?;
    let project = if server.has_project_db() {
        Some(run_skill_quality_guards_for_scope(
            server,
            DbScope::Project,
        )?)
    } else {
        None
    };
    Ok(json!({"global": global, "project": project}))
}

// ─── Wiki Search ────────────────────────────────────────────────────────────

/// Wiki category prefixes for quick lookup. Resolves short names to full paths.
fn resolve_wiki_category(category: &str) -> String {
    let trimmed = category.trim().trim_start_matches('/');
    // Already a full wiki path
    if trimmed.starts_with("wiki/") || trimmed.starts_with("wiki\\") {
        return format!("/{}", trimmed);
    }
    // Short alias → full path
    match trimmed.to_ascii_lowercase().as_str() {
        "quant" | "trading" => "/wiki/quant".to_string(),
        "quant/strategy" | "strategy" => "/wiki/quant/strategy".to_string(),
        "quant/stock-analysis" | "stock-analysis" | "stock" => {
            "/wiki/quant/stock-analysis".to_string()
        }
        "quant/portfolio" | "portfolio" => "/wiki/quant/portfolio".to_string(),
        "quant/market-analysis" | "market-analysis" | "market" => {
            "/wiki/quant/market-analysis".to_string()
        }
        "quant/data-pipeline" | "data-pipeline" | "data" => "/wiki/quant/data-pipeline".to_string(),
        "quant/autoresearch" | "autoresearch" => "/wiki/quant/autoresearch".to_string(),
        "engineering" | "eng" | "code" | "coding" => "/wiki/engineering".to_string(),
        "engineering/architecture" | "architecture" | "arch" => {
            "/wiki/engineering/architecture".to_string()
        }
        "engineering/devops" | "devops" => "/wiki/engineering/devops".to_string(),
        "engineering/debugging" | "debugging" | "debug" => {
            "/wiki/engineering/debugging".to_string()
        }
        "engineering/code-review" | "code-review" | "review" => {
            "/wiki/engineering/code-review".to_string()
        }
        "agent" => "/wiki/agent".to_string(),
        "agent/tachi" | "tachi" => "/wiki/agent/tachi".to_string(),
        "agent/openclaw" | "openclaw" => "/wiki/agent/openclaw".to_string(),
        "agent/evolution" | "evolution" => "/wiki/agent/evolution".to_string(),
        "product" => "/wiki/product".to_string(),
        "product/hyperion" | "hyperion" => "/wiki/product/hyperion".to_string(),
        "product/crimson-alphard" | "crimson-alphard" | "crimson" => {
            "/wiki/product/crimson-alphard".to_string()
        }
        "misc" => "/wiki/misc".to_string(),
        other => format!("/wiki/{}", other),
    }
}

/// All known wiki top-level categories for browse stats.
const WIKI_CATEGORIES: &[&str] = &[
    "/wiki/quant/strategy",
    "/wiki/quant/stock-analysis",
    "/wiki/quant/portfolio",
    "/wiki/quant/market-analysis",
    "/wiki/quant/data-pipeline",
    "/wiki/quant/autoresearch",
    "/wiki/engineering/architecture",
    "/wiki/engineering/devops",
    "/wiki/engineering/debugging",
    "/wiki/engineering/code-review",
    "/wiki/agent/tachi",
    "/wiki/agent/openclaw",
    "/wiki/agent/evolution",
    "/wiki/product/hyperion",
    "/wiki/product/crimson-alphard",
    "/wiki/misc",
];

pub(crate) async fn handle_wiki_search(
    server: &MemoryServer,
    params: WikiSearchParams,
) -> Result<String, String> {
    if params.query.trim().is_empty() {
        return Ok("## Wiki search\n\n_Skipped: empty query._".to_string());
    }

    let path_prefix = params
        .category
        .as_deref()
        .map(resolve_wiki_category)
        .or_else(|| params.path_prefix.clone())
        .or_else(|| Some("/wiki".to_string()));
    let ctx = crate::db_context::describe_db_context(
        server,
        params.project.as_deref(),
        params.domain.as_deref(),
    );

    let rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k: params.top_k.max(1).min(50),
            path_prefix,
            include_archived: params.include_archived,
            candidates_per_channel: params.top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: params.weights.or_else(|| {
                Some(HybridWeightsParam {
                    semantic: 0.48,
                    fts: 0.30,
                    symbolic: 0.20,
                    decay: 0.02,
                    use_rrf: true,
                })
            }),
            agent_role: params.agent_role,
            project: params.project,
            domain: params.domain,
            file_context: params.file_context,
            error_context: params.error_context,
            enable_rerank: false,
            as_of: None,
        },
    )
    .await?;

    append_wiki_log(
        server,
        "search",
        &format!("{} | {} result(s)", params.query, rows.len()),
    );

    Ok(crate::agent_markdown::format_wiki_search(
        &ctx,
        &params.query,
        rows.len(),
        &serde_json::Value::Array(rows),
    ))
}

pub(crate) fn handle_wiki_browse(
    server: &MemoryServer,
    params: WikiBrowseParams,
) -> Result<String, String> {
    let project_name = params.project;

    match params.category.as_deref() {
        None | Some("") => {
            let mut categories = Vec::new();
            let mut total = 0usize;
            let all_entries =
                list_related_candidates(server, &project_name, 5000).unwrap_or_default();

            for &cat_path in WIKI_CATEGORIES {
                let cat_prefix = format!("{cat_path}/");
                let count = all_entries
                    .iter()
                    .filter(|entry| entry.path == cat_path || entry.path.starts_with(&cat_prefix))
                    .count();
                if count > 0 {
                    categories.push((cat_path.to_string(), count));
                    total += count;
                }
            }

            append_wiki_log(
                server,
                "browse",
                &format!("stats | {} categor(ies), {} total", categories.len(), total),
            );

            Ok(crate::agent_markdown::format_wiki_browse_stats(
                total,
                &categories,
            ))
        }
        Some(category) => {
            let resolved_path = resolve_wiki_category(category);
            let limit = params.limit.max(1).min(500);

            let (entries, _) = list_wiki_entries(server, &project_name, 5000)?;
            let resolved_prefix = format!("{resolved_path}/");
            let slim_entries: Vec<Value> = entries
                .into_iter()
                .filter(|entry| {
                    entry.path == resolved_path || entry.path.starts_with(&resolved_prefix)
                })
                .take(limit)
                .map(|entry| {
                    json!({
                        "path": entry.path,
                        "summary": entry.summary,
                        "importance": entry.importance,
                    })
                })
                .collect();

            append_wiki_log(
                server,
                "browse",
                &format!("{} | {} entry(s)", resolved_path, slim_entries.len()),
            );

            Ok(crate::agent_markdown::format_wiki_browse_category(
                &resolved_path,
                &slim_entries,
            ))
        }
    }
}

// ─── Wiki Read ──────────────────────────────────────────────────────────────

pub(crate) fn handle_wiki_read(
    server: &MemoryServer,
    path: &str,
    project: &str,
) -> Result<String, String> {
    let resolved = if path.trim().starts_with('/') {
        let trimmed = path.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err("Wiki path cannot be root '/' — specify a concrete path like /wiki/my-topic".to_string());
        }
        trimmed.to_string()
    } else {
        resolve_wiki_category(path)
    };

    let (entries, _) = list_wiki_entries(server, project, 5000)?;
    let entry = entries.iter().find(|e| e.path == resolved).or_else(|| {
        let prefix = format!("{resolved}/");
        entries.iter().find(|e| e.path.starts_with(&prefix))
    });

    match entry {
        Some(entry) => {
            append_wiki_log(server, "read", &resolved);
            Ok(crate::agent_markdown::format_wiki_read(&json!({
                "path": entry.path,
                "text": entry.text,
                "summary": entry.summary,
                "importance": entry.importance,
                "keywords": entry.keywords,
                "entities": entry.entities,
                "topic": entry.topic,
                "timestamp": entry.timestamp,
            })))
        }
        None => Ok(format!(
            "## Wiki read\n\n_No entry found at `{resolved}`._\n\nUse `tachi_wiki(action=\"search\")` or `tachi_wiki(action=\"browse\")` to find entries."
        )),
    }
}

// ─── Wiki Lint ──────────────────────────────────────────────────────────────

/// Count-only wiki hygiene for agent alerts/briefing — never runs skill-quality guards.
pub(crate) async fn wiki_hygiene_counts(
    server: &MemoryServer,
) -> Result<serde_json::Value, String> {
    let lint = handle_wiki_lint(
        server,
        WikiLintParams {
            path_prefix: Some("/wiki".to_string()),
            checks: vec![
                "orphans".to_string(),
                "stale".to_string(),
                "duplicates".to_string(),
            ],
            limit: 25,
            stale_days: 90,
            missing_edge_threshold: 0.72,
            contradiction_threshold: 0.75,
            include_skill_quality: false,
        },
    )
    .await?;
    let parsed: serde_json::Value = serde_json::from_str(&lint).unwrap_or_else(|_| json!({}));
    Ok(json!({
        "orphans": parsed.get("orphans").and_then(|v| v.as_array()).map(|rows| rows.len()).unwrap_or(0),
        "stale_nodes": parsed.get("stale_nodes").and_then(|v| v.as_array()).map(|rows| rows.len()).unwrap_or(0),
        "duplicates": parsed.get("duplicates").and_then(|v| v.as_array()).map(|rows| rows.len()).unwrap_or(0),
    }))
}

pub(crate) async fn handle_wiki_lint(
    server: &MemoryServer,
    params: WikiLintParams,
) -> Result<String, String> {
    let checks: Vec<String> = if params.checks.is_empty() {
        default_checks()
    } else {
        params.checks.clone()
    };
    let path_prefix = params.path_prefix.as_deref().unwrap_or("/wiki");
    let limit = params.limit.max(1).min(500);
    let stale_cutoff = Utc::now() - ChronoDuration::days(params.stale_days as i64);

    let mut nodes: Vec<(MemoryEntry, DbScope)> = Vec::new();
    let global_entries = server.with_global_store_read(|store| {
        store
            .list_by_path(path_prefix, limit, false)
            .map_err(|e| format!("wiki_lint global list: {e}"))
    })?;
    nodes.extend(
        global_entries
            .into_iter()
            .map(|entry| (entry, DbScope::Global)),
    );
    if server.has_project_db() {
        let project_entries = server.with_project_store_read(|store| {
            store
                .list_by_path(path_prefix, limit, false)
                .map_err(|e| format!("wiki_lint project list: {e}"))
        })?;
        nodes.extend(
            project_entries
                .into_iter()
                .map(|entry| (entry, DbScope::Project)),
        );
    }

    let mut orphans = Vec::new();
    let mut stale_nodes = Vec::new();
    let mut contradiction_candidates = Vec::new();
    let mut missing_edge_hints = Vec::new();
    let mut dirty_data = Vec::new();
    let mut duplicates = Vec::new();

    let mut all_edges = Vec::<memory_core::MemoryEdge>::new();
    for (entry, scope) in &nodes {
        let edges = if *scope == DbScope::Global {
            server.with_global_store_read(|store| {
                store
                    .get_edges(&entry.id, "both", None)
                    .map_err(|e| format!("wiki_lint get edges: {e}"))
            })?
        } else {
            server.with_project_store_read(|store| {
                store
                    .get_edges(&entry.id, "both", None)
                    .map_err(|e| format!("wiki_lint get edges: {e}"))
            })?
        };
        if checks.iter().any(|check| check == "orphans") && edges.is_empty() {
            orphans.push(json!({
                "id": entry.id,
                "path": entry.path,
                "db": scope.as_str(),
            }));
        }
        all_edges.extend(edges);
        if checks.iter().any(|check| check == "stale") {
            if let Some(ts) = parse_rfc3339_utc(&entry.timestamp) {
                if ts < stale_cutoff
                    && !matches!(
                        entry.retention_policy.as_deref(),
                        Some("permanent" | "pinned")
                    )
                {
                    stale_nodes.push(json!({
                        "id": entry.id,
                        "path": entry.path,
                        "timestamp": entry.timestamp,
                        "db": scope.as_str(),
                    }));
                }
            }
        }
        if checks.iter().any(|check| check == "dirty_data")
            && (entry.text.contains("<think）")
                || entry.summary.contains("<think）")
                || entry.text.contains("<think>")
                || entry.summary.contains("<think>"))
        {
            dirty_data.push(json!({
                "id": entry.id,
                "path": entry.path,
                "issue": "think_tag_leak",
                "db": scope.as_str(),
            }));
        }
    }

    if checks
        .iter()
        .any(|check| check == "contradictions" || check == "missing_edges" || check == "duplicates")
    {
        // Cap pairwise comparison to avoid O(n²) blowup on large wikis.
        // At 500 nodes the nested loop produces ≤124,750 pairs, which is
        // fast enough for an interactive lint call.
        const PAIRWISE_NODE_CAP: usize = 500;
        let nodes_for_pairwise = &nodes[..nodes.len().min(PAIRWISE_NODE_CAP)];
        for i in 0..nodes_for_pairwise.len() {
            for j in (i + 1)..nodes_for_pairwise.len() {
                let left = &nodes_for_pairwise[i].0;
                let right = &nodes_for_pairwise[j].0;
                if nodes_for_pairwise[i].1 != nodes_for_pairwise[j].1 {
                    continue;
                }
                let similarity = token_cosine_similarity(&left.text, &right.text);
                if checks.iter().any(|check| check == "missing_edges")
                    && similarity > params.missing_edge_threshold
                    && !relation_exists(&all_edges, &left.id, &right.id, None)
                {
                    missing_edge_hints.push(json!({
                        "left_id": left.id,
                        "right_id": right.id,
                        "left_path": left.path,
                        "right_path": right.path,
                        "similarity": similarity,
                        "db": nodes_for_pairwise[i].1.as_str(),
                    }));
                }
                if checks.iter().any(|check| check == "duplicates") && similarity > 0.95 {
                    duplicates.push(json!({
                        "left_id": left.id,
                        "right_id": right.id,
                        "left_path": left.path,
                        "right_path": right.path,
                        "similarity": similarity,
                        "db": nodes_for_pairwise[i].1.as_str(),
                    }));
                }
                if checks.iter().any(|check| check == "contradictions") {
                    let contradiction = contradiction_score(&left.text, &right.text);
                    if contradiction > params.contradiction_threshold {
                        contradiction_candidates.push(json!({
                            "left_id": left.id,
                            "right_id": right.id,
                            "left_path": left.path,
                            "right_path": right.path,
                            "score": contradiction,
                            "db": nodes_for_pairwise[i].1.as_str(),
                        }));
                    }
                }
            }
        }
    }

    let skill_quality = if params.include_skill_quality {
        refresh_skill_quality_guards(server)?
    } else {
        json!({ "skipped": true })
    };
    append_wiki_log(
        server,
        "lint",
        &format!("{} | {} node(s)", path_prefix, nodes.len()),
    );

    serde_json::to_string(&json!({
        "path_prefix": path_prefix,
        "checks": checks,
        "orphans": orphans,
        "stale_nodes": stale_nodes,
        "contradiction_candidates": contradiction_candidates,
        "missing_edge_hints": missing_edge_hints,
        "dirty_data": dirty_data,
        "duplicates": duplicates,
        "skill_quality": skill_quality,
    }))
    .map_err(|e| format!("serialize wiki_lint: {e}"))
}
