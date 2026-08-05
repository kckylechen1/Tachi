//! Post-ranking graph expansion for hybrid search.

use rusqlite::Connection;
use std::collections::HashSet;
use std::time::Instant;

use crate::{
    db::{get_superseded_ids, graph_expand},
    error::MemoryError,
    namespace::surface_of,
    types::{HybridScore, MemoryEntry, SearchResult},
};

use super::{
    filtering::{is_search_noise_entry, normalized_seed_weights, valid_at},
    GraphPhaseReceipt, SearchOptions,
};

pub(super) fn append_graph_expansion(
    conn: &Connection,
    results: &mut Vec<SearchResult>,
    opts: &SearchOptions,
    include_superseded: bool,
    as_of_utc: Option<&str>,
    sample: bool,
) -> Result<((), Option<GraphPhaseReceipt>), MemoryError> {
    let phase_start = sample.then(Instant::now);
    // The "graph disabled" branch is the `graph_expand_hops == 0 || results
    // .is_empty()` early return at graph_expansion.rs:24-26. We still produce
    // a receipt (`enabled = false`) when sampling so the empty phase is
    // visibly recorded as "did not run" rather than absent.
    if opts.graph_expand_hops == 0 || results.is_empty() {
        let receipt = phase_start.map(|s| GraphPhaseReceipt {
            enabled: false,
            failed: false,
            elapsed: s.elapsed(),
            expanded_count: 0,
        });
        return Ok(((), receipt));
    }

    // Graph is a bounded fallback channel, not a second unbounded result set.
    // Ranking has already truncated the primary set to `top_k`; once those
    // slots are full, appended neighbors would be invisible to the caller but
    // would still be recorded as displays by `hybrid_search` below this phase.
    let remaining_slots = opts.top_k.saturating_sub(results.len());
    if remaining_slots == 0 {
        let receipt = phase_start.map(|s| GraphPhaseReceipt {
            enabled: true,
            failed: false,
            elapsed: s.elapsed(),
            expanded_count: 0,
        });
        return Ok(((), receipt));
    }

    let seed_ids: Vec<String> = results.iter().map(|r| r.entry.id.clone()).collect();
    let rel_filter = opts.graph_relation_filter.as_deref();

    // Graph expansion is a best-effort enrichment; failures are non-fatal.
    let Ok(expand_result) = graph_expand(
        conn,
        &seed_ids,
        opts.graph_expand_hops,
        rel_filter,
        opts.wiki_corpus_store,
    ) else {
        let receipt = phase_start.map(|s| GraphPhaseReceipt {
            enabled: true,
            failed: true,
            elapsed: s.elapsed(),
            expanded_count: 0,
        });
        return Ok(((), receipt));
    };

    let existing_ids: HashSet<String> = results.iter().map(|r| r.entry.id.clone()).collect();
    let min_score = results
        .last()
        .map(|r| r.score.final_score * 0.5)
        .unwrap_or(0.1);

    let seed_weights = normalized_seed_weights(results);
    let activations = crate::scorer::graph_spreading_activation_with_seed_weights(
        &seed_weights,
        &expand_result.edges,
        opts.graph_expand_hops,
        0.5,
    );

    let expanded_entries: Vec<MemoryEntry> = expand_result
        .entries
        .into_iter()
        .filter(|entry| !existing_ids.contains(&entry.id))
        .filter(|entry| valid_at(entry, as_of_utc))
        .filter(|entry| {
            !is_search_noise_entry(
                entry,
                opts.path_prefix.as_deref(),
                opts.bypass_wiki_lifecycle_gate,
            )
        })
        // #1413 concern 2: graph expansion must not leak cross-surface
        // neighbors. When the caller scoped the base query to a surface, only
        // entries the canonical classifier `surface_of` places on that same
        // surface may be expanded in — this reuses the single source of truth,
        // not a duplicate classification rule.
        .filter(|entry| match opts.surface {
            Some(target) => surface_of(entry) == target,
            None => true,
        })
        .collect();
    let expanded_ids: Vec<String> = expanded_entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    let expanded_superseded_ids = if include_superseded {
        HashSet::new()
    } else {
        get_superseded_ids(conn, &expanded_ids)?
    };

    // Decorate each result with its parsed instant (epoch millis) once, then
    // sort on the pre-parsed key — never re-parse in the comparator (tachi#718
    // CP2/CP3).
    let mut new_entries: Vec<(i64, SearchResult)> = expanded_entries
        .into_iter()
        .filter(|entry| {
            if include_superseded {
                return true;
            }
            !expanded_superseded_ids.contains(&entry.id)
        })
        .map(|entry| {
            let distance = expand_result.distances.get(&entry.id).copied().unwrap_or(1);
            let activation = activations.get(&entry.id).copied().unwrap_or(0.0);
            let graph_boost =
                min_score * (0.4 / (distance as f64 + 1.0) + 0.6 * activation).clamp(0.0, 1.0);
            let ms = crate::scorer::timestamp_epoch_millis(&entry.timestamp);
            (
                ms,
                SearchResult {
                    entry,
                    score: HybridScore {
                        vector: 0.0,
                        fts: 0.0,
                        symbolic: 0.0,
                        decay: 0.0,
                        final_score: graph_boost,
                    },
                },
            )
        })
        .collect();
    new_entries.sort_by(|a, b| {
        crate::scorer::cmp_recall_rank(
            (a.1.score.final_score, a.0, a.1.entry.id.as_str()),
            (b.1.score.final_score, b.0, b.1.entry.id.as_str()),
        )
    });
    new_entries.truncate(remaining_slots);

    let expanded_count = new_entries.len();
    results.extend(new_entries.into_iter().map(|(_, sr)| sr));
    let receipt = phase_start.map(|s| GraphPhaseReceipt {
        enabled: true,
        failed: false,
        elapsed: s.elapsed(),
        expanded_count,
    });
    Ok(((), receipt))
}
