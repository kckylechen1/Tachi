//! Orphan build-artifact reaper (#894 S2b) — the bytes nobody is holding.
//!
//! S1 tracks leases (`exec_envs`), S2a tracks the bytes those leases own
//! (`exec_env_resources`). Neither sees the *orphans*: a build target left
//! behind by a process that died, owned by no lease, in no ledger. The
//! measured case (2026-07-13): 78 GB under `/private/tmp`, 61 GB of it a
//! single dead codex bootstrap target — zero processes holding it, mtime
//! frozen the night before.
//!
//! ## The reap predicate (all three, no override)
//!
//! 1. **name match** — a directory whose name says "build artifact"
//!    (`*-target`, `*cargo-home*`; see [`classify_orphan_dir_name`]),
//! 2. **stale** — nothing under it has been touched for `max_age_days`
//!    (default 7),
//! 3. **unheld** — a holder check *positively proves* no process has a file
//!    open under it.
//!
//! Plus the ledger gates from S2a: a resource with a live binding
//! (`released_at IS NULL`) or a `quarantined` row is never reclaimed.
//!
//! ## Fail-closed holder check
//!
//! [`HolderCheck`] has three states, not two. `lsof` missing, permission
//! chatter on stderr (which means the walk was *partial*, so "found nothing"
//! proves nothing), or an unexpected exit all yield [`HolderCheck::Unknown`],
//! and Unknown never reclaims — it is reported as a skip with its reason. The
//! only thing that unlocks a delete is a clean, complete "nothing open here".
//!
//! ## The ledger is written even for strangers
//!
//! An orphan that was never registered (the dead codex target) is *inserted*
//! into `exec_env_resources` first (`kind=build_target`, reclaim reason
//! `unmanaged`) and only then reclaimed through the one S2a reclaim path —
//! so bytes freed by this reaper land in the same ledger, with the same
//! `reclaiming → delete → reclaimed_bytes` ordering, as bytes freed by
//! `safe_merge`. Nothing is deleted off the books.
//!
//! NOTE on the reclaim reason: `exec_env_resources` has no "managed" column
//! (S2a's schema is frozen and this knife does not touch it), so
//! *unmanaged* is carried by `reclaim_reason = 'unmanaged'` — which is also
//! the key the byte report groups on (`safe_merge` / `expired` / `orphan` /
//! `unmanaged`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use memcore::{
    ExecEnvResource, MemoryError, NewExecEnvResource, ResourceKind, ResourceReclaimOutcome,
    ResourceState,
};

use tachi_clean::wt_clean::OutputFormat;

// The staleness default lives with the clap flag it defaults
// (`tachi_bootstrap::cli::DEFAULT_ORPHAN_REAP_MAX_AGE_DAYS`); this module takes
// it from the caller so there is exactly one number, not two that can drift.

/// How deep under a scan root a candidate may sit. Scratch roots are shallow
/// by nature (`/private/tmp/<something>-target`); a deep walk of `~/.cache`
/// is not worth the stat storm.
const DEFAULT_MAX_DEPTH: usize = 3;

const SECS_PER_DAY: u64 = 24 * 60 * 60;

// ── Holder check ────────────────────────────────────────────────────────────

/// Whether any process holds a file under a candidate directory.
///
/// Three states on purpose. A delete path may only ever act on [`Self::None`],
/// which means "we ran a *complete* check and it found nothing". Anything we
/// could not verify is [`Self::Unknown`] and is skipped with its reason —
/// never silently treated as free (the `_ => false` shape in the older
/// worktree sweep is exactly the fail-open we refuse to copy onto a
/// 61-GB-deleting path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HolderCheck {
    /// Complete check, nothing open under the path.
    None,
    /// At least one process has a file open under the path.
    Held(Vec<String>),
    /// The check could not be trusted (no `lsof`, partial walk, odd exit).
    Unknown(String),
}

impl HolderCheck {
    /// One-line rendering for reports.
    pub(crate) fn describe(&self) -> String {
        match self {
            HolderCheck::None => "none".to_string(),
            HolderCheck::Held(holders) => format!("held by {}", holders.join(", ")),
            HolderCheck::Unknown(reason) => format!("unknown ({reason})"),
        }
    }
}

