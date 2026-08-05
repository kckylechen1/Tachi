//! tachi#1348-A: byte-exact dedupe plan/apply/restore, proven reachable and
//! correct through the genuinely no-admin portable API — the same isolation
//! contract [`portable_contract.rs`] proves for `upsert_batch`/
//! `import_snapshot_batch`/the outbox. Only compiled in isolation, via
//! `required-features = ["portable-contract-test"]`, so workspace feature
//! unification cannot mask an accidental admin dependency the way it would
//! inside a normal `cargo test` run (same rationale as `portable_contract.rs`
//! tachi#1599's doc comment).
//!
//! These are ports of the SHAPE of the `memcore::store::exact_dedupe` unit
//! tests (`crates/memcore/src/store/exact_dedupe.rs` `mod tests`), not a
//! byte-for-byte copy: that suite reaches into `store.conn` (private outside
//! `memcore`) for fixture setup and raw-row assertions, which a portable
//! consumer cannot do. Every fixture and assertion below goes through the
//! same public API surface an external embedder has — `upsert`, `add_edge`,
//! `get`/`get_with_options`, `get_edges`, `supersession_target` — which is
//! the point: this file is a golden log of what the plan/apply/restore
//! kernel guarantees to a caller with no admin surface and no raw SQL door.
//! Fixture bodies are kept distinct per tachi#1572 discipline so an
//! accidental cross-test collision fails loudly instead of silently forming
//! an unintended dedupe group.

use portable_kernel::store::exact_dedupe::{ExactDedupeReceiptDbState, ExactDedupeReceiptPhase};
use portable_kernel::{MemoryEdge, MemoryEntry, MemoryStore};

fn entry(id: &str, path: &str, text: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.into(),
        path: path.into(),
        summary: format!("{id} summary"),
        text: text.into(),
        importance: 0.7,
        timestamp: "2026-07-09T00:00:00Z".into(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: String::new(),
        keywords: vec![],
        persons: vec![],
        entities: vec![],
        location: String::new(),
        source: "manual".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: None,
        domain: None,
        metadata: serde_json::Value::Object(Default::default()),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".into(),
    }
}

/// The `retention_policy: pinned` ranking edge memcore's `order()` uses to
/// deterministically pick a group's winner (`retention_rank` is the primary
/// sort key ahead of every other candidate signal).
fn pinned(mut e: MemoryEntry) -> MemoryEntry {
    e.retention_policy = Some("pinned".into());
    e
}

fn edge(source: &str, target: &str, relation: &str) -> MemoryEdge {
    MemoryEdge {
        source_id: source.into(),
        target_id: target.into(),
        relation: relation.into(),
        weight: 1.0,
        metadata: serde_json::Value::Object(Default::default()),
        created_at: String::new(),
        valid_from: String::new(),
        valid_to: None,
    }
}

