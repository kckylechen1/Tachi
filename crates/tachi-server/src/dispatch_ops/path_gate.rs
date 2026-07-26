//! Shared path-traversal gate for caller-supplied dispatch/run ids.
//!
//! tachi#1173 board autopsy (codex cold-review of uc-u-k2-board-autopsy,
//! eb473fd0) found `board::runs::collect_run_task_by_id` joining a
//! caller-controlled `dispatch_id` (from `tachi_task(action='wait'|'status'|
//! 'cancel')`) directly onto `runs_dir` with no validation -- a
//! `../../../../etc/passwd`-shaped id could read arbitrary files outside
//! `~/.tachi/runs`. The follow-up grep sweep (tachi#1173 k2 fix, this module)
//! found the identical join-with-no-validation shape reused at three more
//! call sites, all fed straight from a caller-supplied `dispatch_id`:
//!
//! - `dispatch::dedupe::load_dispatch_identity_receipt_checked` (from
//!   `TachiCompleteParams::dispatch_id` on `tachi_complete`)
//! - `tools::dispatch_complete_defaults::read_dispatch_defaults_for_complete`
//!   (same param, same call)
//! - `dispatch_ops::predicate::resolve_completion_predicate_context` (same
//!   param, same call)
//!
//! Centralized here so the character-class allowlist and the
//! canonicalize-and-confine defense-in-depth layer are defined once instead
//! of re-derived (and potentially re-drifted) per call site.

use std::io::Read;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::time::SystemTime;

/// Every real dispatch id minted by `dispatch::dedupe::new_dispatch_id` is a
/// single path component drawn from `[A-Za-z0-9_-]`
/// (timestamp-agent-suffix), so anything outside that allowlist is rejected
/// fail-closed -- treated identically to "run not found" rather than
/// surfaced as an error, so a probe gets no signal about what does or
/// doesn't exist on disk.
pub(crate) fn is_valid_dispatch_id(dispatch_id: &str) -> bool {
    !dispatch_id.is_empty()
        && dispatch_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

/// Defense in depth on top of [`is_valid_dispatch_id`]: canonicalize
/// `candidate_dir` (which must already exist) and confirm the resolved path
/// still lives under `root_dir`. Catches anything the character allowlist
/// alone might miss -- e.g. a symlinked run directory planted inside
/// `root_dir` that points elsewhere.
pub(crate) fn canonical_dir_is_within(candidate_dir: &Path, root_dir: &Path) -> bool {
    let (Ok(canonical_candidate), Ok(canonical_root)) =
        (candidate_dir.canonicalize(), root_dir.canonicalize())
    else {
        return false;
    };
    canonical_candidate.starts_with(&canonical_root)
}

#[cfg(unix)]
pub(crate) fn ensure_descriptor_reads_supported() -> Result<(), String> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn ensure_descriptor_reads_supported() -> Result<(), String> {
    Err("descriptor-bound O_NOFOLLOW reads are unavailable on this platform".to_string())
}

/// Open a regular file beneath `root_dir` without following any path component
/// during the descriptor walk, then read from that same final descriptor.
///
/// Canonicalization is used only to resolve an allowed in-root alias to a
/// relative physical path. The subsequent open never reopens the caller's
/// candidate pathname: it opens the canonical root as a directory descriptor,
/// walks each resolved component with `openat(O_NOFOLLOW)`, validates the final
/// descriptor as a regular file, and returns that descriptor for the read.
fn open_regular_file_within(
    root_dir: &Path,
    candidate: &Path,
) -> Result<Option<std::fs::File>, String> {
    ensure_descriptor_reads_supported()?;
    let canonical_root = root_dir.canonicalize().map_err(|error| {
        format!(
            "refusing descriptor-bound read: containment root {} cannot be resolved: {error}",
            root_dir.display()
        )
    })?;
    let canonical_candidate = match candidate.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "refusing descriptor-bound read: {} cannot be resolved: {error}",
                candidate.display()
            ));
        }
    };
    let relative = canonical_candidate
        .strip_prefix(&canonical_root)
        .map_err(|_| {
            format!(
                "refusing descriptor-bound read: {} resolves outside containment root {}",
                canonical_candidate.display(),
                canonical_root.display()
            )
        })?
        .to_path_buf();

    run_secure_read_hook(SecureReadHookStage::AfterValidation, &canonical_candidate);

    let file = open_regular_file_relative(&canonical_root, &relative).map_err(|error| {
        format!(
            "refusing descriptor-bound read of {}: {error}",
            canonical_candidate.display()
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "refusing descriptor-bound read of {}: final descriptor metadata failed: {error}",
            canonical_candidate.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "refusing descriptor-bound read of {}: final descriptor is not a regular file",
            canonical_candidate.display()
        ));
    }

    run_secure_read_hook(SecureReadHookStage::AfterOpen, &canonical_candidate);
    Ok(Some(file))
}