/// Injectable holder probe — the real one shells out to `lsof`; tests pass a
/// closure so the decision logic is exercised without depending on the host's
/// process table.
pub(crate) type HolderProbe = dyn Fn(&Path) -> HolderCheck;

/// Real probe: `lsof +D <dir>` (recursive — a live `cargo` holds files deep
/// inside the target, not just at its root).
pub(crate) fn lsof_holder_probe(path: &Path) -> HolderCheck {
    match Command::new("lsof").arg("+D").arg(path).output() {
        Ok(out) => interpret_lsof(
            out.status.code(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        ),
        // No lsof on this host ⇒ we cannot prove "unheld" ⇒ nothing is
        // reclaimed. Loud, not silent.
        Err(err) => HolderCheck::Unknown(format!("cannot run lsof: {err}")),
    }
}

/// Pure interpreter for an `lsof +D` run — the part worth testing.
///
/// * data lines on stdout ⇒ [`HolderCheck::Held`]
/// * anything on stderr ⇒ [`HolderCheck::Unknown`]: lsof warns (e.g. "can't
///   stat()", "Permission denied") when it could not descend part of the tree,
///   and a partial walk that "found nothing" is not proof of nothing.
/// * exit 0/1 with clean stdout+stderr ⇒ [`HolderCheck::None`] (1 is lsof's
///   documented "no matching files" status)
/// * any other exit / signal ⇒ [`HolderCheck::Unknown`]
fn interpret_lsof(exit_code: Option<i32>, stdout: &str, stderr: &str) -> HolderCheck {
    // First stdout line is the COMMAND/PID header; the rest are holders.
    let holders: Vec<String> = stdout
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    if !holders.is_empty() {
        return HolderCheck::Held(holders);
    }

    let noise = stderr.trim();
    if !noise.is_empty() {
        let first = noise.lines().next().unwrap_or(noise);
        return HolderCheck::Unknown(format!("lsof walk incomplete: {first}"));
    }

    match exit_code {
        Some(0) | Some(1) => HolderCheck::None,
        Some(code) => HolderCheck::Unknown(format!("lsof exited with status {code}")),
        None => HolderCheck::Unknown("lsof terminated by a signal".to_string()),
    }
}

// ── Candidates ──────────────────────────────────────────────────────────────

/// A directory that *looks like* a reclaimable build artifact. Being a
/// candidate says nothing about whether it may be deleted — that is
/// [`decide_reap`].
#[derive(Debug, Clone)]
pub(crate) struct OrphanCandidate {
    pub(crate) path: PathBuf,
    pub(crate) kind: ResourceKind,
    pub(crate) bytes: u64,
    pub(crate) age_days: u64,
    pub(crate) holders: HolderCheck,
}

/// Name-match half of the predicate: which resource kind (if any) a directory
/// name claims to be.
///
/// Deliberately narrow. A bare `target` is NOT matched: under a scan root such
/// as `~/.cache` that name also belongs to live repository checkouts, and the
/// blast radius of a false positive here is a multi-GB delete. The measured
/// orphan population (#894 S2b) is `*-target` and `*cargo-home*`.
pub(crate) fn classify_orphan_dir_name(name: &str) -> Option<ResourceKind> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with("-target") || lower.ends_with("_target") {
        return Some(ResourceKind::BuildTarget);
    }
    if lower.contains("cargo-home") || lower.contains("cargo_home") {
        return Some(ResourceKind::ScratchDir);
    }
    None
}

