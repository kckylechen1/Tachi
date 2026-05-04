// search.rs — Hybrid search orchestration
//
// Runs Vec + FTS + Symbolic channels, merges scores in Rust, returns top K.
// Optional graph expansion augments results with memory-graph neighbors.
// This is the hottest path: all computation stays in Rust, zero JS/Python overhead.

use chrono::NaiveDate;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use crate::{
    db::{
        fetch_by_ids, get_access_times, get_superseded_ids, graph_expand, record_access,
        search_fts, search_vec,
    },
    error::MemoryError,
    scorer::{cosine_similarity, hybrid_score, symbolic_score, HybridWeights},
    types::{MemoryEntry, SearchResult},
};

/// Options for a hybrid search query.
pub struct SearchOptions {
    /// Number of candidates to pull from each channel before merging.
    pub candidates_per_channel: usize,
    /// Final top-K to return after scoring.
    pub top_k: usize,
    /// Scoring weights.
    pub weights: HybridWeights,
    /// Optionally restrict results to a path prefix (e.g. "/openclaw")
    pub path_prefix: Option<String>,
    /// Optionally restrict results to a specific domain (e.g. "finance")
    pub domain: Option<String>,
    /// Pre-computed query embedding; if None, skip vector channel.
    pub query_vec: Option<Vec<f32>>,
    /// Whether the sqlite-vec extension is available for vector search.
    pub vec_available: bool,
    /// Whether to bump access_count after retrieval (disable in bulk/bench mode).
    pub record_access: bool,
    /// Whether to include archived entries in query results.
    pub include_archived: bool,
    /// MMR diversity threshold: cosine similarity > threshold → defer to end.
    /// Set to None to disable MMR. Default: Some(0.85).
    pub mmr_threshold: Option<f64>,
    /// Graph expand hops: 0 = disabled, 1-2 = expand through memory_edges after ranking.
    /// Expanded entries are appended after the ranked results (lower priority).
    pub graph_expand_hops: u32,
    /// Optional filter for graph edges: "causes", "follows", "related_to", etc.
    /// None = traverse all relation types.
    pub graph_relation_filter: Option<String>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            candidates_per_channel: 20,
            top_k: 6,
            weights: HybridWeights::default(),
            path_prefix: None,
            domain: None,
            query_vec: None,
            vec_available: false,
            record_access: true,
            include_archived: false,
            mmr_threshold: Some(0.85),
            graph_expand_hops: 0,
            graph_relation_filter: None,
        }
    }
}

fn resolve_weights(opts: &SearchOptions) -> HybridWeights {
    if opts.weights != HybridWeights::default() {
        return opts.weights.clone();
    }

    let path = opts.path_prefix.as_deref().unwrap_or("");
    if path.starts_with("/wiki") || path.starts_with("/behavior") || path.starts_with("/rules") {
        HybridWeights {
            decay: 0.02,
            semantic: 0.48,
            fts: 0.30,
            symbolic: 0.20,
            use_rrf: true,
        }
    } else if path.starts_with("/events") || path.starts_with("/notes") {
        HybridWeights {
            decay: 0.25,
            semantic: 0.35,
            fts: 0.25,
            symbolic: 0.15,
            use_rrf: true,
        }
    } else {
        HybridWeights::default()
    }
}

/// MMR-inspired diversity filter: greedily select results that are both
/// relevant (high score) and diverse (low similarity to already-selected).
///
/// Candidates with cosine similarity > `threshold` to any already-selected
/// entry are deferred to the end rather than dropped entirely.
///
/// Ported from memory-lancedb-pro's `applyMMRDiversity()`.
fn apply_mmr_diversity(
    ranked: &[(&String, f64)],
    entries: &HashMap<String, MemoryEntry>,
    threshold: f64,
    needed: usize,
) -> Vec<String> {
    if ranked.len() <= 1 {
        return ranked.iter().map(|(id, _)| id.to_string()).collect();
    }

    let mut selected: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    for (idx, (id, _)) in ranked.iter().enumerate() {
        if selected.len() >= needed {
            deferred.extend(
                ranked[idx..]
                    .iter()
                    .map(|(rest_id, _)| (*rest_id).to_string()),
            );
            break;
        }

        let candidate = entries.get(*id);
        let c_vec = candidate.and_then(|e| e.vector.as_ref());

        let too_similar = selected.iter().any(|sel_id| {
            let sel_entry = entries.get(sel_id);
            let s_vec = sel_entry.and_then(|e| e.vector.as_ref());

            match (s_vec, c_vec) {
                (Some(sv), Some(cv)) if !sv.is_empty() && !cv.is_empty() => {
                    cosine_similarity(sv, cv) > threshold
                }
                _ => false, // can't compare without vectors
            }
        });

        if too_similar {
            deferred.push(id.to_string());
        } else {
            selected.push(id.to_string());
        }
    }

    selected.extend(deferred);
    selected
}

