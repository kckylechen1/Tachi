use super::*;
use std::path::{Path, PathBuf};

fn input(name: &str) -> ProjectDbInput {
    ProjectDbInput {
        project_name: name.to_string(),
        db_path: PathBuf::from(format!("/u/.tachi/projects/{name}/memory.db")),
        is_symlink: false,
        symlink_target: None,
        symlink_target_exists: false,
        owning_repo: None,
        sibling_conflict: false,
    }
}

#[test]
fn symlink_with_live_target_is_alias() {
    let mut i = input("hyperion");
    i.is_symlink = true;
    i.symlink_target = Some(PathBuf::from("/repos/hyperion/.tachi/memory.db"));
    i.symlink_target_exists = true;
    let item = classify_project_db(&i);
    assert_eq!(item.class, ProjectDbClass::SymlinkAlias);
    assert_eq!(item.action, PlannedAction::KeepAlias);
    assert!(item.relocate_to.is_none());
}

#[test]
fn symlink_with_missing_target_is_broken_gc() {
    let mut i = input("stale-proj");
    i.is_symlink = true;
    i.symlink_target = Some(PathBuf::from("/gone/.tachi/memory.db"));
    i.symlink_target_exists = false;
    let item = classify_project_db(&i);
    assert_eq!(item.class, ProjectDbClass::SymlinkBroken);
    assert_eq!(item.action, PlannedAction::GarbageCollect);
}

#[test]
fn real_file_with_owning_repo_is_relocatable() {
    let mut i = input("quant");
    i.owning_repo = Some(PathBuf::from("/Users/me/repos/quant"));
    let item = classify_project_db(&i);
    assert_eq!(item.class, ProjectDbClass::RealFileWithOwningRepo);
    assert_eq!(item.action, PlannedAction::RelocateToRepo);
    assert_eq!(
        item.relocate_to.as_deref(),
        // #1132: relocation destination uses the canonical filename.
        Some("/Users/me/repos/quant/.tachi/tachi-memory.db")
    );
}

#[test]
fn real_file_without_repo_is_home_resident() {
    let i = input("Quant_Analyzer_2026_audit_notes");
    let item = classify_project_db(&i);
    assert_eq!(item.class, ProjectDbClass::RealFileHomeResident);
    assert_eq!(item.action, PlannedAction::KeepHomeResident);
    assert!(item.relocate_to.is_none());
}

#[test]
fn uuid_name_is_garbage_even_as_real_file_with_repo() {
    // A UUID-named dir that *happens* to have an owning repo and real data
    // must still be classified garbage (precedence rule 1).
    let mut i = input("8f14e45f-ceea-467a-9c8a-1b2c3d4e5f60");
    i.owning_repo = Some(PathBuf::from("/repos/whatever"));
    let item = classify_project_db(&i);
    assert_eq!(item.class, ProjectDbClass::UuidSmokeTestGarbage);
    assert_eq!(item.action, PlannedAction::GarbageCollect);
}

#[test]
fn uuid_detection_matches_expected_shapes() {
    // Canonical dashed UUID.
    assert!(is_uuid_smoke_test_name(
        "8f14e45f-ceea-467a-9c8a-1b2c3d4e5f60"
    ));
    // UUID embedded with a prefix.
    assert!(is_uuid_smoke_test_name(
        "run-8f14e45f-ceea-467a-9c8a-1b2c3d4e5f60"
    ));
    // 32-char hex blob (no dashes).
    assert!(is_uuid_smoke_test_name("9b74c9897bac770ffc029102a200c5de"));
    // smoke marker + hex tail.
    assert!(is_uuid_smoke_test_name("tachi-recall-smoke.ab12cd34"));
    // Real named projects must NOT match.
    assert!(!is_uuid_smoke_test_name("quant"));
    assert!(!is_uuid_smoke_test_name("hyperion-research"));
    assert!(!is_uuid_smoke_test_name("Quant_Analyzer_2026_audit_main"));
    // "facade" contains hex chars a,c,e but only 6-long → not a blob.
    assert!(!is_uuid_smoke_test_name("facade"));
    // A short hex-ish word like "decade" (6) must not trip the 32-run rule.
    assert!(!is_uuid_smoke_test_name("decade"));
}

#[test]
fn resolve_owning_repo_prefers_manifest_repo_local_path() {
    let manifest = vec![
        // Central path for this project — must be IGNORED as an owner.
        (
            "project:quant".to_string(),
            "/u/.tachi/projects/quant/memory.db".to_string(),
        ),
        // Repo-local path — this is the real owner.
        (
            "project:quant".to_string(),
            "/Users/me/repos/quant/.tachi/memory.db".to_string(),
        ),
    ];
    let repo = resolve_owning_repo("quant", &manifest, &[]);
    assert_eq!(repo, Some(PathBuf::from("/Users/me/repos/quant")));
}