/// Scan roots for orphan candidates. Read-only: measures, ages, and probes,
/// but never writes to disk or to the ledger.
///
/// A scan root is never itself a candidate (`depth > 0` below) — pointing the
/// reaper at a root only ever authorizes it to look *inside*, never to delete
/// the root you handed it.
pub(crate) fn scan_orphan_candidates(
    roots: &[PathBuf],
    protected: &[PathBuf],
    now: SystemTime,
    probe: &HolderProbe,
) -> Vec<OrphanCandidate> {
    let mut candidates = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            if is_protected(&dir, protected) {
                continue;
            }
            let name = dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            // A matching directory is a leaf: never descend into a target to
            // find nested "targets".
            if depth > 0 {
                if let Some(kind) = classify_orphan_dir_name(&name) {
                    candidates.push(OrphanCandidate {
                        bytes: dir_size(&dir),
                        age_days: age_days(&dir, now),
                        holders: probe(&dir),
                        kind,
                        path: dir,
                    });
                    continue;
                }
            }
            if depth >= DEFAULT_MAX_DEPTH {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                // symlink_metadata: never follow a symlink out of the scan
                // root (a symlinked "…-target" must not become a delete
                // candidate for whatever it points at).
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.is_dir() {
                    stack.push((path, depth + 1));
                }
            }
        }
    }
    candidates.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    candidates
}

fn is_protected(dir: &Path, protected: &[PathBuf]) -> bool {
    protected.iter().any(|p| dir == p || dir.starts_with(p))
}

/// Default scan roots: the scratch volumes where dead build artifacts pile up.
pub(crate) fn default_orphan_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        roots.push(PathBuf::from(tmpdir));
    }
    roots.push(PathBuf::from("/private/tmp"));
    roots.push(std::env::temp_dir());
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".cache"));
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Paths the reaper refuses to consider at all.
///
/// The shared `CARGO_TARGET_DIR` matches `*-target` and is the one target on
/// this machine that is *supposed* to persist across every worktree (it is the
/// serial-build cache). Its lifecycle belongs to the disk governor, not to an
/// orphan reaper that would happily nuke a week-idle shared cache and cost the
/// next build an hour.
pub(crate) fn protected_paths() -> Vec<PathBuf> {
    tachi_clean::wt_open::default_shared_cargo_target_dir()
        .into_iter()
        .collect()
}

// ── Decision ────────────────────────────────────────────────────────────────

/// Ledger `reclaim_reason` this reaper stamps, and the bucket its bytes are
/// reported under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReclaimReason {
    /// A resource already on the books, aged out with no holder and no binding.
    Orphan,
    /// Never registered by anyone (a stranger's dead target) — booked first,
    /// then reclaimed.
    Unmanaged,
}

impl ReclaimReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReclaimReason::Orphan => "orphan",
            ReclaimReason::Unmanaged => "unmanaged",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SkipReason {
    TooYoung { age_days: u64, max_age_days: u64 },
    Held(String),
    HolderCheckInconclusive(String),
    BoundByLease { active_bindings: i64 },
    Quarantined,
    AlreadyReclaimed,
}

