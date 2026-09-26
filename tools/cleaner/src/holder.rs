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
        &out.stderr,
        path,
    )
}

/// Classify a raw `lsof +D <target>` run: stderr is first reduced, as raw
/// bytes, to the diagnostics that bear on `target` (tachi#1978 — on Linux
/// only, lsof's exact tracefs start-up warning pair for a mount disjoint from
/// the target is dropped; see [`crate::lsof_stderr`]), then handed to the
/// fail-closed [`interpret_lsof_output`].
fn interpret_lsof_run(
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &[u8],
    target: &Path,
) -> HolderEvidence {
    let relevant = crate::lsof_stderr::relevant_lsof_stderr(stderr, &[target]);
    interpret_lsof_output(exit_code, stdout, &String::from_utf8_lossy(&relevant))
}

/// Pure interpreter for an `lsof +D` run — the part worth testing in
/// isolation, without shelling out. Deliberately mirrors
/// `exec_env_reaper::interpret_lsof`'s fail-closed idiom exactly (tachi#1212
/// fix-round, codex checkpoint 4/5): that function is the sealed, already
/// reviewed precedent for the SAME kind of evidence (an `lsof +D` walk) on
/// the destructive (#1062) side; this module borrows its interpretation,
/// never its destructive consequence — this module only ever detects.
///
/// * non-blank stdout whose first line is not lsof's default header ⇒
///   [`HolderEvidence::Unknown`]: the first line is only skipped once it is
///   proven to be the header, never discarded unvalidated
/// * any data row on stdout that yields a parseable pid ⇒ [`HolderEvidence::Held`]
/// * data rows present but NONE yield a parseable pid ⇒ [`HolderEvidence::Unknown`]:
///   the walk produced output we could not attribute, which is not proof of
///   absence (earned-not-defaulted: `Clear` must never be the fallback for
///   evidence we failed to parse).
/// * no data rows, but anything on stderr ⇒ [`HolderEvidence::Unknown`]: lsof
///   warns (e.g. "can't stat()", "Permission denied") when it could not
///   descend part of the tree, and a partial walk that "found nothing" is
///   not proof of nothing.
/// * no data rows, no stderr noise, exit 1 AND blank stdout ⇒
///   [`HolderEvidence::Clear`]: lsof's documented "not found" signature, and
///   what real lsof prints for an unheld dir (4.95.0 on atom-dgx-2, 4.91 on
///   macOS)
/// * no data rows otherwise ⇒ [`HolderEvidence::Unknown`]: exit 0 means lsof
///   found and listed files, so exit 0 with nothing listed is anomalous, and a
///   header with no rows on exit 1 is not the silent "not found" signature
///   (cold review round 2; tightened, lead-authorized)
/// * any other exit / signal ⇒ [`HolderEvidence::Unknown`]
fn interpret_lsof_output(exit_code: Option<i32>, stdout: &str, stderr: &str) -> HolderEvidence {
    if !stdout.trim().is_empty() && !crate::lsof_stderr::starts_with_lsof_header(stdout) {
        return HolderEvidence::Unknown(format!(
            "lsof output unrecognized: {}",
            stdout.lines().next().unwrap_or("").trim()
        ));
    }
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
        // Absence is bounded by what this lsof can see: see "Visibility
        // boundary" in crate::lsof_stderr (tachi#1989).
        Some(1) if stdout.trim().is_empty() => HolderEvidence::Clear,
        Some(1) => {
            HolderEvidence::Unknown("lsof exited 1 with a header but no file rows".to_string())
        }
        Some(0) => {
            HolderEvidence::Unknown("lsof exited 0 without listing any file row".to_string())
        }
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

    /// Tightened in #1978 cold review round 2 (lead-authorized, fail-closed
    /// direction). Was `clean_empty_run_is_clear`, asserting `Clear` for exit
    /// 1 with a header-only stdout. lsof prints its header only while listing
    /// a file, and real lsof on an unheld dir exits 1 with BOTH streams empty
    /// (4.95.0 on atom-dgx-2, 4.91 on macOS) — that silent run is the clean
    /// one. A header with no rows, or exit 0 listing nothing, is anomalous.
    #[test]
    fn only_the_silent_exit_1_run_is_clear() {
        assert_eq!(
            interpret_lsof_output(Some(1), "", ""),
            HolderEvidence::Clear
        );
        for (code, stdout) in [
            (1, "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n"),
            (0, "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n"),
            (0, ""),
        ] {
            assert!(
                matches!(
                    interpret_lsof_output(Some(code), stdout, ""),
                    HolderEvidence::Unknown(_)
                ),
                "exit {code} {stdout:?}"
            );
        }
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

    // --- tachi#1978: lsof's Linux tracefs start-up warning, via stub lsof ---

    /// Byte-for-byte stderr of lsof 4.95.0 on Ubuntu 24.04 as a non-root user,
    /// printed on every run whatever the query (captured on atom-dgx-2).
    const LINUX_TRACEFS_WARNING: &[u8] = b"lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\n";
    const HEADER: &[u8] = b"COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n";

    fn tracefs_pair_for(mount: &Path) -> Vec<u8> {
        let mut bytes = b"lsof: WARNING: can't stat() tracefs file system ".to_vec();
        bytes.extend_from_slice(mount.as_os_str().as_encoded_bytes());
        bytes.extend_from_slice(b"\n      Output information may be incomplete.\n");
        bytes
    }

    /// Drive the real call site (`Command`, argv, byte capture, stderr
    /// filtering, classification) with a stub `lsof` that checks it was asked
    /// `+D <target>` and replays exact stdout/stderr bytes and an exit code.
    #[cfg(unix)]
    fn stub_probe(
        name: &str,
        target: &Path,
        stdout: &[u8],
        stderr: &[u8],
        code: i32,
    ) -> HolderEvidence {
        use std::os::unix::fs::PermissionsExt;
        let case = unique_temp_dir(&format!("holder-1978-stub-{name}"));
        std::fs::write(case.join("stdout"), stdout).unwrap();
        std::fs::write(case.join("stderr"), stderr).unwrap();
        let script = case.join("lsof");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$1\" = '+D' ] && [ \"$2\" = '{}' ] || {{ echo 'STUB ARGV MISMATCH' >&2; exit 93; }}\ncat '{}/stdout'\ncat '{}/stderr' >&2\nexit {code}\n",
                target.display(),
                case.display(),
                case.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let evidence = probe_holders_with(script.as_os_str(), target);
        let _ = std::fs::remove_dir_all(&case);
        if let HolderEvidence::Unknown(reason) = &evidence {
            assert!(!reason.contains("STUB ARGV MISMATCH"), "{name}: {reason}");
        }
        evidence
    }

    #[cfg(unix)]
    fn assert_unknown(case: &str, evidence: HolderEvidence) {
        assert!(
            matches!(evidence, HolderEvidence::Unknown(_)),
            "{case}: expected Unknown, got {evidence:?}"
        );
    }

    /// The only signature the filter may turn into `Clear`, and only on Linux
    /// (round-1 cold review finding 8: other platforms are unchanged). Round
    /// 2: header-only exit 1 is no longer one of them (lead-authorized
    /// tightening; it was `Clear` on Linux in `0d1d61877`).
    #[cfg(unix)]
    #[test]
    fn stub_tracefs_only_empty_walk_is_clear_on_linux_only() {
        let target = unique_temp_dir("holder-1978-clear");
        let evidence = stub_probe("exit1", &target, b"", LINUX_TRACEFS_WARNING, 1);
        if cfg!(target_os = "linux") {
            assert_eq!(evidence, HolderEvidence::Clear);
        } else {
            assert_unknown("exit1", evidence);
        }
        let _ = std::fs::remove_dir_all(&target);
    }

    /// Round 2 (BUG-A/BUG-B): exit 0 listing no row, and exit 1 with a bare
    /// header, are Unknown on every platform, with or without the warning.
    #[cfg(unix)]
    #[test]
    fn stub_exit_0_without_rows_and_exit_1_with_a_header_stay_unknown() {
        let target = unique_temp_dir("holder-1978-norows");
        for (name, stdout, stderr, code) in [
            ("e0-empty", &b""[..], &b""[..], 0),
            ("e0-header", HEADER, &b""[..], 0),
            ("e0-empty-w", &b""[..], LINUX_TRACEFS_WARNING, 0),
            ("e1-header", HEADER, &b""[..], 1),
            ("e1-header-w", HEADER, LINUX_TRACEFS_WARNING, 1),
        ] {
            assert_unknown(name, stub_probe(name, &target, stdout, stderr, code));
        }
        let _ = std::fs::remove_dir_all(&target);
    }

    #[cfg(unix)]
    #[test]
    fn stub_holder_next_to_the_warning_is_held() {
        let target = unique_temp_dir("holder-1978-held");
        let pid = std::process::id();
        let mut stdout = HEADER.to_vec();
        stdout.extend_from_slice(format!("x {pid} u 3r REG 1,4 0 1 /f\n").as_bytes());
        let evidence = stub_probe("held", &target, &stdout, LINUX_TRACEFS_WARNING, 0);
        let HolderEvidence::Held(procs) = &evidence else {
            panic!("a holder row must be Held despite the warning: {evidence:?}");
        };
        assert!(procs.iter().any(|p| p.pid == pid as i32));
        let _ = std::fs::remove_dir_all(&target);
    }

    /// Round-1 finding 1: an unvalidated first stdout line is never skipped.
    #[cfg(unix)]
    #[test]
    fn stub_unrecognized_stdout_stays_unknown() {
        let target = unique_temp_dir("holder-1978-garbage");
        assert_unknown(
            "exit1+warning",
            stub_probe("g1w", &target, b"garbage\n", LINUX_TRACEFS_WARNING, 1),
        );
        assert_unknown("exit1", stub_probe("g1", &target, b"garbage\n", b"", 1));
        assert_unknown("exit0", stub_probe("g0", &target, b"garbage\n", b"", 0));
        let _ = std::fs::remove_dir_all(&target);
    }

    /// Round-1 finding 2: an odd exit status is never "no holder".
    #[cfg(unix)]
    #[test]
    fn stub_odd_exit_codes_stay_unknown() {
        let target = unique_temp_dir("holder-1978-exit");
        for code in [2, 3, 126, 127] {
            assert_unknown(
                &format!("exit {code}"),
                stub_probe(
                    &format!("x{code}"),
                    &target,
                    b"",
                    LINUX_TRACEFS_WARNING,
                    code,
                ),
            );
        }
        let _ = std::fs::remove_dir_all(&target);
    }

    /// Byte-for-byte stderr of lsof 4.95.0 on atom-dgx-2 run as the CI user
    /// `gha`, who cannot stat the desktop owner's xdg-document-portal mount
    /// (tachi#1978 follow-up).
    const GHA_TRACEFS_AND_PORTAL: &[u8] = b"lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\nlsof: WARNING: can't stat() fuse.portal file system /run/user/1000/doc\n      Output information may be incomplete.\n";

    fn portal_pair_for(mount: &Path) -> Vec<u8> {
        let mut bytes = b"lsof: WARNING: can't stat() fuse.portal file system ".to_vec();
        bytes.extend_from_slice(mount.as_os_str().as_encoded_bytes());
        bytes.extend_from_slice(b"\n      Output information may be incomplete.\n");
        bytes
    }

    /// The disjoint portal pair is dropped like tracefs (Linux only; other
    /// platforms keep every diagnostic). A portal mount that is the target, an
    /// ancestor or a descendant, or a lone portal line, stays relevant.
    #[cfg(unix)]
    #[test]
    fn stub_portal_pair_is_dropped_only_when_disjoint_and_paired() {
        let target = unique_temp_dir("holder-1978-portal");
        let evidence = stub_probe("gha", &target, b"", GHA_TRACEFS_AND_PORTAL, 1);
        if cfg!(target_os = "linux") {
            assert_eq!(evidence, HolderEvidence::Clear);
        } else {
            assert_unknown("gha", evidence);
        }
        for (name, mount) in [
            ("same", target.clone()),
            ("ancestor", target.parent().unwrap().to_path_buf()),
            ("descendant", target.join("doc")),
        ] {
            let mut stderr = LINUX_TRACEFS_WARNING.to_vec();
            stderr.extend_from_slice(&portal_pair_for(&mount));
            assert_unknown(name, stub_probe(name, &target, b"", &stderr, 1));
        }
        let mut lone = LINUX_TRACEFS_WARNING.to_vec();
        lone.extend_from_slice(
            b"lsof: WARNING: can't stat() fuse.portal file system /run/user/1000/doc\n",
        );
        assert_unknown("lone", stub_probe("lone", &target, b"", &lone, 1));
        let _ = std::fs::remove_dir_all(&target);
    }

    /// Round-1 finding 3: only the observed tracefs pair is accepted.
    #[cfg(unix)]
    #[test]
    fn stub_other_file_systems_and_lone_warning_lines_stay_unknown() {
        let target = unique_temp_dir("holder-1978-fstype");
        let cases: [(&str, &[u8]); 3] = [
            (
                "nfs-pair",
                b"lsof: WARNING: can't stat() nfs file system /unrelated\n      Output information may be incomplete.\n",
            ),
            ("nfs-lone", b"lsof: WARNING: can't stat() nfs file system /unrelated\n"),
            (
                "tracefs-lone",
                b"lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n",
            ),
        ];
        for (name, stderr) in cases {
            assert_unknown(name, stub_probe(name, &target, b"", stderr, 1));
        }
        let _ = std::fs::remove_dir_all(&target);
    }

    /// Round-1 finding 4: a non-UTF-8 line is kept, never decoded away.
    #[cfg(unix)]
    #[test]
    fn stub_non_utf8_warning_stays_unknown() {
        let target = unique_temp_dir("holder-1978-bytes");
        let stderr = b"lsof: WARNING: can't stat() tracefs file system /tmp/m-\xff\n      Output information may be incomplete.\n";
        assert_unknown("non-utf8", stub_probe("nonutf8", &target, b"", stderr, 1));
        let _ = std::fs::remove_dir_all(&target);
    }

    /// Round-1 finding 5: the caller's spelling (a symlinked alias) is
    /// compared as well as the canonical one.
    #[cfg(unix)]
    #[test]
    fn stub_warning_over_the_callers_alias_stays_unknown() {
        let root = unique_temp_dir("holder-1978-alias");
        let real = root.join("real");
        let aliases = root.join("aliases");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&aliases).unwrap();
        let alias = aliases.join("link");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let control = stub_probe(
            "alias-control",
            &alias,
            b"",
            &tracefs_pair_for(&root.join("elsewhere")),
            1,
        );
        if cfg!(target_os = "linux") {
            assert_eq!(control, HolderEvidence::Clear);
        }
        assert_unknown(
            "alias",
            stub_probe("alias", &alias, b"", &tracefs_pair_for(&aliases), 1),
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Round-1 finding 7: a relevant diagnostic on a success exit is Unknown.
    #[cfg(unix)]
    #[test]
    fn stub_exit_0_with_a_real_diagnostic_stays_unknown() {
        let target = unique_temp_dir("holder-1978-exit0");
        let error = b"lsof: WARNING: can't opendir(/x/sub): Permission denied\n";
        assert_unknown("exit0-header", stub_probe("e0h", &target, HEADER, error, 0));
        let mut both = LINUX_TRACEFS_WARNING.to_vec();
        both.extend_from_slice(error);
        let evidence = stub_probe("e1", &target, b"", &both, 1);
        let HolderEvidence::Unknown(reason) = &evidence else {
            panic!("a partial walk must stay Unknown next to the warning: {evidence:?}");
        };
        // Off Linux the tracefs line is itself kept and surfaced first.
        if cfg!(target_os = "linux") {
            assert!(reason.contains("can't opendir"), "{reason}");
        }
        let _ = std::fs::remove_dir_all(&target);
    }

    #[cfg(unix)]
    #[test]
    fn stub_warning_inside_or_over_the_walk_stays_unknown() {
        let target = unique_temp_dir("holder-1978-inside");
        for (name, mount) in [
            ("inside", target.join("mnt")),
            ("over", target.parent().unwrap().to_path_buf()),
            ("same", target.clone()),
        ] {
            assert_unknown(
                name,
                stub_probe(name, &target, b"", &tracefs_pair_for(&mount), 1),
            );
        }
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
