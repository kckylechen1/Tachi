//! Candidate collection for hybrid search.

use rusqlite::{types::Value, Connection};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::{
    db::{fetch_by_ids, search_symbolic_candidates_with_relevance, search_vec},
    error::MemoryError,
    namespace::surface_sql_splice,
    recall_config::TypoFallbackConfig,
    types::MemoryEntry,
};

use super::{
    expansion::{search_fts_with_expansion_config, symbolic_query_with_expansion},
    recall_config, CandidatePhaseReceipt, ChannelPhaseReceipt, SearchOptions,
    TypoFallbackAttribution, TypoFallbackPhaseReceipt,
};

const SYMBOLIC_CANDIDATE_MULTIPLIER: usize = 10;

pub(super) struct CandidateSet {
    pub(super) vec_scores: HashMap<String, f64>,
    pub(super) fts_scores: HashMap<String, f64>,
    pub(super) typo_scores: HashMap<String, f64>,
    pub(super) typo_candidate_ids: HashSet<String>,
    pub(super) typo_attribution: TypoFallbackAttribution,
    pub(super) exact_id: Option<String>,
    pub(super) candidate_ids: Vec<String>,
    /// `None` is the normal hot path: no observer map, candidate-ID clone, or
    /// symbolic membership scan is performed unless the caller explicitly
    /// asks for bounded normal-leg coverage evidence. Typo fallback membership
    /// travels separately through `typo_candidate_ids` and #1447 attribution.
    pub(super) observed_evidence: Option<HashMap<String, super::CandidateLegEvidence>>,
}

struct TypoFallbackCandidates {
    scores: HashMap<String, f64>,
    candidate_ids: HashSet<String>,
    attribution: TypoFallbackAttribution,
    receipt: Option<TypoFallbackPhaseReceipt>,
}

impl TypoFallbackCandidates {
    fn not_activated() -> Self {
        Self {
            scores: HashMap::new(),
            candidate_ids: HashSet::new(),
            attribution: TypoFallbackAttribution::default(),
            receipt: None,
        }
    }
}

pub(super) fn collect_candidates(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
    include_superseded: bool,
    as_of_utc: Option<&str>,
    sample: bool,
    observed_ids: Option<&[String]>,
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
                opts.surface,
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
        opts.surface,
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
        opts.surface,
    )?;
    let symbolic_elapsed = symbolic_start.map(|s| s.elapsed());
    let symbolic_candidate_count = symbolic_candidate_entries.len();

    let exact_id = exact_memory_id_query(query);
    let normal_candidate_ids = vec_scores
        .keys()
        .chain(fts_scores.keys())
        .chain(symbolic_candidate_entries.iter().map(|entry| &entry.id))
        .chain(exact_id.as_ref())
        .cloned()
        .collect::<HashSet<_>>();
    let typo = collect_typo_fallback_candidates(
        conn,
        query,
        opts,
        include_superseded,
        as_of_utc,
        &vec_scores,
        &fts_scores,
        &symbolic_candidate_entries,
        exact_id.as_deref(),
        &normal_candidate_ids,
        sample,
    )?;
    let TypoFallbackCandidates {
        scores: typo_scores,
        candidate_ids: typo_candidate_ids,
        attribution: typo_attribution,
        receipt: typo_receipt,
    } = typo;
    // Explicit type: the receipt below calls `candidate_ids.len()` inside a
    // closure, and method resolution can't wait for the `CandidateSet` literal
    // at the end of the function to pin the element type down. Matches
    // `CandidateSet::candidate_ids` exactly.
    let candidate_ids: Vec<String> = vec_scores
        .keys()
        .chain(fts_scores.keys())
        .chain(symbolic_candidate_entries.iter().map(|entry| &entry.id))
        .chain(exact_id.as_ref())
        .chain(typo_candidate_ids.iter())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let observed_evidence = observed_ids.map(|ids| {
        ids.iter()
            .map(|id| {
                (
                    id.clone(),
                    super::CandidateLegEvidence {
                        vector: vec_scores.contains_key(id),
                        fts: fts_scores.contains_key(id),
                        symbolic: symbolic_candidate_entries
                            .iter()
                            .any(|entry| entry.id == id.as_str()),
                        exact_id: exact_id.as_deref() == Some(id.as_str()),
                    },
                )
            })
            .collect()
    });

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
        typo_fallback: typo_receipt,
        merged_candidate_count: candidate_ids.len(),
    });

    Ok((
        CandidateSet {
            vec_scores,
            fts_scores,
            typo_scores,
            typo_candidate_ids,
            typo_attribution,
            exact_id,
            candidate_ids,
            observed_evidence,
        },
        receipt,
    ))
}

