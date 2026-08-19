use super::apply::*;
use super::classify::*;
use super::fs::*;
use super::legacy::*;
use super::plan::*;
use super::repair::*;
use super::types::*;
use super::*;

use memcore::db::migrations::EXPECTED_SCHEMA_VERSION;
use memcore::{ExpectedMemoryState, MemoryEntry, MemoryStore};
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_wiki_corpus_command_with_race_hook(
    apply: bool,
    confirm: Option<String>,
    backup_dir: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
    race_hook: CorpusRaceHook,
) -> Result<WikiCorpusReport, String> {
    run_wiki_corpus_command_internal(
        apply,
        confirm,
        backup_dir,
        plan_path,
        global_db,
        project_db,
        app_home,
        Some(race_hook),
    )
}

fn fixture_entry(id: &str, path: &str, metadata: Value) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: path.to_string(),
        summary: "fixture summary".to_string(),
        text: "fixture text".to_string(),
        importance: 0.5,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        valid_from: "2026-01-01T00:00:00Z".to_string(),
        valid_until: None,
        category: "wiki".to_string(),
        topic: "fixture".to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "wiki".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        vector: None,
        retention_policy: Some("permanent".to_string()),
        domain: Some("wiki".to_string()),
        metadata,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

fn create_current_fixture(path: &Path, entries: &[MemoryEntry]) {
    let mut store = MemoryStore::open(&path.display().to_string()).unwrap();
    for entry in entries {
        store.upsert(entry).unwrap();
    }
}

fn remove_memory_hard_delete_guard(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("DROP TRIGGER IF EXISTS wiki_corpus_no_hard_delete")
        .unwrap();
}

fn fixture_entry_with_vector(
    id: &str,
    path: &str,
    metadata: Value,
    vector: Vec<f32>,
) -> MemoryEntry {
    let mut entry = fixture_entry(id, path, metadata);
    entry.vector = Some(vector);
    entry
}

fn db_snapshot(path: &Path) -> BTreeMap<String, Vec<u8>> {
    ["", "-wal", "-shm"]
        .into_iter()
        .filter_map(|suffix| {
            let candidate = PathBuf::from(format!("{}{}", path.display(), suffix));
            std::fs::read(&candidate)
                .ok()
                .map(|bytes| (candidate.display().to_string(), bytes))
        })
        .collect()
}

fn db_snapshot_fingerprint(
    snapshot: &BTreeMap<String, Vec<u8>>,
) -> BTreeMap<String, (usize, String)> {
    snapshot
        .iter()
        .map(|(path, bytes)| {
            (
                path.clone(),
                (bytes.len(), format!("{:x}", Sha256::digest(bytes))),
            )
        })
        .collect()
}

fn directory_snapshot(path: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

fn create_schema_18_fixture(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                summary TEXT NOT NULL,
                text TEXT NOT NULL,
                importance REAL NOT NULL,
                timestamp TEXT NOT NULL,
                valid_from TEXT NOT NULL,
                valid_until TEXT,
                category TEXT NOT NULL,
                topic TEXT NOT NULL,
                keywords TEXT NOT NULL,
                entities TEXT NOT NULL,
                source TEXT NOT NULL,
                scope TEXT NOT NULL,
                archived INTEGER NOT NULL,
                revision INTEGER NOT NULL,
                metadata TEXT
            );
            PRAGMA user_version = 18;",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories
             (id, path, summary, text, importance, timestamp, valid_from,
              valid_until, category, topic, keywords, entities, source, scope,
              archived, revision, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10, ?11,
                     ?12, ?13, 0, 1, ?14)",
        rusqlite::params![
            "legacy-wiki",
            "/wiki/legacy",
            "legacy",
            "legacy text",
            0.5_f64,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
            "wiki",
            "legacy",
            "[]",
            "[]",
            "wiki",
            "general",
            r#"{"wiki":true}"#,
        ],
    )
    .unwrap();
}

fn fixture_scan(store: LogicalStore, path: &Path) -> StoreScan {
    inventory_store(StoreSpec {
        logical_store: store,
        addressed_path: Some(path.to_path_buf()),
        resolution_error: None,
    })
}

fn classify_scans(scans: &mut [StoreScan]) {
    finalize_classifications(scans);
    for scan in scans {
        refresh_report(scan);
    }
}

fn item_for(source: &RawRow) -> PlanItem {
    let replay_identity = replay_identity_for(
        LogicalStore::LegacyGlobal.reference(),
        "unix:source",
        source,
        false,
    );
    PlanItem {
        action: "copy_to_shared_and_supersede".to_string(),
        source_store_ref: LogicalStore::LegacyGlobal.reference().to_string(),
        source_physical_id: "unix:source".to_string(),
        source_id: source.id.clone(),
        source_path: source.path.clone(),
        normalized_path: source.normalized_path(),
        source_revision: source.revision,
        source_content_sha256: source.content_sha256(),
        source_copy_identity_sha256: source.copy_identity_sha256(),
        source_valid_until: source.valid_until.clone(),
        source_superseded_by: source.superseded_by.clone(),
        source_vector: source.vector_fingerprint(false),
        replay_identity: replay_identity.clone(),
        target_store_ref: LogicalStore::SharedWiki.reference().to_string(),
        target_physical_id: "unix:target".to_string(),
        target_id: Some(format!("wiki-corpus:{replay_identity}")),
    }
}

fn row(store: LogicalStore, id: &str, path: &str, metadata: Value) -> RawRow {
    RawRow {
        id: id.to_string(),
        path: path.to_string(),
        summary: "summary".to_string(),
        text: "text".to_string(),
        importance: 0.5,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
        valid_from: "2026-01-01T00:00:00Z".to_string(),
        valid_until: None,
        category: "wiki".to_string(),
        topic: "topic".to_string(),
        keywords: Vec::new(),
        entities: Vec::new(),
        source: "wiki".to_string(),
        scope: if store == LogicalStore::BoundProject {
            "general"
        } else {
            "general"
        }
        .to_string(),
        archived: false,
        revision: 1,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        retention_policy: Some("permanent".to_string()),
        domain: Some("wiki".to_string()),
        metadata,
        vector: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
        superseded_by: None,
        metadata_parse_error: None,
        classification: None,
        reasons: Vec::new(),
        effective: None,
    }
}

fn shared_metadata() -> Value {
    json!({
        "artifact_kind": "wiki",
        "knowledge_scope": "shared",
        "origin_projects": ["sigil"],
        "applies_to": {"projects": ["sigil"]},
        "lifecycle": "candidate"
    })
}

#[test]
fn classifier_keeps_project_general_and_rejects_underspecified_shared() {
    let project = row(
        LogicalStore::BoundProject,
        "project",
        "/wiki/project",
        json!({}),
    );
    let (classification, reasons, _, _) = classify_row(LogicalStore::BoundProject, &project);
    assert_eq!(classification, CorpusClassification::ProjectBound);
    assert!(reasons
        .iter()
        .any(|reason| reason.contains("legacy_scope_general")));

    let malformed = row(
        LogicalStore::LegacyGlobal,
        "malformed",
        "/wiki/malformed",
        json!({"knowledge_scope":"shared","origin_projects":[],"applies_to":{}}),
    );
    let (classification, _, _, _) = classify_row(LogicalStore::LegacyGlobal, &malformed);
    assert_eq!(classification, CorpusClassification::ManualReview);
}

#[test]
fn authority_operational_and_test_markers_beat_wiki_category() {
    let lane = row(
        LogicalStore::LegacyGlobal,
        "lane",
        "/wiki/lane",
        json!({"record_kind":"lane_card","knowledge_scope":"shared","origin_projects":["sigil"],"applies_to":{"projects":["sigil"]}}),
    );
    assert_eq!(
        classify_row(LogicalStore::LegacyGlobal, &lane).0,
        CorpusClassification::AuthorityRecord
    );

    let operation = row(
        LogicalStore::LegacyGlobal,
        "operation",
        "/wiki",
        json!({"snapshot_kind":"operational"}),
    );
    assert_eq!(
        classify_row(LogicalStore::LegacyGlobal, &operation).0,
        CorpusClassification::OperationalSnapshot
    );

    let rem_operation = row(
        LogicalStore::LegacyGlobal,
        "wiki-rem:operation",
        "/wiki/rem",
        json!({"rem": {"operation": "recover"}}),
    );
    assert_eq!(
        classify_row(LogicalStore::LegacyGlobal, &rem_operation).0,
        CorpusClassification::OperationalSnapshot
    );

    let recall_cache = row(
        LogicalStore::LegacyGlobal,
        "foundry:recall-cache:fixture",
        "/wiki/recall-cache/fixture",
        json!({}),
    );
    assert_eq!(
        classify_row(LogicalStore::LegacyGlobal, &recall_cache).0,
        CorpusClassification::OperationalSnapshot
    );

    let mut recall_cache_outside_wiki = row(
        LogicalStore::LegacyGlobal,
        "foundry:recall-cache:outside-wiki",
        "/recall-cache",
        json!({}),
    );
    recall_cache_outside_wiki.category = "memory".to_string();
    recall_cache_outside_wiki.source = "runtime".to_string();
    recall_cache_outside_wiki.domain = None;
    assert!(is_wiki_related(&recall_cache_outside_wiki));
    assert_eq!(
        classify_row(LogicalStore::LegacyGlobal, &recall_cache_outside_wiki).0,
        CorpusClassification::OperationalSnapshot
    );

    let mut rem_outside_wiki = row(
        LogicalStore::LegacyGlobal,
        "wiki-rem:outside-wiki",
        "/operations",
        json!({"rem": {"operation": "recover"}}),
    );
    rem_outside_wiki.category = "memory".to_string();
    rem_outside_wiki.source = "runtime".to_string();
    rem_outside_wiki.domain = None;
    assert!(is_wiki_related(&rem_outside_wiki));
    assert_eq!(
        classify_row(LogicalStore::LegacyGlobal, &rem_outside_wiki).0,
        CorpusClassification::OperationalSnapshot
    );

    let fixture = row(
        LogicalStore::LegacyGlobal,
        "fixture",
        "/wiki/fixture",
        json!({"fixture":true}),
    );
    assert_eq!(
        classify_row(LogicalStore::LegacyGlobal, &fixture).0,
        CorpusClassification::TestEphemeral
    );
}

#[test]
fn valid_shared_requires_bounded_typed_applicability() {
    let candidate = row(
        LogicalStore::LegacyGlobal,
        "candidate",
        "/wiki/candidate",
        shared_metadata(),
    );
    assert_eq!(
        classify_row(LogicalStore::LegacyGlobal, &candidate).0,
        CorpusClassification::SharedCandidate
    );
}

#[test]
fn production_copy_only_completion_is_honest_and_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry(
            "source",
            "/wiki/copy-only",
            shared_metadata(),
        )],
    );
    create_current_fixture(&target_path, &[]);
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    let item = &plan.items[0];
    let target_id = item.target_id.as_deref().unwrap();
    let source_store = open_apply_store(&scans[0]).unwrap();
    let mut target_store = open_apply_store(&scans[1]).unwrap();
    let source_entry = source_store
        .get_with_options(&item.source_id, true)
        .unwrap()
        .unwrap();
    let source_row = raw_from_entry(&source_entry);
    let mut target_entry = source_entry;
    target_entry.id = target_id.to_string();
    target_entry.metadata = metadata_with_receipt(
        &source_row,
        receipt_value(item, &plan.plan_id, "target_copied"),
    )
    .unwrap();
    target_store.insert_if_absent(&target_entry).unwrap();

    for _ in 0..2 {
        let outcome = copy_only_outcome_from_fresh_state(
            &source_store,
            &target_store,
            item,
            &plan.plan_id,
            target_id,
        )
        .expect("copy-only completion must be replay-safe");
        assert_eq!(outcome.outcome, "copied_without_supersession");
        assert_eq!(outcome.phases, vec!["target_copied"]);
    }
    let source_after = source_store
        .get_with_options(&item.source_id, true)
        .unwrap()
        .unwrap();
    assert!(!source_after.archived);
    assert_eq!(
        source_store.supersession_target(&item.source_id).unwrap(),
        Some(None)
    );
    assert!(parse_migration_receipt(&raw_from_entry(&source_after))
        .unwrap()
        .is_none());
}

#[test]
fn duplicate_paths_are_manual_review_across_logical_stores() {
    let mut scans = vec![
        empty_test_scan(
            LogicalStore::BoundProject,
            vec![row(
                LogicalStore::BoundProject,
                "same-project",
                "/wiki/same",
                shared_metadata(),
            )],
        ),
        empty_test_scan(
            LogicalStore::SharedWiki,
            vec![row(
                LogicalStore::SharedWiki,
                "same-shared",
                "/wiki/same",
                shared_metadata(),
            )],
        ),
    ];
    finalize_classifications(&mut scans);
    assert!(scans
        .iter()
        .all(|scan| { scan.rows[0].classification == Some(CorpusClassification::ManualReview) }));
}

fn empty_test_scan(store: LogicalStore, rows: Vec<RawRow>) -> StoreScan {
    StoreScan {
        spec: StoreSpec {
            logical_store: store,
            addressed_path: None,
            resolution_error: None,
        },
        physical: None,
        rows,
        report: StoreReport {
            logical_store_ref: store.reference().to_string(),
            semantic_role: store.semantic_role().to_string(),
            addressed_path: None,
            existence: false,
            resolved_path: None,
            canonical_path: None,
            open_path: None,
            open_path_basis: None,
            physical_identity: None,
            read_failure: None,
            stored_schema: None,
            expected_schema: EXPECTED_SCHEMA_VERSION,
            counts: RowCounts::default(),
            rows: Vec::new(),
        },
        row_digest: String::new(),
        vector_table_present: false,
    }
}

#[test]
fn deterministic_plan_material_and_receipt_are_stable() {
    let candidate = row(
        LogicalStore::LegacyGlobal,
        "candidate",
        "/wiki/candidate",
        shared_metadata(),
    );
    let item = PlanItem {
        action: "copy_to_shared_and_supersede".to_string(),
        source_store_ref: LogicalStore::LegacyGlobal.reference().to_string(),
        source_physical_id: "unix:1:2".to_string(),
        source_id: candidate.id.clone(),
        source_path: candidate.path.clone(),
        normalized_path: "/wiki/candidate".to_string(),
        source_revision: 1,
        source_content_sha256: candidate.content_sha256(),
        source_copy_identity_sha256: candidate.copy_identity_sha256(),
        source_valid_until: candidate.valid_until.clone(),
        source_superseded_by: None,
        source_vector: candidate.vector_fingerprint(false),
        replay_identity: "replay".to_string(),
        target_store_ref: LogicalStore::SharedWiki.reference().to_string(),
        target_physical_id: "unix:3:4".to_string(),
        target_id: Some("wiki-corpus:target".to_string()),
    };
    assert_eq!(plan_item_material(&item), plan_item_material(&item));
    assert_eq!(
        receipt_value(&item, "plan", "target_copied"),
        receipt_value(&item, "plan", "target_copied")
    );
}

