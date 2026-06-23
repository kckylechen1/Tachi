use super::text::contains_any;
use crate::foundry_runtime_ops::helpers::dedup_strings;
use memory_core::MemoryEntry;
use regex::Regex;
use std::sync::OnceLock;

fn trim_context_token(raw: &str) -> String {
    raw.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '`' | '"' | '\'' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}'
            )
    })
    .trim_end_matches('.')
    .to_string()
}

fn looks_like_file_pattern(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return false;
    }
    let has_path_separator = value.contains('/');
    let has_wildcard = value.contains('*');
    if !has_path_separator && !has_wildcard {
        match value.rfind('.') {
            Some(dot) if dot > 0 && dot < value.len() - 1 => {}
            _ => return false,
        }
    }
    has_wildcard
        || value.ends_with(".rs")
        || value.ends_with(".ts")
        || value.ends_with(".tsx")
        || value.ends_with(".js")
        || value.ends_with(".jsx")
        || value.ends_with(".py")
        || value.ends_with(".go")
        || value.ends_with(".java")
        || value.ends_with(".md")
        || value.ends_with(".toml")
        || value.ends_with(".json")
        || value.ends_with(".yaml")
        || value.ends_with(".yml")
}

fn file_pattern_token_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"[A-Za-z0-9_./*\-]+").expect("file pattern regex compiles"))
}

fn wildcard_for_file_path(path: &str) -> Option<String> {
    let slash = path.rfind('/')?;
    let dot = path.rfind('.')?;
    if dot <= slash {
        return None;
    }
    Some(format!("{}/*{}", &path[..slash], &path[dot..]))
}

fn collect_file_patterns_from_text(text: &str, out: &mut Vec<String>) {
    for mat in file_pattern_token_regex().find_iter(text) {
        let candidate = mat.as_str();
        let bytes = candidate.as_bytes();
        if !bytes.iter().any(|b| matches!(b, b'*' | b'.' | b'/')) {
            continue;
        }
        let token = trim_context_token(candidate);
        if looks_like_file_pattern(&token) {
            if let Some(wildcard) = wildcard_for_file_path(&token) {
                out.push(token);
                out.push(wildcard);
            } else {
                out.push(token);
            }
        }
    }
}

fn collect_metadata_file_patterns(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(raw) => {
            let token = trim_context_token(raw);
            if looks_like_file_pattern(&token) {
                out.push(token.clone());
                if let Some(wildcard) = wildcard_for_file_path(&token) {
                    out.push(wildcard);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_metadata_file_patterns(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let key = key.to_ascii_lowercase();
                if key.contains("file") || key.contains("path") {
                    collect_metadata_file_patterns(value, out);
                }
            }
        }
        _ => {}
    }
}

pub(in crate::foundry_runtime_ops::maintenance) fn infer_file_patterns(
    source_entries: &[MemoryEntry],
) -> Vec<String> {
    let mut patterns = Vec::new();
    for entry in source_entries {
        let context_path = entry
            .metadata
            .get("context_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        for candidate in [&entry.path, &entry.location, context_path] {
            let token = trim_context_token(candidate);
            if looks_like_file_pattern(&token) {
                patterns.push(token.clone());
                if let Some(wildcard) = wildcard_for_file_path(&token) {
                    patterns.push(wildcard);
                }
            }
        }
        collect_metadata_file_patterns(&entry.metadata, &mut patterns);
        collect_file_patterns_from_text(&entry.text, &mut patterns);
    }
    dedup_strings(patterns).into_iter().take(12).collect()
}

fn looks_like_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    contains_any(
        &lower,
        &[
            "error",
            "failed",
            "failure",
            "panic",
            "exception",
            "could not",
            "cannot",
            "linker",
            "报错",
            "错误",
            "失败",
        ],
    )
}

pub(in crate::foundry_runtime_ops::maintenance) fn infer_error_patterns(
    distill_text: &str,
    source_entries: &[MemoryEntry],
) -> Vec<String> {
    let mut patterns = Vec::new();
    for text in std::iter::once(distill_text).chain(
        source_entries
            .iter()
            .flat_map(|entry| [entry.summary.as_str(), entry.text.as_str()]),
    ) {
        for line in text.lines() {
            if looks_like_error_line(line) {
                patterns.push(line.trim().chars().take(120).collect::<String>());
            }
        }
    }
    dedup_strings(patterns).into_iter().take(8).collect()
}
