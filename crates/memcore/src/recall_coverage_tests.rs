use crate::{
    hybrid_search, is_recall_coverage_path_list_only, run_recall_coverage_probe,
    run_recall_coverage_probe_with_corpus, run_recall_coverage_probe_with_equivalences,
    MemoryEntry, MemoryStore, RecallCoverageEquivalenceCorpus, RecallCoverageEquivalenceSet,
    RecallCoverageEvidenceKind, RecallCoverageOptions, RecallCoverageOutcome,
    RecallCoverageQuerySource, SearchOptions, RECALL_COVERAGE_EQUIVALENCE_SCHEMA_VERSION,
};
use rusqlite::{params, Connection};
use serde_json::json;

fn fixture_entry(id: &str, path: &str, content: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: path.to_string(),
        summary: content.to_string(),
        text: content.to_string(),
        importance: 0.7,
        timestamp: "2026-07-28T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "fixture".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: json!({}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

fn insert(store: &mut MemoryStore, entry: MemoryEntry) {
    store.upsert(&entry).expect("insert coverage fixture");
}

#[test]
fn recall_coverage_path_list_only_prefixes_have_exact_boundaries() {
    for path in [
        "/guide",
        "/guide/chapter",
        "/cards",
        "/cards/seat",
        "/sticky",
        "/sticky/rule",
        "/components/v0",
        "/components/v0/auth",
        "/agent/checkpoints",
        "/agent/checkpoints/2026-07-28",
    ] {
        assert!(
            is_recall_coverage_path_list_only(path),
            "{path} must remain a path-list-only namespace"
        );
    }

    for path in [
        "/guidebook",
        "/card",
        "/cards-old",
        "/stickiness",
        "/components/v01",
        "/components/v0x",
        "/agent/checkpoint",
        "/agent/checkpoints-old",
        "/wiki/guide",
    ] {
        assert!(
            !is_recall_coverage_path_list_only(path),
            "{path} must not be swallowed by a prefix-only exclusion"
        );
    }
}

#[test]
fn recall_coverage_partitions_every_row_and_keeps_mixed_populations_eligible() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let path = dir.path().join("coverage.db");
    let mut store = MemoryStore::open(path.to_str().expect("utf8 path")).expect("open store");

    let mut archived = fixture_entry("archived", "/notes/archived", "archived row");
    archived.archived = true;
    insert(&mut store, archived);
    insert(
        &mut store,
        fixture_entry("superseded", "/notes/superseded", "superseded row"),
    );
    insert(
        &mut store,
        fixture_entry("search-noise", "/kanban/tasks/a", "kanban row"),
    );
    let mut metadata_projection = fixture_entry(
        "metadata-projection",
        "/notes/projected",
        "metadata-only projection row",
    );
    metadata_projection.metadata = json!({"projection_kind": "outcome"});
    insert(&mut store, metadata_projection);
    insert(
        &mut store,
        fixture_entry("path-only", "/guide/coverage", "guide row"),
    );
    insert(
        &mut store,
        fixture_entry("already-surfaced", "/notes/surfaced", "surfaced row"),
    );
    for (id, path) in [
        ("wiki", "/wiki/coverage"),
        ("eval", "/eval/coverage"),
        ("patterns", "/user/patterns/coverage"),
        ("timeline", "/timeline/wiki/coverage"),
        ("outcome", "/outcomes/task/coverage"),
        ("project-cycle", "/project-cycle/task/coverage"),
        ("rules", "/behavior/global_rules/coverage"),
        ("feedback", "/feedback/coverage"),
        ("distill", "/foundry/distill/coverage"),
        ("root", "/"),
        ("generic", "/notes/generic"),
    ] {
        insert(
            &mut store,
            fixture_entry(id, path, &format!("eligible {id}")),
        );
    }

    // `superseded_by` is authority-bearing and is intentionally denied through
    // the guarded store connection. A disposable raw fixture connection is the
    // sanctioned way to seed this otherwise-valid database state for a reader.
    drop(store);
    let fixture = Connection::open(&path).expect("open fixture connection");
    fixture
        .execute(
            "UPDATE memories SET superseded_by = 'winner' WHERE id = 'superseded'",
            [],
        )
        .expect("mark superseded fixture");
    fixture
        .execute(
            "UPDATE memories SET access_count = 1 WHERE id = 'already-surfaced'",
            [],
        )
        .expect("mark surfaced fixture");
    drop(fixture);
    let store = MemoryStore::open(path.to_str().expect("utf8 path")).expect("reopen store");

    let report = run_recall_coverage_probe(
        &store,
        RecallCoverageOptions {
            limit: Some(1),
            ..Default::default()
        },
    )
    .expect("coverage report");

    assert_eq!(report.partition.total_rows, 17);
    assert_eq!(report.partition.archived, 1);
    assert_eq!(report.partition.superseded, 1);
    assert_eq!(report.partition.search_noise, 6);
    assert_eq!(report.partition.path_list_only, 1);
    assert_eq!(report.partition.already_surfaced, 1);
    assert_eq!(report.partition.eligible, 7);
    assert!(report.partition_invariant_holds);
    assert_eq!(report.selected_eligible_rows, 1);
    assert_eq!(report.unprobed_due_to_limit, 6);
    assert!(!report.coverage_complete);

    let eligible_ids: Vec<_> = report
        .targets
        .iter()
        .map(|target| target.id.as_str())
        .collect();
    assert_eq!(eligible_ids, ["distill"]);
}

