//! Orphan build-artifact reaper (#894 S2b) — the bytes nobody is holding.
//!
//! # REPORT-ONLY. The destructive path is RE-SHEATHED and is REFUSED.
//!
//! An adversarial review of the delete path (codex, audit `codex-g6f99`) returned
//! **NOT-SAFE** against #1062; every defect it found was closed, pinned by this
//! module's own standing test suite, kill-tested, and receipted on 2026-07-17 —
//! [`DESTRUCTIVE_CERTIFIED`] flipped to `true` and `--force` reached the delete path
//! for six days. On 2026-07-23, `tachi#1379` showed the one fence that receipt never
//! exercised — the pinned `(dev, ino)` identity ([`FileIdentity`]) — can itself be
//! defeated on ext4 by deleting and recreating a directory at the same path fast
//! enough to land on the SAME reused inode, a scenario the 2026-07-17 matrix never
//! staged. This module's own doctrine (below, and the const's own doc) is that
//! touching the reap path invalidates a certification "morally if not mechanically";
//! a hole in the exact fence the receipt certified is exactly that, so the knife is
//! sheathed again: [`run_orphan_reap`] **refuses `--force`** and returns a typed
//! [`DestructiveRefusal`]; the CLI prints that refusal and exits non-zero. Nothing is
//! deleted, by any caller, on any path, until `tachi#1379`'s handle-pinning fix lands
//! and an inode-reuse scenario is added to the kill-test matrix and re-run.
//!
//! This is the doctrine S2d applies to everything else in #894 — *a capability is
//! fail-closed until a kill-test certifies it* — and it binds us too, most of all when
//! the capability deletes 61 GB and the reviewer says it is wrong.
//!
//! ## The blocking defect (why the knife stays sheathed) — [`BLOCKING_DEFECTS`]
//!
//! * **The 2026-07-17 kill-test receipt never staged inode reuse, and `tachi#1379`
//!   showed that gap is exploitable (2026-07-23).** BUGs 1, 2 and 4 below are closed in
//!   source and pinned by this module's own standing test suite, and S2d's own doctrine
//!   (*a capability is fail-closed until a kill-test certifies it*) — which does not
//!   accept "the unit tests pass" as that certification — WAS satisfied: an executed
//!   kill-test matrix, checked in as a receipt naming the binary version, the OS, the
//!   matrix, and the git blob hash of the test source that ran (the same shape
//!   `tachi-dispatch`'s codex sandbox certification uses,
//!   `crates/tachi-dispatch/src/certification.rs`, #894 S2d), flipped
//!   [`DESTRUCTIVE_CERTIFIED`] to `true` on 2026-07-17. That receipt's matrix
//!   (`tests::kill_tests`) never staged an ext4 inode-reuse race against the `(dev, ino)`
//!   fence BUG 2 added — and `tachi#1379`'s 2026-07-23 reproduction on real hardware
//!   showed a delete-and-recreate-at-the-same-path race can land on the SAME reused
//!   `(dev, ino)` pair the fence trusts, walking straight through it. The matrix needs
//!   that scenario added and re-run before this module may claim BUG 2 is closed again.
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
//! Re-enabling the destructive path now means two things, not one: landing
//! `tachi#1379`'s handle-pinning fix for the `(dev, ino)` fence, and adding an
//! inode-reuse scenario to the kill-test matrix and re-running it for real, checked in
//! as a fresh receipt. Until both land, everything below is still a **report** —
//! [`DESTRUCTIVE_CERTIFIED`] does not flip on a green `cargo test` alone, on purpose
//! (see the const's own doc), and the 2026-07-17 receipt does not carry over: this
//! module's own doctrine is that touching the reap path invalidates a certification
//! "morally if not mechanically," and a proven hole in the exact fence it certified is
//! exactly that.
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

