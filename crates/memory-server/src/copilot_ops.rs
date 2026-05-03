use super::*;

const DEBUG_CHECKLIST_LIMIT: usize = 4;
const FALLBACK_DEBUG_CHECKLIST: [&str; DEBUG_CHECKLIST_LIMIT] = [
    "Start from the observed error and trace where the invariant first becomes false.",
    "For MCP argument bugs, verify schema -> client serialization -> server deserialization -> handler -> transport in that order.",
    "Do not keep patching the same layer after two failed attempts; reframe or ask another agent.",
    "If stderr/log visibility is weak, add a durable test or inspect the data structure at the API boundary.",
];

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
    let topic = params
        .topic
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| wiki_slug(&params.title));
    let path = normalize_wiki_path(params.path.clone(), &topic);
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
    });
    let _ = wiki_metadata.as_object_mut();

    let save_result = handle_save_memory(
        server,
        SaveMemoryParams {
            text: params.text,
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
            id: None,
            force: true,
            auto_link: true,
            project: params.project,
            retention_policy: Some(params.retention_policy),
            domain: params.domain.or_else(|| Some("wiki".to_string())),
            timestamp: None,
            metadata: Some(wiki_metadata),
        },
    )
    .await?;

    let mut response: Value =
        serde_json::from_str(&save_result).map_err(|e| format!("parse wiki save response: {e}"))?;
    if let Some(obj) = response.as_object_mut() {
        obj.insert("wiki_path".to_string(), json!(path));
        obj.insert("wiki_topic".to_string(), json!(topic));
    }
    crate::wiki_ops::append_wiki_log(
        server,
        "write",
        &format!(
            "{} | {}",
            path,
            response
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
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
    let project_name = params.project.unwrap_or_else(|| "wiki".to_string());
    let mut rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.query.clone(),
            query_vec: None,
            top_k: params.top_k.max(1),
            path_prefix: Some(path_prefix.clone()),
            include_archived: params.include_archived,
            candidates_per_channel: params.top_k.max(20),
            mmr_threshold: None,
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_role,
            project: Some(project_name.clone()),
            domain: params.domain,
        },
    )
    .await?;

    for row in rows.iter_mut().take(3) {
        crate::wiki_ops::add_related_entries_to_row(row, server, &project_name, 5);
    }

    crate::wiki_ops::append_wiki_log(
        server,
        "search",
        &format!("{} | {} result(s)", params.query, rows.len()),
    );

    serde_json::to_string(&json!({
        "query": params.query,
        "path_prefix": path_prefix,
        "count": rows.len(),
        "results": rows,
    }))
    .map_err(|e| format!("serialize wiki_search: {e}"))
}

pub(crate) async fn handle_tachi_task_brief(
    server: &MemoryServer,
    params: TaskBriefParams,
) -> Result<String, String> {
    let top_k = params.top_k.max(1);
    let wiki_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.task.clone(),
            query_vec: None,
            top_k,
            path_prefix: Some("/wiki".to_string()),
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
        },
    )
    .await?;
    let memory_rows = search_memory_rows(
        server,
        SearchMemoryParams {
            query: params.task.clone(),
            query_vec: None,
            top_k,
            path_prefix: params.path_prefix.clone(),
            include_archived: false,
            candidates_per_channel: top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
        },
    )
    .await?;
    let skills = recommend_skills_light(server, &params.task, 5).unwrap_or_default();
    let debug_checklist = build_debug_checklist(&wiki_rows);

    let route_rec =
        build_route_recommendation(server, &params.task, params.project.as_deref()).await;

    serde_json::to_string(&json!({
        "status": "ok",
        "task": params.task,
        "agent_id": params.agent_id,
        "project": params.project,
        "wiki_hits": compact_rows(wiki_rows, top_k),
        "memory_hits": compact_rows(memory_rows, top_k),
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
            include_archived: false,
            candidates_per_channel: params.top_k.max(20),
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: params.agent_id.clone(),
            project: params.project.clone(),
            domain: params.domain.clone(),
        },
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
        params.latest_error.clone().unwrap_or_else(|| "(none provided)".to_string())
    );

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
        "next_actions": if stuck {
            json!(["search wiki hits", "write a failing boundary test", "ask another agent with ask_codex_prompt", "only then edit code"])
        } else {
            json!(["continue one narrow validation", "record the result", "call tachi_progress_check again after another failed attempt"])
        },
    }))
    .map_err(|e| format!("serialize progress_check: {e}"))
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
            include_archived: false,
            candidates_per_channel: 40,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: project.map(|s| s.to_string()),
            domain: None,
        },
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
}
