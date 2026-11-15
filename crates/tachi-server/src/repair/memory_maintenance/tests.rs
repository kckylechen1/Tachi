use super::*;
use crate::manifest::{DbEntry, DbRole, Manifest};

struct Fixture {
    _dir: tempfile::TempDir,
    app_home: PathBuf,
    db_path: PathBuf,
    plan_path: PathBuf,
}

fn fixture_entry(id: &str, path: &str) -> memcore::MemoryEntry {
    memcore::MemoryEntry {
        id: id.to_string(),
        path: path.to_string(),
        summary: "PRIVATE-SUMMARY-SENTINEL".to_string(),
        text: "PRIVATE-BODY-SENTINEL".to_string(),
        importance: 0.7,
        timestamp: "2026-08-01T00:00:00Z".to_string(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "test".to_string(),
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
        metadata: serde_json::json!({"secret":"PRIVATE-METADATA-SENTINEL"}),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let app_home = dir.path().join("app-home");
    std::fs::create_dir_all(&app_home).unwrap();
    let db_path = dir.path().join(memcore::MEMORY_DB_FILENAME);
    let mut store = MemoryStore::open(&db_path.to_string_lossy()).unwrap();
    store
        .insert_if_absent(&fixture_entry("delete-me", "/notes/private"))
        .unwrap();
    store
        .recall_cache_store("cache:test", "gen", "query", "[]", 0, false)
        .unwrap();
    drop(store);
    let canonical = std::fs::canonicalize(&db_path).unwrap();
    let mut manifest = Manifest::empty();
    manifest.dbs.push(DbEntry {
        path: canonical.to_string_lossy().into_owned(),
        role: DbRole::Project,
        owner: "tachi".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: String::new(),
        last_classification: "healthy".to_string(),
        scope_hint: "project:test".to_string(),
        notes: String::new(),
        aliases: Vec::new(),
    });
    manifest.save(&app_home.join("manifest.json")).unwrap();
    let plan_path = dir.path().join("delete-plan.json");
    Fixture {
        _dir: dir,
        app_home,
        db_path,
        plan_path,
    }
}

fn plan_delete(fixture: &Fixture) {
    run_delete(
        DeleteAction::Plan {
            db: fixture.db_path.clone(),
            id: "delete-me".to_string(),
            out: fixture.plan_path.clone(),
        },
        &fixture.app_home,
    )
    .unwrap();
}

fn exact_file_bytes(paths: &[PathBuf]) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    paths
        .iter()
        .map(|path| (path.clone(), std::fs::read(path).ok()))
        .collect()
}

fn memory_exists(path: &Path) -> bool {
    let store = MemoryStore::open_read_only(&path.to_string_lossy()).unwrap();
    store.get("delete-me").unwrap().is_some()
}

#[test]
fn delete_plan_changes_only_requested_artifact_and_contains_no_private_content() {
    let fixture = fixture();
    let watched = [
        fixture.db_path.clone(),
        PathBuf::from(format!("{}-wal", fixture.db_path.display())),
        PathBuf::from(format!("{}-shm", fixture.db_path.display())),
        fixture.app_home.join("manifest.json"),
        fixture.app_home.join("daemon.lock"),
        fixture.app_home.join("daemon.pid"),
    ];
    let before = exact_file_bytes(&watched);

    plan_delete(&fixture);

    assert_eq!(exact_file_bytes(&watched), before);
    assert!(!receipt_path(&fixture.plan_path).exists());
    let bytes = std::fs::read(&fixture.plan_path).unwrap();
    for sentinel in [
        "PRIVATE-SUMMARY-SENTINEL",
        "PRIVATE-BODY-SENTINEL",
        "PRIVATE-METADATA-SENTINEL",
    ] {
        assert!(
            !bytes
                .windows(sentinel.len())
                .any(|window| window == sentinel.as_bytes()),
            "plan leaked {sentinel}"
        );
    }
}

#[test]
fn candidate_report_is_read_only_body_free_and_not_an_apply_plan() {
    let fixture = fixture();
    let watched = [
        fixture.db_path.clone(),
        PathBuf::from(format!("{}-wal", fixture.db_path.display())),
        PathBuf::from(format!("{}-shm", fixture.db_path.display())),
        fixture.app_home.join("manifest.json"),
        fixture.app_home.join("daemon.lock"),
        fixture.app_home.join("daemon.pid"),
    ];
    let before = exact_file_bytes(&watched);
    let report_path = fixture.plan_path.with_file_name("candidates.json");
    run_gc(
        GcAction::Candidates {
            db: fixture.db_path.clone(),
            prefix: "/".to_string(),
            out: report_path.clone(),
        },
        &fixture.app_home,
    )
    .unwrap();
    assert_eq!(
        exact_file_bytes(&watched),
        before,
        "discovery must not mutate DB, sidecars or manifest"
    );
    let bytes = std::fs::read(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    recursively_assert_body_free(&report);
    assert_eq!(report["operation"], "gc_candidates");
    assert_eq!(report["apply_authority"], false);
    assert_eq!(report["candidates"][0]["id"], "delete-me");
    assert_eq!(report["candidates"][0]["disposition"], "manual_review");
    assert_eq!(
        report["candidates"][0]["row_hash"].as_str().unwrap().len(),
        64
    );
    for sentinel in [
        "PRIVATE-SUMMARY-SENTINEL",
        "PRIVATE-BODY-SENTINEL",
        "PRIVATE-METADATA-SENTINEL",
    ] {
        assert!(!String::from_utf8_lossy(&bytes).contains(sentinel));
    }
    run_gc(
        GcAction::Apply {
            plan: report_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect_err("candidate discovery cannot be applied");
    assert_eq!(exact_file_bytes(&watched), before);
    assert!(!receipt_path(&report_path).exists());
}

#[test]
fn unsafe_old_gc_policy_is_rejected_before_mutation() {
    let fixture = fixture();
    run_gc(
        GcAction::Plan {
            db: fixture.db_path.clone(),
            out: fixture.plan_path.clone(),
        },
        &fixture.app_home,
    )
    .unwrap();
    let mut plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
    assert_eq!(plan.policy_version, GC_POLICY_VERSION);
    plan.policy_version = POLICY_VERSION.to_string();
    let plan = plan.seal().unwrap();
    let bytes = serde_json::to_vec(&plan).unwrap();
    assert!(OperatorMaintenancePlanBinding::from_canonical_json(&bytes).is_err());
    std::fs::write(&fixture.plan_path, bytes).unwrap();
    let before = std::fs::read(&fixture.db_path).unwrap();
    let error = run_gc(
        GcAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported maintenance plan version/policy"),
        "{error}"
    );
    assert_eq!(std::fs::read(&fixture.db_path).unwrap(), before);
    assert!(!receipt_path(&fixture.plan_path).exists());
}

#[test]
fn maintenance_requires_manifest_inventory_membership_before_plan_or_apply() {
    let fixture = fixture();
    let external_db = fixture._dir.path().join("external-memory.db");
    let mut external_store = MemoryStore::open(&external_db.to_string_lossy()).unwrap();
    external_store
        .insert_if_absent(&fixture_entry("delete-me", "/notes/external"))
        .unwrap();
    drop(external_store);

    let external_delete_plan = fixture._dir.path().join("external-delete-plan.json");
    let external_gc_plan = fixture._dir.path().join("external-gc-plan.json");
    let external_before = std::fs::read(&external_db).unwrap();

    for error in [
        run_delete(
            DeleteAction::Plan {
                db: external_db.clone(),
                id: "delete-me".to_string(),
                out: external_delete_plan.clone(),
            },
            &fixture.app_home,
        )
        .expect_err("external delete target must not enter plan authority"),
        run_gc(
            GcAction::Plan {
                db: external_db.clone(),
                out: external_gc_plan.clone(),
            },
            &fixture.app_home,
        )
        .expect_err("external GC target must not enter plan authority"),
    ] {
        assert!(
            error
                .to_string()
                .contains("did not resolve to exactly one manifest DB"),
            "external target reached synthetic inventory fallback: {error}"
        );
    }
    assert!(!external_delete_plan.exists());
    assert!(!external_gc_plan.exists());
    assert!(!receipt_path(&external_delete_plan).exists());
    assert_eq!(std::fs::read(&external_db).unwrap(), external_before);

    // Freeze a valid plan while the external DB is manifest-backed, then
    // remove that authority. Apply must re-resolve the manifest and refuse
    // before publishing a receipt or opening the DB read-write.
    let manifest_path = fixture.app_home.join("manifest.json");
    let original_manifest = std::fs::read(&manifest_path).unwrap();
    let mut manifest = Manifest::load(&manifest_path).unwrap();
    manifest.dbs.push(DbEntry {
        path: std::fs::canonicalize(&external_db)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        role: DbRole::Project,
        owner: "tachi".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: String::new(),
        last_classification: "healthy".to_string(),
        scope_hint: "project:external".to_string(),
        notes: String::new(),
        aliases: Vec::new(),
    });
    manifest.save(&manifest_path).unwrap();
    run_delete(
        DeleteAction::Plan {
            db: external_db.clone(),
            id: "delete-me".to_string(),
            out: external_delete_plan.clone(),
        },
        &fixture.app_home,
    )
    .unwrap();
    std::fs::write(&manifest_path, original_manifest).unwrap();

    let external_before_apply = std::fs::read(&external_db).unwrap();
    let error = run_delete(
        DeleteAction::Apply {
            plan: external_delete_plan.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect_err("apply must revalidate manifest inventory membership");
    assert!(
        error
            .to_string()
            .contains("did not resolve to exactly one manifest DB"),
        "external apply reached synthetic inventory fallback: {error}"
    );
    assert!(!receipt_path(&external_delete_plan).exists());
    assert_eq!(std::fs::read(&external_db).unwrap(), external_before_apply);
    assert!(memory_exists(&external_db));

    // The exact same plan/apply path remains available for the peer that
    // is still backed by the manifest.
    plan_delete(&fixture);
    run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .unwrap();
    assert!(!memory_exists(&fixture.db_path));
    assert!(receipt_path(&fixture.plan_path).exists());
}

#[cfg(unix)]
#[test]
fn confirmation_and_ownership_refuse_before_receipt_or_db_mutation() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    for ownership in [
        None,
        Some(DbOwnership::Owned),
        Some(DbOwnership::Unknown("probe-fault".to_string())),
    ] {
        let fixture = fixture();
        plan_delete(&fixture);
        let before = std::fs::read(&fixture.db_path).unwrap();
        let yes = ownership.is_some();
        if let Some(ownership) = ownership {
            set_ownership_inject_for_test(Some(ownership));
        }
        let error = run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes,
            },
            &fixture.app_home,
        )
        .expect_err("precondition must refuse");
        assert!(
            error.to_string().contains("requires --yes")
                || error.to_string().contains("owned by a live daemon")
                || error.to_string().contains("ownership unknown"),
            "{error}"
        );
        assert_eq!(std::fs::read(&fixture.db_path).unwrap(), before);
        assert!(!receipt_path(&fixture.plan_path).exists());
        assert!(memory_exists(&fixture.db_path));
    }
}

#[cfg(unix)]
#[test]
fn postcommit_cache_and_finalize_failures_replay_without_a_second_delete_transaction() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    for fault_kind in ["cache", "finalize"] {
        let fixture = fixture();
        plan_delete(&fixture);
        DB_APPLY_CALLS.with(|count| count.set(0));
        match fault_kind {
            "cache" => FAIL_BEFORE_CACHE.with(|fault| fault.set(true)),
            "finalize" => FAIL_BEFORE_FINALIZE.with(|fault| fault.set(true)),
            _ => unreachable!(),
        }
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        let error = run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect_err("injected postcommit failure");
        assert!(error.to_string().contains("DB committed"), "{error}");
        assert!(error.to_string().contains("reconciliation"), "{error}");
        assert!(!memory_exists(&fixture.db_path));
        let receipt_out = receipt_path(&fixture.plan_path);
        let prepared: MaintenanceReceipt =
            serde_json::from_slice(&std::fs::read(&receipt_out).unwrap()).unwrap();
        assert_eq!(prepared.phase, ReceiptPhase::Prepared);
        assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);

        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect("replay finalizes committed state");
        assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
        let committed_bytes = std::fs::read(&receipt_out).unwrap();
        let committed: MaintenanceReceipt = serde_json::from_slice(&committed_bytes).unwrap();
        assert_eq!(committed.phase, ReceiptPhase::Committed);

        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect("committed replay");
        assert_eq!(std::fs::read(&receipt_out).unwrap(), committed_bytes);
        assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
    }
}

#[cfg(unix)]
#[test]
fn committed_replay_refuses_a_replaced_target_before_returning_receipt() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    let fixture = fixture();
    plan_delete(&fixture);
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .unwrap();
    let committed = std::fs::read(receipt_path(&fixture.plan_path)).unwrap();

    let displaced = fixture.db_path.with_extension("original");
    std::fs::rename(&fixture.db_path, &displaced).unwrap();
    std::fs::copy(&displaced, &fixture.db_path).unwrap();
    let error = run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect_err("committed replay must not authorize a replacement inode");
    assert!(
        error.to_string().contains("physical DB identity mismatch"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(receipt_path(&fixture.plan_path)).unwrap(),
        committed
    );
}

#[cfg(unix)]
#[test]
fn committed_replay_refuses_changed_post_state_without_a_second_transaction() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    let fixture = fixture();
    plan_delete(&fixture);
    DB_APPLY_CALLS.with(|count| count.set(0));
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .unwrap();
    let receipt_out = receipt_path(&fixture.plan_path);
    let committed = std::fs::read(&receipt_out).unwrap();
    let mut store =
        MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
    store
        .insert_if_absent(&fixture_entry("delete-me", "/notes/recreated"))
        .unwrap();
    drop(store);

    let error = run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect_err("committed replay must bind the recorded post-state");
    assert!(error.to_string().contains("post-state changed"), "{error}");
    assert_eq!(std::fs::read(&receipt_out).unwrap(), committed);
    assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
}