/// Has the destructive path been certified safe to run? **No — not right now.** #1062
/// closed BUGs 1, 2 and 4 (the module docs, and [`BLOCKING_DEFECTS`], have the detail),
/// each pinned by a discriminating test in this module's own standing suite, and on
/// 2026-07-17 the kill-test matrix (`tests::kill_tests`) was EXECUTED for real (Oz seat,
/// all four #1062 scenarios pass) with a checked-in receipt at
/// `crates/tachi-server/certifications/orphan-reaper.toml` — the shape
/// `crates/tachi-dispatch/src/certification.rs` already ships for codex's sandbox. That
/// flipped this const to `true`, and `--force` reached the delete path for six days.
///
/// **On 2026-07-23, `tachi#1379` showed the 2026-07-17 matrix never exercised the one
/// scenario that mattered.** BUG 2's fence pins a real `(dev, ino)` identity
/// ([`FileIdentity`]) at judgement and re-`stat`s it immediately before delete — proof
/// against a symlink retarget or a plain `rm -rf && mkdir` at the judged path. It is not
/// proof against ext4 handing the SAME inode back to a directory recreated fast enough
/// at the same path: `tachi#1379` reproduced exactly that on real hardware, and the
/// fence cannot tell the reused inode from the one it judged. This module's own rule —
/// touching the reap path invalidates a certification "morally if not mechanically:
/// re-run and re-receipt" — applies to a hole discovered IN the certified fence exactly
/// as much as it applies to a code change, so the 2026-07-17 receipt no longer
/// certifies anything and this const goes back to `false` until `tachi#1379`'s
/// handle-pinning fix lands and an inode-reuse scenario is added to the kill-test
/// matrix and re-run.
///
/// A `const` rather than a config flag, on purpose. A flag is something an operator can
/// flip at 2 a.m. under disk pressure; the gate between a scan of `~/.cache` and
/// `remove_dir_all` should cost a code change, a review, and a test suite.
pub(crate) const DESTRUCTIVE_CERTIFIED: bool = false;

/// The finding that sheathed it THIS time. `codex-g6f99` (#1062) is the audit that
/// sheathed it originally, closed and receipted 2026-07-17; `tachi#1379` is why it is
/// sheathed again.
pub(crate) const BLOCKING_AUDIT: &str = "tachi#1379";

/// Why the delete path may not run — verbatim in the refusal, in the report, and on the
/// CLI's stderr. An operator who types `--force` is told exactly what is broken, not
/// merely that they were denied.
///
/// #1062's BUGs 1, 2 and 4 are still closed in source (each pinned by a discriminating
/// test — see the module docs for the detail on each); they are not what blocks
/// `--force` today. The one line below is `tachi#1379`: the 2026-07-17 receipt
/// certified BUG 2's `(dev, ino)` fence against a matrix that never staged an
/// inode-reuse race, and #1379 proved that race beats the fence on real hardware.
pub(crate) const BLOCKING_DEFECTS: &[&str] = &[
    "the (dev, ino) fence (BUG 2, #1062) is beatable by inode reuse (tachi#1379): on ext4, \
     a directory deleted and recreated at the judged path fast enough can land on the SAME \
     (dev, ino) pair the fence trusts, so the re-stat immediately before delete cannot tell \
     the reused inode from the one judged — reproduced on real hardware 2026-07-23. The \
     2026-07-17 kill-test receipt (crates/tachi-server/certifications/orphan-reaper.toml) \
     never exercised this scenario and does not certify against it. Re-enable requires \
     tachi#1379's handle-pinning fix plus an inode-reuse scenario added to the kill-test \
     matrix and re-run for a fresh receipt",
];

