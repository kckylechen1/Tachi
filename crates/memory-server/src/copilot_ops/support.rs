use super::*;

pub(super) fn compact_rows(rows: Vec<Value>, limit: usize) -> Vec<Value> {
    compact_layer_rows(rows, limit, None, None)
}

pub(super) fn compact_layer_rows(
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

            Value::Object(out)
        })
        .collect()
}

pub(super) fn normalize_wiki_path(path: Option<String>, topic: &str) -> String {
    let raw = path
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("/wiki/general/{}", wiki_slug(topic)));
    let with_slash = if raw.starts_with('/') {
        raw
    } else {
        format!("/{raw}")
    };
    if with_slash == "/wiki"
        || with_slash.starts_with("/wiki/")
        || with_slash == "/guide"
        || with_slash.starts_with("/guide/")
    {
        with_slash
    } else {
        format!("/wiki{}", with_slash)
    }
}

pub(super) fn wiki_layer_metadata(
    path: &str,
    scope: &str,
    project: Option<&str>,
    references: &[String],
) -> Value {
    let layer = if path == "/guide" || path.starts_with("/guide/") {
        "guide"
    } else {
        "wiki"
    };
    let scope = if project.is_some() || scope.eq_ignore_ascii_case("project") {
        "project"
    } else {
        "global"
    };
    let authority = if layer == "guide" {
        "playbook"
    } else {
        "advisory"
    };
    json!({
        "layer": layer,
        "scope": scope,
        "authority": authority,
        "status": "active",
        "source_ref": references.first().cloned(),
    })
}

pub(super) fn wiki_text_tokens(input: &str) -> HashSet<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.chars().count() >= 3)
        .collect()
}

pub(super) fn wiki_subject_token(input: &str) -> Option<String> {
    let tokens = wiki_text_tokens(input);
    if tokens.len() == 1 {
        tokens.into_iter().next()
    } else {
        None
    }
}

pub(super) fn wiki_text_jaccard_sets(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

pub(super) fn find_wiki_entry_by_path_or_topic(
    store: &mut MemoryStore,
    path: &str,
    topic: &str,
) -> Result<Option<MemoryEntry>, String> {
    memory_core::db::find_active_wiki_entry_by_path_or_topic(store.connection(), path, topic)
        .map_err(|e| format!("wiki existing lookup: {e}"))
}

pub(super) fn with_existing_wiki_store<T>(
    server: &MemoryServer,
    project_name: &str,
    use_named_project: bool,
    f: impl FnOnce(&mut MemoryStore) -> Result<T, String>,
) -> Result<T, String> {
    if use_named_project {
        server.with_named_project_store(project_name, f)
    } else {
        server.with_global_store(f)
    }
}

pub(super) fn default_named_project_available(server: &MemoryServer, project_name: &str) -> bool {
    let Ok(db_path) = MemoryServer::resolve_named_project_db_path(project_name) else {
        return false;
    };
    let Some(app_home) = db_path
        .parent()
        .and_then(|project_dir| project_dir.parent())
        .and_then(|projects_dir| projects_dir.parent())
    else {
        return false;
    };
    server.global_db_path.starts_with(app_home)
}

/// Identify and supersede wiki entries that duplicate the newly written entry.
pub(super) fn supersede_wiki_duplicates(
    store: &mut MemoryStore,
    canonical_id: &str,
    path: &str,
    topic: &str,
    text: &str,
) -> Result<usize, String> {
    let parent_path = wiki_parent_path(path);
    let candidates = store
        .list_wiki_duplicate_candidates(path, topic, &parent_path, 500)
        .map_err(|e| format!("wiki duplicate scan: {e}"))?;
    let mut changed = 0usize;
    let target_subject = wiki_subject_token(topic);
    let target_text_tokens = wiki_text_tokens(text);
    for candidate in candidates {
        if candidate.id == canonical_id {
            continue;
        }
        // Dedup criteria (OR-combined, but single-token topic match requires path prefix overlap)
        let same_path = candidate.path == path;
        let same_topic = target_subject.as_ref().is_some_and(|token| {
            let cand_token = wiki_subject_token(&candidate.topic);
            cand_token.as_ref() == Some(token)
                // Single-token topics require path prefix overlap to avoid over-broad matching
                && (token.len() > 1
                    || candidate.path.rsplit_once('/').map(|(parent, _)| parent) == path.rsplit_once('/').map(|(parent, _)| parent))
        });
        let similar_text =
            wiki_text_jaccard_sets(&target_text_tokens, &wiki_text_tokens(&candidate.text))
                >= WIKI_DUP_JACCARD_THRESHOLD;
        let same_subject = same_path || same_topic || similar_text;
        if !same_subject {
            continue;
        }
        if store
            .supersede_memory(&candidate.id, canonical_id)
            .map_err(|e| format!("wiki duplicate supersede: {e}"))?
        {
            let edge = memory_core::MemoryEdge {
                source_id: canonical_id.to_string(),
                target_id: candidate.id.clone(),
                relation: "supersedes".to_string(),
                weight: 0.9,
                metadata: json!({
                    "source": "wiki_write_dedup",
                    "path": path,
                    "topic": topic,
                }),
                created_at: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_to: None,
            };
            let _ = store.add_edge(&edge);
            changed += 1;
        }
    }
    Ok(changed)
}

pub(super) fn wiki_parent_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/wiki")
        .to_string()
}