#[cfg(unix)]
#[test]
fn resealed_gc_plan_cannot_invent_a_new_retention_policy() {
    let fixture = fixture();
    let gc_plan = fixture.plan_path.with_file_name("gc-policy-plan.json");
    run_gc(
        GcAction::Plan {
            db: fixture.db_path.clone(),
            out: gc_plan.clone(),
        },
        &fixture.app_home,
    )
    .unwrap();
    let mut plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&gc_plan).unwrap()).unwrap();
    let MaintenancePolicy::Gc {
        access_history_keep_per_memory,
        ..
    } = &mut plan.policy
    else {
        panic!("GC plan must carry GC policy")
    };
    *access_history_keep_per_memory = 0;
    plan = plan.seal().unwrap();
    std::fs::write(&gc_plan, pretty_bytes(&plan).unwrap()).unwrap();

    let error = run_gc(
        GcAction::Apply {
            plan: gc_plan.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect_err("a locally re-sealed plan must not create a new GC policy");
    assert!(error.to_string().contains("canonical GC policy"), "{error}");
    assert!(memory_exists(&fixture.db_path));
    assert!(!receipt_path(&gc_plan).exists());
}

#[cfg(unix)]
#[test]
fn wrong_digest_and_profile_refuse_without_receipt_or_mutation() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    let digest = fixture();
    plan_delete(&digest);
    let mut plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&digest.plan_path).unwrap()).unwrap();
    plan.digest = "0".repeat(64);
    std::fs::write(&digest.plan_path, pretty_bytes(&plan).unwrap()).unwrap();
    let error = run_delete(
        DeleteAction::Apply {
            plan: digest.plan_path.clone(),
            yes: true,
        },
        &digest.app_home,
    )
    .expect_err("wrong plan digest must refuse");
    assert!(error.to_string().contains("digest mismatch"), "{error}");
    assert!(memory_exists(&digest.db_path));
    assert!(!receipt_path(&digest.plan_path).exists());

    let profile = fixture();
    plan_delete(&profile);
    let mut plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&profile.plan_path).unwrap()).unwrap();
    plan.profile = "portable_kernel".to_string();
    plan = plan.seal().unwrap();
    std::fs::write(&profile.plan_path, pretty_bytes(&plan).unwrap()).unwrap();
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    let error = run_delete(
        DeleteAction::Apply {
            plan: profile.plan_path.clone(),
            yes: true,
        },
        &profile.app_home,
    )
    .expect_err("wrong live profile must refuse");
    assert!(
        error.to_string().contains("plan profile mismatch"),
        "{error}"
    );
    assert!(memory_exists(&profile.db_path));
    assert!(!receipt_path(&profile.plan_path).exists());
}

