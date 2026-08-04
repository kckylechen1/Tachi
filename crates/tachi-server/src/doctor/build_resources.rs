//! `tachi doctor` build-resource patrol (tachi#1184 item 2 — "Oz recycle
//! patrol" / the build-resource leaf under the #894 worktree dashboard).
//!
//! REPORT-ONLY, always. Neither function here deletes or reconciles
//! anything; both only ever produce [`DoctorWarning`]s a human/Oz reads and
//! acts on. Two independent halves:
//!
//! 1. [`scan_orphan_build_resources`] — private `CARGO_TARGET_DIR`-shaped
//!    directories that look dead. This is a THIN reuse of
//!    `exec_env_reaper`'s already-certified scan/protection/holder-probe
//!    machinery (#1062) — not a second implementation of it — plus two new
//!    layers: [`is_blessed_target_basename`], a static allowlist checked
//!    independently of the env-based protection `exec_env_reaper` already
//!    applies (that protection only fires when the CALLING shell happens to
//!    export `CARGO_TARGET_DIR`/`TACHI_SHARED_CARGO_TARGET_DIR`; `tachi
//!    doctor` is commonly run from a plain admin shell with neither set, and
//!    without this list the live shared cache would misreport as an orphan
//!    the moment nobody's build happens to be running at that exact
//!    instant); and [`ledger_held_paths`], a read of the `exec_env_resources`
//!    lease ledger (tachi#1184 cross-vendor review C3) — a `ps`/`lsof` probe
//!    alone cannot see a `BuildPrivate` lease's target dir held only via a
//!    live binding, never an open fd (`exec_env_reaper.rs:854-869` documents
//!    the exact same blind spot for the certified reaper's own holder
//!    check), so this patrol reuses the same structural fix the reaper
//!    already ships (`memcore::list_bound_resource_paths`) rather than
//!    re-deriving it or leaving the gap open.
//! 2. [`worktree_inspection_report`] — facts about every managed worktree in
//!    `tachi_clean::registry`'s registry (age, existence, attribution).
//!    Deliberately does NOT compute a "terminal"/safe-to-close verdict: that
//!    reconciliation semantic is tachi#1118's frozen scope (owner routing
//!    note on #1184), and duplicating it here — even as a lighter heuristic
//!    — would be a second, drifting copy of a decision #1118/#1212 already
//!    own. This surfaces only the raw facts an operator needs to make that
//!    call themselves.

use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use memcore::ResourceKind;

use super::DoctorWarning;

/// Basenames that are ALWAYS the machine's blessed shared build cache, never
/// a private orphan candidate — see the module doc for why this check is
/// independent of (and a defense-in-depth complement to) the env/process
/// based protection `exec_env_reaper::protected_paths` already applies.
const BLESSED_SHARED_TARGET_BASENAMES: &[&str] = &["sigil-shared-target", "hyperion-shared-target"];

/// Default staleness gate for the patrol — same default `exec_env_reaper`'s
/// own CLI uses (7 days), so the doctor section and `tachi clean
/// --orphan-reap` do not report two different populations for the same
/// directory.
pub(crate) const DEFAULT_ORPHAN_MAX_AGE_DAYS: u64 = 7;

/// Default staleness gate for worktree inspection notes: a registered
/// managed worktree not touched in this many days is worth an operator's
/// eye. Deliberately longer than the build-target gate — a worktree can sit
/// idle mid-review for a while without being dead.
pub(crate) const DEFAULT_WORKTREE_STALE_DAYS: i64 = 14;

/// True when `path`'s basename is on the machine-wide blessed-shared-target
/// allowlist — never an orphan candidate, regardless of staleness or holder
/// state. Pure: no filesystem access, just a basename string compare.
pub(crate) fn is_blessed_target_basename(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| BLESSED_SHARED_TARGET_BASENAMES.contains(&name))
}

/// Facts about one orphan-target candidate, already measured/probed by the
/// caller. Kept separate from `exec_env_reaper::OrphanCandidate` on purpose:
/// this is the narrow slice [`orphan_target_warning`] needs, so that
/// formatter can be unit-tested with synthetic values — no real `ps`/`du` in
/// the test.
#[derive(Debug, Clone)]
pub(crate) struct OrphanTargetFacts {
    pub(crate) path: String,
    pub(crate) kind: ResourceKind,
    pub(crate) age_days: Option<u64>,
    pub(crate) bytes: Option<u64>,
}

