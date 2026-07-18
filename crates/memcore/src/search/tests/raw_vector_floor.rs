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
