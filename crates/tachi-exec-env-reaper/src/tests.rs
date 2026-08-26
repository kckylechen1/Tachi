//! Orphan-reaper test module, split out of tachi-server's exec_env_reaper module (#1423).
//!
//! Relocated test suite: no existing test was removed, renamed, re-`#[ignore]`d,
//! or had an assertion weakened. The crate-local fixture keeps tachi-server's
//! suite-scoped root and stale-fixture GC contract; the process-env source guard
//! reads BOTH files. This new file is only the source named by future kill-test
//! receipt templates. The historical checked-in receipt remains bound to the old
//! tachi-server source blob and is intentionally unchanged.

use super::*;
use std::collections::BTreeSet;

const TEST_FIXTURE_ROOT_NAME: &str = "tachi-tests";
const TEST_FIXTURE_MAX_AGE: Duration = Duration::from_secs(3600);

fn suite_fixture_root() -> PathBuf {
    std::env::temp_dir().join(TEST_FIXTURE_ROOT_NAME)
}

fn parse_run_dir_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("run-")?;
    let (pid_str, uuid_part) = rest.split_once('-')?;
    if uuid_part.is_empty() {
        return None;
    }
    pid_str.parse().ok().filter(|pid| *pid > 1)
}

fn process_alive(pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    // SAFETY: signal 0 is an existence/permission probe; does not deliver.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    matches!(err.raw_os_error(), Some(code) if code == libc::EPERM)
}

fn is_mtime_stale(entry: &std::fs::DirEntry, now: SystemTime) -> bool {
    entry
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|mtime| now.duration_since(mtime).ok())
        .is_some_and(|age| age > TEST_FIXTURE_MAX_AGE)
}

fn gc_stale_test_fixtures(suite_root: &Path, now: SystemTime, keep: Option<&Path>) -> usize {
    let Ok(entries) = std::fs::read_dir(suite_root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.is_some_and(|keep| keep == path.as_path()) || !is_mtime_stale(&entry, now) {
            continue;
        }
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                if let Some(pid) = parse_run_dir_pid(name) {
                    if process_alive(pid) {
                        continue;
                    }
                }
            }
            if std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        } else if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn test_fixture_root() -> PathBuf {
    static RUN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    RUN.get_or_init(|| {
        let suite = suite_fixture_root();
        let _ = std::fs::create_dir_all(&suite);
        let run = suite.join(format!(
            "run-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = gc_stale_test_fixtures(&suite, SystemTime::now(), Some(run.as_path()));
        let _ = std::fs::create_dir_all(&run);
        run
    })
    .clone()
}

fn test_fixture_path(name: impl AsRef<Path>) -> PathBuf {
    test_fixture_root().join(name)
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let path = test_fixture_path(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// A directory that looks like a dead build target, with some bytes in it.
fn make_target_dir(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("debug")).unwrap();
    std::fs::write(dir.join("debug/artifact.rlib"), vec![7u8; 2048]).unwrap();
    dir
}

fn unheld_probe() -> Box<HolderProbe> {
    Box::new(|_path: &Path, _ignored_holder| HolderCheck::None)
}

fn held_probe() -> Box<HolderProbe> {
    Box::new(|_path: &Path, _ignored_holder| HolderCheck::Held(vec!["cargo 4242".to_string()]))
}

/// `now` shifted far past every fixture's mtime, so the fixtures read as
/// stale without touching the filesystem clock.
fn aged_now(days: u64) -> SystemTime {
    SystemTime::now() + Duration::from_secs(days * SECS_PER_DAY)
}

fn open_store(dir: &Path) -> memcore::MemoryStore {
    let db = dir.join("memory.db");
    memcore::MemoryStore::open(db.to_str().unwrap()).unwrap()
}

fn opts(root: &Path, force: bool) -> ReapOptions {
    ReapOptions {
        roots: vec![root.to_path_buf()],
        max_age_days: 7,
        force,
    }
}

/// A stable stand-in `(dev, ino)` for fixtures that never touch the real
/// filesystem — the identity gate only cares whether one was captured, not
/// what it is; the real re-stat happens in `delete_resource_bytes`, on a
/// fixture that made a real directory.
fn fixture_identity() -> FileIdentity {
    FileIdentity { dev: 1, ino: 1 }
}

/// A stale, unbound, unregistered candidate — everything a reclaim needs
/// except the holder verdict, which is what the caller is testing.
fn stale_candidate(holders: Option<HolderCheck>) -> OrphanCandidate {
    OrphanCandidate {
        path: PathBuf::from("/tmp/x-target"),
        identity: PathBuf::from("/private/tmp/x-target"),
        file_identity: Some(fixture_identity()),
        identity_pin: None,
        kind: ResourceKind::BuildTarget,
        staleness: Staleness::Stale { age_days: 30 },
        bytes: Some(2048),
        holders,
    }
}

fn reclaimed_row(path: &Path) -> ExecEnvResource {
    ExecEnvResource {
        resource_id: "res-tombstone".to_string(),
        kind: ResourceKind::BuildTarget,
        path: path.display().to_string(),
        bytes: Some(2048),
        measured_at: Some("2026-07-01T00:00:00Z".to_string()),
        state: ResourceState::Reclaimed,
        reclaim_reason: Some("unmanaged".to_string()),
        reclaimed_at: Some("2026-07-01T00:00:00Z".to_string()),
        reclaimed_bytes: Some(2048),
        created_at: "2026-07-01T00:00:00Z".to_string(),
        updated_at: "2026-07-01T00:00:00Z".to_string(),
    }
}

// ── protection sources: injected, never ambient ─────────────────────────
//
// There is no `EnvGuard` here any more, and no `env_lock` either. Both are gone
// for the same reason: the `set_var` / `remove_var` pair mutates the environment of
// the *process*, and cargo runs this module's tests as threads of one process. A
// test that unset `HOME` to prove the fail-closed path unset it for every test
// running beside it — which is precisely how `an_incomplete_forced_scan_does_not_
// exit_clean` (whose second half asserts a COMPLETE run) went red on a build seat
// while the four tests that own that behaviour all passed.
//
// A lock is not the fix; a lock is a promise every future test must remember to
// keep. The protected set's sources are an argument now ([`ProtectionSources`]), so
// a test that wants a missing `HOME` says so in its own stack frame and nobody else
// can tell. `no_test_mutates_the_process_environment` keeps it that way.

/// An empty process table: no build is running anywhere. The default for every test
/// that is not itself about live builds — and, unlike shelling out to the real `ps`,
/// the same answer on every machine.
fn no_live_builds() -> (Vec<PathBuf>, Vec<String>) {
    (Vec::new(), Vec::new())
}

static NO_LIVE_BUILDS: fn() -> (Vec<PathBuf>, Vec<String>) = no_live_builds;

/// Sources with every fence resolvable: a home that resolves, no target-dir override,
/// an empty process table. The baseline for every test whose subject is *not* the
/// protected set — it must be COMPLETE, or those tests would be asserting against a
/// run that is incomplete for reasons they never mention.
///
/// The home is a path no fixture lives under, so the only thing it changes about a run
/// is that the fence could be *computed* — which is the property these tests need and
/// the one the process's real `HOME` was accidentally providing.
fn resolved_sources() -> ProtectionSources<'static> {
    ProtectionSources {
        cargo_target_dir: None,
        shared_cargo_target_dir: None,
        home: Some(PathBuf::from("/nonexistent-home-for-tests")),
        live_builds: &NO_LIVE_BUILDS,
    }
}

impl<'a> ProtectionSources<'a> {
    fn with_cargo_target_dir(mut self, dir: &Path) -> Self {
        self.cargo_target_dir = Some(dir.to_path_buf());
        self
    }

    fn with_shared_cargo_target_dir(mut self, dir: &Path) -> Self {
        self.shared_cargo_target_dir = Some(dir.to_path_buf());
        self
    }

    /// The BUG 3 gap, staged in one test's own stack frame instead of in the
    /// process's environment.
    fn without_home(mut self) -> Self {
        self.home = None;
        self
    }

    /// Stand in for the process table — the source a build that starts *after* the
    /// scan actually arrives through.
    fn with_live_builds<'b>(self, scan: &'b LiveBuildScan) -> ProtectionSources<'b> {
        ProtectionSources {
            cargo_target_dir: self.cargo_target_dir,
            shared_cargo_target_dir: self.shared_cargo_target_dir,
            home: self.home,
            live_builds: scan,
        }
    }
}

/// The sealed entry point, on fully resolved sources.
fn reap_sealed(
    conn: &mut rusqlite::Connection,
    opts: &ReapOptions,
    now: SystemTime,
    probe: &HolderProbe,
) -> Result<ReapReport, DestructiveRefusal> {
    run_orphan_reap_with_sources_and_probe(conn, opts, &resolved_sources(), now, probe)
}

/// The sheathed body (the only way `force` reaches the delete path), on fully
/// resolved sources.
fn reap_uncertified(
    conn: &mut rusqlite::Connection,
    opts: &ReapOptions,
    now: SystemTime,
    probe: &HolderProbe,
) -> ReapReport {
    run_orphan_reap_uncertified(conn, opts, &resolved_sources(), now, probe)
}

// ── name matching ───────────────────────────────────────────────────────

#[test]
fn classifies_only_build_artifact_names() {
    assert_eq!(
        classify_orphan_dir_name("codex-bootstrap-target"),
        Some(ResourceKind::BuildTarget)
    );
    assert_eq!(
        classify_orphan_dir_name("sigil_shared_target"),
        Some(ResourceKind::BuildTarget)
    );
    assert_eq!(
        classify_orphan_dir_name("codex-cargo-home-abc"),
        Some(ResourceKind::ScratchDir)
    );
    // A bare `target` is NOT a candidate: too many live repo checkouts own
    // one, and a false positive here deletes gigabytes.
    assert_eq!(classify_orphan_dir_name("target"), None);
    assert_eq!(classify_orphan_dir_name("my-project"), None);
}

// ── protection (round-2: the near-miss) ─────────────────────────────────

