use crate::memory_search_ops::search_memory_rows;
use crate::tool_params::SearchMemoryParams;
use crate::MemoryServer;
use serde_json::{json, Value};

const FEEDBACK_RULE_SEARCH_TASK_MAX_CHARS: usize = 512;

#[derive(Clone, Debug)]
pub(crate) struct FeedbackRuleQuery {
    pub task: String,
    pub task_type: Option<String>,
    pub profile: Option<String>,
    pub stage: Option<String>,
    pub keywords: Vec<String>,
    pub project: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct FeedbackRuleHit {
    pub id: String,
    pub path: String,
    pub title: String,
    pub text: String,
    pub prompt_patch: String,
    pub evidence_contract: Vec<String>,
    pub score: usize,
}

pub(crate) fn normalize_feedback_rule_save(
    kind: &Option<String>,
    path: &mut Option<String>,
    category: &mut Option<String>,
    keywords: &mut Vec<String>,
    metadata: &mut Option<Value>,
) {
    let Some(kind) = kind.as_deref() else {
        return;
    };
    let kind = kind.to_ascii_lowercase();
    if !matches!(kind.as_str(), "feedback_rule" | "prompt_rule") {
        return;
    }

    if path.as_deref().map(str::trim).unwrap_or("").is_empty() {
        *path = Some("/feedback".to_string());
    }
    if category.as_deref().map(str::trim).unwrap_or("").is_empty() {
        *category = Some("prompt_rule".to_string());
    }

    push_unique(keywords, "feedback_rule");
    push_unique(keywords, "prompt_rule");

    let mut obj = metadata
        .take()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    obj.insert("kind".to_string(), json!("feedback_rule"));
    obj.entry("category".to_string())
        .or_insert_with(|| json!("prompt_rule"));
    *metadata = Some(Value::Object(obj));
}

pub(crate) async fn applicable_feedback_rules(
    server: &MemoryServer,
    query: FeedbackRuleQuery,
) -> Vec<FeedbackRuleHit> {
    let search_query = feedback_search_query(&query);
    let rows = match search_memory_rows(
        server,
        SearchMemoryParams {
            query: search_query,
            query_vec: None,
            top_k: 8,
            path_prefix: Some("/feedback".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 24,
            mmr_threshold: Some(0.75),
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: query.project.clone(),
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: true,
        },
        false,
    )
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("feedback rule search failed: {err}");
            return Vec::new();
        }
    };

    let mut hits = rows
        .iter()
        .filter_map(|row| feedback_rule_hit(row, &query))
        .collect::<Vec<_>>();
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
    hits.truncate(5);
    hits
}

pub(crate) fn render_feedback_rules_section(rules: &[FeedbackRuleHit]) -> Option<String> {
    if rules.is_empty() {
        return None;
    }

    let mut out = String::from("## Applicable feedback rules\n");
    for rule in rules {
        out.push('\n');
        out.push_str(&format!("### {}\n", rule.title));
        out.push_str(&format!("- rule_id: `{}`\n", rule.id));
        out.push_str(&format!("- path: `{}`\n", rule.path));
        if !rule.prompt_patch.trim().is_empty() {
            render_multiline_field(&mut out, "prompt_patch", rule.prompt_patch.trim());
        } else {
            render_multiline_field(&mut out, "guidance", rule.text.trim());
        }
        if !rule.evidence_contract.is_empty() {
            out.push_str("- evidence_contract:\n");
            for item in &rule.evidence_contract {
                out.push_str(&format!("  - {item}\n"));
            }
        }
    }
    Some(out)
}

pub(crate) fn feedback_rules_trace(rules: &[FeedbackRuleHit]) -> Value {
    json!({
        "status": if rules.is_empty() { "none" } else { "applied" },
        "count": rules.len(),
        "rules": rules.iter().map(|rule| {
            json!({
                "id": rule.id,
                "path": rule.path,
                "title": rule.title,
                "prompt_patch": rule.prompt_patch,
                "evidence_contract": rule.evidence_contract,
                "score": rule.score,
            })
        }).collect::<Vec<_>>(),
    })
}

