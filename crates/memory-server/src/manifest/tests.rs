use super::*;
use crate::doctor::{DbClassification, DoctorFinding, DoctorReport, JobBreakdown, SummaryByClass};
use std::path::Path;
use tempfile::tempdir;

fn mk_finding(path: &str, class: DbClassification, scope: &str) -> DoctorFinding {
    DoctorFinding {
        path: path.to_string(),
        classification: class,
        file_size: 4096,
        has_wal: false,
        mem_count: Some(1),
        vec_rowid_count: Some(1),
        none_domain_count: Some(0),
        jobs: JobBreakdown::default(),
        schema_kind: "tachi".to_string(),
        error: None,
        scope_hint: scope.to_string(),
    }
}

fn mk_report(findings: Vec<DoctorFinding>) -> DoctorReport {
    DoctorReport {
        scanned_roots: vec![],
        findings,
        summary: SummaryByClass::default(),
        warnings: vec![],
        auto_fix_actions: vec![],
        quarantine_dir: Some("/tmp/q".to_string()),
        generated_at: "2026-04-28T00:00:00+00:00".to_string(),
    }
}

#[test]
fn populate_filters_and_classifies_roles() {
    let mut m = Manifest::empty();
    let report = mk_report(vec![
        mk_finding(
            "/u/.tachi/global/memory.db",
            DbClassification::Healthy,
            "global",
        ),
        mk_finding(
            "/u/.tachi/projects/quant/memory.db",
            DbClassification::Healthy,
            "project:quant",
        ),
        mk_finding(
            "/u/.openclaw/extensions/tachi/data/agents/main/memory.db",
            DbClassification::Healthy,
            "openclaw-agent:main",
        ),
        mk_finding(
            "/u/.tachi/junk.db",
            DbClassification::Placeholder,
            "tachi-other",
        ),
        mk_finding(
            "/u/.tachi/foo.db.bak",
            DbClassification::Backup,
            "tachi-other",
        ),
        mk_finding(
            "/u/.tachi/dead.db",
            DbClassification::Corrupt,
            "tachi-other",
        ),
    ]);
    m.populate_from_doctor(&report);
    assert_eq!(m.dbs.len(), 3, "only healthy/wal/legacy should be recorded");
    assert_eq!(
        m.global().map(|e| e.path.as_str()),
        Some("/u/.tachi/global/memory.db")
    );
    assert_eq!(m.by_role(DbRole::Project).len(), 1);
    assert_eq!(m.by_role(DbRole::Agent).len(), 1);
    let agent = m.by_role(DbRole::Agent)[0];
    assert_eq!(agent.owner, "openclaw-agent:main");
}

#[test]
fn save_and_load_roundtrip_preserves_notes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("manifest.json");
    let mut m = Manifest::empty();
    m.populate_from_doctor(&mk_report(vec![mk_finding(
        "/u/.tachi/global/memory.db",
        DbClassification::Healthy,
        "global",
    )]));
    m.dbs[0].notes = "primary global store".to_string();
    m.save(&path).unwrap();

    let loaded = Manifest::load(&path).unwrap();
    assert_eq!(loaded.dbs.len(), 1);
    assert_eq!(loaded.dbs[0].notes, "primary global store");

    // Re-populating should preserve the note.
    let mut m2 = loaded.clone();
    m2.populate_from_doctor(&mk_report(vec![mk_finding(
        "/u/.tachi/global/memory.db",
        DbClassification::Healthy,
        "global",
    )]));
    assert_eq!(m2.dbs[0].notes, "primary global store");
}

#[test]
fn resolve_agent_db_path_prefers_extension_agent_db() {
    let mut m = Manifest::empty();
    m.dbs = vec![
        DbEntry {
            path: "/u/.openclaw/agents/main/memory/memory.db".to_string(),
            role: DbRole::Agent,
            owner: "openclaw-agent-local:main".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".to_string(),
            scope_hint: "openclaw-agent-local:main".to_string(),
            notes: String::new(),
        },
        DbEntry {
            path: "/u/.openclaw/extensions/tachi/data/agents/main/memory.db".to_string(),
            role: DbRole::Agent,
            owner: "openclaw-agent:main".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".to_string(),
            scope_hint: "openclaw-agent:main".to_string(),
            notes: String::new(),
        },
    ];
    assert_eq!(
        m.resolve_agent_db_path("main").as_deref(),
        Some(Path::new(
            "/u/.openclaw/extensions/tachi/data/agents/main/memory.db"
        ))
    );
}

