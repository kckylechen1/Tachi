use super::*;

// ─── PR-2: hygiene / GC tests ──────────────────────────────────────────

fn restore_env_var(name: &str, value: Option<std::ffi::OsString>) {
    if let Some(v) = value {
        std::env::set_var(name, v);
    } else {
        std::env::remove_var(name);
    }
}

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
fn repo_local_db_fixture_helper_avoids_linux_tmp_skip_semantics() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved_target = std::env::var_os("CARGO_TARGET_DIR");
    std::env::set_var("CARGO_TARGET_DIR", "/private/tmp/sigil-target-review-784");

    assert!(
        should_skip_path(Path::new("/tmp/case/repo/.tachi/memory.db")).is_some(),
        "Linux /tmp repo-local DB fixtures are treated as temporary workspaces"
    );

    let tmp = crate::test_support::non_skipped_fixture_tempdir("manifest-guard-");
    let local_db = tmp.path().join("repo/.tachi/memory.db");
    crate::test_support::assert_repo_local_db_fixture_not_skipped(&local_db);

    restore_env_var("CARGO_TARGET_DIR", saved_target);
}
