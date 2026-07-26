//! Parent-owned workspace manifest: the pre-image and post-image this gate
//! compares (#894 S2e).
//!
//! `git diff` is NOT sufficient and is deliberately not used here. It misses
//! ignored files, git metadata, xattrs, and a mutate-then-restore (which leaves
//! the tree byte-identical but the inode/ctime changed). This module therefore
//! walks the lease workspace itself and fingerprints EVERY entry it finds —
//! tracked, untracked, ignored, git metadata (including a linked worktree's
//! external `gitdir`), symlinks (never followed), and extended attributes.
//!
//! ## What a fingerprint covers, and why each field is load-bearing
//!
//! | field | catches |
//! |---|---|
//! | `kind` | file ⇄ dir ⇄ symlink swaps |
//! | `content_hash` (BLAKE2s-256) | any content edit; cryptographic so a worker cannot forge a colliding preimage |
//! | `link_target` | a symlink retargeted at an outside-root path |
//! | `xattr_hash` | `setxattr`/`removexattr` on an otherwise untouched file |
//! | `size`, `mode`, `nlink` | truncation, chmod, hardlink games |
//! | `ino` | rewrite-via-rename (new inode, same bytes) |
//! | `mtime` | ordinary writes |
//! | `ctime` | **mutate-then-restore**: content and mtime can both be restored by an unprivileged worker (`write` + `utimes`), but POSIX exposes no call that *sets* `ctime` — `utimensat` bumps it — so restoring the bytes still leaves a ctime bump |
//!
//! `atime` is deliberately NOT recorded: reads legitimately bump it (and
//! `relatime`/`noatime` mounts make it non-deterministic), so it is noise, not
//! signal.
//!
//! ## Why a timestamp is only evidence once the clock has been shown to resolve it
//!
//! Two of those rows — the same-size overwrite of a file under an unhashed root,
//! and mutate-then-restore — have **no witness except a timestamp**. Every other
//! fingerprint field (content hash, size, mode, inode, nlink, xattrs) is equal by
//! construction in those two classes, so "no field moved" is decided entirely by
//! whether the inode clock ticked between the capture and the worker's write.
//!
//! Filesystem timestamps are stamped from a *coarse* kernel clock (on Linux, one
//! jiffy — 1–4 ms — regardless of the nanosecond fields ext4/overlayfs can
//! store). Two events inside one tick get byte-identical `{sec, nsec}`. When that
//! happens the comparison finds nothing, and a gate that reads "no delta" as
//! "proven unchanged" certifies a mutated workspace as clean. That is not a flaky
//! test, it is a fail-open (#1440).
//!
//! The fix is not a more tolerant comparison, it is a **capture-time clock
//! barrier**: before a pre-image is sealed, the parent spins on a probe file — on
//! the same filesystem, outside every walk root — until it *observes* a
//! filesystem stamp strictly greater than every `ctime` in the image (see
//! [`WorkspaceManifest::seal_with_clock_barrier`]). Any inode change after that
//! point is stamped by a monotone clock at or past the barrier, hence strictly
//! past the recorded `ctime`, and the existing comparison becomes sound. `ctime`
//! is the barrier's target because POSIX bumps it on *every* inode change and
//! provides no call that sets it (the closest, `utimensat`, bumps it too).
//!
//! If the barrier cannot be observed, capture **fails** — it never degrades to a
//! best-effort sleep, and a pre-image that carries no dominating barrier is
//! refused at gate time ([`WorkspaceManifest::verify_clock_barriers`]) rather
//! than passed. "I could not resolve this window" is not "nothing happened".
//!
//! ### The barrier has exactly two preconditions, and both are REFUSALS
//!
//! A barrier is an argument of the form "I observed this filesystem's clock pass
//! X, therefore anything it stamps later is > X". That argument needs the clock
//! it measured to be (a) the same clock that will stamp the entries, and (b) a
//! clock that actually advances on an inode change. Neither is universal, so
//! both are checked and both fail closed:
//!
//! * **A ctime witness must exist on this platform.** `ctime` is a POSIX
//!   concept. On a platform where `std` exposes no change-time equivalent, there
//!   is nothing for the barrier to observe *and* nothing for `compare_entry` to
//!   convict with — substituting creation time (which does not move when a
//!   file's contents change) would let the barrier "observe" an advance that can
//!   never happen, i.e. manufacture a proof, which is strictly worse than the
//!   bug this module exists to fix. So [`CtimeWitness`] is recorded in the image
//!   and anything but [`CtimeWitness::PosixCtime`] is refused, at seal time and
//!   again at gate time.
//! * **One walk root must be one filesystem.** The barrier is measured with a
//!   probe beside the root, on the root's device. An entry *under* that root but
//!   on a different device (a mount inside the workspace) is stamped by a
//!   different clock, which the probe never observed. The walk records any such
//!   entry in [`WorkspaceManifest::foreign_device_paths`] and the image is
//!   refused. A per-device barrier is not merely unimplemented, it is not
//!   constructible: a probe must live *outside* every walk root, and a
//!   filesystem mounted *inside* the walk root has no location outside it.
//!
//! ## The walk roots are PINNED by the parent, never re-derived from the tree
//!
//! One of the two walk roots — a linked worktree's external `gitdir` — is named
//! by a **file inside the workspace** (`.git`), i.e. by a file the worker can
//! rewrite. Re-reading that file after the worker has run would let the worker
//! choose the parent's walk root (point it at `/` for a denial-of-service hash,
//! or at a decoy so the real `gitdir` is never re-scanned). So
//! [`resolve_external_git_dir`] is called **once, by the parent, before the
//! worker is spawned**, and the resolved path is pinned into the pre-image
//! ([`WorkspaceManifest::gitdir_root`]). [`capture`] never resolves anything: it
//! walks exactly the roots it is handed in the [`CaptureSpec`].
//!
//! ## Known limitation (stated, not hidden)
//!
//! A linked worktree's *commondir* (the main repo's shared `objects/`, `refs/`)
//! lives outside the lease and is NOT covered here. A worker with a shell can
//! write there. That is the shared-repo surface, and it is exactly why this
//! posture is `detect-and-reject` for the lease and never a claim of prevention
//! for the host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use blake2::{Blake2s256, Digest};
use serde::{Deserialize, Serialize};

/// Manifest key namespace for the lease workspace itself.
pub const WORKSPACE_ROOT_LABEL: &str = "workspace";
/// Manifest key namespace for a linked worktree's external git metadata dir
/// (the `gitdir:` target of a `.git` *file*). An in-tree `.git` *directory* is
/// covered by the workspace walk and needs no second root.
pub const GITDIR_ROOT_LABEL: &str = "gitdir";