/// **THE fence this round-2 exists for.**
///
/// The reviewed cut derived its protected set from
/// `default_shared_cargo_target_dir()`, which reads
/// `TACHI_SHARED_CARGO_TARGET_DIR` — a variable nothing in this repo sets —
/// while every build here exports the standard `CARGO_TARGET_DIR`. A shared
/// cache living anywhere but that helper's accidental default was
/// name-matched (`*-target`), unheld between builds, and one week idle away
/// from `tachi clean orphans --force` deleting it.
///
/// Discriminating: a genuinely dead target sits in the same root and MUST be
/// reaped by the same run, so this cannot pass by the reaper doing nothing.
/// The regression that `cargo_target_dir_is_never_a_reap_candidate` caught:
/// a scan root that *contains* a protected path (the real shape — `~/.cache`
/// is a default root and `~/.cache/sigil-shared-target` is protected) must
/// still be walked. The bidirectional `covers` test marks such a root as
/// protected, so asking it at walk time blinds the reaper completely: it
/// enumerates nothing and reclaims nothing, forever, while reporting success.
#[test]
fn a_scan_root_that_contains_a_protected_path_is_still_walked() {
    let root = unique_temp_dir("tachi-reaper-root-contains-protected");
    let live = make_target_dir(&root, "live-shared-target");
    let dead = make_target_dir(&root, "dead-target");

    let protection = protected_paths(&resolved_sources().with_cargo_target_dir(&live));
    assert!(
        protection.covers(&root),
        "the root DOES contain a protected path — that is the whole trap"
    );
    assert!(
        !protection.contains_dir(&root),
        "but the root is not INSIDE it, so the walk must proceed"
    );

    let candidates =
        scan_orphan_candidates(std::slice::from_ref(&root), &protection, aged_now(30), 7)
            .candidates;
    assert!(
        candidates.iter().any(|c| c.path == dead),
        "the dead sibling must survive the walk: {candidates:?}"
    );
    assert!(
        !candidates.iter().any(|c| c.path == live),
        "and the live target must never be a candidate"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cargo_target_dir_is_never_a_reap_candidate() {
    let root = unique_temp_dir("tachi-reaper-cargo-target-dir");
    let live = make_target_dir(&root, "live-shared-target");
    let dead = make_target_dir(&root, "dead-target");
    let mut store = open_store(&root);
    let sources = resolved_sources().with_cargo_target_dir(&live);

    let protection = protected_paths(&sources);
    assert!(
        protection.covers(&live),
        "the dir CARGO_TARGET_DIR points at must be protected: {:?}",
        protection.paths()
    );
    assert!(
        protection.covers(&live.join("debug/deps")),
        "and everything under it"
    );

    let report = run_orphan_reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        &sources,
        aged_now(30),
        &*unheld_probe(),
    );

    assert!(
        !report
            .candidates
            .iter()
            .any(|candidate| candidate.path == live.display().to_string()),
        "the live build cache must never even be a candidate: {report:?}"
    );
    assert!(
        live.join("debug/artifact.rlib").exists(),
        "the live build cache must survive --force"
    );

    // Discrimination: the dead target beside it IS reaped by the same run.
    assert!(
        !dead.exists(),
        "a genuinely dead target must still be reaped: {report:?}"
    );
    assert_eq!(report.reclaimed.len(), 1, "{report:?}");
    assert_eq!(report.reclaimed[0].path, dead.display().to_string());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tachi_shared_target_env_is_protected_too() {
    let root = unique_temp_dir("tachi-reaper-shared-env");
    let shared = make_target_dir(&root, "managed-shared-target");

    assert!(
        protected_paths(&resolved_sources().with_shared_cargo_target_dir(&shared)).covers(&shared),
        "both target-dir variables are read, not just one"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **The wiring test.** Every other test in this module hands the reaper injected
/// sources; without this one, `ProtectionSources::from_process_env` — the thing the
/// *binary* actually runs on — could silently start reading the wrong variables (or
/// none) and the whole suite would stay green.
///
/// It only READS the environment. It never sets or removes anything, so it is safe
/// beside every other test in the process, which is the entire point of the change it
/// guards.
#[test]
fn the_production_sources_are_read_from_the_process_environment() {
    let sources = ProtectionSources::from_process_env();
    let live = |key: &str| {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };

    assert_eq!(sources.cargo_target_dir, live(CARGO_TARGET_DIR_ENV));
    assert_eq!(
        sources.shared_cargo_target_dir,
        live(SHARED_CARGO_TARGET_DIR_ENV)
    );
    assert_eq!(
        sources.home,
        live("HOME").or_else(|| live("USERPROFILE")),
        "the default cache is resolved from HOME, falling back to USERPROFILE"
    );

    // And the value really reaches the fence: the documented default cache under the
    // home this process was handed is protected.
    if let Some(home) = &sources.home {
        let protection = protected_paths(&sources);
        assert!(
            protection.covers(&home.join(".cache").join("sigil-shared-target")),
            "the default shared cargo target dir must be protected: {:?}",
            protection.paths()
        );
    }
}

#[test]
fn protection_covers_ancestors_and_descendants() {
    // `remove_dir_all` on an ancestor takes the protected directory with it,
    // so an ancestor is exactly as untouchable as a descendant.
    let protection = Protection::new([PathBuf::from("/x/y-target/inner")], Vec::new());
    assert!(protection.covers(Path::new("/x/y-target/inner")));
    assert!(protection.covers(Path::new("/x/y-target/inner/deps")));
    assert!(protection.covers(Path::new("/x/y-target")));
    assert!(!protection.covers(Path::new("/x/other-target")));
}

#[test]
fn live_build_target_dirs_are_read_from_the_process_table() {
    assert_eq!(
        target_dirs_from_process_line("cargo build --target-dir /a/b-target --release"),
        vec![PathBuf::from("/a/b-target")]
    );
    assert_eq!(
        target_dirs_from_process_line("rustc --target-dir=/c/d-target foo.rs"),
        vec![PathBuf::from("/c/d-target")]
    );
    assert_eq!(
        target_dirs_from_process_line("env CARGO_TARGET_DIR=/e/f-target cargo test"),
        vec![PathBuf::from("/e/f-target")]
    );
    assert!(target_dirs_from_process_line("vim src/main.rs").is_empty());
}

// ── holder check (fail-closed) ──────────────────────────────────────────

#[test]
fn lsof_data_lines_mean_held() {
    let stdout = "COMMAND   PID USER   FD   TYPE DEVICE  SIZE/OFF NODE NAME\n\
                  cargo   4242 kyle  cwd    DIR   1,16       320  123 /tmp/x-target\n";
    assert_eq!(
        interpret_lsof(Some(0), stdout, ""),
        HolderCheck::Held(vec!["cargo 4242".to_string()])
    );
}

#[test]
fn lsof_excludes_only_the_reapers_exact_pin() {
    let stdout = "COMMAND   PID USER   FD   TYPE DEVICE  SIZE/OFF NODE NAME\n\
                  tachi    123 user    7r   DIR   1,16       320  123 /tmp/x-target\n\
                  tachi    123 user    8r   REG   1,16      2048  124 /tmp/x-target/live\n\
                  cargo    456 user    7r   REG   1,16      2048  125 /tmp/x-target/other\n";
    let filtered = without_ignored_holder(stdout, HolderExclusion { pid: 123, fd: 7 });

    assert!(!filtered.contains("123 user    7r"));
    assert!(filtered.contains("123 user    8r"));
    assert!(filtered.contains("456 user    7r"));
    assert_eq!(
        interpret_lsof(Some(1), &filtered, ""),
        HolderCheck::Held(vec!["tachi 123".to_string(), "cargo 456".to_string()])
    );
}

#[test]
fn lsof_clean_empty_run_means_unheld() {
    assert_eq!(interpret_lsof(Some(1), "", ""), HolderCheck::None);
    assert_eq!(interpret_lsof(Some(0), "", ""), HolderCheck::None);
}

#[test]
fn lsof_stderr_noise_is_unknown_not_unheld() {
    // A partial walk that "found nothing" proves nothing — fail closed.
    let check = interpret_lsof(
        Some(1),
        "",
        "lsof: WARNING: can't stat() /tmp/x-target/deps\n",
    );
    assert!(
        matches!(check, HolderCheck::Unknown(_)),
        "partial lsof walk must be Unknown, got {check:?}"
    );
}

#[test]
fn lsof_odd_exit_or_signal_is_unknown() {
    assert!(matches!(
        interpret_lsof(Some(9), "", ""),
        HolderCheck::Unknown(_)
    ));
    assert!(matches!(
        interpret_lsof(None, "", ""),
        HolderCheck::Unknown(_)
    ));
}

/// The head-line safety invariant of this module — and the one the reviewed
/// cut asserted only in its commit message. An *inconclusive* holder probe
/// must SKIP. The worktree sweep's `_ => false` (unknown ⇒ "not active") is
/// exactly the fail-open this must never become.
#[test]
fn inconclusive_holder_check_never_reclaims() {
    let candidate = stale_candidate(Some(HolderCheck::Unknown(
        "cannot run lsof: No such file or directory".to_string(),
    )));
    assert_eq!(
        decide_reap(&candidate, 7, None, 0),
        ReapDecision::Skip(SkipReason::HolderCheckInconclusive(
            "cannot run lsof: No such file or directory".to_string()
        )),
        "Unknown must skip, never reclaim"
    );
}

/// Fail-closed by *type*: a candidate whose probe never ran is skipped for
/// the same reason an Unknown one is. Nothing is deleted on the strength of
/// an unasked question.
#[test]
fn an_unprobed_candidate_never_reclaims() {
    let candidate = stale_candidate(None);
    assert!(
        matches!(
            decide_reap(&candidate, 7, None, 0),
            ReapDecision::Skip(SkipReason::HolderCheckInconclusive(_))
        ),
        "an unprobed candidate must skip"
    );
}

/// End-to-end: `lsof` cannot answer, and a `--force` run deletes nothing and
/// books nothing.
#[test]
fn inconclusive_holder_probe_survives_a_force_run() {
    let root = unique_temp_dir("tachi-reaper-unknown-holder");
    let dead = make_target_dir(&root, "would-be-dead-target");
    let mut store = open_store(&root);
    let unknown_probe = |_path: &Path, _ignored_holder| {
        HolderCheck::Unknown("cannot run lsof: No such file or directory".to_string())
    };

    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &unknown_probe,
    );

    assert!(
        report.reclaimed.is_empty(),
        "unprovable ⇒ never reclaimed: {report:?}"
    );
    assert_eq!(report.reclaimed_bytes, 0);
    assert!(
        dead.join("debug/artifact.rlib").exists(),
        "the bytes survive an inconclusive probe"
    );
    assert!(
        memcore::list_resources(store.connection(), None, None)
            .unwrap()
            .is_empty(),
        "and nothing is booked either"
    );
    assert_eq!(report.candidates[0].decision, "skip");
    assert!(
        report.candidates[0].reason.contains("inconclusive"),
        "reason: {}",
        report.candidates[0].reason
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn real_probe_never_reports_none_for_a_dir_with_an_open_file() {
    // Discrimination against the REAL lsof call site: hold a file open
    // under the candidate and assert the probe does not claim "nothing
    // open". Held (lsof present) and Unknown (lsof missing/partial) both
    // block a reclaim; None would be the fail-open bug.
    let root = unique_temp_dir("tachi-reaper-real-lsof");
    let target = make_target_dir(&root, "live-target");
    let _handle = std::fs::File::open(target.join("debug/artifact.rlib")).unwrap();

    let check = lsof_holder_probe(&target, None);
    assert_ne!(
        check,
        HolderCheck::None,
        "an open file handle under the dir must never read as unheld: {check:?}"
    );
    let candidate = OrphanCandidate {
        identity: std::fs::canonicalize(&target).unwrap(),
        file_identity: FileIdentity::of(&target),
        identity_pin: PinnedDirectory::open(&target),
        path: target.clone(),
        kind: ResourceKind::BuildTarget,
        staleness: Staleness::Stale { age_days: 30 },
        bytes: Some(2048),
        holders: Some(check),
    };
    assert!(matches!(
        decide_reap(&candidate, 7, None, 0),
        ReapDecision::Skip(_)
    ));

    let _ = std::fs::remove_dir_all(&root);
}

// ── scan: cheap gates first ─────────────────────────────────────────────

#[test]
fn scan_selects_stale_named_dirs_and_ignores_the_rest() {
    let root = unique_temp_dir("tachi-reaper-scan");
    let dead = make_target_dir(&root, "codex-bootstrap-target");
    let _plain = make_target_dir(&root, "some-checkout"); // name does not match
    let cargo_home = root.join("nested/codex-cargo-home-1");
    std::fs::create_dir_all(&cargo_home).unwrap();

    let candidates = scan_orphan_candidates(
        std::slice::from_ref(&root),
        &Protection::default(),
        aged_now(30),
        7,
    )
    .candidates;
    let paths: Vec<_> = candidates.iter().map(|c| c.path.clone()).collect();

    assert!(paths.contains(&dead), "stale *-target must be a candidate");
    assert!(
        paths.contains(&cargo_home),
        "*cargo-home* must be a candidate"
    );
    assert_eq!(candidates.len(), 2, "nothing else may be a candidate");

    let dead_candidate = candidates.iter().find(|c| c.path == dead).unwrap();
    assert_eq!(dead_candidate.kind, ResourceKind::BuildTarget);
    assert_eq!(
        dead_candidate.staleness,
        Staleness::Stale { age_days: 30 },
        "staleness is resolved during the scan (it is cheap)"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Cheap gates first: the reviewed cut called `dir_size` (a full recursive
/// walk) and `lsof +D` (another one) on EVERY name-matched directory, and
/// only then asked whether the thing was a day old. On this machine that is
/// minutes of stat storm across a 61 GB tree to decide nothing.
#[test]
fn the_scan_neither_measures_nor_probes() {
    let root = unique_temp_dir("tachi-reaper-cheap-scan");
    make_target_dir(&root, "some-target");

    let candidates = scan_orphan_candidates(
        std::slice::from_ref(&root),
        &Protection::default(),
        aged_now(30),
        7,
    )
    .candidates;

    assert_eq!(candidates.len(), 1);
    assert!(
        candidates[0].bytes.is_none(),
        "the scan must not measure bytes"
    );
    assert!(
        candidates[0].holders.is_none(),
        "the scan must not probe holders"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_candidate_a_cheap_gate_skips_is_never_probed_or_measured() {
    let root = unique_temp_dir("tachi-reaper-young-unprobed");
    let young = make_target_dir(&root, "fresh-target");
    let mut store = open_store(&root);

    // `HolderProbe` is `'static`, so the counter must be shared into the
    // closure rather than borrowed — the assertion below still needs to read
    // it after the reap has run.
    let probes = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counter = std::rc::Rc::clone(&probes);
    let counting_probe = move |_path: &Path, _ignored_holder| {
        counter.set(counter.get() + 1);
        HolderCheck::None
    };

    // Real `now`: the fixture was created a moment ago, so the age gate skips
    // it — and the expensive probes must never have run.
    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        SystemTime::now(),
        &counting_probe,
    );

    assert_eq!(
        probes.get(),
        0,
        "the holder probe must not run for a candidate a cheap gate already skipped"
    );
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].decision, "skip");
    assert!(
        report.candidates[0].bytes.is_none(),
        "nor may its bytes be measured"
    );
    assert!(young.join("debug/artifact.rlib").exists());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scan_skips_protected_shared_target() {
    let root = unique_temp_dir("tachi-reaper-protected");
    let shared = make_target_dir(&root, "sigil-shared-target");

    let scan = scan_orphan_candidates(
        std::slice::from_ref(&root),
        &Protection::new([shared.clone()], Vec::new()),
        aged_now(30),
        7,
    );
    assert!(
        scan.candidates.is_empty(),
        "the shared cargo target is protected: {:?}",
        scan.candidates
    );
    // …and the prune is a SAFETY REFUSAL on the books, not a `continue`.
    assert_eq!(scan.accounting.protected_pruned, 1, "{:?}", scan.accounting);
    assert!(scan
        .skips
        .iter()
        .any(|skip| skip.path == shared && skip.outcome == UnitOutcome::ProtectedPruned));

    let _ = std::fs::remove_dir_all(&root);
}

// ── staleness: recursive, as the doc always claimed ─────────────────────

/// The module header promised "nothing *under* it has been touched"; the
/// reviewed cut only looked at the root and its immediate children. A target
/// whose only fresh bytes are three levels down — `debug/deps/*.o`, which is
/// exactly where a build writes — read as stale and was eligible for delete.
#[cfg(unix)]
#[test]
fn a_deep_fresh_file_keeps_the_whole_tree_fresh() {
    let root = unique_temp_dir("tachi-reaper-deep-mtime");
    let target = root.join("deep-target");
    std::fs::create_dir_all(target.join("debug/deps")).unwrap();
    std::fs::write(target.join("debug/deps/live.o"), vec![1u8; 16]).unwrap();

    let now = SystemTime::now();
    let long_ago = now - Duration::from_secs(30 * SECS_PER_DAY);
    // Everything the old depth-1 walk could see is ancient…
    set_mtime(&target, long_ago);
    set_mtime(&target.join("debug"), long_ago);
    // …and the only fresh thing is at depth 3, where cargo actually writes.

    let candidates =
        scan_orphan_candidates(std::slice::from_ref(&root), &Protection::default(), now, 7)
            .candidates;

    assert_eq!(candidates.len(), 1);
    assert!(
        matches!(candidates[0].staleness, Staleness::Fresh { .. }),
        "a live file at depth 3 must keep the tree fresh: {:?}",
        candidates[0].staleness
    );
    assert!(matches!(
        decide_reap(&candidates[0], 7, None, 0),
        ReapDecision::Skip(SkipReason::TooYoung { .. })
    ));

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
fn set_mtime(path: &Path, when: SystemTime) {
    // A directory cannot be opened for writing, but futimens(2) on a
    // read-only fd is enough to set times on something you own.
    let file = std::fs::File::open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(when))
        .unwrap();
}

// ── decision ────────────────────────────────────────────────────────────

#[test]
fn young_dir_is_never_reclaimed() {
    let candidate = OrphanCandidate {
        path: PathBuf::from("/tmp/x-target"),
        identity: PathBuf::from("/private/tmp/x-target"),
        file_identity: Some(fixture_identity()),
        identity_pin: None,
        kind: ResourceKind::BuildTarget,
        staleness: Staleness::Fresh { age_days: 2 },
        bytes: None,
        holders: None,
    };
    assert_eq!(
        decide_reap(&candidate, 7, None, 0),
        ReapDecision::Skip(SkipReason::TooYoung {
            age_days: 2,
            max_age_days: 7
        })
    );
}

#[test]
fn an_unprovable_age_is_never_reclaimed() {
    let candidate = OrphanCandidate {
        path: PathBuf::from("/tmp/x-target"),
        identity: PathBuf::from("/private/tmp/x-target"),
        file_identity: Some(fixture_identity()),
        identity_pin: None,
        kind: ResourceKind::BuildTarget,
        staleness: Staleness::Unprovable("cannot read /tmp/x-target/deps".to_string()),
        bytes: None,
        holders: Some(HolderCheck::None),
    };
    assert!(matches!(
        decide_reap(&candidate, 7, None, 0),
        ReapDecision::Skip(SkipReason::StalenessUnprovable(_))
    ));
}

/// Fail-closed by *type*, mirroring `an_unprobed_candidate_never_reclaims`
/// exactly: a candidate whose scan never captured a `(dev, ino)` identity is
/// skipped for it, no matter how eligible everything else about it looks —
/// stale, unheld, unbound. Nothing is deleted against an object the run
/// cannot re-identify at the delete (BUG 2).
#[test]
fn a_candidate_with_no_captured_identity_never_reclaims() {
    let mut candidate = stale_candidate(Some(HolderCheck::None));
    candidate.file_identity = None;
    assert_eq!(
        decide_reap(&candidate, 7, None, 0),
        ReapDecision::Skip(SkipReason::IdentityUnprovable),
        "no captured identity must skip, never reclaim, even though holders/staleness/ledger \
         all say yes"
    );
}

#[test]
fn unmanaged_stranger_is_eligible_as_unmanaged() {
    let candidate = stale_candidate(Some(HolderCheck::None));
    assert_eq!(
        decide_reap(&candidate, 7, None, 0),
        ReapDecision::Reclaim(ReclaimReason::Unmanaged)
    );
}

/// A `reclaimed` row is a tombstone, not a verdict. The reviewed cut answered
/// `Skip(AlreadyReclaimed)`, which — with S2a's `UNIQUE(path, kind)` and a
/// path-keyed lookup that does not filter by state — retired the reaper from
/// every path it had ever cleaned once. That is exactly the population it
/// exists for: a lane's target dir is reborn under the same name on every run.
#[test]
fn a_reclaimed_row_is_revived_not_skipped_forever() {
    let candidate = stale_candidate(Some(HolderCheck::None));
    let tombstone = reclaimed_row(&candidate.path);
    assert_eq!(
        decide_reap(&candidate, 7, Some(&tombstone), 0),
        ReapDecision::Reclaim(ReclaimReason::Unmanaged),
        "a path back on disk after a reclaim must be reapable again"
    );
}

#[test]
fn a_quarantined_row_is_still_fenced() {
    let candidate = stale_candidate(Some(HolderCheck::None));
    let mut fenced = reclaimed_row(&candidate.path);
    fenced.state = ResourceState::Quarantined;
    assert_eq!(
        decide_reap(&candidate, 7, Some(&fenced), 0),
        ReapDecision::Skip(SkipReason::Quarantined)
    );
}

// ── run: fixtures through the real ledger ───────────────────────────────
//
// NOTE ON `run_orphan_reap_uncertified`. The destructive path is refused at the entry
// point (`certify_destructive`), so the tests below that exercise a *delete* call the
// sheathed driver directly. They are not testing something a user can reach — they
// are keeping the delete path's fences honest for the knife that will re-enable it.
// The tests that pin the SHIPPING behaviour (the report, and the refusal itself) go
// through `run_orphan_reap`, like the CLI does.

#[test]
fn dry_run_deletes_nothing_and_books_nothing() {
    let root = unique_temp_dir("tachi-reaper-dryrun");
    let dead = make_target_dir(&root, "dead-target");
    let mut store = open_store(&root);

    // The SEALED entry point: this is the path the CLI takes.
    let report = reap_sealed(
        store.connection_mut(),
        &opts(&root, false),
        aged_now(30),
        &*unheld_probe(),
    )
    .expect("a report-only run is never refused");

    assert!(report.dry_run);
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].decision, "reclaim");
    assert!(report.reclaimed.is_empty(), "preview must not reclaim");
    assert_eq!(report.reclaimed_bytes, 0);
    // The dry run's whole product: the dead bytes, counted and located.
    assert_eq!(
        report.reclaimable_bytes,
        i64::try_from(dir_size(&dead)).unwrap(),
        "the report must say how many bytes are dead: {report:?}"
    );
    assert!(report.reclaimable_bytes > 0);
    // #1062: the knife was certified on 2026-07-17 (receipt checked in,
    // owner-ratified 1A, 2026-07-17), but `tachi#1379` re-sheathed it on 2026-07-23
    // when an inode-reuse race showed BUG 2's (dev, ino) fence is beatable by the
    // very scenario that receipt never staged. This test's core property is
    // unchanged regardless: a report-only request (`force: false`) deletes nothing
    // and books nothing. Was `assert!(!report.destructive_certified)` pre-#1062,
    // `assert!(report.destructive_certified)` while the receipt was valid, and now
    // `assert!(!report.destructive_certified)` again after #1379 revoked it.
    assert!(!report.destructive_certified);
    assert!(!report.blocking_defects.is_empty());
    // The bytes are still on disk...
    assert!(dead.join("debug/artifact.rlib").exists());
    // ...and nothing was written to the ledger.
    assert!(memcore::list_resources(store.connection(), None, None)
        .unwrap()
        .is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unmanaged_orphan_is_booked_then_reclaimed_with_real_bytes() {
    let root = unique_temp_dir("tachi-reaper-unmanaged");
    let dead = make_target_dir(&root, "codex-bootstrap-target");
    let mut store = open_store(&root);

    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &*unheld_probe(),
    );

    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert_eq!(report.reclaimed.len(), 1, "report: {report:?}");
    assert_eq!(report.reclaimed[0].reason, "unmanaged");
    assert!(report.reclaimed_bytes >= 2048);
    // The bytes are actually gone — `reclaimed` means freed, not flipped.
    assert!(!dead.exists(), "the orphan directory must be deleted");

    // And the stranger is on the books, with what it actually gave back.
    let rows = memcore::list_resources(store.connection(), None, None).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.path, dead.display().to_string());
    assert_eq!(row.kind, ResourceKind::BuildTarget);
    assert_eq!(row.state, ResourceState::Reclaimed);
    assert_eq!(row.reclaim_reason.as_deref(), Some("unmanaged"));
    assert!(
        row.reclaimed_bytes.unwrap_or(0) >= 2048,
        "reclaimed_bytes must record the freed bytes: {row:?}"
    );

    assert_eq!(
        report.bytes_by_reason.get("unmanaged").copied(),
        row.reclaimed_bytes,
        "byte report groups by reclaim_reason"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// End-to-end proof of the revive: the same path is reaped, reborn, and
/// reaped again — through S2a's `UNIQUE(path, kind)`, which the first cut
/// could only ever hit once.
#[test]
fn a_reborn_target_at_a_reaped_path_is_reaped_again() {
    let root = unique_temp_dir("tachi-reaper-reborn");
    let dead = make_target_dir(&root, "lane-target");
    let mut store = open_store(&root);

    let first = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &*unheld_probe(),
    );
    assert_eq!(first.reclaimed.len(), 1, "{first:?}");
    assert!(!dead.exists());

    // The lane runs again, rebuilds the same target, and dies again.
    let reborn = make_target_dir(&root, "lane-target");
    let second = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &*unheld_probe(),
    );

    assert_eq!(
        second.reclaimed.len(),
        1,
        "a reborn target must be reapable again: {second:?}"
    );
    assert!(
        !reborn.exists(),
        "the second incarnation's bytes must be freed too"
    );

    // Still exactly one row for the path: the reclaimed row was revived, not
    // duplicated — `(path, kind)` is UNIQUE and splitting it would split the
    // refcount that protects a live worktree.
    let rows = memcore::list_resources(store.connection(), None, None).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].state, ResourceState::Reclaimed);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn resource_with_a_live_binding_is_never_reclaimed() {
    let root = unique_temp_dir("tachi-reaper-bound");
    let bound = make_target_dir(&root, "shared-build-target");
    let mut store = open_store(&root);

    // A lease holding this target. `BuildPrivate` is the one S2c class that owns a
    // build target dir of its own — `EditOnly` gets none and `BuildTicketed` builds
    // in the executor seat's — so it is the only class this fixture can honestly be.
    memcore::insert_exec_env(
        store.connection(),
        &memcore::NewExecEnvLease {
            env_id: "env-holder".to_string(),
            kind: "worktree".to_string(),
            path: root.join("wt").display().to_string(),
            repo_root: "/repo".to_string(),
            branch: "tachi/894/s2b".to_string(),
            base_sha: "abc123".to_string(),
            dispatch_id: None,
            env_class: memcore::EnvClass::BuildPrivate,
            created_at: String::new(),
        },
    )
    .unwrap();
    memcore::insert_resource(
        store.connection_mut(),
        &NewExecEnvResource {
            resource_id: "res-bound".to_string(),
            kind: ResourceKind::BuildTarget,
            path: bound.display().to_string(),
            bytes: None,
            created_at: String::new(),
        },
    )
    .unwrap();
    memcore::bind_resource(store.connection_mut(), "env-holder", "res-bound").unwrap();

    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &*unheld_probe(),
    );

    assert!(report.reclaimed.is_empty(), "a bound resource must survive");
    assert!(bound.join("debug/artifact.rlib").exists(), "bytes survive");

    // **#1062, BUG 1: this fence moved UPSTREAM.** Before #1062 the only source
    // that knew about a binding was `cheap_verdict`, reached after the walk had
    // already turned this directory into a candidate — so the old shape of this
    // assertion was "a candidate, skipped for `BoundByLease`". Now
    // `full_protected_paths` folds every live binding into the protected set
    // BEFORE the walk ever gets here, the same rule an env-var-declared build
    // cache already got: it is pruned at the walk, never becomes a candidate at
    // all, and shows up as a protected path plus a `safety-refusal` scan unit.
    assert!(
        report.protected.contains(&bound.display().to_string()),
        "the bound target must be in the protected set: {:?}",
        report.protected
    );
    assert!(
        !report
            .candidates
            .iter()
            .any(|c| c.path == bound.display().to_string()),
        "a walk-level fence prunes it before candidacy; it must not also appear as a \
         candidate: {report:?}"
    );
    let pruned = report
        .unexamined
        .iter()
        .find(|skip| skip.path == bound.display().to_string())
        .expect("bound target is a pruned scan unit");
    assert_eq!(pruned.class, "safety-refusal", "{pruned:?}");

    assert_eq!(
        memcore::get_resource(store.connection(), "res-bound")
            .unwrap()
            .unwrap()
            .state,
        ResourceState::Active
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn held_directory_is_never_reclaimed() {
    let root = unique_temp_dir("tachi-reaper-held");
    let held = make_target_dir(&root, "busy-target");
    let mut store = open_store(&root);

    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &*held_probe(),
    );

    assert!(report.reclaimed.is_empty());
    assert!(held.join("debug/artifact.rlib").exists(), "bytes survive");
    assert!(memcore::list_resources(store.connection(), None, None)
        .unwrap()
        .is_empty());
    assert_eq!(report.candidates[0].decision, "skip");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn holder_appearing_after_the_scan_aborts_the_delete() {
    // The scan says unheld; by the time the deleter runs, a process holds
    // it. The bytes must survive and the row must land `reclaim_failed`
    // (retryable), never `reclaimed` (which would claim bytes it did not
    // free).
    let root = unique_temp_dir("tachi-reaper-toctou");
    let target = make_target_dir(&root, "racy-target");
    let mut store = open_store(&root);

    let calls = std::cell::Cell::new(0usize);
    let probe = move |_path: &Path, _ignored_holder| {
        let n = calls.get();
        calls.set(n + 1);
        if n == 0 {
            HolderCheck::None // scan
        } else {
            HolderCheck::Held(vec!["cargo 1".to_string()]) // pre-delete recheck
        }
    };

    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &probe,
    );

    assert!(report.reclaimed.is_empty());
    assert!(target.join("debug/artifact.rlib").exists(), "bytes survive");
    assert!(!report.warnings.is_empty(), "the abort is reported");
    let rows = memcore::list_resources(store.connection(), None, None).unwrap();
    let row = rows
        .iter()
        .find(|row| row.path == target.display().to_string())
        .expect("the booked row is there");
    assert_eq!(row.state, ResourceState::ReclaimFailed);
    assert!(row.reclaimed_bytes.is_none(), "no bytes may be claimed");

    let _ = std::fs::remove_dir_all(&root);
}

/// Defense in depth: even a row booked by some other writer — one that never
/// went through our scan — cannot be deleted if it names a protected path.
/// The fence is re-asserted at the line that actually calls `remove_dir_all`.
#[test]
fn the_deleter_refuses_a_protected_path() {
    let root = unique_temp_dir("tachi-reaper-delete-fence");
    let shared = make_target_dir(&root, "sigil-shared-target");
    let resource = ExecEnvResource {
        resource_id: "res-shared".to_string(),
        kind: ResourceKind::BuildTarget,
        path: shared.display().to_string(),
        bytes: Some(2048),
        measured_at: None,
        state: ResourceState::Reclaiming,
        reclaim_reason: Some("orphan".to_string()),
        reclaimed_at: None,
        reclaimed_bytes: None,
        created_at: String::new(),
        updated_at: String::new(),
    };

    let err = delete_resource_bytes(
        &resource,
        &*unheld_probe(),
        &Protection::new([shared.clone()], Vec::new()),
        // Both identity checks would pass — the fence under test is the protected
        // set, re-asserted at the line that deletes.
        &std::fs::canonicalize(&shared).unwrap(),
        FileIdentity::of(&shared),
        PinnedDirectory::open(&shared).as_ref(),
    )
    .expect_err("a protected path must never be deleted");
    assert!(
        err.to_string().contains("protected"),
        "error should name the fence: {err}"
    );
    assert!(shared.join("debug/artifact.rlib").exists(), "bytes survive");

    let _ = std::fs::remove_dir_all(&root);
}

// ── sol audit · BUG 1: the protected set is recomputed AT the delete ─────

/// **The accident this fence exists for.** The run took ONE snapshot of the
/// protected set at the top and never took another; the deleter re-probed only
/// for holder *file descriptors*. So: a build claims a target dir after the scan has
/// looked, and the delete lands in the gap between two compile units, when that build
/// holds no fd anywhere under the tree. Stale snapshot says "not protected", fd probe
/// says "nobody home", and a live build cache is deleted.
///
/// The probe here *is* the claim: it fires between the scan and the delete and
/// still answers `None`, so nothing but a freshly recomputed protected set can
/// save the bytes.
///
/// The claim arrives through the **process table** — a `cargo` that was not running
/// when the scan looked and is running now. That is where a real late claim arrives:
/// another process cannot reach into *this* process's `CARGO_TARGET_DIR`, so the old
/// version of this test (which staged the claim by calling `set_var` on our own
/// environment, from inside the probe) was simulating something that cannot happen —
/// and poisoning every test running beside it while it did.
#[test]
fn a_target_claimed_after_the_scan_is_refused_at_delete_time() {
    let root = unique_temp_dir("tachi-reaper-late-claim");
    let contested = make_target_dir(&root, "contested-target");
    let mut store = open_store(&root);

    // The process table is empty while the scan looks — the candidate must be
    // genuinely eligible — and names the contested dir from the moment the holder
    // probe fires, i.e. after the run's opening snapshot was taken.
    let claimed = std::rc::Rc::new(std::cell::Cell::new(false));
    let claiming_probe = {
        let claimed = std::rc::Rc::clone(&claimed);
        move |_path: &Path, _ignored_holder| {
            claimed.set(true);
            // …and it holds nothing open right now: the fd-only recheck is blind to it.
            HolderCheck::None
        }
    };
    let process_table = {
        let claimed = std::rc::Rc::clone(&claimed);
        let contested = contested.clone();
        move || {
            if claimed.get() {
                (vec![contested.clone()], Vec::new())
            } else {
                (Vec::new(), Vec::new())
            }
        }
    };

    let report = run_orphan_reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        &resolved_sources().with_live_builds(&process_table),
        aged_now(30),
        &claiming_probe,
    );

    assert!(
        contested.join("debug/artifact.rlib").exists(),
        "a target dir claimed after the scan must survive --force: {report:?}"
    );
    assert!(report.reclaimed.is_empty(), "{report:?}");
    assert_eq!(report.reclaimed_bytes, 0);
    assert_eq!(report.candidates.len(), 1, "{report:?}");
    assert_eq!(
        report.candidates[0].decision, "refused",
        "the candidate's TERMINAL state is refused, not reclaim: {report:?}"
    );
    // Discriminating: `refused` (not `skip`) is only reachable from the delete
    // path, so this candidate really did pass every gate the run's opening
    // snapshot had — it was one stale snapshot away from being deleted.
    assert!(
        report.candidates[0]
            .reason
            .contains("protected as of the delete"),
        "reason: {}",
        report.candidates[0].reason
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("protected as of the delete")),
        "the late refusal is reported: {report:?}"
    );
    assert!(
        memcore::list_resources(store.connection(), None, None)
            .unwrap()
            .is_empty(),
        "a path refused at delete time is not booked either"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **BUG 1, closed: the ledger is now an authoritative holder-discovery
/// source, not merely `ps` argv.** A `BuildPrivate` lease's target dir taken
/// from `CARGO_TARGET_DIR` (every build seat in this repo) never appears on
/// any command line — which is precisely why `ps` alone could not see it.
/// The process table here is EMPTY for the whole run (no argv, ever, names
/// the live target), and the fixture still survives, because a lease bound
/// it on the ledger surface S2c ships.
///
/// Discriminating: a sibling fixture, tracked in the ledger but never bound
/// (`res-untouched`-shaped, per `bound_resource_paths_reflects_only_live_bindings`
/// in memcore), is NOT protected by this source — this test's assertion that
/// the run does not even reach a `delete` attempt (it is a cheap, walk-time
/// prune) only holds because the binding is what the ledger source keys off.
#[test]
fn a_ledger_bound_target_survives_even_when_ps_cannot_see_it() {
    let root = unique_temp_dir("tachi-reaper-ledger-bound");
    let live = make_target_dir(&root, "env-var-only-target");
    let mut store = open_store(&root);

    memcore::insert_exec_env(
        store.connection(),
        &memcore::NewExecEnvLease {
            env_id: "env-live-build".to_string(),
            kind: "worktree".to_string(),
            path: "/wt/env-live-build".to_string(),
            repo_root: "/repo".to_string(),
            branch: "tachi/1062/w".to_string(),
            base_sha: "abc123".to_string(),
            dispatch_id: None,
            env_class: memcore::EnvClass::default(),
            created_at: String::new(),
        },
    )
    .unwrap();
    let register = memcore::insert_resource(
        store.connection_mut(),
        &NewExecEnvResource {
            resource_id: "res-live-build".to_string(),
            kind: ResourceKind::BuildTarget,
            path: live.display().to_string(),
            bytes: Some(2048),
            created_at: String::new(),
        },
    )
    .unwrap();
    let resource_id = match register {
        RegisterOutcome::Registered { resource_id } => resource_id,
        other => panic!("expected a fresh registration: {other:?}"),
    };
    memcore::bind_resource(store.connection_mut(), "env-live-build", &resource_id).unwrap();

    // The process table sees NOTHING for this entire run — the whole point.
    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &*unheld_probe(),
    );

    assert!(
        live.join("debug/artifact.rlib").exists(),
        "a target the ledger declares BOUND must survive even though `ps` never saw it: \
         {report:?}"
    );
    assert!(report.reclaimed.is_empty(), "{report:?}");
    // The ledger fence prunes this at the WALK, before it is ever a candidate at
    // all — see `resource_with_a_live_binding_is_never_reclaimed` for the same
    // shape, spelled out in full.
    assert!(
        report.protected.contains(&live.display().to_string()),
        "the ledger-bound target must be in the protected set: {:?}",
        report.protected
    );
    assert!(report.candidates.is_empty(), "{report:?}");
    let pruned = report
        .unexamined
        .iter()
        .find(|skip| skip.path == live.display().to_string())
        .expect("the ledger-bound target is a pruned scan unit");
    assert_eq!(pruned.class, "safety-refusal", "{pruned:?}");

    let _ = std::fs::remove_dir_all(&root);
}

// ── sol audit · BUG 2: the object judged is the object deleted ───────────

/// `--root` is caller-supplied and `is_dir()` follows symlinks; the protection
/// verdict and the `remove_dir_all` each resolved the *name* independently. So a
/// retarget of the link between the two makes the run delete a directory nothing
/// ever judged.
///
/// Discriminating: the decoy holds real bytes and is not protected by anything —
/// only the pinned identity stands between it and `remove_dir_all`.
#[cfg(unix)]
#[test]
fn a_root_symlink_retargeted_after_the_verdict_cannot_redirect_the_delete() {
    let base = unique_temp_dir("tachi-reaper-symlink-root");
    let judged_root = base.join("judged");
    let decoy_root = base.join("decoy");
    std::fs::create_dir_all(&judged_root).unwrap();
    std::fs::create_dir_all(&decoy_root).unwrap();
    // Same name under both roots: only the resolved identity tells them apart.
    let judged = make_target_dir(&judged_root, "lane-target");
    let decoy = make_target_dir(&decoy_root, "lane-target");

    let link = base.join("root-link");
    std::os::unix::fs::symlink(&judged_root, &link).unwrap();
    let mut store = open_store(&base);

    // `probe` is invoked twice per candidate (once to decide eligibility,
    // once again inside the deleter); this closure retargets on its FIRST
    // call, which happens during the eligibility check — see the assertions
    // below for exactly which fence that lands the refusal on.
    let link_for_probe = link.clone();
    let decoy_for_probe = decoy_root.clone();
    let retargeting_probe = move |_path: &Path, _ignored_holder| {
        std::fs::remove_file(&link_for_probe).unwrap();
        std::os::unix::fs::symlink(&decoy_for_probe, &link_for_probe).unwrap();
        HolderCheck::None
    };

    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&link, true),
        aged_now(30),
        &retargeting_probe,
    );

    assert!(
        decoy.join("debug/artifact.rlib").exists(),
        "the delete must not follow a link retargeted after the verdict: {report:?}"
    );
    assert!(
        judged.join("debug/artifact.rlib").exists(),
        "and a refusal deletes nothing at all — not even the object it judged: {report:?}"
    );
    assert!(report.reclaimed.is_empty(), "{report:?}");
    assert_eq!(report.candidates.len(), 1, "{report:?}");
    assert_eq!(report.candidates[0].decision, "refused", "{report:?}");
    // **Correction (fix-round, 2026-07-17): checkpoint 2's own claim about
    // this test was wrong.** `probe` is not called once, at the last possible
    // moment before `remove_dir_all` — it is called TWICE: once as one of
    // `run_orphan_reap_uncertified`'s "expensive checks" that decide whether a
    // candidate is even eligible (`candidate.holders = Some(probe(...))`,
    // BEFORE the `--force` branch is entered at all), and again inside
    // `delete_resource_bytes` itself. This closure's retarget is unconditional
    // on invocation, so it fires on the FIRST call — during that eligibility
    // check, well before `delete_resource_bytes` runs a single fence. By the
    // time the deleter's own `std::fs::canonicalize` re-resolves the pinned
    // root, the link is ALREADY retargeted, so it is THAT check — "it now
    // resolves to X but the verdict was rendered against Y" — that refuses the
    // delete here, not the `(dev, ino)` recheck checkpoint 2 added (which
    // exists for the different case where the canonical *spelling* survives
    // unchanged — see `a_directory_replaced_at_the_same_path_between_verdict_
    // and_delete_is_refused` for that one). The safety property this test
    // exists to prove (the decoy survives, nothing is deleted) still holds —
    // it was the inline claim about *which* fence catches it that was wrong.
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("verdict was rendered against")),
        "the refusal names the retargeted root: {report:?}"
    );
    assert!(
        !report.errors.is_empty(),
        "an identity that no longer resolves to the judged object must land in errors, not \
         just a warning: {report:?}"
    );
    assert!(report.incomplete, "{report:?}");

    let _ = std::fs::remove_dir_all(&base);
}

