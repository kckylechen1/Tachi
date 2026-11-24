use serde_json::Value;

pub(crate) fn sanitize_safe_path_name(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches(|ch| matches!(ch, '.' | '_' | '-'));
    if sanitized.is_empty() {
        "unnamed".to_string()
    } else {
        sanitized.to_string()
    }
}

/// Collapse whitespace and truncate to `limit` chars (including ellipsis).
pub(crate) fn compact_text_line(text: &str, limit: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= limit {
        one_line
    } else {
        let keep = limit.saturating_sub(3);
        format!("{}...", one_line.chars().take(keep).collect::<String>())
    }
}

pub(crate) fn redact_sensitive_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *child = Value::String("[REDACTED]".to_string());
                } else {
                    redact_sensitive_value(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_sensitive_value(item);
            }
        }
        Value::String(s) => {
            *s = redact_sensitive_string(s);
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "apikey",
        "api_key",
        "token",
        "secret",
        "password",
        "authorization",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

/// What a redacted field renders as. A unit struct rather than a string
/// literal, so `{:?}` prints it bare instead of quoting it like a value that
/// could be mistaken for the real one.
pub(crate) struct Redacted;

impl std::fmt::Debug for Redacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

fn redact_sensitive_string(input: &str) -> String {
    // A URL holds no whitespace, so every `?` inside one word opens a query of
    // its own. An earlier URL's query or fragment never hides a later one.
    let mut out = String::with_capacity(input.len());
    for piece in input.split_inclusive(char::is_whitespace) {
        let (word, gap) = piece.split_at(piece.trim_end_matches(char::is_whitespace).len());
        out.push_str(&redact_queries(word));
        out.push_str(gap);
    }
    redact_inline_secret_markers(&out)
}

/// Every `?` in a whitespace-free token opens a query that runs to its `#`.
fn redact_queries(word: &str) -> String {
    let Some(query_start) = word.find('?') else {
        return word.to_string();
    };
    let (head, tail) = word.split_at(query_start + 1);
    let (query, fragment) = match tail.find('#') {
        Some(fragment_start) => tail.split_at(fragment_start),
        None => (tail, ""),
    };
    format!("{head}{}{fragment}", redact_parameters(query))
}

/// Parameters are separated by `&`; a `?` inside a query opens a nested URL.
fn redact_parameters(query: &str) -> String {
    query
        .split_inclusive(['&', '?'])
        .map(|parameter| {
            let Some((name, _)) = parameter.split_once('=') else {
                return parameter.to_string();
            };
            if !is_sensitive_key(name) {
                return parameter.to_string();
            }
            let separator = parameter
                .chars()
                .next_back()
                .filter(|last| matches!(last, '&' | '?'))
                .map(String::from)
                .unwrap_or_default();
            format!("{name}=[REDACTED]{separator}")
        })
        .collect()
}

/// Every `Bearer` value is redacted, not only the first one in the text.
fn redact_inline_secret_markers(input: &str) -> String {
    const MARKER: &[u8] = b"bearer ";
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = find_ignore_ascii_case(rest, MARKER) {
        let value_start = start + MARKER.len();
        let value_end = rest[value_start..]
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '&'))
            .map_or(rest.len(), |index| value_start + index);
        out.push_str(&rest[..value_start]);
        out.push_str("[REDACTED]");
        rest = &rest[value_end..];
    }
    out.push_str(rest);
    out
}

/// Byte position of an ASCII `needle` in `text`, ignoring ASCII case.
fn find_ignore_ascii_case(text: &str, needle: &[u8]) -> Option<usize> {
    let last_start = text.len().checked_sub(needle.len())?;
    (0..=last_start)
        .find(|&index| text.as_bytes()[index..index + needle.len()].eq_ignore_ascii_case(needle))
}

pub(crate) fn append_warning(
    resp: &mut serde_json::Map<String, serde_json::Value>,
    warning: impl Into<String>,
) {
    let warning = warning.into();
    if warning.is_empty() {
        return;
    }

    match resp.remove("warning") {
        Some(serde_json::Value::String(existing)) if !existing.is_empty() => {
            resp.insert(
                "warning".into(),
                serde_json::Value::String(format!("{existing}; {warning}")),
            );
        }
        _ => {
            resp.insert("warning".into(), serde_json::Value::String(warning));
        }
    }
}