/// Pure formatter: candidate facts -> a [`DoctorWarning`]. No fs/ps access.
pub(crate) fn orphan_target_warning(facts: &OrphanTargetFacts) -> DoctorWarning {
    let size = facts
        .bytes
        .map(human_bytes)
        .unwrap_or_else(|| "size unknown".to_string());
    let age = facts
        .age_days
        .map(|d| format!("{d}d"))
        .unwrap_or_else(|| "unknown".to_string());
    DoctorWarning {
        code: "orphan_build_resource".to_string(),
        path: facts.path.clone(),
        message: format!(
            "{} is a private {} ({size}), stale {age}, no live holder found — build work \
             should queue through the shared/Oz target, not a private one (tachi#1184)",
            facts.path,
            facts.kind.as_str(),
        ),
        remediation: format!(
            "confirm with `du -sh {0}` and `lsof +D {0}`, then `rm -rf {0}` if it is really \
             dead (the certified `tachi clean --orphan-reap` delete path is not yet \
             kill-test-certified, tachi#1062 — this warning does not wait on that)",
            facts.path
        ),
    }
}

/// Human-readable byte size — pure, no fs.
fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

/// Everything that can hold a build-resource candidate, already resolved by
/// the caller. Kept as its own tiny enum (rather than reusing
/// `exec_env_reaper::HolderCheck` directly) so [`should_flag_candidate`] below
/// stays a pure function over a minimal, hermetically-constructible input —
/// no real `ps`/`lsof` needed to exercise every branch in a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HolderState {
    /// No process has an open fd under the path (`HolderCheck::None`).
    Unheld,
    /// A process has the path open (`HolderCheck::Held`).
    Held,
    /// The check could not be trusted (`HolderCheck::Unknown` — no `lsof`,
    /// partial walk). Treated exactly like `Held`: a report-only false
    /// negative costs nothing, a false positive costs a live cache.
    Unknown,
}

/// The result of the reaper's own age-vs-`max_age_days` walk
/// (`exec_env_reaper::Staleness`), reduced to the three states
/// [`should_flag_candidate`] needs. Self-caught while wiring this up (not
/// part of the cross-vendor review, but the same defect class it was
/// hunting): `exec_env_reaper::scan_orphan_candidates` computes this per
/// candidate but does NOT filter on it — that filter lives downstream, in
/// `decide_reap`, which only `run_orphan_reap` calls. This patrol calls the
/// scan directly and never called `decide_reap`, so without this enum and
/// the gate below, a brand-new, mid-provision `*-target` directory (0 days
/// old, briefly unheld between build steps) would have been flagged
/// exactly like a truly dead one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StalenessState {
    /// Nothing under the tree is newer than `max_age_days` (`Staleness::Stale`).
    Stale,
    /// Something under the tree is newer than the cutoff (`Staleness::Fresh`).
    Fresh,
    /// The walk was partial/unreadable (`Staleness::Unprovable`) — fail-closed,
    /// same discipline as an unprovable holder check.
    Unprovable,
}

/// Pure decision: should this candidate be flagged as an orphan? Every fence
/// this patrol applies lives here, in one place, so both a HELD path and a
/// genuinely ORPHANED path can be exercised directly by a unit test (两造并察
/// — tachi#1184 review C6b) without touching real `ps`/`lsof`/sqlite.
///
/// Order matters for readability only, not correctness — all four checks
/// are independent short-circuits, any one of which is enough to protect the
/// path:
/// 1. blessed basename (the machine's known-good shared target, tachi#1184
///    item 2's own defense-in-depth layer, independent of env/ledger state),
/// 2. ledger-held (a `BuildPrivate` lease has this path bound RIGHT NOW —
///    tachi#1184 review C3: the exact blind spot `exec_env_reaper.rs:854-869`
///    documents for `ps`/`lsof`-only holder checks, closed here the same way
///    the certified reaper closes it: consult the lease ledger, not just the
///    process table),
/// 3. not (yet) stale ([`StalenessState::Fresh`] / [`StalenessState::Unprovable`]
///    — see that enum's doc for the self-caught gap this closes),
/// 4. process-held or unprovable (`HolderState::Held` / `Unknown`).
pub(crate) fn should_flag_candidate(
    path: &Path,
    ledger_held: &HashSet<String>,
    holder: HolderState,
    staleness: StalenessState,
) -> bool {
    if is_blessed_target_basename(path) {
        return false;
    }
    if is_ledger_held(path, ledger_held) {
        return false;
    }
    if !matches!(staleness, StalenessState::Stale) {
        return false;
    }
    matches!(holder, HolderState::Unheld)
}

