//! DSH owns this short-lived publication transaction. Shared atomic writers may
//! return an error after rename, so neither staged channel is public until both
//! writes succeed. Exclusive links preserve every pre-existing target.

use std::fs::File;
use std::path::{Path, PathBuf};

struct OwnedDirectory {
    path: PathBuf,
    #[cfg(unix)]
    file: File,
}

impl OwnedDirectory {
    fn open(path: PathBuf) -> Result<Self, String> {
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|error| {
                    format!("open DSH publication directory {}: {error}", path.display())
                })?
        };
        Ok(Self {
            path,
            #[cfg(unix)]
            file,
        })
    }

    fn sync(&self) -> Result<(), String> {
        #[cfg(unix)]
        self.file.sync_all().map_err(|error| {
            format!(
                "sync DSH publication directory {}: {error}",
                self.path.display()
            )
        })?;
        Ok(())
    }

    fn remove_owned_staging(&self) -> Result<(), String> {
        let observed = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "inspect DSH staging {}: {error}",
                    self.path.display()
                ))
            }
        };
        #[cfg(unix)]
        if !same_file(
            &observed,
            &self.file.metadata().map_err(|error| error.to_string())?,
        ) {
            return Err(format!(
                "DSH staging ownership changed; preserved {}",
                self.path.display()
            ));
        }
        if !observed.is_dir() || observed.file_type().is_symlink() {
            return Err(format!(
                "DSH staging is no longer the owned directory; preserved {}",
                self.path.display()
            ));
        }
        std::fs::remove_dir_all(&self.path)
            .map_err(|error| format!("remove DSH staging {}: {error}", self.path.display()))
    }
}

struct PublishedSidecar {
    name: &'static str,
    // Pin the object created in private staging, including after its pathname
    // is removed. Cleanup compares the captured entry against this exact inode.
    source: File,
}

/// `before_commit` runs after every sidecar is linked and before publication
/// is made durable. Its failure withdraws the sidecars this call linked, so a
/// caller can make another artifact (the run's `result.md`) part of the same
/// all-or-nothing publication.
pub(super) fn publish(
    run_dir: &Path,
    events: &[u8],
    diagnostics: Option<&[u8]>,
    managed_ephemeral_credential_cleanup: bool,
    before_commit: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    publish_artifacts(
        run_dir,
        &[
            ("dsh-events.jsonl", Some(events)),
            ("dsh-stderr.log", diagnostics),
        ],
        managed_ephemeral_credential_cleanup,
        before_commit,
        || Ok(()),
    )
}

/// Keep ownership of every linked artifact until the lease completes. All
/// fallible filesystem durability work precedes completion, so a completion
/// failure can still withdraw the exact objects this transaction published.
pub(super) fn publish_report(
    run_dir: &Path,
    events: Option<&[u8]>,
    diagnostics: Option<&[u8]>,
    result: &[u8],
    managed_ephemeral_credential_cleanup: bool,
    complete_publication: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    publish_artifacts(
        run_dir,
        &[
            ("dsh-events.jsonl", events),
            ("dsh-stderr.log", diagnostics),
            ("result.md", Some(result)),
        ],
        managed_ephemeral_credential_cleanup,
        || Ok(()),
        complete_publication,
    )
}