/// Build-output directory names a caller may pass as
/// [`CaptureSpec::unhashed_dir_names`]. Their contents are still walked and
/// fingerprinted — only the **content hash** is skipped, which is the part that
/// costs multiple GB of BLAKE2 on a Rust tree with an in-tree `target/`.
pub const BUILD_ARTIFACT_DIR_NAMES: &[&str] = &["target"];

/// How long [`establish_clock_barrier`] will wait for the filesystem clock to
/// be observed advancing. Generous next to any real tick (Linux jiffies are
/// 1–4 ms); reaching it means the clock is not behaving, which is a refusal, not
/// a longer sleep.
const CLOCK_BARRIER_TIMEOUT: Duration = Duration::from_secs(5);
/// Pause between probe writes so the spin does not burn a core on a coarse
/// filesystem.
const CLOCK_BARRIER_POLL: Duration = Duration::from_micros(200);

/// One inode timestamp, `{sec, nsec}`, compared as a whole (derived `Ord` is
/// field order, i.e. seconds then nanoseconds — which is the chronological
/// order, not a lexicographic one).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default, Hash,
)]
pub struct FsTime {
    pub sec: i64,
    pub nsec: i64,
}

impl FsTime {
    pub fn new(sec: i64, nsec: i64) -> FsTime {
        FsTime { sec, nsec }
    }
}

impl std::fmt::Display for FsTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{:09}", self.sec, self.nsec)
    }
}

/// What the `ctime_*` fields of an image actually hold.
///
/// Recorded in the image rather than inferred at read time, so the refusal below
/// is a property of the **data** and can be exercised by a test on any platform
/// — the same reason [`WorkspaceManifest::clock_barriers`] is re-verified at
/// gate time instead of trusted.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum CtimeWitness {
    /// POSIX inode change time: bumped by the kernel on *every* inode change,
    /// with no call that sets it directly. The only value this gate accepts.
    PosixCtime,
    /// This platform exposes no change-time equivalent through `std`, so the
    /// `ctime_*` fields hold a constant placeholder that cannot testify to
    /// anything.
    ///
    /// `#[default]` on purpose: an image that does not say which witness it used
    /// (an older binary, a truncated write) reads as the pessimistic value and
    /// is refused, rather than defaulting into the guarantee it never had.
    #[default]
    None,
}

impl CtimeWitness {
    pub fn as_str(self) -> &'static str {
        match self {
            CtimeWitness::PosixCtime => "posix_ctime",
            CtimeWitness::None => "none",
        }
    }
}

/// The ctime witness available on the platform this binary was built for.
#[cfg(unix)]
pub fn ctime_witness_kind() -> CtimeWitness {
    CtimeWitness::PosixCtime
}

/// No change-time equivalent is reachable through `std` here.
///
/// `Metadata::created()` is deliberately NOT offered as a substitute: creation
/// time does not advance when a file's contents change, so a barrier "proven"
/// against it would be proven against a value that can never move — a
/// manufactured proof, and a worse failure than the missing barrier this module
/// was written to add.
#[cfg(not(unix))]
pub fn ctime_witness_kind() -> CtimeWitness {
    CtimeWitness::None
}

/// Filesystem entry class as seen through `symlink_metadata` (symlinks are
/// never followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::File => "file",
            EntryKind::Dir => "dir",
            EntryKind::Symlink => "symlink",
            EntryKind::Other => "other",
        }
    }
}

/// One entry's full fingerprint. Every field is a detection surface (see the
/// module docs); dropping one silently blinds the gate to that mutation class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryFingerprint {
    pub kind: EntryKind,
    pub size: u64,
    pub mode: u32,
    pub ino: u64,
    pub nlink: u64,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
    /// Inode change time — the field that catches mutate-then-restore, because
    /// POSIX has no call that sets it (`utimensat`, which restores mtime, bumps
    /// ctime).
    ///
    /// Meaningful ONLY when the image records
    /// [`CtimeWitness::PosixCtime`], and it only *proves* anything once the
    /// capture is sealed with a clock barrier that strictly exceeds it
    /// ([`WorkspaceManifest::clock_barriers`]); without that, a write inside the
    /// capture's own clock tick carries the same stamp and is invisible.
    pub ctime_sec: i64,
    pub ctime_nsec: i64,
    /// `None` for a non-file, and also for a file under an unhashed root (see
    /// [`WorkspaceManifest::unhashed_roots`]) — there, detection falls back to
    /// size/inode/mtime/**ctime**, which the capture-time clock barrier makes
    /// resolvable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xattr_hash: Option<String>,
}

/// A complete parent-side image of one lease workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceManifest {
    pub workspace_root: String,
    /// The external `gitdir` walk root **as pinned by the parent before the
    /// worker was spawned**. The post-image is captured against this value, not
    /// against a re-read of the worker-writable `.git` file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gitdir_root: Option<String>,
    pub captured_at: String,
    /// Keyed by `"<root-label>/<relative-path>"`, sorted (BTreeMap) so two
    /// captures of an unchanged tree serialize identically.
    pub entries: BTreeMap<String, EntryFingerprint>,
    /// Anything the walk could not read. A non-empty list means the image is
    /// INCOMPLETE — the gate must then fail closed, because an unreadable entry
    /// is precisely where a change could hide.
    #[serde(default)]
    pub errors: Vec<String>,
    /// Manifest keys of directories whose subtrees were walked and fingerprinted
    /// but **not content-hashed** (opt-in, see [`CaptureSpec`]). Recorded — never
    /// silent — because a receipt must be able to say exactly which paths carry a
    /// weaker proof than the rest of the image.
    #[serde(default)]
    pub unhashed_roots: Vec<String>,
    /// **Capture-time clock barrier**, one per walk root label (`workspace`,
    /// `gitdir`): a filesystem timestamp the parent *observed on that root's
    /// filesystem* after the walk, strictly greater than every `ctime` the walk
    /// recorded for that root.
    ///
    /// This is what turns the timestamp fields from an observation into a proof.
    /// Set only on a **pre-image** (see
    /// [`WorkspaceManifest::seal_with_clock_barrier`]); a post-image never needs
    /// one, and never gets one.
    ///
    /// `#[serde(default)]` yields an EMPTY map, which is the pessimistic value:
    /// a pre-image written by an older binary — or one whose barrier drifted —
    /// carries no barrier, and [`WorkspaceManifest::verify_clock_barriers`]
    /// refuses it. A version skew therefore over-rejects (visible) instead of
    /// silently passing (the #1440 fail-open through a second door).
    #[serde(default)]
    pub clock_barriers: BTreeMap<String, FsTime>,
    /// What the `ctime_*` fields in this image actually are (see
    /// [`CtimeWitness`]). `#[serde(default)]` is [`CtimeWitness::None`], the
    /// pessimistic value: an image that does not say is refused.
    #[serde(default)]
    pub ctime_witness: CtimeWitness,
    /// Manifest keys the walk found on a **different filesystem than their walk
    /// root** — plus, on a platform where device ids cannot be read at all, one
    /// entry naming the root itself.
    ///
    /// A barrier is measured with a probe on the root's device. It says nothing
    /// about a second filesystem's clock, so an image with any entry here cannot
    /// use its timestamps as evidence and is refused at seal time and at gate
    /// time. Empty in the ordinary case (one workspace, one filesystem).
    ///
    /// Note for whoever hits this in the field: a pre-4.17 / `xino=off`
    /// overlayfs can report the *underlying* layer's `st_dev` for a file that
    /// has not been copied up, which would land entries here on a tree that is
    /// really one mount. That is a loud refusal naming the paths, not a silent
    /// pass — the direction this gate is required to fail in.
    #[serde(default)]
    pub foreign_device_paths: Vec<String>,
}