#[test]
fn recall_coverage_reachable_target_surfaces_through_hybrid_kernel_without_content_output() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    assert!(
        store.vec_available,
        "sqlite-vec required for vector-backed coverage"
    );
    let content = "RecallCoverageHybridNeedle whole summary must remain intact";
    let mut entry = fixture_entry("reachable", "/wiki/coverage", content);
    entry.vector = Some(vec![0.25; 1024]);
    insert(&mut store, entry);

    let report =
        run_recall_coverage_probe(&store, RecallCoverageOptions::default()).expect("coverage");
    assert_eq!(report.probed, 1);
    assert_eq!(report.surfaced, 1);
    assert_eq!(report.not_surfaced, 0);
    let target = report.targets.first().expect("one target");
    assert_eq!(target.id, "reachable");
    assert_eq!(
        target.query_source,
        Some(RecallCoverageQuerySource::Summary)
    );
    assert!(target.stored_vector_present);
    assert_eq!(target.outcome, RecallCoverageOutcome::Surfaced);
    assert_eq!(target.rank, Some(1));
    assert_eq!(report.vector_unavailable, 0);

    let output = serde_json::to_string(&report).expect("serialize content-free report");
    assert!(
        !output.contains(content),
        "coverage output must not include a source field or generated query"
    );
}

