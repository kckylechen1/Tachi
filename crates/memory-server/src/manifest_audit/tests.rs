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
        Some("/Users/me/repos/quant/.tachi/memory.db")
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

#[test]
fn gather_returns_empty_when_dir_missing() {
    let got = gather_project_inputs(Path::new("/no/such/dir/xyz"), &[], &[]).unwrap();
    assert!(got.is_empty());
}