pub(super) fn tokenize_task(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_lowercase())
        .filter(|token| is_meaningful_skill_token(token))
        .collect()
}

pub(super) fn is_meaningful_skill_token(token: &str) -> bool {
    if token.chars().count() < 3 {
        return false;
    }
    const STOPWORDS: &[&str] = &[
        "fix", "fixed", "fixing", "repair", "resolve", "bug", "bugs", "issue", "issues", "problem",
        "problems", "error", "errors", "failed", "failure", "task", "work", "use", "using", "add",
        "update", "change", "修复", "问题", "错误", "失败", "任务",
    ];
    !STOPWORDS.contains(&token)
}

pub(super) fn tokenize_skill_text(input: &str) -> HashSet<String> {
    tokenize_task(input).into_iter().collect()
}

pub(super) fn score_capability(task_tokens: &[String], cap: &HubCapability) -> usize {
    let haystack = format!("{} {} {}", cap.id, cap.name, cap.description).to_ascii_lowercase();
    let cap_tokens = tokenize_skill_text(&haystack);
    let exact_matches = task_tokens
        .iter()
        .filter(|token| cap_tokens.contains(token.as_str()))
        .count();

    let long_substring_matches = task_tokens
        .iter()
        .filter(|token| token.len() >= 8 && haystack.contains(token.as_str()))
        .count();

    exact_matches * 3 + long_substring_matches
}

pub(super) fn recommend_skills_light(
    server: &MemoryServer,
    task: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let tokens = tokenize_task(task);
    let mut caps = server.with_global_store_read(|store| {
        store
            .hub_list(Some("skill"), false)
            .map_err(|e| format!("hub list global skills: {e}"))
    })?;
    if server.has_project_db() {
        let mut project_caps = server.with_project_store_read(|store| {
            store
                .hub_list(Some("skill"), false)
                .map_err(|e| format!("hub list project skills: {e}"))
        })?;
        caps.append(&mut project_caps);
    }

    let mut scored = caps
        .into_iter()
        .filter(|cap| cap.enabled && review_status_allows_call(&cap.review_status))
        .map(|cap| (score_capability(&tokens, &cap), cap))
        .filter(|(score, _)| *score >= 3)
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));

    Ok(scored
        .into_iter()
        .take(limit)
        .map(|(score, cap)| {
            json!({
                "id": cap.id,
                "name": cap.name,
                "description": cap.description,
                "score": score,
            })
        })
        .collect())
}

pub(super) fn feature_guide_hits(
    server: &MemoryServer,
    params: &TachiTaskParams,
    query: &str,
    current_stage: &str,
    route_recommendation: &Value,
    limit: usize,
) -> Vec<Value> {
    let profile = params.profile.as_deref().or_else(|| {
        route_recommendation
            .get("recommended_profile")
            .and_then(Value::as_str)
    });
    let stage = params.stage.as_deref().unwrap_or(current_stage);
    let task_type = params.task_type.as_deref();
    let query_tokens = tokenize_skill_text(query);
    let mut candidates = load_feature_guide_candidates(server, params, limit.max(20));

    candidates.retain(|(entry, _, _)| {
        entry.is_guide() && guide_applies_to(entry, task_type, profile, Some(stage), &query_tokens)
    });

    let mut scored = candidates
        .into_iter()
        .map(|(entry, scope, source)| {
            let score = score_feature_guide(&entry, task_type, profile, Some(stage), &query_tokens);
            (score, entry, scope, source)
        })
        .filter(|(score, entry, _, _)| *score > 0 || !guide_has_restrictive_applies_to(entry))
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.timestamp.cmp(&a.1.timestamp))
            .then_with(|| a.1.path.cmp(&b.1.path))
    });
    scored.truncate(limit);

    scored
        .into_iter()
        .map(|(score, entry, scope, source)| guide_hit_row(&entry, scope, source, score))
        .collect()
}