#[cfg(unix)]
#[test]
fn missing_delete_id_applies_as_an_honest_committed_noop() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    let fixture = fixture();
    let missing_plan = fixture.plan_path.with_file_name("missing-delete-plan.json");
    run_delete(
        DeleteAction::Plan {
            db: fixture.db_path.clone(),
            id: "missing-id".to_string(),
            out: missing_plan.clone(),
        },
        &fixture.app_home,
    )
    .unwrap();
    let plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&missing_plan).unwrap()).unwrap();
    assert!(plan.source.iter().all(|fact| fact.count == 0));

    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    run_delete(
        DeleteAction::Apply {
            plan: missing_plan.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .unwrap();
    assert!(memory_exists(&fixture.db_path));
    let receipt: MaintenanceReceipt =
        serde_json::from_slice(&std::fs::read(receipt_path(&missing_plan)).unwrap()).unwrap();
    assert_eq!(receipt.phase, ReceiptPhase::Committed);
    assert!(receipt.source.iter().all(|fact| fact.count == 0));
    assert!(receipt.post.iter().all(|fact| fact.count == 0));
}

#[cfg(unix)]
#[test]
fn source_drift_and_precommit_failure_never_commit_unplanned_delete() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};
    let drift = fixture();
    plan_delete(&drift);
    let mut store =
        MemoryStore::open_existing_read_write(&drift.db_path.to_string_lossy()).unwrap();
    let current = store.get("delete-me").unwrap().unwrap();
    store
        .update_with_revision(
            "delete-me",
            &current.text,
            "changed after plan",
            &current.source,
            &current.metadata,
            None,
            current.revision,
        )
        .unwrap();
    drop(store);
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    let error = run_delete(
        DeleteAction::Apply {
            plan: drift.plan_path.clone(),
            yes: true,
        },
        &drift.app_home,
    )
    .expect_err("source drift must refuse");
    assert!(
        error.to_string().contains("source facts drifted"),
        "{error}"
    );
    assert!(memory_exists(&drift.db_path));
    assert!(!receipt_path(&drift.plan_path).exists());

    let precommit = fixture();
    plan_delete(&precommit);
    FAIL_AFTER_PREPARED_PUBLISH.with(|fault| fault.set(true));
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    let error = run_delete(
        DeleteAction::Apply {
            plan: precommit.plan_path.clone(),
            yes: true,
        },
        &precommit.app_home,
    )
    .expect_err("injected precommit failure must roll back");
    assert!(error.to_string().contains("rolled back"), "{error}");
    assert!(memory_exists(&precommit.db_path));
    let public = receipt_path(&precommit.plan_path);
    let receipt: MaintenanceReceipt =
        serde_json::from_slice(&std::fs::read(&public).unwrap()).unwrap();
    assert_eq!(receipt.phase, ReceiptPhase::Prepared);

    let publish = fixture();
    plan_delete(&publish);
    FAIL_BEFORE_PREPARED_PUBLISH.with(|fault| fault.set(true));
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    let error = run_delete(
        DeleteAction::Apply {
            plan: publish.plan_path.clone(),
            yes: true,
        },
        &publish.app_home,
    )
    .expect_err("prepared publication failure must roll back");
    assert!(error.to_string().contains("rolled back"), "{error}");
    assert!(memory_exists(&publish.db_path));
    assert!(!receipt_path(&publish.plan_path).exists());

    let recovery_collision = fixture();
    plan_delete(&recovery_collision);
    let collision_plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&recovery_collision.plan_path).unwrap()).unwrap();
    let collision_public = receipt_path(&recovery_collision.plan_path);
    let collision_recovery = legacy_recovery_receipt_path(&collision_public, &collision_plan);
    std::fs::write(&collision_recovery, b"FOREIGN-LEGACY-RECOVERY").unwrap();
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    run_delete(
        DeleteAction::Apply {
            plan: recovery_collision.plan_path.clone(),
            yes: true,
        },
        &recovery_collision.app_home,
    )
    .expect("legacy recovery collision is outside the closed authority namespace");
    assert!(!memory_exists(&recovery_collision.db_path));
    assert_eq!(
        std::fs::read(&collision_recovery).unwrap(),
        b"FOREIGN-LEGACY-RECOVERY"
    );
}

