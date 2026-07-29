use super::super::candidates::collect_candidates;
use super::*;
use super::{golden_corpus, ops_audit_corpus};
use std::time::Duration;

const TYPO_QUERY: &str = "persistant memroy retrival boundries";
const INTENDED_ID: &str = "typo-intended";
const NEAR_ID: &str = "typo-near-neighbor";

fn seed_typo_corpus(conn: &mut Connection) {
    insert(
        conn,
        INTENDED_ID,
        "Persistent memory retrieval boundaries keep candidate recovery separate from final ranking policy",
        &["persistent", "memory", "retrieval", "boundaries"],
    );
    insert(
        conn,
        NEAR_ID,
        "Resistant memory revival boundaries describe a nearby but different operational rule",
        &["resistant", "memory", "revival", "boundaries"],
    );
    insert(
        conn,
        "typo-unrelated",
        "Deployment receipts record service health after a release",
        &["deployment", "receipt", "health"],
    );
}

fn typo_opts() -> SearchOptions {
    SearchOptions {
        candidates_per_channel: 32,
        top_k: 10,
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    }
}

fn enabled_config() -> RecallConfig {
    RecallConfig::default()
}

fn disabled_config() -> RecallConfig {
    let mut config = RecallConfig::default();
    config.typo_fallback.enabled = false;
    config
}

fn result_ids(results: Vec<SearchResult>) -> Vec<String> {
    results.into_iter().map(|result| result.entry.id).collect()
}

fn assert_work_within_config(receipt: &TypoFallbackPhaseReceipt, config: &RecallConfig) {
    let budget = &config.typo_fallback;
    assert!(
        receipt.prefilter_candidate_count <= budget.prefilter_candidate_limit,
        "prefilter cardinality exceeded named budget: {receipt:?}"
    );
    assert!(receipt.compared_candidate_count <= receipt.prefilter_candidate_count);
    assert!(receipt.contributed_candidate_count <= budget.max_candidates);
    assert!(
        receipt.token_comparison_count
            <= receipt
                .compared_candidate_count
                .saturating_mul(budget.max_query_terms)
                .saturating_mul(budget.max_candidate_tokens),
        "token comparisons exceeded the deterministic product budget: {receipt:?}"
    );
    assert!(
        receipt.edit_cell_count
            <= receipt
                .token_comparison_count
                .saturating_mul((budget.max_token_chars + 1).pow(2)),
        "edit cells exceeded the deterministic matrix budget: {receipt:?}"
    );
}

#[test]
fn all_misspelled_four_word_case_is_absent_from_normal_legs_then_recovers() {
    let mut conn = setup();
    seed_typo_corpus(&mut conn);
    let observed = vec![INTENDED_ID.to_string(), NEAR_ID.to_string()];

    let (_, evidence) =
        hybrid_search_with_candidate_leg_evidence(&conn, TYPO_QUERY, &typo_opts(), &observed)
            .unwrap();

    let intended_normal = evidence.get(INTENDED_ID).expect("intended evidence");
    assert_eq!(
        *intended_normal,
        CandidateLegEvidence::default(),
        "RED discriminator: intended row must be absent from every normal candidate leg"
    );

    let (results, receipt) = hybrid_search_with_receipt(&conn, TYPO_QUERY, &typo_opts()).unwrap();
    let candidates = receipt.candidates.expect("candidate phase receipt");
    let typo = candidates
        .typo_fallback
        .expect("four-word weak query must activate typo fallback");
    assert!(typo.elapsed <= candidates.total_elapsed);
    assert!(
        typo.prefilter_candidate_count >= 2,
        "reviewed intended and near-neighbor rows must reach the bounded prefilter: {typo:?}"
    );
    assert!(
        typo.contributed_candidate_count >= 2,
        "reviewed intended and near-neighbor rows must pass bounded character recovery: {typo:?}"
    );
    assert_work_within_config(&typo, &enabled_config());
    println!(
        "typo_fallback_reviewed_report prefilter_candidates={} compared_candidates={} contributed_candidates={} token_comparisons={} edit_cells={} fallback_elapsed_us={} candidate_elapsed_us={}",
        typo.prefilter_candidate_count,
        typo.compared_candidate_count,
        typo.contributed_candidate_count,
        typo.token_comparison_count,
        typo.edit_cell_count,
        typo.elapsed.as_micros(),
        candidates.total_elapsed.as_micros(),
    );

    let ids = results
        .iter()
        .map(|result| result.entry.id.as_str())
        .collect::<Vec<_>>();
    let intended_rank = ids.iter().position(|id| *id == INTENDED_ID);
    let near_rank = ids.iter().position(|id| *id == NEAR_ID);
    assert!(
        intended_rank.is_some(),
        "typo fallback must recover the intended fact; ids={ids:?}"
    );
    assert!(
        near_rank.is_none_or(|rank| intended_rank.unwrap() < rank),
        "negative near-neighbor must stay below the intended fact; ids={ids:?}"
    );
}