#[test]
fn preview_inventory_reads_current_fixture_without_sidecars_or_byte_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("current.db");
    create_current_fixture(
        &path,
        &[fixture_entry("current-wiki", "/wiki/current", json!({}))],
    );
    let before = db_snapshot(&path);

    let mut scans = vec![fixture_scan(LogicalStore::SharedWiki, &path)];
    classify_scans(&mut scans);

    assert_eq!(scans[0].report.stored_schema, Some(EXPECTED_SCHEMA_VERSION));
    assert!(scans[0].report.read_failure.is_none());
    assert_eq!(scans[0].report.counts.total_memory_rows, 1);
    assert_eq!(scans[0].report.counts.wiki_related_rows, 1);
    assert_eq!(
        db_snapshot_fingerprint(&db_snapshot(&path)),
        db_snapshot_fingerprint(&before)
    );
}

#[cfg(unix)]
#[test]
fn preview_staging_directory_is_owner_only_before_any_snapshot_copy() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let controlled_root = tempfile::tempdir().unwrap();
    let staging = preview_staging_dir_in(controlled_root.path()).unwrap();
    let metadata = std::fs::symlink_metadata(&staging).unwrap();

    assert!(metadata.file_type().is_dir());
    assert!(!metadata.file_type().is_symlink());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(std::fs::read_dir(&staging).unwrap().count(), 0);
}

#[test]
fn preview_inventories_schema_18_fixture_and_classifies_it_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-v18.db");
    create_schema_18_fixture(&path);
    let before = db_snapshot(&path);

    let mut scans = vec![fixture_scan(LogicalStore::LegacyGlobal, &path)];
    classify_scans(&mut scans);

    assert_eq!(scans[0].report.stored_schema, Some(18));
    assert_eq!(scans[0].report.expected_schema, EXPECTED_SCHEMA_VERSION);
    assert!(scans[0].report.read_failure.is_none());
    assert_eq!(scans[0].report.counts.total_memory_rows, 1);
    assert_eq!(scans[0].report.counts.wiki_related_rows, 1);
    assert_eq!(
        scans[0].report.rows[0].classification,
        CorpusClassification::ManualReview
    );
    assert_eq!(db_snapshot(&path), before);
}

#[test]
fn apply_confirmation_backup_and_schema_gates_refuse_before_writes() {
    let directory = tempfile::tempdir().unwrap();
    let global = directory.path().join("missing-global.db");
    let app_home = directory.path().join("home");
    let before = directory_snapshot(directory.path());

    let error = run_wiki_corpus_command(
        true,
        Some("wrong-token".to_string()),
        Some(directory.path().join("backups")),
        None,
        &global,
        None,
        &app_home,
    )
    .unwrap_err();
    assert!(error.contains("exact --confirm"));
    assert_eq!(directory_snapshot(directory.path()), before);

    let error = run_wiki_corpus_command(
        true,
        Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
        Some(directory.path().join("backups")),
        None,
        &global,
        None,
        &app_home,
    )
    .unwrap_err();
    assert!(error.contains("existing backup directory"));
    assert_eq!(directory_snapshot(directory.path()), before);

    let legacy_path = directory.path().join("legacy-v18.db");
    create_schema_18_fixture(&legacy_path);
    let before_legacy = db_snapshot(&legacy_path);
    let legacy_scan = fixture_scan(LogicalStore::LegacyGlobal, &legacy_path);
    let error = validate_apply_inventory(&[legacy_scan]).unwrap_err();
    assert!(error.contains(&format!(
        "stored Some(18), expected {}",
        memcore::db::migrations::EXPECTED_SCHEMA_VERSION
    )));
    assert_eq!(db_snapshot(&legacy_path), before_legacy);
}

#[test]
fn deterministic_target_occupant_mismatch_fails_closed() {
    let source = row(
        LogicalStore::LegacyGlobal,
        "source",
        "/wiki/occupant",
        shared_metadata(),
    );
    let item = item_for(&source);
    let mut occupant = source.clone();
    occupant.id = item.target_id.clone().unwrap();
    occupant.text = "different content".to_string();
    let target = empty_test_scan(LogicalStore::SharedWiki, vec![occupant]);

    let error = validate_target_occupant(&item, "plan", &source, &target).unwrap_err();
    assert!(error.contains("deterministic target occupant collision"));
}

#[test]
fn partial_target_phase_replays_without_delete_or_revision_churn() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    let source_entry = fixture_entry("source", "/wiki/replay", shared_metadata());
    create_current_fixture(&source_path, &[source_entry.clone()]);
    create_current_fixture(&target_path, &[]);

    let mut initial_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut initial_scans);
    let plan = build_plan(&initial_scans).unwrap();
    assert_eq!(plan.items.len(), 1);
    let item = &plan.items[0];
    let target_id = item.target_id.as_deref().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    for scan in &initial_scans {
        let expected = scan.fingerprint().unwrap();
        create_or_verify_backup(
            scan,
            &backup_dir,
            vec![scan.spec.logical_store.reference().to_string()],
            &expected,
        )
        .unwrap();
    }

    let mut target_store =
        MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
    let mut target_entry = source_entry.clone();
    target_entry.id = target_id.to_string();
    target_entry.metadata = metadata_with_receipt(
        &raw_from_entry(&source_entry),
        receipt_value(item, &plan.plan_id, "target_copied"),
    )
    .unwrap();
    target_store.insert_if_absent(&target_entry).unwrap();
    drop(target_store);

    let mut resumed_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut resumed_scans);
    let (_, outcomes) = apply_plan(&resumed_scans, &plan, &backup_dir).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].phases.contains(&"target_copied".to_string()));
    assert!(outcomes[0]
        .phases
        .contains(&"source_superseded".to_string()));

    let source_after_first = inventory_store(StoreSpec {
        logical_store: LogicalStore::LegacyGlobal,
        addressed_path: Some(source_path.clone()),
        resolution_error: None,
    });
    let source_row = source_after_first.raw_row("source").unwrap();
    assert_eq!(source_row.superseded_by.as_deref(), Some(target_id));
    assert!(source_after_first.raw_row("source").is_some());
    assert!(inventory_store(StoreSpec {
        logical_store: LogicalStore::SharedWiki,
        addressed_path: Some(target_path.clone()),
        resolution_error: None,
    })
    .raw_row(target_id)
    .is_some());

    let source_bytes = db_snapshot(&source_path);
    let target_bytes = db_snapshot(&target_path);
    let backup_bytes = directory_snapshot(&backup_dir);
    let mut replay_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut replay_scans);
    let (_, replay_outcomes) = apply_plan(&replay_scans, &plan, &backup_dir).unwrap();
    assert_eq!(replay_outcomes[0].outcome, "existing_no_op");
    assert_eq!(db_snapshot(&source_path), source_bytes);
    assert_eq!(db_snapshot(&target_path), target_bytes);
    assert_eq!(directory_snapshot(&backup_dir), backup_bytes);
}

#[test]
fn completed_supersession_replay_rejects_missing_deterministic_target() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry(
            "source",
            "/wiki/missing-target",
            shared_metadata(),
        )],
    );
    create_current_fixture(&target_path, &[]);

    let mut initial_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut initial_scans);
    let plan = build_plan(&initial_scans).unwrap();
    let target_id = plan.items[0].target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    apply_plan(&initial_scans, &plan, &backup_dir).unwrap();

    // Simulate a foreign writer; migration paths do not issue hard deletes.
    let target_connection = Connection::open(&target_path).unwrap();
    assert_eq!(
        target_connection
            .execute(
                "DELETE FROM memories WHERE id = ?1",
                rusqlite::params![target_id],
            )
            .unwrap(),
        1
    );
    drop(target_connection);

    let mut replay_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut replay_scans);
    let error = apply_plan(&replay_scans, &plan, &backup_dir)
        .expect_err("a completed source with a missing target must not be a no-op");
    assert!(
        error.contains("deterministic target") || error.contains("source receipt"),
        "{error}"
    );

    let source = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
    let source_row = source.raw_row("source").unwrap();
    assert_eq!(
        source_row.superseded_by.as_deref(),
        Some(target_id.as_str())
    );
    assert_eq!(
        parse_migration_receipt(source_row).unwrap().unwrap().phase,
        MigrationPhase::SourceSuperseded
    );
    assert!(fixture_scan(LogicalStore::SharedWiki, &target_path)
        .raw_row(&target_id)
        .is_none());
}

#[test]
fn supplied_plan_rejects_unkeyed_tamper_after_canonical_rederivation() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry("source", "/wiki/tamper", shared_metadata())],
    );
    create_current_fixture(&target_path, &[]);

    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    validate_plan(&scans, &plan).unwrap();

    let mut tampered = plan.clone();
    tampered.items[0].target_id = Some("wiki-corpus:forged-target".to_string());
    // This is the legacy unkeyed digest an attacker could recompute after
    // editing the serialized item. The current validator must still
    // rederive the item and the complete plan from live stores.
    tampered.plan_id = digest_string(
        &tampered
            .items
            .iter()
            .map(plan_item_material)
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let error = validate_plan(&scans, &tampered).unwrap_err();
    assert!(error.contains("canonical") || error.contains("rederivation"));
}

#[cfg(unix)]
#[test]
fn backup_and_manifest_reservations_reject_symlinks_without_following_them() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry("source", "/wiki/backup", shared_metadata())],
    );
    let source = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
    let expected = source.fingerprint().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    let backup_path = backup_dir.join(backup_file_name(
        &source.physical.as_ref().unwrap().physical_id,
    ));
    let backup_victim = directory.path().join("backup-victim");
    symlink(&backup_victim, &backup_path).unwrap();

    let error = create_or_verify_backup(
        &source,
        &backup_dir,
        vec![LogicalStore::LegacyGlobal.reference().to_string()],
        &expected,
    )
    .unwrap_err();
    assert!(error.contains("regular non-symlink"));
    assert!(!backup_victim.exists());

    let manifest_path = backup_dir.join("wiki-corpus-v1-manifest.json");
    let manifest_victim = directory.path().join("manifest-victim");
    let manifest = BackupManifest {
        version: REPORT_VERSION.to_string(),
        status: "backups_verified".to_string(),
        plan_id: "plan".to_string(),
        backup_directory: backup_dir.display().to_string(),
        receipts: Vec::new(),
        migration_receipts: Vec::new(),
    };
    symlink(&manifest_victim, &manifest_path).unwrap();
    let error = write_manifest_if_needed(&manifest_path, &manifest).unwrap_err();
    assert!(error.contains("regular non-symlink"));
    assert!(!manifest_victim.exists());

    let temporary = manifest_path.with_extension("json.tmp");
    symlink(&manifest_victim, &temporary).unwrap();
    std::fs::remove_file(&manifest_path).unwrap();
    let error = write_manifest_if_needed(&manifest_path, &manifest).unwrap_err();
    assert!(error.contains("regular non-symlink"));
    assert!(!manifest_victim.exists());
}