/// **The rest of BUG 2 (closed): a rename-and-replace, no symlink involved at
/// all.** The symlink test above catches a retargeted *link* — the canonical
/// spelling changes, and the pathname re-resolution alone is enough to see it.
/// This test is the hole THAT one leaves: `rm -rf` + `mkdir` at the exact same
/// path leaves the canonical spelling byte-for-byte identical (there is
/// nothing for `std::fs::canonicalize` to disagree about), so only a REAL
/// identity — `(dev, ino)`, captured at judgement and re-`stat`ed immediately
/// before the delete — can tell the judged directory from its replacement.
///
/// Discriminating: on the pre-#1062 pathname-only check, `canonicalize(path)
/// == pinned` is TRUE here (same string, before and after), so that fence
/// alone would wave this delete through. Only the `(dev, ino)` re-check
/// added by this fix refuses it.
#[cfg(unix)]
#[ignore = "issue #1261: assumes remove_dir_all+create_dir_all yields a fresh inode; on CI overlayfs/tmpfs inode reuse makes the (dev,ino) guard not fire. Sibling kill-test at line ~5339 is #[ignore]d for the same #1062 matrix. Run with --ignored"]
#[test]
fn a_directory_replaced_at_the_same_path_between_verdict_and_delete_is_refused() {
    let root = unique_temp_dir("tachi-reaper-inode-swap");
    let target = make_target_dir(&root, "swapped-target");
    let mut store = open_store(&root);

    // `probe` is invoked twice per candidate (once to decide eligibility,
    // once again inside the deleter); this closure swaps on its FIRST call,
    // during the eligibility check — either invocation's identity recheck
    // would catch it (see the assertion below).
    let target_for_probe = target.clone();
    let swapping_probe = move |_path: &Path, _ignored_holder| {
        std::fs::remove_dir_all(&target_for_probe).unwrap();
        std::fs::create_dir_all(target_for_probe.join("debug")).unwrap();
        std::fs::write(
            target_for_probe.join("debug/replacement.rlib"),
            vec![9u8; 4096],
        )
        .unwrap();
        HolderCheck::None
    };

    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &swapping_probe,
    );

    assert!(
        target.join("debug/replacement.rlib").exists(),
        "the replacement directory — a different object at the same path — must survive: \
         {report:?}"
    );
    assert!(report.reclaimed.is_empty(), "{report:?}");
    assert_eq!(report.candidates.len(), 1, "{report:?}");
    assert_eq!(report.candidates[0].decision, "refused", "{report:?}");
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("(dev, ino) identity")),
        "the refusal names the (dev, ino) mismatch, not just the pathname: {report:?}"
    );
    // **checkpoint 2 fix (codex-9178d) — correction, 2026-07-17: this
    // scenario is actually caught by the FIRST `(dev, ino)` check
    // (`delete_resource_bytes`'s pre-probe recheck), not the second one added
    // for checkpoint 2.** `probe` runs twice per candidate — once as one of
    // `run_orphan_reap_uncertified`'s own eligibility checks, before the
    // `--force` branch is even entered, and again inside
    // `delete_resource_bytes`. This closure's swap is unconditional on
    // invocation, so it fires on that FIRST call, well before the deleter's
    // own probe or its second recheck ever run. Either check would have
    // caught it (that is what checkpoint 2 hardened for the case where BOTH
    // pre-existing checks run before the swap); what matters for #1062 is
    // that this refusal must cost the run its clean exit exactly like any
    // other identity-unresolved unit (checkpoint 3): a fence that fires here
    // means the run does not know what is at this path anymore, not that a
    // designed fence worked cleanly.
    assert!(
        !report.errors.is_empty(),
        "an identity that changed a second time (mid-probe) must land in errors, not just a \
         warning: {report:?}"
    );
    assert!(report.incomplete, "{report:?}");

    let _ = std::fs::remove_dir_all(&root);
}

