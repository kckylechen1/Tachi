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

/// tachi#1144 kill-test: the symbolic channel's candidate cap is an
/// eligibility boundary, so relevance must choose its survivors before
/// timestamp is allowed to break a tie. The target is deliberately old and
/// all 201 newer rows match only one query token. With the old SQL
/// `ORDER BY timestamp DESC LIMIT`, the target never reached scoring.
#[test]
fn symbolic_pre_cap_relevance_recovers_older_stronger_symbolic_only_target() {
    let mut conn = setup();
    let query = "controlplane rehome ledger proof";

    let mut target = memory_entry(
        "symbolic-strongest-target",
        "controlplane rehome ledger proof",
        &[],
    );
    target.timestamp = "2020-01-01T00:00:00Z".to_string();
    insert_entry(&mut conn, target);

    for idx in 0..201 {
        let mut newer_partial = memory_entry(
            &format!("newer-partial-symbolic-{idx:03}"),
            "controlplane only partial distractor",
            &[],
        );
        newer_partial.timestamp = format!(
            "2027-{:02}-{:02}T{:02}:00:00Z",
            1 + idx / (24 * 28),
            1 + (idx / 24) % 28,
            idx % 24,
        );
        insert_entry(&mut conn, newer_partial);
    }

    let opts = SearchOptions {
        // `0` suppresses FTS (and vec is unavailable); the symbolic channel
        // still keeps one candidate because `top_k` is its minimum cap. This
        // makes the target's survival a symbolic-only eligibility proof.
        candidates_per_channel: 0,
        top_k: 1,
        weights: HybridWeights {
            semantic: 0.0,
            fts: 0.0,
            symbolic: 1.0,
            decay: 0.0,
            use_rrf: false,
        },
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    };
    let results = hybrid_search(&conn, query, &opts).expect("symbolic search succeeds");

    let recovered = results
        .first()
        .expect("the relevance oracle's target must survive the symbolic cap");
    assert_eq!(
        recovered.entry.id, "symbolic-strongest-target",
        "the older four-token target must beat 201 newer one-token partial matches before the cap"
    );
    assert_eq!(
        recovered.score.fts, 0.0,
        "FTS is deliberately disabled: this is a symbolic-only recall proof"
    );
    assert_eq!(
        recovered.score.symbolic, 1.0,
        "the returned target covers every query token"
    );
}

/// The pre-cap scorer must use the same expansion-aware symbolic query as the
/// final ranker. Raw `mcp` ties the two rows; the final scorer expands it to
/// model/context/protocol, making only the older target fully relevant.
#[test]
fn symbolic_pre_cap_relevance_uses_the_rankers_expanded_query() {
    let mut conn = setup();

    let mut target = memory_entry(
        "expanded-symbolic-target",
        "mcp model context protocol",
        &[],
    );
    target.timestamp = "2020-01-01T00:00:00Z".to_string();
    insert_entry(&mut conn, target);

    for idx in 0..201 {
        let mut newer_partial = memory_entry(
            &format!("expanded-symbolic-partial-{idx:03}"),
            "mcp only partial distractor",
            &[],
        );
        newer_partial.timestamp = format!(
            "2027-{:02}-{:02}T{:02}:00:00Z",
            1 + idx / (24 * 28),
            1 + (idx / 24) % 28,
            idx % 24,
        );
        insert_entry(&mut conn, newer_partial);
    }

    let opts = SearchOptions {
        candidates_per_channel: 0,
        top_k: 1,
        weights: HybridWeights {
            semantic: 0.0,
            fts: 0.0,
            symbolic: 1.0,
            decay: 0.0,
            use_rrf: false,
        },
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "mcp", &opts).expect("symbolic search succeeds");

    assert_eq!(
        results.first().map(|result| result.entry.id.as_str()),
        Some("expanded-symbolic-target"),
        "pre-cap eligibility must use the final ranker's expanded query, not raw `mcp` alone"
    );
}