#[cfg(unix)]
#[test]
fn ordinary_file_replacement_of_reserved_backup_or_manifest_fails_before_db_mutation() {
    for race_hook in [
        CorpusRaceHook::ReplaceBackupTempWithNormalFile,
        CorpusRaceHook::ReplaceManifestTempWithNormalFile,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/artifact-race",
                shared_metadata(),
            )],
        );
        create_current_fixture(&target_path, &[]);
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let source_before = db_snapshot_fingerprint(&db_snapshot(&source_path));
        let target_before = db_snapshot_fingerprint(&db_snapshot(&target_path));
        let backup_dir = directory.path().join("backups");
        std::fs::create_dir(&backup_dir).unwrap();

        let error = apply_plan_with_race_hook(&scans, &plan, &backup_dir, race_hook)
            .expect_err("ordinary-file replacement must fail closed");

        assert!(
            error.contains("identity changed after reservation"),
            "{error}"
        );
        assert_eq!(
            db_snapshot_fingerprint(&db_snapshot(&source_path)),
            source_before
        );
        assert_eq!(
            db_snapshot_fingerprint(&db_snapshot(&target_path)),
            target_before
        );
        assert!(fixture_scan(LogicalStore::SharedWiki, &target_path)
            .raw_row(plan.items[0].target_id.as_deref().unwrap())
            .is_none());
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[test]
fn existing_final_backup_replacement_by_ordinary_or_source_hardlink_fails_closed() {
    for replace_with_source_hardlink in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/existing-backup-race",
                shared_metadata(),
            )],
        );
        let mut source = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
        classify_scans(std::slice::from_mut(&mut source));
        let expected = source.fingerprint().unwrap();
        let backup_dir = directory.path().join("backups");
        std::fs::create_dir(&backup_dir).unwrap();
        create_or_verify_backup(
            &source,
            &backup_dir,
            vec![LogicalStore::LegacyGlobal.reference().to_string()],
            &expected,
        )
        .unwrap();
        let backup_path = backup_dir.join(backup_file_name(
            &source.physical.as_ref().unwrap().physical_id,
        ));
        let replacement_path = directory.path().join("replacement.db");
        if replace_with_source_hardlink {
            std::fs::hard_link(&source_path, &replacement_path).unwrap();
        } else {
            create_current_fixture(
                &replacement_path,
                &[fixture_entry(
                    "replacement",
                    "/wiki/ordinary-replacement",
                    json!({"kind": "ordinary_replacement"}),
                )],
            );
        }
        let source_before = db_snapshot_fingerprint(&db_snapshot(&source_path));
        let mut race_hook =
            Some(CorpusRaceHook::ReplaceExistingBackupAfterRetain { replacement_path });

        let error = create_or_verify_backup_with_hook(
            &source,
            &backup_dir,
            &backup_file_name(&source.physical.as_ref().unwrap().physical_id),
            vec![LogicalStore::LegacyGlobal.reference().to_string()],
            &expected,
            &mut race_hook,
        )
        .err()
        .expect("existing final backup replacement must fail closed");

        assert!(
            error.contains("identity changed after reservation"),
            "{error}"
        );
        assert_eq!(
            db_snapshot_fingerprint(&db_snapshot(&source_path)),
            source_before
        );
        assert!(backup_path.exists());
        let source_after = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
        let row = source_after.raw_row("source").unwrap();
        assert_eq!(row.revision, 1);
        assert!(parse_migration_receipt(row).unwrap().is_none());
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[test]
fn retained_backup_replacement_before_first_source_update_fails_closed() {
    for replace_with_source_hardlink in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/retained-backup-boundary",
                shared_metadata(),
            )],
        );
        let mut scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].action, "reclassify_in_place");

        let replacement_path = directory.path().join("replacement.db");
        if replace_with_source_hardlink {
            std::fs::hard_link(&source_path, &replacement_path).unwrap();
        } else {
            create_current_fixture(
                &replacement_path,
                &[fixture_entry(
                    "replacement",
                    "/wiki/ordinary-backup-replacement",
                    json!({"kind": "replacement"}),
                )],
            );
        }
        let source_before = db_snapshot_fingerprint(&db_snapshot(&source_path));
        let backup_dir = directory.path().join("backups");
        std::fs::create_dir(&backup_dir).unwrap();

        let error = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::ReplaceRetainedBackupBeforeSourceMutation { replacement_path },
        )
        .expect_err("a detached rollback backup must block the first source update");

        assert!(
            error.contains("identity changed after reservation"),
            "{error}"
        );
        assert_eq!(
            db_snapshot_fingerprint(&db_snapshot(&source_path)),
            source_before
        );
        let source = fixture_scan(LogicalStore::SharedWiki, &source_path);
        let row = source.raw_row("source").unwrap();
        assert_eq!(row.revision, 1);
        assert!(parse_migration_receipt(row).unwrap().is_none());
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[test]
fn opened_source_or_target_path_swap_fails_before_first_write() {
    for swapped_store in [LogicalStore::LegacyGlobal, LogicalStore::SharedWiki] {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry(
                "source",
                "/wiki/opened-store-race",
                shared_metadata(),
            )],
        );
        create_current_fixture(&target_path, &[]);
        let replacement_path = directory.path().join("replacement.db");
        create_current_fixture(
            &replacement_path,
            &[fixture_entry(
                "replacement",
                "/wiki/replacement",
                json!({"kind": "replacement"}),
            )],
        );
        let mut scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut scans);
        let plan = build_plan(&scans).unwrap();
        let target_id = plan.items[0].target_id.as_deref().unwrap().to_string();
        let backup_dir = directory.path().join("backups");
        std::fs::create_dir(&backup_dir).unwrap();

        let error = apply_plan_with_race_hook(
            &scans,
            &plan,
            &backup_dir,
            CorpusRaceHook::SwapOpenedStorePath {
                store: swapped_store,
                replacement_path: replacement_path.clone(),
            },
        )
        .expect_err("detached opened store must fail before the first write");

        assert!(error.contains("detached from logical path"), "{error}");
        let original_source_path = if swapped_store == LogicalStore::LegacyGlobal {
            &replacement_path
        } else {
            &source_path
        };
        let original_target_path = if swapped_store == LogicalStore::SharedWiki {
            &replacement_path
        } else {
            &target_path
        };
        let source_after = fixture_scan(LogicalStore::LegacyGlobal, original_source_path);
        let source_row = source_after.raw_row("source").unwrap();
        assert_eq!(source_row.revision, 1);
        assert!(parse_migration_receipt(source_row).unwrap().is_none());
        assert_eq!(source_row.superseded_by, None);
        assert!(fixture_scan(LogicalStore::SharedWiki, original_target_path)
            .raw_row(&target_id)
            .is_none());
        let replaced_logical_path = if swapped_store == LogicalStore::LegacyGlobal {
            &source_path
        } else {
            &target_path
        };
        assert!(fixture_scan(swapped_store, replaced_logical_path)
            .raw_row("replacement")
            .is_some());
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[test]
fn mixed_completion_reclassification_swap_after_receipt_read_cannot_report_success() {
    let directory = tempfile::tempdir().unwrap();
    let shared_path = directory.path().join("shared.db");
    create_current_fixture(
        &shared_path,
        &[
            fixture_entry("a-completed", "/wiki/a", shared_metadata()),
            fixture_entry("z-pending", "/wiki/z", shared_metadata()),
        ],
    );
    let mut initial_scans = vec![fixture_scan(LogicalStore::SharedWiki, &shared_path)];
    classify_scans(&mut initial_scans);
    let plan = build_plan(&initial_scans).unwrap();
    assert_eq!(plan.items.len(), 2);
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    let expected = initial_scans[0].fingerprint().unwrap();
    create_or_verify_backup(
        &initial_scans[0],
        &backup_dir,
        vec![LogicalStore::SharedWiki.reference().to_string()],
        &expected,
    )
    .unwrap();
    let completed = plan
        .items
        .iter()
        .find(|item| item.source_id == "a-completed")
        .unwrap();
    let mut store =
        MemoryStore::open_existing_read_write(&shared_path.display().to_string()).unwrap();
    let entry = store
        .get_with_options(&completed.source_id, true)
        .unwrap()
        .unwrap();
    let row = raw_from_entry(&entry);
    let metadata = metadata_with_receipt(
        &row,
        receipt_value(completed, &plan.plan_id, "reclassified"),
    )
    .unwrap();
    assert!(store
        .update_with_revision(
            &entry.id,
            &entry.text,
            &entry.summary,
            &entry.source,
            &metadata,
            entry.vector.as_deref(),
            entry.revision,
        )
        .unwrap());
    drop(store);

    let mut partial_scans = vec![fixture_scan(LogicalStore::SharedWiki, &shared_path)];
    classify_scans(&mut partial_scans);
    let replacement_path = directory.path().join("replacement.db");
    create_current_fixture(
        &replacement_path,
        &[fixture_entry(
            "replacement",
            "/wiki/reclassification-replacement",
            json!({"kind": "replacement"}),
        )],
    );
    let error = apply_plan_with_race_hook(
        &partial_scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::SwapReclassificationPathAfterReceiptRead {
            source_id: completed.source_id.clone(),
            replacement_path: replacement_path.clone(),
        },
    )
    .expect_err("reclassification must revalidate after its final receipt read");

    assert!(error.contains("detached from logical path"), "{error}");
    let original = fixture_scan(LogicalStore::SharedWiki, &replacement_path);
    let completed_row = original.raw_row("a-completed").unwrap();
    assert_eq!(completed_row.revision, 2);
    assert!(receipt_matches(
        completed_row,
        completed,
        &plan.plan_id,
        &["reclassified"]
    ));
    let pending_row = original.raw_row("z-pending").unwrap();
    assert_eq!(pending_row.revision, 1);
    assert!(parse_migration_receipt(pending_row).unwrap().is_none());
    assert!(fixture_scan(LogicalStore::SharedWiki, &shared_path)
        .raw_row("replacement")
        .is_some());
}

#[test]
fn post_validation_enrichment_cannot_commit_a_stale_reclassification_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let shared_path = directory.path().join("shared.db");
    create_current_fixture(
        &shared_path,
        &[fixture_entry_with_vector(
            "source",
            "/wiki/reclassification-enrichment-race",
            shared_metadata(),
            vec![0.11; 1024],
        )],
    );
    let mut scans = vec![fixture_scan(LogicalStore::SharedWiki, &shared_path)];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].action, "reclassify_in_place");
    let expected = scans[0].fingerprint().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    create_or_verify_backup(
        &scans[0],
        &backup_dir,
        vec![LogicalStore::SharedWiki.reference().to_string()],
        &expected,
    )
    .unwrap();

    let error = apply_plan_with_race_hook(
        &scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::MutateReclassificationAfterPlanValidation {
            source_id: "source".to_string(),
        },
    )
    .expect_err("post-validation enrichment must invalidate reclassification");

    assert!(
        error.contains("source fingerprint changed during reclassification"),
        "{error}"
    );
    let current = fixture_scan(LogicalStore::SharedWiki, &shared_path);
    let row = current.raw_row("source").unwrap();
    assert_eq!(row.revision, 1);
    assert_eq!(row.summary, "post-validation generated summary");
    assert_ne!(
        row.vector_fingerprint(current.vector_table_present),
        plan.items[0].source_vector
    );
    assert!(parse_migration_receipt(row).unwrap().is_none());
}

#[test]
fn post_validation_valid_until_drift_cannot_commit_a_reclassification_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let shared_path = directory.path().join("shared.db");
    create_current_fixture(
        &shared_path,
        &[fixture_entry(
            "source",
            "/wiki/reclassification-valid-until-race",
            shared_metadata(),
        )],
    );
    let mut scans = vec![fixture_scan(LogicalStore::SharedWiki, &shared_path)];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    create_or_verify_backup(
        &scans[0],
        &backup_dir,
        vec![LogicalStore::SharedWiki.reference().to_string()],
        &scans[0].fingerprint().unwrap(),
    )
    .unwrap();

    let error = apply_plan_with_race_hook(
        &scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::MutateValidUntilAfterPlanValidation {
            source_id: "source".to_string(),
        },
    )
    .expect_err("same-revision valid_until drift must invalidate reclassification");

    assert!(
        error.contains("source fingerprint changed during reclassification"),
        "{error}"
    );
    let current = fixture_scan(LogicalStore::SharedWiki, &shared_path);
    let row = current.raw_row("source").unwrap();
    assert_eq!(row.revision, 1);
    assert_eq!(row.valid_until.as_deref(), Some("2026-12-31T23:59:59Z"));
    assert!(parse_migration_receipt(row).unwrap().is_none());
    assert_ne!(
        scans[0].row_digest, current.row_digest,
        "valid_until must participate in plan and backup row evidence"
    );
    assert!(validate_plan(std::slice::from_ref(&current), &plan).is_err());
}

#[test]
fn post_precheck_copy_enrichment_marks_the_new_target_noncanonical() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry_with_vector(
            "source",
            "/wiki/copy-enrichment-race",
            shared_metadata(),
            vec![0.11; 1024],
        )],
    );
    create_current_fixture(&target_path, &[]);
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    let item = &plan.items[0];
    let target_id = item.target_id.as_deref().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();

    let error = apply_plan_with_race_hook(
        &scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::MutateCopySourceAfterPrecheck {
            source_id: "source".to_string(),
        },
    )
    .expect_err("post-precheck source drift must reject the atomic source transition");

    assert!(error.contains("source fingerprint changed"), "{error}");
    let mut current = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut current);
    let source = current[0].raw_row("source").unwrap();
    assert_eq!(source.revision, 1);
    assert_eq!(source.summary, "post-precheck generated summary");
    assert!(parse_migration_receipt(source).unwrap().is_none());
    assert!(source.superseded_by.is_none());
    let target = current[1].raw_row(target_id).unwrap();
    assert!(target.archived);
    assert_eq!(
        parse_migration_receipt(target).unwrap().unwrap().phase,
        MigrationPhase::TargetNoncanonical
    );
    assert!(
        validate_plan(&current, &plan).is_err(),
        "the old plan must not replay against the enriched source"
    );
}

#[test]
fn source_drift_after_receipt_prep_reconciles_target_without_hard_delete() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry_with_vector(
            "source",
            "/wiki/source-receipt-prep-race",
            shared_metadata(),
            vec![0.11; 1024],
        )],
    );
    create_current_fixture(&target_path, &[]);
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    let target_id = plan.items[0].target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();

    let error = apply_plan_with_race_hook(
        &scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::MutateSourceAfterReceiptPrepared {
            source_id: "source".to_string(),
        },
    )
    .expect_err("source drift must reject the atomic source transition");
    remove_memory_hard_delete_guard(&target_path);
    assert!(
        error.contains("source fingerprint changed before the atomic"),
        "{error}"
    );

    let mut current = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut current);
    let source = current[0].raw_row("source").unwrap();
    assert_eq!(source.revision, 1);
    assert_eq!(source.summary, "source enriched after receipt preparation");
    assert!(source.superseded_by.is_none());
    assert!(parse_migration_receipt(source).unwrap().is_none());
    let target = current[1].raw_row(&target_id).unwrap();
    assert!(
        target.archived,
        "rejected copy target must remain but be noncanonical"
    );
    assert_eq!(
        parse_migration_receipt(target).unwrap().unwrap().phase,
        MigrationPhase::TargetNoncanonical
    );
    let target_revision = target.revision;

    apply_plan(&current, &plan, &backup_dir)
        .expect_err("old plan replay must remain rejected after source drift");
    let replay_target = fixture_scan(LogicalStore::SharedWiki, &target_path);
    assert_eq!(
        replay_target.raw_row(&target_id).unwrap().revision,
        target_revision,
        "replay must converge without repeatedly mutating the noncanonical target"
    );
}

#[test]
fn target_enrichment_after_verification_is_adopted_by_apply_and_replay() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry_with_vector(
            "source",
            "/wiki/target-enrichment-race",
            shared_metadata(),
            vec![0.11; 1024],
        )],
    );
    create_current_fixture(&target_path, &[]);
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    let target_id = plan.items[0].target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();

    let (_, outcomes) = apply_plan_with_race_hook(
        &scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::MutateTargetAfterVerification {
            source_id: "source".to_string(),
        },
    )
    .expect("mutable target enrichment must remain adoptable");
    assert_eq!(outcomes[0].outcome, "copied_and_superseded");

    let mut completed = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut completed);
    let target = completed[1].raw_row(&target_id).unwrap();
    assert_eq!(target.summary, "target enriched after verification");
    assert_eq!(
        target
            .metadata
            .pointer("/enrichment/status")
            .and_then(Value::as_str),
        Some("complete")
    );
    assert_ne!(target.vector_fingerprint(true), plan.items[0].source_vector);
    assert!(!target.archived);

    let (_, replay) = apply_plan(&completed, &plan, &backup_dir)
        .expect("replay must adopt the enriched deterministic target");
    assert_eq!(replay[0].outcome, "existing_no_op");
}

