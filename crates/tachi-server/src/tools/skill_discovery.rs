use super::*;

pub(super) fn skill_discover_result_is_callable(cap: &Value) -> bool {
    cap.get("callable")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && cap
            .get("review_status")
            .and_then(|v| v.as_str())
            .is_some_and(|status| status.eq_ignore_ascii_case("approved"))
        && cap
            .get("health_status")
            .and_then(|v| v.as_str())
            .map(|status| {
                !matches!(
                    status.to_ascii_lowercase().as_str(),
                    "open" | "unhealthy" | "failing" | "broken" | "error"
                )
            })
            .unwrap_or(true)
}

pub(super) fn canonical_skill_name(cap: &Value) -> Option<String> {
    let raw = cap
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| cap.get("name").and_then(Value::as_str))?;
    let without_kind = raw
        .strip_prefix("host-skill:")
        .or_else(|| raw.strip_prefix("skill:waza-"))
        .or_else(|| raw.strip_prefix("skill:"))
        .unwrap_or(raw);
    let trimmed = without_kind.strip_prefix("waza/").unwrap_or(without_kind);
    let normalized = trimmed.trim().to_ascii_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

pub(super) fn discover_local_host_skills(query: &str, limit: usize) -> Vec<Value> {
    if limit == 0 {
        return Vec::new();
    }
    let query_tokens = skill_query_tokens(query);
    let mut candidates = Vec::new();
    for root in local_skill_roots() {
        collect_local_skills(&root, &query_tokens, &mut candidates);
    }
    candidates.sort_by(|a, b| {
        let score_b = b.get("_score").and_then(Value::as_i64).unwrap_or(0);
        let score_a = a.get("_score").and_then(Value::as_i64).unwrap_or(0);
        score_b.cmp(&score_a).then_with(|| {
            a.get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
        })
    });
    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|cap| {
            cap.get("id")
                .and_then(Value::as_str)
                .map(|id| seen.insert(id.to_string()))
                .unwrap_or(true)
        })
        .filter(|cap| {
            query_tokens.is_empty() || cap.get("_score").and_then(Value::as_i64) > Some(0)
        })
        .take(limit)
        .map(|mut cap| {
            if let Some(obj) = cap.as_object_mut() {
                obj.remove("_score");
            }
            cap
        })
        .collect()
}

pub(super) fn local_skill_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        roots.push(home.join(".agents/skills"));
        roots.push(home.join(".codex/skills"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("skill"));
    }
    roots
}

pub(super) fn skill_query_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for raw in query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let token = raw.to_ascii_lowercase();
        if matches!(
            token.as_str(),
            "中文" | "英文" | "chinese" | "english" | "zh" | "en"
        ) {
            continue;
        }
        if token.len() >= 3 || matches!(token.as_str(), "pr" | "ui" | "ux") {
            tokens.push(token.clone());
        }
        append_skill_query_aliases(raw, &mut tokens);
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

pub(super) fn append_skill_query_aliases(raw: &str, tokens: &mut Vec<String>) {
    let lower = raw.to_ascii_lowercase();
    let mut add = |aliases: &[&str]| {
        tokens.extend(aliases.iter().map(|alias| (*alias).to_string()));
    };

    if raw.contains("代码审查") || raw.contains("审查") || raw.contains("评审") {
        add(&["check", "code-review"]);
    }
    if raw.contains("修复") || raw.contains("修") || raw.contains("报错") {
        add(&["fix", "gh-fix-ci", "hunt", "repair"]);
    }
    if raw.contains("排查") || raw.contains("调试") || raw.contains("不工作") {
        add(&["debug", "hunt", "investigate"]);
    }
    if raw.contains("计划")
        || raw.contains("规划")
        || raw.contains("方案")
        || raw.contains("设计一下")
    {
        add(&["brainstorm", "plan", "think"]);
    }
    if raw.contains("设计") || raw.contains("前端") || raw.contains("页面") {
        add(&["design", "ui", "ux"]);
    }
    if raw.contains("合并") || raw.contains("提交") || raw.contains("推送") {
        add(&["check", "commit", "merge", "push"]);
    }
    if raw.contains("测试") || raw.contains("验证") {
        add(&["test", "verify"]);
    }
    if raw.contains("文档") || raw.contains("润色") {
        add(&["docs", "read", "write"]);
    }
    if lower == "ci" {
        add(&["fix", "gh-fix-ci"]);
    }
    if lower == "pr" {
        add(&["check", "pr", "review"]);
    }
}

pub(super) fn collect_local_skills(
    root: &std::path::Path,
    query_tokens: &[String],
    out: &mut Vec<Value>,
) {
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.exists() {
                if let Some(skill) = local_skill_from_file(&skill_md, query_tokens) {
                    out.push(skill);
                }
            } else {
                collect_local_skills(&path, query_tokens, out);
            }
        }
    }
}

pub(super) fn local_skill_from_file(
    path: &std::path::Path,
    query_tokens: &[String],
) -> Option<Value> {
    let content = std::fs::read_to_string(path).ok()?;
    let name = front_matter_value(&content, "name").unwrap_or_else(|| {
        path.parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("skill")
            .to_string()
    });
    let description = front_matter_value(&content, "description")
        .or_else(|| front_matter_value(&content, "when_to_use"))
        .unwrap_or_else(|| first_markdown_heading(&content).unwrap_or_default());
    let haystack = format!(
        "{} {} {}",
        name,
        description,
        front_matter_value(&content, "when_to_use").unwrap_or_default()
    )
    .to_ascii_lowercase();
    let score = if query_tokens.is_empty() {
        1
    } else {
        query_tokens
            .iter()
            .filter(|token| haystack.contains(token.as_str()))
            .count() as i64
    };
    if !query_tokens.is_empty() && score == 0 {
        return None;
    }
    Some(json!({
        "id": format!("host-skill:{}", name),
        "name": name,
        "description": description,
        "cap_type": "skill",
        "enabled": true,
        "review_status": "approved",
        "health_status": "healthy",
        "visibility": "host-local",
        "callable": true,
        "db": "host",
        "source": "host_skill_dir",
        "path": path.display().to_string(),
        "_score": score,
    }))
}

pub(super) fn front_matter_value(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines {
        if line == "---" {
            return None;
        }
        let Some((raw_key, raw_value)) = line.split_once(':') else {
            continue;
        };
        if raw_key.trim() == key {
            let value = raw_value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub(super) fn first_markdown_heading(content: &str) -> Option<String> {
    content
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|line| !line.is_empty())
        .map(str::to_string)
}
