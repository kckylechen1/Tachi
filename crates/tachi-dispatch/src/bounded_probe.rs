//! Bounded probes of vendor processes, for answers the daemon needs before it
//! goes on: run one process to its end or to a deadline, read what it printed
//! without a reader thread, and leave nothing of its process group behind.
//!
//! A probe reaches ordinary descendants. A process that leaves the group with
//! `setsid` or `setpgid` is not reached, and nothing here waits for its end.

use std::io;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The leader of its own process group, and this process's to reap.
///
/// The leader stays unreaped until every group signal has been sent. Reaping
/// it frees its PID, and with it the group's ID, and a signal sent to the
/// group afterwards would reach whoever was given that number next.
pub(crate) struct GroupProcess {
    child: Child,
    owns_root: bool,
}

impl GroupProcess {
    /// Starts `command` as the leader of a new process group.
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        use std::os::unix::process::CommandExt;
        let child = command.process_group(0).spawn()?;
        Ok(Self {
            child,
            owns_root: true,
        })
    }

    pub(crate) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Whether the leader has ended. It is left unreaped, so its PID and its
    /// group's ID stay reserved.
    pub(crate) fn observe_exit(&mut self) -> io::Result<bool> {
        if !self.owns_root {
            return Err(io::Error::other("probe child ownership was lost"));
        }
        // WNOWAIT reserves the child's PID/PGID even after its normal exit.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waits on this process's own child and writes only into `info`.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.owns_root = false;
            }
            return Err(error);
        }
        Ok(unsafe { info.si_pid() } != 0)
    }

    /// Stops and kills the group, then reaps the leader. No group signal
    /// follows the reap. Stopping first closes the window in which a member
    /// forks after the kill has passed it, and the new member is missed.
    pub(crate) fn terminate_and_reap(&mut self) -> io::Result<ExitStatus> {
        if !self.owns_root {
            return Err(io::Error::other("probe child ownership was lost"));
        }
        let _exit_observed = match self.observe_exit() {
            Ok(exited) => exited,
            Err(error) if !self.owns_root => return Err(error),
            Err(_) => false,
        };
        // SAFETY: process_group(0) created this group, and WNOWAIT has not
        // consumed its leader. No termination signal follows an attempt to
        // reap it.
        let pgid = self.child.id() as libc::pid_t;
        unsafe { libc::kill(-pgid, libc::SIGSTOP) };
        let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
        let signal_error = (result != 0)
            .then(io::Error::last_os_error)
            .filter(|error| error.raw_os_error() != Some(libc::ESRCH));
        if signal_error.is_some() {
            let _ = self.child.kill();
        }
        self.owns_root = false;
        let status = self.child.wait();
        #[cfg(target_os = "macos")]
        let signal_error = signal_error.filter(|error| {
            // Darwin skips zombie group members and can return EPERM for a
            // sole unreaped leader. Only definite absence after a successful
            // reap resolves that error; live or unobservable groups still fail.
            !(_exit_observed
                && status.is_ok()
                && error.raw_os_error() == Some(libc::EPERM)
                && group_is_absent(pgid))
        });
        match signal_error {
            Some(error) => Err(error),
            None => status,
        }
    }
}

impl Drop for GroupProcess {
    fn drop(&mut self) {
        if self.owns_root {
            let _ = self.terminate_and_reap();
        }
    }
}