#[test]
fn foreign_source_supersession_reconciles_target_without_hard_delete() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry(
            "source",
            "/wiki/foreign-supersession-race",
            shared_metadata(),
        )],
    );
    create_current_fixture(&target_path, &[]);
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    let target_id = plan.items[0].target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();

    let error = apply_plan_with_race_hook(
        &scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition {
            source_id: "source".to_string(),
        },
    )
    .expect_err("foreign source supersession must win atomically");
    remove_memory_hard_delete_guard(&target_path);
    assert!(error.contains("superseded by foreign target"), "{error}");

    let mut current = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut current);
    let source = current[0].raw_row("source").unwrap();
    assert_eq!(source.superseded_by.as_deref(), Some("foreign-wiki-target"));
    assert!(parse_migration_receipt(source).unwrap().is_none());
    let target = current[1].raw_row(&target_id).unwrap();
    assert!(target.archived);
    assert_eq!(
        parse_migration_receipt(target).unwrap().unwrap().phase,
        MigrationPhase::TargetNoncanonical
    );
    let target_revision = target.revision;

    apply_plan(&current, &plan, &backup_dir)
        .expect_err("old plan replay must converge to the foreign supersession failure");
    assert_eq!(
        fixture_scan(LogicalStore::SharedWiki, &target_path)
            .raw_row(&target_id)
            .unwrap()
            .revision,
        target_revision
    );
}

/// Assert the state a losing worker must leave behind after a sibling
/// worker completed the same deterministic plan item: the winner's target
/// stays canonical and user-visible, the source keeps the winner's
/// supersession edge, and a later apply converges on the completed state
/// instead of dead-ending on the occupant collision check.
fn assert_sibling_completion_survived(
    source_path: &Path,
    target_path: &Path,
    page_path: &str,
    target_id: &str,
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
) {
    let mut current = vec![
        fixture_scan(LogicalStore::LegacyGlobal, source_path),
        fixture_scan(LogicalStore::SharedWiki, target_path),
    ];
    classify_scans(&mut current);
    let source = current[0].raw_row("source").unwrap();
    assert_eq!(source.superseded_by.as_deref(), Some(target_id));
    assert_eq!(
        parse_migration_receipt(source).unwrap().unwrap().phase,
        MigrationPhase::SourceSuperseded
    );
    let target = current[1].raw_row(target_id).unwrap();
    assert!(
        !target.archived,
        "the sibling worker's canonical target must not be archived by the loser"
    );
    assert!(target.superseded_by.is_none());
    assert_eq!(
        parse_migration_receipt(target).unwrap().unwrap().phase,
        MigrationPhase::TargetCopied
    );

    // The user-facing Wiki read surface filters `archived = 0 AND
    // superseded_by IS NULL`, so archiving the target would delete the page
    // from every reader.
    let target_store =
        MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
    let visible = target_store
        .list_user_facing_wiki_entries(page_path, 10, false)
        .unwrap();
    assert_eq!(
        visible
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec![target_id],
        "the migrated page must stay on the user-facing read surface"
    );
    drop(target_store);

    let (_, replay) = apply_plan(&current, plan, backup_dir)
        .expect("replay must converge on the sibling-completed state");
    assert_eq!(replay[0].outcome, "existing_no_op");
}

#[test]
fn sibling_worker_completion_before_target_precheck_is_adopted_not_archived() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry_with_vector(
            "source",
            "/wiki/sibling-worker-precheck-race",
            shared_metadata(),
            vec![0.11; 1024],
        )],
    );
    create_current_fixture(&target_path, &[]);
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    let target_id = plan.items[0].target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();

    let (_, outcomes) = apply_plan_with_race_hook(
        &scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::SiblingWorkerCompletesItem {
            source_id: "source".to_string(),
            seam: SiblingCompletionSeam::AfterSourcePrecheck,
        },
    )
    .expect("a sibling-completed item must return the completed no-op, not an error");
    assert_eq!(outcomes[0].outcome, "existing_no_op");
    assert_eq!(
        outcomes[0].phases,
        vec!["target_copied".to_string(), "source_superseded".to_string()]
    );

    assert_sibling_completion_survived(
        &source_path,
        &target_path,
        "/wiki/sibling-worker-precheck-race",
        &target_id,
        &plan,
        &backup_dir,
    );
}

#[test]
fn sibling_worker_completion_after_receipt_prep_is_adopted_not_archived() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry_with_vector(
            "source",
            "/wiki/sibling-worker-transition-race",
            shared_metadata(),
            vec![0.11; 1024],
        )],
    );
    create_current_fixture(&target_path, &[]);
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    let target_id = plan.items[0].target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();

    let (_, outcomes) = apply_plan_with_race_hook(
        &scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::SiblingWorkerCompletesItem {
            source_id: "source".to_string(),
            seam: SiblingCompletionSeam::AfterReceiptPrepared,
        },
    )
    .expect("losing the atomic transition to a sibling must not fail the item");
    assert_eq!(outcomes[0].outcome, "existing_no_op");
    assert_eq!(
        outcomes[0].phases,
        vec!["target_copied".to_string(), "source_superseded".to_string()]
    );

    assert_sibling_completion_survived(
        &source_path,
        &target_path,
        "/wiki/sibling-worker-transition-race",
        &target_id,
        &plan,
        &backup_dir,
    );
}

#[test]
fn vector_state_is_preserved_or_apply_refuses_without_target_capability() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("vector-source.db");
    let target_path = directory.path().join("no-vector-target.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry_with_vector(
            "vector-source",
            "/wiki/vector",
            shared_metadata(),
            vec![0.25; 1024],
        )],
    );
    create_current_fixture(&target_path, &[]);
    let target_conn = Connection::open(&target_path).unwrap();
    target_conn.execute("DROP TABLE memories_vec", []).unwrap();
    drop(target_conn);

    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    assert!(scans[0].vector_table_present);
    assert!(!scans[1].vector_table_present);
    let plan = build_plan(&scans).unwrap();
    let error = validate_plan(&scans, &plan).unwrap_err();
    assert!(error.contains("cannot preserve the source vector"));
    assert_eq!(scans[1].rows.len(), 0);
}

#[test]
fn vector_copy_preserves_the_source_embedding_in_the_target_store() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("vector-source.db");
    let target_path = directory.path().join("vector-target.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry_with_vector(
            "vector-source",
            "/wiki/vector-copy",
            shared_metadata(),
            vec![0.125; 1024],
        )],
    );
    create_current_fixture(&target_path, &[]);
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    apply_plan(&scans, &plan, &backup_dir).unwrap();

    let mut after_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut after_scans);
    let item = &plan.items[0];
    let target_row = after_scans[1]
        .raw_row(item.target_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(
        target_row.vector_fingerprint(after_scans[1].vector_table_present),
        item.source_vector
    );
}

#[test]
fn vector_mutation_without_memory_revision_invalidates_plan_fingerprint() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("vector-source.db");
    let target_path = directory.path().join("vector-target.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry_with_vector(
            "vector-source",
            "/wiki/vector-mutation",
            shared_metadata(),
            vec![0.25; 1024],
        )],
    );
    create_current_fixture(&target_path, &[]);
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut scans);
    let plan = build_plan(&scans).unwrap();

    memcore::db::register_sqlite_vec();
    let source_conn = Connection::open(&source_path).unwrap();
    source_conn
        .execute(
            "UPDATE memories_vec SET embedding = ?1 WHERE id = ?2",
            rusqlite::params![
                memcore::db::serialize_f32(&vec![0.75; 1024]),
                "vector-source"
            ],
        )
        .unwrap();
    drop(source_conn);

    let mut changed_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut changed_scans);
    let error = validate_plan(&changed_scans, &plan).unwrap_err();
    assert!(error.contains("fingerprint") || error.contains("vector"));
}

#[test]
fn interrupted_copy_boundaries_reconcile_and_rerun_idempotently() {
    for boundary in [
        MigrationBoundary::TargetCopied,
        MigrationBoundary::SourceSuperseded,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("legacy.db");
        let target_path = directory.path().join("shared.db");
        create_current_fixture(
            &source_path,
            &[fixture_entry("source", "/wiki/crash", shared_metadata())],
        );
        create_current_fixture(&target_path, &[]);
        let mut initial_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut initial_scans);
        let plan = build_plan(&initial_scans).unwrap();
        let backup_dir = directory.path().join("backups");
        std::fs::create_dir(&backup_dir).unwrap();
        let interruption = MigrationInterruption {
            source_id: Some("source".to_string()),
            boundary,
        };
        let error = apply_plan_with_interruption(&initial_scans, &plan, &backup_dir, interruption)
            .unwrap_err();
        assert!(error.contains("simulated interruption"));

        let mut partial_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut partial_scans);
        let source_row = partial_scans[0].raw_row("source").unwrap();
        let target_id = plan.items[0].target_id.as_deref().unwrap();
        let target_row = partial_scans[1].raw_row(target_id).unwrap();
        assert_eq!(
            parse_migration_receipt(target_row).unwrap().unwrap().phase,
            MigrationPhase::TargetCopied
        );
        match boundary {
            MigrationBoundary::TargetCopied => {
                assert!(parse_migration_receipt(source_row).unwrap().is_none());
                assert_eq!(source_row.superseded_by, None);
            }
            MigrationBoundary::SourceSuperseded => {
                assert_eq!(
                    parse_migration_receipt(source_row).unwrap().unwrap().phase,
                    MigrationPhase::SourceSuperseded
                );
                assert_eq!(source_row.superseded_by.as_deref(), Some(target_id));
            }
        }

        let mut resumed_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut resumed_scans);
        let (_, outcomes) = apply_plan(&resumed_scans, &plan, &backup_dir).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0]
            .phases
            .contains(&"source_superseded".to_string()));

        let mut replay_scans = vec![
            fixture_scan(LogicalStore::LegacyGlobal, &source_path),
            fixture_scan(LogicalStore::SharedWiki, &target_path),
        ];
        classify_scans(&mut replay_scans);
        let (_, replay_outcomes) = apply_plan(&replay_scans, &plan, &backup_dir).unwrap();
        assert_eq!(replay_outcomes[0].outcome, "existing_no_op");
    }
}

#[test]
fn completed_first_item_and_failed_second_item_resume_from_original_backup() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    let mut first_entry = fixture_entry("a", "/wiki/partial-a", shared_metadata());
    first_entry.summary = "partial first summary".to_string();
    first_entry.text = "partial first text".to_string();
    let mut second_entry = fixture_entry("b", "/wiki/partial-b", shared_metadata());
    second_entry.summary = "partial second summary".to_string();
    second_entry.text = "partial second text".to_string();
    create_current_fixture(&source_path, &[first_entry, second_entry]);
    create_current_fixture(&target_path, &[]);
    let mut initial_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut initial_scans);
    let plan = build_plan(&initial_scans).unwrap();
    assert_eq!(plan.items.len(), 2);
    let second = plan
        .items
        .iter()
        .find(|item| item.source_id == "b")
        .unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    let error = apply_plan_with_interruption(
        &initial_scans,
        &plan,
        &backup_dir,
        MigrationInterruption {
            source_id: Some(second.source_id.clone()),
            boundary: MigrationBoundary::TargetCopied,
        },
    )
    .unwrap_err();
    assert!(error.contains("simulated interruption"), "{error}");
    let backup_path = backup_dir.join(backup_file_name(
        &initial_scans[0].physical.as_ref().unwrap().physical_id,
    ));
    let backup_before_resume = db_snapshot(&backup_path);

    let mut partial_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut partial_scans);
    let first = plan
        .items
        .iter()
        .find(|item| item.source_id == "a")
        .unwrap();
    let first_row = partial_scans[0].raw_row(&first.source_id).unwrap();
    assert_eq!(
        parse_migration_receipt(first_row).unwrap().unwrap().phase,
        MigrationPhase::SourceSuperseded
    );
    let second_row = partial_scans[0].raw_row(&second.source_id).unwrap();
    assert!(parse_migration_receipt(second_row).unwrap().is_none());

    let mut resumed_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut resumed_scans);
    let (_, outcomes) = apply_plan(&resumed_scans, &plan, &backup_dir).unwrap();
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes
        .iter()
        .all(|outcome| { outcome.phases.contains(&"source_superseded".to_string()) }));
    assert_eq!(db_snapshot(&backup_path), backup_before_resume);

    let mut final_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut final_scans);
    let (_, replay_outcomes) = apply_plan(&final_scans, &plan, &backup_dir).unwrap();
    assert!(replay_outcomes
        .iter()
        .all(|outcome| outcome.outcome == "existing_no_op"));
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[test]
fn completed_run_command_rejects_post_inventory_logical_path_swap() {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("global.db");
    let project_path = directory.path().join("project.db");
    let app_home = directory.path().join("home");
    let shared_path = app_home.join("projects/wiki/memory.db");
    std::fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
    create_current_fixture(
        &global_path,
        &[fixture_entry(
            "global",
            "/wiki/completed-replay-race",
            shared_metadata(),
        )],
    );
    create_current_fixture(&project_path, &[]);
    create_current_fixture(&shared_path, &[]);

    let preview = run_wiki_corpus_command(
        false,
        None,
        None,
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .unwrap();
    let plan = preview.plan.unwrap();
    let plan_path = directory.path().join("plan.json");
    std::fs::write(
        &plan_path,
        serde_json::to_vec_pretty(&json!({"plan": plan})).unwrap(),
    )
    .unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    run_wiki_corpus_command(
        true,
        Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
        Some(backup_dir.clone()),
        Some(plan_path.clone()),
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .unwrap();

    let replacement_path = directory.path().join("replacement.db");
    create_current_fixture(
        &replacement_path,
        &[fixture_entry(
            "replacement",
            "/wiki/post-inventory-replacement",
            json!({"kind": "replacement"}),
        )],
    );
    let shared_before = db_snapshot_fingerprint(&db_snapshot(&shared_path));
    let backups_before = directory_snapshot(&backup_dir);

    let error = run_wiki_corpus_command_with_race_hook(
        true,
        Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
        Some(backup_dir.clone()),
        Some(plan_path),
        &global_path,
        Some(&project_path),
        &app_home,
        CorpusRaceHook::SwapLogicalPathAfterInventory {
            store: LogicalStore::LegacyGlobal,
            replacement_path: replacement_path.clone(),
        },
    )
    .expect_err("completed replay must not trust its stale inventory");

    assert!(
        error.contains("physical binding changed since inventory"),
        "{error}"
    );
    assert_eq!(
        db_snapshot_fingerprint(&db_snapshot(&shared_path)),
        shared_before
    );
    assert_eq!(directory_snapshot(&backup_dir), backups_before);
    assert!(fixture_scan(LogicalStore::LegacyGlobal, &global_path)
        .raw_row("replacement")
        .is_some());
    let original = fixture_scan(LogicalStore::LegacyGlobal, &replacement_path);
    let completed_row = original.raw_row("global").unwrap();
    assert_eq!(
        parse_migration_receipt(completed_row)
            .unwrap()
            .unwrap()
            .phase,
        MigrationPhase::SourceSuperseded
    );
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[test]
fn completed_no_op_rejects_path_swap_at_actual_success_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry(
            "source",
            "/wiki/completed-success-boundary",
            shared_metadata(),
        )],
    );
    let mut initial_scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
    classify_scans(&mut initial_scans);
    let plan = build_plan(&initial_scans).unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    apply_plan(&initial_scans, &plan, &backup_dir).unwrap();

    let mut completed_scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
    classify_scans(&mut completed_scans);
    assert!(plan
        .items
        .iter()
        .all(|item| plan_item_completed(&completed_scans, &plan, item)));
    let replacement_path = directory.path().join("replacement.db");
    create_current_fixture(
        &replacement_path,
        &[fixture_entry(
            "replacement",
            "/wiki/completed-return-replacement",
            json!({"kind": "replacement"}),
        )],
    );
    let backups_before = directory_snapshot(&backup_dir);

    let error = apply_plan_with_race_hook(
        &completed_scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::SwapLogicalPathBeforeCompletedReturn {
            store: LogicalStore::SharedWiki,
            replacement_path: replacement_path.clone(),
        },
    )
    .expect_err("completed no-op must revalidate its logical path at return");

    assert!(
        error.contains("physical binding changed since inventory"),
        "{error}"
    );
    assert_eq!(directory_snapshot(&backup_dir), backups_before);
    assert!(fixture_scan(LogicalStore::SharedWiki, &source_path)
        .raw_row("replacement")
        .is_some());
    let original = fixture_scan(LogicalStore::SharedWiki, &replacement_path);
    let row = original.raw_row("source").unwrap();
    assert_eq!(row.revision, 2);
    assert!(receipt_matches(
        row,
        &plan.items[0],
        &plan.plan_id,
        &["reclassified"]
    ));
}

