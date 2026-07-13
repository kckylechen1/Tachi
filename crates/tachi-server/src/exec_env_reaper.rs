//! Orphan build-artifact reaper (#894 S2b) — the bytes nobody is holding.
//!
//! S1 tracks leases (`exec_envs`), S2a tracks the bytes those leases own
//! (`exec_env_resources`). Neither sees the *orphans*: a build target left
//! behind by a process that died, owned by no lease, in no ledger. The
//! measured case (2026-07-13): 78 GB under `/private/tmp`, 61 GB of it a
//! single dead codex bootstrap target — zero processes holding it, mtime
//! frozen the night before.
//!
//! ## The reap predicate (every one of them, `--force` waives none)
//!
//! 1. **name match** — a directory whose name says "build artifact"
//!    (`*-target`, `*cargo-home*`; see [`classify_orphan_dir_name`]),
//! 2. **not protected** — the live shared build cache, and any target dir a
//!    *running* build names, are refused outright ([`protected_paths`]),
//! 3. **stale** — nothing *anywhere* under it (a full recursive walk, see
//!    [`staleness`]) has been touched for `max_age_days` (default 7),
//! 4. **unbound / not quarantined** — the S2a ledger gates: a resource with a
//!    live binding (`released_at IS NULL`) or a `quarantined` row is never
//!    reclaimed,
//! 5. **unheld** — a holder check *positively proves* no process has a file
//!    open under it.
//!
//! ## Cheap gates first
//!
//! The order above is the order they run in, and it is load-bearing: everything
//! that costs a syscall storm (the recursive byte measurement, `lsof +D` — which
//! walks the tree too) runs **only** on candidates that already survived name,
//! protection, staleness and the ledger. Round-2 fix: the first cut measured and
//! probed *every* name-matched directory and only then asked whether the thing
//! was one day old — minutes of stat storm across a 61 GB tree to decide nothing.
//!
//! ## Fail-closed, three times
//!
//! Every gate that can fail to *know* answers "then we do not delete":
//!
//! * [`HolderCheck`] has three states, not two. `lsof` missing, permission
//!   chatter on stderr (which means the walk was *partial*, so "found nothing"
//!   proves nothing), or an unexpected exit all yield [`HolderCheck::Unknown`],
//!   and Unknown never reclaims. Nor does *unprobed*: [`OrphanCandidate`] holds
//!   `Option<HolderCheck>`, and `None` is skipped exactly like `Unknown` — the
//!   fence is in the type, not in remembering to check.
//! * [`Staleness::Unprovable`] is the same shape for the age walk: an unreadable
//!   entry means the walk was partial, and a partial walk that "found nothing
//!   fresh" proves nothing.
//! * [`Protection`] surfaces *why* it could not compute a protected path instead
//!   of returning a quietly empty set (the first cut collected a `Result` into a
//!   `Vec`, so "HOME is unset" silently became "nothing is protected").
//!
//! The existing worktree sweep's `_ => false` shape (unknown ⇒ "not active") is
//! the fail-open we refuse to copy onto a path that deletes 61 GB.
//!
//! ## The ledger is written even for strangers
//!
//! An orphan that was never registered (the dead codex target) is *booked* into
//! `exec_env_resources` first (`kind=build_target`, reclaim reason `unmanaged`)
//! and only then reclaimed through the one S2a reclaim path — so bytes freed by
//! this reaper land in the same ledger, with the same `reclaiming → delete →
//! reclaimed_bytes` ordering, as bytes freed by `safe_merge`. Nothing is deleted
//! off the books.
//!
//! Booking uses the S2a API as shipped, not the `reregister_resource` this
//! module was drafted against before S2a landed:
//!
//! ```ignore
//! pub fn insert_resource(
//!     conn: &mut Connection,
//!     res: &NewExecEnvResource,
//! ) -> Result<RegisterOutcome, MemoryError>;
//!
//! pub enum RegisterOutcome {
//!     Registered { resource_id: String },                 // no row for (path, kind): fresh insert
//!     Revived    { resource_id: String,                   // a `reclaimed` row came back to `active`
//!                  previous_resource_id: String,
//!                  previous_reclaimed_bytes: Option<i64> },
//! }
//! ```
//!
//! `insert_resource` is only safe to call when the (path, kind) row is absent or
//! `reclaimed` — any other state (`active`/`reclaiming`/`reclaim_failed`/
//! `quarantined`) bounces off `MemoryError::Duplicate`. There is no `Live` or
//! `Quarantined` outcome variant: [`cheap_verdict`]'s `ReclaimReason` already
//! tells this module which case it is in before it ever calls `insert_resource` —
//! `Unmanaged` means no row or a `reclaimed` tombstone (insert/revive is safe),
//! `Orphan` means the row is already on the books in a re-enterable state (use
//! its `resource_id` directly, no insert call). Quarantined rows never reach
//! [`reclaim_candidate`] at all — `cheap_verdict` skips them upstream.
//!
//! `(path, kind)` is UNIQUE, so once a path has been reclaimed its tombstone row
//! owns that key forever. The population this reaper exists for is precisely the
//! target that is *reborn under the same name* every time a lane runs, so a
//! reclaimed row must be revivable — the first cut answered `Skip(already
//! reclaimed)` and would have skipped every repeat offender forever.
//!
//! The id in `res` is a *proposal*, used only on the `Registered` branch; on the
//! `Revived` branch the ledger's own id comes back and is authoritative.
//!
//! **Historical bytes on revive.** `insert_resource`'s revive clears the
//! tombstone row's `reclaimed_bytes`/`reclaim_reason` and flips it back to
//! `active` — so the instant a revive commits, those bytes drop out of
//! [`reclaimed_bytes_by_reason`]'s ledger-wide sum (it only sums rows currently
//! `reclaimed`). They are not lost: `RegisterOutcome::Revived` hands back
//! `previous_reclaimed_bytes`, and this module carries it forward on
//! [`ReclaimedReport::revived_previous_bytes`] / [`ReapReport::revived_bytes`]
//! rather than letting the ledger-wide grouping silently shrink.
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
    ExecEnvResource, MemoryError, NewExecEnvResource, RegisterOutcome, ResourceKind,
    ResourceReclaimOutcome, ResourceState,
};