/// (a) happy path: plan -> apply -> a followup plan is empty. Also carries
/// (e), the missing vec-conservation assertion (memcore
/// `successful_apply_soft_archives_and_followup_plan_is_empty`, ~L1935-2030):
/// the loser is soft-archived, not deleted, so its `memories_vec` row must
/// survive the apply exactly like its `memories` row does.
#[test]
fn portable_build_exact_dedupe_plan_apply_and_followup_plan_is_empty() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    let mut winner = pinned(entry(
        "dedupe-happy-winner",
        "/portable/dedupe/happy",
        "byte identical portable dedupe happy path text",
    ));
    winner.vector = Some(vec![0.125_f32; 1024]);
    store.upsert(&winner).expect("upsert winner");
    let mut loser = entry(
        "dedupe-happy-loser",
        "/portable/dedupe/happy",
        "byte identical portable dedupe happy path text",
    );
    loser.vector = Some(vec![0.625_f32; 1024]);
    store.upsert(&loser).expect("upsert loser");

    let (_before_total, before_with_vec) = store.vector_stats().expect("vector_stats before apply");
    assert_eq!(
        before_with_vec, 2,
        "both winner and loser must have landed a memories_vec row before apply"
    );

    let plan = store
        .plan_exact_dedupe(":memory:".into(), None, None)
        .expect("plan");
    assert_eq!(plan.groups.len(), 1);
    assert_eq!(plan.groups[0].winner.id, "dedupe-happy-winner");
    assert_eq!(plan.groups[0].losers.len(), 1);
    assert_eq!(plan.groups[0].losers[0].id, "dedupe-happy-loser");

    let result = store.apply_exact_dedupe(&plan).expect("apply");
    assert_eq!(result.applied_groups, 1);
    assert_eq!(result.applied_losers, 1);
    assert_eq!(result.receipt.phase, ExactDedupeReceiptPhase::Committed);

    // Soft-archive, not delete: the loser row still exists (visible only
    // with `include_archived`), and its memories_vec projection must be
    // untouched by the apply — a portable consumer with no raw SQL door has
    // no other way to see a silent vector row leak/drop on archive.
    assert!(store.get("dedupe-happy-loser").expect("get").is_none());
    let archived_loser = store
        .get_with_options("dedupe-happy-loser", true)
        .expect("get_with_options")
        .expect("archived loser row must still exist");
    assert!(archived_loser.archived);
    assert_eq!(
        store
            .supersession_target("dedupe-happy-loser")
            .expect("supersession_target"),
        Some(Some("dedupe-happy-winner".into()))
    );

    let (_after_total, after_with_vec) = store.vector_stats().expect("vector_stats after apply");
    assert_eq!(
        after_with_vec, before_with_vec,
        "memories_vec row count must be unchanged across a soft-archive apply"
    );

    let followup = store
        .plan_exact_dedupe(":memory:".into(), None, None)
        .expect("followup plan");
    assert!(
        followup.groups.is_empty(),
        "an already-applied group must not replan"
    );
}

/// (b) one drift refusal: re-upserting a planned loser between plan and
/// apply bumps `memories.revision` (the shared ON CONFLICT body in
/// `memcore::db::memory_crud` always does `revision = memories.revision + 1`
/// on an existing id) without changing anything else about the row — exactly
/// the revision-drift shape `apply_rejects_wrong_target_and_all_drift_with_
/// batch_rollback`'s `"revision=revision+1"` case exercises against raw SQL.
/// Apply's read-only revalidation pass runs to completion across every group
/// before any mutation begins, so a drift on the SECOND group refuses before
/// touching the first — the batch-rollback property this asserts.
#[test]
fn portable_build_exact_dedupe_apply_refuses_revision_drift_with_batch_rollback() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    let fixtures = [
        (
            "dedupe-drift-a1",
            "/portable/dedupe/drift-a",
            "portable dedupe drift group a duplicate text",
        ),
        (
            "dedupe-drift-a2",
            "/portable/dedupe/drift-a",
            "portable dedupe drift group a duplicate text",
        ),
        (
            "dedupe-drift-b1",
            "/portable/dedupe/drift-b",
            "portable dedupe drift group b duplicate text",
        ),
        (
            "dedupe-drift-b2",
            "/portable/dedupe/drift-b",
            "portable dedupe drift group b duplicate text",
        ),
    ];
    for (id, path, text) in fixtures {
        store.upsert(&entry(id, path, text)).expect("upsert");
    }

    let plan = store
        .plan_exact_dedupe(":memory:".into(), None, None)
        .expect("plan");
    assert_eq!(plan.groups.len(), 2, "two same-path/text groups expected");

    // Drift group b's loser behind the plan's back with a re-save of the
    // exact same content — content-identical, revision-different.
    store
        .upsert(&entry(
            "dedupe-drift-b2",
            "/portable/dedupe/drift-b",
            "portable dedupe drift group b duplicate text",
        ))
        .expect("re-upsert to drift revision");

    let error = store
        .apply_exact_dedupe(&plan)
        .expect_err("revision drift must be refused");
    assert!(
        error.to_string().contains("drifted"),
        "unexpected refusal: {error}"
    );

    // Batch rollback: every row from every group — including group a, which
    // sorts and would have been applied BEFORE the drifted group b — must be
    // untouched.
    for (id, ..) in fixtures {
        let row = store
            .get_with_options(id, true)
            .expect("get_with_options")
            .unwrap_or_else(|| panic!("{id} must still exist"));
        assert!(
            !row.archived,
            "{id} must not be archived after a refused apply"
        );
        assert_eq!(
            store.supersession_target(id).expect("supersession_target"),
            Some(None),
            "{id} must not have gained a supersession edge"
        );
    }
}

