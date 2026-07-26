const REDACTED_SECRET: &str = "[REDACTED]";

const SECRET_MARKERS: &[&str] = &[
    "api_key",
    "api-key",
    "authorization:",
    "bearer ",
    "client_secret",
    "oauth",
    "password=",
    "private key",
    "secret_key",
    "access_token",
    "refresh_token",
];

/// Shared reject-only secret detector for surfaces that must refuse rather
/// than redact. Keep token-shape knowledge in the same primitive used by
/// `scrub_secrets` so public-pilot screening cannot drift from production
/// search-output scrubbing.
pub(crate) fn contains_secret_like(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    SECRET_MARKERS.iter().any(|marker| lower.contains(marker)) || scrub_secrets(text).1 > 0
}

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

    #[test]
    fn reject_detector_uses_the_same_token_shapes_as_the_scrubber() {
        for token in [
            "sk-abcdefghijklmnopqrstuvwxyz123456",
            "voy-abcdefghijklmnopqrstuvwxyz123456",
            "xoxb-123456789012345678901234",
            "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            assert!(contains_secret_like(token));
        }
        assert!(!contains_secret_like("ordinary verification note"));
    }
}
