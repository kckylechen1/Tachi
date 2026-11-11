//! Server-owned filesystem write confinement for native ACP children.
//! This is not network isolation or a claim that all host files are unreadable.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub struct NativeSandboxCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

/// Final pre-exec hook, after fchdir and process-group setup. CLOEXEC preserves
/// Rust's spawn-error pipe until exec; closing descriptors here would hide an
/// exec failure. The child paths use fixed stack buffers, descriptor syscalls,
/// and allocation-free OS errors; neither lowered limits nor parent snapshots
/// are used as a bound on inherited descriptors.
#[cfg(unix)]
pub fn mark_native_inherited_fds_cloexec() -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let result = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                3_u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EINVAL)) {
            return Err(error);
        }
        mark_linux_fds_cloexec()
    }
    #[cfg(target_os = "macos")]
    {
        mark_macos_fds_cloexec()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn mark_fd_cloexec(fd: libc::c_int) -> std::io::Result<()> {
    if fd < 0 {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    if fd < 3 {
        return Ok(());
    }
    let flags = loop {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags != -1 {
            break flags;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    };
    loop {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != -1 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

#[cfg(target_os = "macos")]
fn mark_macos_fds_cloexec() -> std::io::Result<()> {
    // libproc's proc_pidinfo is a thin __proc_info wrapper in Apple XNU.
    // PROC_PIDLISTFDS silently truncates, so a full buffer is never success.
    // This is a count bound, not a bound on the numeric value of a descriptor.
    let mut entries = [libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    }; 4096];
    let capacity = std::mem::size_of_val(&entries);
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            entries.as_mut_ptr().cast(),
            capacity as libc::c_int,
        )
    };
    if bytes <= 0 {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        return Err(std::io::Error::from_raw_os_error(if errno == 0 {
            libc::EIO
        } else {
            errno
        }));
    }
    let count = complete_fd_list_len(
        bytes as usize,
        capacity,
        std::mem::size_of::<libc::proc_fdinfo>(),
    )?;
    for entry in &entries[..count] {
        mark_fd_cloexec(entry.proc_fd)?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn complete_fd_list_len(
    bytes: usize,
    capacity: usize,
    entry_size: usize,
) -> std::io::Result<usize> {
    if bytes >= capacity {
        return Err(std::io::Error::from_raw_os_error(libc::EOVERFLOW));
    }
    if bytes == 0 || !bytes.is_multiple_of(entry_size) {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(bytes / entry_size)
}

#[cfg(target_os = "linux")]
fn mark_linux_fds_cloexec() -> std::io::Result<()> {
    let directory = unsafe {
        libc::open(
            c"/proc/self/fd".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if directory == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let result = (|| {
        let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::fstatfs(directory, filesystem.as_mut_ptr()) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { filesystem.assume_init() }.f_type != libc::PROC_SUPER_MAGIC {
            return Err(std::io::Error::from_raw_os_error(libc::ENOTSUP));
        }
        // u64 storage keeps the kernel dirent buffer aligned without allocation.
        let mut storage = [0_u64; 512];
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(
                storage.as_mut_ptr().cast::<u8>(),
                std::mem::size_of_val(&storage),
            )
        };
        let mut last_offset = 0;
        loop {
            let bytes = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    directory,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                )
            };
            if bytes == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(error);
            }
            if bytes == 0 {
                return Ok(());
            }
            if bytes < 0 || bytes as usize > buffer.len() {
                return Err(std::io::Error::from_raw_os_error(libc::EIO));
            }
            visit_linux_fd_entries(&buffer[..bytes as usize], &mut last_offset, mark_fd_cloexec)?;
        }
    })();
    // Only this private enumeration descriptor is closed. In particular, the
    // Rust spawn-error pipe stays usable if enumeration or exec fails.
    let close_result = unsafe { libc::close(directory) };
    match result {
        Err(error) => Err(error),
        Ok(()) if close_result == -1 => Err(std::io::Error::last_os_error()),
        Ok(()) => Ok(()),
    }
}

#[cfg(target_os = "linux")]
fn visit_linux_fd_entries(
    buffer: &[u8],
    last_offset: &mut i64,
    mut visit: impl FnMut(libc::c_int) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let invalid = || std::io::Error::from_raw_os_error(libc::EIO);
    let mut cursor = 0;
    while cursor < buffer.len() {
        let remaining = &buffer[cursor..];
        // linux_dirent64 has a 19-byte header followed by a NUL-terminated name.
        if remaining.len() < 20 {
            return Err(invalid());
        }
        let record_len = u16::from_ne_bytes([remaining[16], remaining[17]]) as usize;
        if record_len < 20 || record_len > remaining.len() || record_len % 8 != 0 {
            return Err(invalid());
        }
        let offset = i64::from_ne_bytes(remaining[8..16].try_into().map_err(|_| invalid())?);
        // procfs enumerates FD numbers in increasing order. Replayed offsets
        // would otherwise make an interrupted or malformed stream look complete.
        if offset <= *last_offset {
            return Err(invalid());
        }
        *last_offset = offset;
        let name_field = &remaining[19..record_len];
        let terminator = name_field
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(invalid)?;
        let name = &name_field[..terminator];
        if name != b"." && name != b".." {
            if name.is_empty() {
                return Err(invalid());
            }
            let mut fd: libc::c_int = 0;
            for byte in name {
                if !byte.is_ascii_digit() {
                    return Err(invalid());
                }
                fd = fd
                    .checked_mul(10)
                    .and_then(|fd| fd.checked_add((byte - b'0') as libc::c_int))
                    .ok_or_else(invalid)?;
            }
            visit(fd)?;
        }
        cursor += record_len;
    }
    Ok(())
}

pub fn native_sandbox_provider() -> Option<&'static str> {
    match std::env::consts::OS {
        "linux" => Some("bubblewrap"),
        "macos" => Some("sandbox-exec"),
        _ => None,
    }
}

/// Probe the actual isolation primitive, not just executable discovery. A
/// failed host stays unavailable for this daemon lifetime; there is no bypass.
pub fn native_sandbox_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(probe_native_sandbox)
}

fn probe_native_sandbox() -> bool {
    let mut command = match std::env::consts::OS {
        "linux" => {
            let mut command = Command::new("/usr/bin/bwrap");
            command.args(linux_base_args());
            command.args(["--", "/bin/true"]);
            command
        }
        "macos" => {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command.args([
                "-p",
                "(version 1)(allow default)(deny file-write*)(deny process-info-setcontrol)",
                "/usr/bin/true",
            ]);
            command
        }
        _ => return false,
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(mark_native_inherited_fds_cloexec);
    }
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Err(_) => break,
            Ok(None) if Instant::now() >= deadline => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

fn linux_base_args() -> &'static [&'static str] {
    &[
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--disable-userns",
        "--cap-drop",
        "ALL",
        "--die-with-parent",
        "--ro-bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]
}

fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(format!(
            "native sandbox {label} must be an existing absolute directory"
        ));
    }
    let actual = std::fs::canonicalize(path)
        .map_err(|error| format!("native sandbox resolve {label}: {error}"))?;
    if actual != path {
        return Err(format!(
            "native sandbox {label} must use its canonical physical path"
        ));
    }
    Ok(actual)
}

