//! Cross-library ranking preference for project-bound sessions (tachi#899 / #896 Phase 2).
//!
//! When hybrid search merges **global + project** rows, pure `final_score` sort
//! lets older global roadmap/review noise bury a recent project decision
//! (ops-audit multi-DB case: expected project id rank 5 pre-fix).
//!
//! Policy (provisional knobs, not flat magic in call sites):
//! - Apply only when both scopes are present in the candidate set.
//! - Skip wiki-scoped queries (`path_prefix` under `/wiki`) so deliberate
//!   global/wiki research is not demoted.
//! - Multiply project-row `final_score` by `1 + boost` (default calibrated on
//!   the #897 multi-DB fixture), then re-sort with Project-before-Global
//!   tie-break.
//!
//! Escape hatch: `TACHI_RECALL_CROSS_LIBRARY_PROJECT_BOOST=0` disables *all*
//! project preference — both the score multiplier and the Project-before-Global
//! tie-break (tachi#911 follow-up to #902: `0` previously disabled only the
//! multiplier, leaving equal-score rows still ranked project-first). At
//! `boost <= 0.0` this function is a no-op and returns `false`, so the caller
//! (`rows.rs`) falls back to its plain `final_score` sort, same as the
//! single-scope / wiki-scoped / empty-results cases.

use crate::tool_params::SearchMemoryParams;
use crate::DbScope;
use memory_core::SearchResult;

/// Default additive boost: project `final_score *= 1.0 + boost`.
///
/// Calibrated against `ops_audit_cross_library_dilution_*` (#897 fixture):
/// pre-fix project decision sat at rank ~5 under lexical multi-DB merge.
/// Raise only with ops-audit red→green evidence; never lower silently.
pub(crate) const DEFAULT_CROSS_LIBRARY_PROJECT_BOOST: f64 = 0.85;

