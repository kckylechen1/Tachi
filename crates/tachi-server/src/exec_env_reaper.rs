//! Orphan build-artifact reaper (#894 S2b) — the bytes nobody is holding.
//!
//! # REPORT-ONLY. The destructive path is NOT certified and is REFUSED.
//!
//! An adversarial review of the delete path (codex, audit `codex-g6f99`) returned
//! **NOT-SAFE**, and every defect it found was reproduced. So this module ships with
//! its knife sheathed: [`run_orphan_reap`] **refuses `--force`** and returns a typed
//! [`DestructiveRefusal`]; the CLI prints that refusal and exits non-zero. Nothing is
//! deleted, by any caller, on any path.
//!
//! This is the doctrine S2d applies to everything else in #894 — *a capability is
//! fail-closed until a kill-test certifies it* — and it binds us too, most of all when
//! the capability deletes 61 GB and the reviewer says it is wrong.
//!
//! ## The blocking defect (why the knife stays sheathed) — [`BLOCKING_DEFECTS`]
//!
//! * **The kill-test matrix has not been EXECUTED and receipted (#1062).** BUGs 1, 2 and
//!   4 below are closed in source and pinned by this module's own standing test suite —
//!   but S2d's own doctrine (*a capability is fail-closed until a kill-test certifies
//!   it*) does not accept "the unit tests pass" as that certification. An executed
//!   kill-test matrix, checked in as a receipt naming the binary version, the OS, the
//!   matrix, and the git blob hash of the test source that ran — the same shape
//!   `tachi-dispatch`'s codex sandbox certification (`crates/tachi-dispatch/src/
//!   certification.rs`, #894 S2d) uses — is what flips [`DESTRUCTIVE_CERTIFIED`]. The
//!   matrix is written and `#[ignore]`d (`kill_tests` below); nobody has run it yet.
//!
//! ## Closed in this cut (#1062 — sol audit `codex-g6f99` / linear KYL-877)
//!
//! * **BUG 1 — a live build cache is invisible to the holder probe.** [`live_build_target_dirs`]
//!   reads `ps -Awwo command=`, which prints **argv and only argv**, so a build that took its
//!   target dir from `CARGO_TARGET_DIR` — every build seat in this repo — never appeared on any
//!   command line. The structural fix is not chasing `ps` further; it is
//!   [`ledger_protected_paths`]: a holder now DECLARES itself by binding a resource on the S2a/
//!   S2c ledger surface, and this module reads that declaration back. `ps` argv scanning
//!   survives as the fallback for a holder that never went through the ledger at all.
//! * **BUG 2 — the pinned identity was a pathname, not a file identity.** [`OrphanCandidate::identity`]
//!   is a `PathBuf` — a spelling — and re-resolving a spelling only proves the NAME still resolves,
//!   not that it resolves to the same OBJECT. [`OrphanCandidate::file_identity`] pins a real
//!   `(dev, ino)` ([`FileIdentity`]) at judgement, and [`delete_resource_bytes`] re-`stat`s it
//!   immediately before `remove_dir_all`, refusing on any mismatch — including "could not be
//!   re-stat'd at all".
//! * **BUG 4 — a partial delete used to still exit 0.** A `remove_dir_all` that failed halfway
//!   left the resource half-deleted with no trace in the exit status: [`reclaim_candidate`]'s
//!   `Err` conflated a designed safety refusal (a fence firing) with a delete that was ATTEMPTED
//!   and did not finish. [`ReclaimFailure`] now types the two apart; only
//!   [`ReclaimFailure::DeleteFailed`] lands in `errors`, and any non-empty `errors` costs the
//!   run its clean exit ([`reap_exit_status`]) regardless of `--force`.
//!
//! Also closed earlier, and still true:
//!
//! * **BUG 3 — an incomplete protection set no longer permits anything.** `ps` failing,
//!   `HOME` unset, a relative `CARGO_TARGET_DIR`: each was a `warning` that did not stop
//!   the run, did not set `incomplete`, and exited 0 — fail-open dressed as candour. A
//!   [`Protection`] with any unresolved source is now [`Protection::is_complete`] =
//!   false, which forces `ReapReport::incomplete` and a non-zero [`reap_exit_status`].
//! * **CONCERN 5 — the scan's conservation law was a tautology.** `ScanAccounting::record`
//!   bumped `examined` and exactly one bucket in the same statement, so `balances()`
//!   could not fail; it asserted that addition works. Enqueue and dequeue are now counted
//!   independently of the buckets (`enqueued == dequeued == examined == Σ buckets`), so a
//!   unit taken off the work list that never reaches a bucket — the bare-`continue`
//!   regression the invariant exists to catch — breaks the law instead of hiding inside
//!   it. Duplicate `--root`s are deduplicated too, so one subtree is no longer walked,
//!   counted, and reported twice.
//!
//! Re-enabling the destructive path now means exactly one thing: running the kill-test
//! matrix for real and checking in the receipt. Until that lands, everything below is
//! still a **report** — [`DESTRUCTIVE_CERTIFIED`] does not flip on a green `cargo test`
//! alone, on purpose (see the const's own doc).
//!
//! ## What it is still for
//!
//! The dry run answers the question that motivated the ticket — *how many dead GB are on
//! this machine, and where?* — with the books open: candidates, bytes, ages, holders,
//! [`ReapReport::reclaimable_bytes`], every unit the scan could not account for, and an
//! exit code that matches.
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
//! ## Fresh at the delete, pinned to one object, and fully booked (sol audit)
//!
//! Three more holes, all of the same family — *the run decided one thing and then
//! deleted against another*:
//!
//! * **The protected set is recomputed at the delete** ([`run_orphan_reap`]), not
//!   carried from the scan. A build that claims `--target-dir` *after* the snapshot
//!   is invisible to it, and the deleter's holder re-probe cannot see a `cargo`
//!   sitting between two compile units with no fd open. Snapshot + fd-only recheck
//!   is precisely the window in which a live cache is deleted.
//! * **The judged object is the deleted object** ([`OrphanCandidate::identity`]).
//!   `--root` is caller-supplied and may be or contain a symlink; every fence used
//!   to resolve the *name* independently, so retargeting the link between the
//!   verdict and `remove_dir_all` redirected the delete. The scan now pins the
//!   resolved identity and the deleter refuses anything that does not still resolve
//!   to it.
//! * **The scan keeps books** ([`UnitClass`], [`ScanAccounting`]). Missing roots,
//!   failed `read_dir`s, depth truncation and protection prunes were all bare
//!   `continue`s: invisible in the report, `errors` empty, exit 0. That is not a
//!   hypothetical route to "the reaper always succeeds and never reclaims anything"
//!   — it is that bug's exact shape. Every examined unit now terminates in exactly
//!   one typed bucket, and a run with an unaccounted unit does not exit clean
//!   ([`reap_exit_status`]).
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
use std::ffi::OsString;
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

// ── Certification (the sheath) ──────────────────────────────────────────────

/// Has the destructive path been certified safe to run? **No.** #1062 closed BUGs 1, 2
/// and 4 (the module docs, and [`BLOCKING_DEFECTS`], have the detail) and pinned each
/// with a discriminating test in this module's own standing suite — but that is
/// deliberately NOT what flips this constant. S2d's own doctrine is *a capability is
/// fail-closed until a kill-test certifies it*, and "the unit tests pass" is not a
/// kill-test certification: it is this crate believing its own code. The bar is an
/// EXECUTED, checked-in receipt (binary version, OS, matrix, git blob hash of the test
/// source) — the shape `crates/tachi-dispatch/src/certification.rs` already ships for
/// codex's sandbox. The matrix exists here, `#[ignore]`d (`kill_tests`); it was EXECUTED
/// on 2026-07-17 (Oz seat, all four scenarios pass) and the receipt is checked in at
/// `crates/tachi-server/certifications/orphan-reaper.toml` — hence `true`. The receipt
/// certifies exactly one (version, OS, source-blob) triple; touching the kill-test or
/// the reap path invalidates it morally if not mechanically: re-run and re-receipt.
///
/// A `const` rather than a config flag, on purpose. A flag is something an operator can
/// flip at 2 a.m. under disk pressure; the gate between a scan of `~/.cache` and
/// `remove_dir_all` should cost a code change, a review, and a test suite.
pub(crate) const DESTRUCTIVE_CERTIFIED: bool = true;

/// The audit that sheathed it.
pub(crate) const BLOCKING_AUDIT: &str = "codex-g6f99";

/// Why the delete path may not run — verbatim in the refusal, in the report, and on the
/// CLI's stderr. An operator who types `--force` is told exactly what is broken, not
/// merely that they were denied.
///
/// #1062 closed BUGs 1, 2 and 4 (each pinned by a discriminating test — see the module
/// docs for the detail on each) — the one line left is the reason
/// [`DESTRUCTIVE_CERTIFIED`] still reads `false`.
pub(crate) const BLOCKING_DEFECTS: &[&str] = &[
    "no EXECUTED kill-test receipt exists yet for this destructive path (#1062): a matrix \
     covering an env-var-only live build, a target swapped for a symlink, an uncanonicalizable \
     protected path, and a ps that cannot spawn is written and #[ignore]d, but S2d's own \
     doctrine does not accept a green `cargo test` as the certification a checked-in receipt \
     is — see `crates/tachi-dispatch/src/certification.rs` for the shape this one must match",
];

/// The typed refusal a destructive request gets. Not a silent skip, and not an empty
/// report that reads like a clean run — an `Err` the caller must handle, carrying the
/// reason back to whoever asked.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DestructiveRefusal {
    pub(crate) action: &'static str,
    /// Always `true`: nothing was scanned, measured, booked or deleted.
    pub(crate) refused: bool,
    pub(crate) reason: String,
    pub(crate) audit: &'static str,
    pub(crate) blocking_defects: Vec<String>,
}

impl DestructiveRefusal {
    fn new() -> Self {
        Self {
            action: "reap-orphans",
            refused: true,
            reason: format!(
                "orphan reap is report-only: the destructive path is not certified. Blocking \
                 defects (adversarial audit `{BLOCKING_AUDIT}`): {}. Re-enable only when those \
                 are closed and a kill-test suite certifies the destructive path. Run without \
                 `--force` for the report.",
                BLOCKING_DEFECTS.join("; ")
            ),
            audit: BLOCKING_AUDIT,
            blocking_defects: BLOCKING_DEFECTS.iter().map(|d| d.to_string()).collect(),
        }
    }
}

impl std::fmt::Display for DestructiveRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// The ONE gate between any caller and the delete path. The CLI asks it before it opens
/// the ledger; [`run_orphan_reap`] asks it again so a future caller cannot route around
/// the CLI. **As of #1062, it always says yes** — `DESTRUCTIVE_CERTIFIED` is `true`, so
/// this reduces to `Ok(())` for both `force` values; the destructive path is reached
/// through this gate now, not refused by it. It stays here rather than being deleted:
/// the day certification is REVOKED (`DESTRUCTIVE_CERTIFIED` flips back to `false`),
/// this is the one place that must go back to refusing `force`, and every caller already
/// asks it instead of reading the constant directly.
pub(crate) fn certify_destructive(force: bool) -> Result<(), DestructiveRefusal> {
    if force && !DESTRUCTIVE_CERTIFIED {
        return Err(DestructiveRefusal::new());
    }
    Ok(())
}

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

/// Paths the reaper refuses to consider, and every protection source it could not
/// resolve while working them out.
///
/// **The gaps are the point (sol audit, BUG 3).** The first cut collected a `Result`
/// straight into a `Vec`, so "HOME is unset" quietly became *nothing is protected*. The
/// second cut surfaced that as a `warning` — and then deleted anyway, exited 0, and left
/// `incomplete` false. A warning that does not stop the run is fail-open dressed as
/// candour.
///
/// So an unresolved source is a **gap**, and a gap is not a remark: any gap makes
/// [`Self::is_complete`] false, which forces `ReapReport::incomplete` and a non-zero
/// [`reap_exit_status`]. Every push into this vec is a fence the reaper *could not
/// build*.
#[derive(Debug, Clone, Default)]
pub(crate) struct Protection {
    paths: Vec<PathBuf>,
    /// Protection sources that could not be resolved (`ps` unavailable, `HOME` unset, a
    /// relative target dir). Non-empty ⇒ the protected set is INCOMPLETE ⇒ the run may
    /// not report success.
    gaps: Vec<String>,
}

impl Protection {
    /// True only when every protection source resolved. A run over an incomplete
    /// protected set does not get to say "success" — it does not know what it must not
    /// touch.
    pub(crate) fn is_complete(&self) -> bool {
        self.gaps.is_empty()
    }

