use crate::DbScope;
use memcore::MemoryEntry;
use serde_json::{json, Value};
use std::collections::{hash_map::Entry, HashMap};

pub(crate) fn normalize_search_relevance(results: &mut [(memcore::SearchResult, DbScope)]) {
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
    results: Vec<(memcore::SearchResult, DbScope)>,
    top_k: usize,
) -> Vec<(memcore::SearchResult, DbScope)> {
    let mut by_subject: HashMap<String, (memcore::SearchResult, DbScope)> = HashMap::new();
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
    candidate: &memcore::SearchResult,
    current: &memcore::SearchResult,
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
    results: &mut [(memcore::SearchResult, DbScope)],
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
    let max_rank_score = rows
        .iter()
        .map(search_score)
        .filter(|score| score.is_finite() && *score > 0.0)
        .fold(0.0_f64, f64::max);
    for row in rows.iter_mut() {
        let existing_relevance = row.get("relevance").and_then(serde_json::Value::as_f64);
        let rank_score = search_score(row);
        let score = row.get("score");
        let graph_only = score.is_some_and(|score| {
            ["vector", "fts", "symbolic", "decay"]
                .into_iter()
                .all(|key| {
                    score
                        .get(key)
                        .and_then(serde_json::Value::as_f64)
                        .is_some_and(|value| value.is_finite() && value.abs() <= f64::EPSILON)
                })
        }) && row
            .get("rerank_score")
            .and_then(serde_json::Value::as_f64)
            .is_none_or(|value| !value.is_finite() || value <= 0.0)
            && rank_score.is_finite()
            && rank_score > 0.0;
        let direct_evidence = ["vector", "fts", "symbolic"]
            .into_iter()
            .filter_map(|key| score?.get(key).and_then(serde_json::Value::as_f64))
            .chain(row.get("rerank_score").and_then(serde_json::Value::as_f64))
            .filter(|score| score.is_finite())
            .map(|score| score.clamp(0.0, 1.0))
            .reduce(f64::max);
        let Some(obj) = row.as_object_mut() else {
            continue;
        };
        let relevance = if graph_only && max_rank_score > f64::EPSILON {
            Some((rank_score / max_rank_score).clamp(0.0, 1.0))
        } else {
            direct_evidence.or_else(|| existing_relevance.map(|value| value.clamp(0.0, 1.0)))
        };
        if let Some(relevance) = relevance {
            obj.insert("relevance".into(), json!(round_score(relevance)));
        }
        if max_rank_score > f64::EPSILON {
            if let Some(score) = obj
                .get_mut("score")
                .and_then(serde_json::Value::as_object_mut)
            {
                score.insert(
                    "final".into(),
                    json!(round_score((rank_score / max_rank_score).clamp(0.0, 1.0))),
                );
            }
        }
    }
}

pub(crate) fn round_score(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

pub(crate) fn list_available_named_projects(home: &std::path::Path) -> Vec<String> {
    let projects_dir = home.join("projects");
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
            let has_db = [
                memcore::MEMORY_DB_FILENAME,
                memcore::LEGACY_MEMORY_DB_FILENAME,
            ]
            .into_iter()
            .any(|db_name| entry.path().join(db_name).exists());
            has_db.then_some(name)
        })
        .collect()
}

pub(crate) fn named_project_db_exists(server: &crate::MemoryServer, name: &str) -> bool {
    server
        .resolve_server_named_project_db_path(name)
        .map(|path| path.exists())
        .unwrap_or(false)
}

pub(crate) fn named_project_from_db_path_in_home(
    path: &std::path::Path,
    tachi_home: &std::path::Path,
) -> Option<String> {
    crate::path_utils::named_project_from_path_in_home(path, tachi_home).or_else(|| {
        crate::path_utils::plan_c_project_root_from_local_db(path)
            .and_then(|root| crate::path_utils::plan_c_dir_name_from_root(&root))
    })
}