#[test]
fn typo_evidence_rescues_a_vector_candidate_in_the_normal_leg_dead_band() {
    let mut conn = setup();
    seed_typo_corpus(&mut conn);
    const DIM: usize = 1024;
    let query_vec = vec![0.5_f32; DIM];
    // sqlite-vec converts L2 distance to `1 - distance / 2`. This document
    // lands above the old typo activation gate (0.40) but below the default
    // vector-only survival floor (0.445).
    let mut intended = memory_entry(
        INTENDED_ID,
        "Persistent memory retrieval boundaries keep candidate recovery separate from final ranking policy",
        &["persistent", "memory", "retrieval", "boundaries"],
    );
    intended.vector = Some(vec![0.53625_f32; DIM]);
    upsert(&mut conn, &intended, true).unwrap();

    let config = RecallConfig::default();
    let opts = SearchOptions {
        candidates_per_channel: 32,
        top_k: 10,
        query_vec: Some(query_vec),
        vec_available: true,
        record_access: false,
        mmr_threshold: None,
        recall_config: Some(config.clone()),
        ..Default::default()
    };
    let (candidates, _) =
        collect_candidates(&conn, TYPO_QUERY, &opts, false, None, false, None).unwrap();
    let similarity = candidates.vec_scores[INTENDED_ID];
    assert!(
        similarity > 0.40 && similarity < config.vector_only_similarity_floor,
        "test precondition requires the historical activation/survival dead band, got {similarity}"
    );

    let (results, receipt) = hybrid_search_with_receipt(&conn, TYPO_QUERY, &opts).unwrap();
    let typo = receipt
        .candidates
        .expect("candidate phase receipt")
        .typo_fallback
        .expect("weak normal vector evidence must not suppress typo recovery");
    assert!(typo.contributed_candidate_count > 0);
    assert_eq!(
        results.first().map(|result| result.entry.id.as_str()),
        Some(INTENDED_ID),
        "character evidence must rescue the intended weak-vector candidate"
    );
}

#[test]
fn weak_normal_fts_and_symbolic_overlap_do_not_suppress_typo_recovery() {
    let mut conn = setup();
    const QUERY: &str = "persistant memory retrival boundries";
    const INTENDED: &str = "weak-normal-intended";
    const WEAK: &str = "weak-normal-one-term";
    insert(
        &mut conn,
        INTENDED,
        "Persistent memory retrieval boundaries define the intended recovery rule",
        &["persistent", "memory", "retrieval", "boundaries"],
    );
    insert(
        &mut conn,
        WEAK,
        "Memory deployment receipts are unrelated operational evidence",
        &["memory", "deployment", "receipt"],
    );
    let opts = typo_opts();

    let mut disabled_opts = typo_opts();
    disabled_opts.recall_config = Some(disabled_config());
    let disabled = result_ids(hybrid_search(&conn, QUERY, &disabled_opts).unwrap());
    assert!(
        !disabled.iter().any(|id| id == INTENDED || id == WEAK),
        "one weak normal lexical overlap must not pass the final rich-query gate: {disabled:?}"
    );

    let (enabled, receipt) = hybrid_search_with_receipt(&conn, QUERY, &opts).unwrap();
    let typo = receipt
        .candidates
        .expect("candidate receipt")
        .typo_fallback
        .expect("normal rows that all fail final eligibility must activate fallback");
    assert!(typo.contributed_candidate_count >= 1);
    assert_eq!(
        enabled.first().map(|result| result.entry.id.as_str()),
        Some(INTENDED),
        "fallback must recover the intended row despite weak FTS/symbolic normal legs"
    );
}