use tachi_clean::wt_clean::OutputFormat;
use tachi_clean::wt_open::SHARED_CARGO_TARGET_DIR_ENV;

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
/// never silently treated as free.
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

// ── Protection ──────────────────────────────────────────────────────────────

/// The env var `cargo` itself reads, and the one this repo actually exports for
/// every build, every dispatched lane and CI.
///
/// **Round-2 fix (the near-miss).** The first cut derived its entire protected
/// set from `tachi_clean::wt_open::default_shared_cargo_target_dir()`, which
/// reads [`SHARED_CARGO_TARGET_DIR_ENV`] — the override for what gets *written
/// into a managed worktree's `.cargo/config.toml`* (#484), a variable nothing on
/// this machine sets. The live 16 GB shared cache was inside a default scan root
/// (`~/.cache`), name-matched `*-target`, and unheld between builds; it survived
/// only because that helper's *fallback* happens to spell the same default path.
/// Point `CARGO_TARGET_DIR` anywhere else — the codex lanes point it under
/// `/private/tmp`, which is also a default scan root — and a week-idle live build
/// cache was one `tachi clean orphans --force` away from deletion.
const CARGO_TARGET_DIR_ENV: &str = "CARGO_TARGET_DIR";

/// Paths the reaper refuses to consider, and everything that went wrong while
/// working out what they are.
///
/// The warnings are not decoration: an incomplete protected set has to reach the
/// operator. The first cut collected a `Result` straight into a `Vec`, so "HOME
/// is unset" quietly became *nothing is protected*.
#[derive(Debug, Clone, Default)]
pub(crate) struct Protection {
    paths: Vec<PathBuf>,
    pub(crate) warnings: Vec<String>,
}

impl Protection {
    pub(crate) fn new(paths: impl IntoIterator<Item = PathBuf>, warnings: Vec<String>) -> Self {
        let mut all: Vec<PathBuf> = Vec::new();
        for path in paths {
            // Keep the canonical spelling too: `/tmp/x` and `/private/tmp/x` are
            // the same directory on macOS, and a prefix test that only knows one
            // of the two protects neither.
            if let Ok(real) = std::fs::canonicalize(&path) {
                all.push(real);
            }
            all.push(path);
        }
        all.sort();
        all.dedup();
        Self {
            paths: all,
            warnings,
        }
    }

    pub(crate) fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// True if `dir` must never be reclaimed: it *is* a protected path, sits
    /// *under* one, or *contains* one.
    ///
    /// That last case is not paranoia — `remove_dir_all` on an ancestor takes the
    /// protected directory with it, so an ancestor of a protected path is exactly
    /// as untouchable as a descendant. The first cut only tested one direction.
    pub(crate) fn covers(&self, dir: &Path) -> bool {
        let real = std::fs::canonicalize(dir).ok();
        self.paths.iter().any(|protected| {
            overlaps(dir, protected)
                || real
                    .as_deref()
                    .is_some_and(|real| overlaps(real, protected))
        })
    }
}