pub(super) fn load_feature_guide_candidates(
    server: &MemoryServer,
    params: &TachiTaskParams,
    limit: usize,
) -> Vec<(MemoryEntry, DbScope, &'static str)> {
    let limit = limit.clamp(1, 200);
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    if let Some(project) = params
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        if let Ok(entries) = server.with_named_project_store_read(project, |store| {
            store
                .list_by_path("/guide", limit, false)
                .map_err(|e| format!("guide named project list: {e}"))
        }) {
            merge_guide_candidates(&mut out, &mut seen, entries, DbScope::Project, "named");
        }
    } else if default_named_project_available(server, "wiki") {
        if let Ok(entries) = server.with_named_project_store_read("wiki", |store| {
            store
                .list_by_path("/guide", limit, false)
                .map_err(|e| format!("guide default wiki list: {e}"))
        }) {
            merge_guide_candidates(
                &mut out,
                &mut seen,
                entries,
                DbScope::Project,
                "default_wiki",
            );
        }
    }

    if server.has_project_db() && out.len() < limit {
        if let Ok(entries) = server.with_project_store_read(|store| {
            store
                .list_by_path("/guide", limit, false)
                .map_err(|e| format!("guide project list: {e}"))
        }) {
            merge_guide_candidates(&mut out, &mut seen, entries, DbScope::Project, "project");
        }
    }

    if out.len() < limit {
        if let Ok(entries) = server.with_global_store_read(|store| {
            store
                .list_by_path("/guide", limit, false)
                .map_err(|e| format!("guide global list: {e}"))
        }) {
            merge_guide_candidates(&mut out, &mut seen, entries, DbScope::Global, "global");
        }
    }

    out
}

