use super::super::*;

pub(in crate::gh_ops) fn comment_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

pub(in crate::gh_ops) fn first_meaningful_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with("```")
                && !line.starts_with("---")
                && !line.starts_with("<!--")
                && !line.starts_with("![")
        })
        .unwrap_or(body.trim())
        .chars()
        .take(220)
        .collect()
}

pub(in crate::gh_ops) fn lower_contains_any(lower_haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| lower_haystack.contains(needle))
}

pub(in crate::gh_ops) fn classify_review_comment(body: &str, path: Option<&str>) -> &'static str {
    let combined = match path {
        Some(path) => format!("{body}\n{path}"),
        None => body.to_string(),
    };
    let lower = combined.to_ascii_lowercase();
    if lower_contains_any(
        &lower,
        &[
            "security",
            "secret",
            "token",
            "credential",
            "injection",
            "permission",
            "auth",
        ],
    ) {
        "security"
    } else if lower_contains_any(
        &lower,
        &[
            "panic",
            "bug",
            "incorrect",
            "wrong",
            "race",
            "deadlock",
            "lock",
            "fail",
            "regression",
            "root cause",
        ],
    ) {
        "correctness"
    } else if lower_contains_any(
        &lower,
        &["test", "coverage", "assert", "fixture", "mock", "case"],
    ) {
        "tests"
    } else if lower_contains_any(
        &lower,
        &[
            "api",
            "schema",
            "contract",
            "compat",
            "breaking",
            "parameter",
            "field",
        ],
    ) {
        "api-contract"
    } else if lower_contains_any(
        &lower,
        &[
            "maintain",
            "duplicate",
            "complex",
            "refactor",
            "simpl",
            "readability",
        ],
    ) {
        "maintainability"
    } else if lower_contains_any(&lower, &["nit", "style", "format", "typo", "naming"]) {
        "style"
    } else {
        "unclassified"
    }
}

pub(in crate::gh_ops) fn author_matches_filter(comment: &Value, lower_filter: &str) -> bool {
    if lower_filter.is_empty() {
        return true;
    }
    comment
        .get("author")
        .and_then(Value::as_str)
        .map(|author| author.to_ascii_lowercase().contains(lower_filter))
        .unwrap_or(false)
}

pub(in crate::gh_ops) fn infer_future_rule(
    category: &str,
    path: Option<&str>,
    body: &str,
) -> String {
    let scope = path.unwrap_or("similar code");
    let line = first_meaningful_line(body);
    match category {
        "security" => format!("When changing {scope}, verify trust boundaries and secret handling: {line}"),
        "correctness" => format!("When changing {scope}, check this failure mode before shipping: {line}"),
        "tests" => format!("When changing {scope}, add or update regression coverage for: {line}"),
        "api-contract" => format!("When changing {scope}, preserve or explicitly migrate the API/schema contract: {line}"),
        "maintainability" => format!("When changing {scope}, keep the simpler local pattern and avoid this maintainability trap: {line}"),
        "style" => format!("Style-only review signal for {scope}; do not promote unless it repeats: {line}"),
        _ => format!("Review signal for {scope}; leader must triage before promotion: {line}"),
    }
}
