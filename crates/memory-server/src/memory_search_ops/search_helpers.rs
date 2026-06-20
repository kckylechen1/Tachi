use super::*;
use std::collections::hash_map::Entry;

pub(crate) fn normalize_search_relevance(results: &mut [(memory_core::SearchResult, DbScope)]) {
    let max_score = results
        .iter()
        .map(|(result, _)| result.score.final_score)
        .filter(|score: &f64| score.is_finite() && *score > 0.0)
        .fold(0.0_f64, f64::max);
    if max_score <= f64::EPSILON {
        return;
    }
    for (result, _) in results.iter_mut() {
        result.score.final_score = (result.score.final_score / max_score).clamp(0.0, 1.0);
    }
}

pub(crate) fn dedup_search_results(
    results: Vec<(memory_core::SearchResult, DbScope)>,
    top_k: usize,
) -> Vec<(memory_core::SearchResult, DbScope)> {
    let mut by_subject: HashMap<String, (memory_core::SearchResult, DbScope)> = HashMap::new();
    let mut passthrough = Vec::new();

    for (result, db_scope) in results {
        let Some(key) = dedup_subject_key(&result.entry) else {
            passthrough.push((result, db_scope));
            continue;
        };
        match by_subject.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert((result, db_scope));
            }
            Entry::Occupied(mut slot) => {
                if should_replace_dedup_result(&result, &slot.get().0) {
                    slot.insert((result, db_scope));
                }
            }
        }
    }

    let mut out = by_subject
        .into_values()
        .chain(passthrough)
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        b.0.score
            .final_score
            .partial_cmp(&a.0.score.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(top_k.saturating_mul(3).max(top_k).max(1));
    out
}

