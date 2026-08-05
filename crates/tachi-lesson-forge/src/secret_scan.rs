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
    let mut redactions = 0usize;
    let mut out = text.to_string();
    for re in secret_regexes() {
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

fn secret_regexes() -> &'static [regex::Regex] {
    static REGEXES: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    REGEXES
        .get_or_init(|| {
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
}
