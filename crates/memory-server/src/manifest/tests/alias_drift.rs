//! #736 goldens: Plan C alias identity drift after the #692 casefold-hash
//! change. G1 (drift reconcile) and G2 (single identity) below.
//!
//! G3 (counting basis) lives in `bootstrap::backfill::tests` next to the
//! code it pins. G4 (legacy alias resolution stays intact) is not a new
//! test — it is the pre-existing `plan_c_dir_name_sanitizes_spaces` /
//! `plan_c_dir_name_case_distinct_on_case_sensitive_fs` family in
//! `path_utils/tests.rs`, left unmodified and re-run in the same gate.

use super::*;

fn with_env_lock<F: FnOnce()>(f: F) {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    f();
}

fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
    if let Some(v) = value {
        std::env::set_var(name, v);
    } else {
        std::env::remove_var(name);
    }
}

/// G1 (drift reconcile): a repo-local DB has an OLD-hash alias dir (symlink)
/// and a manifest label under that old name — the live #736 shape. Running
/// the reconciliation surface (`populate_from_doctor`, which `tachi doctor`
/// and `tachi manifest refresh` both call) must: adopt a single alias under
/// the NEW derived name resolving to the same canonical file, update the
/// manifest label, retire the old-name orphan, and leave the canonical data
/// file byte-identical. A backup file living in the old alias dir must never
/// be deleted.
#[test]
fn g1_alias_drift_reconciles_old_hash_alias_to_new_derived_name() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tmp");
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        let tachi_home = tmp.path().join("home");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");

        // Repo-local canonical DB — the real data, never moved by this fix.
        let repo = tmp.path().join("Drift_Repo");
        let local_db = repo.join(".tachi/memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical-bytes").expect("write local db");
        let local_db = std::fs::canonicalize(&local_db).expect("canonicalize local db");

        // Simulate a pre-existing alias dir under an OLD (pre-derivation-
        // change) hash name, symlinked to the repo-local DB — exactly the
        // live-machine shape in #736's Symptom section.
        let old_name = "Drift_Repo-oldhash00";
        let old_dir = tachi_home.join("projects").join(old_name);
        std::fs::create_dir_all(&old_dir).expect("old alias dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&local_db, old_dir.join("memory.db"))
            .expect("old alias symlink");
        // A migration-backup sibling in the OLD alias dir — must survive.
        let backup_name = "memory.db.migration-bak.20260706T135027";
        std::fs::write(old_dir.join(backup_name), b"backup-bytes").expect("write backup");

        let new_name =
            crate::path_utils::plan_c_dir_name_from_root(&repo).expect("current derived name");
        assert_ne!(new_name, old_name, "test fixture must simulate real drift");
        let new_dir = tachi_home.join("projects").join(&new_name);
        assert!(
            !new_dir.exists(),
            "pre-fix state: the new-hash alias must not exist yet"
        );

        // Manifest before reconciliation: single entry, scanned via the OLD
        // alias path and labeled under the OLD name (as a doctor run 3 days
        // before a derivation change would have recorded it).
        let mut m = Manifest::empty();
        let report = mk_report(vec![mk_finding(
            &old_dir.join("memory.db").to_string_lossy(),
            DbClassification::Healthy,
            &format!("project:{old_name}"),
        )]);

        // --- run the reconciliation surface ---
        m.populate_from_doctor(&report);

        assert_eq!(m.dbs.len(), 1, "single canonical identity, not two rows");
        assert_eq!(
            m.dbs[0].scope_hint,
            format!("project:{new_name}"),
            "manifest label must be the CURRENT derived name"
        );
        assert_eq!(m.dbs[0].path, local_db.to_string_lossy());

        // New-name alias now exists and resolves to the same canonical file.
        assert!(
            new_dir.join("memory.db").is_symlink(),
            "new-derived-name alias must be created"
        );
        assert_eq!(
            std::fs::canonicalize(new_dir.join("memory.db")).expect("canon new alias"),
            local_db
        );

        // Old-name alias's symlink is retired (directory holds only a
        // backup file now, so it is not removed outright — but it no longer
        // resolves as a live alias).
        assert!(
            std::fs::symlink_metadata(old_dir.join("memory.db")).is_err(),
            "old-name alias symlink must be retired"
        );
        assert!(
            old_dir.join(backup_name).exists(),
            "backup files are NEVER deleted by this fix"
        );

        // Canonical data file is untouched, byte-identical.
        assert_eq!(
            std::fs::read(&local_db).expect("read canonical"),
            b"canonical-bytes"
        );

        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    });
}

/// G2 (single identity): BOTH an old-name and the new-derived-name alias
/// already exist on disk (both symlinking the same canonical file) before
/// reconciliation runs. The status/coverage scan (driven off `manifest.dbs`)
/// must report the DB exactly once, labeled with the derived (new) name —
/// never the old one, regardless of which alias the scan happens to reach
/// first.
#[test]
fn g2_single_identity_when_both_old_and_new_alias_exist() {
    with_env_lock(|| {
        let tmp = tempfile::tempdir().expect("tmp");
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        let tachi_home = tmp.path().join("home");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");

        let repo = tmp.path().join("Both_Alias_Repo");
        let local_db = repo.join(".tachi/memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical-bytes").expect("write local db");
        let local_db = std::fs::canonicalize(&local_db).expect("canonicalize local db");

        let new_name =
            crate::path_utils::plan_c_dir_name_from_root(&repo).expect("current derived name");
        let new_dir = tachi_home.join("projects").join(&new_name);
        std::fs::create_dir_all(&new_dir).expect("new alias dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&local_db, new_dir.join("memory.db"))
            .expect("new alias symlink");

        let old_name = "Both_Alias_Repo-oldhash00";
        let old_dir = tachi_home.join("projects").join(old_name);
        std::fs::create_dir_all(&old_dir).expect("old alias dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&local_db, old_dir.join("memory.db"))
            .expect("old alias symlink");

        // Report the OLD-name alias FIRST — pre-fix, by_canon's
        // "first-finding-wins" dedup would have kept the OLD name's
        // scope_hint as the surviving (only) manifest entry's label.
        let report = mk_report(vec![
            mk_finding(
                &old_dir.join("memory.db").to_string_lossy(),
                DbClassification::Healthy,
                &format!("project:{old_name}"),
            ),
            mk_finding(
                &new_dir.join("memory.db").to_string_lossy(),
                DbClassification::Healthy,
                &format!("project:{new_name}"),
            ),
        ]);

        let mut m = Manifest::empty();
        m.populate_from_doctor(&report);

        assert_eq!(
            m.dbs.len(),
            1,
            "the same canonical file must be reported exactly once"
        );
        assert_eq!(
            m.dbs[0].scope_hint,
            format!("project:{new_name}"),
            "label must be the derived NEW name even though the OLD-name finding was scanned first"
        );

        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    });
}