fn publish_artifacts(
    run_dir: &Path,
    artifacts: &[(&'static str, Option<&[u8]>)],
    managed_ephemeral_credential_cleanup: bool,
    before_commit: impl FnOnce() -> Result<(), String>,
    complete_publication: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let root = OwnedDirectory::open(
        std::fs::canonicalize(run_dir)
            .map_err(|error| format!("resolve DSH publication directory: {error}"))?,
    )?;
    let staging_path = root.path.join(format!(
        ".dsh-publication-{}",
        uuid::Uuid::new_v4().as_simple()
    ));
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let mut builder = builder;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&staging_path).map_err(|error| {
        format!(
            "create owned DSH staging {}: {error}",
            staging_path.display()
        )
    })?;
    let staging = match OwnedDirectory::open(staging_path.clone()) {
        Ok(directory) => directory,
        Err(error) => {
            return Err(match std::fs::remove_dir(&staging_path) {
                Ok(()) => error,
                Err(cleanup_error) => {
                    format!("{error}; remove newly-created DSH staging: {cleanup_error}")
                }
            });
        }
    };
    let mut published = Vec::new();
    let operation = (|| {
        let mut staged = Vec::new();
        for &(name, body) in artifacts {
            if let Some(body) = body {
                super::super::dispatch::persist_dispatch_result_artifact(
                    &staging.path.join(name),
                    body,
                    managed_ephemeral_credential_cleanup,
                )?;
                staged.push(PublishedSidecar {
                    name,
                    source: open_staged_file(&staging, name)?,
                });
            }
        }
        for artifact in staged {
            link_exclusive(&staging, &root, artifact.name)?;
            // A successful exclusive link creates this ownership fact. A failed
            // link never enters the list, so a blocker is never withdrawn.
            published.push(artifact);
        }
        before_commit()?;
        root.sync()?;
        staging.remove_owned_staging()?;
        root.sync()?;
        complete_publication()
    })();
    if let Err(error) = operation {
        let mut cleanup_errors = Vec::new();
        for artifact in published.iter().rev() {
            if let Err(cleanup_error) = withdraw_owned_link(&root, artifact) {
                cleanup_errors.push(cleanup_error);
            }
        }
        if let Err(cleanup_error) = staging.remove_owned_staging() {
            cleanup_errors.push(cleanup_error);
        }
        if let Err(cleanup_error) = root.sync() {
            cleanup_errors.push(cleanup_error);
        }
        return Err(if cleanup_errors.is_empty() {
            error
        } else {
            format!(
                "{error}; DSH publication cleanup failed: {}",
                cleanup_errors.join("; ")
            )
        });
    }
    Ok(())
}

#[cfg(unix)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(unix)]
fn component(name: &str) -> Result<std::ffi::CString, String> {
    std::ffi::CString::new(name).map_err(|error| format!("DSH publication component: {error}"))
}

#[cfg(unix)]
fn open_staged_file(directory: &OwnedDirectory, name: &str) -> Result<File, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = component(name)?;
    // SAFETY: the directory descriptor is live; name is a fixed, single
    // component, and O_NOFOLLOW refuses a replaced symlink.
    let fd = unsafe {
        libc::openat(
            directory.file.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(format!(
            "open staged DSH sidecar: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: openat returned a fresh descriptor owned by this function.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("staged DSH sidecar is not a regular file".to_string());
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_staged_file(directory: &OwnedDirectory, name: &str) -> Result<File, String> {
    File::open(directory.path.join(name))
        .map_err(|error| format!("open staged DSH sidecar: {error}"))
}

#[cfg(unix)]
fn link_exclusive(
    staging: &OwnedDirectory,
    root: &OwnedDirectory,
    name: &str,
) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let name_c = component(name)?;
    // SAFETY: both descriptors are live and name is one fixed component.
    // linkat has no replace flag: every existing target is preserved.
    if unsafe {
        libc::linkat(
            staging.file.as_raw_fd(),
            name_c.as_ptr(),
            root.file.as_raw_fd(),
            name_c.as_ptr(),
            0,
        )
    } != 0
    {
        return Err(format!(
            "publish DSH sidecar {name}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn link_exclusive(
    staging: &OwnedDirectory,
    root: &OwnedDirectory,
    name: &str,
) -> Result<(), String> {
    std::fs::hard_link(staging.path.join(name), root.path.join(name))
        .map_err(|error| format!("publish DSH sidecar {name}: {error}"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn withdraw_owned_link(root: &OwnedDirectory, artifact: &PublishedSidecar) -> Result<(), String> {
    withdraw_owned_link_after_capture(root, artifact, || {})
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn withdraw_owned_link_after_capture(
    root: &OwnedDirectory,
    artifact: &PublishedSidecar,
    after_capture: impl FnOnce(),
) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let original = component(artifact.name)?;
    let captured = component(&format!(
        ".dsh-withdraw-{}",
        uuid::Uuid::new_v4().as_simple()
    ))?;
    match rename_no_replace(&root.file, &original, &captured) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "capture DSH sidecar {} for cleanup: {error}",
                artifact.name
            ))
        }
    }
    after_capture();
    let inspected = open_staged_file(root, captured.to_str().map_err(|error| error.to_string())?)
        .and_then(|file| {
            let observed = file.metadata().map_err(|error| error.to_string())?;
            let expected = artifact
                .source
                .metadata()
                .map_err(|error| error.to_string())?;
            Ok(same_file(&observed, &expected))
        });
    let reason = match inspected {
        Ok(true) => {
            // SAFETY: captured names the inode verified above under the pinned
            // directory; unlinkat never follows its final component.
            if unsafe { libc::unlinkat(root.file.as_raw_fd(), captured.as_ptr(), 0) } != 0 {
                return Err(format!(
                    "unlink captured DSH sidecar {} (preserved as {}): {}",
                    artifact.name,
                    captured.to_string_lossy(),
                    std::io::Error::last_os_error()
                ));
            }
            return root.sync();
        }
        Ok(false) => "DSH sidecar ownership changed".to_string(),
        Err(error) => error,
    };
    match rename_no_replace(&root.file, &captured, &original) {
        Ok(()) => Err(format!("{reason}; preserved {}", artifact.name)),
        Err(restore_error) => Err(format!(
            "{reason}; restore failed: {restore_error}; preserved as {}",
            captured.to_string_lossy()
        )),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn withdraw_owned_link(_root: &OwnedDirectory, artifact: &PublishedSidecar) -> Result<(), String> {
    // Successful portable hard-link publication is retained. An error path may
    // not guess inode ownership or delete another writer's replacement.
    let pinned_source_is_file = artifact
        .source
        .metadata()
        .map_err(|error| format!("inspect pinned DSH sidecar {}: {error}", artifact.name))?
        .is_file();
    Err(format!("cannot safely withdraw DSH sidecar {}: atomic ownership capture is unsupported on this platform (pinned_regular_file={pinned_source_is_file})", artifact.name))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rename_no_replace(
    directory: &File,
    from: &std::ffi::CString,
    to: &std::ffi::CString,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor is live and both names are single components.
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            directory.as_raw_fd(),
            from.as_ptr(),
            directory.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            directory.as_raw_fd(),
            from.as_ptr(),
            directory.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn lease_completion_failure_withdraws_result_and_both_channels() {
        let temp = tempfile::tempdir().expect("publication fixture");
        let error = publish_report(
            temp.path(),
            Some(b"events"),
            Some(b"stderr"),
            b"result",
            false,
            || {
                for name in ["result.md", "dsh-events.jsonl", "dsh-stderr.log"] {
                    assert!(
                        temp.path().join(name).is_file(),
                        "linked before completion: {name}"
                    );
                }
                assert_eq!(
                    std::fs::read_dir(temp.path()).expect("entries").count(),
                    3,
                    "private staging removed before completing the lease"
                );
                Err("injected lease completion failure".to_string())
            },
        )
        .expect_err("failed completion must withdraw every owned artifact");
        assert!(
            error.contains("injected lease completion failure"),
            "{error}"
        );
        assert_eq!(std::fs::read_dir(temp.path()).expect("entries").count(), 0);
    }

    #[test]
    fn result_publication_preserves_foreign_same_byte_blocker() {
        let temp = tempfile::tempdir().expect("publication fixture");
        let target = temp.path().join("result.md");
        std::fs::write(&target, b"result").expect("foreign blocker");
        let before = std::fs::metadata(&target).expect("foreign identity");
        let error = publish_report(
            temp.path(),
            Some(b"events"),
            Some(b"stderr"),
            b"result",
            false,
            || panic!("blocked result must never complete the lease"),
        )
        .expect_err("exclusive result link refuses the existing object");
        assert!(error.contains("result.md"), "{error}");
        assert!(same_file(
            &before,
            &std::fs::metadata(&target).expect("preserved object")
        ));
        assert_eq!(std::fs::read(&target).expect("preserved bytes"), b"result");
        assert_eq!(std::fs::read_dir(temp.path()).expect("entries").count(), 1);
    }

    #[test]
    fn failed_completion_preserves_foreign_same_byte_result_replacement() {
        let temp = tempfile::tempdir().expect("publication fixture");
        let target = temp.path().join("result.md");
        let mut replacement_identity = None;
        let error = publish_report(
            temp.path(),
            Some(b"events"),
            Some(b"stderr"),
            b"result",
            false,
            || {
                crate::utils::write_owner_only_file_atomic(&target, b"result")
                    .expect("foreign replacement");
                replacement_identity =
                    Some(std::fs::metadata(&target).expect("replacement identity"));
                Err("injected lease completion failure".to_string())
            },
        )
        .expect_err("failed completion with foreign replacement");
        assert!(error.contains("ownership changed"), "{error}");
        assert!(same_file(
            &replacement_identity.expect("replacement observed"),
            &std::fs::metadata(&target).expect("foreign result restored")
        ));
        assert_eq!(std::fs::read(&target).expect("foreign bytes"), b"result");
        assert_eq!(std::fs::read_dir(temp.path()).expect("entries").count(), 1);
    }

    #[test]
    fn result_withdrawal_preserves_new_public_object_after_capture() {
        let temp = tempfile::tempdir().expect("publication fixture");
        let root =
            OwnedDirectory::open(temp.path().canonicalize().expect("root")).expect("pinned root");
        let source_path = root.path.join("owned-source");
        std::fs::write(&source_path, b"result").expect("owned source");
        let artifact = PublishedSidecar {
            name: "result.md",
            source: File::open(&source_path).expect("pin source"),
        };
        let target = root.path.join(artifact.name);
        std::fs::hard_link(&source_path, &target).expect("owned public link");
        let mut foreign_identity = None;
        withdraw_owned_link_after_capture(&root, &artifact, || {
            std::fs::write(&target, b"result")
                .expect("foreign writer fills public pathname after capture");
            foreign_identity = Some(std::fs::metadata(&target).expect("foreign identity"));
        })
        .expect("withdraw captured owned object");
        assert!(same_file(
            &foreign_identity.expect("foreign observed"),
            &std::fs::metadata(&target).expect("foreign object remains")
        ));
        assert_eq!(std::fs::read(&target).expect("foreign bytes"), b"result");
    }

    #[test]
    fn withdrawal_preserves_foreign_inode_even_with_identical_bytes() {
        let temp = tempfile::tempdir().expect("owned publication directory");
        let root = OwnedDirectory::open(temp.path().canonicalize().expect("canonical root"))
            .expect("pinned publication directory");
        let source_path = root.path.join("owned-source");
        std::fs::write(&source_path, b"identical bytes").expect("owned source");
        let artifact = PublishedSidecar {
            name: "dsh-events.jsonl",
            source: File::open(&source_path).expect("pinned owned source"),
        };
        let target = root.path.join(artifact.name);
        std::fs::hard_link(&source_path, &target).expect("owned public link");
        std::fs::remove_file(&target).expect("fixture removes own link");
        std::fs::write(&target, b"identical bytes").expect("replacement owned by another writer");
        let foreign_identity = std::fs::symlink_metadata(&target).expect("foreign identity");
        let error = withdraw_owned_link(&root, &artifact)
            .expect_err("identical content cannot prove inode ownership");
        assert!(error.contains("ownership changed"), "{error}");
        assert!(same_file(
            &foreign_identity,
            &std::fs::symlink_metadata(&target).expect("foreign object restored")
        ));
        assert_eq!(
            std::fs::read(&target).expect("foreign contents preserved"),
            b"identical bytes"
        );
        assert!(!std::fs::read_dir(&root.path)
            .expect("root entries")
            .any(|entry| entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".dsh-withdraw-")));
    }

    /// #2029 review: an artifact published under the same lease (the run's
    /// result) that fails before commit must withdraw both sidecars.
    #[test]
    fn before_commit_failure_withdraws_every_linked_sidecar() {
        let temp = tempfile::tempdir().expect("publication fixture");
        let mut observed = Vec::new();
        let error = publish(temp.path(), b"events", Some(b"stderr"), false, || {
            observed = ["dsh-events.jsonl", "dsh-stderr.log"]
                .iter()
                .map(|name| temp.path().join(name).is_file())
                .collect();
            Err("injected result persistence failure".to_string())
        })
        .expect_err("hook failure fails publication");
        assert!(
            error.contains("injected result persistence failure"),
            "{error}"
        );
        assert_eq!(observed, vec![true, true], "hook runs after both links");
        let leftovers: Vec<_> = std::fs::read_dir(temp.path())
            .expect("run dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert!(
            leftovers.is_empty(),
            "nothing remains public: {leftovers:?}"
        );
    }

    #[test]
    fn publication_preserves_preexisting_symlink_and_its_target() {
        let temp = tempfile::tempdir().expect("publication fixture");
        let sentinel = temp.path().join("sentinel");
        std::fs::write(&sentinel, b"unrelated data").expect("sentinel");
        let target = temp.path().join("dsh-stderr.log");
        std::os::unix::fs::symlink(&sentinel, &target).expect("foreign symlink blocker");
        let error = publish(
            temp.path(),
            b"new stdout",
            Some(b"new stderr"),
            false,
            || Ok(()),
        )
        .expect_err("exclusive publication must preserve existing symlink");
        assert!(error.contains("dsh-stderr.log"), "{error}");
        assert_eq!(
            std::fs::read_link(&target).expect("symlink preserved"),
            sentinel
        );
        assert_eq!(
            std::fs::read(&sentinel).expect("target untouched"),
            b"unrelated data"
        );
        assert!(!temp.path().join("dsh-events.jsonl").exists());
    }
}
