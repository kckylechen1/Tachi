//! Linux children for `required_postflight_kernel_containment_discriminates_setsid_escape`
//! (tachi#1978, ported from the closed #1879 branch). Compiled only where the
//! Required-postflight seccomp filter exists (Linux x86_64/aarch64); the macOS
//! parent/child pair in `subprocess.rs` is untouched.
use super::*;
use std::os::unix::process::ExitStatusExt;

pub(super) const CONTAINMENT_CHILD_TEST_NAME: &str =
    "dispatch_ops::subprocess::required_postflight_child_tests::required_postflight_containment_child_cannot_setsid";
pub(super) const DESCENDANT_PROBE_TEST_NAME: &str =
    "dispatch_ops::subprocess::required_postflight_child_tests::required_postflight_non_group_leader_descendant_probes_setsid";

const DESCENDANT_SETID_DENIED: i32 = 0;
const DESCENDANT_SETID_ALLOWED: i32 = 1;
const DESCENDANT_NOT_NON_GROUP_LEADER: i32 = 2;
const DESCENDANT_WRONG_INHERITED_GROUP: i32 = 3;
const DESCENDANT_UNEXPECTED_SUCCESS: i32 = 4;
const DESCENDANT_WRONG_ERRNO: i32 = 5;
const DESCENDANT_WRONG_GROUP_AFTER_DENIAL: i32 = 6;
const DESCENDANT_WRONG_SETID_RESULT: i32 = 7;
const DESCENDANT_WRONG_GROUP_AFTER_SETID: i32 = 8;

pub(super) async fn assert_descendant_setsid_discrimination() {
    let mut descendant_command =
        Command::new(std::env::current_exe().expect("current test binary"));
    descendant_command
        .arg(DESCENDANT_PROBE_TEST_NAME)
        .arg("--exact")
        .arg("--nocapture")
        .env("TACHI_TEST_REQUIRED_POSTFLIGHT_DESCENDANT_MODE", "deny");
    let contained_descendant =
        run_agent_subprocess_with_liveness(descendant_command, Duration::from_secs(10), true, None)
            .await;
    let result = contained_descendant
        .result
        .expect("contained non-leader descendant probe must pass");
    assert_eq!(result.exit_code, Some(0));
    assert!(matches!(
        contained_descendant.liveness,
        crate::exec_env_postflight::RunnerLivenessEvidence::ConfirmedReaped {
            proof: "kernel_denied_process_group_escape_and_owned_group_absent"
        }
    ));

    let mut control_command = Command::new(std::env::current_exe().expect("current test binary"));
    control_command
        .arg(DESCENDANT_PROBE_TEST_NAME)
        .arg("--exact")
        .arg("--nocapture")
        .env("TACHI_TEST_REQUIRED_POSTFLIGHT_DESCENDANT_MODE", "allow");
    let uncontained_control =
        run_agent_subprocess_with_liveness(control_command, Duration::from_secs(10), false, None)
            .await;
    let result = uncontained_control
        .result
        .expect("uncontained non-leader descendant control must pass");
    assert_eq!(result.exit_code, Some(0));
    assert!(matches!(
        uncontained_control.liveness,
        crate::exec_env_postflight::RunnerLivenessEvidence::Indeterminate { detail }
            if detail.contains("could have escaped it with setsid()")
    ));
}

fn current_errno() -> libc::c_int {
    // SAFETY: libc exposes the calling thread's errno location; this reads
    // the value immediately after the child-only setsid/waitpid syscall.
    unsafe { *libc::__errno_location() }
}

#[test]
fn required_postflight_containment_child_cannot_setsid() {
    if std::env::var_os("TACHI_TEST_ATTEMPT_SETSID").is_none() {
        return;
    }
    // `process_group(0)` makes the re-exec'd runner's root a process-group
    // leader. This is the frozen baseline EPERM assertion; it is deliberately
    // supplemented by the non-leader descendant probe below.
    let pid = unsafe { libc::getpid() };
    let pgid = unsafe { libc::getpgrp() };
    assert_eq!(pid, pgid, "required probe root must be its group leader");
    // SAFETY: setsid takes no pointers. The required-postflight sandbox must
    // deny this process-control operation with EPERM.
    let result = unsafe { libc::setsid() };
    assert_eq!(result, -1, "required worker unexpectedly escaped its group");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    );
}