#[test]
fn recall_coverage_rejects_lexical_only_target_omitted_from_vector_channel() {
    const CANDIDATES_PER_CHANNEL: usize = 1;
    const VECTOR_DIMENSIONS: usize = 1024;
    const FIXTURES: [(&str, &str); 5] = [
        ("vector-tie-a", "aurora cobalt zephyr"),
        ("vector-tie-b", "bramble delta quartz"),
        ("vector-tie-c", "cinder fjord maple"),
        ("vector-tie-d", "ember glacial orbit"),
        ("vector-tie-e", "harbor juniper prism"),
    ];

    let mut store = MemoryStore::open_in_memory().expect("open store");
    assert!(
        store.vec_available,
        "sqlite-vec required to distinguish vector exclusion from lexical recovery"
    );
    let vector = vec![0.25; VECTOR_DIMENSIONS];

    assert!(FIXTURES.len() > CANDIDATES_PER_CHANNEL);
    for (id, content) in FIXTURES {
        let mut entry = fixture_entry(id, "/notes/vector-tie", content);
        entry.vector = Some(vector.clone());
        entry.access_count = 1;
        insert(&mut store, entry);
    }

    let vector_candidates = crate::db::search_vec(
        store.connection(),
        &vector,
        CANDIDATES_PER_CHANNEL,
        false,
        false,
        None,
        None,
        None,
    )
    .expect("vector candidate search");

    let fixture_ids: std::collections::HashSet<&str> = FIXTURES.iter().map(|(id, _)| *id).collect();
    let vector_candidate_ids: std::collections::HashSet<&str> =
        vector_candidates.keys().map(String::as_str).collect();
    let mut omitted_ids: Vec<&str> = fixture_ids
        .difference(&vector_candidate_ids)
        .copied()
        .collect();
    omitted_ids.sort_unstable();
    let target_id = *omitted_ids
        .first()
        .expect("more equal-vector rows than candidate width must leave an omitted target");
    let target_content = FIXTURES
        .iter()
        .find_map(|(id, content)| (*id == target_id).then_some(*content))
        .expect("omitted target content");

    let updated = store
        .connection()
        .execute(
            "UPDATE memories SET access_count = 0 WHERE id = ?1",
            [target_id],
        )
        .expect("select discovered target for coverage");
    assert_eq!(updated, 1);

    let second_vector_candidates = crate::db::search_vec(
        store.connection(),
        &vector,
        CANDIDATES_PER_CHANNEL,
        false,
        false,
        None,
        None,
        None,
    )
    .expect("repeat vector candidate search");
    assert!(
        !second_vector_candidates.contains_key(target_id),
        "discovered target must remain omitted after changing access_count only: {second_vector_candidates:?}"
    );

    let search_options = SearchOptions {
        top_k: 2,
        candidates_per_channel: CANDIDATES_PER_CHANNEL,
        query_vec: Some(vector),
        vec_available: store.vec_available,
        record_access: false,
        graph_expand_hops: 0,
        ..Default::default()
    };
    let hybrid_results = hybrid_search(store.connection(), target_content, &search_options)
        .expect("hybrid lexical recovery");
    let hybrid_rank = hybrid_results
        .iter()
        .position(|result| result.entry.id == target_id)
        .map(|index| index + 1)
        .expect("the target must reach final top-k through lexical/symbolic candidates");

    let report = run_recall_coverage_probe(
        &store,
        RecallCoverageOptions {
            top_k: 2,
            candidates_per_channel: CANDIDATES_PER_CHANNEL,
            limit: Some(1),
        },
    )
    .expect("coverage report");
    let target = report.targets.first().expect("one target");
    assert_eq!(target.id, target_id);
    assert_eq!(target.rank, Some(hybrid_rank));
    assert_eq!(target.outcome, RecallCoverageOutcome::NotSurfaced);
    assert_eq!(report.probed, 1);
    assert_eq!(report.surfaced, 0);
    assert_eq!(report.not_surfaced, 1);
}

#[test]
fn recall_coverage_missing_target_vector_is_not_lexical_only_coverage() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    assert!(
        store.vec_available,
        "sqlite-vec is required to distinguish a missing target vector from an unavailable kernel"
    );
    insert(
        &mut store,
        fixture_entry(
            "missing-target-vector",
            "/notes/missing-target-vector",
            "MissingTargetVectorCoverageNeedle",
        ),
    );

    let report =
        run_recall_coverage_probe(&store, RecallCoverageOptions::default()).expect("coverage");
    let target = report.targets.first().expect("one target");
    assert_eq!(
        target.query_source,
        Some(RecallCoverageQuerySource::Summary),
        "a deterministic textual query must still be selected before vector availability is assessed"
    );
    assert_eq!(
        target.rank, None,
        "lexical-only ranking is not coverage evidence"
    );
    assert_ne!(
        target.outcome,
        RecallCoverageOutcome::Surfaced,
        "a target without a stored vector must not count as surfaced by lexical-only search"
    );
    assert_eq!(
        report.probed, 0,
        "the hybrid kernel must not run without a target vector"
    );
    assert_eq!(report.surfaced, 0);
    assert_eq!(report.not_surfaced, 0);
    assert_eq!(report.unprobeable, 0);
    assert_eq!(report.vector_unavailable, 1);
    assert_eq!(target.outcome, RecallCoverageOutcome::VectorUnavailable);
    assert_eq!(
        serde_json::to_value(&report)
            .expect("serialize report")
            .get("vector_unavailable")
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "the report must account for vector-unavailable targets separately"
    );
}