/// Resolve the default workspace project library when the caller omitted `project`.
///
/// Honors an explicit pin (`TACHI_PROJECT`, populated from `.tachi/config.env`
/// at bootstrap) FIRST, so a repo whose memories live in an explicitly-named
/// library (e.g. `trading`) is the default target without passing `project=` on
/// every call. Falls back to the git-derived Plan C folder name
/// (`~/.tachi/projects/<name>/memory.db`) when no pin is set — which is what a
/// repo without an explicit library relies on.
pub(crate) fn resolve_workspace_named_project() -> Option<String> {
    if let Some(pinned) = explicit_workspace_project() {
        return Some(pinned);
    }
    let git_root = crate::utils::find_project_git_root()?;
    crate::path_utils::plan_c_dir_name_from_root(&git_root)
}

/// Explicit project pin from the `TACHI_PROJECT` env var. Returns `None` when
/// unset/blank/unsafe so resolution falls back to the git-derived name.
pub(crate) fn explicit_workspace_project() -> Option<String> {
    static CACHED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| normalize_pinned_project(&std::env::var("TACHI_PROJECT").ok()?))
        .clone()
}

/// Normalize a raw project pin. Mirrors the `project=` guard in
/// `resolve_named_project_db_path`: reject names that could escape the
/// `projects/` directory; return the trimmed name (downstream resolution
/// applies the same sanitization the `project=` param path does).
fn normalize_pinned_project(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
        || trimmed.starts_with('.')
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Precedence for the daemon's client project label that the stdio proxy
/// injects into project-aware tools (reads AND writes): an explicit
/// `TACHI_PROJECT` pin wins over the path-derived name, which wins over the
/// git-workspace fallback. Kept pure so `serve.rs` supplies the three
/// candidates and this stays unit-testable without touching process env.
/// Aligning the WRITE label with the pin here is what prevents a
/// read-from-pin / write-to-git-hash split.
pub(crate) fn client_project_precedence(
    pin: Option<String>,
    named_from_path: Option<String>,
    workspace_fallback: Option<String>,
) -> Option<String> {
    pin.or(named_from_path).or(workspace_fallback)
}

/// Resolve the named project DB to use when the caller omitted `project` but
/// requested project-scoped behavior.
///
/// READ-ONLY surfaces only (briefing/recall). Never use this for writes
/// (save/complete/eval): workspace detection depends on machine state and would
/// silently reroute writes away from the server's own bound stores.
pub(crate) fn resolve_effective_named_project(
    server: &crate::MemoryServer,
    explicit: Option<&str>,
) -> Option<String> {
    if let Some(name) = explicit.map(str::trim).filter(|name| !name.is_empty()) {
        if named_project_db_exists(server, name) {
            return Some(name.to_string());
        }
    }
    if server.has_project_db() {
        return server
            .project_db_path_buf()
            .and_then(|path| {
                named_project_from_db_path_in_home(path.as_path(), &server.tachi_home_dir())
            })
            .filter(|name| named_project_db_exists(server, name));
    }
    if let Some(name) =
        resolve_workspace_named_project().filter(|name| named_project_db_exists(server, name))
    {
        return Some(name);
    }
    None
}

/// Validate an explicit, caller-named `project=` request against the same
/// existence check the non-`project_only` search path already enforces
/// (`search_memory/rows.rs`'s `Project '<name>' not found (expected DB at …)`
/// branch). `project_only` searches skip that branch entirely (rows.rs falls
/// through to workspace-derived resolution instead of erroring), so any
/// `project_only` caller that resolves a caller-supplied project name must
/// call this FIRST — at the resolution seam, before the name is handed to
/// [`resolve_effective_named_project`] or a `project_only` search — so an
/// explicit nonexistent project fails loudly instead of silently answering
/// from the workspace/bound store (tachi#1575, child of #1539's invariant:
/// store/project resolution fails loud, no surface silently answers from the
/// wrong store).
///
/// Centralized here rather than re-derived per surface because
/// `project_only` is a targeted mode (today: `tachi_memory(action='briefing')`
/// via `facade_memory_ops::briefing_ops::handle_memory_briefing`, and
/// `tachi_task(action='briefing'|'doc_index')` via `copilot_ops::
/// feature_briefing::handlers::handle_tachi_feature_briefing` — both route
/// through here) whose whole point is to skip the ordinary error branch for
/// the *inferred* case (no project named — fall back to workspace) while
/// still needing to reject the *named-but-missing* case the same way the
/// ordinary path does. `name` must already be trimmed/non-empty (callers
/// gate on that before invoking this, mirroring
/// [`resolve_effective_named_project`]'s own `explicit` handling).
pub(crate) fn require_named_project_exists(
    server: &crate::MemoryServer,
    name: &str,
) -> Result<(), String> {
    if named_project_db_exists(server, name) {
        return Ok(());
    }
    Err(format!(
        "Project '{name}' not found (expected DB at {})",
        server
            .resolve_server_named_project_db_path(name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| e)
    ))
}

/// The daemon's currently-bound project store label, with NO workspace/CWD
/// inference (unlike [`resolve_effective_named_project`], which is
/// READ-ONLY-surfaces-only for exactly this reason). Safe to call from a
/// write path: it only reports what this daemon process actually has open
/// (`has_project_db` / `project_db_path_buf`), never what the caller's CWD
/// or git root *would* resolve to. `None` when no project DB is bound, or
/// when the bound path doesn't map to a recognized named-project convention.
///
/// Used by `save_memory`'s domain-store write-affinity gate (#1041 S1) to
/// tell whether an unqualified `scope=project` save is already landing in
/// the store its (explicit-or-classified) domain is registered to.
pub(crate) fn bound_project_label(server: &crate::MemoryServer) -> Option<String> {
    if !server.has_project_db() {
        return None;
    }
    server.project_db_path_buf().and_then(|path| {
        named_project_from_db_path_in_home(path.as_path(), &server.tachi_home_dir())
    })
}

pub(crate) fn infer_search_project(
    home: &std::path::Path,
    query: &str,
    domain: Option<&str>,
    config: &super::routing_config::RoutingConfig,
) -> Option<String> {
    infer_search_project_with(home, query, domain, config)
}

/// Config-injectable core. Domain-specific routing is supplied by
/// [`RoutingConfig`] rather than hardcoded here; the engine itself stays
/// domain-agnostic.
fn infer_search_project_with(
    home: &std::path::Path,
    query: &str,
    domain: Option<&str>,
    config: &super::routing_config::RoutingConfig,
) -> Option<String> {
    infer_search_project_with_available(
        query,
        domain,
        config,
        |name| crate::MemoryServer::resolve_named_project_db_path_in_home(name, home).is_ok(),
        list_available_named_projects(home),
    )
}

fn infer_search_project_with_available<F>(
    query: &str,
    domain: Option<&str>,
    config: &super::routing_config::RoutingConfig,
    project_exists: F,
    available_projects: Vec<String>,
) -> Option<String>
where
    F: Fn(&str) -> bool,
{
    if let Some(domain) = domain.map(str::trim).filter(|d| !d.is_empty()) {
        for route in &config.domain_routes {
            if route.domains.iter().any(|d| d.eq_ignore_ascii_case(domain))
                && project_exists(&route.project)
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
        if ticker_re.is_match(q) && project_exists(project) {
            return Some(project.to_string());
        }
    }

    for project in available_projects {
        if q_lower.contains(&project.to_lowercase()) {
            return Some(project);
        }
    }

    for route in &config.project_routes {
        // Check the cheap term match before the disk-I/O project-exists lookup.
        if route.terms.iter().any(|term| q_lower.contains(term)) && project_exists(&route.project) {
            return Some(route.project.clone());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    use crate::test_support::EnvRestore;

    // #485: explicit TACHI_PROJECT pin normalization. Pure function — no process
    // env is mutated, so these do not race with parallel tests that resolve the
    // workspace project.
    #[test]
    fn normalize_pinned_project_accepts_plain_name() {
        assert_eq!(
            normalize_pinned_project("trading"),
            Some("trading".to_string())
        );
    }

    #[test]
    fn normalize_pinned_project_trims_whitespace() {
        assert_eq!(
            normalize_pinned_project("  trading  "),
            Some("trading".to_string())
        );
    }

    #[test]
    fn normalize_pinned_project_rejects_blank() {
        assert_eq!(normalize_pinned_project(""), None);
        assert_eq!(normalize_pinned_project("   "), None);
    }

    #[test]
    fn normalize_pinned_project_rejects_path_escape() {
        assert_eq!(normalize_pinned_project("../evil"), None);
        assert_eq!(normalize_pinned_project("a/b"), None);
        assert_eq!(normalize_pinned_project("a\\b"), None);
        assert_eq!(normalize_pinned_project(".hidden"), None);
    }

    // #485 review fix: the pin must win over the path-derived (git-hash) name for
    // the daemon's client project label, so proxied WRITES align with reads.
    #[test]
    fn client_project_precedence_prefers_pin_then_path_then_workspace() {
        // Pin present -> pin wins over the git-hash path name and workspace.
        assert_eq!(
            client_project_precedence(
                Some("trading".to_string()),
                Some("Quant_Analyzer_2026-b4773587".to_string()),
                Some("ws".to_string()),
            ),
            Some("trading".to_string())
        );
        // No pin -> path-derived name wins (prior behavior, unchanged).
        assert_eq!(
            client_project_precedence(
                None,
                Some("Quant_Analyzer_2026-b4773587".to_string()),
                Some("ws".to_string()),
            ),
            Some("Quant_Analyzer_2026-b4773587".to_string())
        );
        // No pin, no path name -> workspace fallback.
        assert_eq!(
            client_project_precedence(None, None, Some("ws".to_string())),
            Some("ws".to_string())
        );
        assert_eq!(client_project_precedence(None, None, None), None);
    }

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
            scored_count: 0,
            last_access: None,
            last_use_at: None,
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
    fn resolve_effective_named_project_does_not_pick_single_available_project() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp_home = tempfile::tempdir().expect("temp tachi home");
        let _home = EnvRestore::set_path("TACHI_HOME", temp_home.path());
        let lonely_project = temp_home.path().join("projects").join("lonely");
        std::fs::create_dir_all(&lonely_project).expect("project dir");
        std::fs::write(lonely_project.join("memory.db"), b"").expect("project db marker");

        let db_path = temp_home.path().join("global.sqlite");
        let server = crate::MemoryServer::new(PathBuf::from(&db_path), None).expect("server");

        assert_eq!(
            list_available_named_projects(temp_home.path()),
            vec!["lonely".to_string()]
        );
        assert_eq!(resolve_effective_named_project(&server, None), None);
    }

    #[test]
    fn infer_search_project_defaults_do_not_route_domain_terms() {
        let config = crate::memory_search_ops::routing_config::RoutingConfig::default();
        assert_eq!(
            infer_search_project_with_available(
                "123456 stop rule",
                None,
                &config,
                |_| true,
                Vec::new()
            ),
            None
        );
        assert_eq!(
            infer_search_project_with_available(
                "portfolio risk",
                Some("domain_pack"),
                &config,
                |_| true,
                Vec::new()
            ),
            None
        );
    }

    #[test]
    fn explicit_routing_config_can_route_domain_pack_terms() {
        let config = crate::memory_search_ops::routing_config::RoutingConfig {
            domain_routes: vec![crate::memory_search_ops::routing_config::DomainRoute {
                project: "domain_pack".into(),
                domains: vec!["domain_pack".into()],
            }],
            ticker_route_project: Some("domain_pack".into()),
            project_routes: vec![crate::memory_search_ops::routing_config::ProjectRoute {
                project: "domain_pack".into(),
                terms: vec!["domain-specific-term".into()],
            }],
            foreign_domain_word_terms: Vec::new(),
            foreign_domain_substring_terms: Vec::new(),
            foreign_domains: Vec::new(),
            foreign_path_prefixes: Vec::new(),
        };
        let exists = |name: &str| name == "domain_pack";
        assert_eq!(
            infer_search_project_with_available("123456 notes", None, &config, exists, Vec::new())
                .as_deref(),
            Some("domain_pack")
        );
        assert_eq!(
            infer_search_project_with_available(
                "portfolio risk",
                Some("domain_pack"),
                &config,
                exists,
                Vec::new()
            )
            .as_deref(),
            Some("domain_pack")
        );
        assert_eq!(
            infer_search_project_with_available(
                "domain-specific-term recall",
                None,
                &config,
                exists,
                Vec::new()
            )
            .as_deref(),
            Some("domain_pack")
        );
    }

    #[test]
    fn normalize_search_relevance_scales_top_hit_to_one() {
        let mut results = vec![
            (
                memcore::SearchResult {
                    entry: test_entry("a", "alpha"),
                    score: memcore::HybridScore {
                        vector: 0.2,
                        fts: 0.1,
                        symbolic: 0.0,
                        decay: 0.0,
                        final_score: 0.03,
                    },
                    graph_injected: false,
                    graph_provenance: None,
                },
                DbScope::Project,
            ),
            (
                memcore::SearchResult {
                    entry: test_entry("b", "beta"),
                    score: memcore::HybridScore {
                        vector: 0.1,
                        fts: 0.05,
                        symbolic: 0.0,
                        decay: 0.0,
                        final_score: 0.015,
                    },
                    graph_injected: false,
                    graph_provenance: None,
                },
                DbScope::Project,
            ),
        ];
        normalize_search_relevance(&mut results);
        assert!((results[0].0.score.final_score - 1.0).abs() < f64::EPSILON);
        assert!((results[1].0.score.final_score - 0.5).abs() < 0.01);
    }

    #[test]
    fn normalize_json_relevance_uses_absolute_evidence_and_preserves_relative_final() {
        let mut rows = vec![
            json!({
                "id": "weak-vector",
                "relevance": 1.0,
                "score": {"vector": 0.36, "fts": 0.0, "symbolic": 0.0, "final": 1.0}
            }),
            json!({
                "id": "reranked",
                "relevance": 1.167,
                "rerank_score": 0.92,
                "score": {"vector": 0.20, "fts": 0.10, "symbolic": 0.30, "final": 1.167}
            }),
            json!({
                "id": "graph-only",
                "relevance": 0.0,
                "score": {"vector": 0.0, "fts": 0.0, "symbolic": 0.0, "decay": 0.0, "final": 0.5}
            }),
            json!({"id": "legacy-scoreless", "relevance": 0.42}),
            json!({"id": "l0-rule"}),
        ];

        normalize_json_relevance(&mut rows);

        assert_eq!(rows[0]["relevance"], json!(0.36));
        assert_eq!(rows[0]["score"]["final"], json!(0.857));
        assert_eq!(rows[1]["relevance"], json!(0.92));
        assert_eq!(
            rows[1]["score"]["final"],
            json!(1.0),
            "rerank blend final remains the response-relative ranking key"
        );
        assert_eq!(
            rows[2]["relevance"],
            json!(0.428),
            "graph-only rows need response-relative relevance instead of a false zero"
        );
        assert_eq!(rows[2]["score"]["final"], json!(0.428));
        assert_eq!(rows[3]["relevance"], json!(0.42));
        assert!(rows[4].get("relevance").is_none());
    }
}