impl WorkspaceManifest {
    /// Is this image complete enough to prove anything?
    pub fn is_complete(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The walk roots this image was captured from, as `(root label, path)`.
    pub fn walk_roots(&self) -> Vec<(String, PathBuf)> {
        let mut roots = vec![(
            WORKSPACE_ROOT_LABEL.to_string(),
            PathBuf::from(&self.workspace_root),
        )];
        if let Some(gitdir) = &self.gitdir_root {
            roots.push((GITDIR_ROOT_LABEL.to_string(), PathBuf::from(gitdir)));
        }
        roots
    }

    /// The newest `ctime` recorded under each root label — the value a barrier
    /// for that root must strictly exceed.
    pub fn max_ctime_by_root(&self) -> BTreeMap<String, FsTime> {
        let mut maxes: BTreeMap<String, FsTime> = BTreeMap::new();
        for (key, entry) in &self.entries {
            let label = root_label_of_key(key).to_string();
            let ctime = FsTime::new(entry.ctime_sec, entry.ctime_nsec);
            maxes
                .entry(label)
                .and_modify(|current| {
                    if ctime > *current {
                        *current = ctime;
                    }
                })
                .or_insert(ctime);
        }
        maxes
    }

    /// Seal this **pre-image** with a capture-time clock barrier per walk root.
    ///
    /// Call it after the walk and before the image is written. On success every
    /// root that has entries carries a barrier the parent *observed* on that
    /// root's own filesystem, strictly greater than every `ctime` in the image
    /// for that root — so any later inode change is stamped strictly past what
    /// was recorded, and the `mtime`/`ctime` comparison in `compare_entry`
    /// stops depending on clock resolution.
    ///
    /// Fails (never degrades, never sleeps a fixed amount) when the advance
    /// cannot be observed. A pre-image whose window cannot be resolved cannot
    /// convict, so the dispatch must not start under it.
    pub fn seal_with_clock_barrier(&mut self) -> Result<(), String> {
        // Before spending a single probe write: a barrier is only an argument if
        // the clock it measures is the clock that stamps the entries, and if
        // that clock advances on an inode change at all. Establishing one
        // without these would produce a barrier that *looks* like every other
        // barrier and proves nothing.
        self.verify_timestamp_preconditions()?;
        let maxes = self.max_ctime_by_root();
        let roots = self.walk_roots();
        let root_paths: Vec<PathBuf> = roots.iter().map(|(_, path)| path.clone()).collect();
        let mut barriers: BTreeMap<String, FsTime> = BTreeMap::new();
        for (label, root) in roots {
            // A root the walk produced no entries for needs no barrier — and
            // `verify_clock_barriers` only demands one where entries exist.
            let Some(must_exceed) = maxes.get(&label).copied() else {
                continue;
            };
            let observed = establish_clock_barrier(&root, must_exceed, &root_paths)
                .map_err(|e| format!("clock barrier for walk root {label}: {e}"))?;
            barriers.insert(label, observed);
        }
        self.clock_barriers = barriers;
        // Self-check: seal and verify must agree, or the barrier is decorative.
        self.verify_clock_barriers()
    }

    /// The two things that must hold before ANY timestamp in this image can be
    /// used as evidence, barrier or not: a real ctime witness, and one
    /// filesystem per walk root.
    ///
    /// Checked against both images at gate time — the post-image's timestamps
    /// are half of every comparison, so a post-image with no ctime witness makes
    /// the comparison unsound even against a perfectly sealed pre-image.
    pub fn verify_timestamp_preconditions(&self) -> Result<(), String> {
        if self.ctime_witness != CtimeWitness::PosixCtime {
            return Err(format!(
                "this image records ctime witness {:?}, not {:?}: the platform it was captured on \
                 exposes no inode change time, so the ctime fields hold a placeholder that does \
                 not advance when a file changes. A clock barrier measured against such a field \
                 would prove an advance that cannot happen, and mutate-then-restore would be \
                 invisible to the comparison. This gate refuses to run there rather than certify \
                 a tree it cannot inspect",
                self.ctime_witness.as_str(),
                CtimeWitness::PosixCtime.as_str(),
            ));
        }
        if !self.foreign_device_paths.is_empty() {
            return Err(format!(
                "this walk root spans more than one filesystem: {} entry/entries are not on their \
                 walk root's device. The clock barrier is observed with a probe on the ROOT's \
                 device, so it never measured the clock that stamps these, and applying it to \
                 them would be exactly the cross-filesystem claim the probe placement check \
                 already refuses. A per-device barrier is not constructible here — the probe must \
                 live outside every walk root, and a filesystem mounted inside the root has no \
                 location outside it — so a multi-device walk root is refused rather than \
                 certified on another filesystem's evidence: {}",
                self.foreign_device_paths.len(),
                self.foreign_device_paths.join(", "),
            ));
        }
        Ok(())
    }

    /// Does this pre-image carry a barrier that dominates every `ctime` it
    /// recorded? `Err` names the first entry it cannot vouch for.
    ///
    /// This is deliberately re-checked at gate time rather than trusted: it
    /// makes the invariant a property of the *data*, so an image produced by an
    /// older binary, a partial write, or a future refactor that forgets to seal
    /// is caught instead of quietly passing.
    pub fn verify_clock_barriers(&self) -> Result<(), String> {
        self.verify_timestamp_preconditions()?;
        for (key, entry) in &self.entries {
            let label = root_label_of_key(key);
            let ctime = FsTime::new(entry.ctime_sec, entry.ctime_nsec);
            match self.clock_barriers.get(label) {
                None => {
                    return Err(format!(
                        "walk root {label:?} carries no capture-time clock barrier, so a \
                         timestamp that looks unchanged at {key} cannot be proven unchanged \
                         (a write inside the capture's own clock tick would be invisible)"
                    ))
                }
                Some(barrier) if *barrier <= ctime => {
                    return Err(format!(
                        "clock barrier for walk root {label:?} is {barrier}, which does not \
                         strictly exceed the ctime {ctime} recorded at {key}: a post-capture \
                         write to that entry could carry the same stamp, so an unchanged-looking \
                         comparison there proves nothing"
                    ))
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// The walk-root label a manifest key belongs to (`workspace/src/x` →
/// `workspace`; the bare root entry `workspace` → `workspace`).
pub fn root_label_of_key(key: &str) -> &str {
    match key.split_once('/') {
        Some((label, _)) => label,
        None => key,
    }
}

/// Observe the filesystem clock of `root` advancing strictly past `must_exceed`.
///
/// The probe file is written **beside** `root`, never inside it: an entry the
/// walk already fingerprinted cannot be used as the witness (bumping it would
/// bump the value the barrier has to exceed — the chase never converges), and a
/// probe inside the lease would change this gate's custody story, which is that
/// the parent does not write into the workspace it is about to certify.
///
/// The probe's device is compared against the root's, because a barrier measured
/// on a *different* filesystem's clock proves nothing about this one — a coarse
/// root plus a fine sibling is exactly the silent fail-open this function
/// exists to remove. A mismatch is a refusal.
///
/// `walk_roots` is every root this image covers. A probe location that falls
/// inside any of them is refused rather than used: a `gitdir:` pointer can name
/// a path *inside* the lease, and the probe would then bump a directory the walk
/// had already fingerprinted — a self-inflicted delta, and the parent writing
/// into the tree it is about to certify.
pub fn establish_clock_barrier(
    root: &Path,
    must_exceed: FsTime,
    walk_roots: &[PathBuf],
) -> Result<FsTime, String> {
    // A public entry point, so it repeats the platform refusal instead of
    // relying on its one caller having done it: what this function returns is a
    // `FsTime` that a caller will treat as proof, and on a platform with no
    // change-time witness there is nothing here that could be proof.
    if ctime_witness_kind() != CtimeWitness::PosixCtime {
        return Err(format!(
            "no inode change time is available on this platform, so there is no field for a clock \
             barrier at {} to be observed in. Substituting creation time would prove an advance \
             that cannot occur (creation time does not move when contents change); refusing",
            root.display()
        ));
    }
    let probe_dir = root.parent().ok_or_else(|| {
        format!(
            "walk root {} has no parent directory to hold the clock probe; the parent must be \
             able to write one file beside the root, on the same filesystem",
            root.display()
        )
    })?;
    let resolved_probe_dir = probe_dir
        .canonicalize()
        .unwrap_or_else(|_| probe_dir.to_path_buf());
    for walk_root in walk_roots {
        let resolved = walk_root
            .canonicalize()
            .unwrap_or_else(|_| walk_root.clone());
        if resolved_probe_dir.starts_with(&resolved) {
            return Err(format!(
                "the clock probe for walk root {} would be written to {}, which is inside walk \
                 root {}; the parent must not write into a tree it is about to certify unchanged, \
                 and an entry the walk already fingerprinted cannot witness the clock advance",
                root.display(),
                probe_dir.display(),
                walk_root.display()
            ));
        }
    }
    let root_meta = std::fs::symlink_metadata(root)
        .map_err(|e| format!("lstat walk root {}: {e}", root.display()))?;
    let root_dev = device_id(&root_meta);

    let probe = probe_dir.join(format!(".tachi-postflight-clock-probe.{}", probe_suffix()));
    let observed = barrier_spin(&probe, root, root_dev, must_exceed);
    // Best-effort cleanup on every path: the probe is scratch, and it lives
    // outside every walk root, so a leftover cannot affect a verdict.
    let _ = std::fs::remove_file(&probe);
    observed
}

fn barrier_spin(
    probe: &Path,
    root: &Path,
    root_dev: Option<u64>,
    must_exceed: FsTime,
) -> Result<FsTime, String> {
    let deadline = Instant::now() + CLOCK_BARRIER_TIMEOUT;
    let mut device_checked = false;
    loop {
        std::fs::write(probe, b"tachi postflight clock probe").map_err(|e| {
            format!(
                "write clock probe {}: {e}; the parent must be able to write one file beside the \
                 walk root to observe that filesystem's clock",
                probe.display()
            )
        })?;
        let meta = std::fs::symlink_metadata(probe)
            .map_err(|e| format!("lstat clock probe {}: {e}", probe.display()))?;

        if !device_checked {
            let probe_dev = device_id(&meta);
            if let (Some(root_dev), Some(probe_dev)) = (root_dev, probe_dev) {
                if root_dev != probe_dev {
                    return Err(format!(
                        "clock probe {} is on device {probe_dev} but walk root {} is on device \
                         {root_dev}; a barrier measured on another filesystem's clock proves \
                         nothing about this one",
                        probe.display(),
                        root.display()
                    ));
                }
            }
            device_checked = true;
        }

        let (sec, nsec) = unix_ctime(&meta);
        let observed = FsTime::new(sec, nsec);
        if observed > must_exceed {
            return Ok(observed);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the filesystem clock at {} did not advance past {must_exceed} within \
                 {CLOCK_BARRIER_TIMEOUT:?} (last observed {observed}); without an observed \
                 advance, a worker's write could carry the same stamp as the pre-image and be \
                 invisible, so this dispatch must not start",
                root.display(),
            ));
        }
        std::thread::sleep(CLOCK_BARRIER_POLL);
    }
}

fn probe_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos:x}-{seq:x}", std::process::id())
}

/// What to walk. Both roots are supplied by the caller: nothing in here is
/// derived from a file the worker can write.
#[derive(Debug, Clone)]
pub struct CaptureSpec<'a> {
    pub workspace_root: &'a Path,
    /// The external `gitdir`, **pinned by the parent before the worker was
    /// spawned** (`None` for a plain repo / in-tree `.git`). Pass the value the
    /// pre-image recorded — never a fresh [`resolve_external_git_dir`] of a
    /// workspace the worker has had write access to.
    pub gitdir_root: Option<&'a Path>,
    /// Directory names (e.g. `target`) whose subtrees are walked and
    /// fingerprinted but not content-hashed. Empty = hash everything.
    pub unhashed_dir_names: &'a [String],
}

impl<'a> CaptureSpec<'a> {
    /// Hash everything under `workspace_root`, with the given pinned gitdir.
    pub fn new(workspace_root: &'a Path, gitdir_root: Option<&'a Path>) -> CaptureSpec<'a> {
        CaptureSpec {
            workspace_root,
            gitdir_root,
            unhashed_dir_names: &[],
        }
    }
}