#[cfg(unix)]
#[test]
fn prepared_path_replacement_before_commit_rolls_back_fresh_and_replay() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    for replay in [false, true] {
        let fixture = fixture();
        plan_delete(&fixture);
        let receipt_out = receipt_path(&fixture.plan_path);

        if replay {
            FAIL_AFTER_PREPARED_PUBLISH.with(|fault| fault.set(true));
            set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
            let error = run_delete(
                DeleteAction::Apply {
                    plan: fixture.plan_path.clone(),
                    yes: true,
                },
                &fixture.app_home,
            )
            .expect_err("fixture must retain a prepared replay receipt");
            assert!(error.to_string().contains("rolled back"), "{error}");
            assert!(receipt_out.exists());
            assert!(memory_exists(&fixture.db_path));
        }

        REPLACE_PREPARED_BEFORE_COMMIT.with(|fault| fault.set(true));
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        let error = run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect_err("prepared pathname replacement must refuse before commit");

        assert!(error.to_string().contains("rolled back"), "{error}");
        assert!(
            error.to_string().contains("prepared receipt pathname"),
            "{error}"
        );
        assert!(memory_exists(&fixture.db_path));
        assert_eq!(
            std::fs::read(&receipt_out).unwrap(),
            b"FOREIGN-REPLACEMENT-EVIDENCE"
        );
        let retained: MaintenanceReceipt = serde_json::from_slice(
            &std::fs::read(replaced_prepared_evidence_path(&receipt_out)).unwrap(),
        )
        .unwrap();
        assert_eq!(retained.phase, ReceiptPhase::Prepared);
    }
}

