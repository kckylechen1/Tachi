use serde_json::Value;

pub(crate) fn trim_opt(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

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

pub(crate) fn value_to_template_text(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        s.to_string()
    } else {
        v.to_string()
    }
}

fn is_safe_template_arg_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c == '-' || c.is_ascii_alphanumeric())
}

pub(crate) fn render_skill_prompt_template(
    template: &str,
    args: &serde_json::Map<String, Value>,
) -> Result<String, serde_json::Error> {
    let args_json = serde_json::to_string(args)?;
    let mut prompt = template.replace("{{args_json}}", &args_json);
    prompt = prompt.replace("{{args}}", &args_json);

    for (key, value) in args {
        if matches!(key.as_str(), "args" | "args_json" | "input") || !is_safe_template_arg_key(key)
        {
            continue;
        }
        let placeholder = format!("{{{{{key}}}}}");
        prompt = prompt.replace(&placeholder, &value_to_template_text(value));
    }

    if prompt.contains("{{input}}") {
        let input = args
            .get("input")
            .map(value_to_template_text)
            .unwrap_or(args_json);
        prompt = prompt.replace("{{input}}", &input);
    }

    Ok(prompt)
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

fn redact_sensitive_string(input: &str) -> String {
    let Some(query_start) = input.find('?') else {
        return redact_inline_secret_markers(input);
    };
    let (prefix, rest) = input.split_at(query_start + 1);
    let (query, suffix) = match rest.find('#') {
        Some(fragment_start) => rest.split_at(fragment_start),
        None => (rest, ""),
    };
    let redacted_query = query
        .split('&')
        .map(|part| {
            let Some((name, _value)) = part.split_once('=') else {
                return part.to_string();
            };
            if is_sensitive_key(name) {
                format!("{name}=[REDACTED]")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    redact_inline_secret_markers(&format!("{prefix}{redacted_query}{suffix}"))
}

fn redact_inline_secret_markers(input: &str) -> String {
    let mut out = input.to_string();
    for marker in ["Bearer ", "bearer "] {
        if let Some(start) = out.find(marker) {
            let value_start = start + marker.len();
            let value_end = out[value_start..]
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '&'))
                .map(|idx| value_start + idx)
                .unwrap_or(out.len());
            out.replace_range(value_start..value_end, "[REDACTED]");
        }
    }
    out
}

/// Stable hash function (FNV-1a). Deterministic across Rust toolchain versions,
/// unlike DefaultHasher which uses SipHash with randomized keys.
pub(crate) fn stable_hash(input: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{:016x}", hash)
}
