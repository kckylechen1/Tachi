use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DEBUG_CHECKLIST_LIMIT: usize = 4;
const FALLBACK_DEBUG_CHECKLIST: [&str; DEBUG_CHECKLIST_LIMIT] = [
    "Start from the observed error and trace where the invariant first becomes false.",
    "For MCP argument bugs, verify schema -> client serialization -> server deserialization -> handler -> transport in that order.",
    "Do not keep patching the same layer after two failed attempts; reframe or ask another agent.",
    "If stderr/log visibility is weak, add a durable test or inspect the data structure at the API boundary.",
];

const WIKI_DUP_JACCARD_THRESHOLD: f64 = 0.85;

fn compact_rows(rows: Vec<Value>, limit: usize) -> Vec<Value> {
    compact_layer_rows(rows, limit, None, None)
}

fn compact_layer_rows(
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

fn normalize_wiki_path(path: Option<String>, topic: &str) -> String {
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

fn wiki_layer_metadata(
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

fn wiki_text_tokens(input: &str) -> HashSet<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.chars().count() >= 3)
        .collect()
}

fn wiki_subject_token(input: &str) -> Option<String> {
    let tokens = wiki_text_tokens(input);
    if tokens.len() == 1 {
        tokens.into_iter().next()
    } else {
        None
    }
}

fn wiki_text_jaccard_sets(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
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

fn find_wiki_entry_by_path_or_topic(
    store: &mut MemoryStore,
    path: &str,
    topic: &str,
) -> Result<Option<MemoryEntry>, String> {
    memory_core::db::find_active_wiki_entry_by_path_or_topic(store.connection(), path, topic)
        .map_err(|e| format!("wiki existing lookup: {e}"))
}

fn with_existing_wiki_store<T>(
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

fn default_named_project_available(server: &MemoryServer, project_name: &str) -> bool {
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
fn supersede_wiki_duplicates(
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

fn wiki_parent_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/wiki")
        .to_string()
}

fn tokenize_task(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_lowercase())
        .filter(|token| is_meaningful_skill_token(token))
        .collect()
}

fn is_meaningful_skill_token(token: &str) -> bool {
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

fn tokenize_skill_text(input: &str) -> HashSet<String> {
    tokenize_task(input).into_iter().collect()
}

fn score_capability(task_tokens: &[String], cap: &HubCapability) -> usize {
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

fn recommend_skills_light(
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

fn feature_guide_hits(
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

fn load_feature_guide_candidates(
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

fn merge_guide_candidates(
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

fn guide_applies_to(
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

fn guide_filter_matches(
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

fn score_feature_guide(
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

fn guide_has_restrictive_applies_to(entry: &MemoryEntry) -> bool {
    let applies = entry.metadata.get("applies_to").unwrap_or(&Value::Null);
    ["task_type", "profiles", "stage"]
        .iter()
        .any(|key| !guide_string_values(applies.get(*key).unwrap_or(&Value::Null)).is_empty())
}

fn guide_hit_haystack(entry: &MemoryEntry) -> String {
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

fn guide_string_values(value: &Value) -> Vec<String> {
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

fn guide_hit_row(
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

fn strip_numbered_prefix(line: &str) -> Option<&str> {
    let digits_len = line.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits_len == 0 {
        return None;
    }
    let rest = &line[digits_len..];
    rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
}

fn normalize_checklist_item(raw: &str) -> Option<String> {
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

fn extract_checklist_candidates(text: &str) -> Vec<String> {
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

fn build_debug_checklist(wiki_rows: &[Value]) -> Vec<String> {
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

pub(crate) async fn handle_tachi_wiki_write(
    server: &MemoryServer,
    params: WikiWriteParams,
) -> Result<String, String> {
    crate::wiki_ops::validate_references(&params.references)?;

    if !params.force && memory_core::is_noise_text(&params.text) {
        return serde_json::to_string(&json!({
            "saved": false,
            "noise": true,
            "reason": "Text detected as noise (greeting, denial, or meta-question). Not saved.",
            "hint": "Retry with force=true if this is intentional wiki content.",
        }))
        .map_err(|e| format!("serialize wiki_write noise response: {e}"));
    }

    let entry_text = params.text.clone();
    let topic = params
        .topic
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| wiki_slug(&params.title));
    let path = normalize_wiki_path(params.path.clone(), &topic);
    let requested_project = params.project.clone();
    let project_name = requested_project
        .clone()
        .unwrap_or_else(|| "wiki".to_string());
    let use_named_project =
        requested_project.is_some() || default_named_project_available(server, &project_name);
    let target_project = use_named_project.then(|| project_name.clone());
    let summary = params
        .summary
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| params.title.chars().take(100).collect());

    let mut keywords = params.keywords.clone();
    keywords.push("wiki".to_string());
    keywords.sort();
    keywords.dedup();

    // Wiki content's canonical home is a dedicated wiki project DB. Production
    // callers should pass `project: "wiki"`; if absent we still write where the
    // server routes us, but we set `metadata.allow_cross_project=true` so
    // path-routing validation lets `/wiki/...` through on non-wiki DBs.
    // Audit B11: this metadata flag is the explicit opt-in required by
    // path_router::validate_path_for_db.
    let references = params.references.clone();
    let layer_metadata =
        wiki_layer_metadata(&path, &params.scope, target_project.as_deref(), &references);
    let mut wiki_metadata = params.metadata.clone().unwrap_or_else(|| json!({}));
    if !wiki_metadata.is_object() {
        return Err("metadata must be a JSON object when supplied for wiki write".to_string());
    }
    if let Some(obj) = wiki_metadata.as_object_mut() {
        obj.insert("wiki".to_string(), json!(true));
        obj.insert("wiki_title".to_string(), json!(params.title.clone()));
        obj.insert("user_force".to_string(), json!(params.force));
        obj.insert("allow_cross_project".to_string(), json!(true));
        obj.insert("source_refs".to_string(), json!(references));
    }
    if let (Some(target), Some(layer)) = (wiki_metadata.as_object_mut(), layer_metadata.as_object())
    {
        for (key, value) in layer {
            target.insert(key.clone(), value.clone());
        }
    }

    let existing = with_existing_wiki_store(server, &project_name, use_named_project, |store| {
        find_wiki_entry_by_path_or_topic(store, &path, &topic)
    })?;
    if let Some(existing) = &existing {
        if let Some(obj) = wiki_metadata.as_object_mut() {
            obj.insert("wiki_update_of".to_string(), json!(existing.id));
            obj.insert(
                "wiki_previous_revision".to_string(),
                json!(existing.revision),
            );
        }
    }
    let update_id = existing.as_ref().map(|entry| entry.id.clone());
    let existing_revision = existing.as_ref().map(|entry| entry.revision).unwrap_or(1);

    let save_result = handle_save_memory(
        server,
        SaveMemoryParams {
            text: entry_text.clone(),
            summary,
            path: path.clone(),
            importance: params.importance.clamp(0.0, 1.0),
            category: params.category,
            topic: topic.clone(),
            keywords,
            persons: vec![],
            entities: params.entities,
            location: String::new(),
            scope: params.scope,
            vector: None,
            id: update_id.clone(),
            force: true,
            auto_link: true,
            project: target_project.clone(),
            retention_policy: Some(params.retention_policy),
            domain: params.domain.or_else(|| Some("wiki".to_string())),
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: Some(wiki_metadata),
        },
    )
    .await?;

    let mut response: Value =
        serde_json::from_str(&save_result).map_err(|e| format!("parse wiki save response: {e}"))?;
    if let Some(obj) = response.as_object_mut() {
        obj.insert("wiki_path".to_string(), json!(path));
        obj.insert("wiki_topic".to_string(), json!(topic));
        obj.insert(
            "wiki_write_mode".to_string(),
            json!(if update_id.is_some() {
                "updated"
            } else {
                "created"
            }),
        );
    }
    let canonical_id = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "wiki write response missing id".to_string())?
        .to_string();
    let duplicate_action = |store: &mut MemoryStore| {
        supersede_wiki_duplicates(store, &canonical_id, &path, &topic, &entry_text)
    };
    let duplicates_superseded = match with_existing_wiki_store(
        server,
        &project_name,
        use_named_project,
        duplicate_action,
    ) {
        Ok(count) => count,
        Err(err) => {
            tracing::warn!(wiki_path = %path, wiki_topic = %topic, error = %err, "wiki duplicate scan failed");
            0
        }
    };
    if let Some(obj) = response.as_object_mut() {
        obj.insert(
            "wiki_duplicates_superseded".to_string(),
            json!(duplicates_superseded),
        );
        if update_id.is_some() {
            obj.insert(
                "wiki_previous_revision".to_string(),
                json!(existing_revision),
            );
        }
    }
    crate::wiki_ops::append_wiki_log(
        server,
        "write",
        &format!(
            "{} | {} | {} duplicate(s) superseded",
            path,
            response
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            duplicates_superseded
        ),
    );
    serde_json::to_string(&response).map_err(|e| format!("serialize wiki_write: {e}"))
}

fn wiki_slug(input: &str) -> String {
    let mut output = String::new();
    let mut previous_was_sep = false;

    for ch in input.trim().chars() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '.') {
            output.push(ch);
            previous_was_sep = false;
        } else if matches!(
            ch,
            '-' | ' ' | '\t' | '\n' | '\r' | ':' | '：' | '/' | '\\' | '|'
        ) {
            if !output.is_empty() && !previous_was_sep {
                output.push('-');
                previous_was_sep = true;
            }
        }
    }

    let slug = output.trim_matches(|ch| matches!(ch, '.' | '_' | '-'));
    if slug.is_empty() {
        "unnamed".to_string()
    } else {
        slug.chars().take(96).collect()
    }
}

pub(crate) async fn handle_tachi_wiki_search(
    server: &MemoryServer,
    params: WikiSearchParams,
) -> Result<String, String> {
    let path_prefix = params.path_prefix.unwrap_or_else(|| "/wiki".to_string());
    let top_k = crate::clamp_facade_top_k(params.top_k);
    let mut rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some(path_prefix.clone()),
            include_training: false,
            include_archived: params.include_archived,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: None,
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: params.weights.or(Some(HybridWeightsParam {
                semantic: 0.48,
                fts: 0.30,
                symbolic: 0.20,
                decay: 0.02,
                use_rrf: true,
            })),
            agent_role: params.agent_role,
            project: params.project,
            domain: params.domain,
            file_context: params.file_context,
            error_context: params.error_context,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await?;
    crate::wiki_ops::filter_user_facing_wiki_rows(&mut rows);

    crate::wiki_ops::append_wiki_log(
        server,
        "search",
        &format!("{} | {} result(s)", params.query, rows.len()),
    );

    Ok(crate::agent_markdown::format_wiki_search(
        &params.query,
        rows.len(),
        &serde_json::Value::Array(rows),
    ))
}

pub(crate) async fn handle_tachi_task_brief(
    server: &MemoryServer,
    params: TaskBriefParams,
) -> Result<String, String> {
    let top_k = crate::clamp_facade_top_k(params.top_k);
    let mut wiki_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.task.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some("/wiki".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await?;
    crate::wiki_ops::filter_user_facing_wiki_rows(&mut wiki_rows);
    let memory_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.task.clone(),
            query_vec: None,
            top_k,
            path_prefix: params.path_prefix.clone(),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await?;
    let skills = recommend_skills_light(server, &params.task, 5).unwrap_or_default();
    let debug_checklist = build_debug_checklist(&wiki_rows);
    let routing = build_task_brief_routing(&params.task, &skills);

    let route_rec =
        build_route_recommendation(server, &params.task, params.project.as_deref()).await;
    let intent = routing.intent;
    let selected_sops = routing.selected_sops;
    let tool_plan = routing.tool_plan;

    serde_json::to_string(&json!({
        "status": "ok",
        "task": params.task,
        "agent_id": params.agent_id,
        "project": params.project,
        "wiki_hits": compact_rows(wiki_rows, top_k),
        "memory_hits": compact_rows(memory_rows, top_k),
        "intent": intent,
        "selected_sops": selected_sops,
        "tool_plan": tool_plan,
        "recommended_skills": skills,
        "debug_checklist": debug_checklist,
        "route_recommendation": route_rec,
        "suggested_next_tools": [
            "tachi_wiki(action='search')",
            "tachi_skill(action='discover')",
            "tachi_task(action='plan')",
            "tachi_task(action='board')"
        ],
    }))
    .map_err(|e| format!("serialize task_brief: {e}"))
}

pub(crate) async fn handle_tachi_feature_briefing(
    server: &MemoryServer,
    params: &TachiTaskParams,
) -> Result<String, String> {
    let top_k = if params.compact.unwrap_or(false) {
        params.top_k.unwrap_or(4).clamp(1, 4)
    } else {
        crate::clamp_facade_top_k(params.top_k.unwrap_or(6))
    };
    let query = feature_briefing_query(params);
    let board = feature_board(server, params, top_k).await;
    let project_work_record = project_work_records(params);
    let canonical_docs = canonical_doc_refs(params);
    let run_artifacts = feature_run_artifacts(params.flow_id.as_deref())?;

    let wiki_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some("/wiki".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: None,
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
        },
        false,
    )
    .await
    .unwrap_or_default();

    let memory_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k,
            path_prefix: params.path_prefix.clone(),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
        },
        !params.include_global,
    )
    .await
    .unwrap_or_default();

    let eval_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k: top_k.min(5),
            path_prefix: Some("/eval".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
        },
        !params.include_global,
    )
    .await
    .unwrap_or_default();

    let skills = recommend_skills_light(server, &query, 5).unwrap_or_default();
    let routing = build_task_brief_routing(&query, &skills);
    let route_recommendation = feature_dispatch_recommendation(server, params, &query);
    let current_stage = infer_feature_stage(&run_artifacts, &board);
    let guide_hits = feature_guide_hits(
        server,
        params,
        &query,
        &current_stage,
        &route_recommendation,
        top_k,
    );
    let feedback_profile = params.profile.as_deref().or_else(|| {
        route_recommendation
            .get("recommended_profile")
            .and_then(Value::as_str)
    });
    let feedback_stage = params
        .stage
        .clone()
        .unwrap_or_else(|| current_stage.clone());
    let feedback_rules = crate::feedback_rule_ops::applicable_feedback_rules(
        server,
        crate::feedback_rule_ops::FeedbackRuleQuery {
            task: query.clone(),
            task_type: params.task_type.clone(),
            profile: feedback_profile.map(str::to_string),
            stage: Some(feedback_stage),
            keywords: feature_needles(params),
            project: params.project.clone(),
        },
    )
    .await;
    let feedback_rules_trace = crate::feedback_rule_ops::feedback_rules_trace(&feedback_rules);
    let suggested_dispatch = suggested_feature_dispatch(params, &query, &route_recommendation);
    let relevant_profiles = relevant_feature_profiles(&route_recommendation);
    let next_action = feature_next_action(&canonical_docs, &run_artifacts, &board, &memory_rows);
    let open_loops = crate::shell_ops::scan_open_loops(8);
    let wiki_hits = compact_layer_rows(wiki_rows, top_k, Some("wiki"), Some("advisory"));
    let memory_fragments = compact_layer_rows(memory_rows, top_k, Some("memory"), Some("context"));
    let eval_evidence = compact_layer_rows(eval_rows, top_k.min(5), Some("eval"), Some("evidence"));
    let doc_index = build_feature_doc_index(
        &project_work_record,
        &canonical_docs,
        &wiki_hits,
        &guide_hits,
        &feedback_rules_trace,
        &eval_evidence,
        &run_artifacts,
    );
    let kind = if params.action.eq_ignore_ascii_case("doc_index") {
        "doc_index"
    } else {
        "feature_briefing"
    };
    let response = json!({
        "status": "ok",
        "kind": kind,
        "objective": params.task.clone().unwrap_or_else(|| query.clone()),
        "scope": {
            "project": params.project,
            "flow_id": params.flow_id,
            "issue_ref": params.issue_ref,
            "pr_ref": params.pr_ref,
            "cwd": params.cwd,
            "include_global": params.include_global,
        },
        "current_stage": current_stage,
        "project_work_record": project_work_record,
        "board_state": board,
        "canonical_docs": canonical_docs,
        "run_artifacts": run_artifacts,
        "guide_sop": {
            "intent": routing.intent,
            "selected_sops": routing.selected_sops,
            "tool_plan": routing.tool_plan,
            "recommended_skills": skills,
        },
        "route_recommendation": route_recommendation,
        "relevant_profiles": relevant_profiles,
        "suggested_dispatch": suggested_dispatch,
        "guide_hits": guide_hits,
        "wiki_hits": wiki_hits,
        "feedback_rules": feedback_rules_trace,
        "memory_fragments": memory_fragments,
        "eval_evidence": eval_evidence,
        "doc_index": doc_index,
        "next_action": next_action,
        "open_loops": open_loops,
        "layering": {
            "project_work_record": "GitHub issues/PRs and linked flow state; source of truth for active work",
            "docs": "canonical repo specs/design docs; source of truth for feature/API truth",
            "wiki": "project-specific durable decisions and lessons; advisory unless promoted back to docs/issues",
            "guide": "global workflow/SOP and skill loadout guidance; playbook authority",
            "feedback_rules": "behavior patches that shape future agent prompts",
            "eval": "verification and reviewer usefulness evidence",
            "runtime_artifacts": "arena/dispatch/run files; runtime state, not canonical product truth",
            "principle": "Project facts first. Global playbook second. Feedback rules and eval pitfalls as behavior patches."
        },
    });

    if crate::facade_memory_ops::wants_json(params.format.as_deref()) {
        serde_json::to_string(&response).map_err(|e| format!("serialize feature briefing: {e}"))
    } else {
        Ok(format_feature_briefing_markdown(&response))
    }
}

fn project_work_records(params: &TachiTaskParams) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    push_project_work_record(
        &mut out,
        &mut seen,
        "github_issue",
        params.issue_ref.as_deref(),
    );
    push_project_work_record(&mut out, &mut seen, "github_pr", params.pr_ref.as_deref());
    out
}

fn push_project_work_record(
    out: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    kind: &str,
    raw_ref: Option<&str>,
) {
    let Some(reference) = raw_ref.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if !seen.insert(format!("{kind}:{reference}")) {
        return;
    }
    out.push(json!({
        "kind": kind,
        "ref": reference,
        "layer": "github_ref",
        "authority": "project_work_record",
        "source_of_truth": true,
        "status": "ref_only",
        "retrieval": "Call tachi_task(action='intake') for issue snapshots or tachi_task(action='link_pr'/'pr_status') for PR state.",
    }));
}

fn build_feature_doc_index(
    project_work_record: &[Value],
    canonical_docs: &[Value],
    wiki_hits: &[Value],
    guide_hits: &[Value],
    feedback_rules: &Value,
    eval_evidence: &[Value],
    run_artifacts: &[Value],
) -> Value {
    let feedback_items = feedback_rules
        .get("rules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "authority_order": [
            "project_work_record",
            "canonical",
            "project_wiki",
            "global_guide",
            "feedback_rule",
            "eval",
            "runtime_artifact"
        ],
        "groups": [
            doc_index_group(
                "project_work_record",
                "github_ref",
                "project_work_record",
                "GitHub Issues/PRs remain the source of truth for active project work.",
                project_work_record,
            ),
            doc_index_group(
                "canonical_docs",
                "repo_doc_ref",
                "canonical",
                "Repo docs/specs define accepted design and API truth.",
                canonical_docs,
            ),
            doc_index_group(
                "project_wiki",
                "wiki",
                "advisory",
                "Project decisions and lessons are durable but do not override GitHub or repo docs.",
                wiki_hits,
            ),
            doc_index_group(
                "global_guide",
                "guide",
                "playbook",
                "Global guide entries apply by task_type/profile/stage as reusable workflow playbooks.",
                guide_hits,
            ),
            doc_index_group(
                "feedback_rules",
                "feedback_rule",
                "behavior_patch",
                "Feedback rules patch future agent behavior; they are not project facts.",
                &feedback_items,
            ),
            doc_index_group(
                "eval_evidence",
                "eval",
                "evidence",
                "Eval rows and reviewer findings are evidence for routing and verification.",
                eval_evidence,
            ),
            doc_index_group(
                "runtime_artifacts",
                "runtime_artifact",
                "runtime_state",
                "Arena/dispatch/run artifacts describe execution state and handoffs.",
                run_artifacts,
            ),
        ],
    })
}

fn doc_index_group(name: &str, layer: &str, authority: &str, rule: &str, items: &[Value]) -> Value {
    json!({
        "name": name,
        "layer": layer,
        "authority": authority,
        "rule": rule,
        "count": items.len(),
        "items": items,
    })
}

fn feature_briefing_query(params: &TachiTaskParams) -> String {
    params
        .task
        .as_deref()
        .or(params.issue_ref.as_deref())
        .or(params.pr_ref.as_deref())
        .or(params.flow_id.as_deref())
        .unwrap_or("current feature handoff")
        .to_string()
}

fn canonical_doc_refs(params: &TachiTaskParams) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if let Ok((flow_docs, flow_specs)) =
        crate::task_lifecycle::flow_status_doc_refs(params.flow_id.as_deref())
    {
        for path in flow_specs {
            push_doc_ref(
                &mut out,
                &mut seen,
                "flow_spec",
                &path,
                params.cwd.as_deref(),
            );
        }
        for path in flow_docs {
            push_doc_ref(
                &mut out,
                &mut seen,
                "flow_doc",
                &path,
                params.cwd.as_deref(),
            );
        }
    }
    for path in params
        .spec_paths
        .iter()
        .map(|path| ("spec", path))
        .chain(params.doc_paths.iter().map(|path| ("doc", path)))
    {
        push_doc_ref(&mut out, &mut seen, path.0, path.1, params.cwd.as_deref());
    }
    if let Some(task) = params.task.as_deref() {
        for path in extract_markdown_paths(task) {
            push_doc_ref(
                &mut out,
                &mut seen,
                "mentioned_doc",
                &path,
                params.cwd.as_deref(),
            );
        }
    }
    out
}

fn push_doc_ref(
    out: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    kind: &str,
    raw_path: &str,
    cwd: Option<&str>,
) {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() || !seen.insert(raw_path.to_string()) {
        return;
    }
    let resolved = resolve_workspace_path(raw_path, cwd);
    out.push(json!({
        "kind": kind,
        "path": raw_path,
        "exists": resolved.as_ref().is_some_and(|path| path.exists()),
        "resolved_path": resolved.map(|path| path.to_string_lossy().to_string()),
        "layer": "repo_doc_ref",
        "authority": "canonical",
        "source_of_truth": true,
    }));
}

fn extract_markdown_paths(text: &str) -> Vec<String> {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ')' | '(' | '[' | ']'))
        .map(|token| token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | ':' | ';')))
        .filter(|token| {
            token.ends_with(".md") && (token.starts_with("docs/") || token.contains("/docs/"))
        })
        .map(str::to_string)
        .collect()
}

fn resolve_workspace_path(raw_path: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let path = PathBuf::from(raw_path);
    if path.is_absolute() {
        return Some(path);
    }
    if let Some(cwd) = cwd {
        let cwd = Path::new(cwd);
        for ancestor in cwd.ancestors() {
            let candidate = ancestor.join(raw_path);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        return Some(cwd.join(raw_path));
    }
    let cwd = std::env::current_dir().ok()?;
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join(raw_path);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    Some(cwd.join(raw_path))
}

fn feature_dispatch_recommendation(
    server: &MemoryServer,
    params: &TachiTaskParams,
    query: &str,
) -> Value {
    let mut file_paths = params.doc_paths.clone();
    file_paths.extend(params.spec_paths.clone());
    match crate::dispatch_profile::handle_dispatch_recommendation(
        server,
        query,
        params.risk.as_deref(),
        params.limit.unwrap_or(500),
        &file_paths,
    ) {
        Ok(raw) => serde_json::from_str(&raw)
            .unwrap_or_else(|err| json!({"available": false, "error": err.to_string()})),
        Err(err) => json!({"available": false, "error": err}),
    }
}

fn suggested_feature_dispatch(
    params: &TachiTaskParams,
    query: &str,
    recommendation: &Value,
) -> Value {
    let profile = params.profile.as_deref().map(str::to_string).or_else(|| {
        recommendation
            .get("recommended_profile")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let mut arguments = serde_json::Map::new();
    arguments.insert("action".to_string(), json!("dispatch"));
    arguments.insert(
        "task".to_string(),
        json!(params.task.as_deref().unwrap_or(query)),
    );
    if let Some(profile) = profile {
        arguments.insert("profile".to_string(), json!(profile));
    }
    if let Some(cwd) = params
        .cwd
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("cwd".to_string(), json!(cwd));
    }
    if let Some(project) = params
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("project".to_string(), json!(project));
    }
    if let Some(issue_ref) = params
        .issue_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("issue_ref".to_string(), json!(issue_ref));
    }
    if let Some(pr_ref) = params
        .pr_ref
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("pr_ref".to_string(), json!(pr_ref));
    }
    if let Some(flow_id) = params
        .flow_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("flow_id".to_string(), json!(flow_id));
    }
    if let Some(risk) = params
        .risk
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        arguments.insert("risk".to_string(), json!(risk));
    }
    if let Some(true) = params.auto_capability_bundle {
        arguments.insert("auto_capability_bundle".to_string(), json!(true));
    }
    json!({
        "tool": "tachi_task",
        "arguments": arguments,
        "evidence_required": recommendation
            .get("evidence_required")
            .cloned()
            .unwrap_or(Value::Null),
        "fallback_chain": recommendation
            .get("fallback_chain")
            .cloned()
            .unwrap_or_else(|| json!([])),
    })
}

