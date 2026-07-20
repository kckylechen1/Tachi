//! Discrimination tests for the vendored-skill hermeticity fix
//! (kckylechen1/tachi#895 follow-up).
//!
//! Before this change, `builtins::waza` / `builtins::superpowers` and
//! `bootstrap::skill_surface_cli` embedded `skill/...` content at *compile*
//! time via `include_str!`, and `skill/` is a host-absolute git symlink
//! (#895) — so the same SHA compiled to different binaries depending on the
//! symlink target, and a checkout without that host path couldn't build at
//! all. This module exercises the runtime replacement:
//! `helpers::resolve_skill_content_source`, which resolves through the same
//! `skill_source_resolver::resolve_vendored_skill_path` fallback chain the flow-stage injection
//! path already uses, at *seed* time instead of compile time.
//!
//! Both tests below use a synthetic `rel_path` that cannot exist in the repo
//! tree, cwd, or `CARGO_MANIFEST_DIR` — so they are driven purely by
//! `TACHI_SKILLS_ROOT`, deterministically, on any host (including one where
//! the dev machine's real `skill/` symlink is present, and one where it is
//! not — the exact fresh-checkout condition #895 introduced).

use super::helpers::{resolve_skill_content_source, SkillContentSource};

/// RAII guard for `TACHI_SKILLS_ROOT`, mirroring `SkillsRootGuard` in
/// `shell_ops::tests::instructions` (that struct is private to its module —
/// standard Rust encapsulation — so it can't be imported directly; this is
/// the same pattern, sharing the *same* process-wide lock via
/// `crate::utils::global_test_lock()` so a mutation here can never race a
/// concurrent `TACHI_SKILLS_ROOT` mutation in `shell_ops`'s tests).
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
        // SAFETY: the global test lock is held for this guard's whole
        // lifetime, serialising every `TACHI_SKILLS_ROOT` mutator across the
        // crate's test suite; no non-test code path mutates this key.
        unsafe {
            std::env::set_var("TACHI_SKILLS_ROOT", value);
        }
    }
}

impl Drop for SkillsRootGuard {
    fn drop(&mut self) {
        // SAFETY: still holding the global test lock; restore or clear.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var("TACHI_SKILLS_ROOT", v),
                None => std::env::remove_var("TACHI_SKILLS_ROOT"),
            }
        }
    }
}

#[test]
fn resolve_skill_content_source_library_present_resolves_and_hashes() {
    let env = SkillsRootGuard::acquire();

    let rel_path = "skill/__hermeticity_fixture_present__/SKILL.md";
    let fixture_content = "---\nname: hermeticity-fixture\n---\n# fixture skill\n";

    // Guard the discriminating property: this synthetic path genuinely does
    // not exist anywhere by default (repo root / cwd / cargo manifest dir /
    // the real central library), so a resolve success below is driven by
    // the fixture we're about to mount, not an accidental hit.
    assert!(
        crate::skill_source_resolver::resolve_vendored_skill_path(rel_path).is_none(),
        "fixture probe must not resolve before the fixture central library is mounted"
    );

    let base = chrono::Utc::now().format("%Y%m%dT%H%M%S%fZ").to_string();
    let central = std::env::temp_dir().join(format!("tachi-builtins-hermeticity-present-{base}"));
    let file = central.join(rel_path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, fixture_content).unwrap();

    env.set(&central);

    let (content, resolved_path, content_hash, source_path) =
        resolve_skill_content_source("fixture", &SkillContentSource::Vendored(rel_path));

    assert_eq!(
        content, fixture_content,
        "resolved content must be read from the mounted fixture library"
    );
    let resolved_path = resolved_path.expect("resolved_path must be Some when library present");
    assert!(
        resolved_path.starts_with(&central.to_string_lossy().to_string()),
        "resolved_path must live under the fixture central library root, got {resolved_path}"
    );
    assert_eq!(
        content_hash.as_deref(),
        Some(crate::utils::stable_hash(fixture_content).as_str()),
        "content_hash must be the crate's standard stable_hash of the resolved content, non-null"
    );
    assert_eq!(source_path, rel_path);

    let _ = std::fs::remove_dir_all(&central);
    // `env` drops here: restores `TACHI_SKILLS_ROOT` and releases the lock.
}