/// Pure: is `path` on the ledger-held set? String-keyed, same discipline
/// `exec_env_reaper::run_orphan_reap_uncertified` already uses for its own
/// ledger lookup (`candidate.path.display().to_string()` against
/// `memcore::find_resource_by_path`) — not a new comparison convention.
fn is_ledger_held(path: &Path, ledger_held: &HashSet<String>) -> bool {
    ledger_held.contains(&path.display().to_string())
}

/// Read every path the `exec_env_resources` ledger currently considers BOUND
/// (a live lease binding, `released_at IS NULL`) — the exact structural fix
/// `exec_env_reaper::ledger_protected_paths` uses, reused here via the same
/// public `memcore::list_bound_resource_paths` read (tachi#1184 review C3).
///
/// A global DB that does not exist yet is treated as "zero leases ever
/// recorded" (`Ok(empty set)`) — genuinely true on a machine where no
/// `build-private` env has ever been provisioned, not a gap. Any OTHER
/// failure (unreadable file, schema the binary does not understand, a
/// missing `exec_env_resources` table) is a GAP: the caller must not proceed
/// as though "no leases" were proven when it was merely never checked — the
/// same BUG-3 discipline `exec_env_reaper::Protection::is_complete` applies,
/// translated to a report-only diagnostic that skips rather than fails.
fn ledger_held_paths(global_db_path: &Path) -> Result<HashSet<String>, String> {
    if !global_db_path.exists() {
        return Ok(HashSet::new());
    }
    let path_str = global_db_path
        .to_str()
        .ok_or_else(|| "global db path is not valid UTF-8".to_string())?;
    let store =
        memcore::MemoryStore::open_read_only(path_str).map_err(|err| format!("open: {err}"))?;
    memcore::list_bound_resource_paths(store.connection())
        .map(|paths| paths.into_iter().collect())
        .map_err(|err| format!("query exec_env_resources: {err}"))
}

/// Scan the reaper's own default orphan roots (`/private/tmp`, `$TMPDIR`,
/// `~/.cache`, …) for private build-resource directories that look dead:
/// name-shaped like a target dir ([`crate::exec_env_reaper::classify_orphan_dir_name`]),
/// not on the blessed allowlist, not ledger-held, not env/process-protected,
/// stale past `max_age_days`, and with no live holder.
///
/// REPORT-ONLY. If the lease ledger cannot be consulted (see
/// [`ledger_held_paths`]'s doc for what counts as a gap vs. a genuine
/// "no leases"), the scan is skipped entirely and a single explanatory
/// warning is returned instead — proceeding on an incomplete protected set
/// is exactly how a live, lease-held cache would get named "safe to
/// `rm -rf`" (tachi#1184 review C3), and a report-only tool has no
/// obligation to guess when it can say plainly that it did not check.
pub(crate) fn scan_orphan_build_resources(
    max_age_days: u64,
    global_db_path: &Path,
) -> Vec<DoctorWarning> {
    scan_orphan_build_resources_with_roots(
        max_age_days,
        global_db_path,
        &crate::exec_env_reaper::default_orphan_roots(),
    )
}