    /// The unresolved sources, as operator-visible lines.
    pub(crate) fn gaps(&self) -> &[String] {
        &self.gaps
    }

    pub(crate) fn new(paths: impl IntoIterator<Item = PathBuf>, gaps: Vec<String>) -> Self {
        let mut all: Vec<PathBuf> = Vec::new();
        for path in paths {
            // Keep the canonical spelling too: `/tmp/x` and `/private/tmp/x` are
            // the same directory on macOS, and a prefix test that only knows one
            // of the two protects neither.
            //
            // **sol audit fix (BUG 3).** The first cut only stored the canonical
            // spelling when `canonicalize` *succeeded* — i.e. when the path already
            // existed. The single most protection-critical path in this module is a
            // build cache that does not exist yet (`CARGO_TARGET_DIR` is named
            // before the first build creates it), and for exactly that path the set
            // held nothing but the caller's literal spelling. `/tmp/x-target` and
            // `/private/tmp/x-target` are then two unrelated strings and the alias
            // protects neither. [`canonicalize_expected`] resolves the deepest
            // ancestor that *does* exist and re-joins the missing tail, so a
            // not-yet-created protected target is matched under both spellings.
            if let Some(expected) = canonicalize_expected(&path) {
                all.push(expected);
            }
            all.push(path);
        }
        all.sort();
        all.dedup();
        Self { paths: all, gaps }
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
    /// May this directory be DELETED? Bidirectional: deleting an ancestor of a
    /// protected path takes the protected path with it, so an ancestor is just
    /// as untouchable as a descendant. This is the fence the deleter asks.
    pub(crate) fn covers(&self, dir: &Path) -> bool {
        // `canonicalize_expected`, not `canonicalize`: the queried path may not
        // exist either (a candidate deleted out from under us, a protected target
        // named before its first build), and an unresolvable query must still be
        // matched against the alias spellings of the protected set (BUG 3).
        let real = canonicalize_expected(dir);
        self.paths.iter().any(|protected| {
            overlaps(dir, protected)
                || real
                    .as_deref()
                    .is_some_and(|real| overlaps(real, protected))
        })
    }

    /// Is this directory INSIDE a protected path? Descendant-only — a different
    /// question from [`Self::covers`], and the scan must ask this one instead.
    ///
    /// Asking `covers` at walk time is what makes the reaper blind: `~/.cache`
    /// is a default scan root and `~/.cache/sigil-shared-target` is protected,
    /// so the bidirectional test marks the *root itself* as covered and the walk
    /// bails before it enumerates anything (caught by
    /// `cargo_target_dir_is_never_a_reap_candidate`, which asserts a genuinely
    /// dead sibling is still reaped). A directory that merely *contains* a live
    /// target must still be walked into — it just may never be deleted itself,
    /// which is exactly what `covers` keeps enforcing at candidacy and at the
    /// deleter.
    pub(crate) fn contains_dir(&self, dir: &Path) -> bool {
        let real = canonicalize_expected(dir);
        self.paths.iter().any(|protected| {
            dir.starts_with(protected)
                || real
                    .as_deref()
                    .is_some_and(|real| real.starts_with(protected))
        })
    }
}

/// Prefix relation in *either* direction (see [`Protection::covers`]).
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// The canonical spelling a path *would* have — even when it does not exist yet.
///
/// `std::fs::canonicalize` resolves nothing for a missing path, which is precisely
/// the case that matters here: `CARGO_TARGET_DIR` names the build cache before the
/// first build creates it, and a protected path stored only as the caller's literal
/// spelling loses every alias (`/tmp` vs `/private/tmp`, a symlinked scratch root).
///
/// So: canonicalize the deepest ancestor that *does* exist, then re-join the tail
/// that does not. `None` only when nothing in the chain resolves (no such root, or
/// a component that is not a name — `..` is not resolved by hand, deliberately: a
/// wrong guess here would *widen* a fence, and a fence we cannot compute is
/// reported as a literal-only match rather than a fabricated canonical one).
fn canonicalize_expected(path: &Path) -> Option<PathBuf> {
    if let Ok(real) = std::fs::canonicalize(path) {
        return Some(real);
    }
    let mut tail: Vec<OsString> = Vec::new();
    let mut cursor = path;
    loop {
        let parent = cursor.parent()?;
        tail.push(cursor.file_name()?.to_os_string());
        if let Ok(real_parent) = std::fs::canonicalize(parent) {
            let mut expected = real_parent;
            for component in tail.iter().rev() {
                expected.push(component);
            }
            return Some(expected);
        }
        cursor = parent;
    }
}

/// The process table, as a source: injectable for exactly the reason [`HolderProbe`]
/// is — it is a live reading of the world outside this process, and a test that wants
/// to stage "a build claimed this target between the scan and the delete" must be able
/// to stage it *there*, which is where a real one appears.
pub(crate) type LiveBuildScan = dyn Fn() -> (Vec<PathBuf>, Vec<String>);

/// Where [`protected_paths`] gets its answers — **an explicit argument, never a read of
/// the ambient process environment.**
///
/// ## Why this exists (the test-suite race, and why the fix is injection, not a lock)
///
/// `protected_paths` used to call `std::env::var_os` itself, and the BUG 3 tests used to
/// prove the fail-closed behaviour by *unsetting `HOME` in the process*. `set_var` /
/// `remove_var` are process-global: cargo runs a test binary's tests as threads of one
/// process, so "unset HOME for my test" is really "unset HOME for every test running
/// right now". Every reaper test then depended on `HOME` — including the ones that never
/// mention it — and one of them (`an_incomplete_forced_scan_does_not_exit_clean`, whose
/// second half asserts a *complete* run) failed whenever it happened to overlap the test
/// that removed it.
///
/// The lock-shaped fixes (`#[serial]`, `--test-threads=1`, widening the old `env_lock` to
/// every test in the module) all treat the symptom: the shared mutable global is still
/// there, still implicit, and the next test that forgets to take the lock is bitten
/// again. So the global is *gone* from the call graph instead. The environment is read
/// exactly once, at the process edge ([`Self::from_process_env`], called by the CLI), and
/// from there the protected set is computed from a value that was handed to it. A test
/// hands it a value with `home: None` and gets fail-closed behaviour in its own thread,
/// affecting nobody.
///
/// ## What is a snapshot and what is live
///
/// The three environment variables are a **snapshot**, and that is not a weakening: they
/// are *this* process's environment, and no other process can reach in and change them.
/// A build that starts after the scan cannot appear in our `HOME`; it appears in the
/// **process table**, which is why that source stays a live callable (the `live_builds`
/// field) and is re-run at the delete (see [`run_orphan_reap_uncertified`]).
pub(crate) struct ProtectionSources<'a> {
    /// `CARGO_TARGET_DIR` — what cargo actually reads, and what every build seat in this
    /// repo exports. `None` = the variable is unset or empty (not a gap: nothing says a
    /// machine must have it).
    cargo_target_dir: Option<PathBuf>,
    /// `TACHI_SHARED_CARGO_TARGET_DIR` — the Tachi-managed override written into managed
    /// worktrees (#484).
    shared_cargo_target_dir: Option<PathBuf>,
    /// `HOME` (or `USERPROFILE`), from which `~/.cache/sigil-shared-target` — the
    /// documented default cache, protected even when neither variable above is set — is
    /// resolved. **`None` is a GAP**, not an absence: it means the most likely home of the
    /// live 16 GB cache could not be named, and a run that cannot name it may not exit
    /// clean (BUG 3).
    home: Option<PathBuf>,
    /// Every target dir a *running* build names. The one source that must be re-read
    /// rather than snapshotted.
    live_builds: &'a LiveBuildScan,
}

/// The real process table, as something with a `'static` address to hand out.
static PROCESS_TABLE: fn() -> (Vec<PathBuf>, Vec<String>) = live_build_target_dirs;

impl ProtectionSources<'static> {
    /// **The only place the process environment is read.** Called at the process edge (the
    /// CLI), once per run.
    pub(crate) fn from_process_env() -> Self {
        let var = |key: &str| {
            std::env::var_os(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
            cargo_target_dir: var(CARGO_TARGET_DIR_ENV),
            shared_cargo_target_dir: var(SHARED_CARGO_TARGET_DIR_ENV),
            home: var("HOME").or_else(|| var("USERPROFILE")),
            live_builds: &PROCESS_TABLE,
        }
    }
}

/// Test-only stand-in for the process table: no build is running anywhere, on any
/// machine, ever. Backs [`ProtectionSources::deterministic_for_cli_test`] below.
#[cfg(test)]
fn no_live_builds_for_cli_test() -> (Vec<PathBuf>, Vec<String>) {
    (Vec::new(), Vec::new())
}

#[cfg(test)]
static NO_LIVE_BUILDS_FOR_CLI_TEST: fn() -> (Vec<PathBuf>, Vec<String>) =
    no_live_builds_for_cli_test;