/// Prefix relation in *either* direction (see [`Protection::covers`]).
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Everything the reaper must never touch, from every source that knows where a
/// live build cache is:
///
/// 1. `CARGO_TARGET_DIR` — what cargo actually reads, and what this repo exports,
/// 2. `TACHI_SHARED_CARGO_TARGET_DIR` — the Tachi-managed override written into
///    managed worktrees (#484),
/// 3. `~/.cache/sigil-shared-target` — the documented default, protected even
///    when neither variable is set, because that is the cache this machine builds
///    against,
/// 4. every target dir a *running* build names ([`live_build_target_dirs`]).
///
/// Live **lease bindings** are the fifth class, and they are enforced where they
/// are known — in [`cheap_verdict`], and again inside S2a's own reclaim
/// transaction — rather than here, so a bound resource still appears in the
/// report as a visible `skip` carrying its refcount instead of silently vanishing
/// from the scan.
pub(crate) fn protected_paths() -> Protection {
    let (mut paths, mut warnings) = live_build_target_dirs();

    for var in [CARGO_TARGET_DIR_ENV, SHARED_CARGO_TARGET_DIR_ENV] {
        let Some(raw) = std::env::var_os(var) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let path = PathBuf::from(&raw);
        if path.is_absolute() {
            paths.push(path);
        } else {
            // A relative target-dir resolves against the cwd of whichever process
            // set it, which we do not know, so it cannot be matched against a scan
            // root. Say so out loud rather than pretend it is covered.
            warnings.push(format!(
                "{var} is a relative path ('{}'), so it cannot be matched against the scan roots \
                 and is NOT in the protected set",
                path.display()
            ));
        }
    }

    match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(home) if !home.is_empty() => paths.push(
            PathBuf::from(home)
                .join(".cache")
                .join("sigil-shared-target"),
        ),
        _ => warnings.push(
            "HOME (and USERPROFILE) is unset: the default shared cargo target dir \
             (~/.cache/sigil-shared-target) cannot be resolved and is NOT in the protected set"
                .to_string(),
        ),
    }

    Protection::new(paths, warnings)
}

/// Target dirs that a *running* build owns.
///
/// The `lsof` gate is the fail-closed fence, but it only sees open file
/// descriptors: a `cargo` between compile units — or one whose fds all sit under
/// a subdirectory `lsof` could not descend — can hold nothing measurable while
/// still owning the cache. So the process table is read directly, and every
/// `--target-dir` / `CARGO_TARGET_DIR=` it mentions joins the protected set.
///
/// No attempt is made to check *which* binary a line belongs to: over-protection
/// is the safe direction here. The cost of protecting one directory too many is
/// that it survives; the cost of missing one is a deleted build cache.
///
/// A `ps` that will not run is a **warning, not a hard stop**: this is
/// defense-in-depth on top of the holder probe, which is fail-closed on its own
/// (no `lsof` ⇒ `Unknown` ⇒ nothing is reclaimed).
fn live_build_target_dirs() -> (Vec<PathBuf>, Vec<String>) {
    // -A: every process, not just this terminal's. -ww: never truncate the
    // command line at terminal width — a truncated line silently drops the very
    // argument we came here to read.
    let unavailable = |why: String| {
        vec![format!(
            "process scan unavailable ({why}): the target dirs of live builds are NOT in the \
             protected set for this run; the fail-closed holder probe is the only fence left"
        )]
    };
    match Command::new("ps").args(["-Awwo", "command="]).output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut dirs = Vec::new();
            let mut warnings = Vec::new();
            for path in text.lines().flat_map(target_dirs_from_process_line) {
                if path.is_absolute() {
                    dirs.push(path);
                } else {
                    warnings.push(format!(
                        "a live build names a relative target dir ('{}'), which resolves against \
                         that process's cwd and cannot be protected from here",
                        path.display()
                    ));
                }
            }
            dirs.sort();
            dirs.dedup();
            warnings.sort();
            warnings.dedup();
            (dirs, warnings)
        }
        Ok(out) => (Vec::new(), unavailable(format!("ps exited {}", out.status))),
        Err(err) => (Vec::new(), unavailable(err.to_string())),
    }
}

/// Pure: every target dir named by one process-table line. Handles
/// `--target-dir X`, `--target-dir=X`, and a `CARGO_TARGET_DIR=X` token (an
/// `env`-prefixed invocation, or a `ps` that prints the environment).
fn target_dirs_from_process_line(line: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut tokens = line.split_whitespace();
    while let Some(token) = tokens.next() {
        if let Some(value) = token
            .strip_prefix("--target-dir=")
            .or_else(|| token.strip_prefix("CARGO_TARGET_DIR="))
        {
            if !value.is_empty() {
                dirs.push(PathBuf::from(value));
            }
        } else if token == "--target-dir" {
            if let Some(value) = tokens.next() {
                dirs.push(PathBuf::from(value));
            }
        }
    }
    dirs
}

// ── Candidates ──────────────────────────────────────────────────────────────

/// How old the *newest* byte anywhere under a candidate is, resolved against the
/// `now - max_age_days` cutoff.
///
/// Three states, for the same reason [`HolderCheck`] has three: a walk that could
/// not read part of the tree and "found nothing fresh" has proved nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Staleness {
    /// Nothing anywhere under the tree has been touched since the cutoff, and the
    /// whole tree was readable. The only state that passes the staleness gate.
    Stale { age_days: u64 },
    /// Something under the tree is newer than the cutoff.
    Fresh { age_days: u64 },
    /// The walk was partial (unreadable metadata / directory) — fail-closed.
    Unprovable(String),
}

impl Staleness {
    pub(crate) fn age_days(&self) -> Option<u64> {
        match self {
            Staleness::Stale { age_days } | Staleness::Fresh { age_days } => Some(*age_days),
            Staleness::Unprovable(_) => None,
        }
    }
}