/// Capture a complete image of the roots named by `spec`.
///
/// Errors only when the workspace root itself is unusable; per-entry failures
/// (including an unreachable pinned `gitdir`) are collected into
/// [`WorkspaceManifest::errors`] so the caller sees a *degraded* image rather
/// than a silently partial one, and fails closed on it.
pub fn capture(spec: &CaptureSpec) -> Result<WorkspaceManifest, String> {
    let workspace_root = spec.workspace_root;
    let root_meta = std::fs::symlink_metadata(workspace_root)
        .map_err(|e| format!("stat workspace root {}: {e}", workspace_root.display()))?;
    if !root_meta.is_dir() {
        return Err(format!(
            "workspace root {} is not a directory",
            workspace_root.display()
        ));
    }

    let mut manifest = WorkspaceManifest {
        workspace_root: workspace_root.to_string_lossy().to_string(),
        gitdir_root: spec.gitdir_root.map(|p| p.to_string_lossy().to_string()),
        captured_at: chrono::Utc::now().to_rfc3339(),
        entries: BTreeMap::new(),
        errors: Vec::new(),
        unhashed_roots: Vec::new(),
        // A bare capture is not yet a usable pre-image: only
        // `seal_with_clock_barrier` can fill this in, and a post-image never
        // needs one. Leaving it empty here is what makes an unsealed image fail
        // closed at gate time.
        clock_barriers: BTreeMap::new(),
        // Stamped from the build target, not inferred by a reader: it is what
        // makes the platform refusal a property of the data (and therefore
        // testable) rather than a `cfg` a test can never reach.
        ctime_witness: ctime_witness_kind(),
        foreign_device_paths: Vec::new(),
    };

    walk_root(
        workspace_root,
        WORKSPACE_ROOT_LABEL,
        spec.unhashed_dir_names,
        &mut manifest,
    );
    if let Some(gitdir) = spec.gitdir_root {
        walk_root(
            gitdir,
            GITDIR_ROOT_LABEL,
            spec.unhashed_dir_names,
            &mut manifest,
        );
    }
    manifest.unhashed_roots.sort();
    Ok(manifest)
}