#[test]
fn allow_write_for_healthy_and_wal_orphan() {
    // PR-A: WalOrphan is now write-allowed because a non-empty -wal file
    // is the expected state for any DB held open by the live daemon, and
    // SQLite recovers WAL automatically on next open. LegacySchema still
    // blocks (real schema mismatch).
    let mut m = Manifest::empty();
    m.populate_from_doctor(&mk_report(vec![
        mk_finding("/u/a.db", DbClassification::Healthy, "project:a"),
        mk_finding("/u/b.db", DbClassification::WalOrphan, "project:b"),
        mk_finding("/u/c.db", DbClassification::LegacySchema, "project:c"),
    ]));
    let by_path: std::collections::HashMap<_, _> = m
        .dbs
        .iter()
        .map(|e| (e.path.clone(), e.allow_write))
        .collect();
    assert!(by_path["/u/a.db"]);
    assert!(by_path["/u/b.db"], "WalOrphan must be writable");
    assert!(!by_path["/u/c.db"]);
}

#[test]
fn lookup_returns_entry() {
    let mut m = Manifest::empty();
    m.populate_from_doctor(&mk_report(vec![mk_finding(
        "/u/.tachi/global/memory.db",
        DbClassification::Healthy,
        "global",
    )]));
    assert!(m.lookup("/u/.tachi/global/memory.db").is_some());
    assert!(m.lookup("/u/missing.db").is_none());
}

#[test]
fn check_writable_enforces_allow_write() {
    let mut m = Manifest::empty();
    m.populate_from_doctor(&mk_report(vec![
        mk_finding("/u/healthy.db", DbClassification::Healthy, "project:a"),
        mk_finding("/u/orphan.db", DbClassification::WalOrphan, "project:b"),
        mk_finding("/u/legacy.db", DbClassification::LegacySchema, "project:c"),
    ]));
    // PR-A: WalOrphan now writable (live daemon holds non-empty WAL).
    assert!(m.check_writable("/u/healthy.db").is_ok());
    assert!(
        m.check_writable("/u/orphan.db").is_ok(),
        "WalOrphan must be writable — SQLite recovers WAL on open"
    );
    // LegacySchema still blocks writes (real schema mismatch).
    match m.check_writable("/u/legacy.db") {
        Err(ManifestGuardError::WriteForbidden { .. }) => {}
        other => panic!("expected WriteForbidden for legacy, got {other:?}"),
    }
    match m.check_writable("/u/never-seen.db") {
        Err(ManifestGuardError::NotInManifest { .. }) => {}
        other => panic!("expected NotInManifest, got {other:?}"),
    }
}

#[test]
fn plan_sweep_skips_owned_and_targets_only_placeholders_and_backups() {
    let mut m = Manifest::empty();
    m.populate_from_doctor(&mk_report(vec![mk_finding(
        "/u/.tachi/global/memory.db",
        DbClassification::Healthy,
        "global",
    )]));
    // doctor sees: 1 owned (must be skipped), 1 placeholder, 1 backup, 1 corrupt (ignored)
    let report = mk_report(vec![
        mk_finding(
            "/u/.tachi/global/memory.db",
            DbClassification::Healthy,
            "global",
        ),
        mk_finding(
            "/u/.tachi/junk.db",
            DbClassification::Placeholder,
            "tachi-other",
        ),
        mk_finding(
            "/u/.tachi/foo.db.bak",
            DbClassification::Backup,
            "tachi-other",
        ),
        mk_finding(
            "/u/.tachi/dead.db",
            DbClassification::Corrupt,
            "tachi-other",
        ),
    ]);
    let qdir = std::path::Path::new("/tmp/q");
    let plan = plan_sweep(&report, &m, qdir);
    assert_eq!(
        plan.planned.len(),
        2,
        "placeholder + backup should be planned"
    );
    let paths: Vec<_> = plan.planned.iter().map(|a| a.path.as_str()).collect();
    assert!(paths.contains(&"/u/.tachi/junk.db"));
    assert!(paths.contains(&"/u/.tachi/foo.db.bak"));
    assert_eq!(plan.skipped.len(), 1, "owned global.db should be skipped");
    assert_eq!(plan.skipped[0].path, "/u/.tachi/global/memory.db");
}

