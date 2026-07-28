//! #1242: raw-tier vector similarity floor drops weak vec-channel hits only.

use super::*;
use crate::db::upsert;
use crate::search::candidates::collect_candidates;
use crate::RecallConfig;

fn insert_with_vector(
    conn: &mut Connection,
    id: &str,
    text: &str,
    keywords: &[&str],
    tier: &str,
    vector: Vec<f32>,
) {
    let mut entry = memory_entry(id, text, keywords);
    entry.tier = tier.to_string();
    entry.vector = Some(vector);
    upsert(conn, &entry, true).unwrap();
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

#[test]
fn raw_below_floor_dropped_from_vector_channel_but_reachable_via_fts() {
    let mut conn = setup();
    require_vec_table(&conn);

    const DIM: usize = 1024;
    let query_vec = vec![1.0_f32; DIM];
    let weak_doc_vec = vec![-1.0_f32; DIM];
    let strong_doc_vec = query_vec.clone();

    insert_with_vector(
        &mut conn,
        "raw-weak",
        "quasar nebula vector floor probe",
        &["quasar", "nebula"],
        "raw",
        weak_doc_vec,
    );
    insert_with_vector(
        &mut conn,
        "raw-strong",
        "quasar beacon vector floor strong",
        &["quasar", "beacon"],
        "raw",
        strong_doc_vec,
    );

    let floor = RecallConfig::default().raw_vector_similarity_floor;
    let opts = SearchOptions {
        top_k: 5,
        candidates_per_channel: 20,
        vec_available: true,
        query_vec: Some(query_vec),
        record_access: false,
        recall_config: Some(RecallConfig {
            raw_vector_similarity_floor: floor,
            ..RecallConfig::default()
        }),
        ..Default::default()
    };

    let (candidates, _) =
        collect_candidates(&conn, "quasar nebula probe", &opts, false, None, false).unwrap();

    assert!(
        !candidates.vec_scores.contains_key("raw-weak"),
        "raw row below similarity floor must be absent from vec_scores, got {:?}",
        candidates.vec_scores
    );
    assert!(
        candidates.vec_scores.contains_key("raw-strong"),
        "raw row above floor must remain in vec_scores"
    );
    assert!(
        candidates.fts_scores.contains_key("raw-weak"),
        "below-floor raw row must still surface via FTS"
    );
}

#[test]
fn consolidated_tier_unaffected_by_raw_vector_floor() {
    // Regression guard only: this assertion also holds on pre-#1242 base
    // (no raw floor existed). Not claimed as discrimination evidence.
    let mut conn = setup();
    require_vec_table(&conn);

    const DIM: usize = 1024;
    let query_vec = vec![1.0_f32; DIM];
    let weak_doc_vec = vec![-1.0_f32; DIM];

    insert_with_vector(
        &mut conn,
        "consolidated-weak",
        "consolidated quasar nebula probe",
        &["quasar"],
        "consolidated",
        weak_doc_vec,
    );

    let opts = SearchOptions {
        top_k: 5,
        candidates_per_channel: 20,
        vec_available: true,
        query_vec: Some(query_vec),
        record_access: false,
        recall_config: Some(RecallConfig::default()),
        ..Default::default()
    };

    let (candidates, _) =
        collect_candidates(&conn, "quasar nebula", &opts, false, None, false).unwrap();

    assert!(
        candidates.vec_scores.contains_key("consolidated-weak"),
        "non-raw tiers must not be filtered by raw_vector_similarity_floor"
    );
}

fn vector_only_floor_opts(query_vec: Vec<f32>, record_access: bool) -> SearchOptions {
    SearchOptions {
        top_k: 5,
        candidates_per_channel: 20,
        vec_available: true,
        query_vec: Some(query_vec),
        record_access,
        mmr_threshold: None,
        recall_config: Some(RecallConfig {
            raw_vector_similarity_floor: 0.0,
            vector_only_similarity_floor: 0.40,
            ..RecallConfig::default()
        }),
        ..Default::default()
    }
}

fn weak_vector_query_and_document() -> (Vec<f32>, Vec<f32>) {
    const DIM: usize = 1024;
    // sqlite-vec's L2 conversion is `similarity = 1 - distance / 2`.
    // 0.5 -> 0.54 in every dimension has distance 1.28, i.e. similarity 0.36.
    (vec![0.5; DIM], vec![0.54; DIM])
}

#[test]
fn vector_only_rows_below_floor_are_dropped_in_every_tier_before_access_or_recording() {
    let mut conn = setup();
    require_vec_table(&conn);
    let (query_vec, weak_doc_vec) = weak_vector_query_and_document();
    let query = "lunar canary query";

    for (id, tier, text, keyword) in [
        (
            "raw-vector-only",
            "raw",
            "saffron basalt archive",
            "saffron",
        ),
        (
            "consolidated-vector-only",
            "consolidated",
            "cobalt granite ledger",
            "cobalt",
        ),
        (
            "pattern-vector-only",
            "pattern",
            "umber marble notebook",
            "umber",
        ),
    ] {
        insert_with_vector(&mut conn, id, text, &[keyword], tier, weak_doc_vec.clone());
    }

    let opts = vector_only_floor_opts(query_vec, true);
    let (candidates, _) = collect_candidates(&conn, query, &opts, false, None, false).unwrap();
    for id in [
        "raw-vector-only",
        "consolidated-vector-only",
        "pattern-vector-only",
    ] {
        let similarity = *candidates.vec_scores.get(id).unwrap_or_else(|| {
            panic!(
                "test precondition requires vector candidate {id}; got {:?}",
                candidates.vec_scores
            )
        });
        assert!(
            (0.35..0.40).contains(&similarity),
            "test precondition requires a weak vector-only similarity, got {similarity} for {id}"
        );
        assert!(
            !candidates.fts_scores.contains_key(id),
            "test precondition requires no FTS evidence for {id}"
        );
    }

    let (results, receipt) = hybrid_search_with_receipt(&conn, query, &opts).unwrap();
    assert!(
        results.is_empty(),
        "weak vector-only rows must not be returned"
    );
    assert!(
        receipt
            .rank
            .expect("rank phase ran")
            .get_access_times
            .is_none(),
        "the floor must remove every weak vector-only row before access-history reads"
    );
    assert_eq!(
        receipt
            .access_recording
            .expect("record_access=true runs the recording phase")
            .updated_row_count,
        0,
        "filtered rows must not be recorded as displayed or scored"
    );
    for id in [
        "raw-vector-only",
        "consolidated-vector-only",
        "pattern-vector-only",
    ] {
        let counts: (i64, i64) = conn
            .query_row(
                "SELECT access_count, scored_count FROM memories WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (0, 0), "filtered row {id} must remain unrecorded");
    }
}

#[test]
fn vector_only_row_at_or_above_floor_survives() {
    let mut conn = setup();
    require_vec_table(&conn);
    let (query_vec, _) = weak_vector_query_and_document();
    let strong_doc_vec = vec![0.53; 1024];
    insert_with_vector(
        &mut conn,
        "strong-vector-only",
        "saffron basalt archive",
        &["saffron"],
        "pattern",
        strong_doc_vec,
    );

    let opts = vector_only_floor_opts(query_vec, false);
    let results = hybrid_search(&conn, "lunar canary query", &opts).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry.id, "strong-vector-only");
    assert!(
        results[0].score.vector >= 0.40,
        "vector similarity at or above the floor must survive"
    );
}

#[test]
fn fts_symbolic_and_exact_id_evidence_bypass_vector_only_floor() {
    let mut conn = setup();
    require_vec_table(&conn);
    let (query_vec, weak_doc_vec) = weak_vector_query_and_document();

    insert_with_vector(
        &mut conn,
        "fts-evidence",
        "eclipse lexical evidence",
        &[],
        "consolidated",
        weak_doc_vec.clone(),
    );
    let fts_opts = vector_only_floor_opts(query_vec.clone(), false);
    let (fts_candidates, _) =
        collect_candidates(&conn, "eclipse", &fts_opts, false, None, false).unwrap();
    assert!(fts_candidates.fts_scores.contains_key("fts-evidence"));
    assert!(
        hybrid_search(&conn, "eclipse", &fts_opts)
            .unwrap()
            .iter()
            .any(|result| result.entry.id == "fts-evidence"),
        "FTS evidence must bypass the vector-only floor"
    );

    let mut symbolic = memory_entry("symbolic-evidence", "unrelated archive", &[]);
    symbolic.topic = "topicalpha".into();
    symbolic.tier = "pattern".into();
    symbolic.vector = Some(weak_doc_vec.clone());
    upsert(&mut conn, &symbolic, true).unwrap();
    let symbolic_opts = vector_only_floor_opts(query_vec.clone(), false);
    let (symbolic_candidates, _) =
        collect_candidates(&conn, "topicalpha", &symbolic_opts, false, None, false).unwrap();
    assert!(
        !symbolic_candidates
            .fts_scores
            .contains_key("symbolic-evidence"),
        "topic-only evidence must not accidentally become FTS evidence"
    );
    assert!(crate::scorer::symbolic_score_entry("topicalpha", &symbolic) > 0.0);
    assert!(
        hybrid_search(&conn, "topicalpha", &symbolic_opts)
            .unwrap()
            .iter()
            .any(|result| result.entry.id == "symbolic-evidence"),
        "symbolic evidence must bypass the vector-only floor"
    );

    let exact_id = "2cf77e24-568f-4bc9-b7a8-7a3e9c2a9981";
    insert_with_vector(
        &mut conn,
        exact_id,
        "unique exact id archive",
        &[],
        "raw",
        weak_doc_vec,
    );
    let exact_opts = vector_only_floor_opts(query_vec, false);
    let (exact_candidates, _) =
        collect_candidates(&conn, exact_id, &exact_opts, false, None, false).unwrap();
    assert_eq!(exact_candidates.exact_id.as_deref(), Some(exact_id));
    let exact_results = hybrid_search(&conn, exact_id, &exact_opts).unwrap();
    assert!(
        exact_results
            .iter()
            .any(|result| result.entry.id == exact_id),
        "exact-id queries must bypass the vector-only floor; got {:?}",
        exact_results
            .iter()
            .map(|result| result.entry.id.as_str())
            .collect::<Vec<_>>()
    );
}