pub(crate) struct DescriptorTextRead {
    pub(crate) text: String,
    pub(crate) modified: Option<SystemTime>,
}

pub(crate) fn read_text_file_within_with_metadata(
    root_dir: &Path,
    candidate: &Path,
    max_bytes: usize,
) -> Result<Option<DescriptorTextRead>, String> {
    let Some(mut file) = open_regular_file_within(root_dir, candidate)? else {
        return Ok(None);
    };
    let metadata = file.metadata().map_err(|error| {
        format!(
            "refusing descriptor-bound metadata read of {}: {error}",
            candidate.display()
        )
    })?;
    if metadata.len() > max_bytes as u64 {
        return Err(format!(
            "refusing descriptor-bound text read of {}: {} bytes exceeds named limit of {max_bytes} bytes",
            candidate.display(),
            metadata.len()
        ));
    }
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| "descriptor text-read limit cannot be usize::MAX".to_string())?;
    let mut raw = Vec::with_capacity(read_limit.min(8 * 1024));
    file.by_ref()
        .take(read_limit as u64)
        .read_to_end(&mut raw)
        .map_err(|error| {
            format!(
                "refusing descriptor-bound text read of {}: {error}",
                candidate.display()
            )
        })?;
    if raw.len() > max_bytes {
        return Err(format!(
            "refusing descriptor-bound text read of {}: file grew beyond named limit of {max_bytes} bytes while reading",
            candidate.display()
        ));
    }
    let text = String::from_utf8(raw).map_err(|error| {
        format!(
            "refusing descriptor-bound text read of {}: content is not valid UTF-8: {error}",
            candidate.display()
        )
    })?;
    Ok(Some(DescriptorTextRead {
        text,
        modified: metadata.modified().ok(),
    }))
}

pub(crate) fn read_text_file_within(
    root_dir: &Path,
    candidate: &Path,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    read_text_file_within_with_metadata(root_dir, candidate, max_bytes)
        .map(|read| read.map(|read| read.text))
}

pub(crate) fn regular_file_len_within(
    root_dir: &Path,
    candidate: &Path,
) -> Result<Option<u64>, String> {
    let Some(file) = open_regular_file_within(root_dir, candidate)? else {
        return Ok(None);
    };
    file.metadata()
        .map(|metadata| Some(metadata.len()))
        .map_err(|error| {
            format!(
                "refusing descriptor-bound metadata read of {}: {error}",
                candidate.display()
            )
        })
}