#[test]
fn plan_sweep_refuses_files_outside_tachi_roots() {
    // Manifest with one owned DB in /home/user/.tachi/global/
    let mut m = Manifest::empty();
    m.populate_from_doctor(&mk_report(vec![mk_finding(
        "/home/user/.tachi/global/memory.db",
        DbClassification::Healthy,
        "global",
    )]));
    // Doctor sees a placeholder INSIDE a Tachi root → should plan,
    // and another placeholder OUTSIDE Tachi roots → should skip.
    let report = mk_report(vec![
        mk_finding(
            "/home/user/.tachi/junk.db",
            DbClassification::Placeholder,
            "tachi-other",
        ),
        mk_finding(
            "/home/user/Desktop/Project/data/cache.db",
            DbClassification::Placeholder,
            "tachi-other",
        ),
    ]);
    let plan = plan_sweep(&report, &m, std::path::Path::new("/tmp/q"));
    assert_eq!(
        plan.planned.len(),
        1,
        "only the in-Tachi-root file should be planned"
    );
    assert_eq!(plan.planned[0].path, "/home/user/.tachi/junk.db");
    let outside_skip = plan
        .skipped
        .iter()
        .find(|a| a.path == "/home/user/Desktop/Project/data/cache.db");
    assert!(
        outside_skip.is_some(),
        "outside-roots file must be skipped, not planned"
    );
    assert!(outside_skip
        .unwrap()
        .note
        .contains("outside Tachi-owned roots"));
}

#[test]
fn plan_sweep_assigns_unique_quarantine_names_for_collisions() {
    let m = Manifest::empty();
    let report = mk_report(vec![
        mk_finding(
            "/home/user/.tachi/a/dup.db",
            DbClassification::Placeholder,
            "tachi-other",
        ),
        mk_finding(
            "/home/user/.tachi/b/dup.db",
            DbClassification::Placeholder,
            "tachi-other",
        ),
        mk_finding(
            "/home/user/.tachi/c/dup.db",
            DbClassification::Placeholder,
            "tachi-other",
        ),
    ]);
    let dir = tempdir().unwrap();
    let plan = plan_sweep(&report, &m, dir.path());
    assert_eq!(plan.planned.len(), 3);
    let names: std::collections::HashSet<_> = plan
        .planned
        .iter()
        .filter_map(|a| a.quarantine_to.clone())
        .collect();
    assert_eq!(names.len(), 3, "quarantine targets must be unique");
}

#[test]
fn apply_sweep_moves_files_to_quarantine() {
    let dir = tempdir().unwrap();
    let qdir = dir.path().join("quarantine");
    // Path must look like it's inside a Tachi root so the safety gate allows it.
    let tachi_dir = dir.path().join(".tachi");
    std::fs::create_dir_all(&tachi_dir).unwrap();
    let bad = tachi_dir.join("placeholder.db");
    std::fs::write(&bad, b"").unwrap();

    let m = Manifest::empty(); // empty manifest → bad is unowned, but inside .tachi
    let report = mk_report(vec![mk_finding(
        bad.to_string_lossy().as_ref(),
        DbClassification::Placeholder,
        "tachi-other",
    )]);
    let plan = plan_sweep(&report, &m, &qdir);
    assert_eq!(
        plan.planned.len(),
        1,
        "placeholder under .tachi should be planned"
    );
    let result = apply_sweep(plan, &qdir);
    assert_eq!(result.applied.len(), 1, "placeholder should be moved");
    assert!(!bad.exists(), "original placeholder gone");
}

// ─── PR-2: hygiene / GC tests ──────────────────────────────────────────

#[cfg(unix)]
#[test]
fn canonicalize_db_path_collapses_symlink() {
    let dir = tempdir().unwrap();
    let real = dir.path().join("real.db");
    std::fs::write(&real, b"x").unwrap();
    let link = dir.path().join("alias.db");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let real_canon = canonicalize_db_path(&real);
    let link_canon = canonicalize_db_path(&link);
    assert_eq!(
        real_canon, link_canon,
        "symlink alias must canonicalize to target"
    );
}

#[test]
fn canonicalize_db_path_falls_back_for_missing_files() {
    let p = Path::new("/definitely/does/not/exist/here.db");
    // Should not panic, returns the input path unchanged.
    assert_eq!(canonicalize_db_path(p), p.to_path_buf());
}

#[test]
fn gc_manifest_refuses_to_mutate_when_backup_write_fails() {
    let dir = tempdir().unwrap();
    let manifest_path = dir.path().join("manifest.json");
    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generated_at: "2026-06-14T00:00:00Z".to_string(),
        comment: String::new(),
        dbs: vec![],
    };
    manifest.save(&manifest_path).unwrap();
    let before = std::fs::read(&manifest_path).unwrap();

    let backup_path = {
        let mut s = manifest_path.as_os_str().to_os_string();
        s.push(".bak");
        std::path::PathBuf::from(s)
    };
    std::fs::create_dir(&backup_path).unwrap();

    let err = gc_manifest(&manifest_path).expect_err("backup failure should abort GC");
    assert!(
        err.to_string().contains("manifest backup write"),
        "unexpected error: {err}"
    );
    assert_eq!(
        std::fs::read(&manifest_path).unwrap(),
        before,
        "GC must not mutate manifest when backup cannot be written"
    );
}

