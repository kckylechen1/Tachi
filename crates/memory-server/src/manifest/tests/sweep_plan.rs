use super::*;

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
