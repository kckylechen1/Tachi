use super::*;

#[test]
fn slugify_basic() {
    assert_eq!(slugify("Hello, World!"), "hello-world");
    assert_eq!(slugify("   "), "flow");
    assert_eq!(slugify("已经-OK_test"), "ok-test");
}

#[test]
fn meta_skill_mapping_is_complete() {
    for stage in &["brainstorm", "plan", "dispatch", "review", "ship"] {
        assert!(meta_skill_for_stage(stage).is_some(), "stage {stage}");
    }
    assert!(meta_skill_for_stage("kanban").is_none());
    assert!(meta_skill_for_stage("status").is_none());
}

/// RAII guard for `TACHI_SKILLS_ROOT`: holds the process-wide env test lock for
/// the whole test body and restores the previous value on drop, so a panicking
/// assertion can never leave the env var polluted for a concurrent test.
struct SkillsRootGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: Option<std::ffi::OsString>,
}

impl SkillsRootGuard {
    fn acquire() -> Self {
        let lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("TACHI_SKILLS_ROOT");
        SkillsRootGuard { _lock: lock, prev }
    }

    fn set(&self, value: &std::path::Path) {
        // SAFETY: edition-2021 `set_var` is unsafe because it can race with
        // concurrent env readers. Safe here: the global test lock is held for
        // this guard's whole lifetime, serialising every `TACHI_SKILLS_ROOT`
        // mutator, and no non-test code path mutates this key.
        unsafe {
            std::env::set_var("TACHI_SKILLS_ROOT", value);
        }
    }
}

impl Drop for SkillsRootGuard {
    fn drop(&mut self) {
        // SAFETY: still holding the global test lock; restore the original
        // value (or clear it) before releasing.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var("TACHI_SKILLS_ROOT", v),
                None => std::env::remove_var("TACHI_SKILLS_ROOT"),
            }
        }
    }
}

#[test]
fn superpowers_meta_skills_resolve_for_all_shell_stages() {
    // `resolve_meta_skill` reads `TACHI_SKILLS_ROOT`; share the same env lock as
    // the central-fallback test so its mutation can never pollute this read.
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for stage in STAGE_ACTIONS {
        let rel = meta_skill_for_stage(stage).expect("mapped stage");
        let resolved = resolve_meta_skill(rel)
            .unwrap_or_else(|| panic!("superpowers skill not found for stage {stage} at {rel}"));
        assert!(
            resolved.ends_with("SKILL.md"),
            "stage {stage} should resolve to SKILL.md, got {}",
            resolved.display()
        );
        let content = std::fs::read_to_string(&resolved)
            .unwrap_or_else(|e| panic!("read superpowers skill for {stage}: {e}"));
        assert!(
            content.contains("name:") || content.starts_with("# "),
            "stage {stage} skill should look like a SKILL.md front matter or heading"
        );
    }
}

#[test]
fn resolve_meta_skill_falls_back_to_central_library() {
    let env = SkillsRootGuard::acquire();

    // A rel_path under the superpowers corpus that does NOT exist in the repo
    // tree — so the repo-root, cwd, and cargo-manifest roots all miss. Before
    // the central fallback existed this resolved to `None`; the central
    // vendored-skills library is the only root that can satisfy it, which is
    // exactly what a fresh worktree / clone (no `skill/` on disk) relies on.
    let rel = "skill/superpowers/skills/__central_fallback_probe__/SKILL.md";

    // Guard the discriminating property: the probe genuinely is not present
    // in-repo, so this test exercises the new fallback, not an accidental hit.
    if let Some(root) = cached_git_root() {
        assert!(
            !root.join(rel).exists(),
            "probe rel_path must not exist in the repo tree"
        );
    }
    assert!(
        !PathBuf::from(rel).exists(),
        "probe must not exist under cwd"
    );

    let base = Utc::now().format("%Y%m%dT%H%M%S%fZ").to_string();
    let central = std::env::temp_dir().join(format!("tachi-central-skills-full-{base}"));
    let empty_central = std::env::temp_dir().join(format!("tachi-central-skills-empty-{base}"));
    std::fs::create_dir_all(&empty_central).unwrap();
    let file = central.join(rel);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "---\nname: writing-plans\n---\n# central copy\n").unwrap();

    // (a) Central library missing the probe → still `None`. This is the
    //     pre-change (no central fallback) behaviour, and proves the resolution
    //     is driven by the central library's *contents*, not by the env var
    //     merely being set.
    env.set(&empty_central);
    assert!(
        resolve_meta_skill(rel).is_none(),
        "empty central library must not resolve the probe"
    );

    // (b) Central library holding the probe → resolves, from the central dir.
    env.set(&central);
    let resolved = resolve_meta_skill(rel).expect("probe must resolve from the central library");
    assert_eq!(resolved, file, "must resolve to the central library copy");
    assert!(
        resolved.starts_with(&central),
        "resolved path must live under the central library root"
    );

    let _ = std::fs::remove_dir_all(&central);
    let _ = std::fs::remove_dir_all(&empty_central);
    // `env` drops here: restores `TACHI_SKILLS_ROOT` and releases the lock,
    // even if an assertion above panicked.
}

