//! Kernel-enforced containment for owned POSIX process groups.
//!
//! A process-group absence proof is authoritative only when descendants cannot
//! leave that group before cleanup. Both the managed-run kernel and prerequisite
//! probes use this helper before spawn and fail closed where it is unavailable.

/// Prevent the command and its descendants from using process-control syscalls
/// to escape the POSIX process group owned by the caller.
///
/// Callers configure stdio and `process_group(0)` after this function. On
/// supported Linux ABIs a seccomp filter inherited across fork/exec denies
/// group/session escape. macOS currently has no verified containment route.
#[cfg(target_os = "macos")]
pub fn configure_process_group_escape_containment(_command: &mut std::process::Command) -> bool {
    // The former `(deny process-info-setcontrol)` sandbox-exec profile did not
    // deny setsid() in a nonleader child. A process-group absence probe cannot
    // prove descendant termination when that child can leave the group.
    false
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub fn configure_process_group_escape_containment(command: &mut std::process::Command) -> bool {
    use std::os::unix::process::CommandExt;

    // SAFETY: the closure runs after fork and before exec. It performs only
    // prctl syscalls plus stack-local BPF construction; no allocation or lock
    // is touched in the child. The filter is inherited across fork/clone and
    // exec, so descendants cannot later leave the caller-owned process group.
    unsafe {
        command.pre_exec(install_linux_process_group_escape_filter);
    }
    true
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn install_linux_process_group_escape_filter() -> std::io::Result<()> {
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_JMP_JSET_K: u16 = 0x45;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
    #[cfg(target_arch = "x86_64")]
    const NATIVE_AUDIT_ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const NATIVE_AUDIT_ARCH: u32 = 0xc000_00b7;

    const fn stmt(code: u16, k: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        }
    }
    const fn deny_if(syscall: u32) -> [libc::sock_filter; 2] {
        [
            libc::sock_filter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 1,
                k: syscall,
            },
            stmt(BPF_RET_K, SECCOMP_RET_ERRNO | libc::EPERM as u32),
        ]
    }

    let setsid = deny_if(libc::SYS_setsid as u32);
    let setpgid = deny_if(libc::SYS_setpgid as u32);
    let unshare = deny_if(libc::SYS_unshare as u32);
    let setns = deny_if(libc::SYS_setns as u32);
    let mut filter = [
        // seccomp_data.arch is at byte offset 4. Refuse compatibility ABIs,
        // whose syscall numbers would otherwise bypass the native rules.
        stmt(BPF_LD_W_ABS, 4),
        libc::sock_filter {
            code: BPF_JMP_JEQ_K,
            jt: 1,
            jf: 0,
            k: NATIVE_AUDIT_ARCH,
        },
        stmt(BPF_RET_K, SECCOMP_RET_ERRNO | libc::EPERM as u32),
        stmt(BPF_LD_W_ABS, 0),
        // x86_64's x32 ABI ORs syscall numbers with 0x4000_0000. Deny that
        // alternate ABI wholesale for the same reason.
        libc::sock_filter {
            code: BPF_JMP_JSET_K,
            jt: 0,
            jf: 1,
            k: 0x4000_0000,
        },
        stmt(BPF_RET_K, SECCOMP_RET_ERRNO | libc::EPERM as u32),
        setsid[0],
        setsid[1],
        setpgid[0],
        setpgid[1],
        unshare[0],
        unshare[1],
        setns[0],
        setns[1],
        stmt(BPF_RET_K, SECCOMP_RET_ALLOW),
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: prctl receives scalar options and a pointer to the stack-resident
    // filter for the duration of the syscall.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: no_new_privs is set and `program` names a valid classic-BPF
    // array. Failure aborts spawn rather than running uncontained.
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER,
            &program as *const libc::sock_fprog,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
pub fn configure_process_group_escape_containment(_command: &mut std::process::Command) -> bool {
    false
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn configure_process_group_escape_containment(_command: &mut std::process::Command) -> bool {
    false
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn macos_containment_refuses_a_nonleader_setsid_escape() {
        if std::env::var_os("TACHI_CONTAINMENT_CHILD_PROBE").is_some() {
            // SAFETY: the forked child calls only async-signal-safe syscalls,
            // then exits immediately. A successful setsid is the regression.
            let child = unsafe { libc::fork() };
            assert!(child >= 0, "fork failed");
            if child == 0 {
                let escaped = unsafe { libc::setsid() } >= 0;
                unsafe { libc::_exit(if escaped { 42 } else { 0 }) };
            }
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
            assert_eq!(status, 0, "nonleader child escaped its process group");
            return;
        }

        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "containment::tests::macos_containment_refuses_a_nonleader_setsid_escape",
                "--nocapture",
            ])
            .env("TACHI_CONTAINMENT_CHILD_PROBE", "1");
        if !configure_process_group_escape_containment(&mut command) {
            return; // No claimed containment means no uncontained spawn.
        }
        let output = command.process_group(0).output().unwrap();
        assert!(
            output.status.success(),
            "claimed containment allowed setsid: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