/// (c) edge transfer + dedup + self-loop drop, ported from memcore's
/// `apply_transfers_edges_to_winner_dedupes_and_drops_self_loops` — through
/// [`MemoryStore::add_edge`] instead of a raw `memory_edges` insert, so the
/// edges use ontology-v1-legal relations (`related_to` is write-deprecated,
/// see `relation_ontology.rs`) rather than the internal test's grandfathered
/// values.
#[test]
fn portable_build_exact_dedupe_apply_transfers_edges_dedupes_and_drops_self_loops() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    store
        .upsert(&pinned(entry(
            "dedupe-edges-winner",
            "/portable/dedupe/edges",
            "portable dedupe edge transfer duplicate text",
        )))
        .expect("upsert winner");
    store
        .upsert(&entry(
            "dedupe-edges-loser",
            "/portable/dedupe/edges",
            "portable dedupe edge transfer duplicate text",
        ))
        .expect("upsert loser");
    store
        .upsert(&entry(
            "dedupe-edges-neighbor-in",
            "/portable/dedupe/edges-neighbor-in",
            "portable dedupe edge transfer neighbor-in text",
        ))
        .expect("upsert neighbor-in");
    store
        .upsert(&entry(
            "dedupe-edges-neighbor-out",
            "/portable/dedupe/edges-neighbor-out",
            "portable dedupe edge transfer neighbor-out text",
        ))
        .expect("upsert neighbor-out");

    // A -> loser: must transfer to A -> winner.
    store
        .add_edge(&edge(
            "dedupe-edges-neighbor-in",
            "dedupe-edges-loser",
            "supports",
        ))
        .expect("add A->loser edge");
    // loser -> B: must transfer to winner -> B, colliding with (and being
    // deduped against) an edge the winner already carries under the same
    // relation.
    store
        .add_edge(&edge(
            "dedupe-edges-loser",
            "dedupe-edges-neighbor-out",
            "supports",
        ))
        .expect("add loser->B edge");
    store
        .add_edge(&edge(
            "dedupe-edges-winner",
            "dedupe-edges-neighbor-out",
            "supports",
        ))
        .expect("seed pre-existing winner->B edge");
    // Both loser<->winner directions become winner->winner self-loops after
    // the rename and must be dropped, not inserted.
    store
        .add_edge(&edge(
            "dedupe-edges-loser",
            "dedupe-edges-winner",
            "supports",
        ))
        .expect("add loser->winner edge");
    store
        .add_edge(&edge(
            "dedupe-edges-winner",
            "dedupe-edges-loser",
            "elaborates",
        ))
        .expect("add winner->loser edge");

    let plan = store
        .plan_exact_dedupe(":memory:".into(), None, None)
        .expect("plan");
    assert_eq!(plan.groups.len(), 1, "{plan:#?}");
    store.apply_exact_dedupe(&plan).expect("apply");

    let mut observed: Vec<(String, String, String)> = store
        .get_edges("dedupe-edges-winner", "both", None)
        .expect("get_edges")
        .into_iter()
        .map(|e| (e.source_id, e.target_id, e.relation))
        .collect();
    observed.sort();
    assert_eq!(
        observed,
        vec![
            (
                "dedupe-edges-neighbor-in".to_string(),
                "dedupe-edges-winner".to_string(),
                "supports".to_string()
            ),
            (
                "dedupe-edges-winner".to_string(),
                "dedupe-edges-neighbor-out".to_string(),
                "supports".to_string()
            ),
        ]
    );
    assert!(
        observed.iter().all(|(source, target, _)| source != target),
        "no self-loop should survive the merge"
    );
}