fn private_directory(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let path = parent.join(name);
    match std::fs::create_dir(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("create native sandbox {name}: {error}")),
    }
    canonical_directory(&path, name)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protect native sandbox {name}: {error}"))?;
    }
    Ok(path)
}

#[cfg(unix)]
fn reject_shared_inodes(root: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0_usize;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)
            .map_err(|error| format!("inspect native writable workspace: {error}"))?
        {
            count += 1;
            if count > 500_000 {
                return Err(
                    "native writable workspace exceeds bounded hard-link inspection".into(),
                );
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let metadata =
                std::fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() && metadata.nlink() > 1 {
                return Err("native writable workspace contains a shared hard-linked file; use an isolated managed worktree".into());
            }
        }
    }
    Ok(())
}

/// Build an invocation before applying stdio, env_clear, process-group and
/// anchored-cwd hooks. The caller must fchdir/current_dir to the granted `cwd`
/// before exec and close every non-stdio inherited descriptor. Runtime is the
/// server-created `<run>/runtime`, never the run receipt directory itself.
pub fn native_sandbox_command(
    program: &OsStr,
    args: &[OsString],
    cwd: &Path,
    runtime_dir: &Path,
    writable: bool,
) -> Result<NativeSandboxCommand, String> {
    if !native_sandbox_available() {
        return Err("native Session filesystem confinement is unavailable on this host".into());
    }
    let cwd = canonical_directory(cwd, "cwd")?;
    let runtime = canonical_directory(runtime_dir, "runtime")?;
    let run_dir = runtime
        .parent()
        .ok_or("native sandbox runtime has no run parent")?;
    if runtime.file_name() != Some(OsStr::new("runtime"))
        || runtime.starts_with(&cwd)
        || cwd.starts_with(run_dir)
        || cwd.parent().is_none()
    {
        return Err("native sandbox requires a separate managed workspace and run-owned runtime subdirectory".into());
    }
    #[cfg(unix)]
    {
        reject_shared_inodes(&runtime)?;
        if writable {
            reject_shared_inodes(&cwd)?;
        }
    }
    let home = private_directory(&runtime, "home")?;
    let tmp = private_directory(&runtime, "tmp")?;
    let config = private_directory(&runtime, "config")?;
    let cache = private_directory(&runtime, "cache")?;
    let data = private_directory(&runtime, "data")?;
    let mut invocation = NativeSandboxCommand {
        program: OsString::new(),
        args: Vec::new(),
        env: [
            ("HOME", home),
            ("TMPDIR", tmp),
            ("XDG_CONFIG_HOME", config),
            ("XDG_CACHE_HOME", cache),
            ("XDG_DATA_HOME", data),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into_os_string()))
        .collect(),
    };
    match std::env::consts::OS {
        "linux" => {
            invocation.program = "/usr/bin/bwrap".into();
            invocation
                .args
                .extend(linux_base_args().iter().map(OsString::from));
            invocation.args.extend([
                "--bind".into(),
                runtime.clone().into_os_string(),
                runtime.into_os_string(),
                if writable {
                    "--bind".into()
                } else {
                    "--ro-bind".into()
                },
                "/proc/self/cwd".into(),
                cwd.clone().into_os_string(),
                "--chdir".into(),
                cwd.into_os_string(),
                "--".into(),
                program.to_os_string(),
            ]);
        }
        "macos" => {
            invocation.program = "/usr/bin/sandbox-exec".into();
            let mut policy = format!("(version 1)(allow default)(deny file-write*)(deny process-info-setcontrol)(allow file-write* (subpath {}))", sbpl_path(&runtime)?);
            if writable {
                policy.push_str(&format!(
                    "(allow file-write* (subpath {}))",
                    sbpl_path(&cwd)?
                ));
            }
            invocation
                .args
                .extend(["-p".into(), policy.into(), program.to_os_string()]);
        }
        _ => return Err("native Session filesystem confinement has no host implementation".into()),
    }
    invocation.args.extend_from_slice(args);
    Ok(invocation)
}