/// The SQL eligibility scorer must preserve the final ranker's token
/// boundaries. A substring containing every expansion term is not four token
/// matches: `amcpmodelcontextprotocolx` has no `mcp`, `model`, `context`, or
/// `protocol` token, so it must not evict the older exact-token target.
#[test]
fn symbolic_pre_cap_relevance_rejects_substring_coverage_false_positive() {
    let mut conn = setup();

    let mut target = memory_entry(
        "expanded-token-boundary-target",
        "mcp model context protocol",
        &[],
    );
    target.timestamp = "2020-01-01T00:00:00Z".to_string();
    insert_entry(&mut conn, target);

    let mut newer_substring = memory_entry(
        "expanded-substring-distractor",
        "amcpmodelcontextprotocolx",
        &[],
    );
    newer_substring.timestamp = "2027-01-01T00:00:00Z".to_string();
    insert_entry(&mut conn, newer_substring);

    let opts = SearchOptions {
        candidates_per_channel: 0,
        top_k: 1,
        weights: HybridWeights {
            semantic: 0.0,
            fts: 0.0,
            symbolic: 1.0,
            decay: 0.0,
            use_rrf: false,
        },
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "mcp", &opts).expect("symbolic search succeeds");

    assert_eq!(
        results.first().map(|result| result.entry.id.as_str()),
        Some("expanded-token-boundary-target"),
        "pre-cap eligibility must call the final token scorer, not count LIKE substrings"
    );
}

/// Timestamp only breaks true symbolic-score ties. It must compare parsed
/// instants, rather than lexically ordering mixed RFC3339 representations.
#[test]
fn symbolic_pre_cap_timestamp_tie_break_uses_real_instants() {
    let mut conn = setup();
    let mut older = memory_entry(
        "timestamp-tie-older",
        "controlplane older isolated context",
        &[],
    );
    older.timestamp = "2026-01-01T00:00:00Z".to_string();
    insert_entry(&mut conn, older);

    let mut newer = memory_entry(
        "timestamp-tie-newer",
        "controlplane newer distinct context",
        &[],
    );
    newer.timestamp = "2026-01-01T00:00:00.500Z".to_string();
    insert_entry(&mut conn, newer);

    let opts = SearchOptions {
        candidates_per_channel: 0,
        top_k: 1,
        weights: HybridWeights {
            semantic: 0.0,
            fts: 0.0,
            symbolic: 1.0,
            decay: 0.0,
            use_rrf: false,
        },
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    };
    let symbolic_candidates =
        crate::db::search_symbolic_candidates(&conn, "controlplane", 1, false, false, None, None)
            .expect("symbolic candidate query succeeds");
    assert_eq!(
        symbolic_candidates.first().map(|entry| entry.id.as_str()),
        Some("timestamp-tie-newer"),
        "the pre-cap SQL tie-break itself must select the newer instant"
    );
    let results = hybrid_search(&conn, "controlplane", &opts).expect("symbolic search succeeds");

    assert_eq!(
        results.first().map(|result| result.entry.id.as_str()),
        Some("timestamp-tie-newer"),
        "fractional-second RFC3339 timestamps must be ordered by instant, not raw text"
    );
}

/// ID is the final deterministic tie-break once both exact token relevance and
/// timestamp are equal. This keeps the bounded symbolic candidate set stable.
#[test]
fn symbolic_pre_cap_id_tie_break_is_stable() {
    let mut conn = setup();
    for (id, text) in [
        ("symbolic-id-tie-a", "controlplane alpha context"),
        ("symbolic-id-tie-b", "controlplane beta context"),
    ] {
        let mut entry = memory_entry(id, text, &[]);
        entry.timestamp = "2026-01-01T00:00:00Z".to_string();
        insert_entry(&mut conn, entry);
    }

    let symbolic_candidates =
        crate::db::search_symbolic_candidates(&conn, "controlplane", 1, false, false, None, None)
            .expect("symbolic candidate query succeeds");

    assert_eq!(
        symbolic_candidates.first().map(|entry| entry.id.as_str()),
        Some("symbolic-id-tie-a"),
        "ID must be the deterministic final tie-break after equal relevance and timestamp"
    );
}