impl SkipReason {
    pub(crate) fn describe(&self) -> String {
        match self {
            SkipReason::TooYoung {
                age_days,
                max_age_days,
            } => format!("too young ({age_days}d < {max_age_days}d)"),
            SkipReason::Held(holders) => format!("in use ({holders})"),
            SkipReason::HolderCheckInconclusive(reason) => format!(
                "holder check inconclusive ({reason}); refusing to reclaim what we cannot prove is free"
            ),
            SkipReason::BoundByLease { active_bindings } => {
                format!("{active_bindings} live lease binding(s)")
            }
            SkipReason::Quarantined => "quarantined resource (a human owns it)".to_string(),
            SkipReason::AlreadyReclaimed => {
                "ledger row already reclaimed but the path is back on disk; \
                 re-registration is not this reaper's call"
                    .to_string()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReapDecision {
    Reclaim(ReclaimReason),
    Skip(SkipReason),
}

/// THE eligibility gate (pure). Age AND unheld AND unbound AND not fenced —
/// every one of them, and `--force` waives none of them (it only waives
/// dry-run).
pub(crate) fn decide_reap(
    candidate: &OrphanCandidate,
    max_age_days: u64,
    existing: Option<&ExecEnvResource>,
    active_bindings: i64,
) -> ReapDecision {
    if candidate.age_days < max_age_days {
        return ReapDecision::Skip(SkipReason::TooYoung {
            age_days: candidate.age_days,
            max_age_days,
        });
    }
    match &candidate.holders {
        HolderCheck::Held(holders) => {
            return ReapDecision::Skip(SkipReason::Held(holders.join(", ")))
        }
        HolderCheck::Unknown(reason) => {
            return ReapDecision::Skip(SkipReason::HolderCheckInconclusive(reason.clone()))
        }
        HolderCheck::None => {}
    }
    if let Some(resource) = existing {
        match resource.state {
            ResourceState::Quarantined => return ReapDecision::Skip(SkipReason::Quarantined),
            ResourceState::Reclaimed => return ReapDecision::Skip(SkipReason::AlreadyReclaimed),
            // active / reclaiming / reclaim_failed are all re-enterable.
            ResourceState::Active | ResourceState::Reclaiming | ResourceState::ReclaimFailed => {}
        }
    }
    if active_bindings > 0 {
        return ReapDecision::Skip(SkipReason::BoundByLease { active_bindings });
    }
    ReapDecision::Reclaim(match existing {
        Some(_) => ReclaimReason::Orphan,
        None => ReclaimReason::Unmanaged,
    })
}

// ── Report ──────────────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct CandidateReport {
    pub(crate) path: String,
    pub(crate) kind: &'static str,
    pub(crate) bytes: u64,
    pub(crate) age_days: u64,
    pub(crate) holders: String,
    /// `reclaim` (eligible) or `skip`.
    pub(crate) decision: &'static str,
    pub(crate) reason: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ReclaimedReport {
    pub(crate) path: String,
    pub(crate) resource_id: String,
    pub(crate) kind: &'static str,
    pub(crate) reason: &'static str,
    /// Bytes the filesystem actually gave back (S2a stamps this only after the
    /// delete happened).
    pub(crate) reclaimed_bytes: i64,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ReapReport {
    pub(crate) action: &'static str,
    pub(crate) roots: Vec<String>,
    pub(crate) max_age_days: u64,
    pub(crate) dry_run: bool,
    pub(crate) candidates: Vec<CandidateReport>,
    pub(crate) reclaimed: Vec<ReclaimedReport>,
    /// Bytes freed by THIS run.
    pub(crate) reclaimed_bytes: i64,
    /// Ledger-wide bytes freed per `reclaim_reason`
    /// (`safe_merge` / `expired` / `orphan` / `unmanaged` / …), so the disk
    /// story reads the same no matter which knife freed the bytes.
    pub(crate) bytes_by_reason: BTreeMap<String, i64>,
    pub(crate) warnings: Vec<String>,
    pub(crate) errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReapOptions {
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) max_age_days: u64,
    /// `false` (the default) = preview: measure, decide, report, touch nothing.
    pub(crate) force: bool,
}

// ── Run ─────────────────────────────────────────────────────────────────────

/// Scan → decide → (force only) reclaim through the S2a state machine.
///
/// Dry-run is a hard gate above every write: without `force` this function
/// makes no filesystem change and no ledger row — not even a measurement.
pub(crate) fn run_orphan_reap(
    conn: &mut rusqlite::Connection,
    opts: &ReapOptions,
    now: SystemTime,
    probe: &HolderProbe,
) -> ReapReport {
    let protected = protected_paths();
    let candidates = scan_orphan_candidates(&opts.roots, &protected, now, probe);

    let mut report = ReapReport {
        action: "reap-orphans",
        roots: opts.roots.iter().map(|r| r.display().to_string()).collect(),
        max_age_days: opts.max_age_days,
        dry_run: !opts.force,
        candidates: Vec::new(),
        reclaimed: Vec::new(),
        reclaimed_bytes: 0,
        bytes_by_reason: BTreeMap::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    for candidate in candidates {
        let path = candidate.path.display().to_string();
        let existing = match memcore::find_resource_by_path(conn, &path, candidate.kind) {
            Ok(existing) => existing,
            Err(err) => {
                report
                    .errors
                    .push(format!("ledger lookup failed for {path}: {err}"));
                continue;
            }
        };
        let active_bindings = match &existing {
            Some(resource) => match memcore::active_binding_count(conn, &resource.resource_id) {
                Ok(count) => count,
                Err(err) => {
                    report
                        .errors
                        .push(format!("binding count failed for {path}: {err}"));
                    continue;
                }
            },
            None => 0,
        };

        let decision = decide_reap(
            &candidate,
            opts.max_age_days,
            existing.as_ref(),
            active_bindings,
        );
        let (decision_label, reason) = match &decision {
            ReapDecision::Reclaim(reason) => ("reclaim", reason.as_str().to_string()),
            ReapDecision::Skip(skip) => ("skip", skip.describe()),
        };
        report.candidates.push(CandidateReport {
            path: path.clone(),
            kind: candidate.kind.as_str(),
            bytes: candidate.bytes,
            age_days: candidate.age_days,
            holders: candidate.holders.describe(),
            decision: decision_label,
            reason,
        });

        let ReapDecision::Reclaim(reason) = decision else {
            continue;
        };
        if !opts.force {
            // Preview: eligible is reported, nothing is booked and nothing is
            // deleted.
            continue;
        }

        match reclaim_candidate(conn, &candidate, existing, reason, probe) {
            Ok(Some(reclaimed)) => {
                report.reclaimed_bytes += reclaimed.reclaimed_bytes;
                report.reclaimed.push(reclaimed);
            }
            Ok(None) => {}
            Err(err) => report.warnings.push(err),
        }
    }

    match reclaimed_bytes_by_reason(conn) {
        Ok(by_reason) => report.bytes_by_reason = by_reason,
        Err(err) => report
            .warnings
            .push(format!("byte report by reason unavailable: {err}")),
    }

    report
}

/// Book the resource (inserting a row for an unmanaged stranger) and reclaim it
/// through S2a's single reclaim path. `Ok(None)` = the ledger refused (bound /
/// quarantined / already reclaimed) and said so; that is not an error, it is the
/// gate doing its job — it lands as a warning line via `Err` only when the
/// refusal is worth a human's eye.
fn reclaim_candidate(
    conn: &mut rusqlite::Connection,
    candidate: &OrphanCandidate,
    existing: Option<ExecEnvResource>,
    reason: ReclaimReason,
    probe: &HolderProbe,
) -> Result<Option<ReclaimedReport>, String> {
    let path = candidate.path.display().to_string();
    let resource_id = match existing {
        Some(resource) => resource.resource_id,
        None => {
            let resource_id = uuid::Uuid::new_v4().to_string();
            memcore::insert_resource(
                conn,
                &NewExecEnvResource {
                    resource_id: resource_id.clone(),
                    kind: candidate.kind,
                    path: path.clone(),
                    bytes: Some(clamp_bytes(candidate.bytes)),
                    created_at: String::new(),
                },
            )
            .map_err(|err| format!("cannot book unmanaged orphan {path}: {err}"))?;
            resource_id
        }
    };

    let outcome =
        memcore::reclaim_resource(conn, &resource_id, Some(reason.as_str()), |resource| {
            delete_resource_bytes(resource, probe)
        })
        .map_err(|err| format!("reclaim of {path} failed: {err}"))?;

    match outcome {
        ResourceReclaimOutcome::Reclaimed {
            resource_id,
            reclaimed_bytes,
        } => Ok(Some(ReclaimedReport {
            path,
            resource_id,
            kind: candidate.kind.as_str(),
            reason: reason.as_str(),
            reclaimed_bytes,
        })),
        // The ledger re-checks the binding refcount inside its own transaction;
        // a binding taken between our decision and the reclaim lands here.
        ResourceReclaimOutcome::BlockedByBinding {
            active_bindings, ..
        } => Err(format!(
            "skipped {path}: {active_bindings} live lease binding(s) appeared since the scan"
        )),
        ResourceReclaimOutcome::Quarantined { .. } => {
            Err(format!("skipped {path}: resource is quarantined"))
        }
        ResourceReclaimOutcome::AlreadyReclaimed { .. } => Err(format!(
            "skipped {path}: ledger says already reclaimed (path is back on disk)"
        )),
        ResourceReclaimOutcome::NotFound => {
            Err(format!("skipped {path}: resource row vanished mid-reclaim"))
        }
    }
}

/// The deleter S2a hands the filesystem work to. Runs AFTER `reclaiming` is
/// committed and OUTSIDE any transaction; must be idempotent (a re-entered
/// reclaim of an already-deleted path frees 0 bytes, and S2a assigns rather
/// than accumulates, so 0 cannot corrupt a prior count).
fn delete_resource_bytes(
    resource: &ExecEnvResource,
    probe: &HolderProbe,
) -> Result<i64, MemoryError> {
    let path = Path::new(&resource.path);
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        // Already gone: the idempotent re-entry path (crash after delete,
        // before the finishing commit).
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(MemoryError::Io(err)),
    };
    if meta.file_type().is_symlink() {
        return Err(MemoryError::InvalidArg(format!(
            "refusing to reclaim symlink {} (would delete through it)",
            resource.path
        )));
    }
    if !meta.is_dir() {
        return Err(MemoryError::InvalidArg(format!(
            "refusing to reclaim non-directory {}",
            resource.path
        )));
    }
    // Defense in depth against the scan→delete window: the row is already
    // `reclaiming`, but the bytes are still there. A process that grabbed the
    // directory since the scan aborts the delete (row → `reclaim_failed`,
    // retryable) rather than losing a live build's cache.
    match probe(path) {
        HolderCheck::None => {}
        other => {
            return Err(MemoryError::InvalidArg(format!(
                "holder appeared before delete of {}: {}",
                resource.path,
                other.describe()
            )))
        }
    }
    let bytes = dir_size(path);
    std::fs::remove_dir_all(path).map_err(MemoryError::Io)?;
    Ok(clamp_bytes(bytes))
}

/// Ledger-wide bytes freed, grouped by `reclaim_reason` — the disk story
/// (`safe_merge` / `expired` / `orphan` / `unmanaged`) regardless of which
/// knife did the freeing. Only `reclaimed` rows count: `reclaimed_bytes` is
/// stamped only after a delete actually happened (#1029's whole point).
pub(crate) fn reclaimed_bytes_by_reason(
    conn: &rusqlite::Connection,
) -> Result<BTreeMap<String, i64>, String> {
    let reclaimed = memcore::list_resources(conn, Some(ResourceState::Reclaimed), None)
        .map_err(|err| err.to_string())?;
    let mut by_reason: BTreeMap<String, i64> = BTreeMap::new();
    for resource in reclaimed {
        let reason = resource
            .reclaim_reason
            .unwrap_or_else(|| "unspecified".to_string());
        let bytes = resource.reclaimed_bytes.unwrap_or(0);
        *by_reason.entry(reason).or_insert(0) += bytes;
    }
    Ok(by_reason)
}

// ── Measurement ─────────────────────────────────────────────────────────────

/// Apparent size of a directory tree (same metadata-only walk as
/// `tachi_clean::target_clean::dir_size`).
fn dir_size(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| dir_size(&entry.path()))
        .sum()
}

/// Age of the *newest* thing in the directory's top two levels, in days.
///
/// A directory's own mtime only moves when an entry is added or removed in it,
/// so a long-running build that keeps rewriting existing files can leave the
/// root mtime stale. Taking the max over the root and its immediate children
/// (where cargo's `.cargo-lock` / `CACHEDIR.TAG` / `debug/` live) makes the
/// staleness gate strictly harder to pass, never easier.
fn age_days(path: &Path, now: SystemTime) -> u64 {
    let mut newest = modified_at(path);
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.filter_map(Result::ok) {
            if let Some(modified) = modified_at(&entry.path()) {
                newest = match newest {
                    Some(current) if current >= modified => Some(current),
                    _ => Some(modified),
                };
            }
        }
    }
    let Some(newest) = newest else {
        // Unreadable metadata ⇒ age 0 ⇒ too young ⇒ never reclaimed.
        return 0;
    };
    now.duration_since(newest)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        / SECS_PER_DAY
}

fn modified_at(path: &Path) -> Option<SystemTime> {
    std::fs::symlink_metadata(path).ok()?.modified().ok()
}

/// Bytes are `u64` on the filesystem and `INTEGER` (i64) in SQLite; a value
/// that cannot fit is clamped rather than wrapped into a negative "freed" count.
fn clamp_bytes(bytes: u64) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

// ── Emit ────────────────────────────────────────────────────────────────────

pub(crate) fn emit_reap_report(report: &ReapReport, output: OutputFormat) -> Result<(), String> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|err| format!("serialize report: {err}"))?
        ),
        OutputFormat::Text => {
            let mode = if report.dry_run { "dry-run" } else { "force" };
            println!("tachi clean orphans ({mode})");
            println!("  max_age_days: {}", report.max_age_days);
            for root in &report.roots {
                println!("  root: {root}");
            }
            for candidate in &report.candidates {
                println!(
                    "  {} {} kind={} bytes={} age_days={} holders={} — {}",
                    candidate.decision,
                    candidate.path,
                    candidate.kind,
                    candidate.bytes,
                    candidate.age_days,
                    candidate.holders,
                    candidate.reason
                );
            }
            for reclaimed in &report.reclaimed {
                println!(
                    "  reclaimed: {} ({} bytes, reason={})",
                    reclaimed.path, reclaimed.reclaimed_bytes, reclaimed.reason
                );
            }
            println!("  reclaimed_bytes (this run): {}", report.reclaimed_bytes);
            for (reason, bytes) in &report.bytes_by_reason {
                println!("  ledger bytes by reason: {reason}={bytes}");
            }
            for warning in &report.warnings {
                println!("  warning: {warning}");
            }
            for error in &report.errors {
                println!("  error: {error}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
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
        Box::new(|_path: &Path| HolderCheck::None)
    }

    fn held_probe() -> Box<HolderProbe> {
        Box::new(|_path: &Path| HolderCheck::Held(vec!["cargo 4242".to_string()]))
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

    #[test]
    fn real_probe_never_reports_none_for_a_dir_with_an_open_file() {
        // Discrimination against the REAL lsof call site: hold a file open
        // under the candidate and assert the probe does not claim "nothing
        // open". Held (lsof present) and Unknown (lsof missing/partial) both
        // block a reclaim; None would be the fail-open bug.
        let root = unique_temp_dir("tachi-reaper-real-lsof");
        let target = make_target_dir(&root, "live-target");
        let _handle = std::fs::File::open(target.join("debug/artifact.rlib")).unwrap();

        let check = lsof_holder_probe(&target);
        assert_ne!(
            check,
            HolderCheck::None,
            "an open file handle under the dir must never read as unheld: {check:?}"
        );
        let candidate = OrphanCandidate {
            path: target.clone(),
            kind: ResourceKind::BuildTarget,
            bytes: 2048,
            age_days: 30,
            holders: check,
        };
        assert!(matches!(
            decide_reap(&candidate, 7, None, 0),
            ReapDecision::Skip(_)
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── scan ────────────────────────────────────────────────────────────────

    #[test]
    fn scan_selects_stale_named_dirs_and_ignores_the_rest() {
        let root = unique_temp_dir("tachi-reaper-scan");
        let dead = make_target_dir(&root, "codex-bootstrap-target");
        let _plain = make_target_dir(&root, "some-checkout"); // name does not match
        let cargo_home = root.join("nested/codex-cargo-home-1");
        std::fs::create_dir_all(&cargo_home).unwrap();

        let candidates =
            scan_orphan_candidates(&[root.clone()], &[], aged_now(30), &*unheld_probe());
        let paths: Vec<_> = candidates.iter().map(|c| c.path.clone()).collect();

        assert!(paths.contains(&dead), "stale *-target must be a candidate");
        assert!(
            paths.contains(&cargo_home),
            "*cargo-home* must be a candidate"
        );
        assert_eq!(candidates.len(), 2, "nothing else may be a candidate");

        let dead_candidate = candidates.iter().find(|c| c.path == dead).unwrap();
        assert_eq!(dead_candidate.kind, ResourceKind::BuildTarget);
        assert!(dead_candidate.bytes >= 2048, "bytes are measured");
        assert!(dead_candidate.age_days >= 29, "age is measured");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_skips_protected_shared_target() {
        let root = unique_temp_dir("tachi-reaper-protected");
        let shared = make_target_dir(&root, "sigil-shared-target");

        let candidates = scan_orphan_candidates(
            &[root.clone()],
            std::slice::from_ref(&shared),
            aged_now(30),
            &*unheld_probe(),
        );
        assert!(
            candidates.is_empty(),
            "the shared cargo target is protected: {candidates:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── decision ────────────────────────────────────────────────────────────

    #[test]
    fn young_dir_is_never_reclaimed() {
        let candidate = OrphanCandidate {
            path: PathBuf::from("/tmp/x-target"),
            kind: ResourceKind::BuildTarget,
            bytes: 1,
            age_days: 2,
            holders: HolderCheck::None,
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
    fn unmanaged_stranger_is_eligible_as_unmanaged() {
        let candidate = OrphanCandidate {
            path: PathBuf::from("/tmp/x-target"),
            kind: ResourceKind::BuildTarget,
            bytes: 1,
            age_days: 30,
            holders: HolderCheck::None,
        };
        assert_eq!(
            decide_reap(&candidate, 7, None, 0),
            ReapDecision::Reclaim(ReclaimReason::Unmanaged)
        );
    }

    // ── run: fixtures through the real ledger ───────────────────────────────

    #[test]
    fn dry_run_deletes_nothing_and_books_nothing() {
        let root = unique_temp_dir("tachi-reaper-dryrun");
        let dead = make_target_dir(&root, "dead-target");
        let mut store = open_store(&root);

        let report = run_orphan_reap(
            store.connection_mut(),
            &opts(&root, false),
            aged_now(30),
            &*unheld_probe(),
        );

        assert!(report.dry_run);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.candidates[0].decision, "reclaim");
        assert!(report.reclaimed.is_empty(), "preview must not reclaim");
        assert_eq!(report.reclaimed_bytes, 0);
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

        let report = run_orphan_reap(
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

    #[test]
    fn resource_with_a_live_binding_is_never_reclaimed() {
        let root = unique_temp_dir("tachi-reaper-bound");
        let bound = make_target_dir(&root, "shared-build-target");
        let mut store = open_store(&root);

        // A lease holding this target (the shared-CARGO_TARGET_DIR shape).
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
                created_at: String::new(),
            },
        )
        .unwrap();
        memcore::insert_resource(
            store.connection(),
            &NewExecEnvResource {
                resource_id: "res-bound".to_string(),
                kind: ResourceKind::BuildTarget,
                path: bound.display().to_string(),
                bytes: None,
                created_at: String::new(),
            },
        )
        .unwrap();
        memcore::bind_resource(store.connection(), "env-holder", "res-bound").unwrap();

        let report = run_orphan_reap(
            store.connection_mut(),
            &opts(&root, true),
            aged_now(30),
            &*unheld_probe(),
        );

        assert!(report.reclaimed.is_empty(), "a bound resource must survive");
        assert!(bound.join("debug/artifact.rlib").exists(), "bytes survive");
        let skip = report
            .candidates
            .iter()
            .find(|c| c.path == bound.display().to_string())
            .expect("bound target is still a candidate");
        assert_eq!(skip.decision, "skip");
        assert!(skip.reason.contains("binding"), "reason: {}", skip.reason);
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

        let report = run_orphan_reap(
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
        let probe = move |_path: &Path| {
            let n = calls.get();
            calls.set(n + 1);
            if n == 0 {
                HolderCheck::None // scan
            } else {
                HolderCheck::Held(vec!["cargo 1".to_string()]) // pre-delete recheck
            }
        };

        let report = run_orphan_reap(
            store.connection_mut(),
            &opts(&root, true),
            aged_now(30),
            &probe,
        );

        assert!(report.reclaimed.is_empty());
        assert!(target.join("debug/artifact.rlib").exists(), "bytes survive");
        assert!(!report.warnings.is_empty(), "the abort is reported");
        let rows = memcore::list_resources(store.connection(), None, None).unwrap();
        let row = &rows[0];
        assert_eq!(row.state, ResourceState::ReclaimFailed);
        assert!(row.reclaimed_bytes.is_none(), "no bytes may be claimed");

        let _ = std::fs::remove_dir_all(&root);
    }
}