#[allow(clippy::too_many_arguments)]
fn collect_typo_fallback_candidates(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
    include_superseded: bool,
    as_of_utc: Option<&str>,
    vec_scores: &HashMap<String, f64>,
    fts_scores: &HashMap<String, f64>,
    symbolic_entries: &[MemoryEntry],
    exact_id: Option<&str>,
    normal_candidate_ids: &HashSet<String>,
    sample: bool,
) -> Result<TypoFallbackCandidates, MemoryError> {
    let config = &recall_config(opts).typo_fallback;
    let Some(query_terms) = eligible_typo_query_terms(query, config) else {
        return Ok(TypoFallbackCandidates::not_activated());
    };
    if !normal_legs_are_weak(
        query,
        vec_scores,
        fts_scores,
        symbolic_entries,
        exact_id,
        config,
    ) {
        return Ok(TypoFallbackCandidates::not_activated());
    }

    let started = Instant::now();
    let prefilter_ids = typo_prefilter_ids(
        conn,
        &query_terms,
        opts,
        include_superseded,
        as_of_utc,
        config,
    )?;
    let prefilter_candidate_count = prefilter_ids.len();
    let new_prefilter_ids = prefilter_ids
        .into_iter()
        .filter(|id| !normal_candidate_ids.contains(id))
        .collect::<Vec<_>>();
    let entries = fetch_by_ids(conn, &new_prefilter_ids, opts.include_archived)?;
    let compared_candidate_count = entries.len();
    let mut token_comparison_count = 0usize;
    let mut edit_cell_count = 0usize;
    let mut accepted = entries
        .values()
        .filter_map(|entry| {
            typo_candidate_similarity(
                &query_terms,
                entry,
                config,
                &mut token_comparison_count,
                &mut edit_cell_count,
            )
            .map(|score| (entry.id.clone(), score * config.symbolic_score_factor))
        })
        .collect::<Vec<_>>();
    accepted.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    accepted.truncate(config.max_candidates);
    let typo_scores = accepted.into_iter().collect::<HashMap<_, _>>();
    let typo_candidate_ids = typo_scores.keys().cloned().collect::<HashSet<_>>();
    let attribution = TypoFallbackAttribution {
        activated: true,
        prefilter_candidate_count,
        compared_candidate_count,
        token_comparison_count,
        edit_cell_count,
        contributed_candidate_count: typo_candidate_ids.len(),
    };
    let receipt = sample.then(|| TypoFallbackPhaseReceipt {
        elapsed: started.elapsed(),
        prefilter_candidate_count,
        compared_candidate_count,
        token_comparison_count,
        edit_cell_count,
        contributed_candidate_count: typo_candidate_ids.len(),
    });
    Ok(TypoFallbackCandidates {
        scores: typo_scores,
        candidate_ids: typo_candidate_ids,
        attribution,
        receipt,
    })
}