fn relevant_feature_profiles(recommendation: &Value) -> Vec<Value> {
    recommendation
        .get("candidates")
        .and_then(Value::as_array)
        .map(|candidates| {
            candidates
                .iter()
                .take(4)
                .map(|candidate| {
                    json!({
                        "profile": candidate.get("profile").cloned().unwrap_or(Value::Null),
                        "agent": candidate.get("agent").cloned().unwrap_or(Value::Null),
                        "role": candidate.get("role").cloned().unwrap_or(Value::Null),
                        "score": candidate.get("score").cloned().unwrap_or(Value::Null),
                        "reason": candidate.get("reasons").cloned().unwrap_or_else(|| json!([])),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn feature_run_artifacts(flow_id: Option<&str>) -> Result<Vec<Value>, String> {
    let Some(flow_id) = flow_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id)?;
    let mut out = vec![json!({
        "kind": "run_dir",
        "path": run_dir.to_string_lossy(),
        "exists": run_dir.exists(),
        "layer": "runtime_artifact",
        "authority": "runtime_state",
    })];
    for name in [
        "instruction.md",
        "plan.md",
        "result.md",
        "validation.md",
        "status.json",
        "close_loop.json",
        "events.jsonl",
        "progress.jsonl",
        "trajectory.jsonl",
    ] {
        let path = run_dir.join(name);
        out.push(json!({
            "kind": "run_artifact",
            "path": path.to_string_lossy(),
            "exists": path.exists(),
            "layer": "runtime_artifact",
            "authority": "runtime_state",
        }));
    }
    Ok(out)
}

async fn feature_board(
    server: &MemoryServer,
    params: &TachiTaskParams,
    top_k: usize,
) -> serde_json::Value {
    let raw = feature_board_raw(server, params, params.flow_id.clone(), top_k).await;
    let mut used_fallback = false;
    let mut board: Value = match raw {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| json!({})),
        Err(_) => return json!({"available": false}),
    };
    if params
        .flow_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
        && board
            .get("tasks")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        if let Ok(raw) = feature_board_raw(server, params, None, top_k).await {
            board = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
            used_fallback = true;
        }
    }
    let Some(tasks) = board.get("tasks").and_then(Value::as_array).cloned() else {
        return board;
    };
    let needles = feature_needles(params);
    if needles.is_empty() {
        board["tasks"] = Value::Array(tasks.into_iter().take(top_k).collect());
        board["count"] = json!(board["tasks"].as_array().map(Vec::len).unwrap_or(0));
        return board;
    }
    let filtered = tasks
        .into_iter()
        .filter(|task| value_contains_any(task, &needles))
        .take(top_k)
        .collect::<Vec<_>>();
    board["tasks"] = Value::Array(filtered);
    board["count"] = json!(board["tasks"].as_array().map(Vec::len).unwrap_or(0));
    if used_fallback {
        board["flow_id"] = json!(params.flow_id);
        board["flow_filter_fallback"] = json!("needle_scan");
    }
    board
}

async fn feature_board_raw(
    server: &MemoryServer,
    params: &TachiTaskParams,
    flow_id: Option<String>,
    top_k: usize,
) -> Result<String, String> {
    crate::dispatch_ops::handle_tachi_board(
        server,
        TachiBoardParams {
            state_filter: Some("all".to_string()),
            limit: Some(top_k.max(10)),
            project: params.project.clone(),
            flow_id,
        },
    )
    .await
}

fn feature_needles(params: &TachiTaskParams) -> Vec<String> {
    [
        params.flow_id.as_deref(),
        params.issue_ref.as_deref(),
        params.pr_ref.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_ascii_lowercase)
    .collect()
}

fn value_contains_any(value: &Value, needles: &[String]) -> bool {
    if needles.is_empty() {
        return true;
    }
    [
        "dispatch_id",
        "summary",
        "run_dir",
        "eval_id",
        "agent",
        "state",
        "source",
    ]
    .into_iter()
    .filter_map(|field| value.get(field))
    .any(|field_value| field_value_contains_any(field_value, needles))
}

fn field_value_contains_any(value: &Value, needles: &[String]) -> bool {
    match value {
        Value::String(text) => {
            let text = text.to_ascii_lowercase();
            needles.iter().any(|needle| text.contains(needle))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| field_value_contains_any(value, needles)),
        Value::Object(map) => map
            .values()
            .any(|value| field_value_contains_any(value, needles)),
        _ => false,
    }
}

fn infer_feature_stage(run_artifacts: &[Value], board: &Value) -> String {
    if let Some(task) = board
        .get("tasks")
        .and_then(Value::as_array)
        .and_then(|tasks| tasks.first())
    {
        if let Some(state) = task.get("state").and_then(Value::as_str) {
            return state.to_string();
        }
    }
    let has_result = run_artifacts.iter().any(|artifact| {
        artifact
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("result.md"))
            && artifact.get("exists").and_then(Value::as_bool) == Some(true)
    });
    if has_result {
        "result_available".to_string()
    } else if !run_artifacts.is_empty() {
        "flow_started".to_string()
    } else {
        "intake".to_string()
    }
}

fn feature_next_action(
    canonical_docs: &[Value],
    run_artifacts: &[Value],
    board: &Value,
    memory_rows: &[Value],
) -> String {
    if canonical_docs.is_empty() {
        return "Attach or create a canonical docs/spec reference before treating memory as feature truth.".to_string();
    }
    if board
        .get("tasks")
        .and_then(Value::as_array)
        .is_some_and(|tasks| {
            tasks
                .iter()
                .any(|task| task.get("state").and_then(Value::as_str) == Some("TASK_STATE_WORKING"))
        })
    {
        return "Poll tachi_task(action='board') and collect the active worker result before dispatching more work.".to_string();
    }
    if run_artifacts.iter().any(|artifact| {
        artifact
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("instruction.md"))
            && artifact.get("exists").and_then(Value::as_bool) == Some(true)
    }) && !run_artifacts.iter().any(|artifact| {
        artifact
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("result.md"))
            && artifact.get("exists").and_then(Value::as_bool) == Some(true)
    }) {
        return "Use the flow instruction packet as the worker handoff source and dispatch a bounded slice.".to_string();
    }
    if memory_rows.is_empty() {
        return "Start with tachi_task(action='plan') or save a checkpoint after the next concrete decision.".to_string();
    }
    "Run tachi_task(action='recommend') for the next worker profile, then dispatch or review with explicit verification.".to_string()
}

fn format_feature_briefing_markdown(value: &Value) -> String {
    let mut out = Vec::new();
    out.push("# Feature Briefing".to_string());
    out.push(format!(
        "\n## Objective\n{}",
        value
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or("(unspecified)")
    ));
    out.push(format!(
        "\n## Current Stage\n{}",
        value
            .get("current_stage")
            .and_then(Value::as_str)
            .unwrap_or("intake")
    ));
    out.push(markdown_section(
        "Project Work Record",
        value.get("project_work_record").and_then(Value::as_array),
        "No GitHub issue/PR reference attached.",
    ));
    out.push(markdown_section(
        "Canonical Docs / Specs",
        value.get("canonical_docs").and_then(Value::as_array),
        "No canonical docs/specs attached.",
    ));
    out.push(markdown_section(
        "Run Artifacts",
        value.get("run_artifacts").and_then(Value::as_array),
        "No flow run artifacts attached.",
    ));
    out.push(markdown_section(
        "Board State",
        value
            .get("board_state")
            .and_then(|board| board.get("tasks"))
            .and_then(Value::as_array),
        "No matching board tasks.",
    ));
    let mut guide_rows = Vec::new();
    if let Some(hits) = value.get("guide_hits").and_then(Value::as_array) {
        guide_rows.extend(hits.iter().cloned());
    }
    if let Some(sops) = value
        .get("guide_sop")
        .and_then(|guide| guide.get("selected_sops"))
        .and_then(Value::as_array)
    {
        guide_rows.extend(sops.iter().cloned());
    }
    out.push(markdown_section(
        "Guide / SOP",
        if guide_rows.is_empty() {
            None
        } else {
            Some(&guide_rows)
        },
        "No SOP selected.",
    ));
    out.push(markdown_section(
        "Feedback Rules",
        value
            .get("feedback_rules")
            .and_then(|rules| rules.get("rules"))
            .and_then(Value::as_array),
        "No applicable feedback rules.",
    ));
    out.push(markdown_dispatch_recommendation(value));
    out.push(markdown_section(
        "Relevant Skills / Profiles",
        value.get("relevant_profiles").and_then(Value::as_array),
        "No dispatch profiles ranked.",
    ));
    out.push(markdown_section(
        "Wiki Decisions / Lessons",
        value.get("wiki_hits").and_then(Value::as_array),
        "No wiki hits.",
    ));
    out.push(markdown_section(
        "Memory Fragments / Checkpoints",
        value.get("memory_fragments").and_then(Value::as_array),
        "No project-scoped memory fragments.",
    ));
    out.push(markdown_section(
        "Eval Evidence",
        value.get("eval_evidence").and_then(Value::as_array),
        "No matching eval evidence.",
    ));
    if let Some(loops) = value
        .get("open_loops")
        .and_then(Value::as_array)
        .filter(|loops| !loops.is_empty())
    {
        out.push("\n## ⚠️ Open Loops (closure debt)".to_string());
        for item in loops {
            let detail = item.get("detail").and_then(Value::as_str).unwrap_or("");
            let action = item.get("action").and_then(Value::as_str).unwrap_or("");
            out.push(format!("- {detail} → `{action}`"));
        }
    }
    out.push(format!(
        "\n## Next Action\n{}",
        value
            .get("next_action")
            .and_then(Value::as_str)
            .unwrap_or("Continue from the canonical docs/specs.")
    ));
    out.join("\n")
}

fn markdown_dispatch_recommendation(value: &Value) -> String {
    let mut out = vec!["\n## Recommended Dispatch".to_string()];
    let recommendation = value.get("route_recommendation").unwrap_or(&Value::Null);
    let suggested = value.get("suggested_dispatch").unwrap_or(&Value::Null);
    let Some(profile) = recommendation
        .get("recommended_profile")
        .and_then(Value::as_str)
    else {
        out.push("- No dispatch profile recommendation available.".to_string());
        return out.join("\n");
    };
    let agent = recommendation
        .get("recommended_agent")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let risk = recommendation
        .get("risk")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    out.push(format!(
        "- Profile: `{profile}` via `{agent}` (risk={risk})"
    ));
    if let Some(reason) = recommendation.get("reason").and_then(Value::as_array) {
        let reason = reason
            .iter()
            .take(3)
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        if !reason.is_empty() {
            out.push(format!("- Why: {}", reason.join("; ")));
        }
    }
    if let Some(arguments) = suggested.get("arguments") {
        out.push(format!("- Dispatch args: `{}`", compact_json(arguments)));
    }
    out.join("\n")
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn markdown_section(title: &str, rows: Option<&Vec<Value>>, empty: &str) -> String {
    let mut out = vec![format!("\n## {title}")];
    let Some(rows) = rows.filter(|rows| !rows.is_empty()) else {
        out.push(format!("- {empty}"));
        return out.join("\n");
    };
    for row in rows.iter().take(8) {
        out.push(format!("- {}", compact_value_line(row)));
    }
    out.join("\n")
}

fn compact_value_line(value: &Value) -> String {
    if let Some(path) = value.get("path").and_then(Value::as_str) {
        let summary = value
            .get("summary")
            .or_else(|| value.get("kind"))
            .or_else(|| value.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if summary.is_empty() {
            return format!("`{path}`");
        }
        return format!("`{path}` - {summary}");
    }
    if let Some(summary) = value.get("summary").and_then(Value::as_str) {
        return summary.to_string();
    }
    if let Some(id) = value.get("dispatch_id").and_then(Value::as_str) {
        let state = value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let summary = value.get("summary").and_then(Value::as_str).unwrap_or("");
        return format!("`{id}` [{state}] {summary}");
    }
    value.to_string()
}

pub(crate) struct TaskBriefRouting {
    pub(crate) intent: &'static str,
    pub(crate) selected_sops: Vec<Value>,
    pub(crate) tool_plan: Vec<Value>,
}

pub(crate) fn build_task_brief_routing(
    task: &str,
    recommended_skills: &[Value],
) -> TaskBriefRouting {
    let intent = classify_task_intent(task);
    TaskBriefRouting {
        intent,
        selected_sops: build_selected_sops(intent, recommended_skills),
        tool_plan: build_tool_plan(intent),
    }
}

fn classify_task_intent(task: &str) -> &'static str {
    let lower = task.to_ascii_lowercase();
    let contains_any = |needles: &[&str]| {
        needles
            .iter()
            .any(|needle| task_matches_intent(task, &lower, needle))
    };

    if contains_any(&[
        "review",
        "code review",
        "pull request",
        "pr",
        "审查",
        "看看 pr",
        "看一下 pr",
    ]) {
        "review_request"
    } else if contains_any(&["refactor", "cleanup", "deslop", "重构", "清理"]) {
        "refactor_request"
    } else if contains_any(&[
        "ui",
        "ux",
        "frontend",
        "component",
        "visual",
        "screenshot",
        "页面",
        "前端",
        "组件",
        "截图",
        "视觉",
    ]) {
        "design_request"
    } else if contains_any(&[
        "test",
        "测试",
        "验证",
        "ci",
        "clippy",
        "build",
        "compile",
        "编译",
        "跑起来",
    ]) {
        "test_request"
    } else if contains_any(&[
        "debug",
        "bug",
        "error",
        "failure",
        "排查",
        "报错",
        "不工作",
        "修好",
    ]) {
        "fix_request"
    } else if contains_any(&[
        "release notes",
        "changelog",
        "rewrite",
        "proofread",
        "polish",
        "润色",
        "改稿",
        "去ai味",
        "写一段",
        "文案",
    ]) {
        "write_request"
    } else if contains_any(&["http://", "https://", "pdf", "url", "read this", "读一下"]) {
        "read_request"
    } else if contains_any(&[
        "health",
        "doctor",
        "hooks",
        "mcp broken",
        "配置检查",
        "健康度",
        "体检",
    ]) {
        "health_request"
    } else if contains_any(&[
        "research",
        "investigate",
        "explore",
        "exploration",
        "summarize",
        "summary",
        "overview",
        "map out",
        "walk through",
        "walkthrough",
        "学习",
        "研究",
        "查一下",
        "看一下资料",
        "探索",
        "梳理",
        "概览",
        "盘点",
        "通读",
    ]) {
        "research_request"
    } else if contains_any(&["migration", "migrate", "迁移", "schema"]) {
        "migration_request"
    } else if contains_any(&["plan", "design", "architecture", "方案", "规划", "设计"]) {
        "plan_request"
    } else if contains_any(&["explain", "why", "解释", "为什么"]) {
        "explain_request"
    } else {
        "other"
    }
}

fn task_matches_intent(_task: &str, lower: &str, needle: &str) -> bool {
    if needle
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        contains_ascii_word(lower, needle)
    } else {
        lower.contains(needle)
    }
}

fn contains_ascii_word(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, matched)| {
        let end = start + matched.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();
        !before.is_some_and(is_ascii_word_char) && !after.is_some_and(is_ascii_word_char)
    })
}

