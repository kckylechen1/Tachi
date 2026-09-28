//! Audit B4: hybrid search fetches candidate rows without embeddings, decodes
//! them only for the MMR frontier and the returned rows, and reuses the rows
//! the typo-fallback activation gate already fetched. Everything observable
//! must match the eager path (every row fetched with its embedding, and a
//! full re-fetch plus supersession lookup in the rank phase).

use super::*;
use crate::scorer::DecayPolicy;
use std::sync::Arc;

const DIM: usize = 1024;

/// Time-independent decay, so two searches score bit-identically. The only
/// difference between the two instances is whether hybrid search must keep
/// every candidate's embedding loaded while ranking.
struct ConstDecay {
    reads_vector: bool,
}

impl DecayPolicy for ConstDecay {
    fn score_decay(
        &self,
        _entry: &MemoryEntry,
        _recall_config: &crate::RecallConfig,
        _access_ages: Option<&[f64]>,
    ) -> f64 {
        0.5
    }

    fn reads_entry_vector(&self) -> bool {
        self.reads_vector
    }
}

/// Four near-orthogonal clusters; members of one cluster have cosine > 0.95,
/// so MMR defers most same-cluster rows and walks a long frontier.
fn cluster_vector(cluster: usize, member: usize) -> Vec<f32> {
    let mut vector = vec![0.0_f32; DIM];
    vector[cluster * 10] = 1.0;
    vector[cluster * 10 + 1 + member % 7] = 0.2;
    vector
}

fn require_vec_table(conn: &Connection) {
    let has_vec: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_vec'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert!(
        has_vec > 0,
        "sqlite-vec required for this test (memories_vec missing after setup())"
    );
}

fn seed(conn: &mut Connection) {
    let words = ["orbital", "memory", "notes", "beacon", "ledger"];
    for i in 0..48 {
        let cluster = i % 4;
        let mut entry = memory_entry(
            &format!("orbit-{i:02}"),
            &format!(
                "orbital {} {} alpha record number {i}",
                words[i % words.len()],
                words[(i / 3) % words.len()]
            ),
            &["orbital", words[i % words.len()]],
        );
        entry.timestamp = format!("2026-01-{:02}T00:00:00+00:00", 1 + i % 28);
        entry.importance = 0.5 + (i % 5) as f64 / 10.0;
        // A few rows without an embedding: MMR must treat them as before.
        if i % 11 != 5 {
            entry.vector = Some(cluster_vector(cluster, i));
        }
        upsert(conn, &entry, true).unwrap();
    }
    crate::db::supersede_memory(conn, "orbit-03", "orbit-07").unwrap();
}

fn search_both(
    conn: &Connection,
    query: &str,
    base: impl Fn() -> SearchOptions,
) -> (Vec<SearchResult>, Vec<SearchResult>) {
    let deferred_opts = SearchOptions {
        decay_policy: Some(Arc::new(ConstDecay {
            reads_vector: false,
        })),
        ..base()
    };
    let eager_opts = SearchOptions {
        decay_policy: Some(Arc::new(ConstDecay { reads_vector: true })),
        ..base()
    };
    assert!(!ranking_reads_entry_vectors(&deferred_opts));
    assert!(ranking_reads_entry_vectors(&eager_opts));
    (
        hybrid_search(conn, query, &deferred_opts).unwrap(),
        hybrid_search(conn, query, &eager_opts).unwrap(),
    )
}

fn assert_same_results(context: &str, deferred: &[SearchResult], eager: &[SearchResult]) {
    let ids = |results: &[SearchResult]| {
        results
            .iter()
            .map(|result| result.entry.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(deferred), ids(eager), "{context}: order");
    for (d, e) in deferred.iter().zip(eager) {
        assert_eq!(
            format!("{:?}", d.score),
            format!("{:?}", e.score),
            "{context}: score of {}",
            d.entry.id
        );
        assert_eq!(
            d.entry.vector, e.entry.vector,
            "{context}: returned rows must carry their stored embedding ({})",
            d.entry.id
        );
        assert_eq!(d.graph_injected, e.graph_injected, "{context}");
    }
}

#[test]
fn deferred_embedding_hydration_matches_eager_fetch() {
    let mut conn = setup();
    require_vec_table(&conn);
    seed(&mut conn);

    let mut query_vec = cluster_vector(0, 0);
    query_vec[10] = 0.9;
    let queries = [
        // Not typo-eligible: plain bulk fetch.
        "orbital alpha",
        // Typo-eligible (4 alphabetic terms): the gate's rows are reused.
        "orbital memory notes beacon",
        // Typo-eligible and misspelled: without a vector leg, fallback
        // activates and contributes typo-only rows.
        "orbtal alpah recrod numbr",
    ];
    for query in queries {
        for with_vector in [false, true] {
            for (top_k, mmr_threshold) in [
                (1, Some(0.85)),
                (6, Some(0.85)),
                (30, Some(0.85)),
                (6, None),
                (60, Some(0.99)),
            ] {
                for include_superseded in [false, true] {
                    let base = || SearchOptions {
                        top_k,
                        candidates_per_channel: 20,
                        record_access: false,
                        mmr_threshold,
                        vec_available: true,
                        query_vec: with_vector.then(|| query_vec.clone()),
                        include_superseded,
                        ..Default::default()
                    };
                    let (deferred, eager) = search_both(&conn, query, base);
                    let context = format!(
                        "query={query:?} vec={with_vector} top_k={top_k} mmr={mmr_threshold:?} superseded={include_superseded}"
                    );
                    assert_same_results(&context, &deferred, &eager);
                    if with_vector {
                        assert!(
                            deferred.iter().any(|r| r.entry.vector.is_some()),
                            "{context}: fixture must return embedded rows"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn reused_typo_gate_rows_rank_like_a_full_refetch() {
    let mut conn = setup();
    require_vec_table(&conn);
    seed(&mut conn);

    // The attribution twin re-fetches every candidate row and looks up
    // supersession for all of them in the rank phase; production reuses the
    // typo gate's rows, symbolic scores and supersession set.
    for query in [
        "orbital memory notes beacon",
        "orbtal alpah recrod numbr",
        "persistant orbital ledgr alpha",
    ] {
        let opts = SearchOptions {
            top_k: 10,
            candidates_per_channel: 20,
            record_access: false,
            mmr_threshold: None,
            decay_policy: Some(Arc::new(ConstDecay {
                reads_vector: false,
            })),
            ..Default::default()
        };
        let (results, attribution) = hybrid_search_with_attribution(&conn, query, &opts).unwrap();
        assert!(!results.is_empty(), "{query:?}: fixture must return rows");
        let mut expected: Vec<(&String, f64)> = attribution
            .final_scores
            .iter()
            .map(|(id, score)| (id, *score))
            .collect();
        expected.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        for result in &results {
            let twin = attribution
                .final_scores
                .get(&result.entry.id)
                .unwrap_or_else(|| panic!("{query:?}: {} missing from twin", result.entry.id));
            assert_eq!(
                result.score.final_score.to_bits(),
                twin.to_bits(),
                "{query:?}: {}",
                result.entry.id
            );
        }
        assert_eq!(
            results.len(),
            expected.len().min(10),
            "{query:?}: same survivor count as the full re-fetch"
        );
        assert!(
            results.iter().all(|r| r.entry.id != "orbit-03"),
            "{query:?}: superseded row must stay hidden"
        );
    }
}