#[cfg(unix)]
#[test]
fn postcheck_public_replacement_commits_once_then_replays_from_db_authority() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    let fixture = fixture();
    plan_delete(&fixture);
    let plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
    let receipt_out = receipt_path(&fixture.plan_path);
    let target_scope = format!(
        "{:x}",
        Sha256::digest(plan.target_physical_identity.as_bytes())
    );
    let recovery = receipt_out.parent().unwrap().join(format!(
        ".tachi-maintenance-recovery-{}-{target_scope}",
        plan.digest
    ));

    DB_APPLY_CALLS.with(|count| count.set(0));
    REPLACE_PUBLIC_AFTER_PRECOMMIT_CHECK.with(|fault| fault.set(true));
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    let error = run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect_err("postcheck replacement must return reconciliation, not success");

    assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
    assert!(!memory_exists(&fixture.db_path));
    assert_eq!(
        std::fs::read(&receipt_out).unwrap(),
        b"FOREIGN-AFTER-CHECK-EVIDENCE"
    );
    assert!(!recovery.exists());
    let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
    let authority_json: String = conn
        .query_row(
            "SELECT value_json FROM hard_state
             WHERE namespace='operator_maintenance_receipt' AND key=?1",
            [&plan.digest],
            |row| row.get(0),
        )
        .expect("same-transaction committed authority must survive both path losses");
    assert!(error.to_string().contains("DB committed"), "{error}");
    assert!(error.to_string().contains("reconciliation"), "{error}");
    let authority: serde_json::Value = serde_json::from_str(&authority_json).unwrap();
    assert_eq!(authority["plan_digest"], plan.digest);
    assert!(
        !authority_json.contains("PRIVATE-BODY-SENTINEL")
            && !authority_json.contains("PRIVATE-SUMMARY-SENTINEL")
            && !authority_json.contains("PRIVATE-METADATA-SENTINEL")
    );
    let expected_committed_digest = authority["committed_receipt_digest"]
        .as_str()
        .expect("typed committed receipt digest")
        .to_string();
    drop(conn);

    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    let replay_error = run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect_err("replay must retain and report the untrusted foreign projection");
    assert!(
        replay_error.to_string().contains("reconciliation required"),
        "{replay_error}"
    );
    assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
    let committed_bytes = std::fs::read(&receipt_out).unwrap();
    let committed: MaintenanceReceipt = serde_json::from_slice(&committed_bytes).unwrap();
    assert_eq!(committed.phase, ReceiptPhase::Committed);
    assert_eq!(committed.digest, expected_committed_digest);
    assert!(fixture._dir.path().read_dir().unwrap().any(|entry| {
        std::fs::read(entry.unwrap().path())
            .is_ok_and(|bytes| bytes == b"FOREIGN-AFTER-CHECK-EVIDENCE")
    }));

    run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect("committed replay must be byte-identical");
    assert_eq!(std::fs::read(&receipt_out).unwrap(), committed_bytes);
    assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
}