#[cfg(unix)]
fn open_regular_file_relative(root: &Path, relative: &Path) -> Result<std::fs::File, String> {
    use std::ffi::{CString, OsStr};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    fn openat_owned(parent: &OwnedFd, name: &OsStr, directory: bool) -> Result<OwnedFd, String> {
        let name = CString::new(name.as_bytes())
            .map_err(|error| format!("path component contains an interior NUL: {error}"))?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: `parent` is a live directory descriptor, `name` is a valid
        // NUL-terminated component, and no O_CREAT flag means no mode argument
        // is consumed. A successful descriptor is transferred into OwnedFd.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: `fd` is freshly returned by openat and exclusively owned.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    let slash = CString::new("/").expect("slash has no interior NUL");
    // SAFETY: slash is a valid NUL-terminated absolute path and no O_CREAT
    // flag is present. A successful descriptor is transferred into OwnedFd.
    let root_fd = unsafe {
        libc::open(
            slash.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY,
        )
    };
    if root_fd < 0 {
        return Err(format!(
            "open filesystem root: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `root_fd` is freshly returned by open and exclusively owned.
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };

    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => directory = openat_owned(&directory, name, true)?,
            other => return Err(format!("unsupported containment-root component {other:?}")),
        }
    }

    let components: Vec<_> = relative.components().collect();
    if components.is_empty() {
        return Err("candidate resolves to the containment root, not a file".to_string());
    }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(format!("unsupported candidate component {component:?}"));
        };
        let final_component = index + 1 == components.len();
        let opened = openat_owned(&directory, name, !final_component)?;
        if final_component {
            return Ok(std::fs::File::from(opened));
        }
        directory = opened;
    }
    unreachable!("non-empty component walk returns at the final component")
}

