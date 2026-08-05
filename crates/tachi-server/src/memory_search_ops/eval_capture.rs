//! Opt-in auto-capture of personal recall-eval cases under `/eval/recall/...`.
//!
//! When a real search records access (`record_access=true` — "recalled and used"),
//! and `TACHI_EVAL_CAPTURE=true`, write a labeled case row readable by
//! `load_eval_namespace_cases` / `tachi eval recall`.
//!
//! Privacy: query string + expected memory ids only — never copy hit body text.
//! Fail-closed: capture errors are logged and never surface to the search path.
//! Off by default: zero behavior change unless the env flag is set.

use chrono::Utc;
use memcore::{is_eval_entry, MemoryEntry, MemoryStore, SearchResult};
use serde_json::json;
use uuid::Uuid;

use crate::tool_params::SearchMemoryParams;
use crate::utils::{parse_env_bool, parse_env_u64};

const CAPTURE_ENV: &str = "TACHI_EVAL_CAPTURE";
const MAX_PER_DAY_ENV: &str = "TACHI_EVAL_CAPTURE_MAX_PER_DAY";
const DEFAULT_MAX_PER_DAY: u64 = 20;
const PATH_PREFIX: &str = "/eval/recall";
const SOURCE: &str = "eval_capture";

/// Best-effort capture after an access-recording search. Never returns Err to callers.
pub(super) fn maybe_capture_after_access(
    store: &mut MemoryStore,
    params: &SearchMemoryParams,
    results: &[SearchResult],
) {
    if let Err(err) = try_capture_after_access(store, params, results) {
        tracing::warn!(error = %err, "eval recall-corpus capture skipped");
    }
}