/// Resolve a linked git worktree's external metadata dir. Returns `None` when
/// `.git` is a directory (already inside the walk) or absent.
///
/// # This reads a worker-writable file
///
/// `.git` lives *inside the lease workspace*. Call this **only from the parent,
/// before the worker is spawned**, and pin the answer (that is what
/// [`WorkspaceManifest::gitdir_root`] is for). Calling it on a workspace a
/// worker has already touched hands the worker the choice of walk root.
pub fn resolve_external_git_dir(workspace_root: &Path) -> Option<PathBuf> {
    let dot_git = workspace_root.join(".git");
    let meta = std::fs::symlink_metadata(&dot_git).ok()?;
    if !meta.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim()
        .to_string();
    if target.is_empty() {
        return None;
    }
    let path = PathBuf::from(target);
    let path = if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    };
    if path.is_dir() {
        Some(path)
    } else {
        None
    }
}

fn walk_root(
    root: &Path,
    label: &str,
    unhashed_dir_names: &[String],
    manifest: &mut WorkspaceManifest,
) {
    // The root entry itself is fingerprinted (keyed by the bare label) — an
    // xattr or a chmod applied to the workspace directory is a change to the
    // workspace, and a walk that only covered the children would miss it.
    //
    // The root's device is captured here and every entry below is checked
    // against it. This is the walk-level half of the same rule the clock probe
    // enforces at the root level: one barrier is one filesystem's clock, so an
    // entry on a second filesystem is an entry the barrier never covered.
    // `device_id` is read off metadata the walk already stat'ed — no extra
    // syscall.
    let mut root_dev: Option<u64> = None;
    match std::fs::symlink_metadata(root) {
        Ok(meta) => {
            root_dev = device_id(&meta);
            if root_dev.is_none() {
                // Not "assume one device": on a platform that cannot report a
                // device id, `Some(a) == Some(b)` degenerates to `None == None`
                // and every cross-device entry would silently compare equal.
                // Record the root itself so the image is refused.
                manifest.foreign_device_paths.push(format!(
                    "{label} (device ids are unavailable on this platform, so a walk root that \
                     spans two filesystems cannot be ruled out)"
                ));
            }
            let fingerprint = fingerprint_entry(root, &meta, true, manifest);
            manifest.entries.insert(label.to_string(), fingerprint);
        }
        Err(e) => manifest
            .errors
            .push(format!("{label}: lstat root {}: {e}", root.display())),
    }

    // `bool` = hash file content in this subtree.
    let mut stack = vec![(root.to_path_buf(), true)];
    while let Some((dir, hash_content)) = stack.pop() {
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                manifest
                    .errors
                    .push(format!("{label}: read_dir {}: {e}", dir.display()));
                continue;
            }
        };
        for entry in read_dir {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    manifest
                        .errors
                        .push(format!("{label}: dir entry under {}: {e}", dir.display()));
                    continue;
                }
            };
            let path = entry.path();
            let Some(key) = manifest_key(root, &path, label) else {
                manifest.errors.push(format!(
                    "{label}: non-UTF-8 path under {} (cannot be keyed unambiguously)",
                    dir.display()
                ));
                continue;
            };
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(e) => {
                    manifest
                        .errors
                        .push(format!("{label}: lstat {}: {e}", path.display()));
                    continue;
                }
            };
            // Only when the root's own device is known — otherwise the marker
            // pushed above already covers the whole root, and repeating it per
            // entry would bury it under one line per file.
            if root_dev.is_some() {
                let entry_dev = device_id(&meta);
                if !shares_barrier_device(root_dev, entry_dev) {
                    manifest.foreign_device_paths.push(format!(
                        "{key} (device {}; walk root {label} is device {})",
                        entry_dev.map_or_else(|| "unreadable".to_string(), |d| d.to_string()),
                        root_dev.map_or_else(|| "unreadable".to_string(), |d| d.to_string()),
                    ));
                }
            }
            let fingerprint = fingerprint_entry(&path, &meta, hash_content, manifest);
            if fingerprint.kind == EntryKind::Dir {
                let is_unhashed_root = hash_content
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            unhashed_dir_names.iter().any(|skip| skip.as_str() == name)
                        });
                if is_unhashed_root {
                    manifest.unhashed_roots.push(key.clone());
                }
                stack.push((path, hash_content && !is_unhashed_root));
            }
            manifest.entries.insert(key, fingerprint);
        }
    }
}