fn feedback_rule_hit(row: &Value, query: &FeedbackRuleQuery) -> Option<FeedbackRuleHit> {
    let metadata = row.get("metadata").unwrap_or(&Value::Null);
    if !is_feedback_rule(row, metadata) {
        return None;
    }

    let applies = metadata.get("applies_to").unwrap_or(&Value::Null);
    if !matches_filter(applies.get("task_type"), query.task_type.as_deref()) {
        return None;
    }
    if !matches_filter(applies.get("profiles"), query.profile.as_deref()) {
        return None;
    }
    if !matches_filter(applies.get("stage"), query.stage.as_deref()) {
        return None;
    }

    let mut score = 0;
    score += token_hits(&query.task, metadata.get("trigger_keywords"));
    score += token_hits(&query.task, metadata.get("keywords"));
    score += token_hits(&query.keywords.join(" "), metadata.get("trigger_keywords"));
    if query.task_type.is_some()
        && matches_filter(applies.get("task_type"), query.task_type.as_deref())
    {
        score += 3;
    }
    if query.profile.is_some() && matches_filter(applies.get("profiles"), query.profile.as_deref())
    {
        score += 2;
    }
    if query.stage.is_some() && matches_filter(applies.get("stage"), query.stage.as_deref()) {
        score += 1;
    }
    if score == 0 && has_restrictive_applies_to(applies) {
        return None;
    }

    let id = row.get("id")?.as_str()?.to_string();
    let path = row
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("/feedback")
        .to_string();
    let title = row
        .get("topic")
        .and_then(Value::as_str)
        .or_else(|| row.get("summary").and_then(Value::as_str))
        .or_else(|| metadata.get("title").and_then(Value::as_str))
        .unwrap_or("Feedback rule")
        .to_string();
    let text = row
        .get("excerpt")
        .and_then(Value::as_str)
        .or_else(|| row.get("summary").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let prompt_patch = metadata
        .get("prompt_patch")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let evidence_contract = metadata
        .get("evidence_contract")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|item| !item.trim().is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(FeedbackRuleHit {
        id,
        path,
        title,
        text,
        prompt_patch,
        evidence_contract,
        score,
    })
}

fn feedback_search_query(query: &FeedbackRuleQuery) -> String {
    let mut parts = vec![truncate_chars(
        &query.task,
        FEEDBACK_RULE_SEARCH_TASK_MAX_CHARS,
    )];
    if let Some(task_type) = query.task_type.as_deref() {
        parts.push(task_type.to_string());
    }
    if let Some(profile) = query.profile.as_deref() {
        parts.push(profile.to_string());
    }
    if let Some(stage) = query.stage.as_deref() {
        parts.push(stage.to_string());
    }
    parts.extend(query.keywords.iter().cloned());
    parts.join(" ")
}

fn is_feedback_rule(row: &Value, metadata: &Value) -> bool {
    let path = row.get("path").and_then(Value::as_str).unwrap_or("");
    if !path.starts_with("/feedback") {
        return false;
    }
    let kind = metadata
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let category = row
        .get("category")
        .and_then(Value::as_str)
        .or_else(|| metadata.get("category").and_then(Value::as_str))
        .unwrap_or("")
        .to_ascii_lowercase();
    kind == "feedback_rule" || kind == "prompt_rule" || category == "prompt_rule"
}

fn matches_filter(filter: Option<&Value>, actual: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let values = string_values(filter);
    if values.is_empty() {
        return true;
    }
    let Some(actual) = actual.map(|value| value.to_ascii_lowercase()) else {
        return false;
    };
    values
        .iter()
        .any(|value| value == "*" || value.eq_ignore_ascii_case(&actual))
}

fn has_restrictive_applies_to(applies: &Value) -> bool {
    ["task_type", "profiles", "stage"]
        .iter()
        .any(|key| !string_values(applies.get(*key).unwrap_or(&Value::Null)).is_empty())
}

fn token_hits(haystack: &str, needles: Option<&Value>) -> usize {
    let haystack = haystack.to_ascii_lowercase();
    string_values(needles.unwrap_or(&Value::Null))
        .iter()
        .filter(|needle| contains_keyword(&haystack, needle))
        .count()
}

fn render_multiline_field(out: &mut String, label: &str, value: &str) {
    if !value.contains('\n') {
        out.push_str(&format!("- {label}: {value}\n"));
        return;
    }

    out.push_str(&format!("- {label}:\n"));
    for line in value.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
}

fn contains_keyword(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }

    let mut offset = 0;
    while let Some(pos) = haystack[offset..].find(&needle) {
        let start = offset + pos;
        let end = start + needle.len();
        if is_keyword_boundary(haystack[..start].chars().next_back())
            && is_keyword_boundary(haystack[end..].chars().next())
        {
            return true;
        }
        offset = end;
    }
    false
}

fn is_keyword_boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn string_values(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => vec![s.trim().to_ascii_lowercase()],
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase)
            .collect(),
        _ => Vec::new(),
    }
}

fn push_unique(items: &mut Vec<String>, value: &str) {
    if !items.iter().any(|item| item.eq_ignore_ascii_case(value)) {
        items.push(value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_hits_requires_keyword_boundaries() {
        let needles = json!(["rust", "dead code", "grep"]);

        assert_eq!(token_hits("trust me, this is robust", Some(&needles)), 0);
        assert_eq!(
            token_hits("Rust workers should grep for dead code.", Some(&needles)),
            3
        );
        assert_eq!(token_hits("dead-code grep", Some(&needles)), 1);
    }

    #[test]
    fn feedback_search_query_truncates_long_task_text() {
        let query = FeedbackRuleQuery {
            task: "a".repeat(FEEDBACK_RULE_SEARCH_TASK_MAX_CHARS + 200),
            task_type: Some("review".to_string()),
            profile: None,
            stage: None,
            keywords: vec!["grep".to_string()],
            project: None,
        };

        let rendered = feedback_search_query(&query);
        assert!(rendered.starts_with(&"a".repeat(FEEDBACK_RULE_SEARCH_TASK_MAX_CHARS)));
        assert!(!rendered.contains(&"a".repeat(FEEDBACK_RULE_SEARCH_TASK_MAX_CHARS + 1)));
        assert!(rendered.ends_with("review grep"));
    }

    #[test]
    fn render_feedback_rules_section_preserves_multiline_prompt_patch() {
        let section = render_feedback_rules_section(&[FeedbackRuleHit {
            id: "rule-1".to_string(),
            path: "/feedback/rule-1".to_string(),
            title: "Evidence rule".to_string(),
            text: String::new(),
            prompt_patch: "Use this checklist:\n- search identifiers\n- list files".to_string(),
            evidence_contract: Vec::new(),
            score: 1,
        }])
        .expect("section");

        assert!(section.contains("- prompt_patch:\n"));
        assert!(section.contains("  - search identifiers\n"));
        assert!(section.contains("  - list files\n"));
    }
}
