use super::*;

pub(in crate::copilot_ops) fn tokenize_task(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_lowercase())
        .filter(|token| is_meaningful_skill_token(token))
        .collect()
}

pub(in crate::copilot_ops) fn is_meaningful_skill_token(token: &str) -> bool {
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

pub(in crate::copilot_ops) fn tokenize_skill_text(input: &str) -> HashSet<String> {
    tokenize_task(input).into_iter().collect()
}

pub(in crate::copilot_ops) fn score_capability(
    task_tokens: &[String],
    cap: &HubCapability,
) -> usize {
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

fn is_pattern_bridge_token(token: &str) -> bool {
    if !is_meaningful_skill_token(token) {
        return false;
    }
    const STOPWORDS: &[&str] = &[
        "agent",
        "alignment",
        "bridge",
        "completion",
        "context",
        "durable",
        "project",
        "pattern",
        "record",
        "records",
        "skill",
        "workflow",
    ];
    !STOPWORDS.contains(&token)
}

fn bridge_tokens(input: &str) -> HashSet<String> {
    tokenize_task(input)
        .into_iter()
        .filter(|token| is_pattern_bridge_token(token))
        .collect()
}

fn pattern_bridge_signals(server: &MemoryServer, task: &str) -> Vec<(Value, HashSet<String>)> {
    crate::continuity_ops::list_active_patterns(server, None, Some(task), 5)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            let projection_key = entry
                .metadata
                .get("projection_key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let tokens = bridge_tokens(&format!(
                "{} {} {} {} {}",
                entry.id, entry.path, entry.summary, entry.text, projection_key
            ));
            if tokens.is_empty() {
                None
            } else {
                Some((crate::continuity_ops::pattern_ref_json(&entry), tokens))
            }
        })
        .collect()
}

fn pattern_bridge_score(
    task_tokens: &[String],
    cap: &HubCapability,
    signals: &[(Value, HashSet<String>)],
) -> (usize, Vec<Value>) {
    if signals.is_empty() {
        return (0, Vec::new());
    }
    let task_bridge_tokens = task_tokens
        .iter()
        .filter(|token| is_pattern_bridge_token(token))
        .cloned()
        .collect::<HashSet<_>>();
    if task_bridge_tokens.is_empty() {
        return (0, Vec::new());
    }
    let cap_tokens = bridge_tokens(&format!("{} {} {}", cap.id, cap.name, cap.description));
    if cap_tokens.is_empty() {
        return (0, Vec::new());
    }

    let mut score = 0;
    let mut refs = Vec::new();
    for (pattern_ref, pattern_tokens) in signals {
        if !task_bridge_tokens
            .iter()
            .any(|token| pattern_tokens.contains(token))
        {
            continue;
        }
        if !cap_tokens
            .iter()
            .any(|token| pattern_tokens.contains(token))
        {
            continue;
        }
        score += 3;
        refs.push(pattern_ref.clone());
    }
    (score, refs)
}

pub(in crate::copilot_ops) fn recommend_skills_light(
    server: &MemoryServer,
    task: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let tokens = tokenize_task(task);
    let pattern_signals = pattern_bridge_signals(server, task);
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
        .map(|cap| {
            let (pattern_score, pattern_refs) =
                pattern_bridge_score(&tokens, &cap, &pattern_signals);
            (
                score_capability(&tokens, &cap) + pattern_score,
                cap,
                pattern_refs,
            )
        })
        .filter(|(score, _, _)| *score >= 3)
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));

    Ok(scored
        .into_iter()
        .take(limit)
        .map(|(score, cap, pattern_refs)| {
            let mut row = json!({
                "id": cap.id,
                "name": cap.name,
                "description": cap.description,
                "score": score,
            });
            if !pattern_refs.is_empty() {
                if let Some(object) = row.as_object_mut() {
                    object.insert("pattern_refs".to_string(), json!(pattern_refs));
                }
            }
            row
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_server;
    use chrono::Utc;
    use memory_core::MemoryEntry;

    fn make_test_entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/".to_string(),
            summary: String::new(),
            text: "test memory".to_string(),
            importance: 0.7,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn make_test_skill(id: &str, name: &str, description: &str) -> HubCapability {
        HubCapability {
            id: id.to_string(),
            cap_type: "skill".to_string(),
            name: name.to_string(),
            version: 1,
            description: description.to_string(),
            definition: json!({
                "prompt": format!("Run skill {name}"),
                "content": format!("# {name}\n\n{description}"),
                "policy": {"visibility": "listed"},
                "inputSchema": {"type": "object"}
            })
            .to_string(),
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
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn recommend_skills_light_uses_pattern_bridge() {
        let server = make_server();
        server
            .with_global_store(|store| {
                let closure = make_test_skill(
                    "skill:marmalade-closure-light",
                    "marmalade-closure-light",
                    "Write marmalade closure notes.",
                );
                store.hub_register(&closure).map_err(|e| e.to_string())?;
                let mut pattern = make_test_entry("pattern-zephyr-light");
                pattern.path = "/user/patterns/agent_os/zephyr-light".to_string();
                pattern.summary = "Zephyr requests use marmalade closure".to_string();
                pattern.text = "A zephyr task should route to marmalade closure notes.".to_string();
                pattern.metadata = json!({
                    "projection_kind": "pattern",
                    "projection_key": "zephyr-marmalade-light",
                    "source_event_id": "pattern-event-light-recommend",
                    "counters": {"seen": 4, "hit": 2}
                });
                store.upsert(&pattern).map_err(|e| e.to_string())
            })
            .expect("seed skill and pattern");

        let skills = recommend_skills_light(&server, "zephyr", 5).expect("recommend light");
        let hit = skills
            .iter()
            .find(|skill| {
                skill.get("id").and_then(Value::as_str) == Some("skill:marmalade-closure-light")
            })
            .expect("pattern-bridged skill should be recommended");
        assert_eq!(
            hit["pattern_refs"][0]["projection_key"],
            json!("zephyr-marmalade-light")
        );
    }
}