#[test]
fn minimum_typo_score_is_explicit_retrieval_evidence_not_symbolic_dead_band() {
    let mut conn = setup();
    const QUERY: &str = "persxxt restxxe arxxive hixxory";
    const INTENDED: &str = "minimum-typo-evidence";
    insert(
        &mut conn,
        INTENDED,
        "persist restore archive history",
        &["persist", "restore", "archive", "history"],
    );
    let opts = typo_opts();
    let (candidates, _) =
        collect_candidates(&conn, QUERY, &opts, false, None, false, None).unwrap();
    let score = candidates.typo_scores[INTENDED];
    assert!(
        (0.455..0.5).contains(&score),
        "fixture must land in the historical fallback-score / symbolic-coverage dead band: {score}"
    );

    let (results, receipt) = hybrid_search_with_receipt(&conn, QUERY, &opts).unwrap();
    assert!(
        receipt
            .candidates
            .expect("candidate receipt")
            .typo_fallback
            .is_some(),
        "minimum accepted typo assignment must execute the fallback"
    );
    assert_eq!(
        results.first().map(|result| result.entry.id.as_str()),
        Some(INTENDED),
        "typed typo evidence must survive the final retrieval gate"
    );
}

#[test]
fn one_candidate_token_cannot_satisfy_two_misspelled_query_terms() {
    let mut conn = setup();
    const QUERY: &str = "memroy memmory retrival boundries";
    const ONLY_ONE: &str = "one-memory-occurrence";
    insert(
        &mut conn,
        ONLY_ONE,
        "memory retrieval boundaries archive",
        &["memory", "retrieval", "boundaries", "archive"],
    );
    let (results, receipt) = hybrid_search_with_receipt(&conn, QUERY, &typo_opts()).unwrap();
    let typo = receipt
        .candidates
        .expect("candidate receipt")
        .typo_fallback
        .expect("eligible misspelled query must exercise the bounded matcher");
    assert_eq!(
        typo.contributed_candidate_count, 0,
        "one `memory` occurrence must not satisfy both misspelled memory terms"
    );
    assert!(
        !results.iter().any(|result| result.entry.id == ONLY_ONE),
        "many-to-one token reuse must not recover the row"
    );
}

#[test]
fn structured_metadata_survives_a_long_free_text_budget() {
    let mut conn = setup();
    const STRUCTURED: &str = "structured-typo-target";
    let mut entry = memory_entry(
        STRUCTURED,
        &"incidental prose ".repeat(1_024),
        &["persistent", "memory", "retrieval", "boundaries"],
    );
    entry.summary = "incidental prose".to_string();
    entry.topic = "persistent memory retrieval boundaries".to_string();
    insert_entry(&mut conn, entry);
    let mut config = enabled_config();
    config.typo_fallback.max_candidate_chars = 256;
    config.typo_fallback.max_structured_field_chars = 64;
    let opts = SearchOptions {
        recall_config: Some(config),
        ..typo_opts()
    };

    let (results, receipt) = hybrid_search_with_receipt(&conn, TYPO_QUERY, &opts).unwrap();
    assert!(
        receipt
            .candidates
            .expect("candidate receipt")
            .typo_fallback
            .is_some(),
        "structured evidence must reach the matcher even when body text exceeds its budget"
    );
    assert_eq!(
        results.first().map(|result| result.entry.id.as_str()),
        Some(STRUCTURED),
        "topic/keywords must be evaluated before independently bounded free text"
    );
}

#[test]
fn short_cjk_uuid_and_identifier_queries_preserve_exact_behavior_without_activation() {
    let mut conn = setup();
    seed_typo_corpus(&mut conn);
    insert(
        &mut conn,
        "cjk-exact",
        "持久记忆检索边界保持候选恢复与最终排序分离",
        &["持久记忆", "检索边界"],
    );
    const UUID: &str = "8f9a2fb0-9b72-4a7f-a8b1-d20f4bf65d44";
    insert(&mut conn, UUID, "exact UUID identity", &["identity"]);
    insert(
        &mut conn,
        "identifier-exact",
        "RECALL_PROBE_TYPO_BOUNDARY_20260729",
        &["recall-probe"],
    );

    for (query, expected) in [
        ("memroy", None),
        ("持久记忆检索边界", Some("cjk-exact")),
        (UUID, Some(UUID)),
        (
            "RECALL_PROBE_TYPO_BOUNDARY_20260729",
            Some("identifier-exact"),
        ),
    ] {
        let opts = SearchOptions {
            top_k: 10,
            candidates_per_channel: 32,
            record_access: false,
            mmr_threshold: None,
            recall_config: Some(enabled_config()),
            ..Default::default()
        };
        let (results, receipt) = hybrid_search_with_receipt(&conn, query, &opts).unwrap();
        assert!(
            receipt
                .candidates
                .expect("candidate receipt")
                .typo_fallback
                .is_none(),
            "ineligible query must not activate fallback: {query}"
        );
        if let Some(expected) = expected {
            assert_eq!(
                results.first().map(|result| result.entry.id.as_str()),
                Some(expected),
                "exact semantics changed for {query}"
            );
        }
    }
}