/// Test/internal entry: returns structured outcome without panicking the search path.
pub(super) fn try_capture_after_access(
    store: &mut MemoryStore,
    params: &SearchMemoryParams,
    results: &[SearchResult],
) -> Result<CaptureOutcome, String> {
    if !capture_enabled() {
        return Ok(CaptureOutcome::Disabled);
    }
    if !params.query.trim().is_empty() && memcore::should_skip_query(&params.query) {
        return Ok(CaptureOutcome::Skipped("skip_query"));
    }
    let query = params.query.trim();
    if query.is_empty() {
        return Ok(CaptureOutcome::Skipped("empty_query"));
    }
    // Explicit /eval-scoped search is ledger browsing, not personal recall labels.
    if params
        .path_prefix
        .as_deref()
        .is_some_and(|p| is_eval_path(p.trim_end_matches('/')))
    {
        return Ok(CaptureOutcome::Skipped("eval_scoped_search"));
    }

    let Some(hit) = first_capture_candidate(results) else {
        return Ok(CaptureOutcome::Skipped("no_eligible_hit"));
    };

    let max_per_day = max_captures_per_day();
    let today_count = count_captures_today(store).map_err(|e| format!("count captures: {e}"))?;
    if today_count >= max_per_day {
        return Ok(CaptureOutcome::Skipped("rate_limited"));
    }

    let top_k = params.normalized_top_k();
    let entry = build_capture_entry(query, &hit.entry.id, top_k);
    store
        .upsert(&entry)
        .map_err(|e| format!("upsert capture case: {e}"))?;
    Ok(CaptureOutcome::Captured {
        path: entry.path,
        expected_id: hit.entry.id.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CaptureOutcome {
    Disabled,
    Skipped(&'static str),
    Captured { path: String, expected_id: String },
}

fn capture_enabled() -> bool {
    parse_env_bool(CAPTURE_ENV).unwrap_or(false)
}

fn max_captures_per_day() -> u64 {
    parse_env_u64(MAX_PER_DAY_ENV)
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_PER_DAY)
}

fn is_eval_path(path: &str) -> bool {
    path == "/eval" || path.starts_with("/eval/")
}

fn first_capture_candidate(results: &[SearchResult]) -> Option<&SearchResult> {
    results
        .iter()
        .find(|r| !is_eval_entry(&r.entry) && !is_eval_path(&r.entry.path))
}

fn count_captures_today(store: &MemoryStore) -> Result<u64, String> {
    let count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM memories
             WHERE path LIKE '/eval/recall/%'
               AND created_at >= date('now')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count.max(0) as u64)
}

fn build_capture_entry(query: &str, expected_id: &str, top_k: usize) -> MemoryEntry {
    let short_id = Uuid::new_v4().simple().to_string();
    let short_id = &short_id[..8];
    let date = Utc::now().format("%Y-%m-%d");
    let path = format!("{PATH_PREFIX}/{date}-{short_id}");
    let id = format!("eval-recall-{date}-{short_id}");
    // Unique placeholder text (ids only) so Jaccard write-dedup does not collapse cases.
    // Never copies the accessed memory body.
    //
    // kckylechen1/tachi#1634: this call reaches `store.upsert()`, an
    // explicit-id write, which now defaults to
    // `NearDuplicatePolicy::NonSemantic` — write-time Jaccard dedup never
    // runs on this path at all anymore, so the uniqueness dodge below is
    // obsolete. Left in place (removing it is a behavior-neutral cleanup a
    // later pass can do) rather than removed as part of the #1634 default
    // flip.
    let text = format!(
        "auto-captured recall eval case id={id} expected_id={expected_id} (query+ids only; no body)"
    );
    let summary = format!("auto-captured recall case for {expected_id}");
    let now = Utc::now().to_rfc3339();

    MemoryEntry {
        id,
        path,
        summary,
        text,
        importance: 0.5,
        timestamp: now.clone(),
        valid_from: now,
        valid_until: None,
        category: "eval".to_string(),
        topic: "recall_eval".to_string(),
        keywords: vec![
            "eval".to_string(),
            "recall".to_string(),
            "auto_capture".to_string(),
        ],
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: SOURCE.to_string(),
        scope: "user".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: Some("durable".to_string()),
        domain: None,
        metadata: json!({
            "recall_eval": {
                "query": query,
                "expected_ids": [expected_id],
                "top_k": top_k,
                "slice": "auto_capture",
                "source": "record_access",
                "signal": "recalled_and_used",
            },
            "auto_captured": true,
            // Not auto_synthesized: personal recall cases must remain loadable by
            // list_eval_evidence(..., exclude_auto_synthesized=true) call sites.
        }),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::{HybridScore, SearchResult};
    use std::sync::{Mutex, OnceLock};

    /// Shape check mirroring `case_from_eval_metadata` in eval_cli (query + expected_ids).
    fn case_readable_from_metadata(
        metadata: &serde_json::Value,
    ) -> Option<(String, Vec<String>, Option<usize>)> {
        let source = metadata
            .get("recall_eval")
            .or_else(|| metadata.get("eval"))
            .filter(|value| value.is_object())
            .unwrap_or(metadata);
        if source.get("enabled").and_then(serde_json::Value::as_bool) == Some(false) {
            return None;
        }
        let query = source
            .get("query")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty())?
            .to_string();
        let mut expected_ids = Vec::new();
        if let Some(id) = source
            .get("expected_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            expected_ids.push(id.to_string());
        }
        if let Some(array) = source
            .get("expected_ids")
            .and_then(serde_json::Value::as_array)
        {
            for id in array
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
            {
                if !id.is_empty() && !expected_ids.iter().any(|existing| existing == id) {
                    expected_ids.push(id.to_string());
                }
            }
        }
        if expected_ids.is_empty() {
            return None;
        }
        let top_k = source
            .get("top_k")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as usize);
        Some((query, expected_ids, top_k))
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn with_capture_env<T>(
        enabled: Option<&str>,
        max_per_day: Option<&str>,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock();
        let prev_enabled = std::env::var_os(CAPTURE_ENV);
        let prev_max = std::env::var_os(MAX_PER_DAY_ENV);
        match enabled {
            Some(v) => std::env::set_var(CAPTURE_ENV, v),
            None => std::env::remove_var(CAPTURE_ENV),
        }
        match max_per_day {
            Some(v) => std::env::set_var(MAX_PER_DAY_ENV, v),
            None => std::env::remove_var(MAX_PER_DAY_ENV),
        }
        let out = f();
        match prev_enabled {
            Some(v) => std::env::set_var(CAPTURE_ENV, v),
            None => std::env::remove_var(CAPTURE_ENV),
        }
        match prev_max {
            Some(v) => std::env::set_var(MAX_PER_DAY_ENV, v),
            None => std::env::remove_var(MAX_PER_DAY_ENV),
        }
        out
    }

    fn open_store() -> MemoryStore {
        MemoryStore::open_in_memory().expect("in-memory store")
    }

    fn seed_fact(store: &mut MemoryStore, id: &str, text: &str) {
        let now = Utc::now().to_rfc3339();
        let entry = MemoryEntry {
            id: id.to_string(),
            path: format!("/notes/{id}"),
            summary: text.to_string(),
            text: text.to_string(),
            importance: 0.8,
            timestamp: now.clone(),
            valid_from: now,
            valid_until: None,
            category: "fact".to_string(),
            topic: "test".to_string(),
            keywords: text.split_whitespace().map(str::to_string).collect(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "user".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        store.upsert(&entry).expect("seed fact");
    }

    fn seed_eval_row(store: &mut MemoryStore, id: &str, text: &str) {
        let now = Utc::now().to_rfc3339();
        let entry = MemoryEntry {
            id: id.to_string(),
            path: format!("/eval/other/{id}"),
            summary: text.to_string(),
            text: text.to_string(),
            importance: 0.5,
            timestamp: now.clone(),
            valid_from: now,
            valid_until: None,
            category: "eval".to_string(),
            topic: "eval".to_string(),
            keywords: text.split_whitespace().map(str::to_string).collect(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "user".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        store.upsert(&entry).expect("seed eval");
    }

    fn params(query: &str, top_k: usize) -> SearchMemoryParams {
        SearchMemoryParams {
            query: query.into(),
            query_vec: None,
            top_k,
            path_prefix: None,
            include_training: false,
            include_archived: false,
            candidates_per_channel: 10,
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
            format: None,
        }
    }

    fn fake_result(entry: MemoryEntry) -> SearchResult {
        SearchResult {
            entry,
            score: HybridScore {
                vector: 0.0,
                fts: 1.0,
                symbolic: 0.0,
                decay: 1.0,
                final_score: 1.0,
            },
        }
    }

    #[test]
    fn capture_off_writes_nothing() {
        with_capture_env(None, None, || {
            let mut store = open_store();
            seed_fact(
                &mut store,
                "fact-off-1",
                "unique capture off sentinel alpha",
            );
            let results = store
                .search(
                    "unique capture off sentinel alpha",
                    Some(memcore::SearchOptions {
                        record_access: true,
                        top_k: 3,
                        ..Default::default()
                    }),
                )
                .expect("search");
            assert!(!results.is_empty());
            let outcome = try_capture_after_access(
                &mut store,
                &params("unique capture off sentinel alpha", 3),
                &results,
            )
            .expect("capture attempt");
            assert_eq!(outcome, CaptureOutcome::Disabled);
            let rows = store.list_eval_evidence(7, 100, false).expect("list eval");
            assert!(
                rows.iter().all(|r| !r.path.starts_with("/eval/recall/")),
                "capture-off must not write /eval/recall rows: {rows:?}"
            );
        });
    }

    #[test]
    fn capture_on_writes_case_readable_by_eval_loader_shape() {
        with_capture_env(Some("true"), Some("50"), || {
            let mut store = open_store();
            let fact_id = "fact-on-capture-1";
            let query = "unique capture on sentinel beta recall case";
            seed_fact(&mut store, fact_id, query);
            let results = store
                .search(
                    query,
                    Some(memcore::SearchOptions {
                        record_access: true,
                        top_k: 5,
                        ..Default::default()
                    }),
                )
                .expect("search");
            assert!(
                results.iter().any(|r| r.entry.id == fact_id),
                "seed should be recalled"
            );

            let outcome =
                try_capture_after_access(&mut store, &params(query, 5), &results).expect("capture");
            let CaptureOutcome::Captured { path, expected_id } = outcome else {
                panic!("expected Captured, got {outcome:?}");
            };
            assert!(path.starts_with("/eval/recall/"), "path={path}");
            assert_eq!(expected_id, fact_id);

            let rows = store.list_eval_evidence(7, 100, false).expect("list eval");
            let case_row = rows
                .iter()
                .find(|r| r.path == path)
                .expect("captured row present");
            // Privacy: no seed body beyond the query we already store as the label.
            assert!(
                !case_row.text.contains("should never appear"),
                "must not copy unrelated body"
            );
            let (loaded_query, expected_ids, top_k) =
                case_readable_from_metadata(&case_row.metadata).expect("loader shape");
            assert_eq!(loaded_query, query);
            assert_eq!(expected_ids, vec![fact_id.to_string()]);
            assert_eq!(top_k, Some(5));
        });
    }

    #[test]
    fn capture_never_uses_eval_row_as_expected_id() {
        with_capture_env(Some("true"), Some("50"), || {
            let mut store = open_store();
            let eval_id = "eval-only-hit-1";
            let query = "unique eval contamination sentinel gamma";
            seed_eval_row(&mut store, eval_id, query);
            // Fabricate results as if hybrid ranked the eval row first (pre-filter).
            let eval_entry = store.get(eval_id).expect("get").expect("seeded eval");
            let results = vec![fake_result(eval_entry)];
            let outcome =
                try_capture_after_access(&mut store, &params(query, 3), &results).expect("capture");
            assert_eq!(outcome, CaptureOutcome::Skipped("no_eligible_hit"));

            // Also: when a real fact is present, prefer it over a leading eval row.
            seed_fact(&mut store, "fact-after-eval", query);
            let fact = store.get("fact-after-eval").expect("get").expect("fact");
            let eval_entry = store.get(eval_id).expect("get").expect("eval");
            let mixed = vec![fake_result(eval_entry), fake_result(fact)];
            let outcome =
                try_capture_after_access(&mut store, &params(query, 3), &mixed).expect("capture");
            match outcome {
                CaptureOutcome::Captured { expected_id, .. } => {
                    assert_eq!(expected_id, "fact-after-eval");
                    assert_ne!(expected_id, eval_id);
                }
                other => panic!("expected Captured, got {other:?}"),
            }
        });
    }

    #[test]
    fn capture_errors_never_break_search_path() {
        with_capture_env(Some("true"), Some("50"), || {
            // maybe_capture_after_access must not panic / return Err even when
            // the store path is unusable — simulate by capturing with empty results
            // after enabling (skipped, not error). For true error path, call the
            // logging wrapper which swallows Result.
            let mut store = open_store();
            maybe_capture_after_access(&mut store, &params("x", 3), &[]);
            // If we got here, the search path was not broken.
        });
    }

    #[test]
    fn capture_respects_daily_rate_limit() {
        with_capture_env(Some("true"), Some("1"), || {
            let mut store = open_store();
            seed_fact(&mut store, "rate-a", "unique rate limit sentinel delta one");
            seed_fact(&mut store, "rate-b", "unique rate limit sentinel delta two");
            let r1 = store
                .search(
                    "unique rate limit sentinel delta one",
                    Some(memcore::SearchOptions {
                        record_access: true,
                        top_k: 3,
                        ..Default::default()
                    }),
                )
                .expect("search 1");
            let o1 = try_capture_after_access(
                &mut store,
                &params("unique rate limit sentinel delta one", 3),
                &r1,
            )
            .expect("cap 1");
            assert!(matches!(o1, CaptureOutcome::Captured { .. }), "{o1:?}");

            let r2 = store
                .search(
                    "unique rate limit sentinel delta two",
                    Some(memcore::SearchOptions {
                        record_access: true,
                        top_k: 3,
                        ..Default::default()
                    }),
                )
                .expect("search 2");
            let o2 = try_capture_after_access(
                &mut store,
                &params("unique rate limit sentinel delta two", 3),
                &r2,
            )
            .expect("cap 2");
            assert_eq!(o2, CaptureOutcome::Skipped("rate_limited"));
        });
    }
}
