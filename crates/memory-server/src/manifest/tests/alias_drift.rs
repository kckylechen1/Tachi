//! #736 goldens: Plan C alias identity drift after the #692 casefold-hash
//! change. The user-visible symptom (`tachi status` labels a DB with the
//! stale old-hash name while briefing binds the current derived name) is
//! fixed ENTIRELY by an in-memory `scope_hint` label recompute in
//! `populate_from_doctor`. This PR deliberately does NOT mutate the
//! filesystem — physical alias-dir retirement is split to #743.
//!
//! G-relabel below pins that decision: the label flips to the new name
//! WHILE both on-disk alias dirs are left untouched. G-role pins FIX-D
//! (scope and role must agree). G3 (counting basis) lives in
//! `bootstrap::backfill::tests`. G4 (legacy alias resolution stays intact)
//! is the pre-existing `path_utils/tests.rs` suite, left unmodified.

use super::*;

fn alias_drift_tempdir() -> tempfile::TempDir {
    crate::test_support::non_skipped_fixture_tempdir("alias-drift-")
}

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

/// G-relabel: the real live-machine 3-alias shape — a legacy un-hashed
/// `sigil`-style alias dir AND an old-hash `Sigil-94c144a9`-style alias dir
/// (both symlinking the same repo-local DB), and NO new-derived-name dir.
/// After `populate_from_doctor` (what `tachi status`/`doctor`/`manifest
/// refresh` all drive), the manifest label for that DB must be the CURRENT
/// derived name, AND — critically — the filesystem must be untouched: both
/// existing alias dirs still present, and no new-name dir created. This pins
/// the scope-reduction decision (label recompute only, zero filesystem
/// mutation) and would FAIL on the pre-rework head, which retired the dirs.
#[test]
fn g_relabel_flips_label_without_touching_filesystem() {
    with_env_lock(|| {
        let tmp = alias_drift_tempdir();
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        let tachi_home = tmp.path().join("home");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");

        // Repo-local canonical DB — the real data, never moved by this fix.
        let repo = tmp.path().join("Sigil");
        let local_db = repo.join(".tachi/memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical-bytes").expect("write local db");
        let local_db = std::fs::canonicalize(&local_db).expect("canonicalize local db");
        crate::test_support::assert_repo_local_db_fixture_not_skipped(&local_db);

        // Legacy un-hashed alias dir (`sigil`), symlinked to the repo-local DB.
        let legacy_name =
            crate::path_utils::plan_c_legacy_dir_name_from_root(&repo).expect("legacy name");
        let legacy_dir = tachi_home.join("projects").join(&legacy_name);
        std::fs::create_dir_all(&legacy_dir).expect("legacy alias dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&local_db, legacy_dir.join("memory.db"))
            .expect("legacy alias symlink");

        // Old-hash alias dir (pre-#692 derivation), symlinked to the same DB.
        let old_hash_name = "Sigil-94c144a9";
        let old_hash_dir = tachi_home.join("projects").join(old_hash_name);
        std::fs::create_dir_all(&old_hash_dir).expect("old-hash alias dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&local_db, old_hash_dir.join("memory.db"))
            .expect("old-hash alias symlink");

        // The CURRENT derived name has no alias dir on disk (exactly the #736
        // symptom: briefing binds `Sigil-<newhash>`, nothing exists there).
        let new_name =
            crate::path_utils::plan_c_dir_name_from_root(&repo).expect("current derived name");
        assert_ne!(new_name, legacy_name);
        assert_ne!(new_name, old_hash_name);
        let new_dir = tachi_home.join("projects").join(&new_name);
        assert!(!new_dir.exists(), "precondition: no new-name dir yet");

        // Manifest before: single entry, scanned via the OLD-hash alias path
        // and labeled under the OLD-hash name.
        let mut m = Manifest::empty();
        let report = mk_report(vec![mk_finding(
            &old_hash_dir.join("memory.db").to_string_lossy(),
            DbClassification::Healthy,
            &format!("project:{old_hash_name}"),
        )]);

        // --- run the surface that status/doctor/manifest-refresh all use ---
        m.populate_from_doctor(&report);

        // Label flips to the CURRENT derived name.
        assert_eq!(m.dbs.len(), 1);
        assert_eq!(
            m.dbs[0].scope_hint,
            format!("project:{new_name}"),
            "manifest label must be the CURRENT derived name"
        );
        assert_eq!(m.dbs[0].path, local_db.to_string_lossy());

        // CRITICAL: the filesystem is UNTOUCHED. Both existing alias dirs and
        // their symlinks survive; no new-name dir is created.
        assert!(
            legacy_dir.join("memory.db").is_symlink(),
            "legacy alias must survive (requirement 4)"
        );
        assert!(
            old_hash_dir.join("memory.db").is_symlink(),
            "old-hash alias must survive — no filesystem retirement in this PR"
        );
        assert!(
            !new_dir.exists(),
            "a plain status/refresh must NOT create the new-name alias dir (that is #743, --fix-gated)"
        );

        // Canonical data file untouched, byte-identical.
        assert_eq!(
            std::fs::read(&local_db).expect("read canonical"),
            b"canonical-bytes"
        );

        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    });
}

/// G-role (FIX-D): a repo-local DB whose stored scope_hint is NOT
/// project-shaped must still end up with BOTH the corrected `project:<new>`
/// scope AND a role consistent with it (`DbRole::Project`) — never a
/// `project:*`-scope-with-`Unknown`-role mismatch. We feed a finding whose
/// stored hint is `tachi-other` — what `scope_hint_for` returns for a
/// repo-local `.tachi/memory.db` reached via its own scan root rather than a
/// `projects/<name>/` alias. Pre-FIX-D, `role` was classified from that
/// stale hint and came out `Unknown` while the recomputed scope became
/// `project:<new>`; with FIX-D role is recomputed from the corrected scope
/// and agrees.
#[test]
fn g_role_scope_and_role_agree_after_relabel() {
    with_env_lock(|| {
        let tmp = alias_drift_tempdir();
        let saved_home = std::env::var_os("TACHI_HOME");
        let saved_sigil = std::env::var_os("SIGIL_HOME");
        let saved_app = std::env::var_os("TACHI_APP_HOME");
        let tachi_home = tmp.path().join("home");
        std::env::set_var("TACHI_HOME", &tachi_home);
        std::env::remove_var("SIGIL_HOME");
        std::env::remove_var("TACHI_APP_HOME");

        let repo = tmp.path().join("Role_Repo");
        let local_db = repo.join(".tachi/memory.db");
        std::fs::create_dir_all(local_db.parent().unwrap()).expect("local parent");
        std::fs::write(&local_db, b"canonical-bytes").expect("write local db");
        let local_db = std::fs::canonicalize(&local_db).expect("canonicalize local db");
        crate::test_support::assert_repo_local_db_fixture_not_skipped(&local_db);

        let new_name =
            crate::path_utils::plan_c_dir_name_from_root(&repo).expect("current derived name");

        // Repo-local DB reached via its own scan root: `scope_hint_for`
        // returns "tachi-other" (NOT project-shaped) for such a path.
        let mut m = Manifest::empty();
        let report = mk_report(vec![mk_finding(
            &local_db.to_string_lossy(),
            DbClassification::Healthy,
            "tachi-other",
        )]);

        m.populate_from_doctor(&report);

        assert_eq!(m.dbs.len(), 1);
        assert_eq!(
            m.dbs[0].scope_hint,
            format!("project:{new_name}"),
            "scope must be recomputed to the current project name"
        );
        assert_eq!(
            m.dbs[0].role,
            DbRole::Project,
            "role must agree with the project scope (no project-scope-with-Unknown-role)"
        );
        assert_eq!(
            m.dbs[0].owner, "tachi",
            "owner must be recomputed from the corrected scope"
        );

        restore_env("TACHI_HOME", saved_home);
        restore_env("SIGIL_HOME", saved_sigil);
        restore_env("TACHI_APP_HOME", saved_app);
    });
}
