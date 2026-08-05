//! Post-ranking graph expansion for hybrid search.

use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use crate::{
    db::{get_superseded_ids, graph_expand},
    error::MemoryError,
    namespace::surface_of,
    types::{
        GraphInjectionProvenance, GraphTraversalInjection, HybridScore, MemoryEntry, SearchResult,
    },
};

use super::{
    filtering::{is_search_noise_entry, normalized_seed_weights, valid_at},
    GraphPhaseReceipt, SearchOptions,
};

fn injection_parent<'a>(id: &str, injection: &'a GraphTraversalInjection) -> Option<&'a str> {
    if injection.source_id == id {
        Some(injection.target_id.as_str())
    } else if injection.target_id == id {
        Some(injection.source_id.as_str())
    } else {
        None
    }
}

/// Walk the recorded BFS injection chain from `id` back to the seed it
/// ultimately traces to (the entry in `distances` with distance 0).
/// Bounded by `distances[id] + 1` steps: each recorded injection must point
/// to a node one hop closer to a seed, so the walk terminates at a seed.
fn resolve_seed_ancestor(
    id: &str,
    injections: &HashMap<String, GraphTraversalInjection>,
    distances: &HashMap<String, u32>,
) -> Option<String> {
    let mut current = id.to_string();
    let max_steps = distances.get(id).copied().unwrap_or(0) as usize + 1;
    for _ in 0..max_steps {
        if distances.get(&current).copied() == Some(0) {
            return Some(current);
        }
        let current_distance = distances.get(&current).copied()?;
        let injection = injections.get(&current)?;
        let parent = injection_parent(&current, injection)?;
        let parent_distance = distances.get(parent).copied()?;
        if parent_distance + 1 != current_distance {
            return None;
        }
        current = parent.to_string();
    }
    None
}

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
            relation_counts: BTreeMap::new(),
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
            relation_counts: BTreeMap::new(),
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
            relation_counts: BTreeMap::new(),
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
            // tachi#1647: attribution is the edge that actually inserted
            // this id during BFS, recorded at traversal time. When multiple
            // same-depth parent edges exist, the winning edge is the first
            // eligible edge processed by `graph_expand_limited`'s batch
            // order (`source_id`, `target_id`, `relation`), independent of
            // edge strength. Receipts are observational; scoring still reads
            // only `edges`/`distances` above.
            let graph_provenance = expand_result
                .injection_edges
                .get(&entry.id)
                .and_then(|edge| {
                    resolve_seed_ancestor(
                        &entry.id,
                        &expand_result.injection_edges,
                        &expand_result.distances,
                    )
                    .map(|from_id| GraphInjectionProvenance {
                        via_edge: edge.relation.clone(),
                        from_id,
                        activation: activation as f32,
                        distance: u8::try_from(edge.depth).unwrap_or(u8::MAX),
                    })
                });
            (
                ms,
                SearchResult {
                    entry,
                    // The all-zero component scores below are the shape a
                    // consumer previously had to pattern-match on to detect
                    // a graph-injected result (tachi#1647): none of the
                    // vector/FTS/symbolic/decay channels ran for this
                    // candidate, only `final_score` (the graph boost). That
                    // shape is still produced here — it is an honest report
                    // of "these channels did not run" — but `graph_injected`
                    // below is now the documented, explicit marker; do not
                    // reintroduce a new all-zero-sniffing consumer.
                    score: HybridScore {
                        vector: 0.0,
                        fts: 0.0,
                        symbolic: 0.0,
                        decay: 0.0,
                        final_score: graph_boost,
                    },
                    graph_injected: true,
                    graph_provenance,
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

    // tachi#1647 2b: counts reflect what is actually returned to the caller
    // (post-truncation), keyed by each injected result's own `via_edge` —
    // not a separate re-derivation.
    let relation_counts: BTreeMap<String, usize> =
        new_entries
            .iter()
            .fold(BTreeMap::new(), |mut acc, (_, sr)| {
                if let Some(provenance) = &sr.graph_provenance {
                    *acc.entry(provenance.via_edge.clone()).or_insert(0) += 1;
                }
                acc
            });

    let expanded_count = new_entries.len();
    results.extend(new_entries.into_iter().map(|(_, sr)| sr));
    let receipt = phase_start.map(|s| GraphPhaseReceipt {
        enabled: true,
        failed: false,
        elapsed: s.elapsed(),
        expanded_count,
        relation_counts,
    });
    Ok(((), receipt))
}