/// **tachi#1210: the checkpoint-2 fix (codex-9178d) itself has no discriminating
/// coverage.** The test above swaps on `probe`'s FIRST call — the eligibility check
/// at `run_orphan_reap_uncertified`'s `candidate.holders = Some(probe(...))`, which
/// runs before `delete_resource_bytes` is even entered — so it is caught by
/// `delete_resource_bytes`'s FIRST `(dev, ino)` recheck (right before its own
/// `probe(path)` call), never reaching the SECOND recheck
/// (`identity_at_unlink`, right before `remove_dir_all`) that checkpoint 2 added.
/// Neither existing test exercises a swap that survives past the deleter's own
/// probe call.
///
/// This test makes the swap fire on `probe`'s SECOND invocation instead — the one
/// `delete_resource_bytes` itself makes — so by the time this closure runs, the
/// eligibility check and the deleter's FIRST identity recheck have both already
/// passed against the original (unswapped) directory. The swap then lands in the
/// window the FIRST recheck cannot see: between the deleter's `probe(path)` call and
/// its `remove_dir_all`. Only the SECOND recheck — checkpoint 2's own addition — can
/// catch this.
#[cfg(unix)]
#[ignore = "issue #1261: same inode-reuse flake as the verdict-and-delete sibling above; CI overlayfs can hand back the same inode after remove_dir_all+create_dir_all. Run with --ignored"]
#[test]
fn a_directory_replaced_between_the_deleters_own_probe_and_unlink_is_refused() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let root = unique_temp_dir("tachi-reaper-post-probe-swap");
    let target = make_target_dir(&root, "swapped-target");
    let mut store = open_store(&root);

    // `probe` is invoked twice per candidate: once during
    // `run_orphan_reap_uncertified`'s eligibility check (BEFORE `delete_resource_bytes`
    // runs a single fence), and once again inside `delete_resource_bytes` itself
    // (its own defense-in-depth holder check, right before the byte walk and the
    // second `(dev, ino)` recheck). This closure counts invocations and only swaps
    // on the SECOND one, so the FIRST identity recheck inside `delete_resource_bytes`
    // (which runs before its own `probe` call) still sees the original, unswapped
    // directory and passes — leaving only the second recheck to catch this.
    let calls = Arc::new(AtomicUsize::new(0));
    let target_for_probe = target.clone();
    let swap_on_second_call = move |_path: &Path, _ignored_holder| {
        let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 2 {
            std::fs::remove_dir_all(&target_for_probe).unwrap();
            std::fs::create_dir_all(target_for_probe.join("debug")).unwrap();
            std::fs::write(
                target_for_probe.join("debug/replacement.rlib"),
                vec![9u8; 4096],
            )
            .unwrap();
        }
        HolderCheck::None
    };

    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &swap_on_second_call,
    );

    assert!(
        target.join("debug/replacement.rlib").exists(),
        "the replacement directory — swapped in after the deleter's own probe — must \
         survive: {report:?}"
    );
    assert!(report.reclaimed.is_empty(), "{report:?}");
    assert_eq!(report.candidates.len(), 1, "{report:?}");
    assert_eq!(report.candidates[0].decision, "refused", "{report:?}");
    assert!(
        report.warnings.iter().any(
            |warning| warning.contains("(dev, ino) identity changed again")
                && warning.contains("a second time")
        ),
        "the refusal must name the SECOND recheck's own language (\"changed again\" / \
         \"a second time\"), proving checkpoint 2's own recheck fired — not the first \
         recheck's \"replaced between judgement and delete\" wording: {report:?}"
    );
    assert!(
        !report.errors.is_empty(),
        "an identity that changed after the deleter's own probe must land in errors, not \
         just a warning: {report:?}"
    );
    assert!(report.incomplete, "{report:?}");

    let _ = std::fs::remove_dir_all(&root);
}

