//! Candidate collection for hybrid search.

use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::{
    db::{search_symbolic_candidates_with_relevance, search_vec},
    error::MemoryError,
};

use super::{
    expansion::{search_fts_with_expansion_config, symbolic_query_with_expansion},
    recall_config, CandidatePhaseReceipt, ChannelPhaseReceipt, SearchOptions,
};

const SYMBOLIC_CANDIDATE_MULTIPLIER: usize = 10;

pub(super) struct CandidateSet {
    pub(super) vec_scores: HashMap<String, f64>,
    pub(super) fts_scores: HashMap<String, f64>,
    pub(super) exact_id: Option<String>,
    pub(super) candidate_ids: Vec<String>,
}

pub(super) fn collect_candidates(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
    include_superseded: bool,
    as_of_utc: Option<&str>,
    sample: bool,
) -> Result<(CandidateSet, Option<CandidatePhaseReceipt>), MemoryError> {
    let n = opts.candidates_per_channel;
    let phase_start = sample.then(Instant::now);

    // ── Vector KNN ───────────────────────────────────────────────────────────
    // The "vector unavailable" branch is the `if opts.vec_available { ... }
    // else { HashMap::new() }` at candidates.rs:30-31 / 44-45 — when the
    // channel was structurally off we record `vec: None` (honest "did not
    // run") rather than `Some({0 elapsed, 0 count})`.
    let vec_start = sample.then(Instant::now);
    let mut vec_scores = if opts.vec_available {
        if let Some(qv) = &opts.query_vec {
            search_vec(
                conn,
                qv,
                n,
                opts.include_archived,
                include_superseded,
                opts.path_prefix.as_deref(),
                as_of_utc,
            )?
        } else {
            HashMap::new()
        }
    } else {
        HashMap::new()
    };
    apply_raw_vector_similarity_floor(conn, &mut vec_scores, recall_config(opts))?;
    let vec_receipt = if opts.vec_available && opts.query_vec.is_some() {
        vec_start.map(|s| ChannelPhaseReceipt {
            elapsed: s.elapsed(),
            candidate_count: vec_scores.len(),
        })
    } else {
        None
    };

    // ── FTS + expansion + OR-fallback ────────────────────────────────────────
    // Per #1097 D3 each executed FTS group (the original query at idx 0, each
    // expanded variant at idx > 0, and the OR-fallback that fires only when
    // `merged.is_empty()`) gets its own `Instant`. Without per-group timing
    // a "FTS slow" report could not distinguish a noisy expanded variant
    // from the original query — and could not give a bounded follow-up.
    let (fts_scores, fts_groups) = search_fts_with_expansion_config(
        conn,
        query,
        n,
        opts.include_archived,
        include_superseded,
        opts.path_prefix.as_deref(),
        as_of_utc,
        recall_config(opts),
        sample,
    )?;
    let fts_candidate_count = fts_scores.len();

    // ── Symbolic bag-of-words ────────────────────────────────────────────────
    // FTS5 can miss hyphenated slugs, exact ids, and short technical tokens
    // (`clean-cli`, `dry-run`, `RECALL_PROBE_*`). Pull a bounded lexical set so
    // symbolic scoring can add candidates instead of merely re-ranking FTS/vec.
    let symbolic_start = sample.then(Instant::now);
    let symbolic_relevance_query = symbolic_query_with_expansion(query);
    let symbolic_candidate_entries = search_symbolic_candidates_with_relevance(
        conn,
        query,
        &symbolic_relevance_query,
        n.saturating_mul(SYMBOLIC_CANDIDATE_MULTIPLIER)
            .max(opts.top_k),
        opts.include_archived,
        include_superseded,
        opts.path_prefix.as_deref(),
        as_of_utc,
    )?;
    let symbolic_elapsed = symbolic_start.map(|s| s.elapsed());
    let symbolic_candidate_count = symbolic_candidate_entries.len();

    let exact_id = exact_memory_id_query(query);
    // Explicit type: the receipt below calls `candidate_ids.len()` inside a
    // closure, and method resolution can't wait for the `CandidateSet` literal
    // at the end of the function to pin the element type down. Matches
    // `CandidateSet::candidate_ids` exactly.
    let candidate_ids: Vec<String> = vec_scores
        .keys()
        .chain(fts_scores.keys())
        .chain(symbolic_candidate_entries.iter().map(|entry| &entry.id))
        .chain(exact_id.as_ref())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let receipt = phase_start.map(|s| CandidatePhaseReceipt {
        total_elapsed: s.elapsed(),
        vec_available: opts.vec_available,
        vec: vec_receipt,
        fts_groups: fts_groups.unwrap_or_default(),
        fts_candidate_count,
        symbolic: ChannelPhaseReceipt {
            elapsed: symbolic_elapsed.unwrap_or_default(),
            candidate_count: symbolic_candidate_count,
        },
        merged_candidate_count: candidate_ids.len(),
    });

    Ok((
        CandidateSet {
            vec_scores,
            fts_scores,
            exact_id,
            candidate_ids,
        },
        receipt,
    ))
}

fn exact_memory_id_query(query: &str) -> Option<String> {
    let trimmed = query.trim().trim_matches(|c| matches!(c, '`' | '"' | '\''));
    uuid::Uuid::parse_str(trimmed)
        .ok()
        .map(|_| trimmed.to_string())
}

/// Drop raw-tier vector-channel hits whose similarity is below the configured floor.
fn apply_raw_vector_similarity_floor(
    conn: &Connection,
    vec_scores: &mut HashMap<String, f64>,
    config: &crate::RecallConfig,
) -> Result<(), MemoryError> {
    let floor = config.raw_vector_similarity_floor;
    if vec_scores.is_empty() || floor <= 0.0 {
        return Ok(());
    }

    let ids: Vec<String> = vec_scores.keys().cloned().collect();
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!("SELECT id, tier FROM memories WHERE id IN ({placeholders})");
    let mut stmt = conn.prepare(&sql)?;
    let tiers = stmt
        .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?;

    vec_scores.retain(|id, sim| {
        tiers
            .get(id)
            .is_none_or(|tier| tier != "raw" || *sim >= floor)
    });
    Ok(())
}