/// [`scan_orphan_build_resources`]'s production body, with the scan roots
/// INJECTED rather than read from `default_orphan_roots()` — the same "real
/// production path, controlled inputs" idiom `exec_env_reaper`'s own CLI
/// already uses (`run_orphan_reap_cli_with_sources` vs `run_orphan_reap_cli`,
/// `ProtectionSources::deterministic_for_cli_test()` vs `::from_process_env()`).
/// `default_orphan_roots()` always includes real system paths
/// (`/private/tmp`, `~/.cache`, …) regardless of any env override a test
/// might set, so a hermetic test of THIS function needs its own seam rather
/// than trying to redirect those roots (tachi#1184 review C6c).
fn scan_orphan_build_resources_with_roots(
    max_age_days: u64,
    global_db_path: &Path,
    roots: &[std::path::PathBuf],
) -> Vec<DoctorWarning> {
    let ledger_held = match ledger_held_paths(global_db_path) {
        Ok(held) => held,
        Err(err) => {
            return vec![DoctorWarning {
                code: "orphan_scan_skipped_ledger_unavailable".to_string(),
                path: global_db_path.display().to_string(),
                message: format!(
                    "build-resource orphan scan skipped: could not consult the \
                     exec_env_resources lease ledger ({err}) — a lease-held private target \
                     with no open file descriptor would otherwise misreport as orphan"
                ),
                remediation: "confirm the global tachi DB is reachable and migrated, then re-run \
                              `tachi doctor`"
                    .to_string(),
            }];
        }
    };

    let sources = crate::exec_env_reaper::ProtectionSources::from_process_env();
    let protection = crate::exec_env_reaper::protected_paths(&sources);
    let now = SystemTime::now();
    let scan =
        crate::exec_env_reaper::scan_orphan_candidates(roots, &protection, now, max_age_days);

    scan.candidates
        .iter()
        // Cheap pre-filter first: skip the real `lsof` shell-out entirely for
        // a path already excluded by the blessed list, the (already-read)
        // ledger, or staleness — `should_flag_candidate` below re-checks all
        // three anyway (it is the single source of truth for the decision),
        // this is purely to avoid probing paths that can never survive it.
        .filter(|candidate| {
            !is_blessed_target_basename(&candidate.path)
                && !is_ledger_held(&candidate.path, &ledger_held)
                && matches!(
                    candidate.staleness,
                    crate::exec_env_reaper::Staleness::Stale { .. }
                )
        })
        .filter_map(|candidate| {
            let holder = match crate::exec_env_reaper::lsof_holder_probe(&candidate.path, None) {
                crate::exec_env_reaper::HolderCheck::None => HolderState::Unheld,
                crate::exec_env_reaper::HolderCheck::Held(_) => HolderState::Held,
                crate::exec_env_reaper::HolderCheck::Unknown(_) => HolderState::Unknown,
            };
            let staleness = match candidate.staleness {
                crate::exec_env_reaper::Staleness::Stale { .. } => StalenessState::Stale,
                crate::exec_env_reaper::Staleness::Fresh { .. } => StalenessState::Fresh,
                crate::exec_env_reaper::Staleness::Unprovable(_) => StalenessState::Unprovable,
            };
            if !should_flag_candidate(&candidate.path, &ledger_held, holder, staleness) {
                return None;
            }
            let facts = OrphanTargetFacts {
                path: candidate.path.display().to_string(),
                kind: candidate.kind,
                age_days: candidate.staleness.age_days(),
                bytes: Some(crate::exec_env_reaper::dir_size(&candidate.path)),
            };
            Some(orphan_target_warning(&facts))
        })
        .collect()
}

/// Parse an RFC3339 timestamp and return its age in whole days against `now`.
/// `None` on an unparseable timestamp (never a panic, never a guessed age).
fn age_in_days(rfc3339: &str, now: DateTime<Utc>) -> Option<i64> {
    let parsed = DateTime::parse_from_rfc3339(rfc3339)
        .ok()?
        .with_timezone(&Utc);
    Some((now - parsed).num_days())
}

/// One inspection note about a registered managed worktree. Pure given
/// `worktrees`/`now`/`stale_days` — no fs, no git shell-out — so this is
/// unit-testable with synthetic [`tachi_clean::registry::ListedWorktree`]
/// values.
///
/// Deliberately produces FACTS, never a "safe to close" verdict: closing a
/// worktree is tachi#1118's reconciliation call, not this patrol's (owner
/// routing note on #1184).
pub(crate) fn worktree_inspection_warnings(
    worktrees: &[tachi_clean::registry::ListedWorktree],
    now: DateTime<Utc>,
    stale_days: i64,
) -> Vec<DoctorWarning> {
    worktrees
        .iter()
        .filter_map(|wt| worktree_inspection_warning(wt, now, stale_days))
        .collect()
}

fn worktree_inspection_warning(
    wt: &tachi_clean::registry::ListedWorktree,
    now: DateTime<Utc>,
    stale_days: i64,
) -> Option<DoctorWarning> {
    if !wt.path_exists {
        return Some(DoctorWarning {
            code: "registered_worktree_missing".to_string(),
            path: wt.path.clone(),
            message: format!(
                "{} is registered (branch '{}') but the directory no longer exists — stale \
                 registry entry",
                wt.path, wt.branch
            ),
            // #1605: the remediation must name a subcommand that exists.
            // `tachi worktree remove --path <p>` never did — the verb is
            // `close` and the path is positional
            // (tachi-bootstrap/src/cli/maintenance_actions.rs:178-191). The
            // hand-edit escape hatch is gone too: `worktree close --force`
            // now falls back to a registry-row match when the directory is
            // already gone (tools/cleaner/src/wt_clean.rs).
            remediation: format!(
                "if this worktree was already removed by hand, drop the stale row: \
                 `tachi worktree close {} --force`",
                wt.path
            ),
        });
    }
    let age_days = age_in_days(&wt.updated_at, now)?;
    if age_days < stale_days {
        return None;
    }
    let attribution = wt
        .dispatch_id
        .as_deref()
        .map(|id| format!(", dispatch {id}"))
        .unwrap_or_default();
    Some(DoctorWarning {
        code: "worktree_inspection_stale".to_string(),
        path: wt.path.clone(),
        message: format!(
            "{} (branch '{}'{attribution}) has not been touched in {age_days}d — worth review \
             (inspection only; whether it is safe to close/reap is tachi#1118's call, not this \
             patrol's)",
            wt.path, wt.branch,
        ),
        remediation: format!(
            "`git -C {0} status --short` and `git -C {0} log -1 --format=%cr` to judge, then \
             `tachi worktree close {0} --force` once a human/Oz confirms it is done",
            wt.path
        ),
    })
}

