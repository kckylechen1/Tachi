use super::*;

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
fn manifest_populates_global_store_with_global_role_and_owner() {
    let mut m = Manifest::empty();
    let report = mk_report(vec![
        mk_finding(
            "/u/.tachi/global/tachi-memory.db",
            DbClassification::Healthy,
            "global",
        ),
        mk_finding("/u/.tachi/memory.db", DbClassification::Healthy, "global"),
    ]);
    m.populate_from_doctor(&report);
    assert_eq!(m.dbs.len(), 2);
    for entry in &m.dbs {
        assert_eq!(
            entry.role,
            DbRole::Global,
            "must be classified as DbRole::Global"
        );
        assert_eq!(entry.owner, "tachi", "global store owner must be tachi");
        assert_eq!(entry.scope_hint, "global");
        assert!(entry.allow_write);
    }
    assert!(m.global().is_some());
}