/// Whether nothing is left of a process group, as far as a signal can tell.
#[cfg(any(target_os = "macos", test))]
fn group_is_absent(pgid: libc::pid_t) -> bool {
    // SAFETY: signal 0 is only an existence/permission query. A recycled PGID
    // can make this proof fail conservatively, but receives no termination signal.
    let result = unsafe { libc::kill(-pgid, 0) };
    result != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// How long a probe may take, and how much it may print on each stream. A
/// stream without a limit is not opened: what the process prints there is
/// discarded.
pub(crate) struct ProbeLimits {
    pub timeout: Duration,
    pub stdout_bytes: usize,
    pub stderr_bytes: Option<usize>,
}

/// A probe that ran to its end.
pub(crate) struct ProbeOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Why a probe gave nothing. Every case is a refusal: a caller never acts on
/// part of what a probe printed.
#[derive(Debug)]
pub(crate) enum ProbeFailure {
    /// The deadline passed while the process was still running.
    TimedOut,
    /// The process had ended, but its output did not end with it: something
    /// outside the group holds a pipe.
    OutputOpen,
    /// A stream printed past its limit.
    OverLimit,
    /// It could not be started, read or finished.
    Io,
}

/// A pipe held open by a descendant would not wake a wait once the leader has
/// ended, so the leader is asked this often while a pipe is open. With every
/// pipe ended it is asked this often until it ends.
const OUTPUT_POLL: Duration = Duration::from_millis(50);
const EXIT_POLL: Duration = Duration::from_millis(5);
/// How long an output still open after the group was killed is given to end.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);

/// One output pipe of the probe, read without a thread.
struct Stream {
    fd: std::os::fd::OwnedFd,
    limit: usize,
    bytes: Vec<u8>,
    ended: bool,
}

impl Stream {
    /// Takes the pipe and makes a read on it return at once.
    fn open(fd: std::os::fd::OwnedFd, limit: usize) -> Result<Self, ProbeFailure> {
        use std::os::fd::AsRawFd;
        // SAFETY: fcntl only inspects and sets flags on a descriptor this
        // stream owns and keeps owning; it reads no memory of ours.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(ProbeFailure::Io);
        }
        Ok(Self {
            fd,
            limit,
            bytes: Vec::new(),
            ended: false,
        })
    }

    /// Reads what the pipe holds now, and marks it ended once nothing more
    /// will come. A pipe that has ended still delivers what it holds first.
    fn read_available(&mut self) -> Result<(), ProbeFailure> {
        use std::os::fd::AsRawFd;
        let mut buffer = [0u8; 8192];
        loop {
            // SAFETY: reads into `buffer`, which is live and as long as stated.
            let count = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if count > 0 {
                let count = count as usize;
                if self.bytes.len() + count > self.limit {
                    return Err(ProbeFailure::OverLimit);
                }
                self.bytes.extend_from_slice(&buffer[..count]);
                continue;
            }
            if count == 0 {
                self.ended = true;
                return Ok(());
            }
            match io::Error::last_os_error().kind() {
                io::ErrorKind::WouldBlock => return Ok(()),
                io::ErrorKind::Interrupted => continue,
                _ => return Err(ProbeFailure::Io),
            }
        }
    }
}

