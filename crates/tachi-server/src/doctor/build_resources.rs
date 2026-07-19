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
//!    machinery (#1062) — not a second implementation of it — plus exactly
//!    one new layer: [`is_blessed_target_basename`], a static allowlist
//!    checked independently of the env-based protection `exec_env_reaper`
//!    already applies (that protection only fires when the CALLING shell
//!    happens to export `CARGO_TARGET_DIR`/`TACHI_SHARED_CARGO_TARGET_DIR`;
//!    `tachi doctor` is commonly run from a plain admin shell with neither
//!    set, and without this list the live shared cache would misreport as
//!    an orphan the moment nobody's build happens to be running at that
//!    exact instant).
//! 2. [`worktree_inspection_report`] — facts about every managed worktree in
//!    `tachi_clean::registry`'s registry (age, existence, attribution).
//!    Deliberately does NOT compute a "terminal"/safe-to-close verdict: that
//!    reconciliation semantic is tachi#1118's frozen scope (owner routing
//!    note on #1184), and duplicating it here — even as a lighter heuristic
//!    — would be a second, drifting copy of a decision #1118/#1212 already
//!    own. This surfaces only the raw facts an operator needs to make that
//!    call themselves.

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

/// Scan the reaper's own default orphan roots (`/private/tmp`, `$TMPDIR`,
/// `~/.cache`, …) for private build-resource directories that look dead:
/// name-shaped like a target dir ([`crate::exec_env_reaper::classify_orphan_dir_name`]),
/// not on the blessed allowlist, not env/process-protected, stale past
/// `max_age_days`, and with no live holder.
///
/// REPORT-ONLY. Conservative on purpose: a candidate whose holder check came
/// back `Unknown` (no `lsof`, partial probe) is treated exactly like `Held`
/// — skipped, not flagged. A report-only false negative here costs nothing;
/// a false positive naming a live build cache as safe-to-delete is the
/// literal shape of the incident this patrol exists to prevent, not repeat.
///
/// **Known limitation, by design:** unlike `exec_env_reaper::run_orphan_reap`,
/// this does NOT consult the `exec_env_resources` ledger (that needs a live
/// DB connection `tachi doctor` does not otherwise open) — so an
/// explicitly-approved, currently-idle `build-private` lease's target dir can
/// surface here as a false-positive warning. That is an acceptable cost for a
/// report-only diagnostic whose own remediation text already tells the
/// operator to confirm with `du`/`lsof` before touching anything; it is NOT
/// acceptable for the certified delete path, which is exactly why that path
/// (`run_orphan_reap`) unions the ledger in and this function does not try to
/// re-derive that half of it.
pub(crate) fn scan_orphan_build_resources(max_age_days: u64) -> Vec<DoctorWarning> {
    let roots = crate::exec_env_reaper::default_orphan_roots();
    let sources = crate::exec_env_reaper::ProtectionSources::from_process_env();
    let protection = crate::exec_env_reaper::protected_paths(&sources);
    let now = SystemTime::now();
    let scan =
        crate::exec_env_reaper::scan_orphan_candidates(&roots, &protection, now, max_age_days);

    scan.candidates
        .iter()
        .filter(|candidate| !is_blessed_target_basename(&candidate.path))
        .filter_map(|candidate| {
            match crate::exec_env_reaper::lsof_holder_probe(&candidate.path) {
                crate::exec_env_reaper::HolderCheck::None => {}
                crate::exec_env_reaper::HolderCheck::Held(_)
                | crate::exec_env_reaper::HolderCheck::Unknown(_) => return None,
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
    let parsed = DateTime::parse_from_rfc3339(rfc3339).ok()?.with_timezone(&Utc);
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
            remediation: format!(
                "if this worktree was already removed by hand, drop the stale row: \
                 `tachi worktree remove --path {}` (or edit ~/.tachi/worktrees.json if the \
                 path is already gone and the CLI has nothing left to canonicalize)",
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
             `tachi worktree remove --path {0}` once a human/Oz confirms it is done",
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
        assert!(warning.message.contains("build_target"), "{}", warning.message);
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
