use super::*;

#[test]
fn gc_manifest_full_flow() {
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let manifest_path = dir.path().join("manifest.json");

    // Build five fake DBs on disk, plus one missing entry, plus one fixture.
    // (a) good Tachi DB
    let good = dir.path().join("good.db");
    let conn = rusqlite::Connection::open(&good).unwrap();
    conn.execute_batch(
            "CREATE TABLE memories (id TEXT PRIMARY KEY, path TEXT, summary TEXT, text TEXT, importance REAL, timestamp TEXT NOT NULL);",
        )
        .unwrap();
    drop(conn);

    // (b) symlink alias to (a) — should be deduped
    let alias = dir.path().join("alias.db");
    symlink(&good, &alias).unwrap();

    // (c) fixture
    let fixtures_dir = dir.path().join("node_modules/vite/tmp");
    std::fs::create_dir_all(&fixtures_dir).unwrap();
    let fixture = fixtures_dir.join("feature-daemon-global.db");
    std::fs::write(&fixture, b"sqlite-stub").unwrap();

    // (d) misclassified entry: chunks DB tagged tachi
    let chunks = dir.path().join("legacy.db");
    let conn = rusqlite::Connection::open(&chunks).unwrap();
    conn.execute_batch("CREATE TABLE chunks (id TEXT PRIMARY KEY);")
        .unwrap();
    drop(conn);

    // (e) entry with a path that no longer exists
    let missing = dir.path().join("ghost.db");
    // do not create

    // Build a manifest by hand to exercise the GC pass directly.
    let now = "2026-04-30T00:00:00+00:00".to_string();
    let mk = |p: &std::path::Path, schema: &str| DbEntry {
        path: p.to_string_lossy().to_string(),
        role: DbRole::Unknown,
        owner: "tachi".to_string(),
        schema_kind: schema.to_string(),
        vec_enabled: false,
        allow_write: true,
        last_doctor_at: now.clone(),
        last_classification: "healthy".to_string(),
        scope_hint: "test".to_string(),
        notes: String::new(),
    };
    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generated_at: now.clone(),
        comment: String::new(),
        dbs: vec![
            mk(&good, "tachi"),
            mk(&alias, "tachi"),   // dup-of-good
            mk(&fixture, "tachi"), // fixture → drop
            mk(&chunks, "tachi"),  // mis-tagged → fix
            mk(&missing, "tachi"), // missing → drop
        ],
    };
    manifest.save(&manifest_path).unwrap();

    // Sanity guard: this would remove >50% (3 of 5). Confirm the guard
    // trips and the manifest is preserved untouched.
    let report = gc_manifest(&manifest_path).unwrap();
    assert!(
        report.aborted,
        "5-entry manifest with 3 removals must trip sanity guard"
    );
    let reloaded = Manifest::load(&manifest_path).unwrap();
    assert_eq!(reloaded.dbs.len(), 5, "aborted GC must not mutate manifest");

    // Add four more good entries so removals (3) stay below 50%.
    for i in 0..4 {
        let extra = dir.path().join(format!("extra{i}.db"));
        let conn = rusqlite::Connection::open(&extra).unwrap();
        conn.execute_batch(
                "CREATE TABLE memories (id TEXT PRIMARY KEY, path TEXT, summary TEXT, text TEXT, importance REAL, timestamp TEXT NOT NULL);",
            )
            .unwrap();
        drop(conn);
        let mut m2 = Manifest::load(&manifest_path).unwrap();
        m2.dbs.push(mk(&extra, "tachi"));
        m2.save(&manifest_path).unwrap();
    }

    let report = gc_manifest(&manifest_path).unwrap();
    assert!(!report.aborted, "non-aborted: {:?}", report.abort_reason);
    assert_eq!(
        report.removed_fixture, 1,
        "the vite fixture must be dropped"
    );
    assert_eq!(
        report.removed_missing, 1,
        "the missing entry must be dropped"
    );
    assert_eq!(
        report.dedup_collapsed, 1,
        "alias must collapse onto good.db"
    );
    assert_eq!(report.schema_kind_fixed, 1, "chunks DB must be re-tagged");

    // Backup file written.
    let bak = {
        let mut s = manifest_path.as_os_str().to_os_string();
        s.push(".bak");
        std::path::PathBuf::from(s)
    };
    assert!(bak.exists(), "manifest.json.bak must exist after GC");

    // Idempotency: a second run should report all zeros.
    let report2 = gc_manifest(&manifest_path).unwrap();
    assert_eq!(report2.canonicalized, 0);
    assert_eq!(report2.removed_missing, 0);
    assert_eq!(report2.removed_fixture, 0);
    assert_eq!(report2.schema_kind_fixed, 0);
    assert_eq!(report2.dedup_collapsed, 0);
    assert!(!report2.aborted);

    // Final manifest contents: good, chunks (re-tagged), and the four extras.
    let final_m = Manifest::load(&manifest_path).unwrap();
    assert_eq!(final_m.dbs.len(), 6);
    let good_canon = std::fs::canonicalize(&good)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let chunks_canon = std::fs::canonicalize(&chunks)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let chunks_entry = final_m.dbs.iter().find(|e| e.path == chunks_canon).unwrap();
    assert_eq!(chunks_entry.schema_kind, "openclaw_legacy");
    assert!(final_m.dbs.iter().any(|e| e.path == good_canon));
}
