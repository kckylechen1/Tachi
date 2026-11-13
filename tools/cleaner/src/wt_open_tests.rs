use super::*;
use crate::test_support::home_env_lock as env_lock;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn default_short_id_is_12_hex_and_unique() {
    // Discriminating test for the #1026 sibling fix: 12 chars pins the
    // truncation width (a regression to the old 6-hex mask would fail
    // here), hex-only pins the uuid `simple()` form, and two consecutive
    // calls differing pins the same-instant-collision fix itself.
    let a = short_id();
    let b = short_id();
    assert_eq!(a.len(), 12, "short_id must keep 48 bits (12 hex chars)");
    assert!(a
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_ne!(a, b, "consecutive short_ids must not collide");
}

// ─── tachi#1184 item 3: worktree-location law ──────────────────────────

#[test]
fn private_tmp_and_tmp_are_ephemeral() {
    let roots = vec![PathBuf::from("/tmp"), PathBuf::from("/private/tmp")];
    assert!(is_under_any_root(Path::new("/private/tmp/wz-1184"), &roots));
    assert!(is_under_any_root(Path::new("/tmp/some-session"), &roots));
    // The bare root itself also counts, not just children of it.
    assert!(is_under_any_root(Path::new("/private/tmp"), &roots));
}

#[test]
fn cache_root_is_not_ephemeral() {
    let roots = vec![PathBuf::from("/tmp"), PathBuf::from("/private/tmp")];
    assert!(!is_under_any_root(
        Path::new("/home/x/.cache/tachi/worktrees/repo"),
        &roots
    ));
}

#[test]
fn sibling_directory_is_not_a_false_positive() {
    // `/private/tmpfs-something` must not match boundary `/private/tmp`
    // via a naive string-prefix check — this is exactly the sibling
    // pitfall `path_is_within_components`'s own doc comment calls out
    // (`~/.cache/tachi/worktrees-evil` vs `~/.cache/tachi/worktrees`).
    let roots = vec![PathBuf::from("/private/tmp")];
    assert!(!is_under_any_root(
        Path::new("/private/tmpfs-something/foo"),
        &roots
    ));
}

#[test]
fn tmpdir_override_is_treated_as_ephemeral_too() {
    let roots = vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/folders/xy/abc"),
    ];
    assert!(is_under_any_root(
        Path::new("/var/folders/xy/abc/session-42"),
        &roots
    ));
}

// symlink construction is unix-only; the canonicalize fallback it
// exercises is a portable code path, but this fixture needs a real
// symlink to prove it fires, so the test itself is unix-only too (same
// convention as `work_claim.rs`'s `held_env_is_found_through_a_symlinked_path_spelling`).
#[cfg(unix)]
#[test]
fn a_symlinked_root_is_caught_via_canonicalization_c4() {
    // tachi#1184 review C4: a listed root that is ITSELF a symlink to a
    // real directory must still catch a candidate path spelled through
    // the symlink's TARGET (not just the literal listed spelling) — the
    // exact `/tmp` -> `/private/tmp`-shaped case, reproduced with a real
    // (hermetic, tempdir-scoped) symlink rather than relying on the
    // system's actual /tmp layout.
    let base = unique_temp("wt-open-c4-root");
    let real_dir = base.join("real-ephemeral-root");
    let alias = base.join("alias-into-real");
    std::fs::create_dir_all(&real_dir).unwrap();
    std::os::unix::fs::symlink(&real_dir, &alias).unwrap();
    let candidate = alias.join("session-under-symlink");
    std::fs::create_dir_all(&candidate).unwrap();

    // The ephemeral roots list only knows the ALIAS spelling; the
    // candidate path is spelled through it too, so the literal pass
    // alone already matches here — this establishes the fixture is
    // sound before the next assertion exercises the canonicalize pass.
    assert!(is_under_any_root(&candidate, std::slice::from_ref(&alias)));

    // Now spell the SAME candidate through its canonical (real_dir)
    // form, while the roots list only knows the alias spelling — the
    // literal pass cannot match this, only the canonicalize fallback
    // can.
    let canonical_candidate = std::fs::canonicalize(&candidate).unwrap();
    assert_ne!(
        canonical_candidate, candidate,
        "fixture must actually traverse a symlink for this test to mean anything"
    );
    assert!(is_under_any_root(
        &canonical_candidate,
        std::slice::from_ref(&alias)
    ));

    let _ = std::fs::remove_dir_all(&base);
}

