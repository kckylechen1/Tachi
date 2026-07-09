const REDACTED_SECRET: &str = "[REDACTED]";

pub(crate) fn scrub_secrets(text: &str) -> (String, usize) {
    static REGEXES: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    let regexes = REGEXES.get_or_init(|| {
        [
            r#"(?i)(Authorization\s*:\s*Bearer\s+)([^\s`'\"]+)"#,
            r#"(?i)((?:api[_-]?key|token|secret|password)\s*[:=]\s*)([^\s`'\"]{8,})"#,
            r"(?i)\b(sk-[A-Za-z0-9_-]{20,})\b",
            r"(?i)\b(voy-[A-Za-z0-9_-]{20,})\b",
            r"(?i)\b(xox[baprs]-[A-Za-z0-9-]{20,})\b",
            r"(?i)\b(gh[pousr]_[A-Za-z0-9_]{20,})\b",
            r"(?i)\b(AKIA[0-9A-Z]{16})\b",
        ]
        .iter()
        .filter_map(|pattern| regex::Regex::new(pattern).ok())
        .collect()
    });

    let mut redactions = 0usize;
    let mut out = text.to_string();
    for re in regexes {
        let matches = re.find_iter(&out).count();
        if matches == 0 {
            continue;
        }
        redactions += matches;
        out = re
            .replace_all(&out, |caps: &regex::Captures<'_>| {
                if caps.len() > 2 {
                    format!("{}{}", &caps[1], REDACTED_SECRET)
                } else {
                    REDACTED_SECRET.to_string()
                }
            })
            .to_string();
    }
    (out, redactions)
}

pub(crate) fn scrub_think_tags(text: &str) -> String {
    static THINK_BLOCK_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = THINK_BLOCK_RE.get_or_init(|| {
        regex::Regex::new(r"(?is)<think\b[^>]*>.*?</think\s*>").expect("valid think-tag regex")
    });
    re.replace_all(text, "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_secrets_masks_bearer_tokens() {
        let input = "Authorization: Bearer sk-abc123def456ghi789jkl012mno345";
        let (output, count) = scrub_secrets(input);
        assert!(count > 0, "should detect bearer token");
        assert!(output.contains(REDACTED_SECRET));
        assert!(!output.contains("sk-abc123"));
    }

    #[test]
    fn scrub_secrets_masks_api_keys() {
        let input = r#"api_key: "sk-proj-abcdefghijklmnopqrstuvwxyz""#;
        let (output, count) = scrub_secrets(input);
        assert!(count > 0);
        assert!(output.contains(REDACTED_SECRET));
    }

    #[test]
    fn scrub_secrets_masks_aws_keys() {
        let input = "AWS key: AKIAIOSFODNN7EXAMPLE";
        let (output, count) = scrub_secrets(input);
        assert!(count > 0);
        assert!(output.contains(REDACTED_SECRET));
    }

    #[test]
    fn scrub_secrets_preserves_safe_text() {
        let input = "This is a normal text with no secrets at all.";
        let (output, count) = scrub_secrets(input);
        assert_eq!(count, 0);
        assert_eq!(output, input);
    }

    #[test]
    fn scrub_secrets_masks_github_tokens() {
        let input = "token=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";
        let (output, count) = scrub_secrets(input);
        assert!(count > 0);
        assert!(output.contains(REDACTED_SECRET));
    }
}