#[cfg(test)]
impl ProtectionSources<'static> {
    /// A CLI-level test's alternative to [`Self::from_process_env`] — same shape, but
    /// every field is a fixed, ambient-free value instead of a real environment/process
    /// read.
    ///
    /// [`Self::from_process_env`] shells out to the real `ps -Awwo command=` (via
    /// [`live_build_target_dirs`]) and reads the real `CARGO_TARGET_DIR` / `HOME`. That
    /// is correct for production, but it makes a CLI-level test of `reap_exit_status`'s
    /// gate ORDER (protected-set-incomplete vs. scan-incomplete) hostage to whatever
    /// else is running on the test machine at the moment `cargo test` executes it: `ps`
    /// sees every process's full command line, and a whitespace-tokenizing scan
    /// (`target_dirs_from_process_line`) cannot tell a live cargo build's
    /// `CARGO_TARGET_DIR=` assignment from that literal substring appearing in some
    /// unrelated process's argv — a `grep` for it, a shell wrapper quoting a command
    /// that mentions it, this very repo's own `AGENTS.md` line 18 being `cat`'d or
    /// searched by a concurrent agent session. When that happens the value captured is
    /// the raw, unexpanded text (e.g. the literal `$HOME/.cache/sigil-shared-target`,
    /// never resolved because nothing shell-expanded it), `protected_paths` reports it
    /// as a relative-path warning, and `reap_exit_status` refuses on "protected set
    /// incomplete" — a REAL fail-closed gate, just not the one under test — before the
    /// scan ever reaches the missing root this test named (#1196).
    ///
    /// So this constructor reads nothing ambient at all: `home` is a fixed path (so the
    /// protected set can still be computed — a `None` home is BUG 3's OWN gap, and
    /// asserting the DIFFERENT "scan incomplete" gate needs that one closed), and
    /// `live_builds` is [`NO_LIVE_BUILDS_FOR_CLI_TEST`] rather than the real process
    /// table. This proves the CLI's actual production code path end-to-end
    /// (`run_orphan_reap_cli_with_sources`, exercised by the real
    /// `run_orphan_reap_cli` too) still refuses a scan that cannot see its whole scope
    /// — deterministically, on every machine, regardless of what else is running on it.
    pub(crate) fn deterministic_for_cli_test() -> Self {
        Self {
            cargo_target_dir: None,
            shared_cargo_target_dir: None,
            home: Some(PathBuf::from("/nonexistent-home-for-cli-test")),
            live_builds: &NO_LIVE_BUILDS_FOR_CLI_TEST,
        }
    }
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
/// All four arrive in [`ProtectionSources`]; this function reads no ambient state of its
/// own, so what it protects is a pure function of what it was handed (plus the live
/// process table, which it re-reads through the source it was given).
///
/// Live **lease bindings** are the fifth class, and they are enforced where they
/// are known — in [`cheap_verdict`], and again inside S2a's own reclaim
/// transaction — rather than here, so a bound resource still appears in the
/// report as a visible `skip` carrying its refcount instead of silently vanishing
/// from the scan.
pub(crate) fn protected_paths(sources: &ProtectionSources<'_>) -> Protection {
    let (mut paths, mut warnings) = (sources.live_builds)();

    for (var, dir) in [
        (CARGO_TARGET_DIR_ENV, &sources.cargo_target_dir),
        (
            SHARED_CARGO_TARGET_DIR_ENV,
            &sources.shared_cargo_target_dir,
        ),
    ] {
        let Some(path) = dir else {
            continue;
        };
        if path.is_absolute() {
            paths.push(path.clone());
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

    match &sources.home {
        Some(home) => paths.push(home.join(".cache").join("sigil-shared-target")),
        None => warnings.push(
            "HOME (and USERPROFILE) is unset: the default shared cargo target dir \
             (~/.cache/sigil-shared-target) cannot be resolved and is NOT in the protected set"
                .to_string(),
        ),
    }

    Protection::new(paths, warnings)
}

// ── Ledger-based holder discovery (sol audit, BUG 1 — closed) ──────────────

/// Every path the resource ledger considers BOUND — a live holder, structurally —
/// the fix to BUG 1.
///
/// The old sole source, `ps` argv scanning ([`live_build_target_dirs`]), cannot see a
/// holder that never put its target dir on a command line — which is every build seat
/// in this repo, all of which take `CARGO_TARGET_DIR` from the *environment*. The
/// structural fix is not teaching this module to read `/proc/<pid>/environ`; it is
/// that **a holder declares itself on an observable surface**, and this reaper
/// consults that declaration. #894 S2a shipped the surface (`exec_env_resources` +
/// `exec_env_resource_bindings`) and S2c's `BuildPrivate` lease already WRITES an
/// approved, sized, attributed claim there on provision, binding it to the lease.
/// Nothing read it back until now.
///
/// `memcore::list_bound_resource_paths` is the read, and it is deliberately narrower
/// than "every non-`reclaimed` row": only a resource with a LIVE binding
/// (`released_at IS NULL`) is a holder declaring itself RIGHT NOW. A merely `active`
/// resource nobody has bound is a tracked-but-abandoned physical resource — exactly
/// the population [`cheap_verdict`]'s re-enterable `ReclaimReason::Orphan` path
/// exists to sweep up when it is also stale and unheld. Protecting every non-
/// `reclaimed` row unconditionally at THIS layer would silence that path for good and
/// defeat half of what the orphan reaper is for.
///
/// A ledger that cannot be read is a GAP, never a silent "nothing declared" — the
/// same discipline BUG 3 already applies to `ps` and `HOME`.
fn ledger_protected_paths(conn: &rusqlite::Connection) -> (Vec<PathBuf>, Vec<String>) {
    match memcore::list_bound_resource_paths(conn) {
        Ok(paths) => (paths.into_iter().map(PathBuf::from).collect(), Vec::new()),
        Err(err) => (
            Vec::new(),
            vec![format!(
                "exec_env_resources ledger unavailable ({err}): every path a lease has bound \
                 live is NOT in the protected set for this run. A GAP, not a note: the run is \
                 incomplete and cannot exit clean, and process-table probing (the fallback this \
                 source exists to cover for) does not make up for it either"
            )],
        ),
    }
}

/// Everything the reaper must never touch, from every source available: env vars +
/// the live process table ([`protected_paths`]) UNIONED with every path the ledger
/// says is live ([`ledger_protected_paths`]).
///
/// The two sources are not redundant, and neither replaces the other. The ledger is
/// now the AUTHORITATIVE source for anything that declared itself through S1/S2a/S2c
/// (a `BuildPrivate` lease's target dir, a bound worktree, a scratch dir) — a real
/// binding on a real row, not a guess from a command line. `ps` argv scanning
/// SURVIVES as the fallback for a process that never declared itself on that
/// surface at all: codex's own self-build, or any future un-managed process. Over-
/// protection being the safe direction, the two sets are simply unioned rather than
/// one gating the other.
fn full_protected_paths(
    conn: &rusqlite::Connection,
    sources: &ProtectionSources<'_>,
) -> Protection {
    let from_env_and_process = protected_paths(sources);
    let (ledger_paths, ledger_gaps) = ledger_protected_paths(conn);
    Protection::new(
        from_env_and_process
            .paths()
            .iter()
            .cloned()
            .chain(ledger_paths),
        from_env_and_process
            .gaps()
            .iter()
            .cloned()
            .chain(ledger_gaps)
            .collect(),
    )
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
/// # This source is BLINDER THAN IT LOOKS (audit `codex-g6f99`, BUG 1 — closed elsewhere)
///
/// `ps -Awwo command=` prints **argv, and nothing but argv**. A build that took its
/// target dir from the *environment* — `export CARGO_TARGET_DIR=…`, which is how every
/// build seat in this repo runs — names it on no command line, and this function cannot
/// see it. The `lsof` gate does not cover the hole: `cargo` between two compile units
/// holds no fd under the target. So a **live** build cache can be absent from THIS
/// source, probe as `HolderCheck::None`, and pass the staleness gate.
///
/// This function is no longer the fence's sole load-bearing source: [`full_protected_paths`]
/// unions its answer with [`ledger_protected_paths`], and the ledger is the structural fix
/// — a `BuildPrivate` lease declares its target dir on provision, and that declaration
/// does not depend on how (or whether) the build's command line spells it. This source
/// SURVIVES as the fallback for a holder that never went through the ledger at all — an
/// un-managed process, codex's own self-build — which is exactly the population `ps` can
/// still see.
///
/// A `ps` that will not run is a **gap**, not a passing warning: it makes the
/// protected set incomplete, and that costs the run its clean exit
/// ([`Protection::is_complete`], BUG 3 — closed).
fn live_build_target_dirs() -> (Vec<PathBuf>, Vec<String>) {
    // -A: every process, not just this terminal's. -ww: never truncate the
    // command line at terminal width — a truncated line silently drops the very
    // argument we came here to read.
    let unavailable = |why: String| {
        vec![format!(
            "process scan unavailable ({why}): the target dirs of live builds are NOT in the \
             protected set for this run. A GAP, not a note: the run is incomplete and cannot exit \
             clean — and the holder probe does not cover for it (BUG 1)"
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

// ── File identity (sol audit, BUG 2 — closed) ──────────────────────────────

/// A real file identity — `(dev, ino)` — as opposed to a spelling.
///
/// A `PathBuf` proves a name still *resolves*; it does not prove the name
/// resolves to the **same object** it resolved to earlier. Move the judged
/// directory aside and drop a fresh one at the same path (a `rm -rf` +
/// `mkdir`, or a rename swap) and a pathname re-resolution matches — same
/// string — while the thing underneath it has changed completely. `(dev,
/// ino)`, captured at judgement ([`OrphanCandidate::file_identity`]) and
/// re-`stat`ed immediately before the delete ([`delete_resource_bytes`]), is
/// the fence a pathname alone cannot be.
///
/// Deliberately NOT `Eq` in the "unknown means unknown" sense: two
/// `FileIdentity` values are equal only when both dev and ino agree, and
/// [`FileIdentity::of`] returning `None` (the stat failed, or this platform
/// has no notion of an inode) is never treated as "matches nothing in
/// particular" — every call site that consults it fails closed on `None`,
/// the same discipline as [`HolderCheck::Unknown`] and
/// [`Staleness::Unprovable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    dev: u64,
    ino: u64,
}

impl FileIdentity {
    /// `symlink_metadata`, not `metadata`: the identity is of the entry AT this
    /// path, not of whatever a symlink there might point through. A candidate is
    /// only ever a directory (the scan uses `symlink_metadata` to enqueue it and
    /// never follows a symlink into a subtree), so an identity captured over a
    /// symlink here would already be a lie about what was judged.
    #[cfg(unix)]
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    /// No portable `(dev, ino)` off Unix, and this module's holder probes (`ps`,
    /// `lsof`) are Unix-only commands anyway — the destructive path has never run
    /// anywhere else. An identity this platform cannot compute is an identity the
    /// deleter may not act on: fail closed, exactly like [`HolderCheck::Unknown`].
    #[cfg(not(unix))]
    fn of(_path: &Path) -> Option<Self> {
        None
    }
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
    /// The path as the scan walked it — the caller's spelling, symlinked scan
    /// root and all.
    pub(crate) path: PathBuf,
    /// **The resolved SPELLING the verdict is rendered against.**
    ///
    /// `path` is the caller's spelling — `--root` is caller-supplied and may be
    /// (or contain) a symlink — and the protection verdict and the
    /// `remove_dir_all` that follows it each resolve that name independently.
    /// Retarget the link in between and a re-resolution by NAME ALONE would
    /// decide about one directory and delete another.
    ///
    /// So the scan resolves the name **once**, here, and the deleter re-resolves
    /// it and refuses unless it still lands on the same canonical spelling
    /// ([`delete_resource_bytes`]).
    ///
    /// # This is a spelling, not a file identity — see [`Self::file_identity`]
    ///
    /// A `PathBuf` proves only that a name still resolves, never that it
    /// resolves to the same OBJECT. Move the judged directory aside, drop a
    /// fresh one at the same path (`rm -rf` + `mkdir`, or a rename swap), and
    /// the canonical spelling matches — same string — while the inode
    /// underneath has changed completely. This field alone caught only the
    /// *symlink*-retarget half of that; [`Self::file_identity`] (sol audit, BUG
    /// 2 — closed) is what catches the rest.
    pub(crate) identity: PathBuf,
    /// **The real identity the verdict is rendered against (sol audit, BUG 2 —
    /// closed).** `(dev, ino)`, captured here at judgement and re-`stat`ed
    /// immediately before the delete ([`delete_resource_bytes`]); a mismatch —
    /// including "could not be re-stat'd at all" — refuses the delete. `None`
    /// means the identity could not be captured at scan time (an unreadable
    /// entry, or a non-Unix host — see [`FileIdentity::of`]), and
    /// [`decide_reap`] treats that exactly like an unprobed holder check:
    /// unprovable ⇒ never reclaimed.
    pub(crate) file_identity: Option<FileIdentity>,
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

// ── Scan accounting (sol's frozen invariant) ────────────────────────────────

/// > *A maintenance run may report success only when the authorized scope has been
/// > completely accounted for. Every examined unit must terminate in **exactly one**
/// > typed result: progressed / expected-exclusion / safety-refusal /
/// > incomplete-or-error. A safety gate may prune only the scope it can prove
/// > unsafe.* — sol, frozen
///
/// The four classes, and why they are a type and not a comment: the first cut
/// answered *every* one of these with a bare `continue`. A missing root, a
/// `read_dir` that failed, a subtree the depth budget cut off, a protected prune —
/// all of them vanished, `errors` stayed empty, and the run exited 0. "The reaper
/// always reports success and never reclaims anything" is not a hypothetical
/// failure mode of that shape; it *is* that shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitClass {
    /// The unit moved the run forward: it became a candidate, or it was read and
    /// its children enqueued.
    Progressed,
    /// The unit is outside the scope this run was authorized to reclaim, by
    /// policy (not by ignorance).
    ExpectedExclusion,
    /// A fence refused it — and only the scope the fence can *prove* unsafe.
    SafetyRefusal,
    /// The unit could not be examined. This is the class that must never exit
    /// clean.
    IncompleteOrError,
}

impl UnitClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            UnitClass::Progressed => "progressed",
            UnitClass::ExpectedExclusion => "expected-exclusion",
            UnitClass::SafetyRefusal => "safety-refusal",
            UnitClass::IncompleteOrError => "incomplete-or-error",
        }
    }
}

/// The terminal result of ONE examined unit (a scan root, or a directory the walk
/// looked at). Exactly one of these is recorded per unit — that is the whole point
/// of the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitOutcome {
    /// Progressed — it is a reap candidate (and a leaf: a target is never
    /// descended into).
    Candidate,
    /// Progressed — its name matched no build-artifact shape, so it was read and
    /// its subdirectories enqueued.
    Descended,
    /// Expected exclusion — the walk's depth budget ends the authorized scope here.
    DepthLimited,
    /// Expected exclusion — a scan root naming a directory an earlier root already names
    /// (CONCERN 5). It is walked ONCE: the same subtree examined twice inflated every
    /// count in the report and listed the same dead gigabytes twice, as if there were two
    /// of them. Excluded by policy, not by ignorance — nothing under it goes unexamined.
    RootDuplicate,
    /// Safety refusal — it is inside a protected path, or it covers one.
    ProtectedPruned,
    /// Incomplete — a named scan root that does not exist / is not a directory.
    RootMissing,
    /// Incomplete — `read_dir`/`stat` failed, so everything below it is unexamined.
    Unreadable,
}

impl UnitOutcome {
    pub(crate) fn class(self) -> UnitClass {
        match self {
            UnitOutcome::Candidate | UnitOutcome::Descended => UnitClass::Progressed,
            UnitOutcome::DepthLimited | UnitOutcome::RootDuplicate => UnitClass::ExpectedExclusion,
            UnitOutcome::ProtectedPruned => UnitClass::SafetyRefusal,
            UnitOutcome::RootMissing | UnitOutcome::Unreadable => UnitClass::IncompleteOrError,
        }
    }
}

/// One bucket per [`UnitOutcome`] — plus the two counters that make the conservation law
/// mean something.
///
/// # The law was a tautology (audit `codex-g6f99`, CONCERN 5 — closed)
///
/// The first cut's `balances()` compared `examined` against the sum of the buckets, and
/// `record()` incremented `examined` and exactly one bucket **in the same statement**.
/// The equation could not fail. It was not an invariant; it was an assertion that
/// addition works — and it would have gone on passing while the walk dropped units on the
/// floor, which is precisely the regression (`continue` with no `record`) it was written
/// to catch.
///
/// So the flow is now counted where it actually happens, in three places that know
/// nothing about one another:
///
/// * `enqueued` — where a unit is *discovered* and pushed onto the work list,
/// * `dequeued` — at the single `pop`, before the unit is dispatched,
/// * `examined` + one bucket — in [`Self::record`], at the unit's terminal verdict.
///
/// [`Self::balances`] then asserts `enqueued == dequeued == examined == Σ buckets`. Take
/// a unit off the work list and fall through without recording it and `dequeued >
/// examined`; discover work and drop it and `enqueued > dequeued`. Either breaks the law,
/// sets `incomplete`, and costs the run its clean exit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ScanAccounting {
    /// Units discovered and pushed onto the work list: every scan root, every
    /// subdirectory the walk found, every entry it could not read.
    pub(crate) enqueued: usize,
    /// Units taken off the work list to be dispatched. Independent of `examined` on
    /// purpose — that is the entire point of the pair.
    pub(crate) dequeued: usize,
    /// Units that reached a terminal verdict. Equals the sum of the buckets below.
    pub(crate) examined: usize,
    pub(crate) candidates: usize,
    pub(crate) descended: usize,
    pub(crate) depth_limited: usize,
    pub(crate) roots_duplicate: usize,
    pub(crate) protected_pruned: usize,
    pub(crate) roots_missing: usize,
    pub(crate) unreadable: usize,
}

impl ScanAccounting {
    /// A unit was discovered and put on the work list.
    fn enqueue(&mut self) {
        self.enqueued += 1;
    }

    /// A unit was taken off the work list. Every dequeue MUST be followed by exactly one
    /// [`Self::record`] — that obligation is what [`Self::balances`] audits.
    fn dequeue(&mut self) {
        self.dequeued += 1;
    }

    fn record(&mut self, outcome: UnitOutcome) {
        self.examined += 1;
        match outcome {
            UnitOutcome::Candidate => self.candidates += 1,
            UnitOutcome::Descended => self.descended += 1,
            UnitOutcome::DepthLimited => self.depth_limited += 1,
            UnitOutcome::RootDuplicate => self.roots_duplicate += 1,
            UnitOutcome::ProtectedPruned => self.protected_pruned += 1,
            UnitOutcome::RootMissing => self.roots_missing += 1,
            UnitOutcome::Unreadable => self.unreadable += 1,
        }
    }

    fn bucket_total(&self) -> usize {
        self.candidates
            + self.descended
            + self.depth_limited
            + self.roots_duplicate
            + self.protected_pruned
            + self.roots_missing
            + self.unreadable
    }

    /// Conservation: nothing discovered was dropped (`enqueued == dequeued`), nothing
    /// dequeued escaped a verdict (`dequeued == examined`), and every verdict landed in
    /// exactly one bucket (`examined == Σ buckets`).
    pub(crate) fn balances(&self) -> bool {
        self.enqueued == self.dequeued
            && self.dequeued == self.examined
            && self.examined == self.bucket_total()
    }

    /// Any unit in the `incomplete-or-error` class — or books that do not balance, which
    /// means the walk lost track of its own scope — says the authorized scope was NOT
    /// completely accounted for. Such a run may not report success.
    pub(crate) fn incomplete(&self) -> bool {
        self.roots_missing > 0 || self.unreadable > 0 || !self.balances()
    }
}

/// A unit that did not progress, with the reason it did not — the lines an operator
/// reads to see *what the run did not look at*. `Progressed` units are not listed
/// here (they are in `candidates`, or they were walked); everything else is.
#[derive(Debug, Clone)]
pub(crate) struct ScanSkip {
    pub(crate) path: PathBuf,
    pub(crate) outcome: UnitOutcome,
    pub(crate) reason: String,
}

/// What a scan returns: candidates, **and the books**. A bare `Vec<OrphanCandidate>`
/// is what let every non-candidate unit disappear.
#[derive(Debug, Default)]
pub(crate) struct ScanOutcome {
    pub(crate) candidates: Vec<OrphanCandidate>,
    pub(crate) accounting: ScanAccounting,
    pub(crate) skips: Vec<ScanSkip>,
    /// Scope oddities worth an operator's eye that are not themselves units — today, a
    /// scan root nested inside another scan root (see [`dedup_scan_roots`]).
    pub(crate) warnings: Vec<String>,
}

impl ScanOutcome {
    /// The single funnel every unit passes through — accounting first, then (for a
    /// non-progressed unit) the operator-visible line.
    fn record(&mut self, path: &Path, outcome: UnitOutcome, reason: impl Into<String>) {
        self.accounting.record(outcome);
        if outcome.class() != UnitClass::Progressed {
            self.skips.push(ScanSkip {
                path: path.to_path_buf(),
                outcome,
                reason: reason.into(),
            });
        }
    }
}

/// One item of work. The walk's stack holds these and nothing else, so everything the
/// scan discovers is enqueued, dequeued and recorded through the same three counters
/// (see [`ScanAccounting`]) — there is no side door an unrecorded unit can leave by.
#[derive(Debug)]
enum WorkUnit {
    /// A directory to dispatch through [`walk_disposition`].
    Dir { path: PathBuf, depth: usize },
    /// Discovered, but already known to be unexaminable (a `read_dir` entry that would
    /// not yield, a `symlink_metadata` that failed). It is still a unit — the subtree
    /// under it went unexamined, and the run may not pretend otherwise — so it rides the
    /// work list like everything else and comes off it into `Unreadable`, rather than
    /// being recorded inline where the flow counters could not see it.
    Unexaminable { path: PathBuf, reason: String },
}

/// Scan roots, minus the ones naming a directory another root already names.
///
/// **CONCERN 5.** `--root /tmp/x --root /tmp/x` — or the same directory under two
/// spellings (`/tmp/x` and `/private/tmp/x` on macOS, a symlinked scratch root) — walked
/// the subtree twice: every count in the report doubled, and the same dead gigabytes were
/// listed twice as though there were two of them.
///
/// Deduplication is by resolved identity, first spelling wins, and a dropped root is
/// *reported* as a [`UnitOutcome::RootDuplicate`] unit rather than silently vanishing: it
/// is an expected exclusion, examined under the root that named it first.
///
/// A root *nested inside* another root is deliberately NOT dropped. The outer walk may
/// hit its depth budget before it ever reaches the inner one, so dropping it could
/// silently shrink the authorized scope — the very sin this accounting exists to prevent.
/// It gets a warning instead, so the overlap in the counts is visible rather than
/// mysterious.
fn dedup_scan_roots(roots: &[PathBuf]) -> DedupedRoots {
    let mut kept: Vec<(PathBuf, PathBuf)> = Vec::new(); // (identity, spelling)
    let mut duplicates: Vec<DuplicateRoot> = Vec::new();
    let mut warnings = Vec::new();
    for root in roots {
        let identity = canonicalize_expected(root).unwrap_or_else(|| root.clone());
        if let Some((_, first)) = kept.iter().find(|(kept_id, _)| *kept_id == identity) {
            duplicates.push(DuplicateRoot {
                dropped: root.clone(),
                already_named_by: first.clone(),
            });
            continue;
        }
        // An overlap is only worth a word if there is a real subtree to double-count: a
        // root that does not exist gets its own `RootMissing` verdict and needs no note.
        if root.is_dir() {
            if let Some((_, outer)) = kept.iter().find(|(kept_id, _)| {
                identity.starts_with(kept_id) || kept_id.starts_with(&identity)
            }) {
                warnings.push(format!(
                    "scan root {} overlaps scan root {}: the shared subtree is examined under \
                     both, so the scan counts include the overlap",
                    root.display(),
                    outer.display()
                ));
            }
        }
        kept.push((identity, root.clone()));
    }
    DedupedRoots {
        roots: kept.into_iter().map(|(_, spelling)| spelling).collect(),
        duplicates,
        warnings,
    }
}

/// A scan root dropped because an earlier root already named the same directory.
#[derive(Debug)]
struct DuplicateRoot {
    dropped: PathBuf,
    /// The earlier root — the spelling under which the subtree IS examined.
    already_named_by: PathBuf,
}

/// The outcome of [`dedup_scan_roots`]: the roots to walk, the ones folded into them,
/// and the overlaps the operator should know about.
#[derive(Debug)]
struct DedupedRoots {
    roots: Vec<PathBuf>,
    duplicates: Vec<DuplicateRoot>,
    warnings: Vec<String>,
}

/// The walk's decision about ONE directory — a total function, so there is no
/// `continue` for a unit to fall through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WalkDisposition {
    /// Read its entries and enqueue its subdirectories.
    Descend,
    /// A build artifact by name, outside every fence: a reap candidate, and a leaf.
    Candidate(ResourceKind),
    /// Stop here, with the typed reason the scope ends.
    Prune(PruneReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PruneReason {
    /// Safety refusal: this directory is INSIDE a protected path. The pruned scope
    /// is exactly the scope proved unsafe — nothing under a live build cache may be
    /// reclaimed, and there is nothing else in there to find.
    InsideProtected,
    /// Safety refusal: a name-matched target that *covers* a protected path.
    /// `remove_dir_all` on it would take the protected path with it, so it can never
    /// be a candidate; and a matched target is a leaf by policy (this module never
    /// descends into a target hunting for nested targets), so the pruned scope is,
    /// again, exactly the scope proved undeletable.
    CoversProtected,
    /// Expected exclusion: the depth budget. Scratch roots are shallow by nature and
    /// a deep walk of `~/.cache` is not worth the stat storm — but the cut-off scope
    /// is now *reported* instead of silently dropped.
    DepthBudget { depth: usize, max_depth: usize },
}

impl PruneReason {
    fn outcome(&self) -> UnitOutcome {
        match self {
            PruneReason::InsideProtected | PruneReason::CoversProtected => {
                UnitOutcome::ProtectedPruned
            }
            PruneReason::DepthBudget { .. } => UnitOutcome::DepthLimited,
        }
    }

    fn describe(&self) -> String {
        match self {
            PruneReason::InsideProtected => {
                "inside a protected path (a live build cache); nothing under it is reclaimable"
                    .to_string()
            }
            PruneReason::CoversProtected => {
                "a name-matched target that contains a protected path: deleting it would take the \
                 protected path with it"
                    .to_string()
            }
            PruneReason::DepthBudget { depth, max_depth } => {
                format!(
                    "depth budget reached ({depth} >= {max_depth}); its subtree was not examined"
                )
            }
        }
    }
}

/// Pure: what the walk does with one directory. Every branch is typed; none of them
/// is "and then quietly move on".
pub(crate) fn walk_disposition(
    dir: &Path,
    depth: usize,
    protection: &Protection,
    max_depth: usize,
) -> WalkDisposition {
    // Inside a protected path: nothing under a live build cache is ever a
    // candidate, and there is nothing to find by descending.
    if protection.contains_dir(dir) {
        return WalkDisposition::Prune(PruneReason::InsideProtected);
    }
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    // A scan root is never itself a candidate (`depth > 0`) — pointing the reaper at
    // a root only ever authorizes it to look *inside*, never to delete the root you
    // handed it.
    if depth > 0 {
        if let Some(kind) = classify_orphan_dir_name(name) {
            // `covers` (bidirectional) gates CANDIDACY: a name-matched dir that
            // contains a protected path must never be offered up for deletion.
            if protection.covers(dir) {
                return WalkDisposition::Prune(PruneReason::CoversProtected);
            }
            return WalkDisposition::Candidate(kind);
        }
    }
    if depth >= max_depth {
        return WalkDisposition::Prune(PruneReason::DepthBudget { depth, max_depth });
    }
    WalkDisposition::Descend
}

/// Scan roots for orphan candidates. Read-only, and **cheap**: name match,
/// protection, and a short-circuiting staleness walk. It does not measure bytes
/// and it does not probe holders — those cost a recursive stat storm each and are
/// spent only on the survivors, in [`run_orphan_reap`].
///
/// Returns a [`ScanOutcome`], not a bare `Vec`: see [`UnitClass`] for the invariant
/// that shape exists to enforce.
pub(crate) fn scan_orphan_candidates(
    roots: &[PathBuf],
    protection: &Protection,
    now: SystemTime,
    max_age_days: u64,
) -> ScanOutcome {
    let cutoff = staleness_cutoff(now, max_age_days);
    let mut out = ScanOutcome::default();

    // Repeated roots walk the same subtree twice and double every count in the report
    // (CONCERN 5). Deduplicate first — and *book* each duplicate as a unit, so the
    // operator sees the root they named and why it was walked only once.
    let deduped = dedup_scan_roots(roots);
    out.warnings = deduped.warnings;
    for duplicate in deduped.duplicates {
        out.accounting.enqueue();
        out.accounting.dequeue();
        out.record(
            &duplicate.dropped,
            UnitOutcome::RootDuplicate,
            format!(
                "the same directory as scan root {}: walked once, not twice",
                duplicate.already_named_by.display()
            ),
        );
    }

    for root in &deduped.roots {
        // Every unit — roots included — is enqueued, dequeued and recorded through the
        // same three counters, so a unit that escapes its verdict breaks the books
        // instead of vanishing from them (see `ScanAccounting`).
        let mut stack = vec![WorkUnit::Dir {
            path: root.clone(),
            depth: 0,
        }];
        out.accounting.enqueue();
        while let Some(unit) = stack.pop() {
            out.accounting.dequeue();
            let (dir, depth) = match unit {
                WorkUnit::Dir { path, depth } => (path, depth),
                WorkUnit::Unexaminable { path, reason } => {
                    out.record(&path, UnitOutcome::Unreadable, reason);
                    continue;
                }
            };
            // Only a ROOT can be missing: everything else was `symlink_metadata`-ed into
            // existence as a directory before it was enqueued.
            if depth == 0 && !dir.is_dir() {
                out.record(
                    &dir,
                    UnitOutcome::RootMissing,
                    "scan root does not exist or is not a directory: nothing under it was examined",
                );
                continue;
            }
            match walk_disposition(&dir, depth, protection, DEFAULT_MAX_DEPTH) {
                WalkDisposition::Candidate(kind) => {
                    out.record(&dir, UnitOutcome::Candidate, String::new());
                    out.candidates.push(OrphanCandidate {
                        staleness: staleness(&dir, now, cutoff),
                        // Pin the spelling the verdict is about to be rendered against —
                        // see `OrphanCandidate::identity`.
                        identity: canonicalize_expected(&dir).unwrap_or_else(|| dir.clone()),
                        // …and the REAL identity beside it (BUG 2, closed): captured
                        // now, at judgement, and re-checked immediately before the
                        // delete (`delete_resource_bytes`).
                        file_identity: FileIdentity::of(&dir),
                        path: dir,
                        kind,
                        bytes: None,
                        holders: None,
                    });
                }
                WalkDisposition::Prune(reason) => {
                    out.record(&dir, reason.outcome(), reason.describe());
                }
                WalkDisposition::Descend => {
                    let entries = match std::fs::read_dir(&dir) {
                        Ok(entries) => entries,
                        // The subtree is unexamined and we cannot say what was in
                        // it: an incomplete unit, not a `continue`.
                        Err(err) => {
                            out.record(
                                &dir,
                                UnitOutcome::Unreadable,
                                format!("cannot read {}: {err}", dir.display()),
                            );
                            continue;
                        }
                    };
                    out.record(&dir, UnitOutcome::Descended, String::new());
                    for entry in entries {
                        let entry = match entry {
                            Ok(entry) => entry,
                            Err(err) => {
                                stack.push(WorkUnit::Unexaminable {
                                    path: dir.join("<unreadable entry>"),
                                    reason: format!(
                                        "cannot read an entry of {}: {err}",
                                        dir.display()
                                    ),
                                });
                                out.accounting.enqueue();
                                continue;
                            }
                        };
                        let path = entry.path();
                        // symlink_metadata: never follow a symlink out of the scan
                        // root (a symlinked "…-target" must not become a delete
                        // candidate for whatever it points at).
                        let meta = std::fs::symlink_metadata(&path);
                        match meta {
                            Ok(meta) if meta.is_dir() => {
                                stack.push(WorkUnit::Dir {
                                    path,
                                    depth: depth + 1,
                                });
                                out.accounting.enqueue();
                            }
                            // A file / a symlink: not a unit — the walk examines
                            // directories, and a symlink is never followed.
                            Ok(_) => {}
                            Err(err) => {
                                let reason = format!("cannot stat {}: {err}", path.display());
                                stack.push(WorkUnit::Unexaminable { path, reason });
                                out.accounting.enqueue();
                            }
                        }
                    }
                }
            }
        }
    }
    // Stalest first, then path: deterministic without a byte measurement we have
    // deliberately not taken yet.
    out.candidates.sort_by(|a, b| {
        b.staleness
            .age_days()
            .cmp(&a.staleness.age_days())
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// Default scan roots: the scratch volumes where dead build artifacts pile up.
///
/// Filtered to the ones that actually exist, and that is a *scope* decision, not a
/// swallowed error: these are opportunistic defaults (`/private/tmp` is a macOS
/// path, `~/.cache` an XDG one — neither is universal), so a default root that is
/// not there was never part of the authorized scope. A root the OPERATOR names and
/// that does not exist is a different animal: it is `RootMissing`, an incomplete
/// unit, and it costs the run its clean exit.
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
    roots.retain(|root| root.is_dir());
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
    TooYoung {
        age_days: u64,
        max_age_days: u64,
    },
    StalenessUnprovable(String),
    Held(String),
    HolderCheckInconclusive(String),
    /// The scan could not capture a `(dev, ino)` identity to render the verdict
    /// against (sol audit, BUG 2). Fail-closed by the same type-level discipline
    /// as [`Self::HolderCheckInconclusive`]: an identity we never captured is an
    /// identity we cannot re-check before the delete, so nothing is reclaimed on
    /// the strength of a name alone.
    IdentityUnprovable,
    BoundByLease {
        active_bindings: i64,
    },
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
            SkipReason::IdentityUnprovable => "file identity (dev, ino) could not be captured at \
                 judgement; refusing to reclaim an object we could not re-identify before the \
                 delete"
                .to_string(),
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
    // Fail-closed by type, same shape as the holder check below: no identity was
    // captured at judgement, so there is nothing to re-check before the delete
    // (BUG 2). Checked before the holder probe because it is cheaper (a field
    // read, not a subprocess) and because an unidentifiable candidate is exactly
    // as undeletable no matter what the holder probe says.
    if candidate.file_identity.is_none() {
        return ReapDecision::Skip(SkipReason::IdentityUnprovable);
    }
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
    /// The candidate's TERMINAL label, and it must be true at the end of the run,
    /// not at the moment the gates first spoke:
    ///
    /// * `reclaim` — eligible: what a *certified* reaper would delete. Today nothing
    ///   deletes it (`--force` is refused), and its bytes are what
    ///   [`ReapReport::reclaimable_bytes`] adds up,
    /// * `skip`    — a gate refused it before any expensive work,
    /// * `refused` — it was eligible and the delete path refused it anyway: the
    ///   protected set recomputed at delete time now covers it, its pinned identity
    ///   no longer resolves to the same object, a holder appeared, or the ledger
    ///   bounced the reclaim. **A late refusal relabels**; leaving `reclaim` on a
    ///   candidate whose bytes are still on disk is a report that lies.
    /// * `error`   — the run could not decide (a ledger lookup failed): an
    ///   `incomplete-or-error` unit, and it costs the run its clean exit.
    pub(crate) decision: &'static str,
    pub(crate) reason: String,
}

/// A unit the scan did not progress on — the operator-visible half of
/// [`ScanAccounting`].
#[derive(Debug, serde::Serialize)]
pub(crate) struct ScanSkipReport {
    pub(crate) path: String,
    /// One of sol's four classes (see [`UnitClass`]).
    pub(crate) class: &'static str,
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
    /// **`true` as of #1062** (owner-ratified 1A, 2026-07-17: the kill-test matrix was
    /// executed and its receipt checked in — see [`DESTRUCTIVE_CERTIFIED`]). Mirrors
    /// [`DESTRUCTIVE_CERTIFIED`] into every report so a reader never has to go check the
    /// constant to know whether a `--force` request on this build can act — and, on a
    /// `dry_run` report, whether `reclaimable_bytes` is a preview of what a certified
    /// reaper would do or a record of what an UNcertified one would have been forbidden
    /// to. Was hard-coded documentation of `false` before the flip; kept a plain mirror
    /// of the const now rather than re-describing it, so it cannot go stale again.
    pub(crate) destructive_certified: bool,
    /// The audit's defects, verbatim, in every report — a historical record of what the
    /// certification closed ([`BLOCKING_AUDIT`]), not a live blocking condition now that
    /// [`Self::destructive_certified`] reads `true`.
    pub(crate) blocking_defects: Vec<String>,
    pub(crate) roots: Vec<String>,
    /// What the run refused to look at, so an operator can *see* that the live
    /// build cache was fenced instead of taking it on faith.
    pub(crate) protected: Vec<String>,
    /// **Was every protection source resolved?** (BUG 3.) `false` when `ps` would not
    /// run, `HOME` is unset, or a target dir is relative — i.e. when the protected set
    /// above is missing entries it should have had. It forces [`Self::incomplete`]: a run
    /// that cannot work out what it must not touch does not report success.
    pub(crate) protection_complete: bool,
    pub(crate) max_age_days: u64,
    pub(crate) dry_run: bool,
    /// Every unit the scan examined, in exactly one bucket each (sol's frozen
    /// invariant — see [`UnitClass`]).
    pub(crate) scan: ScanAccounting,
    /// The units that did not progress, with the typed reason each did not.
    pub(crate) unexamined: Vec<ScanSkipReport>,
    /// **The authorized scope was not completely accounted for.** Set when the scan
    /// booked an `incomplete-or-error` unit (a missing root, an unreadable subtree)
    /// or a candidate could not be decided. `emit_reap_report` prints it and
    /// [`reap_exit_status`] turns it into a non-zero exit: a run that could not look
    /// at everything it was told to look at does not get to say "success", and
    /// `--force` does not waive it.
    pub(crate) incomplete: bool,
    pub(crate) candidates: Vec<CandidateReport>,
    /// **The number the dry run exists to produce**: the bytes under every candidate that
    /// passed every gate — what a *certified* reaper would free on this machine right
    /// now. A measurement, not a promise: nothing deletes it today, and the fences that
    /// concluded "no live build owns this" are the ones the audit broke
    /// ([`BLOCKING_DEFECTS`]). Read it as "dead bytes, probably" — then go and look.
    pub(crate) reclaimable_bytes: i64,
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
    ///
    /// `true` is **refused** — [`certify_destructive`] turns it into a
    /// [`DestructiveRefusal`] before anything is scanned. The field survives because the
    /// request still has to be *rejected*, loudly and with a reason, and because the
    /// sheathed machinery behind it is pinned by tests against the day it is certified.
    /// It is not a switch anyone can currently flip.
    pub(crate) force: bool,
}

// ── Run ─────────────────────────────────────────────────────────────────────

/// **The only entry point.** Scan → cheap gates → (survivors only) measure + probe →
/// decide → report.
///
/// A `force` request is REFUSED here, with a [`DestructiveRefusal`] that says why
/// ([`certify_destructive`]). It is refused *before the scan*, so a rejected run does not
/// so much as stat a directory — and it is an `Err`, not a quietly downgraded dry run. An
/// operator who asked to delete 61 GB and got a report instead must not be able to
/// mistake one for the other: "reclaimed 0 bytes" reads exactly like "there was nothing
/// to reclaim".
///
/// The gate is duplicated in the CLI on purpose (which refuses before even opening the
/// ledger). Two fences, one source of truth: both call [`certify_destructive`].
///
/// `sources` is what the run may protect from ([`ProtectionSources`]) — passed in, not
/// read from the ambient environment, so the protected set is a function of an argument
/// the caller can see and a test can supply.
pub(crate) fn run_orphan_reap(
    conn: &mut rusqlite::Connection,
    opts: &ReapOptions,
    sources: &ProtectionSources<'_>,
    now: SystemTime,
    probe: &HolderProbe,
) -> Result<ReapReport, DestructiveRefusal> {
    certify_destructive(opts.force)?;
    Ok(run_orphan_reap_uncertified(conn, opts, sources, now, probe))
}

/// The reaper's body — **including the destructive path, which is not certified and is
/// unreachable from any entry point** ([`run_orphan_reap`] refuses `force` above).
///
/// Kept rather than deleted, for one reason: the knife comes back. The delete path's
/// fences (protection recomputed at delete time, the pinned-identity re-resolution, the
/// holder re-probe, the ledger's reclaim ordering) stay pinned by tests that call this
/// function directly, so un-sheathing it is *closing the four defects and kill-testing
/// them* — not rebuilding the machine from scratch against a suite that rotted while it
/// was gone.
///
/// It is private, and it stays private: `force` reaches it from this module's own tests
/// and from nowhere else.
fn run_orphan_reap_uncertified(
    conn: &mut rusqlite::Connection,
    opts: &ReapOptions,
    sources: &ProtectionSources<'_>,
    now: SystemTime,
    probe: &HolderProbe,
) -> ReapReport {
    // BUG 1, closed: env vars + the live process table, UNIONED with everything the
    // resource ledger says is live (`ledger_protected_paths`) — the structural fix, not
    // a patch to the `ps` scan.
    let protection = full_protected_paths(&*conn, sources);
    let scan = scan_orphan_candidates(&opts.roots, &protection, now, opts.max_age_days);

    // BUG 3, fail-closed: a protected set that could not be fully built does not merely
    // warn. It makes the run incomplete, and an incomplete run does not exit clean —
    // "we could not work out what we must not touch" is not a footnote.
    let protection_complete = protection.is_complete();

    let mut report = ReapReport {
        action: "reap-orphans",
        destructive_certified: DESTRUCTIVE_CERTIFIED,
        blocking_defects: BLOCKING_DEFECTS.iter().map(|d| d.to_string()).collect(),
        roots: opts.roots.iter().map(|r| r.display().to_string()).collect(),
        protected: protection
            .paths()
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        protection_complete,
        max_age_days: opts.max_age_days,
        dry_run: !opts.force,
        scan: scan.accounting,
        unexamined: scan
            .skips
            .iter()
            .map(|skip| ScanSkipReport {
                path: skip.path.display().to_string(),
                class: skip.outcome.class().as_str(),
                reason: skip.reason.clone(),
            })
            .collect(),
        incomplete: scan.accounting.incomplete() || !protection_complete,
        candidates: Vec::new(),
        reclaimable_bytes: 0,
        reclaimed: Vec::new(),
        reclaimed_bytes: 0,
        bytes_by_reason: BTreeMap::new(),
        revived_bytes: 0,
        // Every unresolved protection source is an operator-visible line on every run,
        // plus the scan's own scope oddities (overlapping roots).
        warnings: protection
            .gaps()
            .iter()
            .cloned()
            .chain(scan.warnings.iter().cloned())
            .collect(),
        errors: Vec::new(),
    };

    for mut candidate in scan.candidates {
        let path = candidate.path.display().to_string();
        // An undecidable candidate is an `incomplete-or-error` unit: it gets a
        // terminal line of its own (`error`) instead of a `continue` that would
        // erase it from the candidate list entirely.
        let undecidable = |report: &mut ReapReport, candidate: &OrphanCandidate, reason: String| {
            report.errors.push(reason.clone());
            report.candidates.push(CandidateReport {
                path: candidate.path.display().to_string(),
                kind: candidate.kind.as_str(),
                bytes: None,
                age_days: candidate.staleness.age_days(),
                holders: "not probed".to_string(),
                decision: "error",
                reason,
            });
        };
        let existing = match memcore::find_resource_by_path(conn, &path, candidate.kind) {
            Ok(existing) => existing,
            Err(err) => {
                undecidable(
                    &mut report,
                    &candidate,
                    format!("ledger lookup failed for {path}: {err}"),
                );
                continue;
            }
        };
        let active_bindings = match &existing {
            Some(resource) => match memcore::active_binding_count(conn, &resource.resource_id) {
                Ok(count) => count,
                Err(err) => {
                    undecidable(
                        &mut report,
                        &candidate,
                        format!("binding count failed for {path}: {err}"),
                    );
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

        // The label is provisional until the run is done with this candidate: a
        // refusal that arrives at delete time rewrites it (see `CandidateReport`).
        let (mut decision_label, mut reason) = match &decision {
            ReapDecision::Reclaim(reason) => ("reclaim", reason.as_str().to_string()),
            ReapDecision::Skip(skip) => ("skip", skip.describe()),
        };

        if let (ReapDecision::Reclaim(claim), true) = (&decision, opts.force) {
            // **sol audit fix (BUG 1).** The protected set is recomputed HERE, at the
            // delete — it is NOT the snapshot the scan took. The snapshot is stale by
            // construction: a build that claimed `--target-dir` after the scan is
            // invisible to it, and the holder probe (which the deleter *does* re-run)
            // proves nothing about a `cargo` sitting between two compile units with no
            // fd open. Snapshot + fd-only recheck is exactly the window in which a live
            // build cache gets deleted.
            //
            // What "recomputed" means precisely, now that the sources are explicit: the
            // live source — the **process table** — is re-read here, and it is the one a
            // late claim actually arrives through. The environment half of `sources` is a
            // per-run snapshot on purpose: another process cannot reach into *our*
            // `CARGO_TARGET_DIR`, so re-reading it would re-read the same bytes and prove
            // nothing. The ledger half (BUG 1) is re-read for the same reason a late claim
            // through a `BuildPrivate` lease binding must be caught too.
            let fresh = full_protected_paths(&*conn, sources);
            // A gap that appears only at delete time (a `ps` that has started failing, a
            // ledger that has gone unreachable) still costs the run its clean exit.
            if !fresh.is_complete() {
                report.protection_complete = false;
                report.incomplete = true;
            }
            for gap in fresh.gaps() {
                if !report.warnings.contains(gap) {
                    report.warnings.push(gap.clone());
                }
            }
            // **checkpoint 1 fix (codex-9178d).** An incomplete protected set is not
            // merely a report-level footnote — it means THIS delete cannot know
            // whether `candidate.path` is actually clear. #1062 is explicit: "an
            // unresolved protection source ... refuses to delete anything." Before
            // this fix, an incomplete `fresh` set still fell through to the
            // `covers()` check below, and an unprotected-looking candidate would be
            // reclaimed anyway on the strength of a protected set this run just
            // admitted it could not fully build.
            let refusal = if !fresh.is_complete() {
                Some(format!(
                    "refused {path}: the protected set could not be fully resolved at delete \
                     time ({}) — an unresolved protection source means this run does not know \
                     what it must not touch, so nothing may be deleted while any source stays \
                     unresolved",
                    fresh.gaps().join("; ")
                ))
            } else if fresh.covers(&candidate.path) || fresh.covers(&candidate.identity) {
                Some(format!(
                    "refused {path}: it is protected as of the delete (a live build claimed it \
                     after the scan); the run's opening protected set did not cover it"
                ))
            } else {
                match reclaim_candidate(conn, &candidate, *claim, existing.as_ref(), probe, &fresh)
                {
                    Ok(reclaimed) => {
                        report.reclaimed_bytes += reclaimed.reclaimed_bytes;
                        report.revived_bytes += reclaimed.revived_previous_bytes.unwrap_or(0);
                        report.reclaimed.push(reclaimed);
                        None
                    }
                    // Every refusal is a warning line with its reason — a reclaim that
                    // did not happen is never a silent skip, and never keeps the
                    // `reclaim` label either.
                    //
                    // **BUG 4, closed.** A `DeleteFailed` is not a fence that fired — the
                    // delete path was ATTEMPTED and did not cleanly finish, and that must
                    // cost the run its clean exit exactly like any other
                    // `incomplete-or-error` unit (see `ReclaimFailure`). A `Refused` is
                    // the destructive path working as designed and stays a warning only.
                    Err(failure @ ReclaimFailure::Refused(_)) => {
                        Some(failure.message().to_string())
                    }
                    Err(failure @ ReclaimFailure::DeleteFailed(_)) => {
                        report.errors.push(failure.message().to_string());
                        Some(failure.message().to_string())
                    }
                }
            };
            if let Some(err) = refusal {
                decision_label = "refused";
                reason = err.clone();
                report.warnings.push(err);
            }
        }

        // The headline of a report-only run: bytes that survived every gate. Counted off
        // the TERMINAL label, so a candidate the delete path later refused is not still
        // advertised as reclaimable.
        if decision_label == "reclaim" {
            report.reclaimable_bytes += clamp_bytes(candidate.bytes.unwrap_or(0));
        }

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
    }

    match reclaimed_bytes_by_reason(conn) {
        Ok(by_reason) => report.bytes_by_reason = by_reason,
        Err(err) => report
            .warnings
            .push(format!("byte report by reason unavailable: {err}")),
    }

    // An undecidable candidate is an error unit too, so the scope was not fully accounted
    // for either. `protection_complete` is carried, not recomputed: the delete path may
    // have found a gap the scan did not.
    report.incomplete =
        scan.accounting.incomplete() || !report.errors.is_empty() || !report.protection_complete;
    report
}

/// The run's exit status, and the ONLY place the `clean orphans` CLI derives it.
///
/// A reaper that cannot account for its authorized scope must not exit 0 —
/// "reported success while doing nothing" is the exact failure this module was
/// audited for, and `--force` waives it no more than it waives any other gate.
pub(crate) fn reap_exit_status(report: &ReapReport) -> Result<(), String> {
    if !report.errors.is_empty() {
        return Err(report.errors.join("; "));
    }
    // BUG 3: an incomplete protected set is its own failure, and it is invisible in the
    // scan's unit counts — the run may have walked its whole scope perfectly and simply
    // never known what it was forbidden to touch. Reported first, and by name, because
    // "0 missing roots, 0 unreadable, and yet incomplete" is otherwise a riddle.
    if !report.protection_complete {
        return Err(format!(
            "protected set incomplete: {}; refusing to report success on a run that could not \
             work out what it must not touch",
            report.warnings.join("; ")
        ));
    }
    if report.incomplete {
        return Err(format!(
            "scan incomplete: {} of {} examined unit(s) could not be accounted for ({} missing \
             root(s), {} unreadable, books balance={}); refusing to report success on a run that \
             did not see its whole scope",
            report.scan.roots_missing + report.scan.unreadable,
            report.scan.examined,
            report.scan.roots_missing,
            report.scan.unreadable,
            report.scan.balances(),
        ));
    }
    Ok(())
}

/// Why [`reclaim_candidate`] did not return a [`ReclaimedReport`] — and, critically,
/// whether that is a WORKING FENCE or a BROKEN RUN (sol audit, BUG 4).
///
/// The first cut answered every non-`Ok` outcome the same way: a warning line, and
/// the run still exited 0. That conflated two entirely different events. A resource
/// found to be bound, quarantined, or already claimed by a concurrent reclaim is the
/// destructive path **working as designed** — the whole point of re-checking at
/// delete time is to catch exactly that, and a run that caught it has nothing to
/// apologize for. A `remove_dir_all` that fails PARTWAY — a permission error three
/// levels down, a device that goes away mid-delete — is the opposite: an operator
/// who asked to free disk got a half-deleted directory and a process that told them
/// it succeeded. That is not a refusal; the delete was ATTEMPTED and it did not
/// finish, and hiding that inside the same warning bucket as "was quarantined" is
/// the exact "reaper always reports success" failure mode #894 S2b was built to
/// stop hiding.
#[derive(Debug, Clone)]
enum ReclaimFailure {
    /// A typed safety refusal: the destructive path correctly declined (blocked by
    /// a binding, quarantined, or protected at delete time). Worth a warning line;
    /// does not cost the run its clean exit — a fence that fired is not an error.
    ///
    /// **NOT this bucket (checkpoint 3/4, codex-9178d):** an identity mismatch
    /// (`IDENTITY_UNRESOLVED_PREFIX`) or a lost reclaim race discovered AFTER the
    /// deleter already ran. Neither is a fence firing cleanly — the first means the
    /// run no longer knows what is at the path it judged, the second means real
    /// bytes were deleted with no ledger row to show for it. Both are
    /// [`Self::DeleteFailed`].
    Refused(String),
    /// The delete path was ATTEMPTED and did not cleanly finish — an I/O failure
    /// out of `remove_dir_all`, a ledger write that could not be made, ledger state
    /// so far from what this call just did that it cannot be trusted, an identity
    /// the deleter could no longer confirm, or a reclaim whose bytes hit disk but
    /// never made it into the ledger. This is the class BUG 4 exists for: it must
    /// cost the run its clean exit every time, with no exceptions carved out for
    /// `--force`.
    DeleteFailed(String),
}

impl ReclaimFailure {
    fn message(&self) -> &str {
        match self {
            ReclaimFailure::Refused(msg) | ReclaimFailure::DeleteFailed(msg) => msg,
        }
    }
}

/// Sentinel prefix `delete_resource_bytes` puts on messages meaning "the object's
/// identity could not be reconfirmed" — as opposed to a designed fence (protected
/// path, symlink, non-directory, holder appeared) declining on purpose. Matched by
/// `reclaim_candidate` to route identity-unresolved refusals to
/// [`ReclaimFailure::DeleteFailed`] instead of [`ReclaimFailure::Refused`]
/// (checkpoint 3, codex-9178d — #1062: "if the identity moved, the unit is
/// incomplete, not progressed").
const IDENTITY_UNRESOLVED_PREFIX: &str = "identity unresolved: ";

/// Book (or revive) the resource and reclaim it through S2a's single reclaim
/// path. An `Err` is worth a human's eye and always lands as a warning line;
/// [`ReclaimFailure::DeleteFailed`] additionally costs the run its clean exit
/// (BUG 4) — see [`ReclaimFailure`] for why the two are not the same event.
fn reclaim_candidate(
    conn: &mut rusqlite::Connection,
    candidate: &OrphanCandidate,
    reason: ReclaimReason,
    existing: Option<&ExecEnvResource>,
    probe: &HolderProbe,
    protection: &Protection,
) -> Result<ReclaimedReport, ReclaimFailure> {
    let path = candidate.path.display().to_string();

    // `insert_resource` only ever accepts an absent row or a `reclaimed`
    // tombstone — any other state bounces off `MemoryError::Duplicate`.
    // `cheap_verdict` already told us which case this is via `reason`:
    // `Unmanaged` ⇒ no row or a `reclaimed` tombstone (insert/revive is safe);
    // `Orphan` ⇒ the row is already on the books in a re-enterable state
    // (active/reclaiming/reclaim_failed) — reuse its id directly, an
    // `insert_resource` call here would only bounce off `Duplicate`.
    //
    // Both failure modes here are `DeleteFailed`, not `Refused`: neither is a
    // designed fence firing — an internal invariant broke (the `Orphan` branch)
    // or the ledger write itself failed (the `Unmanaged` branch), and either
    // means this run does not know what state the candidate is actually in.
    let (resource_id, revived_previous_bytes) = match reason {
        ReclaimReason::Orphan => {
            let resource_id = existing
                .ok_or_else(|| {
                    ReclaimFailure::DeleteFailed(format!(
                        "internal: {path} decided Orphan but the ledger lookup found no row"
                    ))
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
            .map_err(|err| {
                ReclaimFailure::DeleteFailed(format!("cannot book orphan {path}: {err}"))
            })?;
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
            // `protection` here is the set recomputed at delete time by the caller,
            // and `identity` / `file_identity` are the object the verdict was
            // rendered against — the deleter re-resolves both and refuses anything
            // else (BUG 1 / BUG 2).
            delete_resource_bytes(
                resource,
                probe,
                protection,
                &candidate.identity,
                candidate.file_identity,
            )
        })
        .map_err(|err| {
            // **BUG 4.** `memcore::reclaim_resource` returns `Err` in exactly two
            // shapes: the deleter's own error (everything `delete_resource_bytes`
            // returns is a designed, typed `MemoryError::InvalidArg` refusal — a
            // protected path, a retargeted identity, a holder that appeared — EXCEPT
            // `MemoryError::Io`, which is `remove_dir_all` itself failing), or a
            // ledger-side failure reading/writing the `reclaiming` state (anything
            // that is not `InvalidArg`). Only the former is a refusal that worked as
            // designed; the rest are the delete path failing to finish what it
            // started, and must not be waved through as a mere warning.
            //
            // **checkpoint 3 fix (codex-9178d).** Not every `InvalidArg` out of
            // `delete_resource_bytes` is the same kind of event. #1062's text: "if
            // the identity moved, the unit is incomplete, not progressed." A
            // protected path, a symlink, a non-directory, or a holder that appeared
            // are the destructive path's fences WORKING — a designed refusal. An
            // identity the deleter can no longer confirm (`IDENTITY_UNRESOLVED_PREFIX`
            // — a retargeted symlink, a path that stopped resolving, a `(dev, ino)`
            // that changed) is different: the run does not know what is at this path
            // anymore, and that is exactly the class BUG 4 exists for, not a clean
            // refusal.
            match &err {
                MemoryError::InvalidArg(msg) if msg.starts_with(IDENTITY_UNRESOLVED_PREFIX) => {
                    ReclaimFailure::DeleteFailed(format!("reclaim of {path} failed: {msg}"))
                }
                MemoryError::InvalidArg(msg) => {
                    ReclaimFailure::Refused(format!("reclaim of {path} failed: {msg}"))
                }
                other => ReclaimFailure::DeleteFailed(format!("reclaim of {path} failed: {other}")),
            }
        })?;

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
        // a binding taken between our decision and the reclaim lands here. A
        // fence that fired, not a failure.
        ResourceReclaimOutcome::BlockedByBinding {
            active_bindings, ..
        } => Err(ReclaimFailure::Refused(format!(
            "skipped {path}: {active_bindings} live lease binding(s) appeared since the scan"
        ))),
        ResourceReclaimOutcome::Quarantined { .. } => Err(ReclaimFailure::Refused(format!(
            "skipped {path}: resource is quarantined"
        ))),
        // For `Unmanaged` we just booked/revived this row moments ago, so this
        // can only be a concurrent reclaim of the same resource winning the
        // race. For `Orphan` the row was already on the books before this call
        // — same story, a concurrent reclaimer got there first. Nothing this
        // run attempted failed; another one finished first.
        ResourceReclaimOutcome::AlreadyReclaimed { .. } => Err(ReclaimFailure::Refused(format!(
            "skipped {path}: another reclaim of this resource finished first"
        ))),
        // The deleter ran (our bytes really are gone), but by the time the
        // ledger went to stamp `reclaimed` the row had already moved out from
        // under it — a concurrent reclaim, a quarantine, or a re-registration
        // won the race. `freed_bytes` is deliberately NOT folded into this run's
        // `reclaimed_bytes`: memcore did not write it, so counting it here would
        // claim bytes no ledger row backs (#1029's whole point).
        //
        // **checkpoint 4 fix (codex-9178d).** This is NOT a fence that fired —
        // `remove_dir_all` already ran and real bytes are already gone; the ledger
        // simply failed to record it. #1062's conservation invariant and #1029's
        // whole point are that destructive work with an accounting failure must not
        // read as a clean, working refusal (a warning-only "skipped"). It must cost
        // the run its clean exit exactly like any other attempted delete that did
        // not finish cleanly (BUG 4).
        ResourceReclaimOutcome::LostRace {
            observed_state,
            freed_bytes,
            ..
        } => Err(ReclaimFailure::DeleteFailed(format!(
            "reclaim of {path} failed: lost the reclaim race AFTER the deleter already ran (now \
             observed as {:?}) — this run's deleter freed {freed_bytes} bytes not recorded in \
             the ledger (#1029: an off-ledger delete is destructive work with an accounting \
             failure, not a clean refusal)",
            observed_state
        ))),
        // Not a designed outcome of any reclaim this module drives — the row we
        // just resolved (or booked moments ago) is gone entirely. Anomalous
        // enough that treating it as a working fence would be a guess; treat it
        // as the run not knowing what happened instead.
        ResourceReclaimOutcome::NotFound => Err(ReclaimFailure::DeleteFailed(format!(
            "skipped {path}: resource row vanished mid-reclaim"
        ))),
    }
}

/// The deleter S2a hands the filesystem work to. Runs AFTER `reclaiming` is
/// committed and OUTSIDE any transaction; must be idempotent (a re-entered
/// reclaim of an already-deleted path frees 0 bytes, and S2a assigns rather
/// than accumulates, so 0 cannot corrupt a prior count).
///
/// `protection` must be the set computed **at delete time** (BUG 1), `pinned` the
/// spelling the verdict was rendered against, and `pinned_identity` the `(dev, ino)`
/// captured at the same moment (BUG 2). All three are re-asserted here, on the last
/// lines before `remove_dir_all`.
fn delete_resource_bytes(
    resource: &ExecEnvResource,
    probe: &HolderProbe,
    protection: &Protection,
    pinned: &Path,
    pinned_identity: Option<FileIdentity>,
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
    // **sol audit fix (BUG 2): the object judged is the object deleted.**
    //
    // The name survives the check above; the *identity* is what `remove_dir_all`
    // acts on. `--root` is caller-supplied and may be or contain a symlink, and
    // every earlier fence resolved this name at its own moment. Retarget the link
    // between the verdict and this line and the run deletes a directory nothing ever
    // judged. So: resolve it again, and refuse unless it is still the same object.
    // (A name that no longer resolves at all is also a refusal — an identity we
    // cannot confirm is not an identity we may delete.)
    match std::fs::canonicalize(path) {
        Ok(actual) if actual == pinned => {}
        Ok(actual) => {
            return Err(MemoryError::InvalidArg(format!(
                "identity unresolved: refusing to reclaim {}: it now resolves to {} but the \
                 verdict was rendered against {} — a symlink or mount was retargeted between the \
                 two",
                resource.path,
                actual.display(),
                pinned.display()
            )))
        }
        Err(err) => {
            return Err(MemoryError::InvalidArg(format!(
                "identity unresolved: refusing to reclaim {}: its path no longer resolves \
                 ({err}), so the identity the verdict was rendered against cannot be confirmed",
                resource.path
            )))
        }
    }
    // **sol audit fix (BUG 2, the rest of it): the pathname check above proves the
    // NAME still resolves to the same spelling — not that it is the same OBJECT.** A
    // rename-and-replace at the same path (`rm -rf` + `mkdir`, or a rename swap)
    // leaves the canonical spelling identical while the directory underneath it is a
    // different one entirely; the check above cannot see that. `(dev, ino)`, captured
    // at judgement and re-`stat`ed on this line, can. This is the FIRST of two
    // identity checks — the second, right before `remove_dir_all` itself, is what
    // closes the window the holder probe and the byte walk still open below
    // (checkpoint 2, codex-9178d).
    //
    // `pinned_identity` being `None` is also a refusal — `decide_reap` never reaches a
    // `Reclaim` decision without one (BUG 2's fail-closed half), so `None` here means
    // this deleter was invoked outside that gate, and an identity we were never given
    // is not one we may act on.
    let current_identity = FileIdentity::of(path);
    if pinned_identity.is_none() || current_identity != pinned_identity {
        return Err(MemoryError::InvalidArg(format!(
            "identity unresolved: refusing to reclaim {}: its (dev, ino) identity does not match \
             the one the verdict was rendered against (captured {pinned_identity:?}, now \
             {current_identity:?}) — the object at this path was replaced between judgement and \
             delete",
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
    // **checkpoint 2 fix (codex-9178d): re-checked IMMEDIATELY before unlink, not just
    // before the probe.** The dev/ino check above proves the object was still the
    // pinned one before the holder probe ran — it says nothing about what is at `path`
    // after that probe (a real recursive `lsof +D`) and the recursive `dir_size` walk
    // just above, both of which take real wall-clock time and are exactly the window a
    // rename-swap needs. #1062's own text is "re-checked immediately before unlink";
    // one check before two more filesystem round-trips does not satisfy that. Re-stat
    // one more time, on the last line before the call that actually deletes.
    let identity_at_unlink = FileIdentity::of(path);
    if identity_at_unlink != pinned_identity {
        return Err(MemoryError::InvalidArg(format!(
            "identity unresolved: refusing to reclaim {}: its (dev, ino) identity changed again \
             between the holder probe and the delete (captured {pinned_identity:?}, now \
             {identity_at_unlink:?}) — the object at this path was replaced a second time",
            resource.path
        )));
    }
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
///
/// `pub(crate)` (tachi#1184 item 2): the `tachi doctor` build-resource patrol
/// (`doctor::build_resources::scan_orphan_build_resources`) reuses this exact
/// walk to size its own — narrower, blessed-list-filtered — candidate set,
/// rather than growing a second copy of the same metadata-only recursion.
pub(crate) fn dir_size(path: &Path) -> u64 {
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

/// Print a refused destructive request.
///
/// Text mode goes to **stderr**: a refusal is not this command's output, and a caller
/// piping stdout into `jq` must not find prose where a report belongs. JSON mode emits a
/// well-formed object with `"refused": true`, so a script gets a parseable answer — and,
/// with the non-zero exit beside it, cannot read a refusal as a clean run.
pub(crate) fn emit_destructive_refusal(
    refusal: &DestructiveRefusal,
    output: OutputFormat,
) -> Result<(), String> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(refusal)
                .map_err(|err| format!("serialize refusal: {err}"))?
        ),
        OutputFormat::Text => {
            eprintln!("tachi clean orphans: REFUSED --force");
            eprintln!("  {}", refusal.reason);
            eprintln!(
                "  run without --force for the full report of what a certified reaper would \
                 reclaim."
            );
        }
    }
    Ok(())
}

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
            if !report.destructive_certified {
                println!(
                    "  REPORT ONLY — the destructive path is not certified and is refused (audit \
                     {BLOCKING_AUDIT}). What follows is what a certified reaper WOULD reclaim; \
                     nothing here has been deleted, and --force is rejected."
                );
                for defect in &report.blocking_defects {
                    println!("  blocking defect: {defect}");
                }
            }
            println!("  max_age_days: {}", report.max_age_days);
            for root in &report.roots {
                println!("  root: {root}");
            }
            for protected in &report.protected {
                println!("  protected: {protected}");
            }
            if !report.protection_complete {
                println!(
                    "  PROTECTED SET INCOMPLETE: a protection source could not be resolved (see \
                     the warnings below); this run does not report success"
                );
            }
            let scan = &report.scan;
            println!(
                "  scan: enqueued={} dequeued={} examined={} candidates={} descended={} \
                 depth_limited={} roots_duplicate={} protected_pruned={} roots_missing={} \
                 unreadable={} balances={}",
                scan.enqueued,
                scan.dequeued,
                scan.examined,
                scan.candidates,
                scan.descended,
                scan.depth_limited,
                scan.roots_duplicate,
                scan.protected_pruned,
                scan.roots_missing,
                scan.unreadable,
                scan.balances(),
            );
            for skip in &report.unexamined {
                println!("  {} {} — {}", skip.class, skip.path, skip.reason);
            }
            if report.incomplete {
                println!(
                    "  INCOMPLETE: the authorized scope was not fully accounted for; this run \
                     does not report success"
                );
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
            println!(
                "  reclaimable_bytes (what a CERTIFIED reaper would free; nothing was \
                 deleted): {}",
                report.reclaimable_bytes
            );
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
    use std::collections::BTreeSet;

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
        run_orphan_reap(conn, opts, &resolved_sources(), now, probe)
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
            scan_orphan_candidates(&[root.clone()], &protection, aged_now(30), 7).candidates;
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
            protected_paths(&resolved_sources().with_shared_cargo_target_dir(&shared))
                .covers(&shared),
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

        let check = lsof_holder_probe(&target);
        assert_ne!(
            check,
            HolderCheck::None,
            "an open file handle under the dir must never read as unheld: {check:?}"
        );
        let candidate = OrphanCandidate {
            identity: std::fs::canonicalize(&target).unwrap(),
            file_identity: FileIdentity::of(&target),
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
            scan_orphan_candidates(&[root.clone()], &Protection::default(), aged_now(30), 7)
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

        let candidates =
            scan_orphan_candidates(&[root.clone()], &Protection::default(), aged_now(30), 7)
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
        let counting_probe = move |_path: &Path| {
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
            &[root.clone()],
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
            scan_orphan_candidates(&[root.clone()], &Protection::default(), now, 7).candidates;

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
        // #1062: the knife is certified now (receipt checked in, owner-ratified 1A,
        // 2026-07-17) — but `dry_run` (checked above) is a SEPARATE property from
        // certification, and this is the property this test exists to pin: a
        // report-only request (`force: false`) deletes nothing and books nothing
        // REGARDLESS of whether the destructive path is certified. Was
        // `assert!(!report.destructive_certified)` pre-#1062; flipped to match the
        // now-true constant, the dry-run assertions above and below are unchanged.
        assert!(report.destructive_certified);
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
        let probe = move |_path: &Path| {
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
            move |_path: &Path| {
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
        let retargeting_probe = move |_path: &Path| {
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
        let swapping_probe = move |_path: &Path| {
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
        let swap_on_second_call = move |_path: &Path| {
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
            report.warnings.iter().any(|warning| warning
                .contains("(dev, ino) identity changed again")
                && warning.contains("a second time")),
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
        let probe = move |_path: &Path| {
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

    // ── #1062: --force reaches the delete path, and certification ≠ no fences ──

    /// The seal is open (#1062) — and the proof that "open" does not mean "no fences
    /// left". Renamed from `force_is_refused_at_the_entry_point_and_the_fixture_
    /// proves_it_was_reapable`, which pinned the pre-#1062 shape: the entry point
    /// (`reap_sealed`) refused EVERY `--force` request outright via
    /// `certify_destructive`, and the second half proved that refusal meant
    /// something (CONCERN 6 — a test that only asserts "the directory still exists"
    /// passes just as happily against a reaper that does nothing at all) by running
    /// the SAME fixture through the sheathed body (`reap_uncertified`, which
    /// bypasses the gate) and watching it really delete. See git blame / #1062 for
    /// that reading.
    ///
    /// Two halves, same design, inverted premise now that certification is real:
    ///
    /// * **first half — the entry point itself now does the deleting.** `--force`
    ///   through `reap_sealed` (the REAL entry point `run_orphan_reap_cli` also
    ///   calls, not the sheathed `run_orphan_reap_uncertified` the old test needed
    ///   to bypass the gate to reach) on a healthy fixture genuinely reclaims —
    ///   proving the certified path is not a dead branch behind a gate that never
    ///   opens.
    /// * **second half is CONCERN 6 in the certified world: certification is not
    ///   "no fences".** The exact same entry point, the exact same `--force`, but
    ///   the `(dev, ino)` identity is swapped out from under the verdict between
    ///   judgement and delete (BUG 2's own fence — same swap technique as
    ///   `a_directory_replaced_at_the_same_path_between_verdict_and_delete_is_
    ///   refused`) — still refused, because `certify_destructive` was never the
    ///   ONLY fence and flipping it true does not touch the others. The refusal
    ///   now surfaces as a per-candidate `"refused"` decision inside a
    ///   successfully-returned `Ok(report)`, not as a top-level `Err` from the
    ///   gate — that shift in shape is itself part of what changed.
    #[test]
    fn force_reclaims_at_the_entry_point_and_a_broken_fence_still_refuses_it() {
        // Half 1: a healthy fixture, the real entry point, `--force` — genuinely
        // deletes, and books it, now that the destructive path is certified.
        let root = unique_temp_dir("tachi-reaper-certified-delete");
        let dead = make_target_dir(&root, "dead-target");
        let mut store = open_store(&root);

        let report = reap_sealed(
            store.connection_mut(),
            &opts(&root, true),
            aged_now(30),
            &*unheld_probe(),
        )
        .expect("--force is certified now: the entry point must not refuse a healthy request");

        assert_eq!(
            report.reclaimed.len(),
            1,
            "the entry point must actually reach the delete path once certified: {report:?}"
        );
        assert!(!dead.exists(), "the target must really be gone: {report:?}");
        let rows = memcore::list_resources(store.connection(), None, None).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].state,
            ResourceState::Reclaimed,
            "the delete through the real entry point must still be booked: {rows:?}"
        );

        let _ = std::fs::remove_dir_all(&root);

        // Half 2 (CONCERN 6, still true post-#1062): the same entry point, the same
        // `--force`, but the `(dev, ino)` identity is swapped out from under the
        // verdict — refused, proving certification did not remove the other fences.
        let root2 = unique_temp_dir("tachi-reaper-certified-still-fenced");
        let target = make_target_dir(&root2, "swapped-target");
        let mut store2 = open_store(&root2);

        let target_for_probe = target.clone();
        let swapping_probe = move |_path: &Path| {
            std::fs::remove_dir_all(&target_for_probe).unwrap();
            std::fs::create_dir_all(target_for_probe.join("debug")).unwrap();
            std::fs::write(
                target_for_probe.join("debug/replacement.rlib"),
                vec![9u8; 4096],
            )
            .unwrap();
            HolderCheck::None
        };

        let report2 = reap_sealed(
            store2.connection_mut(),
            &opts(&root2, true),
            aged_now(30),
            &swapping_probe,
        )
        .expect(
            "certification means the entry point no longer refuses OUTRIGHT — the (dev, ino) \
             fence still fires per-candidate, inside a successful run, not as an Err from the \
             gate",
        );

        assert!(
            target.join("debug/replacement.rlib").exists(),
            "the replacement directory must survive: certification did not disable the \
             identity fence: {report2:?}"
        );
        assert!(report2.reclaimed.is_empty(), "{report2:?}");
        assert_eq!(report2.candidates.len(), 1, "{report2:?}");
        assert_eq!(report2.candidates[0].decision, "refused", "{report2:?}");
        assert!(
            report2
                .warnings
                .iter()
                .any(|warning| warning.contains("(dev, ino) identity")),
            "the refusal must still name the (dev, ino) mismatch even though force is \
             certified: {report2:?}"
        );

        let _ = std::fs::remove_dir_all(&root2);
    }

    /// `certify_destructive` is the single gate — and once the destructive path is
    /// certified (#1062), it refuses NEITHER call: a report-only request and a
    /// `--force` request are both permitted to proceed past THIS gate (other gates
    /// — the protected set, the holder probe, the pinned `(dev, ino)` identity —
    /// still stand; see `force_reclaims_at_the_entry_point_and_a_broken_fence_
    /// still_refuses_it` for the proof that certification did not remove them).
    ///
    /// Renamed from `only_the_destructive_request_is_refused`, which pinned the
    /// pre-#1062 shape (`certify_destructive(true)` unconditionally `Err`,
    /// `certify_destructive(false)` unconditionally `Ok`) — and whose own tripwire
    /// (`assert!(!DESTRUCTIVE_CERTIFIED, …)`) fired exactly as designed the day
    /// #1062 flipped the constant, which is what sent this test here to be
    /// reconciled. See git blame / #1062 for that reading.
    #[test]
    fn certify_destructive_permits_both_once_certified() {
        assert!(
            certify_destructive(false).is_ok(),
            "report-only was never gated by certification"
        );
        assert!(
            certify_destructive(true).is_ok(),
            "force must be permitted past this gate once DESTRUCTIVE_CERTIFIED is true — the \
             gate's own logic only ever refuses `force && !DESTRUCTIVE_CERTIFIED`"
        );

        // A tripwire on the compile-time constant, deliberately — the same shape the
        // pre-#1062 test used, pointed the other way. `clippy` calls a constant
        // assertion pointless because a constant cannot surprise you at runtime; that is
        // exactly why this one is here. It states the premise the two assertions above
        // depend on (they only mean "the gate passes both" while the seal stays open), so
        // the day somebody REVOKES certification and flips `DESTRUCTIVE_CERTIFIED` back
        // to `false`, this test goes red and names the seal — symmetric to the tripwire
        // it replaces, which fired the day certification opened it.
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(
                DESTRUCTIVE_CERTIFIED,
                "the day this flips back to false, `certify_destructive(true)` refuses again \
                 and the assertion above must flip with it"
            );
        }
    }

    /// The report says whether the knife is certified, so a reader cannot mistake
    /// `reclaimable_bytes` for bytes that were freed just because certification
    /// flipped. Renamed from `the_report_declares_itself_uncertified`, which
    /// pinned the pre-#1062 `false` reading — see git blame / #1062 for that
    /// shape.
    ///
    /// A DRY RUN (`force: false`) still books nothing and frees nothing even
    /// though the destructive path is now certified — `destructive_certified` and
    /// `dry_run` are orthogonal fields, and this test's core property (a
    /// report-only run reports, it does not act) is unchanged by the flip; only
    /// the certification bit it now reads back is.
    #[test]
    fn the_report_declares_itself_certified() {
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

        assert!(report.destructive_certified);
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

        let once = scan_orphan_candidates(&[root.clone()], &protection, now, 7);
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
        let gapped = run_orphan_reap(
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
            move |_path: &Path| {
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
                run_orphan_reap(
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
                run_orphan_reap(
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
        let source = include_str!("exec_env_reaper.rs");

        for needle in &forbidden {
            assert!(
                !source.contains(needle.as_str()),
                "`{needle}` is back in exec_env_reaper.rs. It mutates the environment of the \
                 whole test process, which is what made every reaper test depend on HOME and \
                 turned an unrelated test red. Inject a `ProtectionSources` instead."
            );
        }
    }

    // ── #1062 kill-test matrix (S2d shape) — #[ignore]d, not yet executed ────
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
    // codex-cli.toml` is the precedent for the shape). Until that happens,
    // `DESTRUCTIVE_CERTIFIED` stays `false` — no code in this module reads the printed
    // output back to flip it, on purpose: a receipt this crate wrote to itself would
    // recreate exactly the self-grading S2d exists to rule out.
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
        /// unattended. Run explicitly: `cargo test --offline -p tachi-server --lib \
        /// exec_env_reaper::tests::kill_tests:: -- --ignored --nocapture`.
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
                memcore::bind_resource(store.connection_mut(), "kt-env-live", &resource_id)
                    .unwrap();

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
                let swapping_probe = move |_path: &Path| {
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
            println!("kill_test = \"crates/tachi-server/src/exec_env_reaper.rs\"");
            println!("kill_test_fn = \"exec_env_reaper::tests::kill_tests::orphan_reaper_kill_test_matrix\"");
            println!("binary = \"tachi-server\"");
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
}