#[test]
fn strong_fts_symbolic_and_vector_cases_do_not_activate_or_change_order() {
    let mut conn = setup();
    seed_typo_corpus(&mut conn);

    let run = |conn: &Connection, query: &str, mut opts: SearchOptions| {
        opts.recall_config = Some(enabled_config());
        let (enabled, receipt) = hybrid_search_with_receipt(conn, query, &opts).unwrap();
        let typo = receipt.candidates.expect("candidate receipt").typo_fallback;
        assert!(typo.is_none(), "strong normal query activated: {query}");
        opts.recall_config = Some(disabled_config());
        let disabled = hybrid_search(conn, query, &opts).unwrap();
        assert_eq!(
            result_ids(enabled),
            result_ids(disabled),
            "fallback changed strong ordering for {query}"
        );
    };

    run(
        &conn,
        "persistent memory retrieval boundaries",
        SearchOptions {
            top_k: 10,
            candidates_per_channel: 32,
            record_access: false,
            mmr_threshold: None,
            ..Default::default()
        },
    );

    insert(
        &mut conn,
        "symbolic-only-strong",
        "controlplane rehome ledger proof",
        &[],
    );
    run(
        &conn,
        "controlplane rehome ledger proof",
        SearchOptions {
            top_k: 3,
            candidates_per_channel: 0,
            record_access: false,
            mmr_threshold: None,
            ..Default::default()
        },
    );

    let has_vec: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_vec'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert!(
        has_vec > 0,
        "sqlite-vec is required for the strong vector case"
    );
    const DIM: usize = 1024;
    let query_vec = vec![1.0_f32; DIM];
    let mut vector_target = memory_entry(
        "vector-only-strong",
        "saffron basalt archive unrelated words",
        &[],
    );
    vector_target.vector = Some(query_vec.clone());
    crate::db::upsert(&mut conn, &vector_target, true).unwrap();
    run(
        &conn,
        "lunar canary vector probe",
        SearchOptions {
            top_k: 3,
            candidates_per_channel: 20,
            query_vec: Some(query_vec),
            vec_available: true,
            record_access: false,
            mmr_threshold: None,
            ..Default::default()
        },
    );
}

#[derive(Default)]
struct CorpusMeasurement {
    queries: usize,
    activations: usize,
    contributed_candidates: usize,
    token_comparisons: usize,
    edit_cells: usize,
    fallback_elapsed: Duration,
    candidate_elapsed: Duration,
}

fn measure_query(
    conn: &Connection,
    query: &str,
    enabled_opts: SearchOptions,
    disabled_opts: SearchOptions,
    measurement: &mut CorpusMeasurement,
) {
    let config = enabled_opts
        .recall_config
        .as_ref()
        .expect("enabled corpus config");
    let (enabled, receipt) = hybrid_search_with_receipt(conn, query, &enabled_opts).unwrap();
    let disabled = hybrid_search(conn, query, &disabled_opts).unwrap();
    assert_eq!(
        result_ids(enabled),
        result_ids(disabled),
        "default typo fallback changed existing corpus order for query `{query}`"
    );
    measurement.queries += 1;
    let candidates = receipt.candidates.expect("candidate receipt");
    measurement.candidate_elapsed += candidates.total_elapsed;
    if let Some(typo) = candidates.typo_fallback {
        assert_work_within_config(&typo, config);
        measurement.activations += 1;
        measurement.contributed_candidates += typo.contributed_candidate_count;
        measurement.token_comparisons += typo.token_comparison_count;
        measurement.edit_cells += typo.edit_cell_count;
        measurement.fallback_elapsed += typo.elapsed;
    }
}

