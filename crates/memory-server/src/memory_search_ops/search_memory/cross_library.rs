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
//! Escape hatch: `TACHI_RECALL_CROSS_LIBRARY_PROJECT_BOOST=0` disables.

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
pub(crate) fn cross_library_project_boost() -> f64 {
    std::env::var("TACHI_RECALL_CROSS_LIBRARY_PROJECT_BOOST")
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(DEFAULT_CROSS_LIBRARY_PROJECT_BOOST)
        .min(10.0)
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
/// No-op when boost is 0, wiki-scoped, or the candidate set is single-scope.
pub(crate) fn apply_cross_library_project_preference(
    results: &mut [(SearchResult, DbScope)],
    params: &SearchMemoryParams,
) {
    if results.is_empty() || wiki_scoped(params) {
        return;
    }
    let boost = cross_library_project_boost();
    let has_project = results
        .iter()
        .any(|(_, scope)| matches!(scope, DbScope::Project));
    let has_global = results
        .iter()
        .any(|(_, scope)| matches!(scope, DbScope::Global));
    if !has_project || !has_global {
        return;
    }

    if boost > 0.0 {
        let factor = 1.0 + boost;
        for (result, scope) in results.iter_mut() {
            if matches!(scope, DbScope::Project) {
                let score = result.score.final_score;
                if score.is_finite() && score > 0.0 {
                    result.score.final_score = score * factor;
                }
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
        apply_cross_library_project_preference(&mut results, &params);
        // Unchanged order: global still first
        assert_eq!(results[0].0.entry.id, "global-wiki");
    }

    #[test]
    fn single_scope_is_noop() {
        let mut results = vec![
            row("p1", 0.5, DbScope::Project),
            row("p2", 0.9, DbScope::Project),
        ];
        apply_cross_library_project_preference(&mut results, &bare_params());
        assert_eq!(results[0].0.entry.id, "p1"); // order unchanged (no re-sort without both scopes)
        assert_eq!(results[0].0.score.final_score, 0.5);
    }
}