/// Waits up to `until` for any of the pipes to have more or to end.
fn wait_for_output(streams: &mut [Stream], until: Instant) -> Result<(), ProbeFailure> {
    use std::os::fd::AsRawFd;
    let mut fds: Vec<libc::pollfd> = streams
        .iter()
        .filter(|stream| !stream.ended)
        .map(|stream| libc::pollfd {
            fd: stream.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let millis = i32::try_from(until.saturating_duration_since(Instant::now()).as_millis())
        .unwrap_or(i32::MAX);
    // SAFETY: `fds` is live for the call and holds only initialised entries.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
    if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
        return Err(ProbeFailure::Io);
    }
    Ok(())
}

/// Runs `command` to its end, or to `limits.timeout`, and reclaims its process
/// group either way. The output is complete or there is none of it.
pub(crate) fn run_bounded_probe(
    command: &mut Command,
    limits: &ProbeLimits,
) -> Result<ProbeOutput, ProbeFailure> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if limits.stderr_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    // On every exit from here that is not a completed probe, dropping
    // `process` stops and kills the group and reaps the leader.
    let mut process = GroupProcess::spawn(command).map_err(|_| ProbeFailure::Io)?;
    let stdout = process
        .child_mut()
        .stdout
        .take()
        .expect("piped stdout")
        .into();
    let mut streams = vec![Stream::open(stdout, limits.stdout_bytes)?];
    if let Some(limit) = limits.stderr_bytes {
        let stderr = process
            .child_mut()
            .stderr
            .take()
            .expect("piped stderr")
            .into();
        streams.push(Stream::open(stderr, limit)?);
    }

    let deadline = Instant::now() + limits.timeout;
    let mut leader_ended = false;
    let ended = loop {
        for stream in &mut streams {
            if !stream.ended {
                stream.read_available()?;
            }
        }
        if leader_ended {
            break true;
        }
        let now = Instant::now();
        if now >= deadline {
            break false;
        }
        // With a pipe still open, the leader's end is the only thing that
        // would not wake a longer wait.
        let output_open = streams.iter().any(|stream| !stream.ended);
        let until = deadline.min(now + if output_open { OUTPUT_POLL } else { EXIT_POLL });
        wait_for_output(&mut streams, until)?;
        leader_ended = process.observe_exit().map_err(|_| ProbeFailure::Io)?;
    };

    // The group is stopped and killed whether the probe ended or not: what
    // the process left running in it is not meant to outlive the probe.
    let status = process.terminate_and_reap().map_err(|_| ProbeFailure::Io)?;
    if !ended {
        return Err(ProbeFailure::TimedOut);
    }
    // With the group gone, a pipe still open ends now, unless a process
    // outside the group holds it.
    let until = Instant::now() + OUTPUT_GRACE;
    for index in 0..streams.len() {
        while !streams[index].ended {
            wait_for_output(&mut streams[index..index + 1], until)?;
            streams[index].read_available()?;
            if !streams[index].ended && Instant::now() >= until {
                return Err(ProbeFailure::OutputOpen);
            }
        }
    }
    let stderr = if streams.len() == 2 {
        streams.pop().map(|stream| stream.bytes).unwrap_or_default()
    } else {
        Vec::new()
    };
    let stdout = streams.pop().map(|stream| stream.bytes).unwrap_or_default();
    Ok(ProbeOutput {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A stand-in for a vendor binary: a shell script with a directory of
    /// its own for what it writes.
    struct StandIn {
        root: std::path::PathBuf,
        program: std::path::PathBuf,
    }

    impl StandIn {
        fn new(name: &str) -> Self {
            // A directory per run: a leftover one from an aborted earlier run
            // must not fail this one (#2340).
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "tachi-bounded-probe-{name}-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).expect("stand-in root");
            let program = root.join("program");
            Self { root, program }
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.root.join(name)
        }

        fn write(&self, body: &str) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&self.program, format!("#!/bin/sh\n{body}\n")).expect("stand-in script");
            std::fs::set_permissions(&self.program, std::fs::Permissions::from_mode(0o700))
                .expect("stand-in mode");
        }

        fn command(&self) -> Command {
            Command::new(&self.program)
        }

        /// Removes the stand-in's own directory, and nothing above it.
        fn remove(&self) {
            assert_ne!(self.root, std::env::temp_dir());
            assert!(self.root.starts_with(std::env::temp_dir()));
            std::fs::remove_dir_all(&self.root).expect("remove stand-in root");
        }
    }

    fn limits(timeout: Duration, stdout_bytes: usize, stderr_bytes: Option<usize>) -> ProbeLimits {
        ProbeLimits {
            timeout,
            stdout_bytes,
            stderr_bytes,
        }
    }

    fn alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 is only an existence/permission query.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// The pid a stand-in wrote down, once it has.
    fn pid_from(path: &Path) -> Option<libc::pid_t> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| text.trim().parse().ok())
    }

    /// Waits, for at most `within`, for a process to be gone. The probe's own
    /// child is reaped before the probe returns, so it must be gone at once. A
    /// descendant it killed is an orphan, and may stay a zombie until init
    /// reaps it, so liveness is asked again until it is no.
    fn wait_until_gone(what: &str, pid_file: &Path, within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            let Some(pid) = pid_from(pid_file) else {
                return;
            };
            assert!(
                Instant::now() < deadline,
                "{what} (pid {pid}) is still alive"
            );
            if !alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn reads_what_a_fast_exit_left_in_the_pipe() {
        // More than one read holds, with the last line the one that matters.
        let stand_in = StandIn::new("tail");
        stand_in.write("head -c 200000 /dev/zero | tr '\\0' x\nprintf '\\nthe end\\n'");
        let output = run_bounded_probe(
            &mut stand_in.command(),
            &limits(Duration::from_secs(5), 300_000, None),
        )
        .expect("a probe that ended");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200_009);
        assert!(output.stdout.ends_with(b"\nthe end\n"));
        assert!(output.stderr.is_empty());
        stand_in.remove();
    }

    #[test]
    fn the_status_is_the_leaders_and_stderr_is_read_only_when_asked_for() {
        let stand_in = StandIn::new("status");
        stand_in.write("printf 'note' >&2\nexit 7");
        let output = run_bounded_probe(
            &mut stand_in.command(),
            &limits(Duration::from_secs(5), 1024, Some(1024)),
        )
        .expect("a probe that ended");
        assert_eq!(output.status.code(), Some(7));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"note");
        stand_in.remove();

        let stand_in = StandIn::new("status-no-stderr");
        stand_in.write("printf 'note' >&2\nexit 7");
        let output = run_bounded_probe(
            &mut stand_in.command(),
            &limits(Duration::from_secs(5), 1024, None),
        )
        .expect("a probe that ended");
        assert_eq!(output.status.code(), Some(7));
        assert!(output.stderr.is_empty());
        stand_in.remove();
    }

    #[test]
    fn output_past_the_limit_is_refused_and_nothing_is_left_behind() {
        let stand_in = StandIn::new("over-limit");
        let pid_file = stand_in.path("leader");
        stand_in.write("echo $$ > \"$1\"\nwhile :; do printf 'xxxxxxxxxx'; done");
        let mut command = stand_in.command();
        command.arg(&pid_file);
        let failure =
            run_bounded_probe(&mut command, &limits(Duration::from_secs(10), 1_000, None))
                .err()
                .expect("output past the limit gives nothing");
        assert!(matches!(failure, ProbeFailure::OverLimit), "{failure:?}");
        wait_until_gone(
            "a leader that printed past the limit",
            &pid_file,
            Duration::from_secs(2),
        );
        stand_in.remove();
    }

    #[test]
    fn a_hanging_process_is_given_up_on_at_the_deadline_and_nothing_is_left_behind() {
        let stand_in = StandIn::new("hanging");
        let pid_file = stand_in.path("leader");
        stand_in.write("echo $$ > \"$1\"\nexec sleep 20");
        let mut command = stand_in.command();
        command.arg(&pid_file);
        let started = Instant::now();
        let failure = run_bounded_probe(&mut command, &limits(Duration::from_secs(1), 1024, None))
            .err()
            .expect("a probe past its deadline gives nothing");
        assert!(matches!(failure, ProbeFailure::TimedOut), "{failure:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the deadline must end the probe: {:?}",
            started.elapsed()
        );
        wait_until_gone(
            "a leader that had to be killed",
            &pid_file,
            Duration::from_secs(2),
        );
        stand_in.remove();
    }

    #[test]
    fn a_dead_leader_with_a_live_descendant_ends_the_probe_and_the_descendant() {
        let stand_in = StandIn::new("orphan");
        let marker = stand_in.path("leaked");
        let pid_file = stand_in.path("descendant");
        stand_in.write(&format!(
            "(sleep 20; echo leaked > '{}') &\necho $! > '{}'\nprintf 'done'\nexit 0",
            marker.display(),
            pid_file.display()
        ));
        let started = Instant::now();
        let output = run_bounded_probe(
            &mut stand_in.command(),
            &limits(Duration::from_secs(10), 1024, None),
        )
        .expect("the leader ended before the deadline");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"done");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the descendant must not hold the probe: {:?}",
            started.elapsed()
        );
        wait_until_gone(
            "a descendant left behind by its leader",
            &pid_file,
            // Short of the 20 seconds the descendant would otherwise sleep.
            Duration::from_secs(10),
        );
        assert!(!marker.exists(), "the descendant outlived the probe");
        stand_in.remove();
    }

    /// Run again by the test below, as a descendant that leaves the group and
    /// then holds on to the probe's pipe.
    #[test]
    fn pipe_holder() {
        let Some(pid_file) = std::env::var_os("TACHI_BOUNDED_PROBE_HOLDER_PID") else {
            return;
        };
        // SAFETY: setsid starts a session; it passes no pointers and aliases
        // no memory. This process is not a group leader, so it succeeds.
        unsafe { libc::setsid() };
        std::fs::write(Path::new(&pid_file), std::process::id().to_string()).unwrap();
        if let Some(ready) = std::env::var_os("TACHI_BOUNDED_PROBE_HOLDER_READY") {
            std::fs::write(Path::new(&ready), b"ready").unwrap();
        }
        std::thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn a_pipe_held_from_outside_the_group_is_given_up_on() {
        let stand_in = StandIn::new("escaped");
        let pid_file = stand_in.path("holder");
        let ready = stand_in.path("escaped");
        // The leader waits until the holder is out of the group, then ends.
        stand_in.write(&format!(
            "\"$1\" --exact bounded_probe::tests::pipe_holder --nocapture &
while [ ! -f '{}' ]; do sleep 0.01; done
printf 'done'
exit 0",
            ready.display()
        ));
        let mut command = stand_in.command();
        command
            .arg(std::env::current_exe().expect("test binary"))
            .env("TACHI_BOUNDED_PROBE_HOLDER_PID", &pid_file)
            .env("TACHI_BOUNDED_PROBE_HOLDER_READY", &ready);
        let started = Instant::now();
        let failure = run_bounded_probe(&mut command, &limits(Duration::from_secs(20), 1024, None))
            .err()
            .expect("an output that never ends gives nothing");
        assert!(matches!(failure, ProbeFailure::OutputOpen), "{failure:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "a pipe held from outside the group must not hold the probe: {:?}",
            started.elapsed()
        );
        // The holder left the group, so the probe did not end it. It is this
        // test's to end.
        let pid = pid_from(&pid_file).expect("the holder wrote its pid");
        // SAFETY: signals the pid the holder wrote down for this test.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        wait_until_gone("the pipe holder", &pid_file, Duration::from_secs(10));
        stand_in.remove();
    }

    #[test]
    fn group_absence_requires_reap_and_does_not_accept_live_or_zombie_members() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "read -r release; exit 7"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut process = GroupProcess::spawn(&mut command).unwrap();
        let pgid = process.child.id() as libc::pid_t;
        assert!(process.observe_exit().is_ok_and(|exited| !exited));
        assert!(
            !group_is_absent(pgid),
            "a live process group must not clear a permission failure"
        );

        drop(process.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !process.observe_exit().unwrap() {
            assert!(Instant::now() < deadline, "fixture leader did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !group_is_absent(pgid),
            "an unreaped zombie (including Darwin EPERM) is not definite absence"
        );

        // The child has already exited. Reap without further group signals;
        // successful reap removes the last member and preserves its exit code.
        process.owns_root = false;
        assert_eq!(process.child.wait().unwrap().code(), Some(7));
        assert!(group_is_absent(pgid));
    }

    #[test]
    fn lost_child_ownership_disarms_all_later_group_signals() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        let mut process = GroupProcess::spawn(&mut command).unwrap();
        process.child.wait().unwrap();
        assert_eq!(
            process.observe_exit().unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        assert!(!process.owns_root);
        assert!(process
            .terminate_and_reap()
            .unwrap_err()
            .to_string()
            .contains("ownership was lost"));
    }
}