#[test]
fn recall_coverage_no_query_precedes_vector_unavailable() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    store.vec_available = false;
    let mut entry = fixture_entry("empty", "/notes/empty", "");
    entry.summary = String::new();
    entry.text = String::new();
    entry.topic = String::new();
    entry.keywords = vec![String::new(), "   ".to_string()];
    entry.entities = vec![String::new(), "\t".to_string()];
    insert(&mut store, entry);

    let report =
        run_recall_coverage_probe(&store, RecallCoverageOptions::default()).expect("coverage");
    assert_eq!(report.probed, 0);
    assert_eq!(report.unprobeable, 1);
    assert_eq!(report.vector_unavailable, 0);
    let target = report.targets.first().expect("one target");
    assert_eq!(target.query_source, None);
    assert_eq!(target.outcome, RecallCoverageOutcome::Unprobeable);
    assert_eq!(target.rank, None);
}

#[test]
fn recall_coverage_marks_unavailable_sqlite_vec_leg_without_lexical_probe() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    assert!(
        store.vec_available,
        "sqlite-vec required to seed a stored vector"
    );
    let mut entry = fixture_entry(
        "unavailable-sqlite-vec",
        "/notes/unavailable-sqlite-vec",
        "UnavailableSqliteVecCoverageNeedle",
    );
    entry.vector = Some(vec![0.5; 1024]);
    insert(&mut store, entry);
    store.vec_available = false;

    let report =
        run_recall_coverage_probe(&store, RecallCoverageOptions::default()).expect("coverage");
    let target = report.targets.first().expect("one target");
    assert!(target.stored_vector_present);
    assert_eq!(
        target.query_source,
        Some(RecallCoverageQuerySource::Summary)
    );
    assert_eq!(target.outcome, RecallCoverageOutcome::VectorUnavailable);
    assert_eq!(target.rank, None);
    assert_eq!(report.probed, 0);
    assert_eq!(report.vector_unavailable, 1);
}

#[test]
fn recall_coverage_limit_is_deterministic_and_options_fail_loudly() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    insert(
        &mut store,
        fixture_entry("z-target", "/notes/z", "z needle"),
    );
    insert(
        &mut store,
        fixture_entry("a-target", "/notes/a", "a needle"),
    );

    let options = RecallCoverageOptions {
        limit: Some(1),
        ..Default::default()
    };
    let first = run_recall_coverage_probe(&store, options.clone()).expect("first coverage");
    let second = run_recall_coverage_probe(&store, options).expect("second coverage");
    assert_eq!(first.targets.len(), 1);
    assert_eq!(first.targets[0].id, "a-target");
    assert_eq!(first.targets[0].id, second.targets[0].id);
    assert!(!first.coverage_complete);
    assert_eq!(first.unprobed_due_to_limit, 1);

    for (options, invariant) in [
        (
            RecallCoverageOptions {
                top_k: 0,
                ..Default::default()
            },
            "recall coverage invariant: top_k must be > 0",
        ),
        (
            RecallCoverageOptions {
                candidates_per_channel: 0,
                ..Default::default()
            },
            "recall coverage invariant: candidates_per_channel must be > 0",
        ),
        (
            RecallCoverageOptions {
                limit: Some(0),
                ..Default::default()
            },
            "recall coverage invariant: limit must be > 0 when provided",
        ),
    ] {
        let error = run_recall_coverage_probe(&store, options).expect_err("invalid options fail");
        assert!(
            error.to_string().contains(invariant),
            "guard must name the invariant: {error}"
        );
    }
}