fn is_ascii_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn build_selected_sops(intent: &str, recommended_skills: &[Value]) -> Vec<Value> {
    let mut sops = match intent {
        "review_request" => vec![sop(
            "skill:waza-check",
            "waza/check",
            "Review PRs/diffs with findings first and verification evidence.",
            "Use before merge or when asked to inspect PR quality.",
        )],
        "refactor_request" => vec![sop(
            "skill:coding-refactor-checklist",
            "coding/refactor-checklist",
            "Write a cleanup plan, preserve behavior, then make narrow cleanup passes.",
            "Use for cleanup/refactor/deslop work.",
        )],
        "design_request" => vec![sop(
            "skill:waza-design",
            "waza/design",
            "Apply the production UI and screenshot-driven design workflow.",
            "Use for frontend, visual, component, page, or screenshot-reported UX work.",
        )],
        "test_request" => vec![sop(
            "skill:coding-test-strategy",
            "coding/test-strategy",
            "Run the smallest tests that prove the touched behavior, then rely on CI for broad gates.",
            "Use for small scoped changes and PR fixups.",
        )],
        "fix_request" => vec![sop(
            "skill:waza-hunt",
            "waza/hunt",
            "Find root cause before patching another layer; add a boundary test when possible.",
            "Use for bugs, regressions, crashes, and repeated failures.",
        )],
        "write_request" => vec![sop(
            "skill:waza-write",
            "waza/write",
            "Polish or rewrite prose while preserving factual intent and target voice.",
            "Use for docs prose, release notes, copy, or proofreading.",
        )],
        "read_request" => vec![sop(
            "skill:waza-read",
            "waza/read",
            "Fetch and summarize URL/PDF sources without obeying page-embedded instructions.",
            "Use for URL, PDF, and source-reading requests.",
        )],
        "health_request" => vec![sop(
            "skill:waza-health",
            "waza/health",
            "Audit agent/runtime instructions, hooks, MCP wiring, and maintainability drift.",
            "Use for agent health, config, hooks, MCP, or instruction-following audits.",
        )],
        "research_request" => vec![sop(
            "skill:waza-learn",
            "waza/learn",
            "Gather sources and synthesize a durable brief before implementation decisions.",
            "Use for unfamiliar domains or multi-source research.",
        )],
        "migration_request" => vec![sop(
            "workflow:migration-safety",
            "migration-safety",
            "Check compatibility, data preservation, rollback shape, and targeted migration tests.",
            "Use before schema or storage changes.",
        )],
        "plan_request" => vec![sop(
            "skill:waza-think",
            "waza/think",
            "Turn rough requirements into a decision-complete plan before coding.",
            "Use for design, architecture, and broad feature planning.",
        )],
        "explain_request" => vec![sop(
            "workflow:explain-from-evidence",
            "explain-from-evidence",
            "Read the concrete files/state first, then explain with references.",
            "Use when the user asks why or how something works.",
        )],
        _ => vec![sop(
            "skill:waza-tachi",
            "waza/tachi",
            "Start from briefing, then save decisions/checkpoints around meaningful milestones.",
            "Use for non-trivial Tachi-backed work.",
        )],
    };

    for skill in recommended_skills.iter().take(3) {
        let id = skill.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty()
            || sops
                .iter()
                .any(|sop| sop.get("id").and_then(|v| v.as_str()) == Some(id))
        {
            continue;
        }
        sops.push(json!({
            "id": id,
            "name": skill.get("name").and_then(|v| v.as_str()).unwrap_or(id),
            "source": "hub_recommendation",
            "reason": skill.get("description").and_then(|v| v.as_str()).unwrap_or("Recommended by local skill matching."),
            "activation_hint": "Call tachi_skill(action='discover') or run the corresponding host skill when available.",
        }));
    }
    sops
}

