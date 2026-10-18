//! Deterministic row-default policy shared by memory writes and `tachi repair`.
//!
//! Pure functions mapping a row's `(path, category, source)` to the domain and
//! retention policy it should carry. Lives outside `repair` so the save paths
//! (`memory_search_ops`, `continuity_ops`, `foundry_runtime_ops`) do not depend
//! upward on the admin repair layer; `repair::domain` (R9) and
//! `repair::retention` (R2) consume the same functions.

fn normalize_domain_label(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(trimmed.len().min(64));
    let mut last_was_sep = false;
    for ch in trimmed.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            last_was_sep = false;
            Some(ch.to_ascii_lowercase())
        } else if last_was_sep || out.is_empty() {
            None
        } else {
            last_was_sep = true;
            Some('_')
        };
        if let Some(mapped) = mapped {
            out.push(mapped);
            if out.len() >= 64 {
                break;
            }
        }
    }

    let normalized = out.trim_matches('_').to_string();
    (!normalized.is_empty()).then_some(normalized)
}

fn path_head(path: &str) -> Option<&str> {
    let trimmed = path.trim();
    let trimmed = trimmed.strip_prefix('/').unwrap_or(trimmed);
    let head = trimmed.split('/').next()?.trim();
    (!head.is_empty()).then_some(head)
}

fn infer_domain_from_row(path: &str, category: &str, source: &str) -> String {
    let path_lower = path.trim().to_ascii_lowercase();
    let category_lower = category.trim().to_ascii_lowercase();
    let source_lower = source.trim().to_ascii_lowercase();

    if path_lower.starts_with("/wiki")
        || path_lower.starts_with("/guide")
        || path_lower.starts_with("/skills")
        || path_lower.starts_with("/behavior")
        || matches!(category_lower.as_str(), "wiki" | "guide")
    {
        return "wiki".to_string();
    }
    if path_lower.starts_with("/trading/equity") {
        return "equity_trading".to_string();
    }
    if path_lower.starts_with("/trading") {
        return "trading".to_string();
    }
    if path_lower.starts_with("/_quarantine") {
        return "quarantine".to_string();
    }
    if path_lower.starts_with("/agent-logs")
        || path_lower.starts_with("/agent/")
        || path_lower == "/agent"
    {
        return "agent".to_string();
    }
    if path_lower.starts_with("/scratch") {
        return "scratch".to_string();
    }
    if path_lower.starts_with("/handoff") {
        return "handoff".to_string();
    }
    if path_lower.starts_with("/kanban") {
        return "kanban".to_string();
    }
    if path_lower.starts_with("/project") {
        return "project".to_string();
    }
    if path_lower.starts_with("/user") {
        return "user".to_string();
    }
    if path_lower.starts_with("/notes") {
        return "notes".to_string();
    }
    if path_lower.starts_with("/events") {
        return "events".to_string();
    }
    if path_lower.starts_with("/decisions") {
        return "decisions".to_string();
    }
    if path_lower.starts_with("/ghost") || category_lower == "ghost" {
        return "ghost".to_string();
    }
    if path_lower.starts_with("/foundry") || source_lower == "foundry_distill" {
        return "foundry".to_string();
    }
    if let Some(head) = path_head(&path_lower).and_then(normalize_domain_label) {
        return head;
    }
    "general".to_string()
}

pub(crate) fn repair_target(
    current_domain: Option<&str>,
    path: &str,
    category: &str,
    source: &str,
) -> Option<String> {
    let trimmed = current_domain.map(str::trim).unwrap_or("");
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return Some(infer_domain_from_row(path, category, source));
    }
    if trimmed.starts_with('/') || trimmed.contains('/') || trimmed.contains('\\') {
        return Some(infer_domain_from_row(path, category, source));
    }
    let normalized = normalize_domain_label(trimmed)?;
    (normalized != trimmed).then_some(normalized)
}

pub(crate) fn default_retention_for_row(path: &str, category: &str, source: &str) -> &'static str {
    let path_lower = path.trim().to_ascii_lowercase();
    let category_lower = category.trim().to_ascii_lowercase();
    let source_lower = source.trim().to_ascii_lowercase();

    if category_lower == "handoff"
        || category_lower == "kanban"
        || path_lower.starts_with("/handoff")
        || path_lower.starts_with("/kanban")
    {
        "pinned"
    } else if category_lower == "ghost" || path_lower.starts_with("/ghost") {
        "ephemeral"
    } else if path_lower.starts_with("/wiki")
        || path_lower.starts_with("/skills")
        || path_lower.starts_with("/behavior")
        || matches!(category_lower.as_str(), "decision" | "preference")
        || source_lower == "foundry_distill"
    {
        "permanent"
    } else {
        "durable"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_wiki_and_trading_domains() {
        assert_eq!(
            infer_domain_from_row("/wiki/agent/tachi", "fact", "manual"),
            "wiki"
        );
        assert_eq!(
            infer_domain_from_row("/trading/equity/positions", "fact", "manual"),
            "equity_trading"
        );
    }

    #[test]
    fn repairs_path_like_and_missing_domains() {
        assert_eq!(
            repair_target(Some("/scratch/sigil"), "/scratch/repro", "fact", "manual"),
            Some("scratch".to_string())
        );
        assert_eq!(
            repair_target(None, "/project/notes", "fact", "manual"),
            Some("project".to_string())
        );
        assert_eq!(
            repair_target(Some("Hyperion"), "/hyperion/notes", "fact", "manual"),
            Some("hyperion".to_string())
        );
    }
}