fn mutable_state(conn: &Connection, id: &str) -> (i64, i64, Option<String>, i64, i64) {
    let (access_count, scored_count, last_access) = conn
        .query_row(
            "SELECT access_count, scored_count, last_access FROM memories WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read memory state");
    let history_rows = conn
        .query_row(
            "SELECT COUNT(*) FROM access_history WHERE memory_id = ?1",
            [id],
            |row| row.get(0),
        )
        .expect("read access history count");
    let generation = conn
        .query_row(
            "SELECT generation FROM memory_search_generation WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("read search generation");
    (
        access_count,
        scored_count,
        last_access,
        history_rows,
        generation,
    )
}

#[test]
fn recall_coverage_reopened_read_only_preserves_all_search_mutation_surfaces() {
    let dir = tempfile::tempdir().expect("temp db dir");
    let path = dir.path().join("coverage.db");
    let mut writable = MemoryStore::open(path.to_str().expect("utf8 path")).expect("open db");
    assert!(
        writable.vec_available,
        "sqlite-vec required for vector-backed coverage"
    );
    let mut entry = fixture_entry(
        "read-only-target",
        "/notes/read-only",
        "ReadOnlyCoverageNeedle",
    );
    entry.vector = Some(vec![0.75; 1024]);
    insert(&mut writable, entry);
    writable
        .connection()
        .execute(
            "UPDATE memories
             SET access_count = 0, scored_count = 7, last_access = ?1
             WHERE id = 'read-only-target'",
            params!["2026-07-27T00:00:00Z"],
        )
        .expect("seed mutable fields");
    writable
        .connection()
        .execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash)
             VALUES ('read-only-target', '2026-07-27T00:00:00Z', 'seed')",
            [],
        )
        .expect("seed access history");
    let before = mutable_state(writable.connection(), "read-only-target");
    drop(writable);

    let read_only =
        MemoryStore::open_read_only(path.to_str().expect("utf8 path")).expect("reopen read-only");
    assert!(
        read_only.vec_available,
        "the read-only probe must exercise the sqlite-vec hybrid channel"
    );
    let report = run_recall_coverage_probe(&read_only, RecallCoverageOptions::default())
        .expect("probe must succeed against a read-only DB");
    assert_eq!(
        report.probed, 1,
        "the read-only probe must enter hybrid_search"
    );
    assert_eq!(report.surfaced, 1);
    assert_eq!(report.vector_unavailable, 0);
    assert!(report.targets[0].stored_vector_present);
    assert_eq!(report.targets[0].prior_scored_count, 7);
    drop(read_only);

    let verify = Connection::open(&path).expect("reopen verification connection");
    assert_eq!(
        mutable_state(&verify, "read-only-target"),
        before,
        "record_access must remain false: access_count, scored_count, last_access, access history, and search generation are immutable under the probe"
    );
}

fn reviewed_equivalence_fixture(
    canonical_exists: bool,
) -> (
    crate::RecallCoverageReport,
    crate::RecallCoverageReport,
    String,
    String,
) {
    const CANDIDATES_PER_CHANNEL: usize = 1;
    const VECTOR_DIMENSIONS: usize = 1024;
    const FIXTURES: [(&str, &str); 5] = [
        ("fact-row-a", "aurora cobalt zephyr"),
        ("fact-row-b", "bramble delta quartz"),
        ("fact-row-c", "cinder fjord maple"),
        ("fact-row-d", "ember glacial orbit"),
        ("fact-row-e", "harbor juniper prism"),
    ];

    let mut store = MemoryStore::open_in_memory().expect("open store");
    assert!(store.vec_available, "sqlite-vec required for fact coverage");
    let vector = vec![0.25; VECTOR_DIMENSIONS];
    for (id, content) in FIXTURES {
        let mut entry = fixture_entry(id, "/notes/fact-coverage", content);
        entry.vector = Some(vector.clone());
        entry.access_count = 1;
        insert(&mut store, entry);
    }

    let vector_candidates = crate::db::search_vec(
        store.connection(),
        &vector,
        CANDIDATES_PER_CHANNEL,
        false,
        false,
        None,
        None,
        None,
    )
    .expect("vector candidate search");
    let actual_canonical_id = vector_candidates
        .keys()
        .next()
        .expect("one vector candidate")
        .clone();
    let mut omitted_ids: Vec<&str> = FIXTURES
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !vector_candidates.contains_key(*id))
        .collect();
    omitted_ids.sort_unstable();
    let target_id = omitted_ids.first().expect("an omitted target").to_string();
    store
        .connection()
        .execute(
            "UPDATE memories SET access_count = 0 WHERE id = ?1",
            [&target_id],
        )
        .expect("select exact target");

    let canonical_id = if canonical_exists {
        actual_canonical_id
    } else {
        "reviewed-canonical-row-not-in-store".to_string()
    };
    let equivalences = [RecallCoverageEquivalenceSet {
        canonical_id: canonical_id.clone(),
        equivalent_ids: vec![target_id.clone()],
        evidence_source: "fixture:reviewed-equivalence-v1".to_string(),
    }];
    let exact_only = run_recall_coverage_probe(
        &store,
        RecallCoverageOptions {
            top_k: 10,
            candidates_per_channel: CANDIDATES_PER_CHANNEL,
            limit: Some(1),
        },
    )
    .expect("exact-only coverage report");
    let report = run_recall_coverage_probe_with_equivalences(
        &store,
        RecallCoverageOptions {
            top_k: 10,
            candidates_per_channel: CANDIDATES_PER_CHANNEL,
            limit: Some(1),
        },
        &equivalences,
    )
    .expect("fact coverage report");
    (report, exact_only, target_id, canonical_id)
}