#[test]
fn completed_copy_no_op_rejects_target_archive_after_first_proof() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry(
            "source",
            "/wiki/completed-target-archive",
            shared_metadata(),
        )],
    );
    create_current_fixture(&target_path, &[]);

    let mut initial_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut initial_scans);
    let plan = build_plan(&initial_scans).unwrap();
    let target_id = plan.items[0].target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    apply_plan(&initial_scans, &plan, &backup_dir).unwrap();

    let mut completed_scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, &source_path),
        fixture_scan(LogicalStore::SharedWiki, &target_path),
    ];
    classify_scans(&mut completed_scans);
    assert!(plan
        .items
        .iter()
        .all(|item| plan_item_completed(&completed_scans, &plan, item)));

    let error = apply_plan_with_race_hook(
        &completed_scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::ArchiveTargetBeforeCompletedReturn {
            source_id: "source".to_string(),
        },
    )
    .expect_err("completed no-op must reject a target archived after its first proof");

    assert!(
        error.contains("completed Wiki corpus state changed")
            || error.contains("existing_no_op proof"),
        "{error}"
    );
    let target = fixture_scan(LogicalStore::SharedWiki, &target_path);
    assert!(target.raw_row(&target_id).unwrap().archived);
    let source = fixture_scan(LogicalStore::LegacyGlobal, &source_path);
    assert_eq!(
        source.raw_row("source").unwrap().superseded_by.as_deref(),
        Some(target_id.as_str())
    );
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[test]
fn completed_no_op_reverifies_backup_object_instead_of_trusting_manifest() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry(
            "source",
            "/wiki/completed-backup-recheck",
            shared_metadata(),
        )],
    );
    let mut initial_scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
    classify_scans(&mut initial_scans);
    let plan = build_plan(&initial_scans).unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    apply_plan(&initial_scans, &plan, &backup_dir).unwrap();

    let mut completed_scans = vec![fixture_scan(LogicalStore::SharedWiki, &source_path)];
    classify_scans(&mut completed_scans);
    let replacement_path = directory.path().join("replacement.db");
    create_current_fixture(
        &replacement_path,
        &[fixture_entry(
            "replacement",
            "/wiki/completed-backup-replacement",
            json!({"kind": "replacement"}),
        )],
    );

    let error = apply_plan_with_race_hook(
        &completed_scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::ReplaceExistingBackupAfterRetain { replacement_path },
    )
    .expect_err("completed replay must reopen and retain its rollback backup");

    assert!(
        error.contains("identity changed after reservation"),
        "{error}"
    );
    let source = fixture_scan(LogicalStore::SharedWiki, &source_path);
    let row = source.raw_row("source").unwrap();
    assert_eq!(row.revision, 2);
    assert!(receipt_matches(
        row,
        &plan.items[0],
        &plan.plan_id,
        &["reclassified"]
    ));
}

#[test]
fn command_boundary_uses_three_physical_stores_and_replays_the_same_plan() {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("global.db");
    let project_path = directory.path().join("project.db");
    let app_home = directory.path().join("home");
    let shared_path = app_home.join("projects/wiki/memory.db");
    std::fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
    create_current_fixture(
        &global_path,
        &[fixture_entry("global", "/wiki/global", shared_metadata())],
    );
    create_current_fixture(
        &project_path,
        &[fixture_entry("project", "/wiki/project", shared_metadata())],
    );
    create_current_fixture(
        &shared_path,
        &[fixture_entry("shared", "/wiki/shared", shared_metadata())],
    );

    let preview = run_wiki_corpus_command(
        false,
        None,
        None,
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .unwrap();
    assert_eq!(preview.mode, "preview");
    assert_eq!(preview.stores.len(), 3);
    let plan = preview.plan.clone().unwrap();
    assert_eq!(plan.store_fingerprints.len(), 3);
    let physical_ids = plan
        .store_fingerprints
        .iter()
        .map(|fingerprint| fingerprint.physical_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(physical_ids.len(), 3);

    let plan_path = directory.path().join("plan.json");
    std::fs::write(
        &plan_path,
        serde_json::to_vec_pretty(&json!({"plan": plan})).unwrap(),
    )
    .unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    let applied = run_wiki_corpus_command(
        true,
        Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
        Some(backup_dir.clone()),
        Some(plan_path.clone()),
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .unwrap();
    assert_eq!(applied.mode, "apply");
    assert_eq!(applied.stores.len(), 3);
    assert_eq!(applied.migration_outcomes.len(), 3);

    let replay = run_wiki_corpus_command(
        true,
        Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
        Some(backup_dir),
        Some(plan_path),
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .unwrap();
    assert_eq!(replay.migration_outcomes.len(), 3);
    assert!(replay
        .migration_outcomes
        .iter()
        .all(|outcome| outcome.outcome == "existing_no_op"));
}

/// Put a completed migration into the exact terminal state the sibling
/// race used to leave behind: the canonical target is archived and carries
/// a `target_noncanonical` receipt while its source keeps the supersession
/// edge and the `source_superseded` receipt.
fn damage_completed_target_as_sibling_race(
    target_path: &Path,
    target_id: &str,
    item: &PlanItem,
    plan_id: &str,
) {
    let mut target_store =
        MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
    reconcile_target_noncanonical(&mut target_store, target_id, item, plan_id).unwrap();
    let damaged = target_store
        .get_with_options(target_id, true)
        .unwrap()
        .unwrap();
    assert!(
        damaged.archived,
        "the damage fixture must archive the target"
    );
}

fn user_facing_ids(target_path: &Path, page_path: &str) -> Vec<String> {
    let store = MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
    store
        .list_user_facing_wiki_entries(page_path, 10, false)
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect()
}

fn corpus_scans(source_path: &Path, target_path: &Path) -> Vec<StoreScan> {
    let mut scans = vec![
        fixture_scan(LogicalStore::LegacyGlobal, source_path),
        fixture_scan(LogicalStore::SharedWiki, target_path),
    ];
    classify_scans(&mut scans);
    scans
}

#[test]
fn sibling_race_damaged_target_is_repaired_back_onto_the_read_surface() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    let page_path = "/wiki/sibling-damage-repair";
    create_current_fixture(
        &source_path,
        &[fixture_entry_with_vector(
            "source",
            page_path,
            shared_metadata(),
            vec![0.11; 1024],
        )],
    );
    create_current_fixture(&target_path, &[]);
    let scans = corpus_scans(&source_path, &target_path);
    let plan = build_plan(&scans).unwrap();
    let item = plan.items[0].clone();
    let target_id = item.target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    let (_, outcomes) = apply_plan(&scans, &plan, &backup_dir).unwrap();
    assert_eq!(outcomes[0].outcome, "copied_and_superseded");
    assert_eq!(
        user_facing_ids(&target_path, page_path),
        vec![target_id.clone()]
    );

    damage_completed_target_as_sibling_race(&target_path, &target_id, &item, &plan.plan_id);
    let damaged = corpus_scans(&source_path, &target_path);
    let damaged_target = damaged[1].raw_row(&target_id).unwrap();
    assert!(damaged_target.archived);
    assert_eq!(
        parse_migration_receipt(damaged_target)
            .unwrap()
            .unwrap()
            .phase,
        MigrationPhase::TargetNoncanonical
    );
    assert!(
        user_facing_ids(&target_path, page_path).is_empty(),
        "the damaged page must be invisible to every reader before the repair"
    );

    // The dead end this repair exists for, with its upgraded diagnosis.
    let error = apply_plan(&damaged, &plan, &backup_dir)
        .expect_err("a damaged target must still dead-end the migration");
    assert!(
        error.contains("deterministic target occupant collision"),
        "{error}"
    );
    assert!(
        error.contains("observed receipt_phase=target_noncanonical archived=true"),
        "{error}"
    );
    assert!(
        error.contains("expected receipt_phase=target_copied archived=false"),
        "{error}"
    );
    assert!(error.contains("--repair-sibling-damage"), "{error}");

    let repair_dir = directory.path().join("repair-backups");
    std::fs::create_dir(&repair_dir).unwrap();
    let report = repair_sibling_damage(&damaged, Some(&repair_dir)).unwrap();
    assert_eq!(report.inspected_rows, 1);
    assert_eq!(report.repairable, 1);
    assert_eq!(report.repaired, 1);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.rows[0].outcome, "repaired");
    assert!(report.rows[0].skipped_reasons.is_empty());
    assert_eq!(
        report.backups.len(),
        1,
        "the mutated store must be backed up"
    );

    let healed = corpus_scans(&source_path, &target_path);
    let target = healed[1].raw_row(&target_id).unwrap();
    assert!(!target.archived);
    assert!(target.superseded_by.is_none());
    assert_eq!(
        parse_migration_receipt(target).unwrap().unwrap().phase,
        MigrationPhase::TargetCopied,
        "the repaired row must hold the canonical terminal receipt"
    );
    let history = target
        .metadata
        .get(REPAIR_RECEIPT_KEY)
        .and_then(Value::as_array)
        .expect("the repair history must be preserved");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["phase"], json!(REPAIR_PHASE));
    assert_eq!(
        history[0]["replaced_receipt"]["phase"],
        json!("target_noncanonical"),
        "the replaced receipt must be kept verbatim instead of erased"
    );
    assert_eq!(
        user_facing_ids(&target_path, page_path),
        vec![target_id.clone()],
        "the repaired page must be back on the user-facing read surface"
    );

    let (_, replay) = apply_plan(&healed, &plan, &backup_dir)
        .expect("apply must converge on the repaired state instead of colliding");
    assert_eq!(replay[0].outcome, "existing_no_op");
}

#[test]
fn foreign_source_supersession_is_reported_but_never_repaired() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry(
            "source",
            "/wiki/foreign-supersession-not-repairable",
            shared_metadata(),
        )],
    );
    create_current_fixture(&target_path, &[]);
    let scans = corpus_scans(&source_path, &target_path);
    let plan = build_plan(&scans).unwrap();
    let target_id = plan.items[0].target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();

    apply_plan_with_race_hook(
        &scans,
        &plan,
        &backup_dir,
        CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition {
            source_id: "source".to_string(),
        },
    )
    .expect_err("foreign source supersession must win atomically");
    // The hook installs a hard-delete assertion trigger; the surrounding
    // test fixture drops it exactly like the reconciliation test does.
    remove_memory_hard_delete_guard(&target_path);

    let current = corpus_scans(&source_path, &target_path);
    let damaged_target = current[1].raw_row(&target_id).unwrap();
    assert!(damaged_target.archived);
    assert_eq!(
        parse_migration_receipt(damaged_target)
            .unwrap()
            .unwrap()
            .phase,
        MigrationPhase::TargetNoncanonical
    );
    let target_revision = damaged_target.revision;

    let repair_dir = directory.path().join("repair-backups");
    std::fs::create_dir(&repair_dir).unwrap();
    let report = repair_sibling_damage(&current, Some(&repair_dir)).unwrap();
    assert_eq!(report.inspected_rows, 1);
    assert_eq!(report.repairable, 0);
    assert_eq!(report.repaired, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.rows[0].outcome, "skipped");
    assert!(
        report.rows[0]
            .skipped_reasons
            .iter()
            .any(|reason| reason.contains("is superseded by foreign-wiki-target")),
        "{:?}",
        report.rows[0].skipped_reasons
    );
    assert!(
        report.rows[0]
            .skipped_reasons
            .iter()
            .any(|reason| reason.contains("does not carry a source_superseded receipt")),
        "{:?}",
        report.rows[0].skipped_reasons
    );
    assert!(
        report.backups.is_empty(),
        "a run with nothing to repair must not touch the backup directory"
    );
    assert!(directory_snapshot(&repair_dir).is_empty());

    let after = corpus_scans(&source_path, &target_path);
    let untouched = after[1].raw_row(&target_id).unwrap();
    assert_eq!(untouched.revision, target_revision);
    assert!(untouched.archived);
    assert_eq!(
        parse_migration_receipt(untouched).unwrap().unwrap().phase,
        MigrationPhase::TargetNoncanonical
    );
}