fn sbpl_path(path: &Path) -> Result<String, String> {
    let path = path
        .to_str()
        .ok_or("native macOS sandbox requires a UTF-8 canonical path")?;
    if path.chars().any(char::is_control) {
        return Err("native macOS sandbox path contains a control character".into());
    }
    Ok(format!(
        "\"{}\"",
        path.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        cwd: PathBuf,
        runtime: PathBuf,
        outside: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("tachi-native-sandbox-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("run/runtime")).unwrap();
            std::fs::create_dir(root.join("workspace")).unwrap();
            let root = root.canonicalize().unwrap();
            let outside = root.join("outside");
            std::fs::write(&outside, "owner").unwrap();
            Self {
                cwd: root.join("workspace"),
                runtime: root.join("run/runtime"),
                root,
                outside,
            }
        }
        fn command(&self, writable: bool, script: &str) -> Command {
            let args = [
                OsString::from("-c"),
                OsString::from(script),
                OsString::from("probe"),
                self.outside.clone().into_os_string(),
            ];
            let wrapper = native_sandbox_command(
                OsStr::new("/bin/sh"),
                &args,
                &self.cwd,
                &self.runtime,
                writable,
            )
            .unwrap();
            let mut command = Command::new(wrapper.program);
            command
                .args(wrapper.args)
                .envs(wrapper.env)
                .current_dir(&self.cwd);
            #[cfg(unix)]
            unsafe {
                use std::os::unix::process::CommandExt;
                command.pre_exec(mark_native_inherited_fds_cloexec);
            }
            command.stdout(Stdio::null()).stderr(Stdio::null());
            command
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn child_cannot_write_outside_but_can_write_declared_workspace() {
        if !native_sandbox_available() {
            eprintln!("native sandbox unavailable; host probe refused");
            return;
        }
        let f = Fixture::new();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&f.outside, f.cwd.join("escape")).unwrap();
        let status = f.command(true, "printf own > allowed && printf tmp > \"$TMPDIR/probe\" && ! (printf bad > \"$1\") && ! (printf bad > escape) && ! (ln \"$1\" \"$HOME/new-link\" && printf bad > \"$HOME/new-link\")").status().unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&f.outside).unwrap(), "owner");
        assert_eq!(
            std::fs::read_to_string(f.cwd.join("allowed")).unwrap(),
            "own"
        );
        assert_eq!(
            std::fs::read_to_string(f.runtime.join("tmp/probe")).unwrap(),
            "tmp"
        );
    }

    #[test]
    fn read_only_child_cannot_write_its_own_cwd() {
        if !native_sandbox_available() {
            eprintln!("native sandbox unavailable; host probe refused");
            return;
        }
        let f = Fixture::new();
        assert!(f
            .command(
                false,
                "! (printf bad > denied) && printf ok > \"$HOME/allowed\" && ! (ln \"$1\" \"$HOME/new-link\" && printf bad > \"$HOME/new-link\")"
            )
            .status()
            .unwrap()
            .success());
        assert!(!f.cwd.join("denied").exists());
        assert_eq!(std::fs::read_to_string(&f.outside).unwrap(), "owner");
        assert_eq!(
            std::fs::read_to_string(f.runtime.join("home/allowed")).unwrap(),
            "ok"
        );
    }

    #[test]
    #[cfg(unix)]
    fn writable_shared_hard_link_is_refused_before_spawn() {
        if !native_sandbox_available() {
            return;
        }
        let f = Fixture::new();
        std::fs::hard_link(&f.outside, f.cwd.join("alias")).unwrap();
        let result = native_sandbox_command(OsStr::new("/bin/true"), &[], &f.cwd, &f.runtime, true);
        assert!(result.err().unwrap().contains("hard-linked"));
    }

    #[test]
    #[cfg(unix)]
    fn pre_exec_sweep_prevents_inherited_writable_fd_escape() {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        if !native_sandbox_available() {
            return;
        }
        let f = Fixture::new();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&f.outside)
            .unwrap();
        let fd = file.as_raw_fd();
        let mut command = f.command(false, "! (printf bad >&9)");
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(fd, 9) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                mark_native_inherited_fds_cloexec()
            });
        }
        assert!(command.status().unwrap().success());
        assert_eq!(std::fs::read_to_string(&f.outside).unwrap(), "owner");
    }

    #[cfg(unix)]
    fn check_pipe_after_sweep(sweep: fn() -> std::io::Result<()>) -> std::io::Result<()> {
        let mut pipe = [-1; 2];
        if unsafe { libc::pipe(pipe.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            for fd in pipe {
                if unsafe { libc::fcntl(fd, libc::F_GETFD) } != 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
            }
            sweep()?;
            for fd in pipe {
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                if flags == -1 || flags & libc::FD_CLOEXEC == 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
            }
            let byte = b'x';
            let mut received = 0_u8;
            if unsafe { libc::write(pipe[1], (&byte as *const u8).cast(), 1) } != 1
                || unsafe { libc::read(pipe[0], (&mut received as *mut u8).cast(), 1) } != 1
                || received != byte
            {
                return Err(std::io::Error::from_raw_os_error(libc::EIO));
            }
            Ok(())
        })();
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
        result
    }

    #[cfg(unix)]
    fn assert_sweep_preserves_pipe_and_exec_errors(sweep: fn() -> std::io::Result<()>) {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new("/usr/bin/true");
        unsafe {
            command.pre_exec(move || check_pipe_after_sweep(sweep));
        }
        assert!(command.status().unwrap().success());

        let nonexistent =
            std::env::temp_dir().join(format!("tachi-missing-executable-{}", uuid::Uuid::new_v4()));
        let mut command = Command::new(nonexistent);
        unsafe {
            command.pre_exec(sweep);
        }
        // Closing the private Rust error pipe would turn this into an apparent
        // successful spawn instead of communicating the exec failure.
        assert_eq!(
            command.spawn().unwrap_err().raw_os_error(),
            Some(libc::ENOENT)
        );
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn fd_sweep_marks_pipe_without_closing_it_or_losing_exec_errors() {
        assert_sweep_preserves_pipe_and_exec_errors(mark_native_inherited_fds_cloexec);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn fd_enumeration_fallback_marks_pipe_and_preserves_exec_errors() {
        assert_sweep_preserves_pipe_and_exec_errors(mark_linux_fds_cloexec);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn fd_enumeration_finds_high_inherited_fd_after_hard_limit_is_lowered() {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new("/usr/bin/true");
        unsafe {
            command.pre_exec(|| {
                let mut pipe = [-1; 2];
                if libc::pipe(pipe.as_mut_ptr()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // F_DUPFD chooses an unused descriptor, so this cannot replace
                // Rust's error pipe or another inherited descriptor.
                let minimum = 256.max(pipe[0].max(pipe[1]) + 1);
                let high = libc::fcntl(pipe[1], libc::F_DUPFD, minimum);
                if high == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // Free a slot below the new limit for /proc/self/fd itself.
                if libc::close(pipe[1]) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let limit = libc::rlimit {
                    rlim_cur: high as libc::rlim_t,
                    rlim_max: high as libc::rlim_t,
                };
                // Limits change only in this forked child, never in the host
                // test process. The open high FD now equals the hard limit.
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(high, libc::F_GETFD) != 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
                // Exercise the old-kernel fallback even on close_range hosts.
                mark_linux_fds_cloexec()?;
                let flags = libc::fcntl(high, libc::F_GETFD);
                if flags == -1 || flags & libc::FD_CLOEXEC == 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
                let byte = b'h';
                let mut received = 0_u8;
                if libc::write(high, (&byte as *const u8).cast(), 1) != 1
                    || libc::read(pipe[0], (&mut received as *mut u8).cast(), 1) != 1
                    || received != byte
                {
                    return Err(std::io::Error::from_raw_os_error(libc::EIO));
                }
                if libc::close(pipe[0]) != 0 || libc::close(high) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        assert!(command.status().unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    fn fd_list_capacity_refuses_truncation_and_partial_entries() {
        const ENTRY_BYTES: usize = 8;
        const CAPACITY: usize = 4096 * ENTRY_BYTES;
        assert_eq!(complete_fd_list_len(24, CAPACITY, ENTRY_BYTES).unwrap(), 3);
        assert_eq!(
            complete_fd_list_len(CAPACITY, CAPACITY, ENTRY_BYTES)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EOVERFLOW)
        );
        assert_eq!(
            complete_fd_list_len(CAPACITY - 1, CAPACITY, ENTRY_BYTES)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EIO)
        );
    }

    #[cfg(target_os = "linux")]
    fn fd_dirent(name: &[u8], offset: i64) -> Vec<u8> {
        let length = (19 + name.len() + 1 + 7) & !7;
        let mut record = vec![0_u8; length];
        record[8..16].copy_from_slice(&offset.to_ne_bytes());
        record[16..18].copy_from_slice(&(length as u16).to_ne_bytes());
        record[19..19 + name.len()].copy_from_slice(name);
        record
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn fd_directory_parser_rejects_truncated_invalid_and_replayed_records() {
        let mut offset = 0;
        let mut seen = Vec::new();
        let mut first = fd_dirent(b".", 1);
        first.extend(fd_dirent(b"..", 2));
        first.extend(fd_dirent(b"3", 7));
        visit_linux_fd_entries(&first, &mut offset, |fd| {
            seen.push(fd);
            Ok(())
        })
        .unwrap();
        visit_linux_fd_entries(&fd_dirent(b"65536", 65540), &mut offset, |fd| {
            seen.push(fd);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [3, 65536]);
        assert!(
            visit_linux_fd_entries(&fd_dirent(b"65536", 65540), &mut offset, |_| Ok(())).is_err()
        );

        let valid = fd_dirent(b"3", 7);
        let mut no_progress = valid.clone();
        no_progress[16..18].copy_from_slice(&0_u16.to_ne_bytes());
        let mut no_terminator = valid.clone();
        no_terminator[19..].fill(b'3');
        for invalid in [
            valid[..valid.len() - 1].to_vec(),
            no_progress,
            no_terminator,
            fd_dirent(b"2147483648", 7),
            fd_dirent(b"not-a-descriptor", 7),
        ] {
            let mut visited = false;
            assert!(visit_linux_fd_entries(&invalid, &mut 0, |_| {
                visited = true;
                Ok(())
            })
            .is_err());
            assert!(
                !visited,
                "invalid records must never change an unrelated descriptor"
            );
        }
    }
}