#[test]
fn recall_coverage_exact_hit_populates_both_metrics_without_changing_exact_outcome() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    let mut entry = fixture_entry("exact-hit", "/notes/exact-hit", "ExactFactCoverageNeedle");
    entry.vector = Some(vec![0.75; 1024]);
    insert(&mut store, entry);

    let report = run_recall_coverage_probe(&store, RecallCoverageOptions::default())
        .expect("exact coverage");
    let target = report.targets.first().expect("one target");
    assert_eq!(target.outcome, RecallCoverageOutcome::Surfaced);
    assert_eq!(target.rank, Some(1));
    assert_eq!(
        target.canonical_fact_outcome,
        RecallCoverageOutcome::Surfaced
    );
    assert_eq!(target.canonical_id.as_deref(), Some("exact-hit"));
    assert_eq!(target.matched_canonical_id.as_deref(), Some("exact-hit"));
    assert_eq!(target.canonical_rank, Some(1));
    assert_eq!(
        target.fact_evidence.kind,
        RecallCoverageEvidenceKind::ExactIdentity
    );
    assert_eq!(report.exact_metrics.denominator, 1);
    assert_eq!(report.exact_metrics.hits, 1);
    assert_eq!(report.exact_metrics.recall_at_k, 1.0);
    assert_eq!(report.exact_metrics.mrr, 1.0);
    assert_eq!(report.canonical_fact_metrics, report.exact_metrics);
}

#[test]
fn recall_coverage_reviewed_equivalent_hit_keeps_exact_miss_red() {
    let (report, _, target_id, canonical_id) = reviewed_equivalence_fixture(true);
    let target = report.targets.first().expect("one target");

    assert_eq!(target.id, target_id);
    assert_eq!(target.outcome, RecallCoverageOutcome::NotSurfaced);
    assert_eq!(report.surfaced, 0, "legacy exact total must stay red");
    assert_eq!(report.not_surfaced, 1);
    assert_eq!(report.exact_metrics.hits, 0);
    assert_eq!(
        target.canonical_fact_outcome,
        RecallCoverageOutcome::Surfaced
    );
    assert_eq!(target.canonical_id.as_deref(), Some(canonical_id.as_str()));
    assert_eq!(
        target.matched_canonical_id.as_deref(),
        Some(canonical_id.as_str())
    );
    assert!(target.canonical_rank.is_some());
    assert_eq!(
        target.fact_evidence.kind,
        RecallCoverageEvidenceKind::ReviewedEquivalence
    );
    assert_eq!(
        target.fact_evidence.source,
        "fixture:reviewed-equivalence-v1"
    );
    assert_eq!(report.canonical_fact_metrics.hits, 1);
    assert_eq!(report.canonical_fact_metrics.recall_at_k, 1.0);
    assert!(
        target
            .canonical_candidate_legs
            .as_ref()
            .is_some_and(|legs| legs.vector),
        "fact coverage must expose the executed vector candidate evidence"
    );
}

