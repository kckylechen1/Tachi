use std::collections::BTreeMap;
use std::sync::OnceLock;

pub const REDACTED_SECRET: &str = "[REDACTED]";

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
/// `redact_secret_like_text` so public-pilot screening cannot drift from
/// production search-output scrubbing.
pub fn contains_secret_like(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    SECRET_MARKERS.iter().any(|marker| lower.contains(marker))
        || redact_secret_like_text(text).1 > 0
}

pub fn redact_secret_like_text(text: &str) -> (String, usize) {
    let (out, redactions, _) = redact_secret_like_text_with_categories(text);
    (out, redactions)
}

/// Same redaction as [`redact_secret_like_text`] (identical output and count),
/// plus how many spans each detector class matched. The category names are
/// stable response vocabulary (`secret_redaction_categories` on save
/// receipts); they name the pattern class that fired, never the matched text.
pub fn redact_secret_like_text_with_categories(
    text: &str,
) -> (String, usize, BTreeMap<&'static str, usize>) {
    let mut redactions = 0usize;
    let mut categories = BTreeMap::new();
    let mut out = text.to_string();
    for (category, re) in secret_regexes() {
        let matches = re.find_iter(&out).count();
        if matches == 0 {
            continue;
        }
        redactions += matches;
        *categories.entry(*category).or_insert(0) += matches;
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
    (out, redactions, categories)
}

/// Detector classes in application order. Patterns are unchanged; the leading
/// name is the category reported by [`redact_secret_like_text_with_categories`].
fn secret_regexes() -> &'static [(&'static str, regex::Regex)] {
    static REGEXES: OnceLock<Vec<(&'static str, regex::Regex)>> = OnceLock::new();
    REGEXES
        .get_or_init(|| {
            [
                (
                    "bearer_authorization",
                    r#"(?i)(Authorization\s*:\s*Bearer\s+)([^\s`'\"]+)"#,
                ),
                (
                    "labeled_credential",
                    r#"(?i)((?:api[_-]?key|token|secret|password)\s*[:=]\s*)([^\s`'\"]{8,})"#,
                ),
                ("sk_api_key", r"(?i)\b(sk-[A-Za-z0-9_-]{20,})\b"),
                ("voy_api_key", r"(?i)\b(voy-[A-Za-z0-9_-]{20,})\b"),
                ("slack_token", r"(?i)\b(xox[baprs]-[A-Za-z0-9-]{20,})\b"),
                ("github_token", r"(?i)\b(gh[pousr]_[A-Za-z0-9_]{20,})\b"),
                ("aws_access_key_id", r"(?i)\b(AKIA[0-9A-Z]{16})\b"),
            ]
            .into_iter()
            .filter_map(|(category, pattern)| {
                regex::Regex::new(pattern).ok().map(|re| (category, re))
            })
            .collect()
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn categories_name_the_detector_class_without_changing_the_redaction() {
        // tachi#2241: the `token:` label is what fires here, not the value's shape.
        let text = "Synthetic checkpoint token: AmberKeel-2113 and AKIAIOSFODNN7EXAMPLE";
        let (out, count, categories) = redact_secret_like_text_with_categories(text);
        assert_eq!(redact_secret_like_text(text), (out.clone(), count));
        assert_eq!(count, 2);
        assert_eq!(categories.get("labeled_credential"), Some(&1));
        assert_eq!(categories.get("aws_access_key_id"), Some(&1));
        assert!(!out.contains("AmberKeel") && !out.contains("AKIA"));
    }
}