#[test]
fn resolve_skill_content_source_library_absent_degrades_to_stub_not_error() {
    let env = SkillsRootGuard::acquire();

    let rel_path = "skill/__hermeticity_fixture_absent__/SKILL.md";
    assert!(
        crate::skill_source_resolver::resolve_vendored_skill_path(rel_path).is_none(),
        "fixture probe must not resolve before the (empty) fixture library is mounted either"
    );

    let base = chrono::Utc::now().format("%Y%m%dT%H%M%S%fZ").to_string();
    let empty_central =
        std::env::temp_dir().join(format!("tachi-builtins-hermeticity-absent-{base}"));
    std::fs::create_dir_all(&empty_central).unwrap();
    env.set(&empty_central);

    // Owner-ratified degradation policy (b): this call must NOT panic, must
    // NOT return an error type of any kind — it's an infallible tuple
    // return — and must produce a stub: empty content, no resolved path,
    // `content_hash: None` (serializes to JSON `null` in the capability
    // definition built by `waza.rs` / `superpowers.rs`). A hard-error
    // implementation (e.g. propagating the resolver's `None` via
    // `?` up through `builtin_waza_skills() -> Result<..., String>`, failing
    // the whole seed pass) would fail this test differently: either it
    // wouldn't compile against this infallible signature, or — if adapted to
    // return `Err` instead — a caller asserting `Ok(_)` here would fail.
    let (content, resolved_path, content_hash, source_path) =
        resolve_skill_content_source("fixture", &SkillContentSource::Vendored(rel_path));

    assert_eq!(
        content, "",
        "stub content must be empty, not a resolved SKILL.md body"
    );
    assert!(
        resolved_path.is_none(),
        "no filesystem path was resolved for a library-less host"
    );
    assert!(
        content_hash.is_none(),
        "content_hash must be null for a stub capability — this is the property that \
         discriminates policy (b) graceful degradation from a hard-error implementation"
    );
    // The attempted rel_path is still recorded even when unresolved, so a
    // persisted stub capability shows what it was looking for.
    assert_eq!(source_path, rel_path);

    let _ = std::fs::remove_dir_all(&empty_central);
}

#[test]
fn builtin_waza_and_superpowers_seed_successfully_regardless_of_library_state() {
    // Read-only w.r.t. `TACHI_SKILLS_ROOT`, but still shares the global test
    // lock: the resolver reads that env var internally, and the two
    // tests above mutate it, so grabbing the lock (even just to read) keeps
    // this test from ever observing a transiently-mutated value.
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // Whatever this host's ambient `TACHI_SKILLS_ROOT` / `~/.agents/vendored-skills`
    // state is, seeding must not fail outright — `builtin_waza_skills()` /
    // `builtin_superpowers_skills()` return `Result<Vec<HubCapability>, String>`
    // and every element must construct successfully (stub or real content),
    // proving the degradation policy never turns into a seed-pass error.
    let waza = super::waza::builtin_waza_skills();
    assert!(waza.is_ok(), "builtin_waza_skills must not error: {waza:?}");
    let waza = waza.unwrap();
    assert_eq!(
        waza.len(),
        9,
        "all 9 waza skills must still produce a capability"
    );

    let superpowers = super::superpowers::builtin_superpowers_skills();
    assert!(
        superpowers.is_ok(),
        "builtin_superpowers_skills must not error: {superpowers:?}"
    );
    let superpowers = superpowers.unwrap();
    assert_eq!(
        superpowers.len(),
        7,
        "all 7 superpowers skills must still produce a capability"
    );

    // Every capability's `definition` must be valid JSON with a `content_hash`
    // key present (either a hash string or null — never simply missing).
    for cap in waza.iter().chain(superpowers.iter()) {
        let def: serde_json::Value = serde_json::from_str(&cap.definition)
            .unwrap_or_else(|e| panic!("capability {} definition must be valid JSON: {e}", cap.id));
        assert!(
            def.get("content_hash").is_some(),
            "capability {} definition must carry a content_hash key (string or null)",
            cap.id
        );
    }
}

