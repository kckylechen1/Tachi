//! OS-view attributed process-holder evidence (tachi#1118 liveness-gating
//! slice; freeze boundary 1 in the 2026-07-16 leader adjudication comment).
//!
//! Process-holder evidence in this module is built EXCLUSIVELY from
//! OS-view tools (`lsof`, `ps`), attributed to a pid family by
//! ppid/tty/command and, best-effort, child cwd. This module never accepts
//! a harness-reported "previous session exited"/orphan-adoption signal as
//! evidence of process death — a 2026-07-15 live incident (documented on
//! tachi#1118) showed exactly that kind of harness-view claim was false: a
//! process the harness had already declared exited was still alive and
//! actively writing. Harness signals may only ever NOMINATE a candidate for
//! reconciliation elsewhere; establishing liveness or death is exclusively
//! this module's job, and it only ever asks the OS.
//!
//! Fail-closed: an inconclusive probe (`Unknown`) is deliberately NOT the
//! same outcome as `Clear` and callers must never treat it as safe to
//! reclaim — mirrors the `HolderCheck::Unknown` precedent already
//! established (and sealed) for the destructive kill path in
//! `tachi-exec-env-reaper/src/lib.rs` under #1062. This module only ever detects; it
//! never signals or kills a process.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

/// One OS-view-attributed process holding (or having held) a candidate
/// path open, as reported by `lsof` + `ps`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HolderProcess {
    pub pid: i32,
    pub ppid: Option<i32>,
    pub tty: Option<String>,
    pub command: String,
    /// Best-effort child/self cwd attribution (`lsof -d cwd`), when the OS
    /// exposes it. Absence does not weaken the pid/ppid/tty attribution
    /// above; it is one more corroborating signal, not a requirement.
    pub cwd: Option<String>,
}

impl HolderProcess {
    pub fn describe(&self) -> String {
        format!(
            "pid={} ppid={} tty={} cwd={} cmd={}",
            self.pid,
            self.ppid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".to_string()),
            self.tty.as_deref().unwrap_or("?"),
            self.cwd.as_deref().unwrap_or("?"),
            self.command,
        )
    }
}

/// The three-way outcome of an OS-view holder probe. `Unknown` is
/// deliberately distinct from `Clear` (see module docs: fail-closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderEvidence {
    /// No OS-view holder found for the path.
    Clear,
    /// One or more processes attributed as holding the path open.
    Held(Vec<HolderProcess>),
    /// The probe itself could not run or could not be parsed (missing
    /// tool, permission denial, unexpected exit code). NOT equivalent to
    /// `Clear` — callers must refuse rather than proceed.
    Unknown(String),
}

impl HolderEvidence {
    /// Human-readable pid-family description for a loud refusal message.
    /// Named per the freeze contract's "refuses ... names the pid family"
    /// requirement.
    pub fn describe_family(&self) -> String {
        match self {
            HolderEvidence::Clear => "no OS-view holder".to_string(),
            HolderEvidence::Held(procs) => procs
                .iter()
                .map(HolderProcess::describe)
                .collect::<Vec<_>>()
                .join(" | "),
            HolderEvidence::Unknown(reason) => format!("holder probe inconclusive: {reason}"),
        }
    }
}

/// Injectable holder-probe signature, mirroring the established
/// `exec_env_reaper::HolderProbe` idiom for the (sealed, #1062) build-target
/// reap path: production callers pass `&probe_holders`; tests inject a
/// deterministic fake to exercise `Held`/`Unknown` without shelling out.
pub type HolderProbeFn = dyn Fn(&Path) -> HolderEvidence;

/// Probe OS-view process holders of `path`.
///
/// 1. `lsof +D <path>` enumerates candidate pids with any open file under
///    the path (recursive).
/// 2. Each candidate pid is attributed via `ps -o pid=,ppid=,tty=,command=`.
/// 3. Best-effort, each candidate's current working directory is resolved
///    via `lsof -a -p <pid> -d cwd -Fn` (child-cwd attribution).
///
/// This is the ONLY source of liveness truth for the worktree close
/// predicate; a caller must never substitute a harness-reported liveness
/// claim for this probe (freeze boundary 1, tachi#1118).
pub fn probe_holders(path: &Path) -> HolderEvidence {
    probe_holders_with(OsStr::new("lsof"), path)
}