#[test]
fn exact_golden_and_ops_audit_corpora_preserve_order_and_measure_bounded_work() {
    let mut golden_conn = setup();
    golden_corpus::seed_corpus(&mut golden_conn);
    let mut golden = CorpusMeasurement::default();
    for spec in golden_corpus::QUERIES {
        let path_prefix =
            (spec.slice == golden_corpus::Slice::WikiScoped).then(|| "/wiki".to_string());
        let enabled_opts = SearchOptions {
            top_k: 40,
            candidates_per_channel: 128,
            record_access: false,
            mmr_threshold: None,
            path_prefix: path_prefix.clone(),
            recall_config: Some(enabled_config()),
            ..Default::default()
        };
        let disabled_opts = SearchOptions {
            top_k: 40,
            candidates_per_channel: 128,
            record_access: false,
            mmr_threshold: None,
            path_prefix,
            recall_config: Some(disabled_config()),
            ..Default::default()
        };
        measure_query(
            &golden_conn,
            spec.query,
            enabled_opts,
            disabled_opts,
            &mut golden,
        );
    }

    let mut ops_conn = setup();
    ops_audit_corpus::seed_corpus(&mut ops_conn);
    let mut ops = CorpusMeasurement::default();
    for case in ops_audit_corpus::CASES {
        let mut enabled_opts = ops_audit_corpus::search_opts(case.surface);
        enabled_opts.recall_config = Some(enabled_config());
        let mut disabled_opts = ops_audit_corpus::search_opts(case.surface);
        disabled_opts.recall_config = Some(disabled_config());
        measure_query(&ops_conn, case.query, enabled_opts, disabled_opts, &mut ops);
    }

    assert_eq!(
        golden.activations, 0,
        "typo fallback must remain dormant across the exact golden_corpus"
    );
    assert_eq!(
        ops.activations, 0,
        "typo fallback must remain dormant across the exact ops_audit_corpus"
    );

    println!(
        "typo_fallback_corpus_report corpus=golden_corpus queries={} activations={} contributed_candidates={} token_comparisons={} edit_cells={} fallback_elapsed_us={} candidate_elapsed_us={}",
        golden.queries,
        golden.activations,
        golden.contributed_candidates,
        golden.token_comparisons,
        golden.edit_cells,
        golden.fallback_elapsed.as_micros(),
        golden.candidate_elapsed.as_micros(),
    );
    println!(
        "typo_fallback_corpus_report corpus=ops_audit_corpus queries={} activations={} contributed_candidates={} token_comparisons={} edit_cells={} fallback_elapsed_us={} candidate_elapsed_us={}",
        ops.queries,
        ops.activations,
        ops.contributed_candidates,
        ops.token_comparisons,
        ops.edit_cells,
        ops.fallback_elapsed.as_micros(),
        ops.candidate_elapsed.as_micros(),
    );
}

#[test]
fn sampled_impression_records_content_free_fallback_activation_and_contribution() {
    let mut conn = setup();
    seed_typo_corpus(&mut conn);
    let mut config = enabled_config();
    config.impression_sample_rate_bps = 10_000;
    let opts = SearchOptions {
        candidates_per_channel: 32,
        top_k: 10,
        record_access: true,
        mmr_threshold: None,
        recall_config: Some(config.clone()),
        ..Default::default()
    };
    let results = hybrid_search(&conn, TYPO_QUERY, &opts).unwrap();
    assert_eq!(
        results.first().map(|result| result.entry.id.as_str()),
        Some(INTENDED_ID)
    );

    let group: (i64, i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT typo_fallback_activated,
                    typo_fallback_prefilter_count,
                    typo_fallback_compared_count,
                    typo_fallback_token_comparison_count,
                    typo_fallback_edit_cell_count,
                    typo_fallback_candidate_count
               FROM recall_impression_groups
              WHERE query_fingerprint = ?1",
            [crate::recall_impressions::query_fingerprint(TYPO_QUERY)],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(group.0, 1);
    assert!(group.1 >= 2 && group.1 <= config.typo_fallback.prefilter_candidate_limit as i64);
    assert!(group.2 >= 2 && group.2 <= group.1);
    assert!(group.3 > 0);
    assert!(group.4 > 0);
    assert!(group.5 >= 2 && group.5 <= config.typo_fallback.max_candidates as i64);
    let intended_flag: i64 = conn
        .query_row(
            "SELECT typo_fallback_candidate
               FROM recall_impressions
              WHERE memory_id = ?1",
            [INTENDED_ID],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(intended_flag, 1);

    let forbidden_columns: i64 = conn
        .query_row(
            "SELECT COUNT(*)
               FROM pragma_table_info('recall_impression_groups')
              WHERE lower(name) IN ('query', 'query_text', 'content', 'entity', 'path', 'embedding', 'cache_id')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        forbidden_columns, 0,
        "fallback telemetry must remain content-free"
    );
}