fn eligible_typo_query_terms(query: &str, config: &TypoFallbackConfig) -> Option<Vec<String>> {
    if !config.enabled || config.symbolic_score_factor <= 0.0 {
        return None;
    }
    let terms = query
        .split_whitespace()
        .map(|term| term.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if !(config.min_query_terms..=config.max_query_terms).contains(&terms.len()) {
        return None;
    }
    if terms.iter().any(|term| {
        !term.chars().all(|ch| ch.is_ascii_alphabetic())
            || !(config.min_token_chars..=config.max_token_chars).contains(&term.len())
    }) {
        return None;
    }
    Some(terms)
}

fn normal_legs_are_weak(
    query: &str,
    vec_scores: &HashMap<String, f64>,
    fts_scores: &HashMap<String, f64>,
    symbolic_entries: &[MemoryEntry],
    exact_id: Option<&str>,
    config: &TypoFallbackConfig,
) -> bool {
    exact_id.is_none()
        && vec_scores
            .values()
            .all(|score| *score <= config.max_normal_vector_similarity)
        && fts_scores
            .values()
            .all(|score| *score <= config.max_normal_fts_score)
        && symbolic_entries.iter().all(|entry| {
            crate::scorer::symbolic_score_entry(query, entry) <= config.max_normal_symbolic_score
        })
}

fn typo_prefilter_ids(
    conn: &Connection,
    query_terms: &[String],
    opts: &SearchOptions,
    include_superseded: bool,
    as_of_utc: Option<&str>,
    config: &TypoFallbackConfig,
) -> Result<Vec<String>, MemoryError> {
    let table_available = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories_symbolic_fts'",
            [],
            |_| Ok(()),
        )
        .is_ok();
    if !table_available {
        return Ok(Vec::new());
    }
    let mut trigrams = Vec::new();
    let mut seen = HashSet::new();
    for term in query_terms {
        let bytes = term.as_bytes();
        for window in bytes.windows(3) {
            let trigram = std::str::from_utf8(window).expect("ASCII query eligibility");
            if seen.insert(trigram.to_string()) {
                trigrams.push(trigram.to_string());
                if trigrams.len() >= config.max_query_trigrams {
                    break;
                }
            }
        }
        if trigrams.len() >= config.max_query_trigrams {
            break;
        }
    }
    if trigrams.is_empty() {
        return Ok(Vec::new());
    }
    let match_query = trigrams
        .iter()
        .map(|trigram| format!("\"{trigram}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let path_like = opts.path_prefix.as_ref().map(|prefix| format!("{prefix}%"));
    let surface_clause = surface_sql_splice(opts.surface, true);
    let sql = format!(
        "SELECT m.id
           FROM memories_symbolic_fts
           JOIN memories m ON m.id = memories_symbolic_fts.id
          WHERE (?1 = 1 OR m.archived = 0)
            AND (?2 = 1 OR m.superseded_by IS NULL)
            AND (?3 IS NULL OR m.path LIKE ?3)
            AND (?4 IS NULL OR (COALESCE(NULLIF(m.valid_from, ''), m.timestamp) <= ?4 AND (m.valid_until IS NULL OR m.valid_until > ?4)))
            AND m.id NOT LIKE 'anchor:%'
            AND memories_symbolic_fts MATCH ?5{surface_clause}
          ORDER BY bm25(memories_symbolic_fts), m.id
          LIMIT ?6"
    );
    let params: Vec<Value> = vec![
        (opts.include_archived as i64).into(),
        (include_superseded as i64).into(),
        path_like.into(),
        as_of_utc.map(str::to_owned).into(),
        match_query.into(),
        (config.prefilter_candidate_limit as i64).into(),
    ];
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| row.get(0))?;
    rows.collect::<Result<Vec<String>, _>>().map_err(Into::into)
}

fn typo_candidate_similarity(
    query_terms: &[String],
    entry: &MemoryEntry,
    config: &TypoFallbackConfig,
    token_comparison_count: &mut usize,
    edit_cell_count: &mut usize,
) -> Option<f64> {
    let candidate_tokens = bounded_candidate_tokens(entry, config);
    let mut score_sum = 0.0;
    for query_term in query_terms {
        let mut best = 0.0_f64;
        for candidate_term in &candidate_tokens {
            *token_comparison_count = token_comparison_count.saturating_add(1);
            let length_delta = query_term.len().abs_diff(candidate_term.len());
            if length_delta > config.max_edit_distance {
                continue;
            }
            let distance = osa_distance(query_term, candidate_term, edit_cell_count);
            if distance > config.max_edit_distance {
                continue;
            }
            let similarity =
                1.0 - distance as f64 / query_term.len().max(candidate_term.len()).max(1) as f64;
            best = best.max(similarity);
        }
        if best < config.min_token_similarity {
            return None;
        }
        score_sum += best;
    }
    Some(score_sum / query_terms.len() as f64)
}

fn bounded_candidate_tokens(entry: &MemoryEntry, config: &TypoFallbackConfig) -> Vec<String> {
    let keyword_text = entry.keywords.join(" ");
    let entity_text = entry.entities.join(" ");
    let fields = [
        entry.summary.as_str(),
        entry.text.as_str(),
        entry.topic.as_str(),
        keyword_text.as_str(),
        entity_text.as_str(),
        entry.path.as_str(),
        entry.id.as_str(),
    ];
    let mut remaining_chars = config.max_candidate_chars;
    let mut seen = HashSet::new();
    let mut tokens = Vec::new();
    for field in fields {
        if remaining_chars == 0 || tokens.len() >= config.max_candidate_tokens {
            break;
        }
        let bounded = field.chars().take(remaining_chars).collect::<String>();
        remaining_chars = remaining_chars.saturating_sub(bounded.chars().count());
        for token in crate::scorer::tokenize(&bounded) {
            if token.chars().all(|ch| ch.is_ascii_alphabetic())
                && (config.min_token_chars..=config.max_token_chars).contains(&token.len())
                && seen.insert(token.clone())
            {
                tokens.push(token);
                if tokens.len() >= config.max_candidate_tokens {
                    break;
                }
            }
        }
    }
    tokens
}

/// Optimal-string-alignment distance: insertion/deletion/substitution plus one
/// adjacent transposition. Inputs are ASCII-only by the fallback eligibility
/// contract, so byte indexing is character indexing.
fn osa_distance(left: &str, right: &str, edit_cell_count: &mut usize) -> usize {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let columns = right.len() + 1;
    let rows = left.len() + 1;
    *edit_cell_count = edit_cell_count.saturating_add(rows.saturating_mul(columns));
    let mut distance = vec![0usize; rows * columns];
    for row in 0..rows {
        distance[row * columns] = row;
    }
    for (column, cell) in distance.iter_mut().take(columns).enumerate() {
        *cell = column;
    }
    for row in 1..rows {
        for column in 1..columns {
            let substitution_cost = usize::from(left[row - 1] != right[column - 1]);
            let mut value = (distance[(row - 1) * columns + column] + 1)
                .min(distance[row * columns + column - 1] + 1)
                .min(distance[(row - 1) * columns + column - 1] + substitution_cost);
            if row > 1
                && column > 1
                && left[row - 1] == right[column - 2]
                && left[row - 2] == right[column - 1]
            {
                value = value.min(distance[(row - 2) * columns + column - 2] + 1);
            }
            distance[row * columns + column] = value;
        }
    }
    distance[left.len() * columns + right.len()]
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