/// Collect + inspect every registered managed worktree. The one impure edge
/// in this file: reads `~/.tachi/worktrees.json` and the wall clock, then
/// hands off to the pure [`worktree_inspection_warnings`] above.
pub(crate) fn worktree_inspection_report(stale_days: i64) -> Vec<DoctorWarning> {
    match tachi_clean::registry::list_registered_worktrees() {
        Ok(worktrees) => worktree_inspection_warnings(&worktrees, Utc::now(), stale_days),
        Err(err) => vec![DoctorWarning {
            code: "worktree_registry_unreadable".to_string(),
            path: String::new(),
            message: format!("could not read the managed-worktree registry: {err}"),
            remediation: "check ~/.tachi/worktrees.json exists and is valid JSON".to_string(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── tachi#1184 cross-vendor review C3/C6b: should_flag_candidate ──────
    // Both sides of the decision (两造并察), fully hermetic — no real ps/du/
    // sqlite. A ledger-held path must survive exactly like a process-held one;
    // a genuinely orphaned path (blessed=no, ledger=no, holder=unheld) is the
    // only shape that gets flagged.

    #[test]
    fn ledger_held_path_is_never_flagged_even_when_process_probe_says_unheld() {
        let mut ledger = HashSet::new();
        ledger.insert("/private/tmp/leased-build-private-target".to_string());
        assert!(
            !should_flag_candidate(
                Path::new("/private/tmp/leased-build-private-target"),
                &ledger,
                HolderState::Unheld,
                StalenessState::Stale,
            ),
            "a lease-held path must never be flagged, regardless of what ps/lsof saw"
        );
    }

    #[test]
    fn genuinely_orphaned_path_is_flagged() {
        let ledger = HashSet::new();
        assert!(should_flag_candidate(
            Path::new("/private/tmp/issue1140-review-target"),
            &ledger,
            HolderState::Unheld,
            StalenessState::Stale,
        ));
    }

    #[test]
    fn process_held_path_is_not_flagged() {
        let ledger = HashSet::new();
        assert!(!should_flag_candidate(
            Path::new("/private/tmp/mid-build-target"),
            &ledger,
            HolderState::Held,
            StalenessState::Stale,
        ));
    }

    #[test]
    fn unknown_holder_state_is_not_flagged() {
        let ledger = HashSet::new();
        assert!(!should_flag_candidate(
            Path::new("/private/tmp/unprobable-target"),
            &ledger,
            HolderState::Unknown,
            StalenessState::Stale,
        ));
    }

    #[test]
    fn blessed_path_is_not_flagged_even_if_somehow_ledger_and_process_agree_it_looks_unheld() {
        let ledger = HashSet::new();
        assert!(!should_flag_candidate(
            Path::new("/home/x/.cache/sigil-shared-target"),
            &ledger,
            HolderState::Unheld,
            StalenessState::Stale,
        ));
    }

    #[test]
    fn fresh_candidate_is_never_flagged_even_if_unheld_and_not_ledgered() {
        // Self-caught gap: `exec_env_reaper::scan_orphan_candidates` computes
        // staleness but does not filter on it (that gate is `decide_reap`'s,
        // which only `run_orphan_reap` calls) — without this check, a
        // brand-new `*-target` dir mid-provision would misreport as orphan.
        let ledger = HashSet::new();
        assert!(!should_flag_candidate(
            Path::new("/private/tmp/just-created-target"),
            &ledger,
            HolderState::Unheld,
            StalenessState::Fresh,
        ));
    }

    #[test]
    fn unprovable_staleness_is_never_flagged() {
        let ledger = HashSet::new();
        assert!(!should_flag_candidate(
            Path::new("/private/tmp/partially-unreadable-target"),
            &ledger,
            HolderState::Unheld,
            StalenessState::Unprovable,
        ));
    }

    // ─── tachi#1184 review C3: ledger_held_paths against a real fixture DB ─
    // "injected ledger fixture", not real ps/lsof — a temp-file sqlite DB
    // populated through memcore's own public registration API, proving the
    // SQL wiring (not just the in-memory HashSet decision above) is correct.

    #[test]
    fn ledger_held_paths_reads_a_live_binding_from_a_real_fixture_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("global-fixture.db");
        {
            let mut store =
                memcore::MemoryStore::open_with_label(db_path.to_str().unwrap(), "test-global")
                    .expect("open fixture db");
            memcore::insert_exec_env(
                store.connection(),
                &memcore::NewExecEnvLease {
                    env_id: "env-fixture-1".to_string(),
                    kind: "worktree".to_string(),
                    path: "/some/worktree".to_string(),
                    repo_root: "/repo".to_string(),
                    branch: "feat/x".to_string(),
                    base_sha: "deadbeef".to_string(),
                    dispatch_id: None,
                    env_class: memcore::EnvClass::BuildPrivate,
                    created_at: "2026-07-18T00:00:00Z".to_string(),
                },
            )
            .expect("insert exec_env fixture");
            memcore::insert_resource(
                store.connection_mut(),
                &memcore::NewExecEnvResource {
                    resource_id: "res-fixture-1".to_string(),
                    kind: ResourceKind::BuildTarget,
                    path: "/private/tmp/leased-build-private-target".to_string(),
                    bytes: None,
                    created_at: "2026-07-18T00:00:00Z".to_string(),
                },
            )
            .expect("insert resource fixture");
            memcore::bind_resource(store.connection_mut(), "env-fixture-1", "res-fixture-1")
                .expect("bind resource fixture");
        }

        let held = ledger_held_paths(&db_path).expect("ledger read should succeed");
        assert!(
            held.contains("/private/tmp/leased-build-private-target"),
            "{held:?}"
        );
        assert_eq!(held.len(), 1, "{held:?}");
    }

    #[test]
    fn ledger_held_paths_is_empty_when_the_global_db_does_not_exist_yet() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("never-provisioned.db");
        let held = ledger_held_paths(&db_path).expect("a missing db is an empty ledger, not a gap");
        assert!(held.is_empty());
    }

    #[test]
    fn scan_orphan_build_resources_skips_with_a_warning_when_the_ledger_path_is_unreadable() {
        // A DB path that exists but is not a valid sqlite file: `open_read_only`
        // must error, and that error must surface as a loud skip-warning, not
        // as "proceed assuming nothing is held" (tachi#1184 review C3 —
        // BUG-3-style discipline: an unresolved protection source is a gap).
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("corrupt.db");
        std::fs::write(&db_path, b"not a sqlite file").expect("write corrupt fixture");

        let warnings = scan_orphan_build_resources(7, &db_path);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].code, "orphan_scan_skipped_ledger_unavailable");
    }

    #[cfg(unix)]
    fn set_mtime(path: &Path, when: SystemTime) {
        // Same technique `exec_env_reaper`'s own test module uses: a
        // directory cannot be opened for writing, but futimens(2) on a
        // read-only fd is enough to set times on something you own.
        let file = std::fs::File::open(path).expect("open for mtime set");
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("set mtime");
    }

    #[cfg(unix)]
    #[test]
    fn scan_orphan_build_resources_is_report_only_and_idempotent_c6c() {
        // tachi#1184 review C6c ("identical output with and without --fix")
        // + an end-to-end (not just unit-level) exercise of C3's ledger
        // gate: a REAL, genuinely stale (mtime backdated past the 7-day
        // cutoff), non-blessed on-disk directory that IS bound in a real
        // fixture ledger must never be flagged, must survive two full scans
        // untouched, and the two scans must return byte-identical output —
        // the operational meaning of "with/without --fix" for a section
        // that never reads a `fix` flag at all (see the call site in
        // `bootstrap::manifest_cli::run_doctor_command`).
        //
        // Uses `scan_orphan_build_resources_with_roots` (roots injected)
        // rather than the public `scan_orphan_build_resources`
        // (`default_orphan_roots()` always walks the real machine's
        // `/private/tmp`/`~/.cache`, which would make this test's output
        // depend on whatever happens to be on the machine running it).
        let dir = tempfile::tempdir().expect("tempdir");
        let scan_root = dir.path().join("scan-root");
        std::fs::create_dir_all(&scan_root).expect("scan root");
        let candidate = scan_root.join("leased-but-old-looking-target");
        std::fs::create_dir_all(&candidate).expect("candidate dir");
        let ten_days_ago = SystemTime::now() - std::time::Duration::from_secs(10 * 24 * 60 * 60);
        set_mtime(&candidate, ten_days_ago);
        let candidate_str = candidate.display().to_string();

        let db_path = dir.path().join("global-fixture.db");
        {
            let mut store =
                memcore::MemoryStore::open_with_label(db_path.to_str().unwrap(), "test-global")
                    .expect("open fixture db");
            memcore::insert_exec_env(
                store.connection(),
                &memcore::NewExecEnvLease {
                    env_id: "env-c6c".to_string(),
                    kind: "worktree".to_string(),
                    path: "/some/worktree".to_string(),
                    repo_root: "/repo".to_string(),
                    branch: "feat/x".to_string(),
                    base_sha: "deadbeef".to_string(),
                    dispatch_id: None,
                    env_class: memcore::EnvClass::BuildPrivate,
                    created_at: "2026-07-01T00:00:00Z".to_string(),
                },
            )
            .expect("insert exec_env fixture");
            memcore::insert_resource(
                store.connection_mut(),
                &memcore::NewExecEnvResource {
                    resource_id: "res-c6c".to_string(),
                    kind: ResourceKind::BuildTarget,
                    path: candidate_str.clone(),
                    bytes: None,
                    created_at: "2026-07-01T00:00:00Z".to_string(),
                },
            )
            .expect("insert resource fixture");
            memcore::bind_resource(store.connection_mut(), "env-c6c", "res-c6c")
                .expect("bind resource fixture");
        }

        let roots = vec![scan_root.clone()];
        let first = scan_orphan_build_resources_with_roots(7, &db_path, &roots);
        let second = scan_orphan_build_resources_with_roots(7, &db_path, &roots);

        assert!(
            !first.iter().any(|w| w.path == candidate_str),
            "a ledger-held, genuinely stale directory must never be flagged: {first:?}"
        );
        let render = |warnings: &[DoctorWarning]| -> Vec<(String, String)> {
            warnings
                .iter()
                .map(|w| (w.code.clone(), w.path.clone()))
                .collect()
        };
        assert_eq!(
            render(&first),
            render(&second),
            "two consecutive report-only scans over unchanged state must be identical"
        );
        assert!(
            candidate.exists(),
            "report-only: the directory must survive the scan"
        );
        let held_after = ledger_held_paths(&db_path).expect("ledger read after scan");
        assert!(
            held_after.contains(&candidate_str),
            "report-only: the ledger binding must survive the scan too"
        );
    }

    #[test]
    fn blessed_basename_matches_exactly() {
        assert!(is_blessed_target_basename(Path::new(
            "/home/x/.cache/sigil-shared-target"
        )));
        assert!(is_blessed_target_basename(Path::new(
            "/home/x/.cache/hyperion-shared-target"
        )));
    }

    #[test]
    fn blessed_basename_rejects_lookalikes() {
        // A private dispatch target that merely CONTAINS the blessed name as
        // a substring must not slip through on a `contains` check — this is
        // an exact-basename allowlist, not a fuzzy one.
        assert!(!is_blessed_target_basename(Path::new(
            "/home/x/.cache/sigil-shared-target-fork-1173"
        )));
        assert!(!is_blessed_target_basename(Path::new(
            "/private/tmp/sigil-1098-impl-target"
        )));
    }

    #[test]
    fn blessed_basename_ignores_directory_prefix() {
        // Same basename under a DIFFERENT parent must still match — the
        // allowlist is a basename check, not a full-path check (the shared
        // cache always lives directly under `~/.cache`, but this function
        // should not silently depend on that; the caller's scan roots are
        // what fix the parent).
        assert!(is_blessed_target_basename(Path::new(
            "/some/other/root/sigil-shared-target"
        )));
    }

    #[test]
    fn orphan_target_warning_reports_size_and_age() {
        let facts = OrphanTargetFacts {
            path: "/private/tmp/issue1140-review-target".to_string(),
            kind: ResourceKind::BuildTarget,
            age_days: Some(9),
            bytes: Some(3 * 1024 * 1024 * 1024),
        };
        let warning = orphan_target_warning(&facts);
        assert_eq!(warning.code, "orphan_build_resource");
        assert_eq!(warning.path, facts.path);
        assert!(warning.message.contains("3.0GB"), "{}", warning.message);
        assert!(warning.message.contains("9d"), "{}", warning.message);
        assert!(
            warning.message.contains("build_target"),
            "{}",
            warning.message
        );
        assert!(warning.remediation.contains("rm -rf"));
    }

    #[test]
    fn orphan_target_warning_handles_unknown_bytes_and_age() {
        let facts = OrphanTargetFacts {
            path: "/private/tmp/orchestrator-cas-target".to_string(),
            kind: ResourceKind::ScratchDir,
            age_days: None,
            bytes: None,
        };
        let warning = orphan_target_warning(&facts);
        assert!(warning.message.contains("size unknown"));
        assert!(warning.message.contains("stale unknown"));
    }

    #[test]
    fn human_bytes_formats_units() {
        assert_eq!(human_bytes(0), "0B");
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(2048), "2.0KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0GB");
    }

    fn listed(
        path: &str,
        branch: &str,
        dispatch_id: Option<&str>,
        updated_at: &str,
        path_exists: bool,
    ) -> tachi_clean::registry::ListedWorktree {
        tachi_clean::registry::ListedWorktree {
            path: path.to_string(),
            repo_root: "/repo".to_string(),
            branch: branch.to_string(),
            dispatch_id: dispatch_id.map(str::to_string),
            pr: None,
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            path_exists,
        }
    }

    #[test]
    fn missing_path_always_flagged_regardless_of_age() {
        let now = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let worktrees = vec![listed(
            "/private/tmp/wz-gone",
            "feat/gone",
            None,
            "2026-07-17T00:00:00Z",
            false,
        )];
        let warnings = worktree_inspection_warnings(&worktrees, now, 14);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "registered_worktree_missing");
    }

    /// #1605: the remediation used to name `tachi worktree remove --path <p>`,
    /// which is not a subcommand — the verb is `close` and the path is
    /// positional. An unrunnable remediation is worse than none: it reads as
    /// tried-and-failed when an operator's shell rejects it.
    #[test]
    fn worktree_remediations_name_a_real_subcommand() {
        let now = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let worktrees = vec![
            listed(
                "/private/tmp/wz-gone",
                "feat/gone",
                None,
                "2026-07-17T00:00:00Z",
                false,
            ),
            listed(
                "/private/tmp/wz-stale",
                "feat/stale",
                None,
                "2026-06-01T00:00:00Z",
                true,
            ),
        ];
        let warnings = worktree_inspection_warnings(&worktrees, now, 14);
        assert_eq!(
            warnings.len(),
            2,
            "expected both warning kinds: {warnings:?}"
        );
        for warning in &warnings {
            assert!(
                !warning.remediation.contains("worktree remove --path"),
                "{}: remediation names a subcommand that does not exist: {}",
                warning.code,
                warning.remediation
            );
            assert!(
                warning
                    .remediation
                    .contains(&format!("tachi worktree close {} --force", warning.path)),
                "{}: remediation must be the runnable close command: {}",
                warning.code,
                warning.remediation
            );
        }
    }

    #[test]
    fn fresh_existing_worktree_is_silent() {
        let now = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let worktrees = vec![listed(
            "/home/x/.cache/tachi/worktrees/repo/fresh",
            "feat/fresh",
            Some("dispatch-9"),
            "2026-07-17T12:00:00Z",
            true,
        )];
        assert!(worktree_inspection_warnings(&worktrees, now, 14).is_empty());
    }

    #[test]
    fn stale_existing_worktree_reports_facts_not_a_verdict() {
        let now = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let worktrees = vec![listed(
            "/home/x/.cache/tachi/worktrees/repo/stale",
            "feat/stale",
            Some("dispatch-1"),
            "2026-06-01T00:00:00Z",
            true,
        )];
        let warnings = worktree_inspection_warnings(&worktrees, now, 14);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "worktree_inspection_stale");
        assert!(warnings[0].message.contains("dispatch-1"));
        assert!(
            warnings[0].message.contains("tachi#1118"),
            "must defer the close verdict to #1118, not claim one itself: {}",
            warnings[0].message
        );
    }

    #[test]
    fn boundary_age_is_inclusive_of_stale_days() {
        let now = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let worktrees = vec![listed(
            "/home/x/.cache/tachi/worktrees/repo/exact",
            "feat/exact",
            None,
            "2026-07-04T00:00:00Z", // exactly 14 days before `now`
            true,
        )];
        assert_eq!(worktree_inspection_warnings(&worktrees, now, 14).len(), 1);
    }

    #[test]
    fn unparseable_timestamp_is_skipped_not_panicked() {
        let now = DateTime::parse_from_rfc3339("2026-07-18T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let worktrees = vec![listed(
            "/home/x/.cache/tachi/worktrees/repo/bad-ts",
            "feat/bad-ts",
            None,
            "not-a-timestamp",
            true,
        )];
        assert!(worktree_inspection_warnings(&worktrees, now, 14).is_empty());
    }
}