#[test]
fn should_skip_path_matches_fixture_patterns() {
    assert!(should_skip_path(Path::new(
        "/Users/me/.tachi/.agent/claude-code-runs/run/backups/global_memory.db"
    ))
    .is_some());
    assert!(should_skip_path(Path::new(
        "/Users/me/.tachi/tidy/manual-cleanup-20260503101336/global__memory.db"
    ))
    .is_some());
    assert!(should_skip_path(Path::new(
        "/Users/me/.tachi/cleanup-backups/20260529T144750/main_memory.db"
    ))
    .is_some());
    // node_modules + tmp
    assert!(should_skip_path(Path::new(
        "/repo/node_modules/vitejs/vite/tmp/feature-daemon-global.db"
    ))
    .is_some());
    // node_modules + .test.db
    assert!(should_skip_path(Path::new("/repo/node_modules/foo/tests/bar.db")).is_some());
    // *.test.db anywhere
    assert!(should_skip_path(Path::new("/some/path/foo.test.db")).is_some());
    // *.fixture.db anywhere
    assert!(should_skip_path(Path::new("/some/path/foo.fixture.db")).is_some());
    // vite zread belt-and-suspenders
    assert!(should_skip_path(Path::new("/x/zread/.cache/feature-daemon-global.db")).is_some());
    // temporary Tachi workspaces should not become daemon-maintained DBs.
    assert!(should_skip_path(Path::new(
        "/private/tmp/quant-hypermemory-ux/.tachi/memory.db"
    ))
    .is_some());
    assert!(should_skip_path(Path::new(
        "/private/tmp/tachi-recall-smoke.abc123/project/memory.db"
    ))
    .is_some());
    // Real-looking Tachi DB must NOT be skipped.
    assert!(should_skip_path(Path::new("/Users/me/.tachi/global/memory.db")).is_none());
    assert!(should_skip_path(Path::new("/repo/.tachi/memory.db")).is_none());
    assert!(should_skip_path(Path::new("/Users/me/.openclaw/agents/x/memory/memory.db")).is_none());
    assert!(should_skip_path(Path::new(
        "/Users/me/.openclaw/extensions/tachi/data/agents/weixin-backup/memory.db"
    ))
    .is_none());
}

#[test]
fn classify_db_schema_distinguishes_kinds() {
    let dir = tempdir().unwrap();

    // Tachi-shaped DB.
    let tachi_path = dir.path().join("tachi.db");
    let conn = rusqlite::Connection::open(&tachi_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (
                id TEXT PRIMARY KEY, path TEXT NOT NULL DEFAULT '/',
                summary TEXT, text TEXT, importance REAL, timestamp TEXT NOT NULL,
                category TEXT, topic TEXT
            );",
    )
    .unwrap();
    drop(conn);
    assert_eq!(classify_db_schema(&tachi_path), SchemaKind::Tachi);

    // OpenClaw chunks-shaped DB.
    let chunks_path = dir.path().join("chunks.db");
    let conn = rusqlite::Connection::open(&chunks_path).unwrap();
    conn.execute_batch("CREATE TABLE chunks (id TEXT PRIMARY KEY);")
        .unwrap();
    drop(conn);
    assert_eq!(classify_db_schema(&chunks_path), SchemaKind::OpenclawChunks);

    // Empty SQLite file → Unknown.
    let empty_path = dir.path().join("empty.db");
    let conn = rusqlite::Connection::open(&empty_path).unwrap();
    drop(conn);
    assert_eq!(classify_db_schema(&empty_path), SchemaKind::Unknown);

    // Missing file → Unknown.
    let missing = dir.path().join("nope.db");
    assert_eq!(classify_db_schema(&missing), SchemaKind::Unknown);
}

#[test]
fn schema_kind_serde_roundtrip() {
    let json = serde_json::to_string(&SchemaKind::Tachi).unwrap();
    assert_eq!(json, "\"tachi\"");
    // Wire form: openclaw_legacy (preserves backward-compat with the
    // existing manifest schema_kind string).
    let json = serde_json::to_string(&SchemaKind::OpenclawChunks).unwrap();
    assert_eq!(json, "\"openclaw_legacy\"");
    let back: SchemaKind = serde_json::from_str("\"openclaw_legacy\"").unwrap();
    assert_eq!(back, SchemaKind::OpenclawChunks);
}

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