/// [`probe_holders`] with the `lsof` program injectable, so tests can drive
/// the real call site (argv, stream capture, classification) with a stub.
fn probe_holders_with(lsof: &OsStr, path: &Path) -> HolderEvidence {
    let out = match Command::new(lsof).arg("+D").arg(path).output() {
        Ok(out) => out,
        Err(err) => return HolderEvidence::Unknown(format!("lsof unavailable: {err}")),
    };
    interpret_lsof_run(
        out.status.code(),
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
        path,
    )
}

/// Classify a raw `lsof +D <target>` run: stderr is first reduced to the
/// diagnostics that bear on `target` (tachi#1978 — lsof's Linux start-up
/// warnings about unrelated, unstat()able mounts such as tracefs are proven
/// irrelevant and dropped; see [`crate::lsof_stderr`]), then handed to the
/// fail-closed [`interpret_lsof_output`] unchanged.
fn interpret_lsof_run(
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    target: &Path,
) -> HolderEvidence {
    interpret_lsof_output(
        exit_code,
        stdout,
        &crate::lsof_stderr::relevant_lsof_stderr(stderr, target),
    )
}

/// Pure interpreter for an `lsof +D` run — the part worth testing in
/// isolation, without shelling out. Deliberately mirrors
/// `exec_env_reaper::interpret_lsof`'s fail-closed idiom exactly (tachi#1212
/// fix-round, codex checkpoint 4/5): that function is the sealed, already
/// reviewed precedent for the SAME kind of evidence (an `lsof +D` walk) on
/// the destructive (#1062) side; this module borrows its interpretation,
/// never its destructive consequence — this module only ever detects.
///
/// * any data row on stdout that yields a parseable pid ⇒ [`HolderEvidence::Held`]
/// * data rows present but NONE yield a parseable pid ⇒ [`HolderEvidence::Unknown`]:
///   the walk produced output we could not attribute, which is not proof of
///   absence (earned-not-defaulted: `Clear` must never be the fallback for
///   evidence we failed to parse).
/// * no data rows, but anything on stderr ⇒ [`HolderEvidence::Unknown`]: lsof
///   warns (e.g. "can't stat()", "Permission denied") when it could not
///   descend part of the tree, and a partial walk that "found nothing" is
///   not proof of nothing.
/// * no data rows, no stderr noise, exit 0/1 ⇒ [`HolderEvidence::Clear`] (1 is
///   lsof's documented "no matching files" status)
/// * any other exit / signal ⇒ [`HolderEvidence::Unknown`]
fn interpret_lsof_output(exit_code: Option<i32>, stdout: &str, stderr: &str) -> HolderEvidence {
    let data_lines: Vec<&str> = stdout
        .lines()
        .skip(1) // header row: COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME
        .filter(|line| !line.trim().is_empty())
        .collect();

    if !data_lines.is_empty() {
        let mut pids: Vec<i32> = data_lines
            .iter()
            .filter_map(|line| line.split_whitespace().nth(1))
            .filter_map(|pid_str| pid_str.parse::<i32>().ok())
            .collect();
        pids.sort_unstable();
        pids.dedup();
        if pids.is_empty() {
            return HolderEvidence::Unknown(format!(
                "lsof produced {} data row(s) but none yielded a parseable pid",
                data_lines.len()
            ));
        }
        return HolderEvidence::Held(pids.into_iter().map(attribute_pid).collect());
    }

    let noise = stderr.trim();
    if !noise.is_empty() {
        let first = noise.lines().next().unwrap_or(noise);
        return HolderEvidence::Unknown(format!("lsof walk incomplete: {first}"));
    }

    match exit_code {
        Some(0) | Some(1) => HolderEvidence::Clear,
        Some(code) => HolderEvidence::Unknown(format!("lsof exited with status {code}")),
        None => HolderEvidence::Unknown("lsof terminated by a signal".to_string()),
    }
}