/// Can an entry on `entry_dev` be vouched for by a barrier measured on the walk
/// root's `root_dev`?
///
/// The `_ => false` arm is the load-bearing one. Comparing the two `Option<u64>`
/// values directly would make `None == None` **true**, so on any platform that
/// reports no device id at all, every entry would silently qualify as "same
/// filesystem as the root" — a barrier applied to entries it never measured,
/// which is the exact fail-open shape this module is here to remove. Unknown is
/// not a match.
pub fn shares_barrier_device(root_dev: Option<u64>, entry_dev: Option<u64>) -> bool {
    match (root_dev, entry_dev) {
        (Some(root), Some(entry)) => root == entry,
        _ => false,
    }
}

fn manifest_key(root: &Path, path: &Path, label: &str) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let rel = rel.to_str()?;
    Some(format!("{label}/{}", rel.replace('\\', "/")))
}

fn fingerprint_entry(
    path: &Path,
    meta: &std::fs::Metadata,
    hash_content: bool,
    manifest: &mut WorkspaceManifest,
) -> EntryFingerprint {
    let file_type = meta.file_type();
    let kind = if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };

    let content_hash = if kind == EntryKind::File && hash_content {
        match digest_file(path) {
            Ok(hash) => Some(hash),
            Err(e) => {
                manifest
                    .errors
                    .push(format!("hash {}: {e}", path.display()));
                None
            }
        }
    } else {
        None
    };

    let link_target = if kind == EntryKind::Symlink {
        match std::fs::read_link(path) {
            Ok(target) => Some(target.to_string_lossy().to_string()),
            Err(e) => {
                manifest
                    .errors
                    .push(format!("readlink {}: {e}", path.display()));
                None
            }
        }
    } else {
        None
    };

    let xattr_hash = match xattr_digest(path) {
        Ok(hash) => hash,
        Err(e) => {
            manifest
                .errors
                .push(format!("xattr {}: {e}", path.display()));
            None
        }
    };

    let (mtime_sec, mtime_nsec) = unix_mtime(meta);
    let (ctime_sec, ctime_nsec) = unix_ctime(meta);
    EntryFingerprint {
        kind,
        size: unix_size(meta),
        mode: unix_mode(meta),
        ino: unix_ino(meta),
        nlink: unix_nlink(meta),
        mtime_sec,
        mtime_nsec,
        ctime_sec,
        ctime_nsec,
        content_hash,
        link_target,
        xattr_hash,
    }
}

// ─── platform metadata accessors ────────────────────────────────────────────

#[cfg(unix)]
fn unix_size(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.size()
}
#[cfg(unix)]
fn unix_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.mode()
}
#[cfg(unix)]
fn unix_ino(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}
#[cfg(unix)]
fn unix_nlink(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.nlink()
}
#[cfg(unix)]
fn unix_mtime(meta: &std::fs::Metadata) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (meta.mtime(), meta.mtime_nsec())
}
#[cfg(unix)]
fn unix_ctime(meta: &std::fs::Metadata) -> (i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (meta.ctime(), meta.ctime_nsec())
}
/// The filesystem a path lives on. `None` on a platform where this module has
/// no way to ask — a **stated blind spot**, in the same shape as the xattr one
/// below: the clock-barrier device check is then skipped rather than guessed at.
#[cfg(unix)]
fn device_id(meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.dev())
}

#[cfg(not(unix))]
fn unix_size(meta: &std::fs::Metadata) -> u64 {
    meta.len()
}
#[cfg(not(unix))]
fn unix_mode(_meta: &std::fs::Metadata) -> u32 {
    0
}
#[cfg(not(unix))]
fn unix_ino(_meta: &std::fs::Metadata) -> u64 {
    0
}
#[cfg(not(unix))]
fn unix_nlink(_meta: &std::fs::Metadata) -> u64 {
    0
}
#[cfg(not(unix))]
fn unix_mtime(meta: &std::fs::Metadata) -> (i64, i64) {
    file_time_pair(meta.modified().ok())
}
/// There is no inode change time here, and this says so with a CONSTANT.
///
/// It used to return `meta.created()`. That is not a change time: creation time
/// does not advance when a file's contents change, so `establish_clock_barrier`
/// could observe a probe's creation time "advance" past a walk's creation times
/// and report a barrier — a proof manufactured out of a field that can never
/// move, which is worse than the missing barrier the barrier was added to
/// supply.
///
/// A constant is the fail-closed choice twice over: [`ctime_witness_kind`]
/// refuses this platform outright (the loud path), and if that refusal is ever
/// deleted, `observed > must_exceed` can never hold against a constant, so the
/// barrier spin times out and still refuses. There is no arrangement of this
/// file in which a non-unix platform passes.
#[cfg(not(unix))]
fn unix_ctime(_meta: &std::fs::Metadata) -> (i64, i64) {
    (0, 0)
}
#[cfg(not(unix))]
fn device_id(_meta: &std::fs::Metadata) -> Option<u64> {
    None
}
#[cfg(not(unix))]
fn file_time_pair(time: Option<std::time::SystemTime>) -> (i64, i64) {
    let Some(time) = time else {
        return (0, 0);
    };
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos() as i64),
        Err(_) => (0, 0),
    }
}

// ─── hashing ────────────────────────────────────────────────────────────────

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Cryptographic digest — NOT a cheap non-cryptographic hash. The worker this
/// gate is aimed at can run arbitrary code, so a forgeable digest (FNV/SipHash)
/// would let a deliberate change be dressed up as an unchanged file.
pub fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Blake2s256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

fn digest_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Blake2s256::new();
    let mut buf = [0u8; 65536];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

// ─── extended attributes ────────────────────────────────────────────────────

