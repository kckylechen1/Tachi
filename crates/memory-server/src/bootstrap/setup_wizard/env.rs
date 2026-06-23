// ─── Pure helpers (unit-tested) ──────────────────────────────────────────────

/// Merge a list of (key, value) pairs into an existing `KEY=VALUE`-style file
/// body. Lines that already define the same key (commented or not) are replaced
/// in place; otherwise the new entry is appended. The output always ends with
/// a single trailing newline.
pub(super) fn merge_config_env(existing: &str, updates: &[(String, String)]) -> String {
    let mut lines: Vec<String> = existing.lines().map(String::from).collect();

    for (key, value) in updates {
        let prefix = format!("{key}=");
        let mut replaced = false;
        for line in lines.iter_mut() {
            let trimmed = line.trim_start();
            let is_match = trimmed.starts_with(&prefix)
                || trimmed
                    .strip_prefix('#')
                    .map(|s| s.trim_start().starts_with(&prefix))
                    .unwrap_or(false);
            if is_match {
                // Keep inline comments (text after value) but drop the # prefix if the whole line was commented
                let inline_comment = if !trimmed.starts_with('#') {
                    // Active line — check for inline comment after the value
                    let after_key = trimmed.strip_prefix(&prefix).unwrap_or("");
                    // Find # that is preceded by whitespace (inline comment marker)
                    if let Some(pos) = after_key.find(" #") {
                        format!("{}", &after_key[pos..])
                    } else {
                        String::new()
                    }
                } else {
                    String::new() // Commented-out line: just activate it
                };
                *line = format!("{key}={value}{inline_comment}");
                replaced = true;
                break;
            }
        }
        if !replaced {
            lines.push(format!("{key}={value}"));
        }
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// True for config keys that should be redacted in any user-visible output.
pub(super) fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("KEY")
        || upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
}

/// Render a redacted preview of a secret value (`voy_••••abcd` style).
pub(super) fn mask_secret(value: &str) -> String {
    let v = value.trim();
    let chars: Vec<char> = v.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    if n <= 4 {
        return "•".repeat(n);
    }
    if n <= 8 {
        let tail: String = chars[n - 2..].iter().collect();
        return format!("••••{tail}");
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[n - 4..].iter().collect();
    format!("{head}••••{tail}")
}

/// Cheap heuristic for "this looks like a real API key" — we don't enforce a
/// specific provider format, but we do flag obviously-too-short strings so
/// users get a confirmation prompt before storing them.
pub(super) fn looks_like_api_key(value: &str) -> bool {
    let v = value.trim();
    if v.len() < 16 {
        return false;
    }
    // Must be printable ASCII; rule out accidental shell paste of whitespace.
    v.chars().all(|c| c.is_ascii_graphic())
}