fn sop(id: &str, name: &str, reason: &str, activation_hint: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "source": "task_brief_router",
        "reason": reason,
        "activation_hint": activation_hint,
    })
}

fn build_tool_plan(intent: &str) -> Vec<Value> {
    let mut plan = vec![
        json!({
            "step": "brief",
            "tool": "tachi_memory",
            "action": "briefing",
            "when": "before starting non-trivial work",
        }),
        json!({
            "step": "discover_sop",
            "tool": "tachi_skill",
            "action": "discover",
            "when": "when selected_sops includes a skill not already active in the host",
        }),
    ];

    match intent {
        "plan_request" | "research_request" => plan.push(json!({
            "step": "plan",
            "tool": "tachi_task",
            "action": "plan",
            "when": "before dispatching implementation work",
        })),
        "review_request" => plan.push(json!({
            "step": "review",
            "tool": "tachi_task",
            "action": "board",
            "when": "inspect active/completed delegated work before merge",
        })),
        "fix_request" | "test_request" => plan.push(json!({
            "step": "progress_check",
            "tool": "tachi_progress_check",
            "action": "check",
            "when": "after repeated failed attempts or unclear root cause",
        })),
        _ => {}
    }

    plan.push(json!({
        "step": "checkpoint",
        "tool": "tachi_memory",
        "action": "checkpoint",
        "when": "before handoff or after a meaningful milestone",
    }));
    plan
}