#[test]
fn resolve_owning_repo_falls_back_to_git_root_basename() {
    let roots = vec![
        PathBuf::from("/Users/me/repos/other"),
        PathBuf::from("/Users/me/repos/Hyperion"),
    ];
    // Case-insensitive basename match.
    let repo = resolve_owning_repo("hyperion", &[], &roots);
    assert_eq!(repo, Some(PathBuf::from("/Users/me/repos/Hyperion")));
    // No match → None (becomes home-resident).
    assert_eq!(resolve_owning_repo("nope", &[], &roots), None);
}

#[test]
fn resolve_owning_repo_ignores_central_only_manifest_entry() {
    // If the ONLY manifest path for the project is the central one, there is
    // no repo-local owner → None.
    let manifest = vec![(
        "project:lonely".to_string(),
        "/u/.tachi/projects/lonely/memory.db".to_string(),
    )];
    assert_eq!(resolve_owning_repo("lonely", &manifest, &[]), None);
}

#[test]
fn build_plan_tallies_each_class() {
    let mut alias = input("alias-proj");
    alias.is_symlink = true;
    alias.symlink_target = Some(PathBuf::from("/r/.tachi/memory.db"));
    alias.symlink_target_exists = true;

    let mut broken = input("broken-proj");
    broken.is_symlink = true;
    broken.symlink_target_exists = false;

    let mut reloc = input("reloc-proj");
    reloc.owning_repo = Some(PathBuf::from("/repos/reloc-proj"));

    let home = input("home-proj");

    let garbage = input("11111111-2222-3333-4444-555555555555");

    let plan = build_plan(&[alias, broken, reloc, home, garbage]);
    assert_eq!(plan.items.len(), 5);
    assert_eq!(plan.n_symlink_alias, 1);
    assert_eq!(plan.n_symlink_broken, 1);
    assert_eq!(plan.n_relocatable, 1);
    assert_eq!(plan.n_home_resident, 1);
    assert_eq!(plan.n_garbage, 1);
}

#[cfg(unix)]
#[test]
fn gather_project_inputs_reads_symlinks_and_real_files() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");

    // (a) symlink-alias: real target exists.
    let repo = dir.path().join("repo-a/.tachi");
    std::fs::create_dir_all(&repo).unwrap();
    let real_target = repo.join("memory.db");
    std::fs::write(&real_target, b"db").unwrap();
    let alias_dir = projects.join("alias-proj");
    std::fs::create_dir_all(&alias_dir).unwrap();
    symlink(&real_target, alias_dir.join("memory.db")).unwrap();

    // (b) broken symlink.
    let broken_dir = projects.join("broken-proj");
    std::fs::create_dir_all(&broken_dir).unwrap();
    symlink(
        dir.path().join("does-not-exist.db"),
        broken_dir.join("memory.db"),
    )
    .unwrap();

    // (c) real home-resident file.
    let home_dir = projects.join("home-proj");
    std::fs::create_dir_all(&home_dir).unwrap();
    std::fs::write(home_dir.join("memory.db"), b"db").unwrap();

    // (d) uuid garbage dir with a real file.
    let uuid_dir = projects.join("8f14e45f-ceea-467a-9c8a-1b2c3d4e5f60");
    std::fs::create_dir_all(&uuid_dir).unwrap();
    std::fs::write(uuid_dir.join("memory.db"), b"db").unwrap();

    let inputs = gather_project_inputs(&projects, &[], &[]).unwrap();
    let plan = build_plan(&inputs);

    // 4 entries gathered.
    assert_eq!(plan.items.len(), 4);
    assert_eq!(plan.n_symlink_alias, 1);
    assert_eq!(plan.n_symlink_broken, 1);
    assert_eq!(plan.n_home_resident, 1);
    assert_eq!(plan.n_garbage, 1);
    // home-proj must NOT have been relocated (no owning repo passed).
    let home = plan
        .items
        .iter()
        .find(|i| i.project_name == "home-proj")
        .unwrap();
    assert_eq!(home.action, PlannedAction::KeepHomeResident);
}

