use super::*;

/// #1132 compat contract's "manifest path update" half: the ratified rule is
/// "daemon performs a one-time rename-on-open (file rename + manifest path
/// update)". The rename-on-open seam itself (`memcore::db::filename`) only
/// touches the filesystem — it does not reach into the manifest. This test
/// proves that's fine: `gc_manifest`'s existing canonicalize-on-scan pass
/// (`canonicalize_db_path` follows symlinks) is *already* the manifest-side
/// half of the contract, with zero code of its own. Once the seam has
/// renamed `memory.db` -> `tachi-memory.db` and left the compat symlink
/// behind, a manifest entry still recorded at the old literal path
/// canonicalizes straight through the symlink to the new file and gets
/// rewritten in place on the next `gc_manifest` pass (run unconditionally at
/// `tachi serve` startup — see `bootstrap/serve.rs::resolve_global_db`).
#[cfg(unix)]
#[test]
fn gc_manifest_follows_the_1132_compat_symlink_to_the_renamed_path() {
    let dir = tempdir().unwrap();
    let db_dir = dir.path().join("global");
    std::fs::create_dir_all(&db_dir).unwrap();

    // Simulate the on-disk state right after the #1132 rename-on-open seam
    // has fired for this directory: real data now lives under the new
    // canonical name, and a memory.db -> tachi-memory.db compat symlink sits
    // where the manifest still thinks the file is.
    let canonical_db = db_dir.join(memcore::MEMORY_DB_FILENAME);
    let conn = rusqlite::Connection::open(&canonical_db).unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (id TEXT PRIMARY KEY, path TEXT, summary TEXT, text TEXT, importance REAL, timestamp TEXT NOT NULL);",
    )
    .unwrap();
    drop(conn);
    let legacy_db = db_dir.join(memcore::LEGACY_MEMORY_DB_FILENAME);
    std::os::unix::fs::symlink(memcore::MEMORY_DB_FILENAME, &legacy_db).unwrap();

    // A pre-#1132 manifest entry, still recorded at the legacy path — exactly
    // what an install upgraded across the rename would have on disk before
    // its next `gc_manifest` pass.
    let manifest_path = dir.path().join("manifest.json");
    let now = "2026-07-17T00:00:00Z".to_string();
    let entry = DbEntry {
        path: legacy_db.to_string_lossy().to_string(),
        role: DbRole::Global,
        owner: "tachi".to_string(),
        schema_kind: "unknown".to_string(),
        vec_enabled: false,
        allow_write: true,
        last_doctor_at: now.clone(),
        last_classification: "healthy".to_string(),
        scope_hint: "global".to_string(),
        notes: String::new(),
    };
    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generated_at: now,
        comment: String::new(),
        dbs: vec![entry],
    };
    manifest.save(&manifest_path).unwrap();

    let report = gc_manifest(&manifest_path).expect("gc_manifest must succeed");
    assert!(
        !report.aborted,
        "GC must not abort on a single healthy entry"
    );
    assert_eq!(
        report.canonicalized, 1,
        "the legacy-path entry must be rewritten to its canonical path"
    );
    assert_eq!(
        report.removed_missing, 0,
        "the entry resolves via the compat symlink, not missing"
    );

    let reloaded = Manifest::load(&manifest_path).expect("reload manifest");
    assert_eq!(
        reloaded.dbs.len(),
        1,
        "the single entry must survive GC, just with an updated path"
    );
    let expected = std::fs::canonicalize(&canonical_db).unwrap();
    assert_eq!(
        std::path::Path::new(&reloaded.dbs[0].path),
        expected.as_path(),
        "manifest entry must now point at the renamed canonical file, not the legacy symlink path"
    );
}