#[test]
fn archived_target_without_the_noncanonical_receipt_is_reported_not_repaired() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    create_current_fixture(
        &source_path,
        &[fixture_entry(
            "source",
            "/wiki/archived-without-noncanonical-receipt",
            shared_metadata(),
        )],
    );
    create_current_fixture(&target_path, &[]);
    let scans = corpus_scans(&source_path, &target_path);
    let plan = build_plan(&scans).unwrap();
    let target_id = plan.items[0].target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    apply_plan(&scans, &plan, &backup_dir).unwrap();

    // Archive the canonical target while leaving its `target_copied`
    // receipt in place: archived, but not the sibling-race signature.
    let mut target_store =
        MemoryStore::open_existing_read_write(&target_path.display().to_string()).unwrap();
    let entry = target_store
        .get_with_options(&target_id, true)
        .unwrap()
        .unwrap();
    let metadata = entry.metadata.clone();
    let expected = ExpectedMemoryState::from_entry(&entry, None);
    assert!(target_store
        .archive_with_metadata_if_expected_state(&target_id, &metadata, &expected)
        .unwrap());
    drop(target_store);

    let current = corpus_scans(&source_path, &target_path);
    let revision = current[1].raw_row(&target_id).unwrap().revision;
    let repair_dir = directory.path().join("repair-backups");
    std::fs::create_dir(&repair_dir).unwrap();
    let report = repair_sibling_damage(&current, Some(&repair_dir)).unwrap();
    assert_eq!(report.inspected_rows, 1);
    assert_eq!(report.repairable, 0);
    assert_eq!(report.repaired, 0);
    assert_eq!(report.rows[0].outcome, "skipped");
    assert_eq!(
        report.rows[0].observed_receipt_phase.as_deref(),
        Some("target_copied")
    );
    assert!(
        report.rows[0].skipped_reasons.iter().any(
            |reason| reason.contains("receipt phase is target_copied, not target_noncanonical")
        ),
        "{:?}",
        report.rows[0].skipped_reasons
    );

    let after = corpus_scans(&source_path, &target_path);
    let untouched = after[1].raw_row(&target_id).unwrap();
    assert_eq!(untouched.revision, revision);
    assert!(untouched.archived);
}

#[test]
fn copy_identity_drift_is_reported_but_never_repaired() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("legacy.db");
    let target_path = directory.path().join("shared.db");
    let page_path = "/wiki/copy-identity-drift";
    create_current_fixture(
        &source_path,
        &[fixture_entry("source", page_path, shared_metadata())],
    );
    create_current_fixture(&target_path, &[]);
    let scans = corpus_scans(&source_path, &target_path);
    let plan = build_plan(&scans).unwrap();
    let item = plan.items[0].clone();
    let target_id = item.target_id.clone().unwrap();
    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    apply_plan(&scans, &plan, &backup_dir).unwrap();

    damage_completed_target_as_sibling_race(&target_path, &target_id, &item, &plan.plan_id);

    // Every other clause of the repair signature now matches; drift the
    // target's text directly (a foreign writer, not the repair
    // machinery) so its copy identity stops matching the receipt's
    // captured source identity. This is the clause that binds the row to
    // *this content*, not merely to this row's shape, and it is the one
    // future refactors of `copy_identity_sha256` are most likely to
    // silently break.
    let target_connection = Connection::open(&target_path).unwrap();
    assert_eq!(
        target_connection
            .execute(
                "UPDATE memories SET text = ?1 WHERE id = ?2",
                rusqlite::params!["drifted after the receipt was captured", target_id],
            )
            .unwrap(),
        1
    );
    drop(target_connection);

    let current = corpus_scans(&source_path, &target_path);
    let damaged_target = current[1].raw_row(&target_id).unwrap();
    assert!(damaged_target.archived);
    let target_revision = damaged_target.revision;

    let repair_dir = directory.path().join("repair-backups");
    std::fs::create_dir(&repair_dir).unwrap();
    let report = repair_sibling_damage(&current, Some(&repair_dir)).unwrap();
    assert_eq!(report.inspected_rows, 1);
    assert_eq!(report.repairable, 0);
    assert_eq!(report.repaired, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.rows[0].outcome, "skipped");
    assert!(
        report.rows[0]
            .skipped_reasons
            .iter()
            .any(|reason| reason.contains("does not match the receipt item's")),
        "{:?}",
        report.rows[0].skipped_reasons
    );
    assert!(
        report.backups.is_empty(),
        "a run with nothing to repair must not touch the backup directory"
    );
    assert!(directory_snapshot(&repair_dir).is_empty());

    let after = corpus_scans(&source_path, &target_path);
    let untouched = after[1].raw_row(&target_id).unwrap();
    assert_eq!(untouched.revision, target_revision);
    assert!(untouched.archived);
    assert_eq!(
        parse_migration_receipt(untouched).unwrap().unwrap().phase,
        MigrationPhase::TargetNoncanonical,
        "an unrepaired row must keep the damage signature intact for a later, correct repair"
    );
}

#[test]
fn sibling_repair_command_dry_runs_before_it_writes() {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("global.db");
    let project_path = directory.path().join("project.db");
    let app_home = directory.path().join("home");
    let shared_path = app_home.join("projects/wiki/memory.db");
    let page_path = "/wiki/global-repair";
    std::fs::create_dir_all(shared_path.parent().unwrap()).unwrap();
    create_current_fixture(
        &global_path,
        &[fixture_entry("global", page_path, shared_metadata())],
    );
    create_current_fixture(&project_path, &[]);
    create_current_fixture(&shared_path, &[]);

    let backup_dir = directory.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();
    let applied = run_wiki_corpus_command(
        true,
        Some(WIKI_CORPUS_CONFIRMATION_TOKEN.to_string()),
        Some(backup_dir.clone()),
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .unwrap();
    let plan = applied.plan.clone().unwrap();
    let item = plan.items[0].clone();
    let target_id = item.target_id.clone().unwrap();
    damage_completed_target_as_sibling_race(&shared_path, &target_id, &item, &plan.plan_id);

    let repair_dir = directory.path().join("repair-backups");
    std::fs::create_dir(&repair_dir).unwrap();
    let before = db_snapshot_fingerprint(&db_snapshot(&shared_path));

    let refused = run_wiki_corpus_sibling_repair_command(
        true,
        Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
        Some(repair_dir.clone()),
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .expect_err("repair must refuse to ride along with --apply");
    assert!(
        refused.contains("cannot be combined with --apply"),
        "{refused}"
    );
    let refused = run_wiki_corpus_sibling_repair_command(
        false,
        Some("MIGRATE_WIKI_CORPUS_V1".to_string()),
        Some(repair_dir.clone()),
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .expect_err("the apply token must not confirm a repair");
    assert!(refused.contains("requires exact --confirm"), "{refused}");
    let refused = run_wiki_corpus_sibling_repair_command(
        false,
        None,
        Some(repair_dir.clone()),
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .expect_err("a backup directory without a token must not write");
    assert!(
        refused.contains("--backup-dir requires --confirm"),
        "{refused}"
    );
    let refused = run_wiki_corpus_sibling_repair_command(
        false,
        Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
        None,
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .expect_err("a confirmed repair without --backup-dir must not write");
    assert!(
        refused.contains("requires explicit --backup-dir"),
        "{refused}"
    );
    let refused = run_wiki_corpus_sibling_repair_command(
        false,
        Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
        Some(directory.path().join("does-not-exist")),
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .expect_err("a confirmed repair with a nonexistent backup directory must not write");
    assert!(
        refused.contains("requires an existing backup directory"),
        "{refused}"
    );

    let preview = run_wiki_corpus_sibling_repair_command(
        false,
        None,
        None,
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .unwrap();
    assert_eq!(preview.mode, "sibling_repair_preview");
    assert!(!preview.apply);
    let preview_repair = preview.sibling_repair.clone().unwrap();
    assert!(!preview_repair.confirmed);
    assert_eq!(preview_repair.repairable, 1);
    assert_eq!(preview_repair.repaired, 0);
    assert_eq!(preview_repair.rows[0].outcome, "repairable");
    assert_eq!(preview_repair.rows[0].id, target_id);
    assert!(preview_repair.backups.is_empty());
    assert!(preview_repair.backup_directory.is_none());
    assert_eq!(
        db_snapshot_fingerprint(&db_snapshot(&shared_path)),
        before,
        "the dry run must not write a single byte"
    );
    assert!(directory_snapshot(&repair_dir).is_empty());
    assert!(user_facing_ids(&shared_path, page_path).is_empty());

    let repaired = run_wiki_corpus_sibling_repair_command(
        false,
        Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
        Some(repair_dir.clone()),
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .unwrap();
    assert_eq!(repaired.mode, "sibling_repair");
    assert!(repaired.apply);
    let repaired_report = repaired.sibling_repair.clone().unwrap();
    assert!(repaired_report.confirmed);
    assert_eq!(repaired_report.repaired, 1);
    assert_eq!(repaired_report.rows[0].outcome, "repaired");
    assert_eq!(repaired_report.backups.len(), 1);
    assert_eq!(user_facing_ids(&shared_path, page_path), vec![target_id]);

    // A second confirmed run has nothing left to inspect.
    let again = run_wiki_corpus_sibling_repair_command(
        false,
        Some(WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN.to_string()),
        Some(repair_dir),
        None,
        &global_path,
        Some(&project_path),
        &app_home,
    )
    .unwrap();
    let again = again.sibling_repair.clone().unwrap();
    assert_eq!(again.inspected_rows, 0);
    assert_eq!(again.repaired, 0);
}

// -----------------------------------------------------------------
// `--adopt-legacy` (tachi#1624)
// -----------------------------------------------------------------

/// The shape the host runs: a legacy global, no bound project DB, and a
/// Tachi home with no `wiki` store yet.
struct AdoptionFixture {
    _directory: tempfile::TempDir,
    global_path: PathBuf,
    app_home: PathBuf,
}

impl AdoptionFixture {
    fn new(entries: &[MemoryEntry]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let global_path = directory.path().join("global.db");
        let app_home = directory.path().join("home");
        std::fs::create_dir_all(&app_home).unwrap();
        create_current_fixture(&global_path, entries);
        Self {
            _directory: directory,
            global_path,
            app_home,
        }
    }

    fn target(&self) -> PathBuf {
        legacy_adoption_target_path(&self.app_home)
    }

    fn target_dir(&self) -> PathBuf {
        self.target().parent().unwrap().to_path_buf()
    }

    fn run(&self, confirm: Option<&str>) -> Result<WikiCorpusReport, String> {
        run_wiki_corpus_legacy_adoption_command(
            false,
            confirm.map(str::to_string),
            None,
            None,
            &self.global_path,
            None,
            &self.app_home,
        )
    }

    fn adopt(&self) -> LegacyAdoptionReport {
        self.run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
            .expect("confirmed adoption")
            .legacy_adoption
            .expect("legacy adoption report")
    }
}

fn adoption_entry_fixture(id: &str, path: &str, metadata: Value) -> MemoryEntry {
    let mut entry = fixture_entry(id, path, metadata);
    // Distinct bodies: memcore's write path consolidates near-duplicates
    // above a token-Jaccard of 0.9, and every `fixture_entry` shares one
    // literal body.
    entry.text = format!("adoption fixture body for {id} at {path}");
    entry.summary = format!("adoption fixture summary {id}");
    entry
}

/// Overwrite the lifecycle columns `upsert` stamps with wall clock, so a
/// test can assert verbatim preservation of values a write path would
/// never produce.
#[allow(clippy::too_many_arguments)]
fn force_legacy_lifecycle(
    path: &Path,
    id: &str,
    created_at: &str,
    updated_at: &str,
    revision: i64,
    archived: bool,
    valid_until: Option<&str>,
    superseded_by: Option<&str>,
) {
    let conn = Connection::open(path).unwrap();
    let changed = conn
        .execute(
            "UPDATE memories
                 SET created_at = ?2, updated_at = ?3, revision = ?4, archived = ?5,
                     valid_until = ?6, superseded_by = ?7
                 WHERE id = ?1",
            rusqlite::params![
                id,
                created_at,
                updated_at,
                revision,
                archived,
                valid_until,
                superseded_by
            ],
        )
        .unwrap();
    assert_eq!(changed, 1, "fixture row {id} must exist");
}

/// Round-2 bug C. Overwrite the usage-counter columns directly, bypassing
/// `upsert` the same way `force_legacy_lifecycle` does: the ordinary
/// write path does not accept caller-supplied `access_count`/
/// `scored_count`/`last_access`/`last_use_at` (they are bumped by the
/// search/recall path, never set by a write), so this is the only way to
/// build a fixture carrying values a legacy row genuinely accumulated
/// before adoption.
fn force_legacy_usage_counters(
    path: &Path,
    id: &str,
    access_count: i64,
    scored_count: i64,
    last_access: Option<&str>,
    last_use_at: Option<&str>,
) {
    let conn = Connection::open(path).unwrap();
    let changed = conn
        .execute(
            "UPDATE memories
                 SET access_count = ?2, scored_count = ?3, last_access = ?4, last_use_at = ?5
                 WHERE id = ?1",
            rusqlite::params![id, access_count, scored_count, last_access, last_use_at],
        )
        .unwrap();
    assert_eq!(changed, 1, "fixture row {id} must exist");
}

/// Overwrite the `path` column directly, bypassing `upsert`'s
/// `normalize_path` call so the fixture can hold a raw legacy path the
/// current write path would never itself persist -- the shape genuinely
/// legacy data (written before normalization existed, or by a path
/// outside this write seam) can carry.
fn force_legacy_path(path: &Path, id: &str, raw_path: &str) {
    let conn = Connection::open(path).unwrap();
    let changed = conn
        .execute(
            "UPDATE memories SET path = ?2 WHERE id = ?1",
            rusqlite::params![id, raw_path],
        )
        .unwrap();
    assert_eq!(changed, 1, "fixture row {id} must exist");
}

struct DestinationRow {
    created_at: String,
    updated_at: String,
    revision: i64,
    archived: bool,
    valid_until: Option<String>,
    superseded_by: Option<String>,
    metadata: Value,
    path: String,
    // Round-2 bug C.
    access_count: i64,
    scored_count: i64,
    last_access: Option<String>,
    last_use_at: Option<String>,
}

fn destination_row(target: &Path, id: &str) -> DestinationRow {
    let conn = Connection::open(target).unwrap();
    conn.query_row(
        "SELECT created_at, updated_at, revision, archived, valid_until, superseded_by,
                    metadata, path, access_count, scored_count, last_access, last_use_at
             FROM memories WHERE id = ?1",
        [id],
        |row| {
            let metadata: String = row.get(6)?;
            Ok(DestinationRow {
                created_at: row.get(0)?,
                updated_at: row.get(1)?,
                revision: row.get(2)?,
                archived: row.get::<_, i64>(3)? != 0,
                valid_until: row.get(4)?,
                superseded_by: row.get(5)?,
                metadata: serde_json::from_str(&metadata).unwrap(),
                path: row.get(7)?,
                access_count: row.get(8)?,
                scored_count: row.get(9)?,
                last_access: row.get(10)?,
                last_use_at: row.get(11)?,
            })
        },
    )
    .unwrap_or_else(|error| panic!("adopted row {id} must exist: {error}"))
}

/// Give an already-seeded row an id the ordinary write path would refuse,
/// which is the only way to build a fixture for E3. Renaming beats a raw
/// `INSERT`: the reserved-reference insert guard is a trigger over
/// `memories`, and preparing an `INSERT` on a connection that has not
/// registered memcore's guard function would fail for reasons unrelated to
/// what is being tested.
fn rename_legacy_row_id(path: &Path, from: &str, to: &str) {
    let conn = Connection::open(path).unwrap();
    let changed = conn
        .execute(
            "UPDATE memories SET id = ?2 WHERE id = ?1",
            rusqlite::params![from, to],
        )
        .unwrap();
    assert_eq!(changed, 1, "fixture row {from} must exist");
}

/// T1
#[test]
fn adopt_legacy_refuses_wrong_confirm_token() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "adopt-token",
        "/wiki/adopt/token",
        json!({"lifecycle": "active"}),
    )]);

    let error = fixture
        .run(Some(WIKI_CORPUS_CONFIRMATION_TOKEN))
        .expect_err("the migration token must not confirm an adoption");
    assert!(
        error.contains(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN),
        "{error}"
    );
    assert!(
        std::fs::symlink_metadata(fixture.target()).is_err(),
        "a refused adoption must not create the store"
    );
}

/// T2
#[test]
fn adopt_legacy_refuses_apply_plan_and_backup_dir() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "adopt-flags",
        "/wiki/adopt/flags",
        json!({"lifecycle": "active"}),
    )]);
    let token = Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN.to_string());

    let error = run_wiki_corpus_legacy_adoption_command(
        true,
        token.clone(),
        None,
        None,
        &fixture.global_path,
        None,
        &fixture.app_home,
    )
    .expect_err("--adopt-legacy must refuse to ride along with --apply");
    assert_eq!(
        error,
        "--adopt-legacy is its own confirmed mode and cannot be combined with --apply"
    );
    assert!(std::fs::symlink_metadata(fixture.target()).is_err());

    let error = run_wiki_corpus_legacy_adoption_command(
        false,
        token.clone(),
        None,
        Some(fixture.app_home.join("plan.json")),
        &fixture.global_path,
        None,
        &fixture.app_home,
    )
    .expect_err("--adopt-legacy takes no plan");
    assert_eq!(
        error,
        "--adopt-legacy does not take --plan; the adoption set is derived from the legacy \
             store's own classification"
    );
    assert!(std::fs::symlink_metadata(fixture.target()).is_err());

    let error = run_wiki_corpus_legacy_adoption_command(
        false,
        token,
        Some(fixture.app_home.join("backups")),
        None,
        &fixture.global_path,
        None,
        &fixture.app_home,
    )
    .expect_err("--adopt-legacy takes no backup directory");
    assert_eq!(
        error,
        "--adopt-legacy does not take --backup-dir; it never writes to an existing store — \
             back up the legacy global out-of-band before running"
    );
    assert!(std::fs::symlink_metadata(fixture.target()).is_err());
}