fn attribute_pid(pid: i32) -> HolderProcess {
    let mut ppid = None;
    let mut tty = None;
    let mut command = format!("<unattributed pid {pid}>");

    if let Ok(out) = Command::new("ps")
        .args(["-o", "pid=,ppid=,tty=,command=", "-p", &pid.to_string()])
        .output()
    {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout);
            if let Some(line) = raw.lines().next() {
                let mut fields = line.split_whitespace();
                let _pid_field = fields.next();
                if let Some(p) = fields.next() {
                    ppid = p.parse::<i32>().ok();
                }
                if let Some(t) = fields.next() {
                    tty = Some(t.to_string());
                }
                let rest: Vec<&str> = fields.collect();
                if !rest.is_empty() {
                    command = rest.join(" ");
                }
            }
        }
    }

    let cwd = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|line| line.strip_prefix('n').map(str::to_string))
        });

    HolderProcess {
        pid,
        ppid,
        tty,
        command,
        cwd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), nanos));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn probe_is_clear_for_an_empty_dir() {
        let dir = unique_temp_dir("holder-clear");
        let evidence = probe_holders(&dir);
        assert_eq!(
            evidence,
            HolderEvidence::Clear,
            "an empty dir with no open handles must read as Clear: {evidence:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real-tool discrimination (mirrors the exec_env_reaper precedent
    /// `real_probe_never_reports_none_for_a_dir_with_an_open_file`): hold a
    /// file open under the candidate from THIS process and assert the real
    /// `lsof` call site attributes it back to this pid — never `Clear`.
    #[test]
    fn probe_detects_and_attributes_a_self_held_open_file() {
        let dir = unique_temp_dir("holder-held");
        let file_path = dir.join("held.txt");
        std::fs::write(&file_path, b"hold me open").unwrap();
        let _handle = std::fs::File::open(&file_path).unwrap();

        let evidence = probe_holders(&dir);
        let HolderEvidence::Held(procs) = &evidence else {
            panic!(
                "an open file handle under the dir must never read as Clear/Unknown: {evidence:?}"
            );
        };
        let own_pid = std::process::id() as i32;
        assert!(
            procs.iter().any(|p| p.pid == own_pid),
            "the holder family must include this process's own pid {own_pid}: {procs:?}"
        );
        let this_proc = procs.iter().find(|p| p.pid == own_pid).unwrap();
        assert!(
            this_proc.ppid.is_some(),
            "OS-view attribution must resolve a ppid for the holder, not just its pid: {this_proc:?}"
        );

        drop(_handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_is_never_equal_to_clear() {
        // Guards the fail-closed contract at the type level: a caller
        // pattern-matching on this enum can never conflate the two.
        assert_ne!(
            HolderEvidence::Unknown("boom".to_string()),
            HolderEvidence::Clear
        );
    }

    // --- interpret_lsof_output: tachi#1212 fix-round, codex checkpoint 4/5 ---
    // RED on the pre-fix code, which special-cased only
    // `exit_code == Some(1) && stdout.is_empty()` as Clear and never looked
    // at stderr at all on the success path.

    #[test]
    fn stderr_noise_with_no_data_rows_is_unknown_not_clear() {
        // Exit code 1 (lsof's own "nothing found" convention) with a
        // permission-denied warning on stderr must NOT collapse to Clear —
        // a partial walk that "found nothing" proves nothing.
        let evidence = interpret_lsof_output(
            Some(1),
            "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n",
            "lsof: WARNING: can't stat() /some/mount\n      Output information may be incomplete.\n",
        );
        assert!(
            matches!(evidence, HolderEvidence::Unknown(_)),
            "partial/diagnostic lsof output must be Unknown, got {evidence:?}"
        );
    }

    #[test]
    fn stderr_noise_on_a_success_exit_is_also_unknown() {
        // Same bug, other exit code: the pre-fix code never inspected
        // stderr at all when `out.status.success()` was true.
        let evidence = interpret_lsof_output(
            Some(0),
            "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n",
            "lsof: WARNING: can't stat() /some/mount\n",
        );
        assert!(
            matches!(evidence, HolderEvidence::Unknown(_)),
            "stderr noise on a success exit must still be Unknown, got {evidence:?}"
        );
    }

    #[test]
    fn unparsable_data_rows_are_unknown_not_clear() {
        // Data rows are present (the walk found something), but the pid
        // column doesn't parse for any of them — this must never be
        // silently treated as "no holders" (earned-not-defaulted).
        let evidence = interpret_lsof_output(
            Some(0),
            "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\nweird ??? garbled row here\n",
            "",
        );
        assert!(
            matches!(evidence, HolderEvidence::Unknown(_)),
            "data rows with no parseable pid must be Unknown, not Clear: {evidence:?}"
        );
    }

    #[test]
    fn clean_empty_run_is_clear() {
        let evidence = interpret_lsof_output(
            Some(1),
            "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n",
            "",
        );
        assert_eq!(evidence, HolderEvidence::Clear);
    }

    #[test]
    fn odd_exit_or_signal_is_unknown() {
        assert!(matches!(
            interpret_lsof_output(
                Some(2),
                "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n",
                ""
            ),
            HolderEvidence::Unknown(_)
        ));
        assert!(matches!(
            interpret_lsof_output(
                None,
                "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n",
                ""
            ),
            HolderEvidence::Unknown(_)
        ));
    }

    // --- tachi#1978: lsof's Linux start-up mount warnings ---

    /// Byte-for-byte stderr of lsof 4.95.0 on Ubuntu 24.04 as a non-root user,
    /// printed on every run whatever the query (captured on atom-dgx-2).
    const LINUX_TRACEFS_WARNING: &str = "lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\n";

    #[test]
    fn unrelated_mount_warning_on_an_empty_run_is_clear() {
        let dir = unique_temp_dir("holder-1978-clear");
        assert_eq!(
            interpret_lsof_run(Some(1), "", LINUX_TRACEFS_WARNING, &dir),
            HolderEvidence::Clear
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mount_warning_inside_the_walk_stays_unknown() {
        let dir = unique_temp_dir("holder-1978-inside");
        let stderr = format!(
            "lsof: WARNING: can't stat() fuse file system {}\n      Output information may be incomplete.\n",
            dir.canonicalize().unwrap().join("mnt").display()
        );
        assert!(matches!(
            interpret_lsof_run(Some(1), "", &stderr, &dir),
            HolderEvidence::Unknown(_)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn genuine_lsof_error_next_to_the_mount_warning_stays_unknown() {
        let dir = unique_temp_dir("holder-1978-error");
        let stderr = format!(
            "{LINUX_TRACEFS_WARNING}lsof: WARNING: can't opendir({}/sub): Permission denied\n",
            dir.display()
        );
        let evidence = interpret_lsof_run(Some(1), "", &stderr, &dir);
        let HolderEvidence::Unknown(reason) = &evidence else {
            panic!("a partial walk must stay Unknown: {evidence:?}");
        };
        assert!(reason.contains("can't opendir"), "{reason}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Drives the real call site (`Command`, argv, stream capture,
    /// classification) against a stub `lsof`, so the target the stderr filter
    /// compares against is the one actually probed.
    #[cfg(unix)]
    fn stub_lsof(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        // One file per stub: never rewrite a script another exec may hold.
        let stub = dir.join(name);
        std::fs::write(&stub, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).unwrap();
        stub
    }

    #[cfg(unix)]
    #[test]
    fn real_call_site_with_stub_lsof_classifies_linux_warnings() {
        let bin = unique_temp_dir("holder-1978-bin");
        let target = unique_temp_dir("holder-1978-target");
        let warn = "cat >&2 <<'EOF'\nlsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\nEOF\n";

        let clear = stub_lsof(
            &bin,
            "lsof-clear",
            &format!("[ \"$1\" = '+D' ] || exit 93\n{warn}exit 1\n"),
        );
        assert_eq!(
            probe_holders_with(clear.as_os_str(), &target),
            HolderEvidence::Clear,
            "warning-only empty run must be Clear"
        );

        let pid = std::process::id();
        let held = stub_lsof(
            &bin,
            "lsof-held",
            &format!("{warn}printf 'COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\\nx {pid} u 3r REG 1,4 0 1 %s/f\\n' \"$2\"\nexit 0\n"),
        );
        let evidence = probe_holders_with(held.as_os_str(), &target);
        let HolderEvidence::Held(procs) = &evidence else {
            panic!("a holder row must be Held despite the warning: {evidence:?}");
        };
        assert!(procs.iter().any(|p| p.pid == pid as i32));

        let inside = stub_lsof(
            &bin,
            "lsof-inside",
            "echo \"lsof: WARNING: can't stat() fuse file system $2/mnt\" >&2\nexit 1\n",
        );
        assert!(matches!(
            probe_holders_with(inside.as_os_str(), &target),
            HolderEvidence::Unknown(_)
        ));

        let failing = stub_lsof(
            &bin,
            "lsof-failing",
            &format!(
                "{warn}echo 'lsof: status error on '\"$2\"': Permission denied' >&2\nexit 1\n"
            ),
        );
        assert!(matches!(
            probe_holders_with(failing.as_os_str(), &target),
            HolderEvidence::Unknown(_)
        ));

        let _ = std::fs::remove_dir_all(&bin);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn data_rows_with_a_valid_pid_are_held_even_with_some_unparsable_siblings() {
        let evidence = interpret_lsof_output(
            Some(0),
            "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\ncargo 4242 kc cwd DIR 1,4 320 12345 /tmp/x\ngarbled ??? row\n",
            "",
        );
        let HolderEvidence::Held(procs) = evidence else {
            panic!("a data row with a valid pid must be Held even alongside a garbled sibling row");
        };
        assert!(procs.iter().any(|p| p.pid == 4242));
    }
}