/// Digest of an entry's extended attributes (`None` when it has none).
///
/// Symlinks are never followed (`XATTR_NOFOLLOW` on macOS, `l*xattr` on Linux),
/// so a symlink's own xattrs are fingerprinted rather than its target's.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub fn xattr_digest(path: &Path) -> Result<Option<String>, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "path contains an interior NUL byte".to_string())?;

    let size = unsafe { list_xattr(c_path.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return match xattr_errno_class() {
            XattrErrno::Absent => Ok(None),
            XattrErrno::Real(err) => Err(format!("listxattr: {err}")),
        };
    }
    if size == 0 {
        return Ok(None);
    }

    let mut buf: Vec<libc::c_char> = vec![0; size as usize];
    let written = unsafe { list_xattr(c_path.as_ptr(), buf.as_mut_ptr(), buf.len()) };
    if written < 0 {
        return match xattr_errno_class() {
            XattrErrno::Absent => Ok(None),
            XattrErrno::Real(err) => Err(format!("listxattr: {err}")),
        };
    }
    let bytes: Vec<u8> = buf[..written as usize].iter().map(|c| *c as u8).collect();

    let mut records: Vec<String> = Vec::new();
    for name in bytes.split(|b| *b == 0) {
        if name.is_empty() {
            continue;
        }
        let c_name =
            CString::new(name).map_err(|_| "xattr name contains a NUL byte".to_string())?;
        let value = read_xattr_value(&c_path, &c_name)?;
        let printable = String::from_utf8_lossy(name).to_string();
        match value {
            Some(value) => records.push(format!("{printable}={}", digest_bytes(&value))),
            // Raced away between list and get — record the name so the change
            // is still visible rather than silently dropped.
            None => records.push(format!("{printable}=<absent>")),
        }
    }
    if records.is_empty() {
        return Ok(None);
    }
    records.sort();
    Ok(Some(digest_bytes(records.join("\n").as_bytes())))
}

/// Platforms whose xattr API this module has not been qualified against report
/// `None` rather than guessing. That is a *stated blind spot*, and it is why
/// `ctime` is also fingerprinted: on any platform, setting an xattr bumps the
/// inode's ctime, so the change still surfaces as a metadata delta.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
pub fn xattr_digest(_path: &Path) -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
enum XattrErrno {
    /// "This entry has no xattrs" / "this filesystem has none" — not an error.
    Absent,
    Real(std::io::Error),
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn xattr_errno_class() -> XattrErrno {
    let err = std::io::Error::last_os_error();
    let code = err.raw_os_error().unwrap_or(0);
    // "no such attribute" is spelled ENOATTR on macOS and ENODATA on Linux;
    // ENOTSUP means the filesystem carries no xattrs at all. Neither is a
    // failure to capture — both mean "there is nothing here to fingerprint".
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let absent = code == libc::ENOATTR || code == libc::ENOTSUP;
    #[cfg(target_os = "linux")]
    let absent = code == libc::ENODATA || code == libc::ENOTSUP;
    if absent {
        XattrErrno::Absent
    } else {
        XattrErrno::Real(err)
    }
}

/// SAFETY (both `list_xattr` and `read_xattr_value`): the pointers handed to
/// libc are (a) a NUL-terminated path/name from a live `CString`, and (b) a
/// buffer whose capacity is passed as the same `size` argument, so libc never
/// writes past it. No Rust memory is aliased across the call.
#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe fn list_xattr(path: *const libc::c_char, buf: *mut libc::c_char, size: usize) -> isize {
    libc::listxattr(path, buf, size, libc::XATTR_NOFOLLOW)
}

#[cfg(target_os = "linux")]
unsafe fn list_xattr(path: *const libc::c_char, buf: *mut libc::c_char, size: usize) -> isize {
    libc::llistxattr(path, buf, size)
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
fn read_xattr_value(
    c_path: &std::ffi::CStr,
    c_name: &std::ffi::CStr,
) -> Result<Option<Vec<u8>>, String> {
    let size = unsafe { get_xattr(c_path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return match xattr_errno_class() {
            XattrErrno::Absent => Ok(None),
            XattrErrno::Real(err) => Err(format!("getxattr: {err}")),
        };
    }
    let mut buf: Vec<u8> = vec![0; size as usize];
    let written = unsafe {
        get_xattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    if written < 0 {
        return match xattr_errno_class() {
            XattrErrno::Absent => Ok(None),
            XattrErrno::Real(err) => Err(format!("getxattr: {err}")),
        };
    }
    buf.truncate(written as usize);
    Ok(Some(buf))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
unsafe fn get_xattr(
    path: *const libc::c_char,
    name: *const libc::c_char,
    value: *mut libc::c_void,
    size: usize,
) -> isize {
    libc::getxattr(path, name, value, size, 0, libc::XATTR_NOFOLLOW)
}

#[cfg(target_os = "linux")]
unsafe fn get_xattr(
    path: *const libc::c_char,
    name: *const libc::c_char,
    value: *mut libc::c_void,
    size: usize,
) -> isize {
    libc::lgetxattr(path, name, value, size)
}

// ─── diff ───────────────────────────────────────────────────────────────────

/// The class of change observed at one path. Ordered by how loudly it should be
/// read, not by severity of intent — every one of these is a prohibited delta
/// under a `detect-and-reject` contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaKind {
    Added,
    Removed,
    TypeChanged,
    ContentChanged,
    SymlinkTargetChanged,
    XattrChanged,
    /// Same bytes, different inode/ctime/mtime/mode/nlink — this is the class a
    /// `git diff` cannot see at all (mutate-then-restore, chmod, rewrite-via-rename).
    MetadataChanged,
    /// Not a change: a path (or a whole image) the gate could not read, and
    /// therefore cannot prove unchanged. Reported as a prohibited delta because
    /// the gate fails closed — "I could not look" is never "nothing happened".
    Unreadable,
}

impl DeltaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeltaKind::Added => "added",
            DeltaKind::Removed => "removed",
            DeltaKind::TypeChanged => "type_changed",
            DeltaKind::ContentChanged => "content_changed",
            DeltaKind::SymlinkTargetChanged => "symlink_target_changed",
            DeltaKind::XattrChanged => "xattr_changed",
            DeltaKind::MetadataChanged => "metadata_changed",
            DeltaKind::Unreadable => "unreadable",
        }
    }
}

/// One observed change: which path, what kind, and the evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDelta {
    /// Manifest key: `workspace/<rel>` or `gitdir/<rel>`.
    pub path: String,
    pub kind: DeltaKind,
    pub detail: String,
    /// The individual fields that differ (`content …`, `mtime …`, `ctime …`,
    /// `inode …`). Empty for add/remove/unreadable, where there is no
    /// before-and-after to itemize.
    #[serde(default)]
    pub facets: Vec<String>,
    /// What the entry *is* (`None` when the delta is not about a filesystem
    /// entry at all, e.g. an unreadable image). Load-bearing: the declared-scope
    /// forgiveness of directory bookkeeping must apply to directories only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_kind: Option<EntryKind>,
}