/// Read provisional boost from env, falling back to the calibrated default.
///
/// Cached via `OnceLock` so search hot paths do not re-parse env on every call
/// (Gemini #902 review). Tests that mutate the env must re-run in a fresh
/// process or accept the first-read value for the process lifetime.
pub(crate) fn cross_library_project_boost() -> f64 {
    static BOOST: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *BOOST.get_or_init(|| {
        std::env::var("TACHI_RECALL_CROSS_LIBRARY_PROJECT_BOOST")
            .ok()
            .and_then(|raw| raw.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(DEFAULT_CROSS_LIBRARY_PROJECT_BOOST)
            .min(10.0)
    })
}

fn wiki_scoped(params: &SearchMemoryParams) -> bool {
    params
        .path_prefix
        .as_deref()
        .map(str::trim)
        .is_some_and(|prefix| prefix == "/wiki" || prefix.starts_with("/wiki/"))
}

/// Apply project-row score preference + stable Project-before-Global tie-break.
///
/// Returns `true` when preference sorting was applied (multi-scope, non-wiki).
/// Returns `false` when the caller must fall back to a plain score sort
/// (empty, single-scope, or wiki-scoped). Centralizing this avoids path_prefix
/// trim mismatches between this module and `rows.rs` (Gemini #902).
pub(crate) fn apply_cross_library_project_preference(
    results: &mut [(SearchResult, DbScope)],
    params: &SearchMemoryParams,
) -> bool {
    apply_cross_library_project_preference_with_boost(
        results,
        params,
        cross_library_project_boost(),
    )
}

/// Boost-parameterized core of [`apply_cross_library_project_preference`],
/// split out so the `boost <= 0.0` disable path (tachi#911 follow-up to
/// #902) is directly unit-testable: `cross_library_project_boost()` caches
/// its env read in a process-wide `OnceLock`, so a test cannot exercise both
/// the default-boost and boost-disabled behaviors in the same test binary by
/// mutating the env var — passing `boost` explicitly sidesteps that.
fn apply_cross_library_project_preference_with_boost(
    results: &mut [(SearchResult, DbScope)],
    params: &SearchMemoryParams,
    boost: f64,
) -> bool {
    if results.is_empty() || wiki_scoped(params) {
        return false;
    }
    let mut has_project = false;
    let mut has_global = false;
    for (_, scope) in results.iter() {
        match scope {
            DbScope::Project => has_project = true,
            DbScope::Global => has_global = true,
        }
        if has_project && has_global {
            break;
        }
    }
    if !has_project || !has_global {
        return false;
    }

    if boost <= 0.0 {
        // tachi#911 follow-up to #902: `boost == 0` means "disable ALL
        // project preference," not just the score multiplier — skip the
        // Project-before-Global tie-break too and let the caller's plain
        // final_score sort run instead (see module doc + rows.rs fallback).
        return false;
    }

    let factor = 1.0 + boost;
    for (result, scope) in results.iter_mut() {
        if matches!(scope, DbScope::Project) {
            let score = result.score.final_score;
            if score.is_finite() && score > 0.0 {
                result.score.final_score = score * factor;
            }
        }
    }

    results.sort_by(|a, b| {
        b.0.score
            .final_score
            .partial_cmp(&a.0.score.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| scope_preference_key(a.1).cmp(&scope_preference_key(b.1)))
            .then_with(|| b.0.entry.timestamp.cmp(&a.0.entry.timestamp))
            .then_with(|| a.0.entry.id.cmp(&b.0.entry.id))
    });
    true
}

/// Lower key sorts earlier on ties after score (Project preferred over Global).
fn scope_preference_key(scope: DbScope) -> u8 {
    match scope {
        DbScope::Project => 0,
        DbScope::Global => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::{HybridScore, MemoryEntry};

    fn row(id: &str, score: f64, scope: DbScope) -> (SearchResult, DbScope) {
        let entry = MemoryEntry {
            id: id.to_string(),
            path: "/notes/x".to_string(),
            summary: id.to_string(),
            text: id.to_string(),
            importance: 0.7,
            timestamp: "2026-06-01T00:00:00+00:00".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        (
            SearchResult {
                entry,
                score: HybridScore {
                    vector: 0.0,
                    fts: 0.0,
                    symbolic: 0.0,
                    decay: 0.0,
                    final_score: score,
                },
            },
            scope,
        )
    }

    fn bare_params() -> SearchMemoryParams {
        SearchMemoryParams {
            query: "open issue priority project decision".to_string(),
            query_vec: None,
            top_k: 10,
            path_prefix: None,
            include_training: false,
            include_archived: false,
            candidates_per_channel: 20,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            context_symbols: Vec::new(),
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        }
    }

    #[test]
    fn project_row_outranks_higher_global_after_preference() {
        let mut results = vec![
            row("global-heavy", 1.0, DbScope::Global),
            row("project-decision", 0.6, DbScope::Project),
        ];
        // With default boost 0.85: 0.6 * 1.85 = 1.11 > 1.0
        apply_cross_library_project_preference(&mut results, &bare_params());
        assert_eq!(results[0].0.entry.id, "project-decision");
        assert_eq!(results[1].0.entry.id, "global-heavy");
    }

    #[test]
    fn wiki_path_prefix_skips_preference() {
        let mut results = vec![
            row("global-wiki", 1.0, DbScope::Global),
            row("project-wiki", 0.6, DbScope::Project),
        ];
        let mut params = bare_params();
        params.path_prefix = Some("/wiki".to_string());
        assert!(!apply_cross_library_project_preference(
            &mut results, &params
        ));
        // Unchanged order: global still first
        assert_eq!(results[0].0.entry.id, "global-wiki");
    }

    #[test]
    fn wiki_path_prefix_trims_whitespace_before_skip() {
        // Discrimination: leading/trailing spaces must still count as wiki-scoped
        // so rows.rs fallback sort path is taken (Gemini #902 path_prefix mismatch).
        let mut results = vec![
            row("global-wiki", 1.0, DbScope::Global),
            row("project-wiki", 0.6, DbScope::Project),
        ];
        let mut params = bare_params();
        params.path_prefix = Some("  /wiki/  ".to_string());
        assert!(!apply_cross_library_project_preference(
            &mut results, &params
        ));
        assert_eq!(results[0].0.entry.id, "global-wiki");
    }

    #[test]
    fn single_scope_is_noop() {
        let mut results = vec![
            row("p1", 0.5, DbScope::Project),
            row("p2", 0.9, DbScope::Project),
        ];
        assert!(!apply_cross_library_project_preference(
            &mut results,
            &bare_params()
        ));
        assert_eq!(results[0].0.entry.id, "p1"); // order unchanged (no re-sort without both scopes)
        assert_eq!(results[0].0.score.final_score, 0.5);
    }

    #[test]
    fn multi_scope_returns_true_when_preference_sorted() {
        let mut results = vec![
            row("global-heavy", 1.0, DbScope::Global),
            row("project-decision", 0.6, DbScope::Project),
        ];
        assert!(apply_cross_library_project_preference(
            &mut results,
            &bare_params()
        ));
        assert_eq!(results[0].0.entry.id, "project-decision");
    }

    // tachi#911 follow-up to #902: `TACHI_RECALL_CROSS_LIBRARY_PROJECT_BOOST=0`
    // must disable the Project-before-Global tie-break, not just the score
    // multiplier. `cross_library_project_boost()` caches its env read in a
    // process-wide `OnceLock` shared with every other test in this binary, so
    // these tests exercise the boost-parameterized core directly instead of
    // mutating the env var (which would race/leak across the other tests
    // above that rely on the default 0.85 boost).

    #[test]
    fn zero_boost_disables_multiplier_and_tie_break() {
        let mut results = vec![
            row("global-equal", 0.5, DbScope::Global),
            row("project-equal", 0.5, DbScope::Project),
        ];
        let applied =
            apply_cross_library_project_preference_with_boost(&mut results, &bare_params(), 0.0);
        assert!(!applied, "boost=0 must report no preference applied");
        // No mutation: caller (rows.rs) is expected to run its own plain
        // final_score sort when this returns false, so this function must
        // leave both scores and order untouched.
        assert_eq!(results[0].0.entry.id, "global-equal");
        assert_eq!(results[0].0.score.final_score, 0.5);
        assert_eq!(results[1].0.entry.id, "project-equal");
        assert_eq!(results[1].0.score.final_score, 0.5);
    }

    #[test]
    fn negative_boost_also_disables_preference() {
        // `cross_library_project_boost()` already floors env-provided negative
        // values to the calibrated default via `.filter(|v| *v >= 0.0)`, but
        // the boost-parameterized core is defensive against any `boost <= 0.0`
        // reaching it directly.
        let mut results = vec![
            row("global-heavy", 1.0, DbScope::Global),
            row("project-decision", 0.6, DbScope::Project),
        ];
        let applied =
            apply_cross_library_project_preference_with_boost(&mut results, &bare_params(), -1.0);
        assert!(!applied);
        assert_eq!(results[0].0.entry.id, "global-heavy");
        assert_eq!(results[0].0.score.final_score, 1.0);
        assert_eq!(results[1].0.entry.id, "project-decision");
        assert_eq!(results[1].0.score.final_score, 0.6);
    }
}