/// (d) receipt round-trip: `apply_exact_dedupe`'s committed receipt restores
/// through the public [`MemoryStore::restore_exact_dedupe`] door, undoing
/// exactly the archive/supersede state it recorded — the portable-reachable
/// slice of memcore's
/// `apply_produces_a_durable_receipt_and_restore_is_hash_bound_and_reversible`.
#[test]
fn portable_build_exact_dedupe_apply_receipt_round_trips_through_restore() {
    let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
    store
        .upsert(&pinned(entry(
            "dedupe-restore-winner",
            "/portable/dedupe/restore",
            "portable dedupe restore round trip duplicate text",
        )))
        .expect("upsert winner");
    store
        .upsert(&entry(
            "dedupe-restore-loser",
            "/portable/dedupe/restore",
            "portable dedupe restore round trip duplicate text",
        ))
        .expect("upsert loser");

    let plan = store
        .plan_exact_dedupe(":memory:".into(), None, None)
        .expect("plan");
    let result = store.apply_exact_dedupe(&plan).expect("apply");
    let receipt = result.receipt;
    receipt.validate().expect("committed receipt validates");
    assert_eq!(receipt.phase, ExactDedupeReceiptPhase::Committed);
    assert_eq!(receipt.applied_losers, 1);
    assert_eq!(receipt.rows[0].loser_id, "dedupe-restore-loser");
    assert_eq!(receipt.rows[0].winner_id, "dedupe-restore-winner");
    assert_eq!(
        store
            .classify_exact_dedupe_receipt_db_state(&receipt)
            .expect("classify committed receipt"),
        ExactDedupeReceiptDbState::Applied
    );

    let before_revision = receipt.rows[0].before_revision;
    let restored = store
        .restore_exact_dedupe(&receipt)
        .expect("restore the committed receipt");
    assert_eq!(restored.restored_losers, 1);

    let loser = store
        .get("dedupe-restore-loser")
        .expect("get")
        .expect("loser visible again after restore");
    assert!(!loser.archived);
    // `revision` is a CAS counter, not a value that restore rewinds: every
    // UPDATE in exact_dedupe.rs (apply's archive mutation *and* restore's
    // un-archive mutation) does `revision=revision+1`
    // (crates/memcore/src/store/exact_dedupe.rs apply_exact_dedupe L814,
    // restore_exact_dedupe L1009/L1026). So a round trip through
    // apply -> restore bumps revision twice (before_revision -> +1 on
    // apply -> +1 on restore), landing on before_revision + 2, never back
    // on before_revision. The restore contract is "lifecycle fields
    // reopened, revision advances monotonically" — not "revision returns
    // to its pre-apply value". memcore's own restore tests (e.g.
    // `apply_lineage_preserves_non_object_metadata_and_remains_restorable`
    // in exact_dedupe.rs) likewise never assert revision equality after a
    // real restore.
    assert!(
        loser.revision > before_revision,
        "restore must advance revision monotonically, not rewind it: before={before_revision} after={}",
        loser.revision
    );
    assert_eq!(
        store
            .supersession_target("dedupe-restore-loser")
            .expect("supersession_target"),
        Some(None)
    );
    // classify_exact_dedupe_receipt_db_state is documented as scoped to
    // reconciling a *prepared* receipt around the apply commit boundary —
    // its `NotApplied` branch requires `revision == before_revision`
    // (exact_dedupe.rs L962), which models "the apply transaction never
    // touched this row", not "this row was later restored". Since restore
    // advances revision past before_revision (see above) and clears the
    // apply lineage row, a genuinely restored receipt matches neither the
    // `Applied` nor `NotApplied` terminal shape the function distinguishes,
    // so it correctly reports Indeterminate here.
    assert_eq!(
        store
            .classify_exact_dedupe_receipt_db_state(&receipt)
            .expect("classify after restore"),
        ExactDedupeReceiptDbState::Indeterminate
    );

    // Replaying the SAME untampered receipt a second time must not be
    // possible: the row's live state no longer matches the receipt's
    // archived-CAS binding (it was just restored to unarchived above), so
    // the restore's per-row CAS predicate refuses it.
    let error = store
        .restore_exact_dedupe(&receipt)
        .expect_err("a second restore of an already-restored receipt must be refused");
    assert!(
        error.to_string().contains("restore CAS failed"),
        "unexpected recovery refusal: {error}"
    );
}