pub(crate) fn should_replace_dedup_result(
    candidate: &memory_core::SearchResult,
    current: &memory_core::SearchResult,
) -> bool {
    canonical_rank(&candidate.entry)
        .cmp(&canonical_rank(&current.entry))
        .then_with(|| candidate.entry.timestamp.cmp(&current.entry.timestamp))
        .then_with(|| {
            candidate
                .score
                .final_score
                .partial_cmp(&current.score.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .is_gt()
}

pub(crate) fn canonical_rank(entry: &MemoryEntry) -> u8 {
    if entry.source.eq_ignore_ascii_case("foundry_distill") {
        return 2;
    }
    if entry
        .metadata
        .get("wiki")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || entry.domain.as_deref() == Some("wiki")
        || entry.category.eq_ignore_ascii_case("wiki")
    {
        4
    } else if entry.is_guide() {
        3
    } else {
        1
    }
}

pub(crate) fn dedup_subject_key(entry: &MemoryEntry) -> Option<String> {
    let path = entry.path.trim();
    if path == "/wiki/_log" {
        return Some("wiki-log".to_string());
    }
    if path.starts_with("/wiki/") {
        return Some(format!("wiki:path:{}", path));
    }
    let topic = entry.topic.trim().to_ascii_lowercase();
    if !topic.is_empty()
        && (entry.source.eq_ignore_ascii_case("foundry_distill") || entry.is_guide())
    {
        return Some(format!(
            "distill:{topic}:{}:{}",
            path,
            entry.entities.join("|")
        ));
    }
    None
}

pub(crate) fn search_score(row: &serde_json::Value) -> f64 {
    row.get("score")
        .and_then(|score| score.get("final"))
        .and_then(serde_json::Value::as_f64)
        .or_else(|| row.get("relevance").and_then(serde_json::Value::as_f64))
        .unwrap_or(0.0)
}

pub(crate) fn apply_guide_context_boosts(
    results: &mut [(memory_core::SearchResult, DbScope)],
    file_context: Option<&str>,
    error_context: Option<&str>,
) {
    if file_context.is_none() && error_context.is_none() {
        return;
    }
    for (result, _) in results.iter_mut() {
        let boost = guide_context_boost(&result.entry, file_context, error_context);
        if boost > 0.0 {
            result.score.final_score = (result.score.final_score + boost).min(1.0);
        }
    }
}

pub(crate) fn guide_context_boost(
    entry: &MemoryEntry,
    file_context: Option<&str>,
    error_context: Option<&str>,
) -> f64 {
    if !entry.is_guide() {
        return 0.0;
    }
    let mut boost = 0.0;
    if let Some(context) = file_context {
        if context_matches_patterns(context, &entry.file_patterns()) {
            boost += 0.25;
        }
    }
    if let Some(context) = error_context {
        if context_matches_patterns(context, &entry.error_patterns()) {
            boost += 0.35;
        }
    }
    boost
}

pub(crate) fn context_matches_patterns(context: &str, patterns: &[String]) -> bool {
    let context = context.trim().to_ascii_lowercase();
    if context.is_empty() {
        return false;
    }
    patterns
        .iter()
        .map(|pattern| pattern.trim().to_ascii_lowercase())
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| pattern_matches_context(&pattern, &context))
}

pub(crate) fn pattern_matches_context(pattern: &str, context: &str) -> bool {
    if pattern.contains('*') {
        let mut cursor = 0usize;
        for segment in pattern.split('*').filter(|segment| !segment.is_empty()) {
            let Some(pos) = context[cursor..].find(segment) else {
                return false;
            };
            cursor += pos + segment.len();
        }
        true
    } else {
        context.contains(pattern) || pattern.contains(context)
    }
}

pub(crate) fn normalize_json_relevance(rows: &mut [serde_json::Value]) {
    let max_score = rows
        .iter()
        .filter_map(|row| row.get("relevance").and_then(serde_json::Value::as_f64))
        .filter(|score: &f64| score.is_finite() && *score > 0.0)
        .fold(0.0_f64, f64::max);
    if max_score <= f64::EPSILON {
        return;
    }
    for row in rows.iter_mut() {
        let Some(obj) = row.as_object_mut() else {
            continue;
        };
        if let Some(rel) = obj.get("relevance").and_then(serde_json::Value::as_f64) {
            let normalized = (rel / max_score).clamp(0.0, 1.0);
            obj.insert("relevance".into(), json!(round_score(normalized)));
            if let Some(score) = obj
                .get_mut("score")
                .and_then(serde_json::Value::as_object_mut)
            {
                score.insert("final".into(), json!(round_score(normalized)));
            }
        }
    }
}

pub(crate) fn round_score(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

pub(crate) fn list_available_named_projects() -> Vec<String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let app_home = std::env::var("TACHI_HOME")
        .map(|v| {
            if v.starts_with("~/") {
                home.join(&v[2..])
            } else {
                PathBuf::from(v)
            }
        })
        .unwrap_or_else(|_| home.join(".tachi"));
    let projects_dir = app_home.join("projects");
    let Ok(read_dir) = std::fs::read_dir(projects_dir) else {
        return Vec::new();
    };
    read_dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            if !entry.path().is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.contains("..") {
                return None;
            }
            let db_path = entry.path().join("memory.db");
            db_path.exists().then_some(name)
        })
        .collect()
}

pub(crate) fn named_project_db_exists(name: &str) -> bool {
    crate::MemoryServer::resolve_named_project_db_path(name)
        .map(|path| path.exists())
        .unwrap_or(false)
}

pub(crate) fn named_project_from_db_path(path: &std::path::Path) -> Option<String> {
    crate::path_utils::named_project_from_path(path).or_else(|| {
        crate::path_utils::plan_c_project_root_from_local_db(path)
            .and_then(|root| crate::path_utils::plan_c_dir_name_from_root(&root))
    })
}

/// Infer a named project library from query text when the caller omitted `project`.
/// Git repo folder name for Plan C (`~/.tachi/projects/<name>/memory.db`), if any.
pub(crate) fn resolve_workspace_named_project() -> Option<String> {
    let git_root = crate::utils::find_project_git_root()?;
    crate::path_utils::plan_c_dir_name_from_root(&git_root)
}

pub(crate) fn infer_search_project(query: &str, domain: Option<&str>) -> Option<String> {
    infer_search_project_with(query, domain, super::routing_config::RoutingConfig::get())
}