#[cfg(not(unix))]
fn open_regular_file_relative(_root: &Path, _relative: &Path) -> Result<std::fs::File, String> {
    Err("descriptor-bound O_NOFOLLOW reads are unavailable on this platform".to_string())
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecureReadHookStage {
    AfterValidation,
    AfterOpen,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
enum SecureReadHookStage {
    AfterValidation,
    AfterOpen,
}

#[cfg(test)]
type SecureReadHook = (SecureReadHookStage, PathBuf, Box<dyn FnOnce(&Path)>);

#[cfg(test)]
thread_local! {
    static SECURE_READ_HOOK: std::cell::RefCell<Option<SecureReadHook>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn install_secure_read_hook(
    stage: SecureReadHookStage,
    target: PathBuf,
    hook: impl FnOnce(&Path) + 'static,
) {
    let target = target.canonicalize().unwrap_or(target);
    SECURE_READ_HOOK.with(|slot| {
        let previous = slot.replace(Some((stage, target, Box::new(hook))));
        assert!(
            previous.is_none(),
            "secure-read test hook already installed"
        );
    });
}

#[cfg(test)]
fn run_secure_read_hook(stage: SecureReadHookStage, target: &Path) {
    let hook = SECURE_READ_HOOK.with(|slot| {
        let matches = slot
            .borrow()
            .as_ref()
            .is_some_and(|(expected_stage, expected_target, _)| {
                *expected_stage == stage && expected_target == target
            });
        matches.then(|| slot.borrow_mut().take().expect("hook exists").2)
    });
    if let Some(hook) = hook {
        hook(target);
    }
}

#[cfg(not(test))]
fn run_secure_read_hook(_stage: SecureReadHookStage, _target: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_DESCRIPTOR_TEXT_MAX_BYTES: usize = 64;
    const INVALID_UTF8_TEST_MAX_BYTES: usize = 2;

    #[test]
    fn is_valid_dispatch_id_accepts_normal_shapes() {
        for ok in [
            "20260718T101010Z-claude-abc12345",
            "abc",
            "a-b_c9",
            "SIMPLE",
        ] {
            assert!(is_valid_dispatch_id(ok), "{ok:?} should be valid");
        }
    }

    #[test]
    fn is_valid_dispatch_id_rejects_traversal_and_degenerate_shapes() {
        for bad in ["../decoy", "..", "", "/etc/passwd", "a/../../decoy", "a/b"] {
            assert!(!is_valid_dispatch_id(bad), "{bad:?} should be rejected");
        }
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_read_preserves_ordinary_and_in_root_aliases() {
        let root = tempfile::tempdir().expect("root");
        let nested = root.path().join("nested");
        std::fs::create_dir_all(&nested).expect("nested");
        std::fs::write(nested.join("result.md"), "inside bytes").expect("inside file");
        std::os::unix::fs::symlink(&nested, root.path().join("allowed-link"))
            .expect("in-root alias");

        assert_eq!(
            read_text_file_within(
                root.path(),
                &root.path().join("nested/result.md"),
                TEST_DESCRIPTOR_TEXT_MAX_BYTES,
            )
            .expect("ordinary read")
            .as_deref(),
            Some("inside bytes")
        );
        assert_eq!(
            read_text_file_within(
                root.path(),
                &root.path().join("allowed-link/result.md"),
                TEST_DESCRIPTOR_TEXT_MAX_BYTES,
            )
            .expect("allowed in-root alias")
            .as_deref(),
            Some("inside bytes")
        );
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_read_refuses_swap_before_open() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let candidate = root.path().join("result.md");
        let outside_file = outside.path().join("result.md");
        std::fs::write(&candidate, "inside bytes").expect("inside file");
        std::fs::write(&outside_file, "outside bytes").expect("outside file");
        let outside_for_hook = outside_file.clone();
        install_secure_read_hook(
            SecureReadHookStage::AfterValidation,
            candidate.clone(),
            move |validated| {
                std::fs::remove_file(validated).expect("remove validated file");
                std::os::unix::fs::symlink(&outside_for_hook, validated)
                    .expect("swap to outside symlink");
            },
        );

        let error = read_text_file_within(root.path(), &candidate, TEST_DESCRIPTOR_TEXT_MAX_BYTES)
            .expect_err("pre-open swap must be refused");
        assert!(error.contains("refusing descriptor-bound read"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_read_keeps_opened_object_across_post_open_swap() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let candidate = root.path().join("result.md");
        let outside_file = outside.path().join("result.md");
        std::fs::write(&candidate, "inside bytes").expect("inside file");
        std::fs::write(&outside_file, "outside bytes").expect("outside file");
        let outside_for_hook = outside_file.clone();
        install_secure_read_hook(
            SecureReadHookStage::AfterOpen,
            candidate.clone(),
            move |opened| {
                std::fs::remove_file(opened).expect("unlink opened file");
                std::os::unix::fs::symlink(&outside_for_hook, opened)
                    .expect("swap to outside symlink");
            },
        );

        let raw = read_text_file_within(root.path(), &candidate, TEST_DESCRIPTOR_TEXT_MAX_BYTES)
            .expect("descriptor read")
            .expect("present");
        assert_eq!(raw, "inside bytes");
        assert_ne!(raw, "outside bytes");
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_text_read_accepts_exact_limit_and_refuses_one_byte_over() {
        const EXACT_LIMIT_BYTES: usize = 8;

        let root = tempfile::tempdir().expect("root");
        let candidate = root.path().join("bounded.txt");
        std::fs::write(&candidate, b"12345678").unwrap();
        assert_eq!(
            read_text_file_within(root.path(), &candidate, EXACT_LIMIT_BYTES)
                .expect("exact-boundary read")
                .as_deref(),
            Some("12345678")
        );

        std::fs::write(&candidate, b"123456789").unwrap();
        let error = read_text_file_within(root.path(), &candidate, EXACT_LIMIT_BYTES)
            .expect_err("one byte over the named limit must be loud");
        assert!(error.contains("9 bytes"), "{error}");
        assert!(error.contains("limit of 8 bytes"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_text_read_loudly_preserves_utf8_requirement() {
        let root = tempfile::tempdir().expect("root");
        let candidate = root.path().join("invalid-utf8.txt");
        std::fs::write(&candidate, [0xff, 0xfe]).unwrap();

        let error = read_text_file_within(root.path(), &candidate, INVALID_UTF8_TEST_MAX_BYTES)
            .expect_err("invalid UTF-8 must remain a loud read failure");
        assert!(error.contains("not valid UTF-8"), "{error}");
    }

    #[cfg(not(unix))]
    #[test]
    fn descriptor_read_support_is_loudly_unavailable() {
        assert!(ensure_descriptor_reads_supported().is_err());
    }
}