// ── sol audit · BUG 4: the scan keeps books ─────────────────────────────

/// **sol's frozen invariant, as a test.** Every unit the scan examines lands in
/// exactly one terminal bucket, and the buckets add up to what was examined. The
/// first cut answered a missing root, a failed `read_dir`, a depth cut-off and a
/// protection prune with the same bare `continue` — nothing in the report, empty
/// `errors`, exit 0. All four are injected here at once, beside one real
/// candidate, so a reaper that silently examined nothing cannot pass.
#[cfg(unix)]
#[test]
fn every_examined_unit_lands_in_exactly_one_terminal_bucket() {
    use std::os::unix::fs::PermissionsExt;

    let root = unique_temp_dir("tachi-reaper-accounting");
    let dead = make_target_dir(&root, "dead-target"); // progressed: candidate
    let live = make_target_dir(&root, "live-shared-target"); // safety refusal
    let locked = root.join("locked-dir"); // incomplete: unreadable
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let deep = root.join("a/b/c"); // expected exclusion: depth budget
    std::fs::create_dir_all(deep.join("d")).unwrap();
    let missing = root.join("no-such-root"); // incomplete: missing root

    let scan = scan_orphan_candidates(
        &[root.clone(), missing.clone()],
        &Protection::new([live.clone()], Vec::new()),
        aged_now(30),
        7,
    );
    let books = scan.accounting;

    assert!(
        books.balances(),
        "conservation: examined must equal the sum of the buckets: {books:?}"
    );
    assert_eq!(books.candidates, 1, "{books:?}");
    assert_eq!(books.protected_pruned, 1, "{books:?}");
    assert_eq!(books.depth_limited, 1, "{books:?}");
    assert_eq!(books.unreadable, 1, "{books:?}");
    assert_eq!(books.roots_missing, 1, "{books:?}");
    // root, a, a/b — the three directories that were read and descended.
    assert_eq!(books.descended, 3, "{books:?}");
    assert_eq!(books.examined, 8, "{books:?}");

    // Exactly one bucket per unit: no path is booked twice, and no candidate is
    // also a skip.
    let mut booked: Vec<&Path> = scan.skips.iter().map(|skip| skip.path.as_path()).collect();
    booked.extend(scan.candidates.iter().map(|c| c.path.as_path()));
    let unique: BTreeSet<&Path> = booked.iter().copied().collect();
    assert_eq!(
        booked.len(),
        unique.len(),
        "a unit may not appear in two buckets: {booked:?}"
    );
    assert_eq!(scan.skips.len(), 4, "{:?}", scan.skips);
    assert!(scan.candidates.iter().any(|c| c.path == dead));

    // …and each skip carries the class it belongs to.
    let class_of = |path: &Path| {
        scan.skips
            .iter()
            .find(|skip| skip.path == path)
            .map(|skip| skip.outcome.class())
    };
    assert_eq!(class_of(&missing), Some(UnitClass::IncompleteOrError));
    assert_eq!(class_of(&locked), Some(UnitClass::IncompleteOrError));
    assert_eq!(class_of(&live), Some(UnitClass::SafetyRefusal));
    assert_eq!(class_of(&deep), Some(UnitClass::ExpectedExclusion));

    // The run saw units it could not examine ⇒ it did not account for its scope.
    assert!(books.incomplete(), "{books:?}");

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// **BUG 4, closed: an ATTEMPTED delete that does not cleanly finish must not
/// exit 0.** The first cut treated every non-`Ok` outcome from the delete path
/// the same way — a warning line, run still exits 0 — which conflated "a fence
/// fired, working as designed" with "the delete was tried and a
/// `remove_dir_all` failed partway". This fixture forces the second: the
/// candidate is genuinely eligible (stale, unheld, unbound — the OS itself is
/// what refuses one entry), so the failure comes from the delete path, not
/// from any earlier gate.
#[cfg(unix)]
#[test]
fn a_partial_delete_failure_forces_a_nonclean_exit() {
    use std::os::unix::fs::PermissionsExt;

    let root = unique_temp_dir("tachi-reaper-partial-delete");
    let target = make_target_dir(&root, "half-deletable-target");
    let locked = target.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("stuck.o"), vec![1u8; 16]).unwrap();
    // Unlinking `stuck.o` needs write+execute on its PARENT (`locked`), not on
    // the file itself — stripping that makes `remove_dir_all` delete
    // everything else it can (the fixture's `debug/artifact.rlib` included) and
    // then fail on this one entry: a real partial delete, not a simulated one.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

    let mut store = open_store(&root);
    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &*unheld_probe(),
    );

    // Restore permissions before anything else touches the fixture, or the
    // temp dir leaks an undeletable entry past this test.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(report.candidates.len(), 1, "{report:?}");
    assert_eq!(
        report.candidates[0].decision, "refused",
        "a delete that did not finish is not `reclaim`: {report:?}"
    );
    assert!(
        locked.join("stuck.o").exists(),
        "the entry `remove_dir_all` could not touch survives: {report:?}"
    );
    // The discriminating assertion: the pre-#1062 shape put this in `warnings`
    // only and still exited 0. `errors` (not just `warnings`) must carry it.
    assert!(
        !report.errors.is_empty(),
        "an attempted delete that did not finish must land in `errors`, not just a warning: \
         {report:?}"
    );
    assert!(report.incomplete, "{report:?}");
    let status = reap_exit_status(&report);
    assert!(
        status.is_err(),
        "a partial delete failure must never exit clean, even under --force: {status:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A run that could not look at everything it was told to look at does not get
/// to report success — even when it *did* reclaim something, and even under
/// `--force`. Discriminating both ways: the same fixture without the missing root
/// exits clean.
#[test]
fn an_incomplete_forced_scan_does_not_exit_clean() {
    let root = unique_temp_dir("tachi-reaper-incomplete-exit");
    let dead = make_target_dir(&root, "dead-target");
    let mut store = open_store(&root);
    let missing = root.join("no-such-root");

    let incomplete = reap_uncertified(
        store.connection_mut(),
        &ReapOptions {
            roots: vec![root.clone(), missing],
            max_age_days: 7,
            force: true,
        },
        aged_now(30),
        &*unheld_probe(),
    );

    // The run really did work — this is not "it failed, so it deleted nothing".
    assert_eq!(incomplete.reclaimed.len(), 1, "{incomplete:?}");
    assert!(!dead.exists());
    assert!(incomplete.errors.is_empty(), "{:?}", incomplete.errors);
    assert!(
        incomplete.incomplete,
        "a scan with an unaccounted unit is incomplete: {incomplete:?}"
    );
    let status = reap_exit_status(&incomplete);
    assert!(
        status.is_err(),
        "an incomplete run must not exit clean, force or not: {status:?}"
    );

    // Same fixture, whole scope examined ⇒ clean exit. Without this half the test
    // would pass on a reaper that never exits 0 at all.
    let reborn = make_target_dir(&root, "dead-target");
    let complete = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &*unheld_probe(),
    );
    assert_eq!(complete.reclaimed.len(), 1, "{complete:?}");
    assert!(!reborn.exists());
    assert!(!complete.incomplete, "{complete:?}");
    assert!(
        reap_exit_status(&complete).is_ok(),
        "a fully accounted run exits clean: {complete:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The label must be the truth at the END of the run. A candidate the gates
/// approved and the deleter then refused (here: a holder appears in the
/// scan→delete window) keeps its bytes — so a report that still calls it
/// `reclaim` is a report that lies about what happened to them.
#[test]
fn a_late_refusal_relabels_the_candidate_refused_not_reclaimed() {
    let root = unique_temp_dir("tachi-reaper-late-refusal-label");
    let target = make_target_dir(&root, "racy-target");
    let mut store = open_store(&root);

    let calls = std::cell::Cell::new(0usize);
    let probe = move |_path: &Path, _ignored_holder| {
        let n = calls.get();
        calls.set(n + 1);
        if n == 0 {
            HolderCheck::None // the scan's expensive probe: eligible
        } else {
            HolderCheck::Held(vec!["cargo 1".to_string()]) // the deleter's recheck
        }
    };

    let report = reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &probe,
    );

    assert_eq!(report.candidates.len(), 1, "{report:?}");
    assert_eq!(
        report.candidates[0].decision, "refused",
        "a candidate whose bytes are still on disk must not keep the `reclaim` label: \
         {report:?}"
    );
    assert!(
        report.candidates[0].reason.contains("holder appeared"),
        "and the reason is the late refusal, not the stale one: {}",
        report.candidates[0].reason
    );
    assert!(report.reclaimed.is_empty(), "{report:?}");
    assert!(target.join("debug/artifact.rlib").exists(), "bytes survive");

    let _ = std::fs::remove_dir_all(&root);
}

// ── #1062/#1379: --force is refused at the entry point, but the delete-path fences
// are still pinned by direct calls to `run_orphan_reap_uncertified` ────────

/// The seal is closed again (`tachi#1379`, 2026-07-23) — and the proof that
/// "closed" does not mean the fences behind it are gone. Renamed from
/// `force_reclaims_at_the_entry_point_and_a_broken_fence_still_refuses_it`, which
/// pinned the #1062 shape: the entry point (`reap_sealed`) let `--force` through
/// because `DESTRUCTIVE_CERTIFIED` was `true`, the first half genuinely deleted,
/// and the second half proved certification did not disable the per-candidate
/// `(dev, ino)` fence. See git blame / #1062 for that reading.
///
/// Two halves, same design, inverted premise now that certification is revoked:
///
/// * **first half — the entry point itself refuses again.** `--force` through
///   `reap_sealed` (the REAL entry point `run_orphan_reap_cli` also calls) on a
///   healthy fixture now returns a top-level `Err(DestructiveRefusal)` without
///   touching the ledger or deleting anything — proving `certify_destructive`
///   went back to being the gate it was in the pre-#1062 shape.
/// * **second half keeps CONCERN 6 alive behind the seal.** The `(dev, ino)`
///   identity is swapped out from under the verdict between judgement and delete
///   (BUG 2's own fence — same swap technique as
///   `a_directory_replaced_at_the_same_path_between_verdict_and_delete_is_refused`).
///   Because the entry point now refuses `--force`, this half bypasses the gate
///   via `reap_uncertified` (the sheathed body tests already use) so the fence
///   itself stays pinned: it still refuses the delete even though `force` is
///   present. The day #1379's handle-pinning fix and an inode-reuse kill-test
///   re-certify the path, this half is the existing regression test that the
///   fence still works in the re-opened world.
#[test]
fn force_is_refused_at_the_entry_point_and_a_broken_fence_still_refuses_behind_it() {
    // Half 1: a healthy fixture, the real entry point, `--force` — refused
    // outright now that `tachi#1379` re-sheathed the knife.
    let root = unique_temp_dir("tachi-reaper-resheathed-delete");
    let dead = make_target_dir(&root, "dead-target");
    let mut store = open_store(&root);

    let refusal = reap_sealed(
        store.connection_mut(),
        &opts(&root, true),
        aged_now(30),
        &*unheld_probe(),
    )
    .expect_err("--force must be refused now: tachi#1379 revoked the 2026-07-17 certification");

    assert!(refusal.refused);
    assert!(
        refusal.reason.contains("tachi#1379"),
        "reason: {}",
        refusal.reason
    );
    // Nothing was scanned, measured, booked or deleted.
    assert!(
        dead.join("debug/artifact.rlib").exists(),
        "a refused --force must not delete anything: {refusal:?}"
    );
    assert!(
        memcore::list_resources(store.connection(), None, None)
            .unwrap()
            .is_empty(),
        "a refused --force must not book anything"
    );

    let _ = std::fs::remove_dir_all(&root);

    // Half 2 (CONCERN 6, still true behind the seal): the same `--force` intent,
    // routed through the sheathed body so the per-candidate fences remain covered.
    let root2 = unique_temp_dir("tachi-reaper-resheathed-still-fenced");
    let target = make_target_dir(&root2, "swapped-target");
    let mut store2 = open_store(&root2);

    let target_for_probe = target.clone();
    let swapping_probe = move |_path: &Path, _ignored_holder| {
        std::fs::remove_dir_all(&target_for_probe).unwrap();
        std::fs::create_dir_all(target_for_probe.join("debug")).unwrap();
        std::fs::write(
            target_for_probe.join("debug/replacement.rlib"),
            vec![9u8; 4096],
        )
        .unwrap();
        HolderCheck::None
    };

    let report2 = reap_uncertified(
        store2.connection_mut(),
        &opts(&root2, true),
        aged_now(30),
        &swapping_probe,
    );

    assert!(
        target.join("debug/replacement.rlib").exists(),
        "the replacement directory must survive: the identity fence is still armed: {report2:?}"
    );
    assert!(report2.reclaimed.is_empty(), "{report2:?}");
    assert_eq!(report2.candidates.len(), 1, "{report2:?}");
    assert_eq!(report2.candidates[0].decision, "refused", "{report2:?}");
    assert!(
        report2
            .warnings
            .iter()
            .any(|warning| warning.contains("(dev, ino) identity")),
        "the refusal must still name the (dev, ino) mismatch behind the seal: {report2:?}"
    );

    let _ = std::fs::remove_dir_all(&root2);
}

/// `certify_destructive` is the single gate — and since `tachi#1379` revoked the
/// 2026-07-17 certification, it refuses ONLY the destructive request again: a
/// report-only request is still permitted to proceed past this gate, and a
/// `--force` request is turned into [`DestructiveRefusal`]. Other fences (the
/// protected set, the holder probe, the pinned `(dev, ino)` identity) still
/// stand; see `force_is_refused_at_the_entry_point_and_a_broken_fence_still_
/// refuses_behind_it` for the proof that they are still armed behind the seal.
///
/// Renamed from `certify_destructive_permits_both_once_certified`, which pinned
/// the post-#1062 shape (`certify_destructive(true)` unconditionally `Ok`,
/// `certify_destructive(false)` unconditionally `Ok`) — and whose own tripwire
/// (`assert!(DESTRUCTIVE_CERTIFIED, …)`) fired exactly as designed the day
/// `tachi#1379` flipped the constant back, which is what sent this test here to
/// be reconciled. See git blame / #1379 for that reading.
#[test]
fn only_the_destructive_request_is_refused() {
    assert!(
        certify_destructive(false).is_ok(),
        "report-only was never gated by certification"
    );
    assert!(
        certify_destructive(true).is_err(),
        "force must be refused while DESTRUCTIVE_CERTIFIED is false — the gate's own logic \
         only ever refuses `force && !DESTRUCTIVE_CERTIFIED`"
    );

    // A tripwire on the compile-time constant, deliberately — the same shape the
    // post-#1062 test used, pointed the other way. `clippy` calls a constant
    // assertion pointless because a constant cannot surprise you at runtime; that is
    // exactly why this one is here. It states the premise the two assertions above
    // depend on (they only mean "the gate refuses force" while the seal is closed), so
    // the day somebody RE-certifies the path and flips `DESTRUCTIVE_CERTIFIED` back
    // to `true`, this test goes red and names the seal — symmetric to the tripwire
    // it replaces, which fired the day certification was revoked.
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(
            !DESTRUCTIVE_CERTIFIED,
            "the day this flips back to true, `certify_destructive(true)` permits again \
             and the assertion above must flip with it"
        );
    }
}