/// The typed refusal a destructive request gets. Not a silent skip, and not an empty
/// report that reads like a clean run — an `Err` the caller must handle, carrying the
/// reason back to whoever asked.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DestructiveRefusal {
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
/// the CLI. It said yes unconditionally for six days (#1062, 2026-07-17 through
/// 2026-07-23) while `DESTRUCTIVE_CERTIFIED` was `true`; `tachi#1379` REVOKED that
/// certification and this const is `false` again, so `force` is refused once more. It
/// stayed here rather than being deleted for exactly this reason: the day certification
/// is revoked, this is the one place that must go back to refusing `force`, and every
/// caller already asks it instead of reading the constant directly.
pub fn certify_destructive(force: bool) -> Result<(), DestructiveRefusal> {
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
pub enum HolderCheck {
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

/// Injectable holder probe — the real one shells out to `lsof`; this private
/// seam lets crate-local tests exercise the decision logic without depending on
/// the host's process table.
type HolderProbe = dyn Fn(&Path, Option<HolderExclusion>) -> HolderCheck;

/// The one holder the reaper itself creates while pinning a candidate's inode.
/// Both fields must match before an `lsof` row is ignored; excluding the whole
/// process would hide unrelated descriptors and weaken the holder fence.
#[derive(Debug, Clone, Copy)]
pub struct HolderExclusion {
    pid: u32,
    fd: i32,
}

/// Real probe: `lsof +D <dir>` (recursive — a live `cargo` holds files deep
/// inside the target, not just at its root).
pub fn lsof_holder_probe(path: &Path, ignored_holder: Option<HolderExclusion>) -> HolderCheck {
    match Command::new("lsof").arg("+D").arg(path).output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let filtered = ignored_holder
                .map(|ignored| without_ignored_holder(&stdout, ignored))
                .unwrap_or_else(|| stdout.into_owned());
            interpret_lsof(
                out.status.code(),
                &filtered,
                &String::from_utf8_lossy(&out.stderr),
            )
        }
        // No lsof on this host ⇒ we cannot prove "unheld" ⇒ nothing is
        // reclaimed. Loud, not silent.
        Err(err) => HolderCheck::Unknown(format!("cannot run lsof: {err}")),
    }
}