#[cfg(unix)]
#[test]
fn recovery_cleanup_never_deletes_a_foreign_replacement() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    let fixture = fixture();
    plan_delete(&fixture);
    let plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
    let receipt_out = receipt_path(&fixture.plan_path);
    let recovery = legacy_recovery_receipt_path(&receipt_out, &plan);
    std::fs::write(&recovery, b"FOREIGN-RECOVERY-CLEANUP-EVIDENCE").unwrap();
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect("foreign legacy recovery path must not participate in apply");
    assert!(!memory_exists(&fixture.db_path));
    assert_eq!(
        std::fs::read(&recovery).unwrap(),
        b"FOREIGN-RECOVERY-CLEANUP-EVIDENCE"
    );
    run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect("committed replay must ignore and retain the foreign legacy recovery path");
    assert_eq!(
        std::fs::read(&recovery).unwrap(),
        b"FOREIGN-RECOVERY-CLEANUP-EVIDENCE"
    );
    assert!(!memory_exists(&fixture.db_path));
}

#[test]
fn committed_authority_namespace_rejects_general_state_mutators() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    let fixture = fixture();
    plan_delete(&fixture);
    let plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .unwrap();

    let store = MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
    let namespace = "operator_maintenance_receipt";
    assert!(store.set_state(namespace, &plan.digest, "{}").is_err());
    assert!(store
        .set_state_if_version(namespace, &plan.digest, "{}", 1)
        .is_err());
    assert!(store.delete_state(namespace, &plan.digest).is_err());
    assert!(store
        .insert_state_if_absent(namespace, "foreign-key", "{}")
        .is_err());
    assert!(store
        .operator_maintenance_committed_authority(&plan.digest)
        .unwrap()
        .is_some());
}