fn newest_by_shared_entity(entries: &HashMap<String, &MemoryEntry>) -> HashSet<String> {
    let mut by_entity: HashMap<&str, Vec<&MemoryEntry>> = HashMap::new();
    for entry in entries.values() {
        for entity in &entry.entities {
            let entity = entity.trim();
            if !entity.is_empty() {
                by_entity.entry(entity).or_default().push(*entry);
            }
        }
    }

    by_entity
        .into_values()
        .filter(|items| items.len() > 1)
        .filter_map(|items| {
            items
                .into_iter()
                .max_by(|a, b| a.timestamp.cmp(&b.timestamp))
                .map(|entry| entry.id.clone())
        })
        .collect()
}

fn leading_event_date(text: &str) -> Option<NaiveDate> {
    let rest = text.trim_start().strip_prefix('[')?;
    let date = rest.get(0..10)?;
    if !matches!(
        date.as_bytes(),
        [d0, d1, d2, d3, b'-', m0, m1, b'-', day0, day1]
            if d0.is_ascii_digit()
                && d1.is_ascii_digit()
                && d2.is_ascii_digit()
                && d3.is_ascii_digit()
                && m0.is_ascii_digit()
                && m1.is_ascii_digit()
                && day0.is_ascii_digit()
                && day1.is_ascii_digit()
    ) {
        return None;
    }
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn is_residence_query(query: &str) -> bool {
    query.contains('住') && contains_any(query, &["哪里", "哪儿", "哪", "地址", "地方"])
}

fn is_current_query(query: &str) -> bool {
    contains_any(query, &["现在", "当前", "目前", "如今"])
}

fn is_before_query(query: &str) -> bool {
    contains_any(query, &["之前", "以前", "前住", "搬去", "搬到"])
}

fn is_move_anchor(entry: &MemoryEntry, anchor_terms: &[&str]) -> bool {
    !anchor_terms.is_empty()
        && anchor_terms.iter().any(|term| entry.text.contains(term))
        && contains_any(&entry.text, &["搬到", "搬去", "移居", "迁到", "搬家"])
        && is_residence_entry(entry)
}

fn is_residence_entry(entry: &MemoryEntry) -> bool {
    contains_any(&entry.text, &["住在", "居住", "租了", "租房"])
        || (contains_any(&entry.text, &["搬到", "搬去", "移居"])
            && contains_any(&entry.text, &["租", "住"]))
}

fn temporal_anchor_terms(query: &str) -> Vec<&str> {
    ["北京", "上海", "深圳", "广州", "杭州", "天津", "成都"]
        .into_iter()
        .filter(|term| query.contains(term))
        .collect()
}

fn apply_temporal_residence_adjustments(
    query: &str,
    entries: &HashMap<String, &MemoryEntry>,
    scores: &mut HashMap<String, crate::types::HybridScore>,
) {
    if !is_residence_query(query) {
        return;
    }

    if is_current_query(query) {
        if let Some((id, _)) = entries
            .iter()
            .filter(|(_, entry)| is_residence_entry(entry))
            .filter_map(|(id, entry)| leading_event_date(&entry.text).map(|date| (id, date)))
            .max_by_key(|(_, date)| *date)
        {
            if let Some(score) = scores.get_mut(id) {
                score.final_score = (score.final_score + 0.12).min(1.0);
            }
        }
    }

    if !is_before_query(query) {
        return;
    }

    let anchor_terms = temporal_anchor_terms(query);
    let Some(anchor_date) = entries
        .values()
        .filter(|entry| is_move_anchor(entry, &anchor_terms))
        .filter_map(|entry| leading_event_date(&entry.text))
        .max()
    else {
        return;
    };

    for (id, entry) in entries {
        let Some(event_date) = leading_event_date(&entry.text) else {
            continue;
        };
        if event_date < anchor_date && is_residence_entry(entry) {
            if let Some(score) = scores.get_mut(id) {
                score.final_score = (score.final_score + 0.24).min(1.0);
            }
        }
    }
}

fn lower_query(query: &str) -> String {
    query.to_ascii_lowercase()
}

pub fn is_temporal_query(query: &str) -> bool {
    let q = lower_query(query);
    contains_any(
        &q,
        &[
            "first",
            "last",
            "before",
            "after",
            "earliest",
            "latest",
            "most recent",
            "which happened first",
            "how many days",
            "prior to",
        ],
    )
}

pub fn temporal_sort_desc(query: &str) -> Option<bool> {
    let q = lower_query(query);
    if contains_any(&q, &["last", "latest", "most recent", "after"]) {
        Some(true)
    } else if contains_any(
        &q,
        &[
            "first",
            "earliest",
            "before",
            "which happened first",
            "how many days",
            "prior to",
        ],
    ) {
        Some(false)
    } else {
        None
    }
}

fn is_preference_query(query: &str) -> bool {
    let q = lower_query(query);
    (q.contains("what type of") && contains_any(&q, &[" like", " enjoy", " prefer"]))
        || q.contains("what is my favorite")
        || q.contains("what's my favorite")
}

fn preference_expanded_fts_query(query: &str) -> String {
    if is_preference_query(query) {
        format!("{query} preference favorite enjoy like")
    } else {
        query.to_string()
    }
}

/// Execute a full hybrid search, returning ranked `SearchResult`s.
///
/// Execution plan:
///  1. Vector KNN (via sqlite-vec) — if `query_vec` provided
///  2. FTS5 BM25 — always
///  3. Symbolic bag-of-words — computed in Rust on the fetched entries
///  4. Hybrid score merge (weighted sum) with ACT-R decay
///  5. Sort, take top_k, record access
pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
) -> Result<Vec<SearchResult>, MemoryError> {
    let temporal_query = is_temporal_query(query);
    let n = if temporal_query {
        opts.candidates_per_channel.saturating_mul(2).max(1)
    } else {
        opts.candidates_per_channel
    };

    // ── Channel 1: Vector ─────────────────────────────────────────────────────
    let vec_scores: HashMap<String, f64> = if opts.vec_available {
        if let Some(qv) = &opts.query_vec {
            search_vec(
                conn,
                qv,
                n,
                opts.include_archived,
                opts.path_prefix.as_deref(),
            )?
        } else {
            HashMap::new()
        }
    } else {
        HashMap::new()
    };

    // ── Channel 2: FTS5 ───────────────────────────────────────────────────────
    let fts_query = preference_expanded_fts_query(query);
    let fts_scores = search_fts(
        conn,
        &fts_query,
        n,
        opts.include_archived,
        opts.path_prefix.as_deref(),
    )?;

    // ── Collect all candidate IDs ──────────────────────────────────────────────
    let candidate_ids: Vec<String> = vec_scores
        .keys()
        .chain(fts_scores.keys())
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if candidate_ids.is_empty() {
        return Ok(vec![]);
    }

    // ── Bulk-fetch entries ─────────────────────────────────────────────────────
    let entries_map = fetch_by_ids(conn, &candidate_ids, opts.include_archived)?;

    // ── Channel 3: Symbolic ───────────────────────────────────────────────────
    let symbolic_scores: HashMap<String, f64> = entries_map
        .iter()
        .map(|(id, entry)| {
            let score = symbolic_score(query, &entry.text, &entry.keywords);
            (id.clone(), score)
        })
        .collect();

    // ── Optional path-prefix filter ───────────────────────────────────────────
    let entries_ref: HashMap<String, &MemoryEntry> = entries_map
        .iter()
        .filter(|(_, e)| {
            // Path prefix filter
            if let Some(prefix) = &opts.path_prefix {
                if !e.path.starts_with(prefix.as_str()) {
                    return false;
                }
            }
            // Domain filter
            if let Some(domain) = &opts.domain {
                match &e.domain {
                    Some(d) if d == domain => {}
                    _ => return false,
                }
            }
            true
        })
        .map(|(k, v)| (k.clone(), v))
        .collect();

    if entries_ref.is_empty() {
        return Ok(vec![]);
    }

    // ── ACT-R access history (Spreading Activation: 越用越靠前) ──────────────
    let candidate_ids_vec: Vec<String> = entries_ref.keys().cloned().collect();
    let access_times = get_access_times(conn, &candidate_ids_vec).unwrap_or_default();

    // ── Hybrid scoring with ACT-R enhancement ─────────────────────────────────
    let mut weights = resolve_weights(opts);
    if temporal_query {
        // Temporal questions often ask about old events; recency decay should
        // not demote the correct historical answer.
        weights.decay = 0.0;
    }
    let mut scores = hybrid_score(
        &entries_ref,
        &vec_scores,
        &fts_scores,
        &symbolic_scores,
        &weights,
        &access_times,
    );

    let superseded_ids = get_superseded_ids(conn, &candidate_ids_vec).unwrap_or_default();
    for id in &superseded_ids {
        if let Some(score) = scores.get_mut(id) {
            score.final_score *= 0.3;
        }
    }

    let newest_by_entity = newest_by_shared_entity(&entries_ref);
    for id in newest_by_entity {
        if !superseded_ids.contains(&id) {
            if let Some(score) = scores.get_mut(&id) {
                score.final_score *= 1.08;
            }
        }
    }
    apply_temporal_residence_adjustments(query, &entries_ref, &mut scores);
    // ── Sort and take top K ───────────────────────────────────────────────────
    let mut ranked: Vec<(&String, f64)> = scores
        .iter()
        .filter(|(id, _)| entries_ref.contains_key(*id))
        .map(|(id, hs)| (id, hs.final_score))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // ── MMR diversity: defer near-duplicate entries to end ─────────────────────
    let mut ranked_ids: Vec<String> = if let Some(threshold) = opts.mmr_threshold {
        apply_mmr_diversity(&ranked, &entries_map, threshold, opts.top_k)
    } else {
        ranked.iter().map(|(id, _)| id.to_string()).collect()
    };

    if let Some(desc) = temporal_sort_desc(query) {
        ranked_ids.sort_by(|a, b| {
            let a_ts = entries_map
                .get(a)
                .map(|entry| entry.timestamp.as_str())
                .unwrap_or("");
            let b_ts = entries_map
                .get(b)
                .map(|entry| entry.timestamp.as_str())
                .unwrap_or("");
            if desc {
                b_ts.cmp(a_ts)
            } else {
                a_ts.cmp(b_ts)
            }
        });
    }

    // ── Build output ──────────────────────────────────────────────────────────
    let mut results: Vec<SearchResult> = ranked_ids
        .iter()
        .take(opts.top_k)
        .filter_map(|id| {
            let entry = entries_map.get(id)?.clone();
            let score = scores.get(id)?.clone();
            Some(SearchResult { entry, score })
        })
        .collect();

    // ── Graph expansion (post-search augmentation) ────────────────────────────
    // If graph_expand_hops > 0, BFS from result IDs to find related entries
    // and append them to results. This enriches search with causally/temporally
    // linked memories without making them score higher than direct matches.
    if opts.graph_expand_hops > 0 && !results.is_empty() {
        let seed_ids: Vec<String> = results.iter().map(|r| r.entry.id.clone()).collect();
        let rel_filter = opts.graph_relation_filter.as_deref();

        if let Ok(expand_result) = graph_expand(conn, &seed_ids, opts.graph_expand_hops, rel_filter)
        {
            let existing_ids: std::collections::HashSet<String> =
                results.iter().map(|r| r.entry.id.clone()).collect();

            let min_score = results
                .last()
                .map(|r| r.score.final_score * 0.5)
                .unwrap_or(0.1);

            // Compute local PageRank on the expanded subgraph
            let pr_scores = crate::scorer::local_pagerank(&expand_result.edges, 0.85);

            let new_entries: Vec<SearchResult> = expand_result
                .entries
                .into_iter()
                .filter(|entry| !existing_ids.contains(&entry.id))
                .map(|entry| {
                    let distance = expand_result.distances.get(&entry.id).copied().unwrap_or(1);
                    let pr = pr_scores.get(&entry.id).copied().unwrap_or(0.0);
                    // Combine distance decay with PageRank: important hub nodes score higher
                    let graph_boost = min_score * (0.5 / (distance as f64 + 1.0) + 0.5 * pr);
                    SearchResult {
                        entry,
                        score: crate::types::HybridScore {
                            vector: 0.0,
                            fts: 0.0,
                            symbolic: 0.0,
                            decay: 0.0,
                            final_score: graph_boost,
                        },
                    }
                })
                .collect();

            results.extend(new_entries);
        }
    }

    // ── Record access (bump counters) ─────────────────────────────────────────
    if opts.record_access {
        let accessed_ids: Vec<String> = results.iter().map(|r| r.entry.id.clone()).collect();
        record_access(conn, &accessed_ids)?;
        for r in &mut results {
            r.entry.access_count += 1;
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_schema, register_sqlite_vec, try_load_sqlite_vec, upsert};
    use crate::types::{HybridScore, MemoryEntry};
    use chrono::Utc;
    use rusqlite::Connection;
    use serde_json::json;

    fn setup() -> Connection {
        libsimple::enable_auto_extension().unwrap();
        register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        try_load_sqlite_vec(&conn);
        conn
    }

    fn insert(conn: &mut Connection, id: &str, text: &str, keywords: &[&str]) {
        let e = memory_entry(id, text, keywords);
        upsert(conn, &e, false).unwrap();
    }

    fn memory_entry(id: &str, text: &str, keywords: &[&str]) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/test".into(),
            summary: text.chars().take(30).collect(),
            text: text.into(),
            importance: 0.7,
            timestamp: Utc::now().to_rfc3339(),
            category: "fact".into(),
            topic: "".into(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            persons: vec![],
            entities: vec![],
            location: "".into(),
            source: "".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({ "keywords": keywords, "entities": [] }),
            vector: None,
            retention_policy: None,
            domain: None,
        }
    }

    fn score(final_score: f64) -> HybridScore {
        HybridScore {
            vector: 0.0,
            fts: 0.0,
            symbolic: 0.0,
            decay: 0.0,
            final_score,
        }
    }

    #[test]
    fn hybrid_returns_relevant() {
        let mut conn = setup();
        insert(
            &mut conn,
            "a",
            "Rust is fast and memory safe",
            &["rust", "performance"],
        );
        insert(
            &mut conn,
            "b",
            "Python is great for scripting",
            &["python", "scripting"],
        );

        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "rust performance", &opts).unwrap();
        assert!(!results.is_empty());
        // "a" should score higher for "rust performance" query
        assert_eq!(results[0].entry.id, "a");
    }

    #[test]
    fn hybrid_uses_fts_when_vectors_are_available_but_query_vec_missing() {
        let mut conn = setup();
        insert(
            &mut conn,
            "a",
            "Voyage outage should still allow lexical fallback search",
            &["voyage", "fallback"],
        );
        insert(&mut conn, "b", "Unrelated operational note", &["ops"]);

        let opts = SearchOptions {
            top_k: 3,
            record_access: false,
            vec_available: true,
            query_vec: None,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "voyage fallback", &opts).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].entry.id, "a");
        assert!(results[0].score.fts > 0.0);
    }

    #[test]
    fn empty_query_returns_empty() {
        let mut conn = setup();
        insert(&mut conn, "x", "some text", &[]);
        let opts = SearchOptions {
            record_access: false,
            ..Default::default()
        };
        let results = hybrid_search(&conn, "", &opts).unwrap();
        // FTS5 with empty query should produce no FTS results; vec channel also empty
        assert!(results.is_empty());
    }

    #[test]
    fn current_residence_prefers_latest_dated_residence() {
        let beijing = memory_entry(
            "beijing",
            "[2026-01-15] 张明说他28岁，住在北京海淀区，在字节跳动做Go后端工程师。",
            &["张明", "北京", "海淀"],
        );
        let shenzhen = memory_entry(
            "shenzhen",
            "[2026-04-05] 张明搬到深圳南山区了，租了两室一厅，在腾讯大厦附近。",
            &["张明", "深圳", "南山"],
        );

        let entries = HashMap::from([
            (beijing.id.clone(), &beijing),
            (shenzhen.id.clone(), &shenzhen),
        ]);
        let mut scores = HashMap::from([
            (beijing.id.clone(), score(0.50)),
            (shenzhen.id.clone(), score(0.45)),
        ]);

        apply_temporal_residence_adjustments("张明现在住在哪里", &entries, &mut scores);

        assert!(scores["shenzhen"].final_score > scores["beijing"].final_score);
    }

    #[test]
    fn before_move_residence_prefers_prior_residence() {
        let beijing = memory_entry(
            "beijing",
            "[2026-01-15] 张明说他28岁，住在北京海淀区，在字节跳动做Go后端工程师。",
            &["张明", "北京", "海淀"],
        );
        let offer = memory_entry(
            "offer",
            "[2026-03-28] 张明收到深圳一家区块链创业公司的offer，他决定接，说北京待了五年也想换个城市。",
            &["张明", "深圳", "北京"],
        );
        let shenzhen = memory_entry(
            "shenzhen",
            "[2026-04-05] 张明搬到深圳南山区了，租了两室一厅，在腾讯大厦附近。",
            &["张明", "深圳", "南山"],
        );

        let entries = HashMap::from([
            (beijing.id.clone(), &beijing),
            (offer.id.clone(), &offer),
            (shenzhen.id.clone(), &shenzhen),
        ]);
        let mut scores = HashMap::from([
            (beijing.id.clone(), score(0.43)),
            (offer.id.clone(), score(0.44)),
            (shenzhen.id.clone(), score(0.49)),
        ]);

        apply_temporal_residence_adjustments("张明搬去深圳之前住在哪里", &entries, &mut scores);

        assert!(scores["beijing"].final_score > scores["shenzhen"].final_score);
        assert!(scores["beijing"].final_score > scores["offer"].final_score);
    }
}