/// The facets a directory bumps purely by *holding one more (or one fewer)
/// entry*. Which of these actually move is filesystem-specific and was measured,
/// not assumed:
///
/// * classic Unix (ext4, HFS+): `mtime` + `ctime`, and `nlink` when the new
///   child is itself a directory (its `..` backlink);
/// * **APFS** (this repo's dev + CI platform): a directory's `nlink` **and**
///   `size` track its child count, so *every* child — file or directory — bumps
///   `nlink` and `size` too.
///
/// Assuming the classic set is exactly the bug this list fixes: it made a
/// declared-scope write always reject on macOS, because the workspace root came
/// back with `nlink`/`size` facets on top of the timestamps.
pub const DIR_BOOKKEEPING_FACETS: &[&str] = &["mtime ", "ctime ", "size ", "nlink "];

impl WorkspaceDelta {
    /// Is this delta nothing but a directory's own entry-count bookkeeping (see
    /// [`DIR_BOOKKEEPING_FACETS`])? A directory on the path to a permitted write
    /// records that write in these fields and in nothing else — this predicate
    /// is what lets a declared write scope stay usable.
    ///
    /// It forgives nothing that could hide a change: `mode`, `inode`, content
    /// and xattr facets are all outside the list (and a content/xattr/type
    /// change is not even a [`DeltaKind::MetadataChanged`]), and whatever child
    /// caused the bookkeeping bump is itself an `Added`/`Removed` delta that the
    /// contract still has to permit on its own.
    pub fn is_dir_bookkeeping_metadata(&self) -> bool {
        self.kind == DeltaKind::MetadataChanged
            && self.entry_kind == Some(EntryKind::Dir)
            && !self.facets.is_empty()
            && self.facets.iter().all(|facet| {
                DIR_BOOKKEEPING_FACETS
                    .iter()
                    .any(|allowed| facet.starts_with(allowed))
            })
    }
}

/// Compare a pre-image against a post-image. Deterministic order (by path) so a
/// receipt is stable and diffable.
pub fn diff(pre: &WorkspaceManifest, post: &WorkspaceManifest) -> Vec<WorkspaceDelta> {
    let mut deltas: Vec<WorkspaceDelta> = Vec::new();

    for (path, before) in &pre.entries {
        match post.entries.get(path) {
            None => deltas.push(WorkspaceDelta {
                path: path.clone(),
                kind: DeltaKind::Removed,
                detail: format!("{} present in the pre-image is gone", before.kind.as_str()),
                facets: Vec::new(),
                entry_kind: Some(before.kind),
            }),
            Some(after) => {
                if let Some(delta) = compare_entry(path, before, after) {
                    deltas.push(delta);
                }
            }
        }
    }
    for (path, after) in &post.entries {
        if !pre.entries.contains_key(path) {
            deltas.push(WorkspaceDelta {
                path: path.clone(),
                kind: DeltaKind::Added,
                detail: format!("new {} not present in the pre-image", after.kind.as_str()),
                facets: Vec::new(),
                entry_kind: Some(after.kind),
            });
        }
    }

    deltas.sort_by(|a, b| {
        (a.path.as_str(), a.kind.as_str()).cmp(&(b.path.as_str(), b.kind.as_str()))
    });
    deltas
}

fn compare_entry(
    path: &str,
    before: &EntryFingerprint,
    after: &EntryFingerprint,
) -> Option<WorkspaceDelta> {
    if before.kind != after.kind {
        return Some(WorkspaceDelta {
            path: path.to_string(),
            kind: DeltaKind::TypeChanged,
            detail: format!("{} -> {}", before.kind.as_str(), after.kind.as_str()),
            facets: Vec::new(),
            entry_kind: Some(after.kind),
        });
    }

    let mut facets: Vec<String> = Vec::new();
    if before.content_hash != after.content_hash {
        facets.push(format!(
            "content {} -> {}",
            short_hash(before.content_hash.as_deref()),
            short_hash(after.content_hash.as_deref())
        ));
    }
    if before.link_target != after.link_target {
        facets.push(format!(
            "symlink target {:?} -> {:?}",
            before.link_target, after.link_target
        ));
    }
    if before.xattr_hash != after.xattr_hash {
        facets.push(format!(
            "xattrs {} -> {}",
            short_hash(before.xattr_hash.as_deref()),
            short_hash(after.xattr_hash.as_deref())
        ));
    }
    if before.size != after.size {
        facets.push(format!("size {} -> {}", before.size, after.size));
    }
    if before.mode != after.mode {
        facets.push(format!("mode {:o} -> {:o}", before.mode, after.mode));
    }
    if before.ino != after.ino {
        facets.push(format!("inode {} -> {}", before.ino, after.ino));
    }
    if before.nlink != after.nlink {
        facets.push(format!("nlink {} -> {}", before.nlink, after.nlink));
    }
    if (before.mtime_sec, before.mtime_nsec) != (after.mtime_sec, after.mtime_nsec) {
        facets.push(format!(
            "mtime {}.{:09} -> {}.{:09}",
            before.mtime_sec, before.mtime_nsec, after.mtime_sec, after.mtime_nsec
        ));
    }
    if (before.ctime_sec, before.ctime_nsec) != (after.ctime_sec, after.ctime_nsec) {
        facets.push(format!(
            "ctime {}.{:09} -> {}.{:09}",
            before.ctime_sec, before.ctime_nsec, after.ctime_sec, after.ctime_nsec
        ));
    }
    if facets.is_empty() {
        return None;
    }

    // Most specific class wins; the detail carries every facet so the receipt
    // shows a mutate-then-restore as "same bytes, new ctime" rather than hiding
    // it behind a single label.
    let kind = if before.content_hash != after.content_hash {
        DeltaKind::ContentChanged
    } else if before.link_target != after.link_target {
        DeltaKind::SymlinkTargetChanged
    } else if before.xattr_hash != after.xattr_hash {
        DeltaKind::XattrChanged
    } else {
        DeltaKind::MetadataChanged
    };

    Some(WorkspaceDelta {
        path: path.to_string(),
        kind,
        detail: facets.join("; "),
        facets,
        entry_kind: Some(after.kind),
    })
}

fn short_hash(hash: Option<&str>) -> String {
    match hash {
        Some(hash) if hash.len() > 12 => hash[..12].to_string(),
        Some(hash) => hash.to_string(),
        None => "none".to_string(),
    }
}