/// Config-injectable core. Domain-specific routing (tickers, finance terms) is
/// supplied by [`RoutingConfig`] rather than hardcoded here; the engine itself
/// stays domain-agnostic.
fn infer_search_project_with(
    query: &str,
    domain: Option<&str>,
    config: &super::routing_config::RoutingConfig,
) -> Option<String> {
    if let Some(domain) = domain.map(str::trim).filter(|d| !d.is_empty()) {
        for route in &config.domain_routes {
            if route.domains.iter().any(|d| d.eq_ignore_ascii_case(domain))
                && named_project_db_exists(&route.project)
            {
                return Some(route.project.clone());
            }
        }
    }

    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    let q_lower = q.to_lowercase();

    if let Some(project) = config.ticker_route_project.as_deref() {
        static TICKER_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let ticker_re = TICKER_RE.get_or_init(|| regex::Regex::new(r"\b\d{6}\b").unwrap());
        if ticker_re.is_match(q) && named_project_db_exists(project) {
            return Some(project.to_string());
        }
    }

    for project in list_available_named_projects() {
        if q_lower.contains(&project.to_lowercase()) {
            return Some(project);
        }
    }

    for route in &config.project_routes {
        // Check the cheap term match before the disk-I/O project-exists lookup.
        if route.terms.iter().any(|term| q_lower.contains(term))
            && named_project_db_exists(&route.project)
        {
            return Some(route.project.clone());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_entry(id: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            path: "/test".into(),
            summary: text[..text.len().min(30)].into(),
            text: text.into(),
            importance: 0.7,
            timestamp: chrono::Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: "".into(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".into(),
            source: "test".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn infer_search_project_routes_tickers_to_hyperion() {
        if !named_project_db_exists("hyperion") {
            return;
        }
        // Use the default config explicitly so the test is deterministic
        // regardless of any ~/.tachi/routing.json on the test host.
        let config = crate::memory_search_ops::routing_config::RoutingConfig::default();
        assert_eq!(
            infer_search_project_with("688981 止损记录", None, &config).as_deref(),
            Some("hyperion")
        );
        assert_eq!(
            infer_search_project_with("portfolio risk", Some("equity_trading"), &config).as_deref(),
            Some("hyperion")
        );
    }

    #[test]
    fn empty_routing_config_makes_engine_domain_agnostic() {
        // With every routing list emptied, no finance/ticker term routes anywhere
        // — the engine is fully generic. Only an explicit project-name match
        // (handled separately) would still route.
        let config = crate::memory_search_ops::routing_config::RoutingConfig {
            domain_routes: Vec::new(),
            ticker_route_project: None,
            project_routes: Vec::new(),
            foreign_domain_word_terms: Vec::new(),
            foreign_domain_substring_terms: Vec::new(),
            foreign_domains: Vec::new(),
            foreign_path_prefixes: Vec::new(),
        };
        assert_eq!(
            infer_search_project_with("688981 止损记录", None, &config),
            None
        );
        assert_eq!(
            infer_search_project_with("portfolio risk", Some("equity_trading"), &config),
            None
        );
    }

    #[test]
    fn normalize_search_relevance_scales_top_hit_to_one() {
        let mut results = vec![
            (
                memory_core::SearchResult {
                    entry: test_entry("a", "alpha"),
                    score: memory_core::HybridScore {
                        vector: 0.2,
                        fts: 0.1,
                        symbolic: 0.0,
                        decay: 0.0,
                        final_score: 0.03,
                    },
                },
                DbScope::Project,
            ),
            (
                memory_core::SearchResult {
                    entry: test_entry("b", "beta"),
                    score: memory_core::HybridScore {
                        vector: 0.1,
                        fts: 0.05,
                        symbolic: 0.0,
                        decay: 0.0,
                        final_score: 0.015,
                    },
                },
                DbScope::Project,
            ),
        ];
        normalize_search_relevance(&mut results);
        assert!((results[0].0.score.final_score - 1.0).abs() < f64::EPSILON);
        assert!((results[1].0.score.final_score - 0.5).abs() < 0.01);
    }
}
