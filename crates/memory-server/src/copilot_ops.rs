use super::*;

const DEBUG_CHECKLIST_LIMIT: usize = 4;
const FALLBACK_DEBUG_CHECKLIST: [&str; DEBUG_CHECKLIST_LIMIT] = [
    "Start from the observed error and trace where the invariant first becomes false.",
    "For MCP argument bugs, verify schema -> client serialization -> server deserialization -> handler -> transport in that order.",
    "Do not keep patching the same layer after two failed attempts; reframe or ask another agent.",
    "If stderr/log visibility is weak, add a durable test or inspect the data structure at the API boundary.",
];

const WIKI_DUP_JACCARD_THRESHOLD: f64 = 0.85;

fn compact_rows(rows: Vec<Value>, limit: usize) -> Vec<Value> {
    rows.into_iter()
        .take(limit)
        .map(|row| {
            json!({
                "id": row.get("id").cloned().unwrap_or(Value::Null),
                "path": row.get("path").cloned().unwrap_or(Value::Null),
                "topic": row.get("topic").cloned().unwrap_or(Value::Null),
                "summary": row.get("summary").cloned().unwrap_or(Value::Null),
                "score": row.get("score").or_else(|| row.get("relevance")).cloned().unwrap_or(Value::Null),
            })
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
    if with_slash == "/wiki" || with_slash.starts_with("/wiki/") {
        with_slash
    } else {
        format!("/wiki{}", with_slash)
    }
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
///
/// Candidates are loaded via `list_by_path("/wiki")` and filtered in-memory.
/// A future optimization could push the path-prefix or topic filter into SQL
/// (`WHERE path LIKE '/wiki/%' AND (path = ? OR topic = ?)`) to avoid loading
/// the full wiki set when it grows large.
fn supersede_wiki_duplicates(
    store: &mut MemoryStore,
    canonical_id: &str,
    path: &str,
    topic: &str,
    text: &str,
) -> Result<usize, String> {
    let candidates = store
        .list_by_path("/wiki", 5000, false)
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
    let mut wiki_metadata = json!({
        "wiki": true,
        "wiki_title": params.title,
        "user_force": params.force,
        "allow_cross_project": true,
        "source_refs": params.references,
    });
    let _ = wiki_metadata.as_object_mut();

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
    let duplicates_superseded =
        with_existing_wiki_store(server, &project_name, use_named_project, duplicate_action)
            .unwrap_or(0);
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
    let mut rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k: params.top_k.max(1),
            path_prefix: Some(path_prefix.clone()),
            include_training: false,
            include_archived: params.include_archived,
            candidates_per_channel: params.top_k.max(20),
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
    let top_k = params.top_k.max(1);
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
        "research",
        "investigate",
        "学习",
        "研究",
        "查一下",
        "看一下资料",
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
            "skill:check",
            "check",
            "Review PRs/diffs with findings first and verification evidence.",
            "Use before merge or when asked to inspect PR quality.",
        )],
        "refactor_request" => vec![sop(
            "skill:ai-slop-cleaner",
            "ai-slop-cleaner",
            "Write a cleanup plan, preserve behavior, then make narrow cleanup passes.",
            "Use for cleanup/refactor/deslop work.",
        )],
        "test_request" => vec![sop(
            "workflow:targeted-verification",
            "targeted-verification",
            "Run the smallest tests that prove the touched behavior, then rely on CI for broad gates.",
            "Use for small scoped changes and PR fixups.",
        )],
        "fix_request" => vec![sop(
            "skill:hunt",
            "hunt",
            "Find root cause before patching another layer; add a boundary test when possible.",
            "Use for bugs, regressions, crashes, and repeated failures.",
        )],
        "research_request" => vec![sop(
            "skill:learn",
            "learn",
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
            "skill:think",
            "think",
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
            "skill:tachi",
            "tachi",
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
    let wiki_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k: params.top_k.max(1),
            path_prefix: Some("/wiki".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: params.top_k.max(20),
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
        "wiki_hits": compact_rows(wiki_rows, params.top_k.max(1)),
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
            .any(|sop| sop.get("id").and_then(|v| v.as_str()) == Some("skill:check")));
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
            sop.get("id").and_then(|v| v.as_str()) == Some("workflow:targeted-verification")
        }));
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
            .any(|sop| sop.get("id").and_then(|v| v.as_str()) == Some("skill:hunt")));
        assert!(sops.iter().any(|sop| {
            sop.get("id").and_then(|v| v.as_str()) == Some("skill:mcp-schema-debug")
                && sop.get("source").and_then(|v| v.as_str()) == Some("hub_recommendation")
        }));
    }
}
