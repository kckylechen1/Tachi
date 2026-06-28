//! Candidate collection for hybrid search.

use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use crate::{
    db::{search_symbolic_candidates, search_vec},
    error::MemoryError,
};

use super::{expansion::search_fts_with_expansion_config, recall_config, SearchOptions};

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
) -> Result<CandidateSet, MemoryError> {
    let n = opts.candidates_per_channel;
    let vec_scores = if opts.vec_available {
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

    let fts_scores = search_fts_with_expansion_config(
        conn,
        query,
        n,
        opts.include_archived,
        include_superseded,
        opts.path_prefix.as_deref(),
        as_of_utc,
        recall_config(opts),
    )?;

    // FTS5 can miss hyphenated slugs, exact ids, and short technical tokens
    // (`clean-cli`, `dry-run`, `RECALL_PROBE_*`). Pull a bounded lexical set so
    // symbolic scoring can add candidates instead of merely re-ranking FTS/vec.
    let symbolic_candidate_entries = search_symbolic_candidates(
        conn,
        query,
        n.saturating_mul(SYMBOLIC_CANDIDATE_MULTIPLIER)
            .max(opts.top_k),
        opts.include_archived,
        include_superseded,
        opts.path_prefix.as_deref(),
        as_of_utc,
    )?;
    let exact_id = exact_memory_id_query(query);
    let candidate_ids = vec_scores
        .keys()
        .chain(fts_scores.keys())
        .chain(symbolic_candidate_entries.iter().map(|entry| &entry.id))
        .chain(exact_id.as_ref())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    Ok(CandidateSet {
        vec_scores,
        fts_scores,
        exact_id,
        candidate_ids,
    })
}

fn exact_memory_id_query(query: &str) -> Option<String> {
    let trimmed = query.trim().trim_matches(|c| matches!(c, '`' | '"' | '\''));
    uuid::Uuid::parse_str(trimmed)
        .ok()
        .map(|_| trimmed.to_string())
}