fn without_ignored_holder(stdout: &str, ignored: HolderExclusion) -> String {
    stdout
        .lines()
        .filter(|line| {
            let mut fields = line.split_whitespace();
            let _command = fields.next();
            let pid = fields.next().and_then(|value| value.parse::<u32>().ok());
            let _user = fields.next();
            let fd = fields.next().and_then(|value| {
                let digits = value
                    .as_bytes()
                    .iter()
                    .take_while(|byte| byte.is_ascii_digit())
                    .count();
                value[..digits].parse::<i32>().ok()
            });
            pid != Some(ignored.pid) || fd != Some(ignored.fd)
        })
        .collect::<Vec<_>>()
        .join("\n")
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
pub struct Protection {
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
/// again. So the global is *gone* from the reaper body instead. The public production
/// entry reads it exactly once, after the destructive gate
/// ([`Self::from_process_env`]), and from there the protected set is computed from a
/// value that was handed to the private body. A test hands that body a value with
/// `home: None` and gets fail-closed behaviour in its own thread, affecting nobody.
///
/// ## What is a snapshot and what is live
///
/// The three environment variables are a **snapshot**, and that is not a weakening: they
/// are *this* process's environment, and no other process can reach in and change them.
/// A build that starts after the scan cannot appear in our `HOME`; it appears in the
/// **process table**, which is why that source stays a live callable (the `live_builds`
/// field) and is re-run at the delete (see [`run_orphan_reap_uncertified`]).
pub struct ProtectionSources<'a> {
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
    /// **The only constructor that reads the process environment.** The public reaper
    /// calls it only after its destructive gate; the report-only doctor patrol also uses
    /// it to build the same protected set.
    pub fn from_process_env() -> Self {
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
pub fn protected_paths(sources: &ProtectionSources<'_>) -> Protection {
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
struct FileIdentity {
    dev: u64,
    ino: u64,
}

impl FileIdentity {
    #[cfg(unix)]
    fn from_metadata(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    /// `symlink_metadata`, not `metadata`: the identity is of the entry AT this
    /// path, not of whatever a symlink there might point through. A candidate is
    /// only ever a directory (the scan uses `symlink_metadata` to enqueue it and
    /// never follows a symlink into a subtree), so an identity captured over a
    /// symlink here would already be a lie about what was judged.
    #[cfg(unix)]
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(Self::from_metadata(&meta))
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

/// An open handle to the directory judged by the scan. Holding the handle keeps
/// its inode allocated until the candidate has either been refused or deleted,
/// so a remove-and-recreate race cannot make a replacement look identical by
/// receiving the judged directory's just-freed inode number.
#[derive(Debug)]
struct PinnedDirectory {
    handle: std::fs::File,
    identity: FileIdentity,
}

impl PinnedDirectory {
    #[cfg(unix)]
    fn open(path: &Path) -> Option<Self> {
        let handle = std::fs::File::open(path).ok()?;
        let identity = FileIdentity::from_metadata(&handle.metadata().ok()?);
        let path_meta = std::fs::symlink_metadata(path).ok()?;
        if !path_meta.is_dir() || FileIdentity::from_metadata(&path_meta) != identity {
            return None;
        }
        Some(Self { handle, identity })
    }

    #[cfg(unix)]
    fn current_identity(&self) -> Option<FileIdentity> {
        self.handle
            .metadata()
            .ok()
            .map(|meta| FileIdentity::from_metadata(&meta))
    }

    #[cfg(unix)]
    fn holder_exclusion(&self) -> Option<HolderExclusion> {
        use std::os::fd::AsRawFd;
        Some(HolderExclusion {
            pid: std::process::id(),
            fd: self.handle.as_raw_fd(),
        })
    }

    #[cfg(not(unix))]
    fn open(_path: &Path) -> Option<Self> {
        None
    }

    #[cfg(not(unix))]
    fn current_identity(&self) -> Option<FileIdentity> {
        None
    }

    #[cfg(not(unix))]
    fn holder_exclusion(&self) -> Option<HolderExclusion> {
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
pub enum Staleness {
    /// Nothing anywhere under the tree has been touched since the cutoff, and the
    /// whole tree was readable. The only state that passes the staleness gate.
    Stale { age_days: u64 },
    /// Something under the tree is newer than the cutoff.
    Fresh { age_days: u64 },
    /// The walk was partial (unreadable metadata / directory) — fail-closed.
    Unprovable(String),
}

impl Staleness {
    pub fn age_days(&self) -> Option<u64> {
        match self {
            Staleness::Stale { age_days } | Staleness::Fresh { age_days } => Some(*age_days),
            Staleness::Unprovable(_) => None,
        }
    }
}

/// A directory that *looks like* a reclaimable build artifact. Being a
/// candidate says nothing about whether it may be deleted — that is
/// [`decide_reap`].
#[derive(Debug)]
pub struct OrphanCandidate {
    /// The path as the scan walked it — the caller's spelling, symlinked scan
    /// root and all.
    pub path: PathBuf,
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
    /// Open handle for the judged directory. Unlike `(dev, ino)` alone, this
    /// prevents the inode from being recycled onto a replacement while the
    /// delete decision is in flight.
    identity_pin: Option<PinnedDirectory>,
    pub kind: ResourceKind,
    pub staleness: Staleness,
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
pub struct ScanOutcome {
    pub candidates: Vec<OrphanCandidate>,
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
pub fn scan_orphan_candidates(
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
                    let identity_pin = PinnedDirectory::open(&dir);
                    out.candidates.push(OrphanCandidate {
                        staleness: staleness(&dir, now, cutoff),
                        // Pin the spelling the verdict is about to be rendered against —
                        // see `OrphanCandidate::identity`.
                        identity: canonicalize_expected(&dir).unwrap_or_else(|| dir.clone()),
                        // …and the REAL identity beside it (BUG 2, closed): captured
                        // now, at judgement, and re-checked immediately before the
                        // delete (`delete_resource_bytes`).
                        file_identity: identity_pin.as_ref().map(|pin| pin.identity),
                        identity_pin,
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
pub fn default_orphan_roots() -> Vec<PathBuf> {
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
pub struct ReapReport {
    pub(crate) action: &'static str,
    /// **`false` again as of `tachi#1379`** (2026-07-23; was `true` 2026-07-17 through
    /// 2026-07-23 under #1062's receipt — see [`DESTRUCTIVE_CERTIFIED`]). Mirrors
    /// [`DESTRUCTIVE_CERTIFIED`] into every report so a reader never has to go check the
    /// constant to know whether a `--force` request on this build can act — and, on a
    /// `dry_run` report, whether `reclaimable_bytes` is a preview of what a certified
    /// reaper would do or a record of what an UNcertified one is forbidden to. A plain
    /// mirror of the const rather than re-described documentation, so this field itself
    /// cannot go stale — only the const's OWN doc (which it does) needs updating when
    /// the value moves.
    pub(crate) destructive_certified: bool,
    /// The blocking defects, verbatim, in every report — currently `tachi#1379` (the
    /// finding that resheathed the knife); see [`BLOCKING_AUDIT`] / [`BLOCKING_DEFECTS`]
    /// for the detail and history.
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
pub struct ReapOptions {
    pub roots: Vec<PathBuf>,
    pub max_age_days: u64,
    /// `false` (the default) = preview: decide, report, touch nothing.
    ///
    /// `true` is **refused** — [`certify_destructive`] turns it into a
    /// [`DestructiveRefusal`] before anything is scanned. The field survives because the
    /// request still has to be *rejected*, loudly and with a reason, and because the
    /// sheathed machinery behind it is pinned by tests against the day it is certified.
    /// It is not a switch anyone can currently flip.
    pub force: bool,
}

// ── Run ─────────────────────────────────────────────────────────────────────

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
/// Protection sources and the holder fence are not injectable through this production
/// entry point. Only after the destructive gate succeeds does it snapshot the real
/// process environment with [`ProtectionSources::from_process_env`], then it enters the
/// private body with [`lsof_holder_probe`].
pub fn run_orphan_reap(
    conn: &mut rusqlite::Connection,
    opts: &ReapOptions,
    now: SystemTime,
) -> Result<ReapReport, DestructiveRefusal> {
    certify_destructive(opts.force)?;
    let sources = ProtectionSources::from_process_env();
    Ok(run_orphan_reap_uncertified(
        conn,
        opts,
        &sources,
        now,
        &lsof_holder_probe,
    ))
}

/// Crate-local test seam for the sealed entry point. Production callers cannot replace
/// either the real protection sources or the real holder fence.
#[cfg(test)]
fn run_orphan_reap_with_sources_and_probe(
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
                candidate.holders = Some(probe(
                    &candidate.path,
                    candidate
                        .identity_pin
                        .as_ref()
                        .and_then(PinnedDirectory::holder_exclusion),
                ));
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
pub fn reap_exit_status(report: &ReapReport) -> Result<(), String> {
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
                candidate.identity_pin.as_ref(),
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
    identity_pin: Option<&PinnedDirectory>,
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
    let handle_identity = identity_pin.and_then(PinnedDirectory::current_identity);
    let current_identity = FileIdentity::of(path);
    if pinned_identity.is_none()
        || handle_identity != pinned_identity
        || current_identity != pinned_identity
    {
        return Err(MemoryError::InvalidArg(format!(
            "identity unresolved: refusing to reclaim {}: its (dev, ino) identity does not match \
             the pinned directory handle or the one the verdict was rendered against (captured \
             {pinned_identity:?}, handle {handle_identity:?}, now {current_identity:?}) — the object \
             at this path was replaced between judgement and delete",
            resource.path
        )));
    }
    // Defense in depth against the scan→delete window: the row is already
    // `reclaiming`, but the bytes are still there. A process that grabbed the
    // directory since the scan aborts the delete (row → `reclaim_failed`,
    // retryable) rather than losing a live build's cache.
    match probe(
        path,
        identity_pin.and_then(PinnedDirectory::holder_exclusion),
    ) {
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
fn reclaimed_bytes_by_reason(conn: &rusqlite::Connection) -> Result<BTreeMap<String, i64>, String> {
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
/// Public because the `tachi doctor` build-resource patrol
/// (`doctor::build_resources::scan_orphan_build_resources`) reuses this exact
/// scan primitive (tachi#1184 item 2) to size its own — narrower,
/// blessed-list-filtered — candidate set,
/// rather than growing a second copy of the same metadata-only recursion.
pub fn dir_size(path: &Path) -> u64 {
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
pub fn emit_destructive_refusal(
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

pub fn emit_reap_report(report: &ReapReport, output: OutputFormat) -> Result<(), String> {
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
mod tests;
