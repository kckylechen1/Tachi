use super::*;

#[test]
fn hybrid_symbolic_candidates_can_seed_path_scoped_short_technical_terms() {
    let mut conn = setup();
    let mut target = memory_entry(
        "clean-cli-memory",
        "The tachi-server CLI clean bridge defaults to dry-run and requires --force for deletion.",
        &["clean-cli", "target-clean", "dry-run"],
    );
    target.path = "/scratch/tachi/clean-cli-integration".to_string();
    insert_entry(&mut conn, target);

    let mut other = memory_entry(
        "other-clean-memory",
        "Another cleanup note mentions dry-run but belongs elsewhere.",
        &["cleanup", "dry-run"],
    );
    other.path = "/scratch/other".to_string();
    insert_entry(&mut conn, other);

    let opts = SearchOptions {
        top_k: 3,
        candidates_per_channel: 0,
        path_prefix: Some("/scratch/tachi/clean-cli-integration".to_string()),
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "dry-run", &opts).unwrap();
    assert_eq!(results[0].entry.id, "clean-cli-memory");
    assert!(results[0].score.symbolic > 0.0);
}

#[test]
fn hybrid_symbolic_candidates_rank_exact_probe_token_above_siblings() {
    let mut conn = setup();
    insert(
        &mut conn,
        "alpha",
        "RECALL_PROBE_ALPHA_20260607 clean-cli bridge dry-run force-delete subcommands",
        &["recall-probe", "clean-cli", "dry-run"],
    );
    insert(
        &mut conn,
        "beta",
        "RECALL_PROBE_BETA_20260607 cleanup defaults preview before deletion",
        &["recall-probe", "cleanup"],
    );
    insert(
        &mut conn,
        "delta",
        "RECALL_PROBE_DELTA_20260607 profile routing requested_profile tool_profile",
        &["recall-probe", "profile"],
    );

    let opts = SearchOptions {
        top_k: 3,
        candidates_per_channel: 0,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "RECALL_PROBE_ALPHA_20260607", &opts).unwrap();
    assert_eq!(results[0].entry.id, "alpha");
    assert!(results[0].score.symbolic > results[1].score.symbolic);
}