#[test]
fn public_operator_apply_cannot_forge_arbitrary_authority_json() {
    let fixture = fixture();
    plan_delete(&fixture);
    let plan_bytes = std::fs::read(&fixture.plan_path).unwrap();
    let plan: MaintenancePlan = serde_json::from_slice(&plan_bytes).unwrap();
    let operator_plan = OperatorMaintenancePlanBinding::from_canonical_json(&plan_bytes).unwrap();
    let mut store =
        MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
    let error = store
        .apply_operator_delete_with_precommit_receipt(&operator_plan, |source, post| {
            let receipt = MaintenanceReceipt {
                version: RECEIPT_VERSION,
                plan_digest: plan.digest.clone(),
                operation: plan.operation,
                target_path: plan.target_path.clone(),
                target_physical_identity: plan.target_physical_identity.clone(),
                profile: plan.profile.clone(),
                phase: ReceiptPhase::Committed,
                apply_timestamp: "2026-08-13T00:00:00Z".to_string(),
                source: source.to_vec(),
                post: post.to_vec(),
                cache_invalidated: true,
                reconciliation: "complete".to_string(),
                digest: String::new(),
            }
            .seal()
            .unwrap();
            let mut value = serde_json::to_value(receipt).unwrap();
            value["body"] = serde_json::json!("forged maintenance body");
            OperatorMaintenanceCommittedReceiptBinding::from_canonical_json(
                &operator_plan,
                &serde_json::to_vec(&value).unwrap(),
            )
        })
        .expect_err("unknown/body receipt authority must roll back");
    assert!(error.to_string().contains("unknown field"), "{error}");
    assert!(memory_exists(&fixture.db_path));
    assert!(store
        .get_state_kv("operator_maintenance_receipt", &plan.digest)
        .unwrap()
        .is_none());
}

#[test]
fn same_operation_cannot_forge_independent_authority_bindings() {
    let mut violations = Vec::new();
    for forged_field in [
        "plan_digest",
        "physical_identity",
        "profile",
        "receipt_digest",
    ] {
        let fixture = fixture();
        plan_delete(&fixture);
        let mut plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
        match forged_field {
            "plan_digest" => plan.digest = "a".repeat(64),
            "physical_identity" => {
                plan.target_physical_identity = "unix:9:9".to_string();
                plan = plan.seal().unwrap();
            }
            "profile" => {
                plan.profile = "portable_kernel".to_string();
                plan = plan.seal().unwrap();
            }
            "receipt_digest" => {}
            _ => unreachable!(),
        }
        let plan_bytes = serde_json::to_vec(&plan).unwrap();
        let mut store =
            MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
        let result = OperatorMaintenancePlanBinding::from_canonical_json(&plan_bytes).and_then(
            |operator_plan| {
                store.apply_operator_delete_with_precommit_receipt(
                    &operator_plan,
                    |source, post| {
                        let mut receipt = MaintenanceReceipt {
                            version: RECEIPT_VERSION,
                            plan_digest: plan.digest.clone(),
                            operation: plan.operation,
                            target_path: plan.target_path.clone(),
                            target_physical_identity: plan.target_physical_identity.clone(),
                            profile: plan.profile.clone(),
                            phase: ReceiptPhase::Committed,
                            apply_timestamp: "2026-08-13T00:00:00Z".to_string(),
                            source: source.to_vec(),
                            post: post.to_vec(),
                            cache_invalidated: true,
                            reconciliation: "complete".to_string(),
                            digest: String::new(),
                        }
                        .seal()
                        .unwrap();
                        if forged_field == "receipt_digest" {
                            receipt.digest = "c".repeat(64);
                        }
                        OperatorMaintenanceCommittedReceiptBinding::from_canonical_json(
                            &operator_plan,
                            &serde_json::to_vec(&receipt).unwrap(),
                        )
                    },
                )
            },
        );
        let row_exists = store
            .get_state_kv("operator_maintenance_receipt", &plan.digest)
            .unwrap()
            .is_some();
        if result.is_ok() || !memory_exists(&fixture.db_path) || row_exists {
            violations.push(forged_field);
        }
    }
    assert!(
        violations.is_empty(),
        "same-operation forgeries committed instead of rolling back: {violations:?}"
    );
}

