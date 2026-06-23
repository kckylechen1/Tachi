use super::*;

/// Map a `gh` CLI failure string into a typed `GhError`. The input is already
/// sanitized by `run_gh`. We classify by substring so callers can distinguish
/// "PR doesn't exist" (NotFound, terminal) from "API rate limit" (transient).
pub(in crate::gh_ops) fn classify_gh_error(raw: &str) -> GhError {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("could not resolve") || lower.contains("not found") || lower.contains("404") {
        GhError::NotFound(raw.to_string())
    } else if lower.contains("rate limit") || lower.contains("403") && lower.contains("rate") {
        GhError::RateLimited(raw.to_string())
    } else {
        GhError::Sanitized(raw.to_string())
    }
}

pub(in crate::gh_ops) fn is_no_checks_reported(raw: &str) -> bool {
    raw.to_ascii_lowercase().contains("no checks reported")
}