/// The report says whether the knife is certified, so a reader cannot mistake
/// `reclaimable_bytes` for bytes that were freed just because certification
/// flipped. Renamed from `the_report_declares_itself_certified`, which
/// pinned the post-#1062 `true` reading — see git blame / #1062 for that
/// shape.
///
/// A DRY RUN (`force: false`) still books nothing and frees nothing even
/// though the destructive path is re-sheathed — `destructive_certified` and
/// `dry_run` are orthogonal fields, and this test's core property (a
/// report-only run reports, it does not act) is unchanged by the flip; only
/// the certification bit it now reads back is.
#[test]
fn the_report_declares_itself_uncertified() {
    let root = unique_temp_dir("tachi-reaper-declares");
    make_target_dir(&root, "dead-target");
    let mut store = open_store(&root);

    let report = reap_sealed(
        store.connection_mut(),
        &opts(&root, false),
        aged_now(30),
        &*unheld_probe(),
    )
    .expect("a report-only run is never refused");

    assert!(!report.destructive_certified);
    assert_eq!(report.blocking_defects.len(), BLOCKING_DEFECTS.len());
    assert_eq!(report.reclaimed_bytes, 0, "a report frees nothing");
    assert!(report.reclaimable_bytes > 0, "but it counts what is dead");

    let _ = std::fs::remove_dir_all(&root);
}