fn unique_temp(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn init_git_repo(path: &Path) {
    assert!(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["config", "user.email", "tachi-test@example.com"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["config", "user.name", "tachi-test"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    std::fs::write(path.join("README"), "hello").unwrap();
    assert!(Command::new("git")
        .args(["add", "README"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
}

#[test]
fn forbids_desktop_and_in_repo_paths() {
    let repo = PathBuf::from("/Users/me/Desktop/Sigil");
    let desktop_wt = PathBuf::from("/Users/me/Desktop/Sigil/.claude/worktrees/agent-1");
    let reason = forbidden_location_reason(&desktop_wt, &repo).expect("forbidden");
    assert!(
        reason.contains("Desktop") || reason.contains("primary repo"),
        "unexpected reason: {reason}"
    );

    let icloud =
        PathBuf::from("/Users/me/Library/Mobile Documents/com~apple~CloudDocs/worktrees/x");
    let reason = forbidden_location_reason(&icloud, Path::new("/tmp/repo")).expect("icloud");
    assert!(reason.contains("iCloud"), "{reason}");

    let ok = PathBuf::from("/Users/me/.cache/tachi/worktrees/Sigil/484-worker-abc");
    assert!(
        forbidden_location_reason(&ok, Path::new("/Users/me/Desktop/Sigil")).is_none(),
        "managed cache path must be allowed"
    );
}

#[test]
fn plan_path_uses_managed_root_and_slug() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let old = std::env::var_os("TACHI_WORKTREES_ROOT");
    let root = unique_temp("tachi-wt-root");
    std::env::set_var("TACHI_WORKTREES_ROOT", &root);

    let planned = plan_managed_worktree_path(Path::new("/tmp/my-repo"), "484-executor-aa")
        .expect("managed root resolves when TACHI_WORKTREES_ROOT is set");
    assert_eq!(planned, root.join("my-repo").join("484-executor-aa"));

    match old {
        Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
        None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn open_refuses_desktop_path_even_when_explicit() {
    // Discrimination: pre-fix world allowed .claude/worktrees under Desktop;
    // governor must hard-refuse before git worktree add.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-wt-open-desktop");
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);

    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home);

    let desktop_path = home.join("Desktop").join("fake-wt");
    let report = open_worktree(OpenOptions {
        repo_root: repo,
        path: Some(desktop_path),
        branch: Some("tachi/test/desktop-refuse".into()),
        base: Some("HEAD".into()),
        task: Some("484".into()),
        role: Some("executor".into()),
        dispatch_id: None,
        name: None,
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(!report.opened, "must not open under Desktop");
    assert!(
        report.errors.iter().any(|e| e.contains("Desktop")),
        "expected Desktop refusal, got: {:?}",
        report.errors
    );

    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn open_creates_managed_worktree_and_marker() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-wt-open-ok");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);

    let old_home = std::env::var_os("HOME");
    let old_root = std::env::var_os("TACHI_WORKTREES_ROOT");
    std::env::set_var("HOME", &home);
    std::env::set_var("TACHI_WORKTREES_ROOT", &cache);

    let report = open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/484/executor-test".into()),
        base: Some("HEAD".into()),
        task: Some("484".into()),
        role: Some("executor".into()),
        dispatch_id: Some("dispatch-484".into()),
        name: Some("484-executor-test".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(report.errors.is_empty(), "open errors: {:?}", report.errors);
    assert!(report.opened);
    assert!(report.registered);
    let path = PathBuf::from(&report.path);
    let canonical_cache = cache.canonicalize().unwrap();
    assert!(
        path.starts_with(&canonical_cache),
        "path={} cache={}",
        path.display(),
        canonical_cache.display()
    );
    assert!(path.join(".tachi-worktree.json").exists());
    assert!(path.join("README").exists());
    assert!(registry::registry_contains(&path));

    // Cleanup worktree from the temp repo so the test dir can be removed.
    let _ = Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "worktree",
            "remove",
            "--force",
            path.to_str().unwrap(),
        ])
        .status();

    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match old_root {
        Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
        None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dry_run_does_not_create_path() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-wt-open-dry");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);

    let old_home = std::env::var_os("HOME");
    let old_root = std::env::var_os("TACHI_WORKTREES_ROOT");
    std::env::set_var("HOME", &home);
    std::env::set_var("TACHI_WORKTREES_ROOT", &cache);

    let report = open_worktree(OpenOptions {
        repo_root: repo,
        path: None,
        branch: Some("tachi/484/dry".into()),
        base: Some("HEAD".into()),
        task: Some("484".into()),
        role: Some("worker".into()),
        dispatch_id: None,
        name: Some("dry-leaf".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: true,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(report.dry_run);
    assert!(!report.opened);
    assert!(!PathBuf::from(&report.path).exists());

    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match old_root {
        Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
        None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn default_worktrees_root_rejects_relative_env_value() {
    // CP3 discrimination: a relative TACHI_WORKTREES_ROOT must never
    // silently resolve against cwd — the managed root doubles as the
    // sweep GC root and the path-boundary fence.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let old = std::env::var_os("TACHI_WORKTREES_ROOT");
    std::env::set_var("TACHI_WORKTREES_ROOT", "relative/worktrees");

    let result = default_worktrees_root();
    assert!(
        result.is_err(),
        "relative TACHI_WORKTREES_ROOT must be rejected, got {result:?}"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("absolute"),
        "error should explain the absolute-path requirement: {err}"
    );

    match old {
        Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
        None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
    }
}

#[test]
fn open_rejects_option_shaped_base_ref() {
    // CP6 discrimination: a base ref shaped like a git option must be
    // rejected before it ever reaches `git rev-parse`, not just relying
    // on `--end-of-options`. RED if `validate_ref`'s leading-`-` check
    // (or its call site) is removed.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-wt-open-ref-injection");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);

    let result = open_worktree(OpenOptions {
        repo_root: repo,
        path: None,
        branch: Some("tachi/test/ref-injection".into()),
        base: Some("--upload-pack=/bin/sh".into()),
        task: Some("484".into()),
        role: Some("executor".into()),
        dispatch_id: None,
        name: None,
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    });

    assert!(
        result.is_err(),
        "option-shaped base ref must be rejected before git rev-parse, got {result:?}"
    );
    let err = result.unwrap_err();
    assert!(err.contains("refusing base ref"), "unexpected error: {err}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn open_rejects_leading_dash_base_ref() {
    // CP6 discrimination, second shape: a bare leading-dash ref (no
    // '=' payload) must also be rejected, not just the `--flag=value`
    // form.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-wt-open-ref-injection-dash");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);

    let result = open_worktree(OpenOptions {
        repo_root: repo,
        path: None,
        branch: Some("tachi/test/ref-injection-dash".into()),
        base: Some("-x".into()),
        task: Some("484".into()),
        role: Some("executor".into()),
        dispatch_id: None,
        name: None,
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    });

    assert!(
        result.is_err(),
        "leading-dash base ref must be rejected, got {result:?}"
    );
    let err = result.unwrap_err();
    assert!(err.contains("refusing base ref"), "unexpected error: {err}");

    let _ = std::fs::remove_dir_all(root);
}

// --- #484 slice 2: shared cargo target-dir provisioning ---

#[test]
fn provision_writes_config_for_rust_repo() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-cargo-provision-rust");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let target = root.join("shared-target");

    let old = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
    std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, &target);

    let outcome =
        provision_cargo_target_config(&root, &CargoTargetPolicy::Shared).expect("provision ok");
    match &outcome {
        CargoTargetProvision::Written(dir) => assert_eq!(dir, &target),
        other => panic!("expected Written, got {other:?}"),
    }

    let config_path = root.join(".cargo").join("config.toml");
    let contents = std::fs::read_to_string(&config_path).expect("config written");
    assert!(contents.contains("[build]"), "contents: {contents}");
    assert!(
        contents.contains(&format!("target-dir = \"{}\"", target.display())),
        "contents should point at the shared target dir, got: {contents}"
    );

    match old {
        Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
        None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn provision_skips_non_rust_repo_silently() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-cargo-provision-non-rust");
    // No Cargo.toml.

    let outcome =
        provision_cargo_target_config(&root, &CargoTargetPolicy::Shared).expect("provision ok");
    assert_eq!(outcome, CargoTargetProvision::SkippedNotRustRepo);
    assert!(
        !root.join(".cargo").exists(),
        "non-Rust repo must not get a .cargo dir"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn provision_never_overwrites_existing_config() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-cargo-provision-existing");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::create_dir_all(root.join(".cargo")).unwrap();
    let custom = "# hand-written, do not touch\n[build]\ntarget-dir = \"/custom/target\"\n";
    std::fs::write(root.join(".cargo").join("config.toml"), custom).unwrap();

    let target = root.join("shared-target");
    let old = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
    std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, &target);

    let outcome =
        provision_cargo_target_config(&root, &CargoTargetPolicy::Shared).expect("provision ok");
    assert_eq!(outcome, CargoTargetProvision::SkippedExisting);
    let contents = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
    assert_eq!(contents, custom, "existing config.toml must be untouched");

    match old {
        Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
        None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn provision_rejects_relative_override_and_writes_no_config() {
    // A relative TACHI_SHARED_CARGO_TARGET_DIR would make cargo resolve
    // target-dir against whatever cwd the build happens to run from,
    // defeating the whole point of a *shared* target dir. Must be
    // rejected outright rather than silently written relative.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-cargo-provision-relative");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

    let old = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
    std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, "relative/shared-target");

    let err = provision_cargo_target_config(&root, &CargoTargetPolicy::Shared)
        .expect_err("relative override must be rejected");
    assert!(
        err.contains("must be an absolute path"),
        "error should explain the absolute-path requirement, got: {err}"
    );
    assert!(
        !root.join(".cargo").join("config.toml").exists(),
        "no config.toml should be written for a rejected relative override"
    );

    match old {
        Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
        None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn provision_writes_absolute_override_verbatim() {
    // Companion to the relative-rejection test above: an absolute
    // override must be written into config.toml exactly as given, not
    // normalized/canonicalized.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-cargo-provision-absolute-verbatim");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let target = root.join("nested").join("shared-target");
    assert!(target.is_absolute());

    let old = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
    std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, &target);

    let outcome =
        provision_cargo_target_config(&root, &CargoTargetPolicy::Shared).expect("provision ok");
    assert_eq!(outcome, CargoTargetProvision::Written(target.clone()));
    let contents = std::fs::read_to_string(root.join(".cargo").join("config.toml")).unwrap();
    assert!(
        contents.contains(&format!("target-dir = \"{}\"", target.display())),
        "absolute override should be written verbatim, got: {contents}"
    );

    match old {
        Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
        None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn private_config_rollback_removes_only_the_owned_exact_payload() {
    let root = unique_temp("tachi-private-config-rollback");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let target = root.join("private-target");
    assert!(matches!(
        provision_cargo_target_config(&root, &CargoTargetPolicy::Private(target.clone()))
            .unwrap(),
        CargoTargetProvision::Written(ref written) if written == &target
    ));
    assert!(rollback_private_cargo_target_config(&root, &target).unwrap());
    let config = root.join(".cargo/config.toml");
    assert!(!config.exists());

    std::fs::write(&config, "[build]\ntarget-dir = \"/foreign\"\n").unwrap();
    assert!(!rollback_private_cargo_target_config(&root, &target).unwrap());
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "[build]\ntarget-dir = \"/foreign\"\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn private_config_io_refuses_symlinked_cargo_directory_and_never_removes_target() {
    use std::os::unix::fs::symlink;

    let root = unique_temp("tachi-private-config-symlink-root");
    let external = unique_temp("tachi-private-config-symlink-external");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::remove_dir(root.join(".cargo")).ok();
    symlink(&external, root.join(".cargo")).unwrap();
    let target = root.join("private-target");

    let error = provision_cargo_target_config(&root, &CargoTargetPolicy::Private(target.clone()))
        .unwrap_err();
    assert!(
        error.contains("open anchored"),
        "unexpected provisioning error: {error}"
    );
    assert!(!external.join("config.toml").exists());

    let external_config = external.join("config.toml");
    std::fs::write(&external_config, "external must survive\n").unwrap();
    assert!(rollback_private_cargo_target_config(&root, &target).is_err());
    assert_eq!(
        std::fs::read_to_string(&external_config).unwrap(),
        "external must survive\n"
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(external);
}

#[cfg(unix)]
#[test]
fn private_config_rollback_captures_symlink_without_following_its_target() {
    use std::os::unix::fs::symlink;

    let root = unique_temp("tachi-private-config-final-symlink");
    let external = root.join("external-config");
    let cargo = root.join(".cargo");
    std::fs::create_dir_all(&cargo).unwrap();
    std::fs::write(&external, "external must survive\n").unwrap();
    symlink(&external, cargo.join("config.toml")).unwrap();
    let target = root.join("private-target");

    assert!(!rollback_private_cargo_target_config(&root, &target).unwrap());
    assert!(cargo.join("config.toml").is_symlink());
    assert_eq!(
        std::fs::read_to_string(&external).unwrap(),
        "external must survive\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn open_worktree_warns_and_skips_provisioning_for_relative_override() {
    // End-to-end: a relative TACHI_SHARED_CARGO_TARGET_DIR must not
    // fail the worktree open (non-fatal, same style as other
    // provisioning failures) but must surface a warning and leave no
    // config.toml behind.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-wt-open-cargo-relative");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "Cargo.toml"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "add Cargo.toml"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());

    let old_home = std::env::var_os("HOME");
    let old_root = std::env::var_os("TACHI_WORKTREES_ROOT");
    let old_target = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
    std::env::set_var("HOME", &home);
    std::env::set_var("TACHI_WORKTREES_ROOT", &cache);
    std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, "relative/shared-target");

    let report = open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/484/executor-cargo-relative".into()),
        base: Some("HEAD".into()),
        task: Some("484".into()),
        role: Some("executor".into()),
        dispatch_id: Some("dispatch-484-cargo-relative".into()),
        name: Some("484-executor-cargo-relative".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(report.errors.is_empty(), "open errors: {:?}", report.errors);
    assert!(report.opened);
    assert!(
        report.cargo_target_dir.is_none(),
        "no cargo_target_dir should be reported for a rejected relative override"
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("must be an absolute path")),
        "expected a warning about the relative override, got: {:?}",
        report.warnings
    );

    let path = PathBuf::from(&report.path);
    assert!(
        !path.join(".cargo").join("config.toml").exists(),
        "no config.toml should be written when the override is relative"
    );

    // Cleanup worktree from the temp repo so the test dir can be removed.
    let _ = Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "worktree",
            "remove",
            "--force",
            path.to_str().unwrap(),
        ])
        .status();

    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match old_root {
        Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
        None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
    }
    match old_target {
        Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
        None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn open_worktree_provisions_shared_cargo_target_for_rust_repo() {
    // End-to-end: git worktree add checks out a tracked root Cargo.toml,
    // so open_worktree must then write .cargo/config.toml pointing at
    // the shared target dir into the freshly created worktree.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-wt-open-cargo");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let shared_target = root.join("shared-target");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "Cargo.toml"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "add Cargo.toml"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());

    let old_home = std::env::var_os("HOME");
    let old_root = std::env::var_os("TACHI_WORKTREES_ROOT");
    let old_target = std::env::var_os(SHARED_CARGO_TARGET_DIR_ENV);
    std::env::set_var("HOME", &home);
    std::env::set_var("TACHI_WORKTREES_ROOT", &cache);
    std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, &shared_target);

    let report = open_worktree(OpenOptions {
        repo_root: repo.clone(),
        path: None,
        branch: Some("tachi/484/executor-cargo".into()),
        base: Some("HEAD".into()),
        task: Some("484".into()),
        role: Some("executor".into()),
        dispatch_id: Some("dispatch-484-cargo".into()),
        name: Some("484-executor-cargo".into()),
        cargo_target: CargoTargetPolicy::Shared,
        dry_run: false,
        output: OutputFormat::Json,
    })
    .unwrap();

    assert!(report.errors.is_empty(), "open errors: {:?}", report.errors);
    assert!(report.opened);
    assert_eq!(
        report.cargo_target_dir.as_deref(),
        Some(shared_target.display().to_string().as_str())
    );

    let path = PathBuf::from(&report.path);
    let config = path.join(".cargo").join("config.toml");
    assert!(config.exists(), "expected {} to exist", config.display());
    let contents = std::fs::read_to_string(&config).unwrap();
    assert!(
        contents.contains(&shared_target.display().to_string()),
        "contents: {contents}"
    );

    // Cleanup worktree from the temp repo so the test dir can be removed.
    let _ = Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "worktree",
            "remove",
            "--force",
            path.to_str().unwrap(),
        ])
        .status();

    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match old_root {
        Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
        None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
    }
    match old_target {
        Some(v) => std::env::set_var(SHARED_CARGO_TARGET_DIR_ENV, v),
        None => std::env::remove_var(SHARED_CARGO_TARGET_DIR_ENV),
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn open_worktree_accepts_long_and_ref_hazardous_task_prompts() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let root = unique_temp("tachi-wt-open-hazards");
    let home = root.join("home");
    let cache = root.join("cache-worktrees");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);

    let old_home = std::env::var_os("HOME");
    let old_root = std::env::var_os("TACHI_WORKTREES_ROOT");
    std::env::set_var("HOME", &home);
    std::env::set_var("TACHI_WORKTREES_ROOT", &cache);

    assert!(
        plan_managed_worktree_path(&repo, "   ").is_err(),
        "empty leaf name must not resolve to parent worktree root"
    );

    let long_prefix = "audit-multi-sentence-staff-assignment-".repeat(10);
    let cases = [
        (
            format!("{long_prefix}alpha"),
            Some(format!("{long_prefix}leaf-alpha")),
        ),
        (
            format!("{long_prefix}bravo"),
            Some(format!("{long_prefix}leaf-bravo")),
        ),
        ("diff commit range a..b and v1...v2".to_string(), None),
        (".env audit and refresh Cargo.lock".to_string(), None),
        (
            "LOCKfile check ending with FOO.LOCK.lock.".to_string(),
            None,
        ),
        (
            "趁热修掉原生会话长任务启动失败问题".to_string(),
            Some("中文目录名测试-alpha".to_string()),
        ),
        (
            "检查第二条混合中文与英文标识不碰撞".to_string(),
            Some("混合中文ASCII甲".to_string()),
        ),
        (
            "检查第三条混合中文与英文标识不碰撞".to_string(),
            Some("混合中文ASCII乙".to_string()),
        ),
        (
            "检查纯中文无英文任务甲".to_string(),
            Some("纯中文目录甲".to_string()),
        ),
        (
            "检查纯中文无英文任务乙".to_string(),
            Some("纯中文目录乙".to_string()),
        ),
    ];

    let mut seen_branches = std::collections::HashSet::new();
    let mut seen_paths = std::collections::HashSet::new();
    for (idx, (task, custom_name)) in cases.iter().enumerate() {
        let report = open_worktree(OpenOptions {
            repo_root: repo.clone(),
            path: None,
            branch: None,
            base: Some("HEAD".into()),
            task: Some(task.clone()),
            role: Some("reviewer".into()),
            dispatch_id: Some(format!("dispatch-hazard-{idx}")),
            name: custom_name.clone(),
            cargo_target: CargoTargetPolicy::Unallocated,
            dry_run: false,
            output: OutputFormat::Json,
        })
        .unwrap_or_else(|err| panic!("case {idx} ({task}) returned Err: {err}"));

        assert!(
            report.opened && report.errors.is_empty(),
            "case {idx} ({task}) failed to open worktree: {:?}",
            report
        );
        assert!(
            Command::new("git")
                .args(["check-ref-format", "--branch", &report.branch])
                .status()
                .unwrap()
                .success(),
            "branch {:?} failed git check-ref-format",
            report.branch
        );
        for segment in report.branch.split('/') {
            assert!(
                segment.len() <= 255,
                "branch segment too long: {} ({segment})",
                segment.len()
            );
        }
        let leaf = PathBuf::from(&report.path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(leaf.len() <= 128, "leaf too long: {} ({leaf})", leaf.len());
        assert!(
            seen_branches.insert(report.branch.clone()),
            "distinct inputs collided on branch {}",
            report.branch
        );
        assert!(
            seen_paths.insert(report.path.clone()),
            "distinct inputs collided on path {}",
            report.path
        );

        let _ = Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "worktree",
                "remove",
                "--force",
                &report.path,
            ])
            .status();
    }

    match old_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match old_root {
        Some(v) => std::env::set_var("TACHI_WORKTREES_ROOT", v),
        None => std::env::remove_var("TACHI_WORKTREES_ROOT"),
    }
    let _ = std::fs::remove_dir_all(root);
}
