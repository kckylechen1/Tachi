#[cfg(test)]
const REDACTED_SECRET: &str = tachi_runtime_core::secret_scan::REDACTED_SECRET;

pub(crate) fn scrub_secrets(text: &str) -> (String, usize) {
    tachi_runtime_core::secret_scan::redact_secret_like_text(text)
}

/// Per-category redaction counts (`bearer_authorization`, `labeled_credential`,
/// ...), keyed by detector class. Never contains matched text.
pub(crate) type SecretRedactionCategories = std::collections::BTreeMap<&'static str, usize>;

/// [`scrub_secrets`] plus the detector class behind each redacted span, so a
/// save receipt can explain what fired without echoing the secret.
pub(crate) fn scrub_secrets_with_categories(
    text: &str,
) -> (String, usize, SecretRedactionCategories) {
    tachi_runtime_core::secret_scan::redact_secret_like_text_with_categories(text)
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
