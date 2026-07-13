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
//! | `ctime` | **mutate-then-restore**: content and mtime can both be restored by an unprivileged worker (`write` + `utimes`), but `ctime` cannot be set by anyone — restoring the bytes still leaves a ctime bump |
//!
//! `atime` is deliberately NOT recorded: reads legitimately bump it (and
//! `relatime`/`noatime` mounts make it non-deterministic), so it is noise, not
//! signal.
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

use blake2::{Blake2s256, Digest};
use serde::{Deserialize, Serialize};

/// Manifest key namespace for the lease workspace itself.
pub const WORKSPACE_ROOT_LABEL: &str = "workspace";
/// Manifest key namespace for a linked worktree's external git metadata dir
/// (the `gitdir:` target of a `.git` *file*). An in-tree `.git` *directory* is
/// covered by the workspace walk and needs no second root.
pub const GITDIR_ROOT_LABEL: &str = "gitdir";

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
    /// Inode change time — unforgeable by an unprivileged worker, so this is
    /// the field that catches mutate-then-restore.
    pub ctime_sec: i64,
    pub ctime_nsec: i64,
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
}

/// Capture a complete image of `workspace_root` (plus the external git metadata
/// dir when the workspace is a linked git worktree).
///
/// Errors only when the root itself is unusable; per-entry failures are
/// collected into [`WorkspaceManifest::errors`] so the caller sees a *degraded*
/// image rather than a silently partial one.
pub fn capture(workspace_root: &Path) -> Result<WorkspaceManifest, String> {
    let root_meta = std::fs::symlink_metadata(workspace_root)
        .map_err(|e| format!("stat workspace root {}: {e}", workspace_root.display()))?;
    if !root_meta.is_dir() {
        return Err(format!(
            "workspace root {} is not a directory",
            workspace_root.display()
        ));
    }

    let gitdir = resolve_external_git_dir(workspace_root);
    let mut manifest = WorkspaceManifest {
        workspace_root: workspace_root.to_string_lossy().to_string(),
        gitdir_root: gitdir.as_ref().map(|p| p.to_string_lossy().to_string()),
        captured_at: chrono::Utc::now().to_rfc3339(),
        entries: BTreeMap::new(),
        errors: Vec::new(),
    };

    walk_root(workspace_root, WORKSPACE_ROOT_LABEL, &mut manifest);
    if let Some(gitdir) = gitdir.as_ref() {
        walk_root(gitdir, GITDIR_ROOT_LABEL, &mut manifest);
    }
    Ok(manifest)
}

/// Resolve a linked git worktree's external metadata dir. Returns `None` when
/// `.git` is a directory (already inside the walk) or absent.
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

fn walk_root(root: &Path, label: &str, manifest: &mut WorkspaceManifest) {
    // The root entry itself is fingerprinted (keyed by the bare label) — an
    // xattr or a chmod applied to the workspace directory is a change to the
    // workspace, and a walk that only covered the children would miss it.
    match std::fs::symlink_metadata(root) {
        Ok(meta) => {
            let fingerprint = fingerprint_entry(root, &meta, manifest);
            manifest.entries.insert(label.to_string(), fingerprint);
        }
        Err(e) => manifest
            .errors
            .push(format!("{label}: lstat root {}: {e}", root.display())),
    }

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
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
            let fingerprint = fingerprint_entry(&path, &meta, manifest);
            if fingerprint.kind == EntryKind::Dir {
                stack.push(path);
            }
            manifest.entries.insert(key, fingerprint);
        }
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

    let content_hash = if kind == EntryKind::File {
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
#[cfg(not(unix))]
fn unix_ctime(meta: &std::fs::Metadata) -> (i64, i64) {
    file_time_pair(meta.created().ok())
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
}

impl WorkspaceDelta {
    /// Is this delta nothing but a timestamp bump on an entry (no content, no
    /// mode, no inode, no xattr change)? A directory on the path to a permitted
    /// write records that write in its own mtime/ctime and in nothing else —
    /// this predicate is what lets a declared write scope stay usable without
    /// forgiving any other change class.
    pub fn is_time_only_metadata(&self) -> bool {
        self.kind == DeltaKind::MetadataChanged
            && !self.facets.is_empty()
            && self
                .facets
                .iter()
                .all(|facet| facet.starts_with("mtime ") || facet.starts_with("ctime "))
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
    })
}

fn short_hash(hash: Option<&str>) -> String {
    match hash {
        Some(hash) if hash.len() > 12 => hash[..12].to_string(),
        Some(hash) => hash.to_string(),
        None => "none".to_string(),
    }
}
