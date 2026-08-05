//! Post-ranking graph expansion for hybrid search.

use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use crate::{
    db::{get_superseded_ids, graph_expand},
    error::MemoryError,
    namespace::surface_of,
    types::{GraphInjectionProvenance, HybridScore, MemoryEdge, MemoryEntry, SearchResult},
};

use super::{
    filtering::{is_search_noise_entry, normalized_seed_weights, valid_at},
    GraphPhaseReceipt, SearchOptions,
};

/// One BFS hop's worth of attribution for a single discovered id: the
/// neighbor one hop closer to a seed, the relation of the edge that
/// connects them, and the selection weight used to pick that edge among
/// competing candidates.
struct ParentEdge {
    parent_id: String,
    via_edge: String,
    weight: f64,
}

/// For every id `graph_expand`/`graph_expand_limited` discovered, pick the
/// single strongest edge that attaches it to a node exactly one hop closer
/// to a seed (tachi#1647 2a).
///
/// "Strongest" mirrors the same `scorer::graph_relation_activation_weight
/// (relation) * edge.weight` formula the spreading-activation scorer above
/// already uses to propagate `activation` — this does not invent a new
/// selection, it reads off the scorer's own per-edge strength. Ties are
/// broken by `edges`' own deterministic order: `db::graph_expand_limited`
/// already sorts its returned edges by `(source_id, target_id, relation)`
/// before returning them, and this only overwrites a candidate on a
/// strictly-greater weight, so the first edge seen for a given id wins any
/// tie.
fn strongest_parent_edges(
    edges: &[MemoryEdge],
    distances: &HashMap<String, u32>,
) -> HashMap<String, ParentEdge> {
    let mut best: HashMap<String, ParentEdge> = HashMap::new();
    for edge in edges {
        for (id, parent) in [
            (&edge.target_id, &edge.source_id),
            (&edge.source_id, &edge.target_id),
        ] {
            let Some(&id_dist) = distances.get(id) else {
                continue;
            };
            let Some(&parent_dist) = distances.get(parent) else {
                continue;
            };
            // Only a strictly-closer neighbor is a BFS parent candidate —
            // this is what makes the walk in `resolve_seed_ancestor` below
            // terminate: distance strictly decreases at every step.
            if parent_dist + 1 != id_dist {
                continue;
            }
            let weight = crate::scorer::graph_relation_activation_weight(&edge.relation)
                * edge.weight.clamp(0.0, 1.0);
            let is_new_best = match best.get(id) {
                Some(current) => weight > current.weight,
                None => true,
            };
            if is_new_best {
                best.insert(
                    id.clone(),
                    ParentEdge {
                        parent_id: parent.clone(),
                        via_edge: edge.relation.clone(),
                        weight,
                    },
                );
            }
        }
    }
    best
}

/// Walk the parent chain [`strongest_parent_edges`] built, from `id` back to
/// the seed it ultimately traces to (the entry in `distances` with
/// distance 0). Bounded by `distances[id] + 1` steps: each step strictly
/// decreases distance, so the walk always terminates at a seed — `None`
/// only if the parent map is missing a link (defensive; should not happen
/// for any id `strongest_parent_edges` was given).
fn resolve_seed_ancestor(
    id: &str,
    parents: &HashMap<String, ParentEdge>,
    distances: &HashMap<String, u32>,
) -> Option<String> {
    let mut current = id.to_string();
    let max_steps = distances.get(id).copied().unwrap_or(0) as usize + 1;
    for _ in 0..max_steps {
        if distances.get(&current).copied() == Some(0) {
            return Some(current);
        }
        current = parents.get(&current)?.parent_id.clone();
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
    // tachi#1647 2a: computed once against the full traversal (`edges` +
    // `distances` cover every node the BFS visited, not just the entries
    // that survive the noise/surface filters below), then looked up per
    // entry in the map closure further down.
    let parent_edges = strongest_parent_edges(&expand_result.edges, &expand_result.distances);

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
            // tachi#1647 2a: attribution for this entry, if the BFS parent
            // chain resolved one — it always does for a genuinely
            // BFS-discovered id (see `strongest_parent_edges` /
            // `resolve_seed_ancestor` docs above).
            let graph_provenance = parent_edges.get(&entry.id).and_then(|parent| {
                resolve_seed_ancestor(&entry.id, &parent_edges, &expand_result.distances).map(
                    |from_id| GraphInjectionProvenance {
                        via_edge: parent.via_edge.clone(),
                        from_id,
                        activation: activation as f32,
                        distance: u8::try_from(distance).unwrap_or(u8::MAX),
                    },
                )
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