// ── CONCERN 5: the books must be able to FAIL ────────────────────────────

/// The kill-test for the tautology. Every one of these mutations passed the old
/// `balances()` (it compared `examined` with the sum of the buckets, and `record`
/// moved both at once, so the equation was arithmetic, not an invariant).
#[test]
fn a_dequeued_unit_that_never_reaches_a_bucket_breaks_the_books() {
    // A well-formed unit: discovered, taken, judged.
    let mut books = ScanAccounting::default();
    books.enqueue();
    books.dequeue();
    books.record(UnitOutcome::Descended);
    assert!(books.balances(), "{books:?}");
    assert!(!books.incomplete(), "{books:?}");

    // The regression this invariant exists to catch: a unit comes off the work list
    // and falls through a `continue` without a verdict.
    let mut dropped_verdict = books;
    dropped_verdict.enqueue();
    dropped_verdict.dequeue();
    assert!(
        !dropped_verdict.balances(),
        "a dequeued unit with no bucket must break conservation: {dropped_verdict:?}"
    );
    assert!(
        dropped_verdict.incomplete(),
        "and cost the run its clean exit"
    );

    // The other direction: work discovered and never taken (an early `break`).
    let mut dropped_work = books;
    dropped_work.enqueue();
    assert!(
        !dropped_work.balances(),
        "enqueued work that is never dequeued must break conservation: {dropped_work:?}"
    );
    assert!(dropped_work.incomplete());
}

/// CONCERN 5: the same directory named twice — once by its own spelling, once through
/// a symlink — is one scan root, walked once. Before the dedup it was walked twice:
/// the candidate was listed twice and every count in the report was doubled.
#[test]
fn duplicate_roots_are_walked_once_and_counted_once() {
    let root = unique_temp_dir("tachi-reaper-duproots");
    make_target_dir(&root, "dead-target");
    // A second spelling of the very same directory.
    let alias = root.with_extension("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();

    let protection = Protection::default();
    let now = aged_now(30);

    let once = scan_orphan_candidates(std::slice::from_ref(&root), &protection, now, 7);
    let twice = scan_orphan_candidates(
        &[root.clone(), root.clone(), alias.clone()],
        &protection,
        now,
        7,
    );

    assert_eq!(once.candidates.len(), 1, "{:?}", once.candidates);
    assert_eq!(
        twice.candidates.len(),
        1,
        "a directory named three times is still one directory: {:?}",
        twice.candidates
    );
    // Two duplicate roots, booked as expected exclusions — visible, not vanished.
    assert_eq!(
        twice.accounting.roots_duplicate, 2,
        "{:?}",
        twice.accounting
    );
    assert_eq!(
        twice.accounting.candidates, once.accounting.candidates,
        "the subtree is walked once, so the candidate count does not double: {:?}",
        twice.accounting
    );
    assert_eq!(
        twice.accounting.descended, once.accounting.descended,
        "nor does the descended count: {:?}",
        twice.accounting
    );
    // The duplicates are the only extra units, and the books still balance.
    assert_eq!(
        twice.accounting.examined,
        once.accounting.examined + 2,
        "{:?}",
        twice.accounting
    );
    assert!(twice.accounting.balances(), "{:?}", twice.accounting);
    assert!(
        !twice.accounting.incomplete(),
        "a duplicate root is an expected exclusion, not an error: {:?}",
        twice.accounting
    );
    assert!(
        twice
            .skips
            .iter()
            .any(|skip| skip.outcome == UnitOutcome::RootDuplicate
                && skip.reason.contains("walked once")),
        "the operator must see the root they named: {:?}",
        twice.skips
    );

    let _ = std::fs::remove_file(&alias);
    let _ = std::fs::remove_dir_all(&root);
}

// ── BUG 3: an incomplete protected set is fail-CLOSED ────────────────────

/// A protection source that cannot be resolved makes the run incomplete and costs it
/// a clean exit — it does not merely print a warning and carry on.
///
/// An unresolvable `HOME` is the source it is staged with: `HOME` is what
/// `~/.cache/sigil-shared-target` (the documented default cache, protected even when
/// no variable names it) is resolved from. The first cut called that a warning,
/// deleted anyway, and exited 0.
///
/// The gap is staged by handing this run a [`ProtectionSources`] with no home — NOT
/// by unsetting `HOME` in the process, which is what the previous version did and
/// which made every test in this binary silently depend on `HOME` being set.
#[test]
fn an_incomplete_protection_set_never_exits_clean() {
    let root = unique_temp_dir("tachi-reaper-protection-gap");
    make_target_dir(&root, "dead-target");
    let mut store = open_store(&root);

    // The complete half: with the protection sources resolvable, the same run is
    // clean. (Without this, a reaper that called *every* run incomplete would pass.)
    let complete = reap_sealed(
        store.connection_mut(),
        &opts(&root, false),
        aged_now(30),
        &*unheld_probe(),
    )
    .expect("report-only");
    assert!(complete.protection_complete, "{:?}", complete.warnings);
    assert!(!complete.incomplete, "{complete:?}");
    assert!(reap_exit_status(&complete).is_ok(), "{complete:?}");

    // Now break a protection source — for this run, and this run only.
    let gapped = run_orphan_reap_with_sources_and_probe(
        store.connection_mut(),
        &opts(&root, false),
        &resolved_sources().without_home(),
        aged_now(30),
        &*unheld_probe(),
    )
    .expect("report-only");

    assert!(
        !gapped.protection_complete,
        "an unresolvable protection source is a GAP: {gapped:?}"
    );
    assert!(
        gapped.incomplete,
        "and a gap makes the run incomplete: {gapped:?}"
    );
    assert!(
        gapped.warnings.iter().any(|w| w.contains("HOME")),
        "the operator is told which fence is missing: {:?}",
        gapped.warnings
    );
    let status = reap_exit_status(&gapped);
    let err = status.expect_err("an incomplete protected set must not exit clean");
    assert!(
        err.contains("protected set incomplete"),
        "and the exit says why: {err}"
    );
    // The scan itself saw everything it was told to see — this failure is NOT a
    // missing root or an unreadable subtree, which is exactly why it needs its own
    // signal instead of riding on the unit counts.
    assert_eq!(gapped.scan.roots_missing, 0, "{:?}", gapped.scan);
    assert_eq!(gapped.scan.unreadable, 0, "{:?}", gapped.scan);
    assert!(gapped.scan.balances(), "{:?}", gapped.scan);

    let _ = std::fs::remove_dir_all(&root);
}

/// **checkpoint 1 fix, standing coverage (codex-9178d).** The test above
/// (`an_incomplete_protection_set_never_exits_clean`) runs `force: false` and
/// only proves the report-level flags — it never reaches the delete path at
/// all, so it cannot discriminate this bug. #1062's own text: an unresolved
/// protection source "refuses to delete anything," not merely a non-zero exit
/// after the fact. Discriminating: before the fix, `fresh.is_complete()` being
/// false at delete time still fell through to the `covers()` check, and a
/// candidate that the (incomplete) set did not happen to name as covered was
/// reclaimed anyway.
#[test]
fn an_incomplete_protection_set_at_delete_time_deletes_nothing() {
    let root = unique_temp_dir("tachi-reaper-protection-gap-delete");
    let dead = make_target_dir(&root, "dead-target");
    let mut store = open_store(&root);

    let report = run_orphan_reap_uncertified(
        store.connection_mut(),
        &opts(&root, true),
        &resolved_sources().without_home(),
        aged_now(30),
        &*unheld_probe(),
    );

    assert!(
        dead.join("debug/artifact.rlib").exists(),
        "an unresolved protection source at delete time must refuse to delete, not just \
         warn about it afterward: {report:?}"
    );
    assert!(report.reclaimed.is_empty(), "{report:?}");
    assert!(!report.protection_complete, "{report:?}");
    let status = reap_exit_status(&report);
    assert!(status.is_err(), "must not exit clean: {status:?}");

    let _ = std::fs::remove_dir_all(&root);
}

/// The gap is what makes it incomplete — not the mere presence of a note.
#[test]
fn a_protection_set_with_no_gaps_is_complete() {
    let complete = Protection::new([PathBuf::from("/tmp/x-target")], Vec::new());
    assert!(complete.is_complete());
    assert!(complete.gaps().is_empty());

    let gapped = Protection::new(
        [PathBuf::from("/tmp/x-target")],
        vec!["process scan unavailable".to_string()],
    );
    assert!(!gapped.is_complete());
    assert_eq!(gapped.gaps().len(), 1);
}

// ── the race BUG 3's own test used to cause ─────────────────────────────

/// **The discriminating test for the fix.** A run with a MISSING protection source and
/// a run with a COMPLETE one, in flight *at the same time, in the same process*, each
/// getting its own answer.
///
/// This is the exact shape that could not exist before. The gap used to be staged by
/// `remove_var("HOME")`, which is process-global: while it was removed, every other
/// test in this binary — including ones that never mention `HOME` — was running
/// against a reaper that could not resolve the default cache, so
/// `an_incomplete_forced_scan_does_not_exit_clean`'s "the same fixture, whole scope
/// examined ⇒ clean exit" half failed on a build seat while the four tests that own
/// the fail-closed behaviour all passed. A lock would have hidden that by forbidding
/// the overlap; injection makes the overlap *harmless*, which is the property worth
/// pinning.
///
/// The two runs are held in the same window on purpose: each one's holder probe waits
/// for the other to reach its own probe, so both are provably mid-run — past the
/// protected-set computation, before the verdict — at the same instant. The wait has a
/// deadline rather than a barrier, so a regression that stops one run from probing
/// fails the assertions instead of hanging the suite.
#[test]
fn a_gapped_run_and_a_resolved_run_are_in_flight_together_without_contaminating_each_other() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    let gapped_root = unique_temp_dir("tachi-reaper-parallel-gapped");
    let resolved_root = unique_temp_dir("tachi-reaper-parallel-resolved");
    make_target_dir(&gapped_root, "dead-target");
    make_target_dir(&resolved_root, "dead-target");

    // `HolderProbe` is `dyn Fn(...) + 'static` (the same signature production code
    // hands it under), so the closure below cannot BORROW a stack local — even
    // though `thread::scope` would happily let it borrow the stack for the spawn
    // itself, the probe's own type signature demands `'static`. So the counter is
    // owned by the closure via a cloned `Arc`, not borrowed.
    let arrived = Arc::new(AtomicUsize::new(0));
    let rendezvous = {
        let arrived = Arc::clone(&arrived);
        move |_path: &Path, _ignored_holder| {
            arrived.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while arrived.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                std::thread::yield_now();
            }
            HolderCheck::None
        }
    };

    let (gapped, resolved) = std::thread::scope(|scope| {
        let gapped = scope.spawn(|| {
            let mut store = open_store(&gapped_root);
            run_orphan_reap_with_sources_and_probe(
                store.connection_mut(),
                &opts(&gapped_root, false),
                &resolved_sources().without_home(),
                aged_now(30),
                &rendezvous,
            )
            .expect("report-only")
        });
        let resolved = scope.spawn(|| {
            let mut store = open_store(&resolved_root);
            run_orphan_reap_with_sources_and_probe(
                store.connection_mut(),
                &opts(&resolved_root, false),
                &resolved_sources(),
                aged_now(30),
                &rendezvous,
            )
            .expect("report-only")
        });
        (gapped.join().unwrap(), resolved.join().unwrap())
    });

    // Both really were in the same window (each probe saw the other arrive), so the
    // verdicts below were computed concurrently — not one after the other.
    assert_eq!(
        arrived.load(Ordering::SeqCst),
        2,
        "both runs must have reached their holder probe, or they never overlapped"
    );

    // The gapped run is fail-closed …
    assert!(!gapped.protection_complete, "{gapped:?}");
    assert!(gapped.incomplete, "{gapped:?}");
    assert!(
        gapped.warnings.iter().any(|w| w.contains("HOME")),
        "{:?}",
        gapped.warnings
    );
    assert!(reap_exit_status(&gapped).is_err(), "{gapped:?}");

    // … and its neighbour, which shared the process with it the whole time, is not
    // touched by it: complete protected set, clean exit.
    assert!(
        resolved.protection_complete,
        "the neighbouring run's protected set must not be gapped by someone else's \
         missing source: {:?}",
        resolved.warnings
    );
    assert!(!resolved.incomplete, "{resolved:?}");
    assert!(reap_exit_status(&resolved).is_ok(), "{resolved:?}");

    let _ = std::fs::remove_dir_all(&gapped_root);
    let _ = std::fs::remove_dir_all(&resolved_root);
}