#[test]
fn ttl_backfill_and_reaper_never_mutate_committed_authority() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    let fixture = fixture();
    plan_delete(&fixture);
    let plan: MaintenancePlan =
        serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .unwrap();
    let committed_bytes = std::fs::read(receipt_path(&fixture.plan_path)).unwrap();
    let store = MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
    let before = store
        .get_state_kv("operator_maintenance_receipt", &plan.digest)
        .unwrap()
        .expect("same-transaction authority");
    assert_eq!(
        store
            .backfill_missing_expires_at(
                "operator_maintenance_receipt",
                "2020-01-01T00:00:00Z",
                None,
            )
            .unwrap(),
        0
    );
    assert_eq!(store.reap_expired_state("2026-08-13T00:00:00Z").unwrap(), 0);
    assert_eq!(
        store
            .get_state_kv("operator_maintenance_receipt", &plan.digest)
            .unwrap()
            .unwrap(),
        before,
        "authority bytes and version must remain unchanged"
    );
    drop(store);
    run_delete(
        DeleteAction::Apply {
            plan: fixture.plan_path.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .expect("authority-backed replay remains valid after TTL maintenance");
    assert_eq!(
        std::fs::read(receipt_path(&fixture.plan_path)).unwrap(),
        committed_bytes
    );
}

#[cfg(unix)]
#[test]
fn gc_plan_and_apply_execute_the_complete_frozen_registry() {
    use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

    let fixture = fixture();
    let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
    conn.execute(
        "INSERT INTO processed_events(event_hash,event_id,worker,created_at)
         VALUES ('old-gc','event','worker','2020-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);
    let gc_plan = fixture.plan_path.with_file_name("gc-plan.json");
    run_gc(
        GcAction::Plan {
            db: fixture.db_path.clone(),
            out: gc_plan.clone(),
        },
        &fixture.app_home,
    )
    .unwrap();
    let plan: MaintenancePlan = serde_json::from_slice(&std::fs::read(&gc_plan).unwrap()).unwrap();
    plan.validate().unwrap();
    assert_eq!(plan.operation, MaintenanceOperation::Gc);
    assert_eq!(plan.source.len(), OPERATOR_GC_CLASSES.len());
    assert_eq!(
        plan.source
            .iter()
            .find(|fact| fact.class == "processed_events_age")
            .map(|fact| fact.count),
        Some(1)
    );

    set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
    run_gc(
        GcAction::Apply {
            plan: gc_plan.clone(),
            yes: true,
        },
        &fixture.app_home,
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
    assert_eq!(
        conn.query_row::<i64, _, _>(
            "SELECT COUNT(*) FROM processed_events WHERE event_hash='old-gc'",
            [],
            |row| row.get(0),
        )
        .unwrap(),
        0
    );
    let receipt: MaintenanceReceipt =
        serde_json::from_slice(&std::fs::read(receipt_path(&gc_plan)).unwrap()).unwrap();
    assert_eq!(receipt.phase, ReceiptPhase::Committed);
}

#[test]
fn plan_and_receipt_serialization_is_recursively_body_free() {
    let empty = |class: &&str| MaintenanceClassFact {
        class: (*class).to_string(),
        count: 0,
        digest: format!("{:x}", Sha256::digest([])),
    };
    let plan = MaintenancePlan {
        version: PLAN_VERSION,
        operation: MaintenanceOperation::Delete,
        target_path: "/tmp/tachi-memory.db".to_string(),
        target_physical_identity: "unix:1:2".to_string(),
        profile: "tachi_full".to_string(),
        as_of: "2026-08-13T00:00:00Z".to_string(),
        policy_version: POLICY_VERSION.to_string(),
        policy: MaintenancePolicy::Delete {
            exact_id_canonical_delete: true,
        },
        delete_id: Some("safe-id".to_string()),
        source: OPERATOR_DELETE_CLASSES.iter().map(empty).collect(),
        digest: String::new(),
    }
    .seal()
    .unwrap();
    let receipt = MaintenanceReceipt {
        version: RECEIPT_VERSION,
        plan_digest: plan.digest.clone(),
        operation: plan.operation,
        target_path: plan.target_path.clone(),
        target_physical_identity: plan.target_physical_identity.clone(),
        profile: plan.profile.clone(),
        phase: ReceiptPhase::Prepared,
        apply_timestamp: plan.as_of.clone(),
        source: plan.source.clone(),
        post: plan.source.clone(),
        cache_invalidated: false,
        reconciliation: "required".to_string(),
        digest: String::new(),
    }
    .seal()
    .unwrap();
    for value in [
        serde_json::to_value(plan).unwrap(),
        serde_json::to_value(receipt).unwrap(),
    ] {
        recursively_assert_body_free(&value);
    }
}

fn recursively_assert_body_free(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                assert!(
                    !matches!(
                        key.as_str(),
                        "text" | "summary" | "body" | "vector" | "embedding" | "metadata"
                    ),
                    "private content key leaked: {key}"
                );
                recursively_assert_body_free(value);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                recursively_assert_body_free(value);
            }
        }
        _ => {}
    }
}