#[cfg(unix)]
#[test]
fn gather_discovers_both_canonical_and_legacy_filenames() {
    // #1132 compat window: the audit must recognize canonical `tachi-memory.db`
    // stores (post-migration, the in-use file) AND legacy `memory.db` stores
    // (un-migrated), and prefer the canonical file — not the compat symlink —
    // for a dir that carries both.
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");

    // (a) canonical-only store: a fresh post-#1132 real file.
    let canon_dir = projects.join("canon-proj");
    std::fs::create_dir_all(&canon_dir).unwrap();
    std::fs::write(canon_dir.join("tachi-memory.db"), b"db").unwrap();

    // (b) legacy-only store: an un-migrated real file under the old name.
    let legacy_dir = projects.join("legacy-proj");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(legacy_dir.join("memory.db"), b"db").unwrap();

    // (c) migrated store: canonical real file + `memory.db -> tachi-memory.db`
    // compat symlink. The audit must pick the canonical real file (relocatable),
    // NOT double-count the compat symlink.
    let migrated_dir = projects.join("migrated-proj");
    std::fs::create_dir_all(&migrated_dir).unwrap();
    std::fs::write(migrated_dir.join("tachi-memory.db"), b"db").unwrap();
    symlink("tachi-memory.db", migrated_dir.join("memory.db")).unwrap();

    let inputs = gather_project_inputs(&projects, &[], &[]).unwrap();

    // Exactly one entry per project dir — the migrated dir's compat symlink is
    // not counted separately.
    assert_eq!(inputs.len(), 3, "one entry per project dir");

    let by_name = |n: &str| inputs.iter().find(|i| i.project_name == n).unwrap();

    // Canonical-only: real file under the new name is discovered.
    let canon = by_name("canon-proj");
    assert!(canon.db_path.ends_with("tachi-memory.db"));
    assert!(!canon.is_symlink, "canonical store is a real file");

    // Legacy-only: still discovered under the old name during the compat window.
    let legacy = by_name("legacy-proj");
    assert!(legacy.db_path.ends_with("memory.db"));
    assert!(
        !legacy.is_symlink,
        "un-migrated legacy store is a real file"
    );

    // Migrated: canonical real file wins over the compat symlink.
    let migrated = by_name("migrated-proj");
    assert!(migrated.db_path.ends_with("tachi-memory.db"));
    assert!(
        !migrated.is_symlink,
        "the canonical real file must be chosen over the memory.db compat symlink"
    );
}

#[cfg(unix)]
#[test]
fn gather_surfaces_both_real_coexistence_instead_of_hiding_it() {
    // RESIDUAL-3: a real `tachi-memory.db` AND a real `memory.db` in the same
    // project dir is the ambiguous state the opener refuses. The audit must NOT
    // silently show only the canonical one — it must surface BOTH, flagged, so
    // the conflict is visible in the plan.
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    let proj = projects.join("ambiguous-proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("tachi-memory.db"), b"canonical").unwrap();
    std::fs::write(proj.join("memory.db"), b"legacy").unwrap();

    let inputs = gather_project_inputs(&projects, &[], &[]).unwrap();

    // Both real files are surfaced, not just the canonical one.
    assert_eq!(
        inputs.len(),
        2,
        "both coexisting real files must be surfaced"
    );
    assert!(
        inputs.iter().all(|i| i.sibling_conflict),
        "both entries must carry the coexistence conflict flag"
    );
    assert!(inputs
        .iter()
        .any(|i| i.db_path.ends_with("tachi-memory.db")));
    assert!(inputs.iter().any(|i| i.db_path.ends_with("memory.db")));

    // The conflict is loud in the rendered plan (note field).
    let plan = build_plan(&inputs);
    assert!(
        plan.items
            .iter()
            .all(|it| it.note.contains("AMBIGUOUS #1132")),
        "the coexistence must be marked ambiguous in every item's note"
    );
}

#[cfg(unix)]
#[test]
fn gather_does_not_swallow_a_stat_error() {
    // RESIDUAL-3: a non-NotFound stat error (permission denied) on a project
    // dir's DB path must be surfaced, never swallowed and masked as "no DB
    // here". Strip search (x) permission on the project dir so lstat() inside
    // returns EACCES.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    let proj = projects.join("locked-proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("tachi-memory.db"), b"real data").unwrap();

    let original_mode = std::fs::metadata(&proj).unwrap().permissions().mode();
    std::fs::set_permissions(&proj, std::fs::Permissions::from_mode(0o000)).unwrap();

    let result = gather_project_inputs(&projects, &[], &[]);

    // Restore before asserting so tempdir cleanup can recurse in.
    std::fs::set_permissions(&proj, std::fs::Permissions::from_mode(original_mode)).unwrap();

    assert!(
        result.is_err(),
        "an un-stat-able project DB path must surface as an error, not be swallowed"
    );
}

#[test]
fn gather_returns_empty_when_dir_missing() {
    let got = gather_project_inputs(Path::new("/no/such/dir/xyz"), &[], &[]).unwrap();
    assert!(got.is_empty());
}