#[test]
fn required_postflight_non_group_leader_descendant_probes_setsid() {
    let Some(mode) = std::env::var_os("TACHI_TEST_REQUIRED_POSTFLIGHT_DESCENDANT_MODE") else {
        return;
    };
    let expect_denied = match mode.to_str() {
        Some("deny") => true,
        Some("allow") => false,
        _ => panic!("invalid required-postflight descendant probe mode"),
    };

    let leader_pid = unsafe { libc::getpid() };
    let leader_pgid = unsafe { libc::getpgrp() };
    assert_eq!(
        leader_pid, leader_pgid,
        "production runner must place the re-exec root in its own group"
    );

    // SAFETY: the child branch below performs only libc getpid/getpgrp,
    // setsid, errno inspection, and _exit. It does not allocate, lock, panic,
    // touch Rust-managed state, or return through Rust after fork.
    let descendant_pid = unsafe { libc::fork() };
    assert!(descendant_pid >= 0, "fork descendant probe failed");
    if descendant_pid == 0 {
        unsafe { probe_descendant_setsid(expect_denied, leader_pgid) };
    }

    let code = reap_descendant(descendant_pid);
    let expected = if expect_denied {
        DESCENDANT_SETID_DENIED
    } else {
        DESCENDANT_SETID_ALLOWED
    };
    assert_eq!(
        code, expected,
        "descendant probe must prove the non-group-leader PID/PGID invariant and expected setsid result"
    );
}

unsafe fn probe_descendant_setsid(expect_denied: bool, leader_pgid: libc::pid_t) -> ! {
    let descendant_pid = unsafe { libc::getpid() };
    let descendant_pgid = unsafe { libc::getpgrp() };
    if descendant_pid == descendant_pgid {
        unsafe { libc::_exit(DESCENDANT_NOT_NON_GROUP_LEADER) };
    }
    if descendant_pgid != leader_pgid {
        unsafe { libc::_exit(DESCENDANT_WRONG_INHERITED_GROUP) };
    }

    let result = unsafe { libc::setsid() };
    let errno = if result == -1 { current_errno() } else { 0 };
    if expect_denied {
        if result != -1 {
            unsafe { libc::_exit(DESCENDANT_UNEXPECTED_SUCCESS) };
        }
        if errno != libc::EPERM {
            unsafe { libc::_exit(DESCENDANT_WRONG_ERRNO) };
        }
        if unsafe { libc::getpgrp() } != descendant_pgid {
            unsafe { libc::_exit(DESCENDANT_WRONG_GROUP_AFTER_DENIAL) };
        }
        unsafe { libc::_exit(DESCENDANT_SETID_DENIED) };
    }

    if result != descendant_pid {
        unsafe { libc::_exit(DESCENDANT_WRONG_SETID_RESULT) };
    }
    if unsafe { libc::getpgrp() } != descendant_pid {
        unsafe { libc::_exit(DESCENDANT_WRONG_GROUP_AFTER_SETID) };
    }
    unsafe { libc::_exit(DESCENDANT_SETID_ALLOWED) };
}

fn reap_descendant(descendant_pid: libc::pid_t) -> i32 {
    let mut status = 0;
    loop {
        // SAFETY: this parent owns the exact PID returned by fork and waits
        // synchronously, so the descendant is reaped before the root test
        // exits and before the production runner can publish liveness.
        let waited = unsafe { libc::waitpid(descendant_pid, &mut status, 0) };
        if waited == descendant_pid {
            break;
        }
        if waited == -1 && current_errno() == libc::EINTR {
            continue;
        }
        panic!("required-postflight descendant waitpid failed");
    }
    let status = std::process::ExitStatus::from_raw(status);
    status
        .code()
        .unwrap_or_else(|| panic!("required-postflight descendant exited abnormally: {status:?}"))
}