/// T3
#[test]
fn adopt_legacy_preview_creates_nothing() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "adopt-preview",
        "/wiki/adopt/preview",
        json!({"lifecycle": "active"}),
    )]);
    let before = directory_snapshot(&fixture.app_home);

    let report = fixture.run(None).expect("preview must succeed");
    assert_eq!(report.mode, "legacy_adoption_preview");
    assert!(!report.apply);
    let adoption = report.legacy_adoption.expect("legacy adoption report");

    assert!(!adoption.confirmed);
    assert!(!adoption.target_created);
    assert!(!adoption.target_existed_before);
    assert_eq!(adoption.rows_imported, 0);
    assert_eq!(adoption.observed_lifecycle_checksum, None);
    assert_eq!(adoption.checksums_match, None);
    assert!(adoption.eligible_rows > 0);
    assert!(!adoption.expected_lifecycle_checksum.is_empty());
    assert_eq!(adoption.default_retrievable_rows, adoption.eligible_rows);
    assert_eq!(
        adoption.derived_lifecycle_counts.get("active").copied(),
        Some(adoption.eligible_rows)
    );
    assert!(
        std::fs::symlink_metadata(fixture.target()).is_err(),
        "preview must create nothing"
    );
    assert_eq!(directory_snapshot(&fixture.app_home), before);
}

/// T4
#[test]
fn adopt_legacy_refuses_when_wiki_store_already_exists() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "adopt-occupied",
        "/wiki/adopt/occupied",
        json!({"lifecycle": "active"}),
    )]);
    std::fs::create_dir_all(fixture.target_dir()).unwrap();
    create_current_fixture(&fixture.target(), &[]);
    let occupant = db_snapshot_fingerprint(&db_snapshot(&fixture.target()));

    let error = fixture
        .run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
        .expect_err("adoption is bootstrap-only");
    assert!(error.contains("bootstrap-only mode"), "{error}");
    assert!(
        error.contains("remove") && error.contains("re-run"),
        "the remedy must be removal and re-run, never --apply: {error}"
    );
    assert!(
        !error.contains("--apply"),
        "--apply refuses any absent involved store on a --no-project-db host: {error}"
    );
    assert_eq!(
        db_snapshot_fingerprint(&db_snapshot(&fixture.target())),
        occupant,
        "the refused run must not touch the existing store"
    );

    // The preview stays a safe probe against an occupied home.
    let preview = fixture.run(None).expect("preview must still report");
    let adoption = preview.legacy_adoption.unwrap();
    assert!(adoption.target_existed_before);
    assert!(!adoption.target_created);
}

/// T4b, round-2 bug A: the existing-target refusal must fire before the
/// legacy store is opened at all, not merely before the destination is
/// written. Proven with a discriminator rather than instrumentation: the
/// legacy source is corrupted after the fixture is built, so *if* the
/// confirmed path ever opened it, `inventory_store` would capture a read
/// failure and the command would surface "legacy global store is
/// unreadable" instead of the bootstrap-only refusal. Getting the
/// bootstrap-only wording proves the legacy open never happened.
#[test]
fn adopt_legacy_refuses_existing_target_before_touching_legacy_store() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "adopt-order",
        "/wiki/adopt/order",
        json!({"lifecycle": "active"}),
    )]);
    std::fs::create_dir_all(fixture.target_dir()).unwrap();
    create_current_fixture(&fixture.target(), &[]);

    // Corrupt the legacy source only after the fixture (and the
    // preexisting target) are built. A second confirmed run against an
    // occupied target with a legacy source that would error if opened is
    // exactly the shape a refusal-ordering regression would mishandle.
    std::fs::write(&fixture.global_path, b"not a sqlite database").unwrap();

    let error = fixture
        .run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
        .expect_err("adoption is bootstrap-only");
    assert!(
        error.contains("bootstrap-only mode"),
        "the existing-target refusal must fire before the corrupt legacy store is ever \
             opened: {error}"
    );
    assert!(
        !error.contains("legacy global store is unreadable"),
        "a legacy-read error here would mean the legacy store was opened before the \
             existing-target check: {error}"
    );
}

/// T5
#[test]
fn adopt_legacy_eligibility_partition() {
    let fixture = AdoptionFixture::new(&[
        adoption_entry_fixture("row-a", "/wiki/a", json!({"lifecycle": "active"})),
        adoption_entry_fixture(
            "row-b",
            "/wiki/b",
            json!({"knowledge_scope": "project", "lifecycle": "active"}),
        ),
        // `rem` would be the more natural operational marker, but the
        // ordinary write path strips it from caller-supplied metadata, so
        // a fixture seeded through `upsert` cannot carry it.
        adoption_entry_fixture(
            "row-c",
            "/wiki/c",
            json!({"operational_snapshot": true, "lifecycle": "active"}),
        ),
        adoption_entry_fixture(
            "row-d",
            "/wiki/d",
            json!({"authority_record": true, "lifecycle": "active"}),
        ),
        adoption_entry_fixture(
            "row-e",
            "/kanban/e",
            json!({"artifact_kind": "wiki", "lifecycle": "active"}),
        ),
        adoption_entry_fixture("row-f", "/wiki/f", json!({"lifecycle": "active"})),
    ]);
    // E3's only case E1 does not already shadow. A `wiki-rem:` id or a
    // `/wiki/_log` row carries an operational marker and is excluded by E1
    // first; an `anchor:`-prefixed row on a wiki path is not, so it is the
    // one that proves the reserved-identity rule is load-bearing.
    rename_legacy_row_id(&fixture.global_path, "row-f", "anchor:f");

    let report = fixture.run(None).expect("preview");
    let adoption = report.legacy_adoption.unwrap();

    assert_eq!(adoption.adopted_ids, vec!["row-a".to_string()]);
    assert_eq!(adoption.eligible_rows, 1);
    let reasons = adoption
        .skipped
        .iter()
        .map(|skip| (skip.id.as_str(), skip.reason.as_str()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        reasons,
        BTreeMap::from([
            ("anchor:f", "reserved_identity"),
            ("row-b", "classification=project_bound"),
            ("row-c", "classification=operational_snapshot"),
            ("row-d", "classification=authority_record"),
            ("row-e", "path_not_public_knowledge_artifact"),
        ])
    );
    // B3: the six seeded rows are all wiki-related; no `classification_missing`
    // entry may appear, and the count is the classified rows, not the table.
    assert_eq!(adoption.wiki_related_rows, 6);
    assert!(adoption
        .skipped
        .iter()
        .all(|skip| skip.reason != "classification_missing"));
}

/// B3 directly: a legacy store holding non-wiki rows must not inflate
/// `wiki_related_rows` or bury the skip histogram, because `load_rows`
/// selects the whole `memories` table with no wiki predicate.
#[test]
fn adopt_legacy_ignores_rows_the_classifier_calls_non_wiki() {
    let mut kanban = adoption_entry_fixture("kanban-row", "/kanban/card", json!({}));
    kanban.category = "kanban".to_string();
    kanban.source = "kanban".to_string();
    kanban.domain = None;
    let fixture = AdoptionFixture::new(&[
        adoption_entry_fixture("wiki-row", "/wiki/kept", json!({"lifecycle": "active"})),
        kanban,
    ]);

    let adoption = fixture.run(None).expect("preview").legacy_adoption.unwrap();
    assert_eq!(
        adoption.wiki_related_rows, 1,
        "only classified rows are part of this corpus"
    );
    assert!(
        adoption.skipped.is_empty(),
        "a non-wiki row is not a skipped adoption candidate: {:?}",
        adoption.skipped
    );
    assert_eq!(adoption.adopted_ids, vec!["wiki-row".to_string()]);
}

/// T6
#[test]
fn adopt_legacy_preserves_lifecycle_verbatim() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "verbatim",
        "/wiki/adopt/verbatim",
        json!({"lifecycle": "active"}),
    )]);
    force_legacy_lifecycle(
        &fixture.global_path,
        "verbatim",
        "2019-01-01T00:00:00Z",
        "2020-02-02T00:00:00Z",
        7,
        true,
        Some(""),
        None,
    );

    let adoption = fixture.adopt();
    assert_eq!(adoption.checksums_match, Some(true));
    assert_eq!(adoption.rows_imported, 1);

    let stored = destination_row(&fixture.target(), "verbatim");
    assert_eq!(stored.created_at, "2019-01-01T00:00:00Z");
    assert_eq!(stored.updated_at, "2020-02-02T00:00:00Z");
    assert_eq!(stored.revision, 7);
    assert!(stored.archived);
    assert_eq!(
        stored.valid_until,
        Some(String::new()),
        "an empty string is a distinct value from NULL and must survive as such"
    );
    assert_eq!(stored.superseded_by, None);
    assert_eq!(stored.path, "/wiki/adopt/verbatim");
}

/// T6b, round-2 bug C: `access_count`/`scored_count`/`last_access`/
/// `last_use_at` are usage history, not lifecycle policy, but
/// `import_snapshot_batch` writes them straight from the `MemoryEntry`
/// it is handed (memcore `snapshot_import.rs`). The loader used to
/// hardcode all four to zero/None regardless of what the legacy row
/// actually carried, so every adopted row silently lost its usage
/// history while `checksums_match` still passed -- the lifecycle
/// checksum's coverage is `archived`/`created_at`/`id`/`revision`/
/// `superseded_by`/`updated_at`/`valid_until` only (see the field
/// comment on `RawRow`), deliberately not widened here to cover these
/// four; this test is the thing that would catch a regression, not the
/// checksum.
#[test]
fn adopt_legacy_preserves_usage_counters_verbatim() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "usage-verbatim",
        "/wiki/adopt/usage-verbatim",
        json!({"lifecycle": "active"}),
    )]);
    force_legacy_usage_counters(
        &fixture.global_path,
        "usage-verbatim",
        42,
        17,
        Some("2024-03-01T00:00:00Z"),
        Some("2024-03-02T00:00:00Z"),
    );

    let adoption = fixture.adopt();
    assert_eq!(adoption.rows_imported, 1);

    let stored = destination_row(&fixture.target(), "usage-verbatim");
    assert_eq!(stored.access_count, 42);
    assert_eq!(stored.scored_count, 17);
    assert_eq!(stored.last_access, Some("2024-03-01T00:00:00Z".to_string()));
    assert_eq!(stored.last_use_at, Some("2024-03-02T00:00:00Z".to_string()));
}

/// T7
#[test]
fn adopt_legacy_reports_dangling_supersession() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "dangling",
        "/wiki/adopt/dangling",
        json!({"lifecycle": "active"}),
    )]);
    force_legacy_lifecycle(
        &fixture.global_path,
        "dangling",
        "2019-01-01T00:00:00Z",
        "2019-01-01T00:00:00Z",
        1,
        false,
        None,
        Some("not-adopted"),
    );

    let adoption = fixture.adopt();
    assert_eq!(adoption.rows_imported, 1);
    assert!(!adoption.had_failures, "a dangling edge is never fatal");
    assert_eq!(adoption.dangling_supersessions.len(), 1);
    assert_eq!(adoption.dangling_supersessions[0].id, "dangling");
    assert_eq!(
        adoption.dangling_supersessions[0].superseded_by,
        "not-adopted"
    );

    assert_eq!(
        destination_row(&fixture.target(), "dangling").superseded_by,
        Some("not-adopted".to_string()),
        "the edge is preserved, never repaired"
    );
}

