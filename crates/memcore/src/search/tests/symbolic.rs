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

/// Stored keywords and entities participate in the exact same pre-cap score
/// as final ranking. This target has no query token in text, so recovering it
/// proves the SQLite scalar function decodes both JSON columns before scoring.
#[test]
fn symbolic_pre_cap_relevance_scores_keyword_and_entity_json() {
    let mut conn = setup();

    let mut target = memory_entry(
        "expanded-metadata-target",
        "older metadata-only symbolic target",
        &["mcp", "model"],
    );
    target.entities = vec!["context protocol".to_string()];
    target.timestamp = "2020-01-01T00:00:00Z".to_string();
    insert_entry(&mut conn, target);

    let mut newer_substring = memory_entry(
        "expanded-metadata-substring-distractor",
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
        Some("expanded-metadata-target"),
        "pre-cap selection must score parsed keywords and entities, not raw JSON substrings"
    );
    assert_eq!(
        results.first().map(|result| result.score.symbolic),
        Some(1.0),
        "both JSON columns are necessary: omitting either leaves only half of the expanded tokens"
    );
}

/// A legacy table can contain NULL JSON columns even though the current DDL
/// forbids them. Candidate selection must mirror `row_to_entry`'s empty-array
/// fallback instead of failing inside the SQLite scalar function.
#[test]
fn symbolic_pre_cap_legacy_null_json_columns_fall_back_to_empty_arrays() {
    let conn = Connection::open_in_memory().expect("open legacy fixture database");
    conn.execute_batch(
        r#"
        CREATE TABLE memories (
            id TEXT NOT NULL,
            path TEXT NOT NULL,
            summary TEXT NOT NULL,
            text TEXT NOT NULL,
            importance REAL NOT NULL,
            timestamp TEXT NOT NULL,
            valid_from TEXT,
            valid_until TEXT,
            category TEXT NOT NULL,
            topic TEXT NOT NULL,
            keywords TEXT,
            entities TEXT,
            source TEXT NOT NULL,
            scope TEXT NOT NULL,
            archived INTEGER NOT NULL,
            superseded_by TEXT,
            access_count INTEGER NOT NULL,
            last_access TEXT,
            -- tachi#1446. Present in the CREATE TABLE but deliberately absent
            -- from the INSERT below: NULL is exactly the state this column is in
            -- on every row of every database today, so leaving it unset is the
            -- honest fixture, not an omission.
            last_use_at TEXT,
            revision INTEGER,
            metadata TEXT NOT NULL,
            retention_policy TEXT,
            domain TEXT,
            recall_count INTEGER,
            query_diversity INTEGER,
            tier TEXT
        );
        INSERT INTO memories (
            id, path, summary, text, importance, timestamp, valid_from,
            valid_until, category, topic, keywords, entities, source, scope,
            archived, access_count, last_access, revision, metadata,
            retention_policy, domain, recall_count, query_diversity, tier
        ) VALUES (
            'legacy-null-symbolic', '/legacy', 'legacy symbolic row',
            'controlplane legacy text match', 0.7, '2026-01-01T00:00:00Z',
            NULL, NULL, 'fact', '', NULL, NULL, 'legacy', 'general',
            0, 0, NULL, 1, '{}', NULL, NULL, 0, 0, 'raw'
        );
        "#,
    )
    .expect("create legacy row with NULL JSON columns");

    // This fixture is hand-built on purpose: it needs NULL `keywords`/`entities`,
    // which the current DDL forbids and which the CHECK-constraint rebuild would
    // reject (`memories_new.keywords` is `TEXT NOT NULL`), so `init_schema` is
    // not an option here. The cost of hand-building is that the table silently
    // falls behind `MEMORY_SELECT_COLUMNS` every time a column is added — which
    // is what happened when tachi#1446 added `last_use_at`, surfacing as a bare
    // `no such column` from inside `search_symbolic_candidates`. This assertion
    // converts that into a named failure that says which columns to add.
    crate::db::assert_memories_fixture_matches_select_columns(
        &conn,
        "symbolic_pre_cap_legacy_null_json_columns_fall_back_to_empty_arrays",
    );

    let candidates = crate::db::search_symbolic_candidates(
        &conn,
        "controlplane",
        1,
        false,
        false,
        None,
        None,
        None,
    )
    .expect("legacy NULL JSON columns must not fail symbolic search");
    let candidate = candidates.first().expect("legacy row remains searchable");

    assert_eq!(candidate.id, "legacy-null-symbolic");
    assert!(candidate.keywords.is_empty());
    assert!(candidate.entities.is_empty());
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
    let symbolic_candidates = crate::db::search_symbolic_candidates(
        &conn,
        "controlplane",
        1,
        false,
        false,
        None,
        None,
        None,
    )
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

    let symbolic_candidates = crate::db::search_symbolic_candidates(
        &conn,
        "controlplane",
        1,
        false,
        false,
        None,
        None,
        None,
    )
    .expect("symbolic candidate query succeeds");

    assert_eq!(
        symbolic_candidates.first().map(|entry| entry.id.as_str()),
        Some("symbolic-id-tie-a"),
        "ID must be the deterministic final tie-break after equal relevance and timestamp"
    );
}

/// #1331 BUG 1: a single CJK character is 3 UTF-8 bytes so it passes
/// `symbolic_terms`'s byte gate, but FTS5 trigram MATCH requires ≥3 Unicode
/// characters — MATCH-only eligibility is empty while LIKE still hits.
#[test]
fn symbolic_single_cjk_char_keeps_like_eligibility() {
    let mut conn = setup();
    insert(&mut conn, "cjk-note", "今日要记一件重要的事", &["journal"]);

    let hits =
        crate::db::search_symbolic_candidates(&conn, "记", 10, false, false, None, None, None)
            .expect("CJK symbolic search succeeds");
    assert!(
        hits.iter().any(|e| e.id == "cjk-note"),
        "single-CJK query must still retrieve via the LIKE path; got ids: {:?}",
        hits.iter().map(|e| e.id.as_str()).collect::<Vec<_>>()
    );

    // Discrimination: forcing the trigram MATCH path for this term goes red.
    let match_hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories_symbolic_fts WHERE memories_symbolic_fts MATCH ?1",
            rusqlite::params![r#""记""#],
            |row| row.get(0),
        )
        .expect("MATCH query must prepare even when it matches nothing");
    assert_eq!(
        match_hits, 0,
        "trigram MATCH on a 1-char CJK term must be empty — that is why short \
         graphemes must not take the MATCH path"
    );
}
