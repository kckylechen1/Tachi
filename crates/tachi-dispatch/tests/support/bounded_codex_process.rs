//! Test-only owner for a bounded real-binary experiment. The production kernel
//! escape guard supplies process containment, never filesystem confinement.
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub struct Observation {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
    pub refusal: Option<&'static str>,
    pub group_absence_confirmed: bool,
}

struct Owner {
    child: Option<Child>,
    status: Option<ExitStatus>,
}

fn group_absent(pid: u32) -> bool {
    // SAFETY: signal zero only observes the experiment's original process group.
    (unsafe { libc::kill(-(pid as libc::pid_t), 0) }) != 0
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn exited(pid: u32) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // Reserve the group identity until the cleanup signal has been attempted.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

impl Owner {
    fn clean(&mut self, deadline: Instant) -> io::Result<Option<ExitStatus>> {
        let child = self.child.as_mut().expect("owned process");
        let pid = child.id();
        if self.status.is_none() {
            // SAFETY: only the unreaped leader reserves signalling authority.
            // Zombie-only groups may return EPERM; only reap + ESRCH below
            // can prove termination. A surviving group never becomes PASS.
            let _ = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        }
        loop {
            if self.status.is_none() {
                self.status = child.try_wait()?;
            }
            if self.status.is_some() && group_absent(pid) {
                return Ok(self.status);
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            // Keep the reap state on abnormal cleanup: after reap, the reaper
            // may observe but must never signal a reused numeric PGID.
            let mut retained = Self {
                child: Some(child),
                status: self.status,
            };
            std::thread::spawn(move || loop {
                if matches!(
                    retained.clean(Instant::now() + Duration::from_secs(1)),
                    Ok(Some(_))
                ) {
                    retained.child.take();
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            });
        }
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: both calls use this live owned pipe and scalar flags only.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>, cap: usize) -> io::Result<bool> {
    let mut chunk = [0; 4096];
    // Fairness: a continuously writing child cannot starve the deadline check.
    for _ in 0..16 {
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                if bytes.len() + n > cap {
                    return Err(io::Error::other("output limit"));
                }
                bytes.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

pub fn run(mut command: Command, timeout: Duration, cap: usize) -> io::Result<Observation> {
    if !tachi_dispatch::configure_process_group_escape_containment(&mut command) {
        return Err(io::Error::other("kernel process containment unavailable"));
    }
    // The calling test process already has the isolated whitelisted environment.
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut owner = Owner {
        child: Some(command.spawn()?),
        status: None,
    };
    let child = owner.child.as_mut().expect("owned child");
    let mut out = child.stdout.take().expect("stdout pipe");
    let mut err = child.stderr.take().expect("stderr pipe");
    nonblocking(&out)?;
    nonblocking(&err)?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let deadline = Instant::now() + timeout;
    let mut refusal = None;
    loop {
        if drain(&mut out, &mut stdout, cap).is_err() || drain(&mut err, &mut stderr, cap).is_err()
        {
            refusal = Some("output bound or pipe failure");
            break;
        }
        if exited(child.id())? {
            break;
        }
        if Instant::now() >= deadline {
            refusal = Some("deadline exceeded");
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = owner
        .clean(Instant::now() + Duration::from_secs(3))?
        .ok_or_else(|| io::Error::other("cleanup unconfirmed; ownership retained by reaper"))?;
    owner.child.take(); // Already reaped: never signal this numeric PGID again.
    let out_end = drain(&mut out, &mut stdout, cap);
    let err_end = drain(&mut err, &mut stderr, cap);
    if !matches!((out_end, err_end), (Ok(true), Ok(true))) {
        refusal = Some("output bound or missing EOF after cleanup");
    }
    Ok(Observation {
        stdout,
        stderr,
        status,
        refusal,
        group_absence_confirmed: true,
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }
    #[test]
    fn successful_child_and_both_pipes_are_observed() {
        let result = run(
            shell("printf out; printf err >&2"),
            Duration::from_secs(2),
            1024,
        )
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.refusal, None);
        assert_eq!(result.stdout, b"out");
        assert_eq!(result.stderr, b"err");
        assert!(result.group_absence_confirmed);
    }
    #[test]
    fn deadline_terminates_descendants_and_confirms_group_absence() {
        let start = Instant::now();
        let result = run(shell("sleep 30 & wait"), Duration::from_millis(150), 1024).unwrap();
        assert_eq!(result.refusal, Some("deadline exceeded"));
        assert!(!result.status.success());
        assert!(result.group_absence_confirmed);
        assert!(start.elapsed() < Duration::from_secs(4));
    }
    #[test]
    fn root_exit_cannot_leave_a_pipe_holding_descendant() {
        let result = run(shell("sleep 30 & exit 0"), Duration::from_secs(2), 1024).unwrap();
        assert!(result.status.success());
        assert_eq!(result.refusal, None);
        assert!(result.group_absence_confirmed);
    }
    #[test]
    fn flooding_either_stream_is_bounded_and_reaped() {
        for script in [
            "while :; do printf 0123456789; done",
            "while :; do printf 0123456789 >&2; done",
        ] {
            let result = run(shell(script), Duration::from_secs(2), 4096).unwrap();
            assert!(result.refusal.is_some());
            assert!(result.group_absence_confirmed);
            assert!(result.stdout.len() <= 4096);
            assert!(result.stderr.len() <= 4096);
        }
    }
}
