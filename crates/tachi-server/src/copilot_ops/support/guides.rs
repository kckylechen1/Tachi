use super::*;

pub(in crate::copilot_ops) fn feature_guide_hits(
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

pub(in crate::copilot_ops) fn load_feature_guide_candidates(
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

pub(in crate::copilot_ops) fn merge_guide_candidates(
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

pub(in crate::copilot_ops) fn guide_applies_to(
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

pub(in crate::copilot_ops) fn guide_filter_matches(
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

pub(in crate::copilot_ops) fn score_feature_guide(
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

pub(in crate::copilot_ops) fn guide_has_restrictive_applies_to(entry: &MemoryEntry) -> bool {
    let applies = entry.metadata.get("applies_to").unwrap_or(&Value::Null);
    ["task_type", "profiles", "stage"]
        .iter()
        .any(|key| !guide_string_values(applies.get(*key).unwrap_or(&Value::Null)).is_empty())
}

pub(in crate::copilot_ops) fn guide_hit_haystack(entry: &MemoryEntry) -> String {
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

pub(in crate::copilot_ops) fn guide_string_values(value: &Value) -> Vec<String> {
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

pub(in crate::copilot_ops) fn guide_hit_row(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_wiki_guide(home: &Path, id: &str, text: &str) -> std::path::PathBuf {
        let db_path = home
            .join("projects")
            .join("wiki")
            .join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(db_path.parent().expect("wiki DB parent"))
            .expect("create wiki DB parent");
        let mut store =
            MemoryStore::open(db_path.to_str().expect("utf8 wiki DB")).expect("open wiki DB");
        let mut entry = crate::tests::make_entry(id);
        entry.path = format!("/guide/{id}");
        entry.summary = text.to_string();
        entry.text = text.to_string();
        store.upsert(&entry).expect("seed wiki guide");
        drop(store);

        let mut manifest = crate::manifest::Manifest::load_or_empty(&home.join("manifest.json"));
        manifest
            .dbs
            .retain(|entry| entry.scope_hint != "project:wiki");
        manifest.dbs.push(crate::manifest::DbEntry {
            path: db_path.display().to_string(),
            role: crate::manifest::DbRole::Project,
            owner: "test".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "project:wiki".to_string(),
            notes: String::new(),
        });
        manifest
            .save(&home.join("manifest.json"))
            .expect("save wiki manifest");
        db_path
    }

    #[test]
    fn wiki_and_guide_visibility_use_server_home_after_environment_drift() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let server = crate::tests::make_server();
        seed_wiki_guide(
            &server.tachi_home_dir(),
            "fixture-guide-after-env-drift",
            "fixture guide remains visible",
        );

        let ambient_home = tempfile::tempdir().expect("ambient home");
        seed_wiki_guide(
            ambient_home.path(),
            "ambient-guide-after-env-drift",
            "ambient guide must stay hidden",
        );
        let _ambient = crate::test_support::EnvRestore::set_path("TACHI_HOME", ambient_home.path());

        assert!(default_named_project_available(&server, "wiki"));
        let params: TachiTaskParams =
            serde_json::from_value(json!({"action": "briefing"})).expect("task params");
        let candidates = load_feature_guide_candidates(&server, &params, 20);
        let ids = candidates
            .iter()
            .map(|(entry, _, _)| entry.id.as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"fixture-guide-after-env-drift"));
        assert!(!ids.contains(&"ambient-guide-after-env-drift"));
    }
}
