use super::*;
use std::os::windows::fs::OpenOptionsExt;
use std::process::{Child, Command, Stdio};
use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES;
use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_MOUNT_POINT;
use windows_sys::Win32::System::IO::DeviceIoControl;

fn run_directory() -> (tempfile::TempDir, PathBuf) {
    let scratch = tempfile::tempdir().expect("native NTFS test directory");
    let run = scratch.path().join("parent").join("run");
    std::fs::create_dir_all(&run).unwrap();
    (scratch, run)
}

fn write_private_fixture(anchor: &AnchoredRunStatus, name: &str, bytes: &[u8], sddl: &str) {
    let security = PrivateSecurity::from_sddl(sddl).unwrap();
    let mut file = open_relative(
        anchor.directory.directory(),
        OsStr::new(name),
        GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
        Some(&security),
    )
    .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn durable_status_and_owner_binding_survive_reopen() {
    let (_scratch, run) = run_directory();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    assert!(anchor.read_json().unwrap().is_none());
    assert!(anchor.result_report_len().unwrap().is_none());
    assert!(anchor.read_mcp_binding().unwrap().is_none());
    let lock = anchor.lock();
    let guard = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    anchor
        .write_atomic(br#"{"status":"completed"}"#, &fence)
        .unwrap();
    anchor
        .write_mcp_binding(br#"{"owner":"native-owner"}"#, &fence)
        .unwrap();
    anchor
        .write_mcp_binding(br#"{"owner":"native-owner","revision":2}"#, &fence)
        .unwrap();
    assert_eq!(anchor.status_path(), run.join("status.json"));
    drop(fence);
    drop(guard);
    drop(anchor);
    let reopened = AnchoredRunStatus::open(&run).unwrap();
    assert_eq!(
        reopened.read_json().unwrap().unwrap()["status"],
        "completed"
    );
    assert_eq!(
        reopened.read_mcp_binding().unwrap().as_deref(),
        Some(r#"{"owner":"native-owner","revision":2}"#)
    );
    let binding = open_relative(
        reopened.directory.directory(),
        OsStr::new("mcp-tasks.json"),
        GENERIC_READ | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        None,
    )
    .unwrap();
    validate_private(&binding).unwrap();
}

#[test]
fn physical_directory_mutex_is_shared_but_fences_need_the_exact_anchor() {
    let (_scratch, run) = run_directory();
    let first = AnchoredRunStatus::open(&run).unwrap();
    let reopened = AnchoredRunStatus::open_ordinary(&run).unwrap();
    let cloned = first.clone();
    assert!(Arc::ptr_eq(&first.lock(), &reopened.lock()));
    assert!(first.same_opened_directory(&cloned));
    assert!(!first.same_opened_directory(&reopened));
    let lock = first.lock();
    let _guard = lock.lock().unwrap();
    let fence = first.acquire_fence().unwrap();
    assert!(reopened.write_atomic(b"{}", &fence).is_err());
    assert!(first.read_json().unwrap().is_none());
    cloned.write_atomic(b"{}", &fence).unwrap();
}

#[test]
fn replacement_directory_cannot_inherit_a_retained_physical_mutex() {
    let (_scratch, run) = run_directory();
    let original = AnchoredRunStatus::open(&run).unwrap();
    let original_lock = original.lock();
    drop(original);
    std::fs::rename(&run, run.with_file_name("retired")).unwrap();
    std::fs::create_dir(&run).unwrap();
    let replacement = AnchoredRunStatus::open(&run).unwrap();
    assert!(!Arc::ptr_eq(&original_lock, &replacement.lock()));
}

#[test]
fn lexical_parent_components_use_the_validated_handle_stack() {
    let (_scratch, run) = run_directory();
    let original = AnchoredRunStatus::open(&run).unwrap();
    let alias = run.join("..").join("run");
    let reopened = AnchoredRunStatus::open_ordinary(&alias).unwrap();
    assert_eq!(
        reopened.status_path(),
        run.canonicalize().unwrap().join("status.json")
    );
    assert!(Arc::ptr_eq(&original.lock(), &reopened.lock()));
    assert!(Arc::ptr_eq(
        &original.lock(),
        &crate::dispatch_ops::status_json_lock_for(&alias)
    ));
    let lock = reopened.lock();
    let _guard = lock.lock().unwrap();
    let fence = reopened.acquire_fence().unwrap();
    reopened.write_atomic(b"{}", &fence).unwrap();
    assert_eq!(original.read_json().unwrap(), Some(serde_json::json!({})));
    let root = run.ancestors().last().unwrap();
    assert!(AnchoredRunStatus::open(&root.join("..")).is_err());
}

#[test]
fn retained_ancestors_prevent_directory_rename_and_swap() {
    let (scratch, run) = run_directory();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let substitute = scratch.path().join("substitute");
    std::fs::create_dir(&substitute).unwrap();
    std::fs::write(substitute.join("status.json"), b"substitute").unwrap();
    assert!(std::fs::rename(&run, run.with_file_name("old")).is_err());
    assert!(std::fs::rename(run.parent().unwrap(), scratch.path().join("old-parent")).is_err());
    assert!(std::fs::rename(&substitute, &run).is_err());
    let lock = anchor.lock();
    let guard = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    anchor
        .write_atomic(br#"{"anchored":true}"#, &fence)
        .unwrap();
    assert_eq!(
        std::fs::read(substitute.join("status.json")).unwrap(),
        b"substitute"
    );
    assert_eq!(anchor.read_json().unwrap().unwrap()["anchored"], true);
    drop(fence);
    drop(guard);
    drop(anchor);
    std::fs::rename(&run, run.with_file_name("old")).unwrap();
    std::fs::rename(&substitute, &run).unwrap();
    assert_eq!(
        std::fs::read(run.join("status.json")).unwrap(),
        b"substitute"
    );
}

/// Build a real NTFS mount-point reparse buffer. Unlike symbolic links,
/// junction creation does not depend on Developer Mode or symlink privileges.
fn convert_to_junction(directory: &Path, target: &Path) -> io::Result<()> {
    let target = target.canonicalize()?;
    let dos_name = target.as_os_str().encode_wide().collect::<Vec<_>>();
    let prefix = r"\\?\".encode_utf16().collect::<Vec<_>>();
    if !dos_name.starts_with(&prefix) {
        return Err(io::Error::other("junction fixture needs a local DOS path"));
    }
    let substitute: Vec<u16> = r"\??\"
        .encode_utf16()
        .chain(dos_name[4..].iter().copied())
        .collect();
    let print_name = &dos_name[4..];
    let substitute_bytes = (substitute.len() * 2) as u16;
    let print_bytes = (print_name.len() * 2) as u16;
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buffer.extend_from_slice(&(8 + substitute_bytes + 2 + print_bytes + 2).to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    buffer.extend_from_slice(&substitute_bytes.to_le_bytes());
    buffer.extend_from_slice(&(substitute_bytes + 2).to_le_bytes());
    buffer.extend_from_slice(&print_bytes.to_le_bytes());
    for value in substitute
        .iter()
        .chain(std::iter::once(&0))
        .chain(print_name)
        .chain(std::iter::once(&0))
    {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory)?;
    let mut returned = 0;
    // SAFETY: the validated mount-point buffer and returned byte count live
    // through the synchronous call; the handle refers to the directory itself.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            buffer.len() as u32,
            null_mut(),
            0,
            &mut returned,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[test]
fn junctions_in_final_or_ancestor_components_are_refused() {
    let (scratch, run) = run_directory();
    let alias = scratch.path().join("alias");
    std::fs::create_dir(&alias).unwrap();
    convert_to_junction(&alias, run.parent().unwrap()).unwrap();
    assert!(AnchoredRunStatus::open(&alias).is_err());
    assert!(AnchoredRunStatus::open(&alias.join("run")).is_err());
    assert!(AnchoredRunStatus::open_ordinary(&alias.join("run")).is_err());
    assert!(AnchoredRunStatus::open(&alias.join("..").join("parent").join("run")).is_err());
    std::fs::remove_dir(alias).unwrap();
}

#[test]
fn in_place_junction_conversion_cannot_redirect_an_existing_anchor() {
    let (scratch, run) = run_directory();
    let outside = scratch.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("status.json"), br#"{"outside":true}"#).unwrap();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    // FILE_WRITE_ATTRIBUTES deliberately exercises the case that delete-sharing
    // restrictions alone cannot exclude: conversion of the same empty object.
    convert_to_junction(&run, &outside).unwrap();
    assert!(anchor.read_json().is_err());
    assert!(anchor.acquire_fence().is_err());
    // Exercise the operation after validation directly: a directory changed
    // in the validate/open gap must still fail inside NtCreateFile itself.
    let security = PrivateSecurity::current().unwrap();
    assert!(open_relative(
        anchor.directory.directory(),
        OsStr::new(FENCE_NAME),
        GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        FILE_OPEN_IF,
        FILE_NON_DIRECTORY_FILE,
        Some(&security),
    )
    .is_err());
    assert!(!outside.join(FENCE_NAME).exists());
    assert_eq!(
        std::fs::read(outside.join("status.json")).unwrap(),
        br#"{"outside":true}"#
    );
    drop(anchor);
    std::fs::remove_dir(&run).unwrap();
}

#[test]
fn private_binding_and_fence_refuse_hardlinks_without_writing() {
    let (scratch, run) = run_directory();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    anchor.write_mcp_binding(b"private", &fence).unwrap();
    let outside_binding = scratch.path().join("binding-link");
    std::fs::hard_link(run.join("mcp-tasks.json"), &outside_binding).unwrap();
    assert!(anchor.read_mcp_binding().is_err());
    assert!(anchor.write_mcp_binding(b"replacement", &fence).is_err());
    assert_eq!(std::fs::read(&outside_binding).unwrap(), b"private");
    drop(fence);
    let outside_fence = scratch.path().join("fence-link");
    std::fs::hard_link(run.join(FENCE_NAME), &outside_fence).unwrap();
    assert!(anchor.acquire_fence().is_err());
    assert_eq!(std::fs::metadata(outside_fence).unwrap().len(), 0);
}

#[test]
fn every_fixed_artifact_refuses_a_symlink_to_an_otherwise_admitted_private_file() {
    let (scratch, run) = run_directory();
    let outside = scratch.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let outside_anchor = AnchoredRunStatus::open(&outside).unwrap();
    let owner = UserSid::current().unwrap().text().unwrap();
    write_private_fixture(
        &outside_anchor,
        "private.json",
        b"{}",
        &format!("O:{owner}D:P(A;;FA;;;{owner})"),
    );
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    for name in ["status.json", "mcp-tasks.json", "result.md", FENCE_NAME] {
        std::os::windows::fs::symlink_file(outside.join("private.json"), run.join(name))
            .expect("native Windows security tests require symlink privilege or Developer Mode");
        let refused = match name {
            "status.json" => anchor.read_json().is_err(),
            "mcp-tasks.json" => anchor.read_mcp_binding().is_err(),
            "result.md" => {
                anchor.read_mcp_result_report().is_err() && anchor.result_report_len().is_err()
            }
            _ => anchor.acquire_fence().is_err(),
        };
        assert!(refused, "reparse entry was followed: {name}");
        assert_eq!(std::fs::read(outside.join("private.json")).unwrap(), b"{}");
        std::fs::remove_file(run.join(name)).unwrap();
    }
}

#[test]
fn private_binding_and_fence_refuse_nonprivate_or_inherited_acls() {
    for protected in [true, false] {
        for name in ["mcp-tasks.json", FENCE_NAME] {
            let (_scratch, run) = run_directory();
            let anchor = AnchoredRunStatus::open(&run).unwrap();
            let owner = UserSid::current().unwrap().text().unwrap();
            let sddl = if protected {
                format!("O:{owner}D:P(A;;FA;;;{owner})(A;;FR;;;WD)")
            } else {
                format!("O:{owner}D:(A;;FA;;;{owner})")
            };
            write_private_fixture(&anchor, name, b"unchanged", &sddl);
            if name == FENCE_NAME {
                assert!(anchor.acquire_fence().is_err());
            } else {
                assert!(anchor.read_mcp_binding().is_err());
                let lock = anchor.lock();
                let _guard = lock.lock().unwrap();
                let fence = anchor.acquire_fence().unwrap();
                assert!(anchor.write_mcp_binding(b"replacement", &fence).is_err());
            }
            assert_eq!(std::fs::read(run.join(name)).unwrap(), b"unchanged");
        }
    }
}

#[test]
fn bounded_reads_accept_exact_limits_and_refuse_larger_or_non_utf8_artifacts() {
    let (_scratch, run) = run_directory();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    let mut maximum = vec![b' '; STATUS_LIMIT];
    maximum[..2].copy_from_slice(b"{}");
    anchor.write_atomic(&maximum, &fence).unwrap();
    assert_eq!(anchor.read_json().unwrap(), Some(serde_json::json!({})));
    let snapshot = anchor
        .read_status_with_metadata(STATUS_LIMIT)
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.text.as_bytes(), maximum);
    assert_eq!(
        snapshot.modified,
        Some(
            std::fs::metadata(run.join("status.json"))
                .unwrap()
                .modified()
                .unwrap()
        )
    );
    assert!(anchor.read_json_bounded(STATUS_LIMIT - 1).is_err());
    anchor.write_mcp_binding(&maximum, &fence).unwrap();
    assert_eq!(
        anchor.read_mcp_binding().unwrap().unwrap().len(),
        STATUS_LIMIT
    );
    maximum.push(b' ');
    assert!(anchor.write_atomic(&maximum, &fence).is_err());
    assert!(anchor.write_mcp_binding(&maximum, &fence).is_err());
    assert_eq!(
        std::fs::metadata(run.join("mcp-tasks.json")).unwrap().len(),
        STATUS_LIMIT as u64
    );
    std::fs::write(run.join("status.json"), &maximum).unwrap();
    assert!(anchor.read_json().is_err());
    std::fs::write(run.join("mcp-tasks.json"), &maximum).unwrap();
    assert!(anchor.read_mcp_binding().is_err());
    std::fs::write(run.join("result.md"), vec![b'x'; REPORT_LIMIT]).unwrap();
    assert_eq!(
        anchor.read_mcp_result_report().unwrap().unwrap().len(),
        REPORT_LIMIT
    );
    std::fs::write(run.join("result.md"), vec![b'x'; REPORT_LIMIT + 1]).unwrap();
    assert!(anchor.read_mcp_result_report().is_err());
    assert_eq!(
        anchor.result_report_len().unwrap(),
        Some((REPORT_LIMIT + 1) as u64)
    );
    std::fs::write(run.join("result.md"), [0xff]).unwrap();
    assert!(anchor.read_mcp_result_report().is_err());
    assert_eq!(anchor.result_report_len().unwrap(), Some(1));
    std::fs::remove_file(run.join("result.md")).unwrap();
    std::fs::create_dir(run.join("result.md")).unwrap();
    assert!(anchor.result_report_len().is_err());
}

#[test]
fn atomic_status_replacement_never_writes_through_hardlinks_or_symbolic_links() {
    let (scratch, run) = run_directory();
    let outside = scratch.path().join("outside.json");
    std::fs::write(&outside, b"outside").unwrap();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    std::fs::hard_link(&outside, run.join("status.json")).unwrap();
    anchor.write_atomic(b"{}", &fence).unwrap();
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    assert_eq!(anchor.read_json().unwrap(), Some(serde_json::json!({})));
    std::fs::remove_file(run.join("status.json")).unwrap();
    std::os::windows::fs::symlink_file(&outside, run.join("status.json"))
        .expect("native Windows security tests require symlink privilege or Developer Mode");
    assert!(anchor.read_json().is_err());
    anchor
        .write_atomic(br#"{"replacement":true}"#, &fence)
        .unwrap();
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    assert_eq!(anchor.read_json().unwrap().unwrap()["replacement"], true);
    std::fs::create_dir(run.join("mcp-tasks.json")).unwrap();
    assert!(anchor.read_mcp_binding().is_err());
    assert!(anchor.write_mcp_binding(b"binding", &fence).is_err());
}

#[test]
fn fence_cannot_be_replaced_while_held_and_conflicting_locks_time_out() {
    let (_scratch, run) = run_directory();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    assert!(std::fs::rename(run.join(FENCE_NAME), run.join("old-fence")).is_err());
    assert!(std::fs::remove_file(run.join(FENCE_NAME)).is_err());
    let error = anchor.acquire_fence_until(Instant::now() + Duration::from_millis(50));
    assert!(matches!(error, Err(message) if message.contains("timed out")));
    anchor.write_atomic(b"{}", &fence).unwrap();
    drop(fence);
    std::fs::rename(run.join(FENCE_NAME), run.join("old-fence")).unwrap();
    anchor.acquire_fence().unwrap();
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "native process did not produce {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn fence_child_process() {
    let Some(run) = std::env::var_os("TACHI_WINDOWS_FENCE_CHILD_RUN") else {
        return;
    };
    let run = PathBuf::from(run);
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap();
    let error = anchor.acquire_fence_until(Instant::now() + Duration::from_millis(100));
    assert!(matches!(error, Err(message) if message.contains("timed out")));
    std::fs::write(run.join("child-blocked"), b"blocked").unwrap();
    wait_for_file(&run.join("release-child"));
    let fence = anchor.acquire_fence().unwrap();
    anchor
        .write_atomic(br#"{"writer":"child"}"#, &fence)
        .unwrap();
}

#[test]
fn fence_serializes_real_processes_and_reopens_after_release() {
    let (_scratch, run) = run_directory();
    let anchor = AnchoredRunStatus::open(&run).unwrap();
    let lock = anchor.lock();
    let _guard = lock.lock().unwrap();
    let fence = anchor.acquire_fence().unwrap();
    anchor
        .write_atomic(br#"{"writer":"parent"}"#, &fence)
        .unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "managed_run_control::windows_status::tests::fence_child_process",
            "--nocapture",
        ])
        .env("TACHI_WINDOWS_FENCE_CHILD_RUN", &run)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(Some(child));
    wait_for_file(&run.join("child-blocked"));
    assert_eq!(anchor.read_json().unwrap().unwrap()["writer"], "parent");
    drop(fence);
    std::fs::write(run.join("release-child"), b"go").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "native fence child did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.0.take().unwrap().wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "child stdout: {}\nchild stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(anchor.read_json().unwrap().unwrap()["writer"], "child");
}