/// T8
#[test]
fn adopt_legacy_marker_is_the_only_metadata_delta() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "marker",
        "/wiki/adopt/marker",
        json!({
            "lifecycle": "active",
            "artifact_kind": "wiki",
            "nested": {"b": 1, "a": [true, null, "x"]},
        }),
    )]);

    let adoption = fixture.adopt();
    assert_eq!(
        adoption.provenance_marker_key,
        WIKI_LEGACY_ADOPTION_MARKER_KEY
    );

    // Compared against the source row **as stored**, not against the
    // literal this test passed in: the ordinary write path that seeded the
    // fixture has its own metadata sanitization, and the claim under test
    // is that adoption adds nothing beyond the marker to whatever the
    // legacy store actually holds.
    let stored_source = legacy_source_rows(&fixture.global_path)["marker"]["metadata"]
        .as_str()
        .map(|raw| serde_json::from_str::<Value>(raw).unwrap())
        .expect("source metadata");
    assert!(
        stored_source
            .as_object()
            .is_some_and(|object| object.contains_key("nested")),
        "the fixture must actually carry the metadata it claims: {stored_source}"
    );

    let metadata = destination_row(&fixture.target(), "marker").metadata;
    let mut object = metadata.as_object().expect("object metadata").clone();
    let marker = object
        .remove(WIKI_LEGACY_ADOPTION_MARKER_KEY)
        .expect("adoption marker");
    assert_eq!(
        Value::Object(object),
        stored_source,
        "the marker must be the only metadata delta"
    );

    assert_eq!(marker["review_status"], json!("review_pending"));
    assert_eq!(marker["reviewed"], json!(false));
    assert_eq!(
        marker["confirm_token"],
        json!(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN)
    );
    assert_eq!(marker["source_id"], json!("marker"));
    assert_eq!(marker["source_store"], json!("legacy_global"));
    assert_eq!(marker["adoption_run_id"], json!(adoption.adoption_run_id));

    // No assertion is smuggled in at metadata top level. `review_status`
    // in particular would demote the derived lifecycle if it lived here.
    for forbidden in [
        "review_status",
        "reviewed",
        "knowledge_scope",
        "origin_projects",
        "applies_to",
        "applicability_status",
        "review_receipt",
        "source_bundle_hash",
        RECEIPT_KEY,
    ] {
        assert!(
            metadata.get(forbidden).is_none(),
            "adoption must not write a top-level '{forbidden}'"
        );
    }
}

/// T9
#[test]
fn adopt_legacy_never_mutates_the_source() {
    let fixture = AdoptionFixture::new(&[
        adoption_entry_fixture(
            "src-one",
            "/wiki/adopt/src-one",
            json!({"lifecycle": "active"}),
        ),
        adoption_entry_fixture(
            "src-two",
            "/wiki/adopt/src-two",
            json!({"lifecycle": "active"}),
        ),
    ]);
    let before = legacy_source_rows(&fixture.global_path);

    let adoption = fixture.adopt();
    assert_eq!(adoption.rows_imported, 2);
    assert_eq!(
        adoption.legacy_row_digest_after,
        Some(adoption.legacy_row_digest_before.clone()),
        "adoption is read-only on the legacy global"
    );
    assert_eq!(legacy_source_rows(&fixture.global_path), before);
}

/// T10
#[test]
fn adopt_legacy_stamps_store_identity_role_wiki() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "stamped",
        "/wiki/adopt/stamped",
        json!({"lifecycle": "active"}),
    )]);

    let adoption = fixture.adopt();
    assert!(adoption.target_created);
    assert_eq!(adoption.target_store_role_stamped, Some(true));
    assert_eq!(adoption.target_removed_after_failure, None);

    let target = fixture.target();
    let reopened = MemoryStore::open_existing_read_write(target.to_str().unwrap())
        .expect("reopen the adopted store");
    assert!(
        reopened.is_wiki_corpus_store(),
        "the role must be derived from the stamp on a plain reopen"
    );
    drop(reopened);

    // The stamp itself, read out of `hard_state` rather than inferred from
    // the label the bootstrap passed in.
    let conn = Connection::open(&target).unwrap();
    let stamp: String = conn
        .query_row(
            "SELECT value_json FROM hard_state
                 WHERE namespace = 'store_identity' AND key = 'role'",
            [],
            |row| row.get(0),
        )
        .expect("store_identity role stamp");
    let stamp: Value = serde_json::from_str(&stamp).unwrap();
    assert_eq!(
        stamp["value"],
        json!(memcore::path_router::WIKI_CORPUS_DB_LABEL)
    );
    assert_eq!(stamp["conferred_by"], json!("open:create-fresh"));

    let profile: String = conn
        .query_row(
            "SELECT value_json FROM hard_state
                 WHERE namespace = 'store_identity' AND key = 'profile'",
            [],
            |row| row.get(0),
        )
        .expect("store_identity profile stamp");
    let profile: Value = serde_json::from_str(&profile).unwrap();
    assert_eq!(profile["conferred_by"], json!("open:create-fresh"));
}

/// T11
#[test]
fn adopt_legacy_preserves_vectors() {
    let mut vectored = adoption_entry_fixture(
        "vectored",
        "/wiki/adopt/vectored",
        json!({"lifecycle": "active"}),
    );
    vectored.vector = Some(vec![0.25_f32; crate::status_ops::EXPECTED_EMBEDDING_DIM]);
    let plain =
        adoption_entry_fixture("plain", "/wiki/adopt/plain", json!({"lifecycle": "active"}));
    let fixture = AdoptionFixture::new(&[vectored, plain]);

    let adoption = fixture.adopt();
    assert_eq!(adoption.rows_imported, 2);
    assert_eq!(adoption.vectors_imported, 1);
    assert_eq!(adoption.vectors_absent, 1);
    assert_eq!(
        adoption.observed_vector_checksum,
        Some(adoption.expected_vector_checksum.clone())
    );
    assert_eq!(adoption.checksums_match, Some(true));
}

/// T12
#[test]
fn adopt_legacy_refuses_to_create_an_empty_store() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "all-project",
        "/wiki/adopt/all-project",
        json!({"knowledge_scope": "project", "lifecycle": "active"}),
    )]);

    let error = fixture
        .run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
        .expect_err("an empty adoption set must not create a store");
    assert_eq!(
        error,
        "no eligible legacy rows to adopt; refusing to create an empty wiki store"
    );
    assert!(
        std::fs::symlink_metadata(fixture.target_dir()).is_err(),
        "the store directory must not exist"
    );
}

/// T13 (rewritten): the reachable retrievability guard. A store whose
/// adopted rows all derive a non-retrievable lifecycle would flip the
/// named-store existence gate -- silencing the zero-store search refusal
/// -- without making a single row findable.
#[test]
fn adopt_legacy_refuses_when_no_adopted_row_would_be_retrievable() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "pending-only",
        "/wiki/adopt/pending-only",
        json!({"lifecycle": "pending_review"}),
    )]);

    let preview = fixture.run(None).expect("preview").legacy_adoption.unwrap();
    assert_eq!(preview.eligible_rows, 1);
    assert_eq!(preview.default_retrievable_rows, 0);
    assert_eq!(
        preview
            .derived_lifecycle_counts
            .get("pending_review")
            .copied(),
        Some(1)
    );

    let error = fixture
        .run(Some(WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN))
        .expect_err("a store nothing can be read out of must not be created");
    assert!(
        error.contains("default-retrievable") && error.contains("zero-store"),
        "{error}"
    );
    assert!(std::fs::symlink_metadata(fixture.target_dir()).is_err());
}

/// The path column is outside the lifecycle checksum, so a silent rewrite
/// would otherwise verify clean. It is disclosed before the run and
/// re-read after it.
#[test]
fn adopt_legacy_discloses_and_verifies_path_normalization() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "rewritten",
        "/wiki/adopt/rewritten",
        json!({"lifecycle": "active"}),
    )]);
    // `AdoptionFixture::new` seeds the legacy row through `upsert`, which
    // normalizes `path` on write (memcore's `normalize_path` call in
    // `upsert_prepared_within_tx`) -- so no fixture path passed through
    // that constructor can ever land in the legacy DB un-normalized.
    // Force the raw legacy path in directly, the same idiom
    // `force_legacy_lifecycle` uses for columns the current write path
    // would never itself produce.
    force_legacy_path(&fixture.global_path, "rewritten", "/wiki/adopt/rewritten/");

    let preview = fixture.run(None).expect("preview").legacy_adoption.unwrap();
    assert_eq!(preview.path_rewrites.len(), 1);
    assert_eq!(preview.path_rewrites[0].id, "rewritten");
    assert_eq!(
        preview.path_rewrites[0].source_path,
        "/wiki/adopt/rewritten/"
    );
    assert_eq!(
        preview.path_rewrites[0].stored_path,
        "/wiki/adopt/rewritten"
    );

    let adoption = fixture.adopt();
    assert!(!adoption.had_failures);
    assert_eq!(
        destination_row(&fixture.target(), "rewritten").path,
        "/wiki/adopt/rewritten"
    );
}

/// B4, stated by the receipt rather than left for a later reader: after
/// adoption every adopted path lives in two logical stores, and the
/// duplicate-path pass pins both copies to `manual_review`.
#[test]
fn adopt_legacy_reports_that_it_pins_the_reconciler_to_manual_review() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "pinned",
        "/wiki/adopt/pinned",
        json!({"lifecycle": "active"}),
    )]);

    let adoption = fixture.adopt();
    assert!(adoption
        .reconciler_impact
        .contains("build_plan only emits shared_candidate items"));
    assert_eq!(
        adoption.legacy_rows_forced_to_manual_review_by_duplicate_path,
        Some(1),
        "the duplicate-path pass must be measured, not asserted"
    );

    // And the mechanism itself: the next preview sees the row as a
    // cross-store duplicate in both stores.
    let preview = run_wiki_corpus_command(
        false,
        None,
        None,
        None,
        &fixture.global_path,
        None,
        &fixture.app_home,
    )
    .expect("post-adoption corpus preview");
    for store_ref in ["legacy_global", "named:wiki"] {
        let store = preview
            .stores
            .iter()
            .find(|store| store.logical_store_ref == store_ref)
            .unwrap_or_else(|| panic!("{store_ref} must be inventoried"));
        let row = store
            .rows
            .iter()
            .find(|row| row.id == "pinned")
            .unwrap_or_else(|| panic!("{store_ref} must hold the adopted row"));
        assert_eq!(row.classification, CorpusClassification::ManualReview);
        assert!(row
            .reasons
            .iter()
            .any(|reason| reason == "duplicate_normalized_path_across_logical_stores"));
    }
}

/// The three pre-existing modes must keep their exact JSON shape: the new
/// field is `skip_serializing_if = "Option::is_none"`, so `legacy_adoption`
/// may not appear in a preview, apply, or repair report.
#[test]
fn adoption_field_is_absent_from_every_other_mode() {
    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "shape",
        "/wiki/adopt/shape",
        json!({"lifecycle": "active"}),
    )]);

    for report in [
        run_wiki_corpus_command(
            false,
            None,
            None,
            None,
            &fixture.global_path,
            None,
            &fixture.app_home,
        )
        .expect("preview"),
        run_wiki_corpus_sibling_repair_command(
            false,
            None,
            None,
            None,
            &fixture.global_path,
            None,
            &fixture.app_home,
        )
        .expect("repair preview"),
    ] {
        assert!(!report.legacy_adoption_had_failures());
        let value = serde_json::to_value(&report).unwrap();
        assert!(
            value.get("legacy_adoption").is_none(),
            "legacy_adoption must not appear in mode '{}'",
            report.mode
        );
    }
}

/// Round-2 bug B: a preexisting, empty `target_dir` -- no db file inside,
/// so `target_existed_before` is `false` and the confirmed run proceeds
/// past the Bug A gate -- is not this run's to remove wholesale on
/// failure. Only the db file (and its WAL/SHM sidecars) this run itself
/// writes belong to it. Forced deterministically: the preexisting
/// `target_dir` is made read-only before the confirmed run, so
/// `MemoryStore::open_with_label_and_context`'s `create_fresh()` cannot
/// write the db file into it and `adopt_into_bootstrapped_store` fails at
/// its very first step, before anything is written.
#[cfg(unix)]
#[test]
fn adopt_legacy_failure_cleanup_spares_preexisting_dir() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = AdoptionFixture::new(&[adoption_entry_fixture(
        "adopt-preexisting-dir",
        "/wiki/adopt/preexisting-dir",
        json!({"lifecycle": "active"}),
    )]);
    let target_dir = fixture.target_dir();
    std::fs::create_dir_all(&target_dir).unwrap();
    std::fs::set_permissions(&target_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    // Elevated privileges (e.g. root in some CI containers) bypass
    // directory write-permission checks entirely, which would make this
    // probe meaningless (the bootstrap write would silently succeed).
    // Detect that up front and skip rather than assert something
    // environment-dependent.
    let probe_path = target_dir.join("permission-probe");
    let permission_enforced = File::create(&probe_path).is_err();
    let _ = std::fs::remove_file(&probe_path);
    if !permission_enforced {
        std::fs::set_permissions(&target_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        eprintln!(
            "skipping adopt_legacy_failure_cleanup_spares_preexisting_dir: target_dir \
                 write permission was not enforced (root?)"
        );
        return;
    }

    let report = fixture.adopt();
    std::fs::set_permissions(&target_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(report.had_failures);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("cannot bootstrap wiki store")),
        "expected a bootstrap failure: {:?}",
        report.errors
    );
    assert!(
        target_dir.is_dir(),
        "the preexisting target_dir must survive the failure cleanup"
    );
    assert!(
        !fixture.target().exists(),
        "no db file may remain inside the preexisting target_dir"
    );
    assert_eq!(
        report.target_removed_after_failure,
        Some(true),
        "nothing was written before the bootstrap failed, so file-only removal is a no-op \
             success"
    );
    let remediation = report.remediation.expect("remediation must be set");
    assert!(
        remediation.contains("preexisted this run") && remediation.contains("untouched"),
        "remediation must state the preexisting-dir semantics: {remediation}"
    );
}

fn legacy_source_rows(path: &Path) -> BTreeMap<String, Value> {
    let conn = Connection::open(path).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT id, path, revision, archived, metadata, superseded_by, created_at,
                        updated_at, valid_until
                 FROM memories ORDER BY id ASC",
        )
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                json!({
                    "path": row.get::<_, String>(1)?,
                    "revision": row.get::<_, i64>(2)?,
                    "archived": row.get::<_, i64>(3)?,
                    "metadata": row.get::<_, String>(4)?,
                    "superseded_by": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                    "updated_at": row.get::<_, String>(7)?,
                    "valid_until": row.get::<_, Option<String>>(8)?,
                }),
            ))
        })
        .unwrap()
        .collect::<Result<BTreeMap<_, _>, _>>()
        .unwrap();
    rows
}