/// **The escape hatch stays closed.** `set_var` / `remove_var` are how the race got
/// in: they mutate the environment of the *process*, and cargo runs these tests as
/// threads of one. Nothing in this module — production or test — may reach for them
/// again; a protection source that needs to vary is an argument
/// ([`ProtectionSources`]), not a global.
///
/// A source-level fence rather than a code-level one, because the failure it guards
/// against is a *future test* reintroducing the mutation, and no runtime assertion in
/// the current tests can see that coming.
#[test]
fn no_test_mutates_the_process_environment() {
    // Assembled at runtime, or this test's own source would be the first hit.
    let forbidden = ["set", "remove"].map(|verb| format!("env::{verb}_var"));
    // The module was split (#1423): the fence must scan production AND this
    // test file, or a mutation reintroduced on either side walks past it.
    let sources = [include_str!("lib.rs"), include_str!("tests.rs")];

    for needle in &forbidden {
        assert!(
            !sources
                .iter()
                .any(|source| source.contains(needle.as_str())),
            "`{needle}` is back in tachi-exec-env-reaper/src/lib.rs (production) or \
             tachi-exec-env-reaper/src/tests.rs (this test module). It mutates the environment of \
             the whole test process, which is what made every reaper test depend on HOME \
             and turned an unrelated test red. Inject a `ProtectionSources` instead."
        );
    }
}

/// The public production entry point must never accept caller-selected protection
/// sources or a holder probe. Either would turn a production fence into caller policy.
#[test]
fn public_reap_entry_uses_only_the_real_holder_probe() {
    let production = include_str!("lib.rs");
    let public_start = production
        .find("pub fn run_orphan_reap(")
        .expect("public reaper entry point must exist");
    let seam_offset = production[public_start..]
        .find("\nfn run_orphan_reap_with_sources_and_probe(")
        .expect("private sources-and-probe seam must follow the public entry point");
    let public_entry = &production[public_start..public_start + seam_offset];

    assert!(
        !public_entry.contains("HolderProbe") && !public_entry.contains("ProtectionSources<'_>"),
        "the public production entry point must not accept injectable fences:\n{public_entry}"
    );
    let gate = public_entry
        .find("certify_destructive(opts.force)?")
        .expect("public entry must certify destructive intent");
    let sources = public_entry
        .find("ProtectionSources::from_process_env()")
        .expect("public entry must construct real protection sources");
    let probe = public_entry
        .find("&lsof_holder_probe")
        .expect("public entry must fix the real lsof probe");
    assert!(
        gate < sources && sources < probe,
        "the destructive gate must precede environment/process reads and the real holder probe:\n\
         {public_entry}"
    );
    assert!(
        !production.contains("pub fn run_orphan_reap_with_sources_and_probe(")
            && !production.contains("pub(crate) fn run_orphan_reap_with_sources_and_probe("),
        "the injectable sources-and-probe seam must remain private to this module"
    );
}

// ── #1062/#1379 kill-test matrix (S2d shape) — #[ignore]d, not yet re-run ────
//
// Everything above this line is the STANDING suite: it runs on every `cargo test`,
// and every assertion in it is discriminating (red on the pre-#1062 code, green
// after — see the individual test docs). It is not, on its own, what S2d's doctrine
// calls a certification: "the unit tests pass" is this crate believing its own code.
//
// This is the separate thing S2d asks for — an EXECUTED run, checked in as a
// receipt. `orphan_reaper_kill_test_matrix` below drives the four scenarios #1062
// names as the minimum bar, back to back, against real directories, under REAL
// `--force` — and PRINTS a receipt in the certification.rs shape when it passes. It
// never writes one; that is a human/Oz decision, made by reading the printed output
// and checking a TOML file in by hand (`crates/tachi-dispatch/certifications/
// codex-cli.toml` is the precedent for the shape). A receipt WAS checked in for
// 2026-07-17 (`crates/tachi-server/certifications/orphan-reaper.toml`), but
// `tachi#1379` showed that matrix never exercised the inode-reuse scenario that can
// defeat BUG 2's `(dev, ino)` fence. The 2026-07-17 receipt therefore no longer
// certifies the path; `DESTRUCTIVE_CERTIFIED` is `false` again and stays `false`
// until #1379's handle-pinning fix lands, an inode-reuse scenario is added below,
// and a fresh receipt is checked in by hand. No code in this module reads the
// printed output back to flip it, on purpose: a receipt this crate wrote to itself
// would recreate exactly the self-grading S2d exists to rule out.
mod kill_tests {
    use super::*;

    /// One line of the matrix: what was exercised, and whether it survived / was
    /// refused as required. Printed, not asserted into a struct anyone parses —
    /// the human checking in the receipt reads this.
    struct MatrixResult {
        label: &'static str,
        outcome: &'static str,
    }

    /// The four scenarios #1062 names as the minimum kill-test bar, run back to
    /// back against real directories under real `--force`. Each panics (failing
    /// the test, and printing nothing) if the reaper does not behave exactly as
    /// required; only a run where all four survive prints the receipt.
    ///
    /// `#[ignore]`: this is the out-of-band event S2d's own module doc describes
    /// — it deletes real directories on the machine that runs it (inside its own
    /// temp roots only) and is not something an ordinary `cargo test` should run
    /// unattended. Run explicitly: `cargo test --offline -p tachi-exec-env-reaper --lib \
    /// tests::kill_tests:: -- --ignored --nocapture`.
    #[cfg(unix)]
    #[ignore = "#1062 kill-test: real deletes under real --force; run explicitly, not on every cargo test"]
    #[test]
    fn orphan_reaper_kill_test_matrix() {
        let started = std::time::Instant::now();
        let mut results = Vec::new();

        // 1. A live build holding a target via env-var-only MUST survive. The
        //    process table is empty for the whole run — no argv ever names the
        //    target — and the fixture survives only because a lease bound it on
        //    the ledger (BUG 1).
        {
            let root = unique_temp_dir("tachi-reaper-kt-env-var-only");
            let live = make_target_dir(&root, "kt-env-var-only-target");
            let mut store = open_store(&root);
            memcore::insert_exec_env(
                store.connection(),
                &memcore::NewExecEnvLease {
                    env_id: "kt-env-live".to_string(),
                    kind: "worktree".to_string(),
                    path: "/wt/kt-env-live".to_string(),
                    repo_root: "/repo".to_string(),
                    branch: "tachi/1062/kt".to_string(),
                    base_sha: "abc123".to_string(),
                    dispatch_id: None,
                    env_class: memcore::EnvClass::default(),
                    created_at: String::new(),
                },
            )
            .unwrap();
            let resource_id = match memcore::insert_resource(
                store.connection_mut(),
                &NewExecEnvResource {
                    resource_id: "kt-res-live".to_string(),
                    kind: ResourceKind::BuildTarget,
                    path: live.display().to_string(),
                    bytes: Some(2048),
                    created_at: String::new(),
                },
            )
            .unwrap()
            {
                RegisterOutcome::Registered { resource_id } => resource_id,
                other => panic!("expected a fresh registration: {other:?}"),
            };
            memcore::bind_resource(store.connection_mut(), "kt-env-live", &resource_id).unwrap();

            let report = reap_uncertified(
                store.connection_mut(),
                &opts(&root, true),
                aged_now(30),
                &*unheld_probe(),
            );
            assert!(
                live.join("debug/artifact.rlib").exists(),
                "1. env-var-only live build must survive: {report:?}"
            );
            assert!(report.reclaimed.is_empty(), "1. {report:?}");
            let _ = std::fs::remove_dir_all(&root);
            results.push(MatrixResult {
                label: "env_var_only_live_build_survives",
                outcome: "PASS: ledger-bound target untouched, ps blind throughout",
            });
        }

        // 2. A target swapped for a different object at the same path between
        //    scan and delete MUST NOT be followed — the replacement survives
        //    (BUG 2).
        {
            let root = unique_temp_dir("tachi-reaper-kt-swap");
            let target = make_target_dir(&root, "kt-swapped-target");
            let mut store = open_store(&root);
            let target_for_probe = target.clone();
            let swapping_probe = move |_path: &Path, _ignored_holder| {
                std::fs::remove_dir_all(&target_for_probe).unwrap();
                std::fs::create_dir_all(target_for_probe.join("debug")).unwrap();
                std::fs::write(
                    target_for_probe.join("debug/replacement.rlib"),
                    vec![9u8; 4096],
                )
                .unwrap();
                HolderCheck::None
            };
            let report = reap_uncertified(
                store.connection_mut(),
                &opts(&root, true),
                aged_now(30),
                &swapping_probe,
            );
            assert!(
                target.join("debug/replacement.rlib").exists(),
                "2. the replacement object must survive: {report:?}"
            );
            assert!(report.reclaimed.is_empty(), "2. {report:?}");
            let _ = std::fs::remove_dir_all(&root);
            results.push(MatrixResult {
                label: "target_swapped_at_same_path_not_followed",
                outcome: "PASS: (dev, ino) mismatch refused the delete",
            });
        }

        // 3. A protected source that cannot be resolved (HOME unset — the
        //    default shared cache cannot be named) MUST abort the whole run under
        //    --force, deleting nothing (BUG 3, re-proven under this matrix's
        //    real --force + real fixtures).
        {
            let root = unique_temp_dir("tachi-reaper-kt-gap");
            let dead = make_target_dir(&root, "kt-gap-target");
            let mut store = open_store(&root);
            let report = run_orphan_reap_uncertified(
                store.connection_mut(),
                &opts(&root, true),
                &resolved_sources().without_home(),
                aged_now(30),
                &*unheld_probe(),
            );
            assert!(
                dead.join("debug/artifact.rlib").exists(),
                "3. an unresolvable protected source must abort before any delete: {report:?}"
            );
            assert!(report.reclaimed.is_empty(), "3. {report:?}");
            assert!(!report.protection_complete, "3. {report:?}");
            assert!(
                reap_exit_status(&report).is_err(),
                "3. must not exit clean: {report:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
            results.push(MatrixResult {
                label: "unresolvable_protected_source_aborts_the_run",
                outcome: "PASS: fail-closed, non-zero exit, nothing deleted",
            });
        }

        // 4. A process-table scan that cannot spawn MUST abort the whole run
        //    under --force, deleting nothing.
        {
            let root = unique_temp_dir("tachi-reaper-kt-noproc");
            let dead = make_target_dir(&root, "kt-noproc-target");
            let mut store = open_store(&root);
            let failing_scan: fn() -> (Vec<PathBuf>, Vec<String>) = || {
                (
                    Vec::new(),
                    vec!["process scan unavailable (ps: No such file or directory)".to_string()],
                )
            };
            let report = run_orphan_reap_uncertified(
                store.connection_mut(),
                &opts(&root, true),
                &resolved_sources().with_live_builds(&failing_scan),
                aged_now(30),
                &*unheld_probe(),
            );
            assert!(
                dead.join("debug/artifact.rlib").exists(),
                "4. a ps that cannot spawn must abort before any delete: {report:?}"
            );
            assert!(report.reclaimed.is_empty(), "4. {report:?}");
            assert!(
                reap_exit_status(&report).is_err(),
                "4. must not exit clean: {report:?}"
            );
            let _ = std::fs::remove_dir_all(&root);
            results.push(MatrixResult {
                label: "ps_unavailable_aborts_the_run",
                outcome: "PASS: fail-closed, non-zero exit, nothing deleted",
            });
        }

        let duration_secs = started.elapsed().as_secs_f64();

        // Printed, never written — see the section doc above for why checking in
        // the receipt is a human act, not something this test does to itself.
        println!("\n─── #1062 orphan reaper kill-test receipt (S2d shape) ───");
        println!("kill_test = \"crates/tachi-exec-env-reaper/src/tests.rs\"");
        println!("kill_test_fn = \"tests::kill_tests::orphan_reaper_kill_test_matrix\"");
        println!("binary = \"tachi-exec-env-reaper\"");
        println!("binary_version = \"{}\"", env!("CARGO_PKG_VERSION"));
        println!("host_os = \"{}\"", std::env::consts::OS);
        println!("result = \"pass\"");
        println!("duration_secs = \"{duration_secs:.2}\"");
        println!("matrix = [");
        for result in &results {
            println!("  \"{}\", # {}", result.label, result.outcome);
        }
        println!("]");
        println!(
            "# executed_by / executed_at / executed_on_commit / kill_test_source_blob: fill \
             in by hand from the environment that ran this, then check in as \
             crates/tachi-server/certifications/orphan-reaper.toml — see \
             crates/tachi-dispatch/certifications/codex-cli.toml for the shape."
        );
        println!("───────────────────────────────────────────────────────\n");
    }
}