#[test]
fn recall_coverage_true_miss_stays_red_without_fabricated_equivalence() {
    let (report, _, _, canonical_id) = reviewed_equivalence_fixture(false);
    let target = report.targets.first().expect("one target");

    assert_eq!(target.outcome, RecallCoverageOutcome::NotSurfaced);
    assert_eq!(
        target.canonical_fact_outcome,
        RecallCoverageOutcome::NotSurfaced
    );
    assert_eq!(target.canonical_id.as_deref(), Some(canonical_id.as_str()));
    assert_eq!(target.matched_canonical_id, None);
    assert_eq!(target.canonical_rank, None);
    assert!(target.exact_candidate_legs.is_some());
    assert!(
        target
            .canonical_candidate_legs
            .is_some_and(|legs| !legs.vector && !legs.fts && !legs.symbolic && !legs.exact_id),
        "an executed true miss must report negative evidence for every observed candidate leg"
    );
    assert_eq!(
        target.fact_evidence.kind,
        RecallCoverageEvidenceKind::ReviewedEquivalence
    );
    assert_eq!(report.exact_metrics.hits, 0);
    assert_eq!(report.canonical_fact_metrics.hits, 0);
    assert_eq!(report.canonical_fact_metrics.recall_at_k, 0.0);
}

#[test]
fn reviewed_equivalence_changes_only_fact_metrics_not_exact_totals() {
    let (reviewed, exact_only, _, _) = reviewed_equivalence_fixture(true);
    let exact_totals_bytes = |report: &crate::RecallCoverageReport| {
        serde_json::to_vec(&json!({
            "surfaced": report.surfaced,
            "not_surfaced": report.not_surfaced,
        }))
        .expect("serialize legacy exact totals")
    };
    assert_eq!(
        exact_totals_bytes(&reviewed),
        exact_totals_bytes(&exact_only),
        "reviewed equivalence must leave legacy surfaced/not_surfaced bytes unchanged"
    );
    assert_eq!(reviewed.probed, exact_only.probed);
    assert_eq!(reviewed.surfaced, exact_only.surfaced);
    assert_eq!(reviewed.not_surfaced, exact_only.not_surfaced);
    assert_eq!(reviewed.unprobeable, exact_only.unprobeable);
    assert_eq!(reviewed.vector_unavailable, exact_only.vector_unavailable);
    assert_eq!(reviewed.targets[0].outcome, exact_only.targets[0].outcome);
    assert_eq!(reviewed.targets[0].rank, exact_only.targets[0].rank);
    assert_eq!(reviewed.exact_metrics, exact_only.exact_metrics);
    assert_eq!(reviewed.exact_metrics.hits, 0);
    assert_eq!(reviewed.canonical_fact_metrics.hits, 1);
    assert_eq!(
        exact_only.targets[0].fact_evidence.kind,
        RecallCoverageEvidenceKind::ExactIdentity,
        "without reviewed data or stored lineage, the report must not invent equivalence"
    );
    assert_eq!(
        exact_only.targets[0].canonical_id,
        Some(exact_only.targets[0].id.clone())
    );
    assert_eq!(exact_only.canonical_fact_metrics.hits, 0);
}

#[test]
fn recall_coverage_json_remains_content_free_and_exposes_both_denominators() {
    let (report, _, _, _) = reviewed_equivalence_fixture(true);
    let output = serde_json::to_value(&report).expect("serialize report");
    assert_eq!(output["exact_metrics"]["denominator"], 1);
    assert_eq!(output["canonical_fact_metrics"]["denominator"], 1);
    assert!(!output.to_string().contains("aurora cobalt zephyr"));
}

#[test]
fn recall_coverage_human_report_is_content_free_and_shows_both_metrics() {
    let (report, _, _, _) = reviewed_equivalence_fixture(true);
    let output = crate::format_recall_coverage_human(&report);
    assert!(output.contains("Exact-ID Recall: 0/1 Recall@K=0.000000 MRR=0.000000"));
    assert!(output.contains("Canonical Fact/Lineage Recall: 1/1 Recall@K=1.000000"));
    assert!(output.contains("evidence_source=fixture:reviewed-equivalence-v1"));
    assert!(output.contains("exact_legs=[vector=false"));
    assert!(!output.contains("aurora cobalt zephyr"));
}