pub(super) fn merge_guide_candidates(
    out: &mut Vec<(MemoryEntry, DbScope, &'static str)>,
    seen: &mut HashSet<String>,
    entries: Vec<MemoryEntry>,
    scope: DbScope,
    source: &'static str,
) {
    for entry in entries {
        if seen.insert(entry.id.clone()) {
            out.push((entry, scope, source));
        }
    }
}

pub(super) fn guide_applies_to(
    entry: &MemoryEntry,
    task_type: Option<&str>,
    profile: Option<&str>,
    stage: Option<&str>,
    query_tokens: &HashSet<String>,
) -> bool {
    let applies = entry.metadata.get("applies_to").unwrap_or(&Value::Null);
    guide_filter_matches(applies.get("task_type"), task_type, query_tokens)
        && guide_filter_matches(applies.get("profiles"), profile, query_tokens)
        && guide_filter_matches(applies.get("stage"), stage, query_tokens)
}

pub(super) fn guide_filter_matches(
    filter: Option<&Value>,
    actual: Option<&str>,
    query_tokens: &HashSet<String>,
) -> bool {
    let values = guide_string_values(filter.unwrap_or(&Value::Null));
    if values.is_empty() {
        return true;
    }
    if let Some(actual) = actual.map(|value| value.to_ascii_lowercase()) {
        if values
            .iter()
            .any(|value| value == "*" || value.eq_ignore_ascii_case(&actual))
        {
            return true;
        }
    }
    values.iter().any(|value| {
        query_tokens.contains(value)
            || query_tokens.contains(&value.replace('_', "-"))
            || query_tokens.contains(&value.replace('-', "_"))
    })
}

pub(super) fn score_feature_guide(
    entry: &MemoryEntry,
    task_type: Option<&str>,
    profile: Option<&str>,
    stage: Option<&str>,
    query_tokens: &HashSet<String>,
) -> usize {
    let applies = entry.metadata.get("applies_to").unwrap_or(&Value::Null);
    let mut score = 0usize;
    if !guide_string_values(applies.get("task_type").unwrap_or(&Value::Null)).is_empty()
        && guide_filter_matches(applies.get("task_type"), task_type, query_tokens)
    {
        score += 5;
    }
    if !guide_string_values(applies.get("profiles").unwrap_or(&Value::Null)).is_empty()
        && guide_filter_matches(applies.get("profiles"), profile, query_tokens)
    {
        score += 4;
    }
    if !guide_string_values(applies.get("stage").unwrap_or(&Value::Null)).is_empty()
        && guide_filter_matches(applies.get("stage"), stage, query_tokens)
    {
        score += 2;
    }

    let haystack = guide_hit_haystack(entry);
    let haystack_tokens = tokenize_skill_text(&haystack);
    score += query_tokens
        .iter()
        .filter(|token| haystack_tokens.contains(*token) || haystack.contains(token.as_str()))
        .count();
    score
}

pub(super) fn guide_has_restrictive_applies_to(entry: &MemoryEntry) -> bool {
    let applies = entry.metadata.get("applies_to").unwrap_or(&Value::Null);
    ["task_type", "profiles", "stage"]
        .iter()
        .any(|key| !guide_string_values(applies.get(*key).unwrap_or(&Value::Null)).is_empty())
}

pub(super) fn guide_hit_haystack(entry: &MemoryEntry) -> String {
    let metadata_keywords =
        guide_string_values(entry.metadata.get("keywords").unwrap_or(&Value::Null));
    let trigger_keywords = guide_string_values(
        entry
            .metadata
            .get("trigger_keywords")
            .unwrap_or(&Value::Null),
    );
    format!(
        "{} {} {} {} {} {}",
        entry.path,
        entry.topic,
        entry.summary,
        entry.text,
        entry.keywords.join(" "),
        metadata_keywords
            .into_iter()
            .chain(trigger_keywords)
            .collect::<Vec<_>>()
            .join(" ")
    )
    .to_ascii_lowercase()
}

pub(super) fn guide_string_values(value: &Value) -> Vec<String> {
    match value {
        Value::String(raw) => vec![raw.trim().to_ascii_lowercase()],
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|raw| raw.trim().to_ascii_lowercase())
            .filter(|raw| !raw.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn guide_hit_row(
    entry: &MemoryEntry,
    db_scope: DbScope,
    source: &'static str,
    score: usize,
) -> Value {
    let metadata = &entry.metadata;
    json!({
        "id": entry.id,
        "db": db_scope.as_str(),
        "source": source,
        "path": entry.path,
        "topic": if entry.topic.is_empty() { Value::Null } else { json!(entry.topic) },
        "summary": if entry.summary.is_empty() { Value::Null } else { json!(entry.summary) },
        "score": score,
        "layer": metadata.get("layer").and_then(Value::as_str).unwrap_or("guide"),
        "scope": metadata.get("scope").and_then(Value::as_str).unwrap_or(entry.scope.as_str()),
        "authority": metadata.get("authority").and_then(Value::as_str).unwrap_or("playbook"),
        "status": metadata.get("status").and_then(Value::as_str).unwrap_or("active"),
        "applies_to": metadata.get("applies_to").cloned().unwrap_or(Value::Null),
    })
}

pub(super) fn strip_numbered_prefix(line: &str) -> Option<&str> {
    let digits_len = line.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits_len == 0 {
        return None;
    }
    let rest = &line[digits_len..];
    rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
}

pub(super) fn normalize_checklist_item(raw: &str) -> Option<String> {
    let trimmed = raw
        .trim()
        .trim_matches(|ch: char| matches!(ch, '-' | '*' | '#' | ' ' | '\t'));
    if trimmed.is_empty() {
        return None;
    }

    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    let collapsed = collapsed.trim_end_matches(|ch: char| matches!(ch, '.' | ';' | ':' | ','));
    if collapsed.len() < 20 || collapsed.len() > 220 {
        return None;
    }
    Some(collapsed.to_string())
}

pub(super) fn extract_checklist_candidates(text: &str) -> Vec<String> {
    let mut structured = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let bullet = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| strip_numbered_prefix(trimmed));
        if let Some(item) = bullet.and_then(normalize_checklist_item) {
            structured.push(item);
        }
    }
    if !structured.is_empty() {
        return structured;
    }

    text.split(|ch: char| matches!(ch, '.' | '!' | '?' | '\n'))
        .filter_map(normalize_checklist_item)
        .collect()
}

pub(super) fn build_debug_checklist(wiki_rows: &[Value]) -> Vec<String> {
    let mut checklist = Vec::new();
    let mut seen = HashSet::new();

    for row in wiki_rows {
        let Some(path) = row.get("path").and_then(Value::as_str) else {
            continue;
        };
        if !path.starts_with("/wiki/") {
            continue;
        }

        let text_candidates = row
            .get("text")
            .and_then(Value::as_str)
            .map(extract_checklist_candidates)
            .unwrap_or_default();
        let summary_candidates = row
            .get("summary")
            .and_then(Value::as_str)
            .and_then(normalize_checklist_item)
            .into_iter()
            .collect::<Vec<_>>();

        for item in text_candidates
            .into_iter()
            .chain(summary_candidates.into_iter())
        {
            let key = item.to_ascii_lowercase();
            if seen.insert(key) {
                checklist.push(item);
            }
            if checklist.len() >= DEBUG_CHECKLIST_LIMIT {
                return checklist;
            }
        }
    }

    for item in FALLBACK_DEBUG_CHECKLIST {
        let key = item.to_ascii_lowercase();
        if seen.insert(key) {
            checklist.push(item.to_string());
        }
        if checklist.len() >= DEBUG_CHECKLIST_LIMIT {
            break;
        }
    }

    checklist
}