#[test]
fn resolve_meta_skill_rejects_unsafe_rel_paths() {
    // Defence in depth: absolute paths and `..` traversal must never resolve,
    // regardless of whether the target happens to exist on disk. `/etc/passwd`
    // exists on this host, so a naive resolver would return it — the guard must
    // reject it before any filesystem probe.
    assert!(
        resolve_meta_skill("/etc/passwd").is_none(),
        "absolute path must be rejected"
    );
    assert!(
        resolve_meta_skill("skill/../../../etc/passwd").is_none(),
        "`..` traversal must be rejected"
    );
    assert!(
        resolve_meta_skill("../outside/SKILL.md").is_none(),
        "leading `..` must be rejected"
    );
}

#[test]
fn build_instruction_includes_required_sections() {
    let inj = InjectionResult {
        required: true,
        rel_path: Some("skill/x/SKILL.md".into()),
        source_path: Some("/abs/skill/x/SKILL.md".into()),
        injected_path: Some(".tachi/runs/flow_x/injected/superpowers-plan.md".into()),
        content_hash: Some("a".repeat(16)),
        loaded: true,
        warning: None,
        failure_class: None,
    };
    let s = build_instruction_md(
        "flow_x",
        "plan",
        "do the thing",
        &inj,
        Some("be careful"),
        &["cargo test".to_string()],
        &["crates/tachi-server/**".to_string()],
    );
    assert!(s.contains("flow_x"));
    assert!(s.contains("Stage: **plan**"));
    assert!(s.contains("do the thing"));
    assert!(s.contains("superpowers-plan.md"));
    assert!(s.contains("## Native Lifecycle Policy"));
    assert!(s.contains("skill:superpowers-writing-plans"));
    assert!(s.contains("skill:waza-think"));
    assert!(s.contains("max 6 concurrent workers"));
    assert!(s.contains("cargo test"));
    assert!(s.contains("crates/tachi-server/**"));
    assert!(s.contains("be careful"));
}

#[test]
fn ship_instruction_includes_pr_first_release_flow() {
    let inj = InjectionResult {
        required: true,
        rel_path: Some("skill/x/SKILL.md".into()),
        source_path: None,
        injected_path: Some(".tachi/runs/flow_x/injected/superpowers-ship.md".into()),
        content_hash: Some("b".repeat(16)),
        loaded: true,
        warning: None,
        failure_class: None,
    };
    let s = build_instruction_md("flow_x", "ship", "ship it", &inj, None, &[], &[]);
    assert!(s.contains("## Release Flow"));
    assert!(s.contains("Push the feature branch"));
    assert!(s.contains("Open a PR"));
    assert!(s.contains("Pass the PR gate"));
    assert!(s.contains("CI checks"));
    assert!(s.contains("Merge the PR"));
    assert!(!s.contains("direct push to the protected branch:\n\n1. Merge"));
}