/// A directory that *looks like* a reclaimable build artifact. Being a
/// candidate says nothing about whether it may be deleted — that is
/// [`decide_reap`].
#[derive(Debug, Clone)]
pub(crate) struct OrphanCandidate {
    pub(crate) path: PathBuf,
    pub(crate) kind: ResourceKind,
    pub(crate) staleness: Staleness,
    /// Measured *only* for candidates that survived every cheap gate — a
    /// recursive byte walk of a 61 GB tree is not something to spend on a
    /// directory we are about to skip for being two days old.
    pub(crate) bytes: Option<u64>,
    /// Probed *only* for candidates that survived every cheap gate. `None` means
    /// **the probe never ran**, and [`decide_reap`] treats it exactly like
    /// [`HolderCheck::Unknown`]: unprovable ⇒ never reclaimed.
    pub(crate) holders: Option<HolderCheck>,
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

/// Scan roots for orphan candidates. Read-only, and **cheap**: name match,
/// protection, and a short-circuiting staleness walk. It does not measure bytes
/// and it does not probe holders — those cost a recursive stat storm each and are
/// spent only on the survivors, in [`run_orphan_reap`].
///
/// A scan root is never itself a candidate (`depth > 0` below) — pointing the
/// reaper at a root only ever authorizes it to look *inside*, never to delete
/// the root you handed it.
pub(crate) fn scan_orphan_candidates(
    roots: &[PathBuf],
    protection: &Protection,
    now: SystemTime,
    max_age_days: u64,
) -> Vec<OrphanCandidate> {
    let cutoff = staleness_cutoff(now, max_age_days);
    let mut candidates = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            if protection.covers(&dir) {
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
                        staleness: staleness(&dir, now, cutoff),
                        path: dir,
                        kind,
                        bytes: None,
                        holders: None,
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
    // Stalest first, then path: deterministic without a byte measurement we have
    // deliberately not taken yet.
    candidates.sort_by(|a, b| {
        b.staleness
            .age_days()
            .cmp(&a.staleness.age_days())
            .then_with(|| a.path.cmp(&b.path))
    });
    candidates
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

// ── Decision ────────────────────────────────────────────────────────────────

/// Ledger `reclaim_reason` this reaper stamps, and the bucket its bytes are
/// reported under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReclaimReason {
    /// A resource already on the books, aged out with no holder and no binding.
    Orphan,
    /// Registered by nobody (a stranger's dead target, or a path reborn after a
    /// previous reclaim) — booked first, then reclaimed.
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
    StalenessUnprovable(String),
    Held(String),
    HolderCheckInconclusive(String),
    BoundByLease { active_bindings: i64 },
    Quarantined,
}

impl SkipReason {
    pub(crate) fn describe(&self) -> String {
        match self {
            SkipReason::TooYoung {
                age_days,
                max_age_days,
            } => format!("too young ({age_days}d < {max_age_days}d)"),
            SkipReason::StalenessUnprovable(reason) => format!(
                "staleness unprovable ({reason}); a partial walk that found nothing fresh proves \
                 nothing"
            ),
            SkipReason::Held(holders) => format!("in use ({holders})"),
            SkipReason::HolderCheckInconclusive(reason) => format!(
                "holder check inconclusive ({reason}); refusing to reclaim what we cannot prove is free"
            ),
            SkipReason::BoundByLease { active_bindings } => {
                format!("{active_bindings} live lease binding(s)")
            }
            SkipReason::Quarantined => "quarantined resource (a human owns it)".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReapDecision {
    Reclaim(ReclaimReason),
    Skip(SkipReason),
}

/// The gates that cost nothing — no filesystem walk, no process table: staleness
/// (already computed by the scan), ledger state, lease bindings.
///
/// Run this *before* measuring bytes or probing holders. `Ok(reason)` means "this
/// one is worth paying for the expensive probes"; it is NOT a decision to delete
/// — [`decide_reap`] still has the last word, and it needs the holder probe to
/// say yes.
pub(crate) fn cheap_verdict(
    candidate: &OrphanCandidate,
    max_age_days: u64,
    existing: Option<&ExecEnvResource>,
    active_bindings: i64,
) -> Result<ReclaimReason, SkipReason> {
    // The scan resolved staleness against the SAME `max_age_days` cutoff, so this
    // gate reads its verdict rather than re-deriving one from a rounded day count
    // — two derivations of one number is how a gate and its walk come to disagree.
    match &candidate.staleness {
        Staleness::Fresh { age_days } => {
            return Err(SkipReason::TooYoung {
                age_days: *age_days,
                max_age_days,
            })
        }
        Staleness::Unprovable(reason) => {
            return Err(SkipReason::StalenessUnprovable(reason.clone()))
        }
        Staleness::Stale { .. } => {}
    }

    if let Some(resource) = existing {
        if resource.state == ResourceState::Quarantined {
            return Err(SkipReason::Quarantined);
        }
    }
    if active_bindings > 0 {
        return Err(SkipReason::BoundByLease { active_bindings });
    }

    Ok(match existing.map(|resource| resource.state) {
        // A `reclaimed` row is a TOMBSTONE for a path that is back on disk: some
        // later build recreated the same name. It is not "already done" — it is
        // the whole population this reaper exists for, and nobody re-registered
        // the new bytes, so they are unmanaged again. `insert_resource` revives
        // the row (see `reclaim_candidate`); the first cut skipped it forever
        // and quietly retired the reaper from every path it had ever cleaned
        // once.
        Some(ResourceState::Reclaimed) | None => ReclaimReason::Unmanaged,
        // active / reclaiming / reclaim_failed: on the books, and all re-enterable.
        Some(_) => ReclaimReason::Orphan,
    })
}

/// THE eligibility gate (pure). Stale AND unbound AND not fenced AND *provably*
/// unheld — every one of them, and `--force` waives none (it only waives dry-run).
pub(crate) fn decide_reap(
    candidate: &OrphanCandidate,
    max_age_days: u64,
    existing: Option<&ExecEnvResource>,
    active_bindings: i64,
) -> ReapDecision {
    let reason = match cheap_verdict(candidate, max_age_days, existing, active_bindings) {
        Ok(reason) => reason,
        Err(skip) => return ReapDecision::Skip(skip),
    };
    match &candidate.holders {
        // Fail-closed by type: no probe ran, so nothing was proved. A caller that
        // forgets to probe gets a skip, not a delete.
        None => ReapDecision::Skip(SkipReason::HolderCheckInconclusive(
            "holder probe never ran".to_string(),
        )),
        Some(HolderCheck::Held(holders)) => {
            ReapDecision::Skip(SkipReason::Held(holders.join(", ")))
        }
        Some(HolderCheck::Unknown(why)) => {
            ReapDecision::Skip(SkipReason::HolderCheckInconclusive(why.clone()))
        }
        Some(HolderCheck::None) => ReapDecision::Reclaim(reason),
    }
}

// ── Report ──────────────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct CandidateReport {
    pub(crate) path: String,
    pub(crate) kind: &'static str,
    /// `None` = never measured, because a cheap gate skipped this candidate first.
    pub(crate) bytes: Option<u64>,
    /// `None` = the staleness walk was partial (see [`Staleness::Unprovable`]).
    pub(crate) age_days: Option<u64>,
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
    /// Set only when booking this candidate revived a `reclaimed` tombstone row
    /// (same `(path, kind)` reaped before, reborn, reaped again): the bytes the
    /// PREVIOUS incarnation freed. `insert_resource`'s revive clears that history
    /// off the row (state flips back to `active`), so it would otherwise vanish
    /// from [`ReapReport::bytes_by_reason`] the instant this run commits.
    pub(crate) revived_previous_bytes: Option<i64>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ReapReport {
    pub(crate) action: &'static str,
    pub(crate) roots: Vec<String>,
    /// What the run refused to look at, so an operator can *see* that the live
    /// build cache was fenced instead of taking it on faith.
    pub(crate) protected: Vec<String>,
    pub(crate) max_age_days: u64,
    pub(crate) dry_run: bool,
    pub(crate) candidates: Vec<CandidateReport>,
    pub(crate) reclaimed: Vec<ReclaimedReport>,
    /// Bytes freed by THIS run.
    pub(crate) reclaimed_bytes: i64,
    /// Ledger-wide bytes freed per `reclaim_reason`
    /// (`safe_merge` / `expired` / `orphan` / `unmanaged` / …), so the disk
    /// story reads the same no matter which knife freed the bytes. Only rows
    /// CURRENTLY `reclaimed` count — a row this run revived back to `active` no
    /// longer contributes here even though it once freed real bytes; see
    /// `revived_bytes`.
    pub(crate) bytes_by_reason: BTreeMap<String, i64>,
    /// Sum of `revived_previous_bytes` across everything this run reclaimed —
    /// bytes a prior reclaim freed at these same paths, which `bytes_by_reason`
    /// no longer counts because booking this run's orphan revived (and thereby
    /// cleared) that tombstone row. Kept here so the history is not silently
    /// dropped, not because `bytes_by_reason` needs correcting: the ledger's
    /// per-row bookkeeping is working as designed.
    pub(crate) revived_bytes: i64,
    pub(crate) warnings: Vec<String>,
    pub(crate) errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReapOptions {
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) max_age_days: u64,
    /// `false` (the default) = preview: decide, report, touch nothing.
    pub(crate) force: bool,
}

// ── Run ─────────────────────────────────────────────────────────────────────

/// Scan → cheap gates → (survivors only) measure + probe → decide → (force only)
/// reclaim through the S2a state machine.
///
/// Dry-run is a hard gate above every write: without `force` this function makes
/// no filesystem change and no ledger row.
pub(crate) fn run_orphan_reap(
    conn: &mut rusqlite::Connection,
    opts: &ReapOptions,
    now: SystemTime,
    probe: &HolderProbe,
) -> ReapReport {
    let protection = protected_paths();
    let candidates = scan_orphan_candidates(&opts.roots, &protection, now, opts.max_age_days);

    let mut report = ReapReport {
        action: "reap-orphans",
        roots: opts.roots.iter().map(|r| r.display().to_string()).collect(),
        protected: protection
            .paths()
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        max_age_days: opts.max_age_days,
        dry_run: !opts.force,
        candidates: Vec::new(),
        reclaimed: Vec::new(),
        reclaimed_bytes: 0,
        bytes_by_reason: BTreeMap::new(),
        revived_bytes: 0,
        // Anything that stopped the protected set from being complete is an
        // operator-visible warning on every run, force or not.
        warnings: protection.warnings.clone(),
        errors: Vec::new(),
    };

    for mut candidate in candidates {
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

        // Cheap gates first (staleness / ledger / bindings) …
        let decision = match cheap_verdict(
            &candidate,
            opts.max_age_days,
            existing.as_ref(),
            active_bindings,
        ) {
            Err(skip) => ReapDecision::Skip(skip),
            // … and only now the expensive ones, on a survivor: a full recursive
            // byte walk and an `lsof +D` (which walks the tree again).
            Ok(_) => {
                candidate.bytes = Some(dir_size(&candidate.path));
                candidate.holders = Some(probe(&candidate.path));
                decide_reap(
                    &candidate,
                    opts.max_age_days,
                    existing.as_ref(),
                    active_bindings,
                )
            }
        };

        let (decision_label, reason) = match &decision {
            ReapDecision::Reclaim(reason) => ("reclaim", reason.as_str().to_string()),
            ReapDecision::Skip(skip) => ("skip", skip.describe()),
        };
        report.candidates.push(CandidateReport {
            path: path.clone(),
            kind: candidate.kind.as_str(),
            bytes: candidate.bytes,
            age_days: candidate.staleness.age_days(),
            holders: candidate
                .holders
                .as_ref()
                .map(HolderCheck::describe)
                .unwrap_or_else(|| "not probed".to_string()),
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

        match reclaim_candidate(
            conn,
            &candidate,
            reason,
            existing.as_ref(),
            probe,
            &protection,
        ) {
            Ok(reclaimed) => {
                report.reclaimed_bytes += reclaimed.reclaimed_bytes;
                report.revived_bytes += reclaimed.revived_previous_bytes.unwrap_or(0);
                report.reclaimed.push(reclaimed);
            }
            // Every refusal is a warning line with its reason — a reclaim that did
            // not happen is never a silent skip.
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

/// Book (or revive) the resource and reclaim it through S2a's single reclaim
/// path. An `Err` is a refusal worth a human's eye and lands as a warning line;
/// it is never a silent skip.
fn reclaim_candidate(
    conn: &mut rusqlite::Connection,
    candidate: &OrphanCandidate,
    reason: ReclaimReason,
    existing: Option<&ExecEnvResource>,
    probe: &HolderProbe,
    protection: &Protection,
) -> Result<ReclaimedReport, String> {
    let path = candidate.path.display().to_string();

    // `insert_resource` only ever accepts an absent row or a `reclaimed`
    // tombstone — any other state bounces off `MemoryError::Duplicate`.
    // `cheap_verdict` already told us which case this is via `reason`:
    // `Unmanaged` ⇒ no row or a `reclaimed` tombstone (insert/revive is safe);
    // `Orphan` ⇒ the row is already on the books in a re-enterable state
    // (active/reclaiming/reclaim_failed) — reuse its id directly, an
    // `insert_resource` call here would only bounce off `Duplicate`.
    let (resource_id, revived_previous_bytes) = match reason {
        ReclaimReason::Orphan => {
            let resource_id = existing
                .ok_or_else(|| {
                    format!("internal: {path} decided Orphan but the ledger lookup found no row")
                })?
                .resource_id
                .clone();
            (resource_id, None)
        }
        ReclaimReason::Unmanaged => {
            let outcome = memcore::insert_resource(
                conn,
                &NewExecEnvResource {
                    resource_id: uuid::Uuid::new_v4().to_string(),
                    kind: candidate.kind,
                    path: path.clone(),
                    bytes: candidate.bytes.map(clamp_bytes),
                    created_at: String::new(),
                },
            )
            .map_err(|err| format!("cannot book orphan {path}: {err}"))?;
            match outcome {
                // The id we proposed is only used when the row is new; on a
                // revive the ledger's own (retired) id comes back instead.
                RegisterOutcome::Registered { resource_id } => (resource_id, None),
                RegisterOutcome::Revived {
                    resource_id,
                    previous_reclaimed_bytes,
                    ..
                } => (resource_id, previous_reclaimed_bytes),
            }
        }
    };

    let outcome =
        memcore::reclaim_resource(conn, &resource_id, Some(reason.as_str()), |resource| {
            delete_resource_bytes(resource, probe, protection)
        })
        .map_err(|err| format!("reclaim of {path} failed: {err}"))?;

    match outcome {
        ResourceReclaimOutcome::Reclaimed {
            resource_id,
            reclaimed_bytes,
        } => Ok(ReclaimedReport {
            path,
            resource_id,
            kind: candidate.kind.as_str(),
            reason: reason.as_str(),
            reclaimed_bytes,
            revived_previous_bytes,
        }),
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
        // For `Unmanaged` we just booked/revived this row moments ago, so this
        // can only be a concurrent reclaim of the same resource winning the
        // race. For `Orphan` the row was already on the books before this call
        // — same story, a concurrent reclaimer got there first.
        ResourceReclaimOutcome::AlreadyReclaimed { .. } => Err(format!(
            "skipped {path}: another reclaim of this resource finished first"
        )),
        // The deleter ran (our bytes really are gone), but by the time the
        // ledger went to stamp `reclaimed` the row had already moved out from
        // under it — a concurrent reclaim, a quarantine, or a re-registration
        // won the race. `freed_bytes` is deliberately NOT folded into this run's
        // `reclaimed_bytes`: memcore did not write it, so counting it here would
        // claim bytes no ledger row backs (#1029's whole point). It is only a
        // warning line, same as every other refusal.
        ResourceReclaimOutcome::LostRace {
            observed_state,
            freed_bytes,
            ..
        } => Err(format!(
            "skipped {path}: lost the reclaim race (now observed as {:?}); this run's deleter \
             freed {freed_bytes} bytes not recorded in the ledger",
            observed_state
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
    protection: &Protection,
) -> Result<i64, MemoryError> {
    let path = Path::new(&resource.path);

    // The last fence before `remove_dir_all`, and the reason it is here and not
    // only in the scan: the row handed to this deleter came out of the DB, and a
    // row can be booked by writers that never went through our scan. The protected
    // set is re-asserted at the exact line that deletes.
    if protection.covers(path) {
        return Err(MemoryError::InvalidArg(format!(
            "refusing to reclaim protected path {} (a live build cache: CARGO_TARGET_DIR, the \
             shared target dir, or the target of a running build)",
            resource.path
        )));
    }

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
/// `tachi_clean::target_clean::dir_size`). Expensive: only ever called on a
/// candidate that already survived every cheap gate.
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

/// `now - max_age_days`, saturating at the epoch.
fn staleness_cutoff(now: SystemTime, max_age_days: u64) -> SystemTime {
    now.checked_sub(Duration::from_secs(
        max_age_days.saturating_mul(SECS_PER_DAY),
    ))
    .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Staleness of the whole tree under `path`, as the module header has always
/// promised: the newest mtime *anywhere* under it, not just at depth 1.
///
/// **Round-2 fix.** The first cut looked at the root and its immediate children
/// only, while documenting "nothing under it has been touched". A target whose
/// only fresh bytes are three levels down — `debug/deps/*.o`, which is exactly
/// where a build writes — read as stale and was eligible for delete.
///
/// The walk short-circuits at the cutoff: the only question the gate asks is "is
/// anything in here newer than `cutoff`", so the first entry strictly newer than
/// it ends the walk. A *live* tree therefore costs a handful of stats instead of
/// a full traversal — which is what keeps this in the cheap tier — and the answer
/// is exact precisely when it matters, when everything is old. A tree that walks
/// all the way through is by definition one we are about to measure and probe
/// anyway.
///
/// Because the short-circuit triggers on `mtime > cutoff`, an early return always
/// yields `age_days < max_age_days` — the walk and [`cheap_verdict`]'s gate cannot
/// disagree about the boundary. (The age *reported* for a fresh tree is that of
/// the first fresh entry found, not necessarily the newest: enough to say "too
/// young", which is all it is used for.)
fn staleness(path: &Path, now: SystemTime, cutoff: SystemTime) -> Staleness {
    match newest_mtime(path, cutoff) {
        MtimeWalk::Newest(newest) => {
            let age_days = now
                .duration_since(newest)
                .unwrap_or(Duration::ZERO)
                .as_secs()
                / SECS_PER_DAY;
            if newest > cutoff {
                Staleness::Fresh { age_days }
            } else {
                Staleness::Stale { age_days }
            }
        }
        MtimeWalk::Partial(reason) => Staleness::Unprovable(reason),
    }
}

enum MtimeWalk {
    /// The newest mtime found, saturating at the cutoff (see [`staleness`]).
    Newest(SystemTime),
    /// Part of the tree could not be read, so "found nothing fresh" proves
    /// nothing.
    Partial(String),
}

/// Recursive newest-mtime, short-circuiting at `cutoff`. Never follows symlinks:
/// a symlinked entry is aged by the link itself, so a link pointing into a live
/// tree cannot make a dead target look fresh (nor the reverse).
fn newest_mtime(path: &Path, cutoff: SystemTime) -> MtimeWalk {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) => return MtimeWalk::Partial(format!("cannot stat {}: {err}", path.display())),
    };
    let mut newest = match meta.modified() {
        Ok(mtime) => mtime,
        Err(err) => return MtimeWalk::Partial(format!("no mtime for {}: {err}", path.display())),
    };
    if newest > cutoff {
        return MtimeWalk::Newest(newest);
    }
    if !meta.is_dir() {
        return MtimeWalk::Newest(newest);
    }
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) => return MtimeWalk::Partial(format!("cannot read {}: {err}", path.display())),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                return MtimeWalk::Partial(format!(
                    "cannot read an entry of {}: {err}",
                    path.display()
                ))
            }
        };
        match newest_mtime(&entry.path(), cutoff) {
            MtimeWalk::Newest(mtime) if mtime > cutoff => return MtimeWalk::Newest(mtime),
            MtimeWalk::Newest(mtime) => {
                if mtime > newest {
                    newest = mtime;
                }
            }
            partial @ MtimeWalk::Partial(_) => return partial,
        }
    }
    MtimeWalk::Newest(newest)
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
            for protected in &report.protected {
                println!("  protected: {protected}");
            }
            for candidate in &report.candidates {
                println!(
                    "  {} {} kind={} bytes={} age_days={} holders={} — {}",
                    candidate.decision,
                    candidate.path,
                    candidate.kind,
                    candidate
                        .bytes
                        .map(|bytes| bytes.to_string())
                        .unwrap_or_else(|| "unmeasured".to_string()),
                    candidate
                        .age_days
                        .map(|age| age.to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
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
            if report.revived_bytes > 0 {
                println!(
                    "  revived_bytes (freed by a prior reclaim at a revived path, not counted \
                     above): {}",
                    report.revived_bytes
                );
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

    /// A stale, unbound, unregistered candidate — everything a reclaim needs
    /// except the holder verdict, which is what the caller is testing.
    fn stale_candidate(holders: Option<HolderCheck>) -> OrphanCandidate {
        OrphanCandidate {
            path: PathBuf::from("/tmp/x-target"),
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

    /// Serializes the tests that repoint `CARGO_TARGET_DIR` at a fixture, so they
    /// cannot see each other's value.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => std::env::set_var(self.key, previous),
                None => std::env::remove_var(self.key),
            }
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
    #[test]
    fn cargo_target_dir_is_never_a_reap_candidate() {
        let _lock = env_lock().lock().unwrap_or_else(|err| err.into_inner());
        let root = unique_temp_dir("tachi-reaper-cargo-target-dir");
        let live = make_target_dir(&root, "live-shared-target");
        let dead = make_target_dir(&root, "dead-target");
        let _env = EnvGuard::set(CARGO_TARGET_DIR_ENV, &live);
        let mut store = open_store(&root);

        let protection = protected_paths();
        assert!(
            protection.covers(&live),
            "the dir CARGO_TARGET_DIR points at must be protected: {:?}",
            protection.paths()
        );
        assert!(
            protection.covers(&live.join("debug/deps")),
            "and everything under it"
        );

        let report = run_orphan_reap(
            store.connection_mut(),
            &opts(&root, true),
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
        let _lock = env_lock().lock().unwrap_or_else(|err| err.into_inner());
        let root = unique_temp_dir("tachi-reaper-shared-env");
        let shared = make_target_dir(&root, "managed-shared-target");
        let _env = EnvGuard::set(SHARED_CARGO_TARGET_DIR_ENV, &shared);

        assert!(
            protected_paths().covers(&shared),
            "both target-dir variables are read, not just one"
        );

        let _ = std::fs::remove_dir_all(&root);
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
        let unknown_probe = |_path: &Path| {
            HolderCheck::Unknown("cannot run lsof: No such file or directory".to_string())
        };

        let report = run_orphan_reap(
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

        let check = lsof_holder_probe(&target);
        assert_ne!(
            check,
            HolderCheck::None,
            "an open file handle under the dir must never read as unheld: {check:?}"
        );
        let candidate = OrphanCandidate {
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

        let candidates =
            scan_orphan_candidates(&[root.clone()], &Protection::default(), aged_now(30), 7);
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

        let candidates =
            scan_orphan_candidates(&[root.clone()], &Protection::default(), aged_now(30), 7);

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

        let probes = std::cell::Cell::new(0usize);
        let counting_probe = |_path: &Path| {
            probes.set(probes.get() + 1);
            HolderCheck::None
        };

        // Real `now`: the fixture was created a moment ago, so the age gate skips
        // it — and the expensive probes must never have run.
        let report = run_orphan_reap(
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

        let candidates = scan_orphan_candidates(
            &[root.clone()],
            &Protection::new([shared.clone()], Vec::new()),
            aged_now(30),
            7,
        );
        assert!(
            candidates.is_empty(),
            "the shared cargo target is protected: {candidates:?}"
        );

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

        let candidates = scan_orphan_candidates(&[root.clone()], &Protection::default(), now, 7);

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

    /// End-to-end proof of the revive: the same path is reaped, reborn, and
    /// reaped again — through S2a's `UNIQUE(path, kind)`, which the first cut
    /// could only ever hit once.
    #[test]
    fn a_reborn_target_at_a_reaped_path_is_reaped_again() {
        let root = unique_temp_dir("tachi-reaper-reborn");
        let dead = make_target_dir(&root, "lane-target");
        let mut store = open_store(&root);

        let first = run_orphan_reap(
            store.connection_mut(),
            &opts(&root, true),
            aged_now(30),
            &*unheld_probe(),
        );
        assert_eq!(first.reclaimed.len(), 1, "{first:?}");
        assert!(!dead.exists());

        // The lane runs again, rebuilds the same target, and dies again.
        let reborn = make_target_dir(&root, "lane-target");
        let second = run_orphan_reap(
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
        )
        .expect_err("a protected path must never be deleted");
        assert!(
            err.to_string().contains("protected"),
            "error should name the fence: {err}"
        );
        assert!(shared.join("debug/artifact.rlib").exists(), "bytes survive");

        let _ = std::fs::remove_dir_all(&root);
    }
}