pub(crate) async fn handle_tachi_progress_check(
    server: &MemoryServer,
    params: ProgressCheckParams,
) -> Result<String, String> {
    let attempt_count = params.attempts.len();
    let repeated_layer = params
        .attempts
        .iter()
        .filter(|attempt| {
            let lower = attempt.to_ascii_lowercase();
            lower.contains("transport")
                || lower.contains("proxy")
                || lower.contains("http")
                || lower.contains("传输")
        })
        .count()
        >= 2;
    let has_error = params
        .latest_error
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let stuck = attempt_count >= 3 || (attempt_count >= 2 && has_error) || repeated_layer;
    let query = format!(
        "{} {} {}",
        params.task,
        params.latest_error.clone().unwrap_or_default(),
        params.attempts.join(" ")
    );
    let top_k = crate::clamp_facade_top_k(params.top_k);
    let wiki_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some("/wiki".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await?;
    let debug_checklist = build_debug_checklist(&wiki_rows);

    let ask_codex_prompt = format!(
        "Review this stuck debugging task and identify the most likely wrong assumption.\n\nTask: {}\n\nAttempts:\n{}\n\nLatest error:\n{}\n\nPlease reason from the observed error backward across boundaries before proposing code changes.",
        params.task,
        params
            .attempts
            .iter()
            .enumerate()
            .map(|(idx, attempt)| format!("{}. {}", idx + 1, attempt))
            .collect::<Vec<_>>()
            .join("\n"),
        params.latest_error.as_deref().unwrap_or("(none provided)")
    );
    let progress_log = if let Some(flow_id) = params.flow_id.as_deref() {
        record_progress_check_event(flow_id, &params, stuck)?
    } else {
        None
    };

    serde_json::to_string(&json!({
        "status": "ok",
        "stuck": stuck,
        "attempt_count": attempt_count,
        "signals": {
            "has_latest_error": has_error,
            "repeated_same_layer": repeated_layer,
        },
        "reason": if stuck {
            "The task shows repeated attempts or continued errors; stop patching and reframe."
        } else {
            "No strong stuck signal yet; keep validating the next narrow hypothesis."
        },
        "suggested_reframe": "Trace where the invariant first fails. For MCP parameter bugs, check schema -> client serialization -> server deserialization -> handler -> transport before changing transport code.",
        "wiki_hits": compact_rows(wiki_rows, top_k),
        "debug_checklist": debug_checklist,
        "should_ask_codex": stuck,
        "ask_codex_prompt": ask_codex_prompt,
        "progress_log": progress_log,
        "next_actions": if stuck {
            json!(["search wiki hits", "write a failing boundary test", "ask another agent with ask_codex_prompt", "only then edit code"])
        } else {
            json!(["continue one narrow validation", "record the result", "call tachi_progress_check again after another failed attempt"])
        },
    }))
    .map_err(|e| format!("serialize progress_check: {e}"))
}

fn record_progress_check_event(
    flow_id: &str,
    params: &ProgressCheckParams,
    stuck: bool,
) -> Result<Option<String>, String> {
    use std::io::Write;

    if flow_id.is_empty()
        || flow_id.contains('/')
        || flow_id.contains('\\')
        || flow_id.contains("..")
        || !flow_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("Invalid flow_id: '{flow_id}'"));
    }
    let run_dir = crate::shell_ops::shell_runs_root().join(flow_id);
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("create progress run dir: {e}"))?;
    let path = run_dir.join("progress.jsonl");
    let line = serde_json::to_string(&json!({
        "timestamp": Utc::now().to_rfc3339(),
        "flow_id": flow_id,
        "event": "progress_check",
        "task": params.task,
        "attempt_count": params.attempts.len(),
        "latest_error": params.latest_error,
        "stuck": stuck,
        "project": params.project,
        "domain": params.domain,
    }))
    .map_err(|e| format!("serialize progress check: {e}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(Some(path.display().to_string()))
}

async fn build_route_recommendation(
    server: &MemoryServer,
    task: &str,
    project: Option<&str>,
) -> serde_json::Value {
    let eval_rows = match search_memory_rows(
        server,
        SearchMemoryParams {
            query: task.to_string(),
            query_vec: None,
            top_k: 20,
            path_prefix: Some("/eval/".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 40,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: project.map(|s| s.to_string()),
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await
    {
        Ok(rows) => rows,
        Err(_) => return json!({"available": false}),
    };

    if eval_rows.is_empty() {
        return json!({"available": false, "reason": "no eval history"});
    }

    let mut agent_stats: std::collections::HashMap<String, (u32, u32)> =
        std::collections::HashMap::new();

    for row in &eval_rows {
        let meta = match row.get("metadata") {
            Some(m) => m,
            None => continue,
        };
        let agent = meta
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let outcome = meta.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
        let entry = agent_stats.entry(agent).or_insert((0, 0));
        entry.1 += 1;
        if outcome == "success" {
            entry.0 += 1;
        }
    }

    let mut rankings: Vec<serde_json::Value> = agent_stats
        .iter()
        .map(|(agent, (success, total))| {
            let rate = if *total > 0 {
                (*success as f64) / (*total as f64)
            } else {
                0.0
            };
            json!({
                "agent": agent,
                "success": success,
                "total": total,
                "rate": (rate * 100.0).round() / 100.0,
            })
        })
        .collect();
    rankings.sort_by(|a, b| {
        b.get("rate")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .partial_cmp(&a.get("rate").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let recommended = rankings
        .first()
        .and_then(|r| r.get("agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("claude");

    json!({
        "available": true,
        "eval_count": eval_rows.len(),
        "agent_rankings": rankings,
        "recommended_agent": recommended,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_debug_checklist_prefers_wiki_guidance() {
        let checklist = build_debug_checklist(&[json!({
            "path": "/wiki/debug/mcp-args",
            "text": "Checklist:\n- Verify schema -> client serialization -> server deserialization before editing transport.\n- Add a failing boundary test at the API boundary before retrying the same layer.\n- Stop after two failed patches in the same layer and ask another agent.",
            "summary": "MCP argument debugging"
        })]);

        assert!(checklist[0].contains("schema -> client serialization -> server deserialization"));
        assert!(checklist
            .iter()
            .any(|item| item.contains("failing boundary test at the API boundary")));
    }

    #[test]
    fn build_debug_checklist_falls_back_without_wiki_hits() {
        let checklist = build_debug_checklist(&[json!({
            "path": "/behavior/global_rules/retry-policy",
            "text": "This is not a wiki entry and should not override the fallback checklist.",
        })]);

        assert_eq!(checklist.len(), DEBUG_CHECKLIST_LIMIT);
        assert_eq!(checklist[0], FALLBACK_DEBUG_CHECKLIST[0]);
    }

    #[test]
    fn wiki_slug_preserves_cjk_and_readable_separators() {
        assert_eq!(
            wiki_slug("MCP hub_call arguments 丢失：从 schema 层排查"),
            "MCP-hub_call-arguments-丢失-从-schema-层排查"
        );
    }

    #[test]
    fn skill_scoring_ignores_generic_fix_tokens() {
        let frontend = HubCapability {
            id: "skill:frontend-design".to_string(),
            cap_type: "skill".to_string(),
            name: "frontend-design".to_string(),
            version: 1,
            description: "Fix UI layout and visual design issues".to_string(),
            definition: String::new(),
            enabled: true,
            review_status: "approved".to_string(),
            health_status: "healthy".to_string(),
            last_error: None,
            last_success_at: None,
            last_failure_at: None,
            fail_streak: 0,
            active_version: None,
            exposure_mode: "direct".to_string(),
            uses: 0,
            successes: 0,
            failures: 0,
            avg_rating: 0.0,
            last_used: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let mcp = HubCapability {
            id: "skill:mcp-schema-debug".to_string(),
            name: "mcp-schema-debug".to_string(),
            description: "Debug MCP schema arguments and hub_call serialization".to_string(),
            ..frontend.clone()
        };
        let tokens = tokenize_task("fix Exa hub_call arguments 丢失");

        assert_eq!(score_capability(&tokens, &frontend), 0);
        assert!(
            score_capability(&tokens, &mcp) >= 3,
            "expected MCP-specific skill to match task tokens"
        );
    }

    #[test]
    fn task_brief_router_selects_review_sop_for_pr_review() {
        let intent = classify_task_intent("看看这几个 PR 下面 Gemini 的回复");
        let sops = build_selected_sops(intent, &[]);
        let plan = build_tool_plan(intent);

        assert_eq!(intent, "review_request");
        assert!(sops
            .iter()
            .any(|sop| sop.get("id").and_then(|v| v.as_str()) == Some("skill:waza-check")));
        assert!(plan.iter().any(|step| {
            step.get("tool").and_then(|v| v.as_str()) == Some("tachi_task")
                && step.get("action").and_then(|v| v.as_str()) == Some("board")
        }));
    }

    #[test]
    fn task_brief_router_selects_targeted_verification_for_build_run() {
        let intent = classify_task_intent("帮我编译二进制并且跑起来验证功能");
        let sops = build_selected_sops(intent, &[]);

        assert_eq!(intent, "test_request");
        assert!(sops.iter().any(|sop| {
            sop.get("id").and_then(|v| v.as_str()) == Some("skill:coding-test-strategy")
        }));
    }

    #[test]
    fn task_brief_router_maps_waza_capability_intents() {
        let cases = [
            (
                "帮我做一个前端页面截图视觉检查",
                "design_request",
                "skill:waza-design",
            ),
            (
                "润色这段 release notes",
                "write_request",
                "skill:waza-write",
            ),
            (
                "读一下 https://example.com/report.pdf",
                "read_request",
                "skill:waza-read",
            ),
            (
                "检查 agent MCP 配置健康度",
                "health_request",
                "skill:waza-health",
            ),
        ];
        for (task, expected_intent, expected_skill) in cases {
            let intent = classify_task_intent(task);
            let sops = build_selected_sops(intent, &[]);
            assert_eq!(intent, expected_intent, "task: {task}");
            assert!(
                sops.iter()
                    .any(|sop| { sop.get("id").and_then(|v| v.as_str()) == Some(expected_skill) }),
                "expected {expected_skill} for {task}, got {sops:?}"
            );
        }
    }

    #[test]
    fn task_brief_router_avoids_ascii_substring_false_positives() {
        assert_eq!(
            classify_task_intent("explain why this failed"),
            "explain_request"
        );
        assert_eq!(
            classify_task_intent("decide whether this is specific enough"),
            "other"
        );
        assert_eq!(classify_task_intent("run ci checks"), "test_request");
    }

    #[test]
    fn task_brief_router_generic_kankan_is_not_always_review() {
        assert_eq!(classify_task_intent("看看这个报错"), "fix_request");
        assert_eq!(
            classify_task_intent("看看这几个 PR 下面 Gemini 的回复"),
            "review_request"
        );
        assert_eq!(classify_task_intent("看一下 PRs"), "review_request");
    }

    #[test]
    fn task_brief_router_classifies_exploration_as_research() {
        assert_eq!(
            classify_task_intent(
                "List the .rs files under crates/memory-core/src and produce a one-line summary of each."
            ),
            "research_request"
        );
        assert_eq!(
            classify_task_intent("explore the codebase and give an overview"),
            "research_request"
        );
        assert_eq!(
            classify_task_intent("梳理一下这个模块的结构"),
            "research_request"
        );
        assert_eq!(
            classify_task_intent("map out the module dependency graph"),
            "research_request"
        );
        // Gemini guard: a coding task phrased with "map the ..." must NOT be
        // misread as research (the reason "map the" was narrowed to "map out").
        assert_ne!(
            classify_task_intent("map the array values into the new struct fields"),
            "research_request"
        );
    }

    #[test]
    fn feature_board_filter_matches_stable_fields_only() {
        let needles = vec!["flow_20260608t000000z_feature".to_string()];

        assert!(value_contains_any(
            &json!({
                "dispatch_id": "dispatch-1",
                "summary": "work for flow_20260608T000000Z_feature",
                "metadata": {
                    "debug_note": "unrelated"
                }
            }),
            &needles
        ));
        assert!(!value_contains_any(
            &json!({
                "dispatch_id": "dispatch-2",
                "summary": "unrelated work",
                "metadata": {
                    "debug_note": "flow_20260608T000000Z_feature"
                }
            }),
            &needles
        ));
    }

    #[test]
    fn task_brief_router_appends_hub_skill_recommendations() {
        let recommended = vec![json!({
            "id": "skill:mcp-schema-debug",
            "name": "mcp-schema-debug",
            "description": "Debug MCP schema arguments",
            "score": 5
        })];
        let sops = build_selected_sops("fix_request", &recommended);

        assert!(sops
            .iter()
            .any(|sop| sop.get("id").and_then(|v| v.as_str()) == Some("skill:waza-hunt")));
        assert!(sops.iter().any(|sop| {
            sop.get("id").and_then(|v| v.as_str()) == Some("skill:mcp-schema-debug")
                && sop.get("source").and_then(|v| v.as_str()) == Some("hub_recommendation")
        }));
    }
}