#[test]
fn stored_supersession_lineage_runs_end_to_end_in_independent_expected_id_lane() {
    let mut store = MemoryStore::open_in_memory().expect("open store");
    for (id, archived, content) in [
        ("lineage-old", true, "obsolete cobalt origin"),
        ("lineage-middle", true, "transition amber bridge"),
        ("lineage-canonical", false, "current jade destination"),
    ] {
        let mut row = fixture_entry(id, &format!("/notes/{id}"), content);
        row.archived = archived;
        row.vector = Some(vec![0.5; 1024]);
        insert(&mut store, row);
    }
    assert!(store
        .supersede_memory("lineage-old", "lineage-middle")
        .expect("link old to middle"));
    assert!(store
        .supersede_memory("lineage-middle", "lineage-canonical")
        .expect("link middle to canonical"));

    let baseline = run_recall_coverage_probe(&store, RecallCoverageOptions::default())
        .expect("legacy baseline");
    let exact_totals_bytes = |report: &crate::RecallCoverageReport| {
        serde_json::to_vec(&json!({
            "surfaced": report.surfaced,
            "not_surfaced": report.not_surfaced,
        }))
        .expect("serialize exact totals")
    };
    let corpus = RecallCoverageEquivalenceCorpus {
        schema_version: RECALL_COVERAGE_EQUIVALENCE_SCHEMA_VERSION.to_string(),
        expected_ids: vec!["lineage-old".to_string()],
        equivalences: Vec::new(),
    };
    let report =
        run_recall_coverage_probe_with_corpus(&store, RecallCoverageOptions::default(), &corpus)
            .expect("integrated expected-id coverage");
    assert_eq!(
        exact_totals_bytes(&report),
        exact_totals_bytes(&baseline),
        "the independent expected-ID lane must leave legacy exact totals byte-for-byte unchanged"
    );

    assert_eq!(report.expected_id_lane.requested, 1);
    assert_eq!(report.expected_id_lane.probed, 1);
    assert_eq!(report.expected_id_lane.exact_metrics.hits, 0);
    assert_eq!(report.expected_id_lane.canonical_fact_metrics.hits, 1);
    let evidence = report
        .expected_id_lane
        .cases
        .first()
        .expect("one expected-id case");
    assert_eq!(evidence.outcome, RecallCoverageOutcome::NotSurfaced);
    assert_eq!(
        evidence.canonical_fact_outcome,
        RecallCoverageOutcome::Surfaced
    );
    assert_eq!(evidence.canonical_id.as_deref(), Some("lineage-canonical"));
    assert_eq!(
        evidence.matched_canonical_id.as_deref(),
        Some("lineage-canonical")
    );
    assert_eq!(evidence.canonical_rank, Some(1));
    assert_eq!(
        evidence.fact_evidence.kind,
        RecallCoverageEvidenceKind::StoredSupersessionLineage
    );
    assert_eq!(evidence.fact_evidence.source, "memories.superseded_by");
    assert_eq!(
        evidence.fact_evidence.lineage,
        ["lineage-old", "lineage-middle", "lineage-canonical"]
    );
    assert!(
        evidence
            .canonical_candidate_legs
            .is_some_and(|legs| legs.vector),
        "the integrated lineage hit must carry executed vector-leg evidence"
    );

    let json = serde_json::to_value(&report).expect("serialize integrated report");
    assert_eq!(json["expected_id_lane"]["exact_metrics"]["denominator"], 1);
    assert_eq!(
        json["expected_id_lane"]["canonical_fact_metrics"]["hits"],
        1
    );
    let human = crate::format_recall_coverage_human(&report);
    assert!(human.contains("Reviewed Expected-ID Exact Recall: 0/1"));
    assert!(human.contains("Reviewed Expected-ID Canonical Fact/Lineage Recall: 1/1"));
    assert!(human.contains("evidence_source=memories.superseded_by"));
    assert!(!human.contains("obsolete cobalt origin"));
    assert!(!human.contains("current jade destination"));
}