#[test]
fn builtin_skill_definitions_keep_runtime_paths_out_of_scored_content() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let waza = super::waza::builtin_waza_skills().expect("seed Waza builtins");
    let superpowers =
        super::superpowers::builtin_superpowers_skills().expect("seed Superpowers builtins");

    for capability in waza.iter().chain(superpowers.iter()) {
        let definition: serde_json::Value =
            serde_json::from_str(&capability.definition).expect("builtin definition JSON");
        assert!(
            definition.get("resolved_path").is_none(),
            "runtime filesystem paths must not enter the scored definition for {}: {definition}",
            capability.id
        );
        let source_path = definition["source_path"]
            .as_str()
            .expect("builtin definition has a source_path");
        assert!(
            !std::path::Path::new(source_path).is_absolute(),
            "source_path stays relative to the vendored corpus: {source_path}"
        );
    }
}

#[test]
fn reseeding_changed_builtin_definition_preserves_operational_ranking_state() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let server = crate::tests::make_server();
    let builtin = super::waza::builtin_waza_skills()
        .expect("seed Waza builtins")
        .into_iter()
        .next()
        .expect("a Waza builtin");

    server
        .with_global_store(|store| {
            let mut existing = store
                .hub_get(&builtin.id)
                .map_err(|error| format!("load builtin {}: {error}", builtin.id))?
                .expect("server startup seeded builtin");
            let mut definition: serde_json::Value =
                serde_json::from_str(&existing.definition).expect("builtin definition JSON");
            definition["resolved_path"] = serde_json::json!("/tmp/codex-worktree/SKILL.md");
            existing.definition =
                serde_json::to_string(&definition).expect("serialize old definition");
            existing.enabled = false;
            existing.review_status = "pending".to_string();
            existing.health_status = "degraded".to_string();
            existing.last_error = Some("previous transient failure".to_string());
            existing.last_success_at = Some("2026-07-15T00:00:00Z".to_string());
            existing.last_failure_at = Some("2026-07-14T00:00:00Z".to_string());
            existing.fail_streak = 2;
            existing.active_version = Some("skill:waza-check@2".to_string());
            existing.exposure_mode = "disabled_by_operator".to_string();
            existing.uses = 17;
            existing.successes = 13;
            existing.failures = 4;
            existing.avg_rating = 4.25;
            existing.last_used = Some("2026-07-15T12:00:00Z".to_string());
            store
                .hub_register(&existing)
                .map_err(|error| format!("store simulated pre-upgrade builtin: {error}"))
        })
        .expect("prepare pre-upgrade builtin");

    super::seed::seed_builtin_capabilities(&server).expect("reseed changed builtin definition");

    let updated = server
        .with_global_store_read(|store| {
            store
                .hub_get(&builtin.id)
                .map_err(|error| format!("reload builtin {}: {error}", builtin.id))
        })
        .expect("load reseeded builtin")
        .expect("reseeded builtin exists");
    let definition: serde_json::Value =
        serde_json::from_str(&updated.definition).expect("updated definition JSON");
    assert!(definition.get("resolved_path").is_none());
    assert!(!updated.enabled);
    assert_eq!(updated.review_status, "pending");
    assert_eq!(updated.health_status, "degraded");
    assert_eq!(
        updated.last_error.as_deref(),
        Some("previous transient failure")
    );
    assert_eq!(
        updated.last_success_at.as_deref(),
        Some("2026-07-15T00:00:00Z")
    );
    assert_eq!(
        updated.last_failure_at.as_deref(),
        Some("2026-07-14T00:00:00Z")
    );
    assert_eq!(updated.fail_streak, 2);
    assert_eq!(
        updated.active_version.as_deref(),
        Some("skill:waza-check@2")
    );
    assert_eq!(updated.exposure_mode, "disabled_by_operator");
    assert_eq!(updated.uses, 17);
    assert_eq!(updated.successes, 13);
    assert_eq!(updated.failures, 4);
    assert_eq!(updated.avg_rating, 4.25);
    assert_eq!(updated.last_used.as_deref(), Some("2026-07-15T12:00:00Z"));
}
