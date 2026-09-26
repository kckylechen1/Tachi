//! Tri-state probe: does a live tachi daemon hold a DB file open?
//!
//! Extracted from `doctor::autofix` (#1323/#1341) so every call site that is
//! about to perform a non-atomic, multi-file copy of a SQLite DB (main +
//! `-wal` + `-shm`) can reuse the same fail-closed probe instead of each
//! inventing its own liveness check. A raw filesystem copy of those files
//! while a daemon is actively writing produces a torn snapshot; the only
//! copy that is provably safe is one where ownership resolved cleanly to
//! `NotOwned`.

use std::path::Path;

/// Tri-state result of probing whether a live daemon holds `db_path` open.
/// `Unknown` must fail closed at every call site: it is not a synonym for
/// `NotOwned`, and callers must never fall through to an unguarded copy on
/// `Unknown` — that was the fail-open bug (torn main+wal+shm copies plus a
/// `wal_checkpoint(TRUNCATE)` against a partial WAL, reported as `outcome=ok`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DbOwnership {
    /// lsof ran cleanly and found at least one process with the file open.
    Owned,
    /// lsof ran cleanly and found no holders.
    NotOwned,
    /// Ownership could not be determined; `reason` is surfaced verbatim in
    /// the caller's receipt/error note (never swallowed).
    Unknown(String),
}

/// Best-effort detection: does some *other* running tachi-tachi-server have
/// an open file handle on `db_path`? Uses `lsof` on Unix, excluding the
/// calling process's own PID from the holder count — a caller may
/// legitimately hold its own connection to `db_path` at the moment it
/// probes (see `count_other_holders`), and that is not a concurrent daemon.
/// Only a clean lsof run (readable exit status + output) may resolve to
/// `Owned`/`NotOwned`; canonicalize failure, a missing/erroring `lsof`, and
/// non-unix platforms all return `Unknown` — the caller must fail closed on
/// that, not fall through to the unguarded copy path.
#[cfg(unix)]
fn probe_db_ownership(db_path: &Path) -> DbOwnership {
    probe_db_ownership_with(std::ffi::OsStr::new("lsof"), db_path)
}

/// [`probe_db_ownership`] with the `lsof` program injectable, so tests can
/// drive the real call site (argv, stream capture, classification) with a
/// stub instead of the host's lsof.
#[cfg(unix)]
fn probe_db_ownership_with(lsof: &std::ffi::OsStr, db_path: &Path) -> DbOwnership {
    use std::process::Command;
    let abs = match db_path.canonicalize() {
        Ok(p) => p,
        Err(e) => return DbOwnership::Unknown(format!("canonicalize failed: {e}")),
    };
    // Using `--` to terminate options before the path argument so paths
    // beginning with `-` are treated literally.
    let output = Command::new(lsof).arg("--").arg(&abs).output();
    match output {
        Ok(o) => classify_lsof_run(
            o.status.code(),
            &String::from_utf8_lossy(&o.stdout),
            &o.stderr,
            // Both spellings: the caller's path (which may be a symlinked
            // alias) and the canonical path lsof was actually asked about.
            &[db_path, &abs],
            std::process::id(),
        ),
        Err(e) => DbOwnership::Unknown(format!("lsof unavailable: {e}")),
    }
}

/// Classify a raw `lsof -- <target>` run. stderr is first reduced, as raw
/// bytes, to the diagnostics that bear on `targets` (tachi#1978): on Linux,
/// lsof prints a warning for the `tracefs` mount it cannot stat() as a
/// non-root user on every run, which turned lsof's ordinary silent "no
/// holders" exit 1 into `Unknown` on every Linux host. Only that exact
/// warning pair, for a mount proven disjoint from every target spelling, is
/// dropped (`tachi_clean::lsof_stderr`, identity off Linux); everything else
/// reaches [`classify_lsof_output`] and fails closed.
#[cfg(unix)]
fn classify_lsof_run(
    status_code: Option<i32>,
    stdout: &str,
    stderr: &[u8],
    targets: &[&Path],
    self_pid: u32,
) -> DbOwnership {
    let relevant = tachi_clean::lsof_stderr::relevant_lsof_stderr(stderr, targets);
    classify_lsof_output(
        status_code,
        stdout,
        &String::from_utf8_lossy(&relevant),
        self_pid,
    )
}

/// Pure classification of an `lsof` invocation's exit signature into
/// ownership — no process execution, so every signature below is
/// unit-testable without needing to actually kill `lsof` mid-run (a
/// genuinely signal-killed subprocess is not reliably reproducible in an
/// integration-style test).
///
/// `status_code` is `Option<i32>` per `std::process::ExitStatus::code()`:
/// `None` means the process was terminated by a signal on unix (SIGKILL,
/// an OOM kill, ...) rather than exiting normally — that case must be
/// checked *before* the "both streams empty" heuristic below, because a
/// signal-killed `lsof` also leaves both streams empty and would otherwise
/// be misread as the harmless "no holders found" signature.
///
/// `self_pid` excludes the calling process's own holder line: this process
/// itself may legitimately have `db_path` open (a read-only migration scan,
/// an in-progress doctor read, ...) at the moment it probes, and that is not
/// a concurrent daemon — only some *other* process holding the file open is
/// a torn-copy risk. A holder line whose PID field cannot be parsed is
/// counted as a holder anyway (fail closed, never silently excluded).
///
/// `NotOwned` is earned, never defaulted. lsof is given an explicit target
/// (no `-Q`), so its exit contract is: 0 only when the target was found and
/// listed, 1 when it was not found. Real unheld targets give exit 1 with both
/// streams empty (lsof 4.95.0 on atom-dgx-2 and 4.91 on macOS). So exactly
/// two signatures are `NotOwned`: that silent exit 1, and exit 0 whose rows
/// (after lsof's default header) were all this process's own — proof that
/// lsof listed the file and only we hold it.
///
/// Signatures:
/// - `None` (signal death) → `Unknown("lsof terminated by signal")`,
///   regardless of stream contents
/// - `Some(0)`, stdout whose first line is not lsof's default header →
///   `Unknown`
/// - `Some(0)` + at least one non-self holder line → `Owned`
/// - `Some(0)` + no holder rows at all (empty or header-only stdout) →
///   `Unknown`: an exit-0 run that listed nothing contradicts lsof's contract
/// - `Some(0)` + self-only holder rows + any stderr → `Unknown`
/// - `Some(0)` + self-only holder rows + silent stderr → `NotOwned`
/// - `Some(1)` + both streams empty → `NotOwned` (lsof's normal, silent
///   "no holders" signature)
/// - `Some(1)` + text on either stream (a header included) → `Unknown` with
///   the first diagnostic line surfaced
/// - `Some(n)`, n ∉ {0, 1} → `Unknown`, whatever the streams hold
#[cfg(unix)]
fn classify_lsof_output(
    status_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    self_pid: u32,
) -> DbOwnership {
    let first_diagnostic = || {
        stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .or_else(|| stdout.lines().find(|l| !l.trim().is_empty()))
            .map(|l| l.trim().to_string())
    };
    match status_code {
        None => DbOwnership::Unknown("lsof terminated by signal".to_string()),
        Some(0) => {
            if !stdout.trim().is_empty()
                && !tachi_clean::lsof_stderr::starts_with_lsof_header(stdout)
            {
                return DbOwnership::Unknown(format!(
                    "lsof output unrecognized: {}",
                    stdout.lines().next().unwrap_or("").trim()
                ));
            }
            // lsof prints a header line + one line per holder.
            if count_other_holders(stdout, self_pid) > 0 {
                DbOwnership::Owned
            } else if count_holder_rows(stdout) == 0 {
                // Exit 0 means "found and listed"; nothing listed is an
                // anomaly, not an empty search (cold review round 2).
                DbOwnership::Unknown("lsof exited 0 without listing any file row".to_string())
            } else if !stderr.trim().is_empty() {
                DbOwnership::Unknown(format!(
                    "lsof error: {}",
                    first_diagnostic().unwrap_or_default()
                ))
            } else {
                DbOwnership::NotOwned
            }
        }
        Some(1) => {
            // Exit 1 is ambiguous by design: "no holders found" (silent on
            // both streams) or a genuine fault (permission denied, lsof
            // internal error, unexpected args). Only the silent form is
            // trusted; any text means we cannot tell, so fail closed.
            if stderr.trim().is_empty() && stdout.trim().is_empty() {
                DbOwnership::NotOwned
            } else {
                DbOwnership::Unknown(format!(
                    "lsof error: {}",
                    first_diagnostic().unwrap_or_else(|| "lsof exited 1".to_string())
                ))
            }
        }
        Some(code) => DbOwnership::Unknown(match first_diagnostic() {
            Some(detail) => format!("lsof exited with status {code}: {detail}"),
            None => format!("lsof exited with status {code}"),
        }),
    }
}

/// Count every non-blank lsof row after the header line.
#[cfg(unix)]
fn count_holder_rows(stdout: &str) -> usize {
    stdout
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .count()
}

/// Count lsof holder lines (skipping the header line) whose PID differs
/// from `self_pid`. A line whose PID field can't be parsed is counted as a
/// holder anyway — an unparsable line is not proof it's harmless, so this
/// fails toward `Owned`/blocking rather than silently excluding it.
#[cfg(unix)]
fn count_other_holders(stdout: &str, self_pid: u32) -> usize {
    stdout
        .lines()
        .skip(1) // header: "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME"
        .filter(|line| !line.trim().is_empty())
        .filter(|line| {
            line.split_whitespace()
                .nth(1)
                .and_then(|field| field.parse::<u32>().ok())
                .map(|pid| pid != self_pid)
                .unwrap_or(true)
        })
        .count()
}

#[cfg(not(unix))]
fn probe_db_ownership(_db_path: &Path) -> DbOwnership {
    DbOwnership::Unknown("unsupported platform (no ownership probe on non-unix)".to_string())
}

// Test inject: force `daemon_ownership` to return a specific result without
// depending on real lsof behavior (a genuinely erroring lsof is not
// reliably reproducible across CI environments). `pub(crate)` so every
// call-site module's own tests can drive it via `set_ownership_inject_for_test`.
#[cfg(any(test, all(feature = "bootstrap-test-api", unix)))]
thread_local! {
    static INJECT_OWNERSHIP: std::cell::RefCell<Option<DbOwnership>> =
        const { std::cell::RefCell::new(None) };
}

/// Probe whether a live daemon holds `db_path` open. Every non-atomic,
/// multi-file DB copy site in this crate must route through this function
/// (not `probe_db_ownership` directly) so tests can deterministically force
/// `Owned`/`Unknown` without a real lsof/daemon fixture.
pub(crate) fn daemon_ownership(db_path: &Path) -> DbOwnership {
    #[cfg(any(test, all(feature = "bootstrap-test-api", unix)))]
    {
        if let Some(forced) = INJECT_OWNERSHIP.with(|c| c.borrow_mut().take()) {
            return forced;
        }
    }
    probe_db_ownership(db_path)
}

/// Test-only hook: force the next `daemon_ownership` call (on any thread-local
/// caller in this test) to return `value`, or clear the override with `None`.
#[cfg(any(test, all(feature = "bootstrap-test-api", unix)))]
pub(crate) fn set_ownership_inject_for_test(value: Option<DbOwnership>) {
    INJECT_OWNERSHIP.with(|c| *c.borrow_mut() = value);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Dummy "self" PID used by tests whose fixture holder lines represent
    /// some *other* process (never matches the `123` etc. used below), so
    /// existing Owned/NotOwned assertions keep meaning what they said before
    /// `classify_lsof_output` grew the self-exclusion parameter.
    const OTHER_SELF_PID: u32 = 999_999;

    /// lsof's real default header (lsof 4.95.0 / 4.91). The pre-#1978
    /// fixtures used a truncated `COMMAND  PID USER  FD TYPE` header, which
    /// round 2's exact-header check rightly rejects; their expected
    /// classifications are unchanged.
    const H: &str = "COMMAND  PID USER  FD TYPE DEVICE SIZE/OFF NODE NAME\n";

    #[test]
    fn classify_success_multiple_holders_is_owned() {
        let header_plus_holder = &format!("{H}tachi  123 kyle  10r REG 1,4 0 1 /x\n");
        assert_eq!(
            classify_lsof_output(Some(0), header_plus_holder, "", OTHER_SELF_PID),
            DbOwnership::Owned
        );
    }

    /// Tightened in #1978 cold review round 2 (lead-authorized, fail-closed
    /// direction). Was `classify_success_no_holder_lines_is_not_owned`,
    /// asserting `NotOwned`. lsof was given an explicit target without `-Q`,
    /// so exit 0 means "found and listed": exit 0 that lists no row at all is
    /// an anomalous signature, not an empty search, and must be `Unknown`.
    /// (Self-only rows — real listing evidence — stay `NotOwned`; see
    /// `classify_self_only_holder_is_not_owned`.)
    #[test]
    fn classify_success_without_any_row_is_unknown() {
        for stdout in ["", H] {
            assert!(
                matches!(
                    classify_lsof_output(Some(0), stdout, "", OTHER_SELF_PID),
                    DbOwnership::Unknown(_)
                ),
                "{stdout:?}"
            );
        }
    }

    #[test]
    fn classify_self_only_holder_is_not_owned() {
        // The caller (e.g. a migration's own read-only connection) may be
        // the only "holder" lsof reports for its own PID — that must not
        // register as a live daemon.
        let header_plus_self = &format!("{H}tachi  4242 kyle  10r REG 1,4 0 1 /x\n");
        assert_eq!(
            classify_lsof_output(Some(0), header_plus_self, "", 4242),
            DbOwnership::NotOwned
        );
    }

    #[test]
    fn classify_self_and_other_holder_is_owned() {
        // Self holds it AND some other process holds it too — the other
        // holder still makes this Owned.
        let header_self_and_other = &format!(
            "{H}tachi  4242 kyle  10r REG 1,4 0 1 /x\ntachi  777 kyle  11r REG 1,4 0 1 /x\n"
        );
        assert_eq!(
            classify_lsof_output(Some(0), header_self_and_other, "", 4242),
            DbOwnership::Owned
        );
    }

    #[test]
    fn classify_unparsable_pid_field_counts_as_holder() {
        // A holder line whose PID field can't be parsed as u32 is not proof
        // it's harmless (e.g. self) — fail closed and count it.
        let header_plus_garbled = &format!("{H}tachi  ??? kyle  10r REG 1,4 0 1 /x\n");
        assert_eq!(
            classify_lsof_output(Some(0), header_plus_garbled, "", OTHER_SELF_PID),
            DbOwnership::Owned
        );
    }

    #[test]
    fn classify_nonzero_exit_both_streams_empty_is_not_owned() {
        // lsof's normal, silent "no holders found" signature: exit 1, not a
        // fault, both streams empty.
        assert_eq!(
            classify_lsof_output(Some(1), "", "", OTHER_SELF_PID),
            DbOwnership::NotOwned
        );
    }

    #[test]
    fn classify_signal_death_is_unknown_even_with_empty_streams() {
        // The bug this test guards: a signal-killed lsof (SIGKILL/OOM) also
        // leaves both streams empty, which is indistinguishable from the
        // "no holders" signature by stream contents alone. `status_code()
        // == None` must be checked first and must never resolve to
        // `NotOwned`.
        assert_eq!(
            classify_lsof_output(None, "", "", OTHER_SELF_PID),
            DbOwnership::Unknown("lsof terminated by signal".to_string())
        );
        // Also must not be swayed by incidental non-empty output preceding
        // the kill — signal death always wins.
        assert_eq!(
            classify_lsof_output(None, H, "some partial text", OTHER_SELF_PID),
            DbOwnership::Unknown("lsof terminated by signal".to_string())
        );
    }

    #[test]
    fn classify_nonzero_exit_with_diagnostic_is_unknown() {
        let unknown = classify_lsof_output(
            Some(1),
            "",
            "lsof: status error on /x: No such file or directory\n",
            OTHER_SELF_PID,
        );
        match unknown {
            DbOwnership::Unknown(reason) => {
                assert!(
                    reason.contains("lsof: status error on /x"),
                    "reason must surface the diagnostic line, got: {reason}"
                );
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn classify_signal_death_distinct_from_clean_not_owned() {
        // Regression guard: signal death and the ordinary silent-exit
        // NotOwned signature must never collapse to the same variant even
        // though both present as "both streams empty".
        assert_ne!(
            classify_lsof_output(None, "", "", OTHER_SELF_PID),
            classify_lsof_output(Some(1), "", "", OTHER_SELF_PID)
        );
    }

    /// Byte-for-byte stderr of lsof 4.95.0 on Ubuntu 24.04 as a non-root
    /// user, printed on every run whatever the query (atom-dgx-2, tachi#1978).
    const LINUX_TRACEFS_WARNING: &[u8] = b"lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\n";
    const HEADER: &[u8] = b"COMMAND  PID USER  FD TYPE DEVICE SIZE/OFF NODE NAME\n";

    fn tracefs_pair_for(mount: &Path) -> Vec<u8> {
        let mut bytes = b"lsof: WARNING: can't stat() tracefs file system ".to_vec();
        bytes.extend_from_slice(mount.as_os_str().as_encoded_bytes());
        bytes.extend_from_slice(b"\n      Output information may be incomplete.\n");
        bytes
    }

    /// A stub `lsof` that checks it was asked `-- <canonical db>` and then
    /// replays exact stdout/stderr bytes and an exit status.
    struct StubLsof {
        dir: tempfile::TempDir,
    }

    impl StubLsof {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().expect("stub dir"),
            }
        }

        fn probe(
            &self,
            name: &str,
            db: &Path,
            stdout: &[u8],
            stderr: &[u8],
            code: i32,
        ) -> DbOwnership {
            use std::os::unix::fs::PermissionsExt;
            let case = self.dir.path().join(name);
            std::fs::create_dir(&case).unwrap();
            std::fs::write(case.join("stdout"), stdout).unwrap();
            std::fs::write(case.join("stderr"), stderr).unwrap();
            let script = case.join("lsof");
            let expected = db.canonicalize().unwrap();
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\n[ \"$1\" = '--' ] && [ \"$2\" = '{}' ] || {{ echo 'STUB ARGV MISMATCH' >&2; exit 93; }}\ncat '{}/stdout'\ncat '{}/stderr' >&2\nexit {code}\n",
                    expected.display(),
                    case.display(),
                    case.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = probe_db_ownership_with(script.as_os_str(), db);
            if let DbOwnership::Unknown(reason) = &result {
                assert!(!reason.contains("STUB ARGV MISMATCH"), "{name}: {reason}");
            }
            result
        }
    }

    fn fixture_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("memory.db");
        std::fs::write(&db, b"x").expect("seed");
        (dir, db)
    }

    fn assert_unknown(case: &str, result: DbOwnership) {
        assert!(
            matches!(result, DbOwnership::Unknown(_)),
            "{case}: expected Unknown, got {result:?}"
        );
    }

    /// The only raw signature the tracefs filter may turn into `NotOwned`,
    /// and only on Linux (round-1 finding 8: macOS unchanged). Round 2: a
    /// header-only exit 0 is no longer one of them (lead-authorized
    /// tightening; it was `NotOwned` on Linux in `0d1d61877`).
    #[test]
    fn stub_tracefs_only_silent_exit_1_is_not_owned_on_linux_only() {
        let (_dir, db) = fixture_db();
        let stub = StubLsof::new();
        let result = stub.probe("exit1", &db, b"", LINUX_TRACEFS_WARNING, 1);
        if cfg!(target_os = "linux") {
            assert_eq!(result, DbOwnership::NotOwned);
        } else {
            assert_unknown("exit1", result);
        }
        assert_unknown(
            "exit0-header",
            stub.probe("exit0-header", &db, HEADER, LINUX_TRACEFS_WARNING, 0),
        );
    }

    /// Round 2 (BUG-A): exit 0 that lists no row is Unknown on every
    /// platform, with or without the warning; exit 1 with a header is too.
    #[test]
    fn stub_exit_0_without_rows_and_exit_1_with_a_header_stay_unknown() {
        let (_dir, db) = fixture_db();
        let stub = StubLsof::new();
        for (name, stdout, stderr, code) in [
            ("e0-empty", &b""[..], &b""[..], 0),
            ("e0-header", HEADER, &b""[..], 0),
            ("e0-empty-w", &b""[..], LINUX_TRACEFS_WARNING, 0),
            ("e1-header", HEADER, &b""[..], 1),
            ("e1-header-w", HEADER, LINUX_TRACEFS_WARNING, 1),
        ] {
            assert_unknown(name, stub.probe(name, &db, stdout, stderr, code));
        }
    }

    /// Round 2 over-refusal guard: rows that are all this process's own are
    /// real listing evidence and stay `NotOwned` (with the tracefs warning on
    /// Linux too).
    #[test]
    fn stub_self_only_rows_stay_not_owned() {
        let (_dir, db) = fixture_db();
        let stub = StubLsof::new();
        let mut stdout = HEADER.to_vec();
        stdout.extend_from_slice(
            format!("tachi {} kyle 10r REG 1,4 0 1 /x\n", std::process::id()).as_bytes(),
        );
        assert_eq!(
            stub.probe("self", &db, &stdout, b"", 0),
            DbOwnership::NotOwned
        );
        let with_warning = stub.probe("self-w", &db, &stdout, LINUX_TRACEFS_WARNING, 0);
        if cfg!(target_os = "linux") {
            assert_eq!(with_warning, DbOwnership::NotOwned);
        } else {
            assert_unknown("self-w", with_warning);
        }
    }

    #[test]
    fn stub_holder_next_to_the_warning_is_owned() {
        let (_dir, db) = fixture_db();
        let mut stdout = HEADER.to_vec();
        stdout.extend_from_slice(b"tachi  123 kyle  10r REG 1,4 0 1 /x\n");
        assert_eq!(
            StubLsof::new().probe("held", &db, &stdout, LINUX_TRACEFS_WARNING, 0),
            DbOwnership::Owned
        );
    }

    /// Round-1 finding 2: only exit 1 means "no matching files".
    #[test]
    fn stub_odd_exit_codes_stay_unknown_after_filtering() {
        let (_dir, db) = fixture_db();
        let stub = StubLsof::new();
        for code in [2, 3, 126, 127] {
            assert_unknown(
                &format!("exit {code}"),
                stub.probe(
                    &format!("exit{code}"),
                    &db,
                    b"",
                    LINUX_TRACEFS_WARNING,
                    code,
                ),
            );
            assert_unknown(
                &format!("silent exit {code}"),
                stub.probe(&format!("silent{code}"), &db, b"", b"", code),
            );
        }
    }

    /// Byte-for-byte stderr of lsof 4.95.0 on atom-dgx-2 run as the CI user
    /// `gha` (tachi#1978 follow-up: the desktop owner's portal mount).
    const GHA_TRACEFS_AND_PORTAL: &[u8] = b"lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\nlsof: WARNING: can't stat() fuse.portal file system /run/user/1000/doc\n      Output information may be incomplete.\n";

    fn portal_pair_for(mount: &Path) -> Vec<u8> {
        let mut bytes = b"lsof: WARNING: can't stat() fuse.portal file system ".to_vec();
        bytes.extend_from_slice(mount.as_os_str().as_encoded_bytes());
        bytes.extend_from_slice(b"\n      Output information may be incomplete.\n");
        bytes
    }

    #[test]
    fn stub_portal_pair_is_dropped_only_when_disjoint_and_paired() {
        let (dir, db) = fixture_db();
        let stub = StubLsof::new();
        let result = stub.probe("gha", &db, b"", GHA_TRACEFS_AND_PORTAL, 1);
        if cfg!(target_os = "linux") {
            assert_eq!(result, DbOwnership::NotOwned);
        } else {
            assert_unknown("gha", result);
        }
        let parent = dir.path().canonicalize().unwrap();
        for (name, mount) in [
            ("db-dir", parent.clone()),
            ("ancestor", parent.parent().unwrap().to_path_buf()),
            ("same", db.canonicalize().unwrap()),
        ] {
            let mut stderr = LINUX_TRACEFS_WARNING.to_vec();
            stderr.extend_from_slice(&portal_pair_for(&mount));
            assert_unknown(name, stub.probe(name, &db, b"", &stderr, 1));
        }
        let mut lone = LINUX_TRACEFS_WARNING.to_vec();
        lone.extend_from_slice(
            b"lsof: WARNING: can't stat() fuse.portal file system /run/user/1000/doc\n",
        );
        assert_unknown("lone", stub.probe("lone", &db, b"", &lone, 1));
    }

    /// Round-1 finding 3: only the observed tracefs pair is accepted.
    #[test]
    fn stub_other_file_systems_and_lone_warning_lines_stay_unknown() {
        let (_dir, db) = fixture_db();
        let stub = StubLsof::new();
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
            assert_unknown(name, stub.probe(name, &db, b"", stderr, 1));
        }
    }

    /// Round-1 finding 4: a non-UTF-8 line is never decoded and dismissed.
    #[test]
    fn stub_non_utf8_warning_stays_unknown() {
        let (_dir, db) = fixture_db();
        let stderr = b"lsof: WARNING: can't stat() tracefs file system /tmp/m-\xff\n      Output information may be incomplete.\n";
        assert_unknown(
            "non-utf8",
            StubLsof::new().probe("nonutf8", &db, b"", stderr, 1),
        );
    }

    /// Round-1 finding 5: the caller's raw (symlinked) spelling is compared
    /// too, not only the canonical path handed to lsof.
    #[test]
    fn stub_warning_over_the_callers_alias_stays_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        let aliases = dir.path().join("aliases");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&aliases).unwrap();
        std::fs::write(real.join("memory.db"), b"x").unwrap();
        std::os::unix::fs::symlink(&real, aliases.join("link")).unwrap();
        let alias_db = aliases.join("link").join("memory.db");
        let stub = StubLsof::new();
        // Control: the same pair over an unrelated sibling is dropped on Linux.
        let control = stub.probe(
            "control",
            &alias_db,
            b"",
            &tracefs_pair_for(&dir.path().join("elsewhere")),
            1,
        );
        if cfg!(target_os = "linux") {
            assert_eq!(control, DbOwnership::NotOwned);
        }
        assert_unknown(
            "alias",
            stub.probe("alias", &alias_db, b"", &tracefs_pair_for(&aliases), 1),
        );
    }

    /// Round-1 finding 7: exit 0 with a relevant diagnostic is not NotOwned.
    #[test]
    fn stub_exit_0_with_a_real_diagnostic_stays_unknown() {
        let (_dir, db) = fixture_db();
        let stub = StubLsof::new();
        let error = b"lsof: status error on /x: Permission denied\n";
        assert_unknown("exit0-header", stub.probe("e0h", &db, HEADER, error, 0));
        assert_unknown("exit0-empty", stub.probe("e0e", &db, b"", error, 0));
        let mut both = LINUX_TRACEFS_WARNING.to_vec();
        both.extend_from_slice(error);
        match stub.probe("e1", &db, b"", &both, 1) {
            // Off Linux the tracefs line is itself kept and surfaced first.
            DbOwnership::Unknown(reason) => assert!(
                !cfg!(target_os = "linux") || reason.contains("status error"),
                "{reason}"
            ),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// Round-1 finding 1 (DB side): stdout that is not lsof's header is never
    /// discarded as one.
    #[test]
    fn stub_unrecognized_stdout_stays_unknown() {
        let (_dir, db) = fixture_db();
        let stub = StubLsof::new();
        assert_unknown(
            "exit1",
            stub.probe("g1", &db, b"garbage\n", LINUX_TRACEFS_WARNING, 1),
        );
        assert_unknown("exit0", stub.probe("g0", &db, b"garbage\n", b"", 0));
    }

    #[test]
    fn stub_warning_over_the_db_directory_stays_unknown() {
        let (dir, db) = fixture_db();
        let parent = dir.path().canonicalize().unwrap();
        assert_unknown(
            "parent",
            StubLsof::new().probe("parent", &db, b"", &tracefs_pair_for(&parent), 1),
        );
    }

    /// End-to-end proof that the self-PID exclusion is narrowly scoped to
    /// THIS calling process: it must not swallow a genuinely external
    /// holder. Every other test in this file exercises the pure classifier
    /// or an injected result; this is the only one that drives the real
    /// `daemon_ownership` -> `probe_db_ownership` -> `lsof` path against an
    /// actual second process holding the file open.
    ///
    /// Holder is `tail -f <path>` (a single, non-forking process that opens
    /// the path directly) rather than this test's own connection — a
    /// same-process hold would be *excluded* by design and prove nothing
    /// about external holders. If `tail` can't be spawned or `lsof` is
    /// missing, the test skips with an explanation rather than failing —
    /// `probe_db_ownership` itself treats a missing `lsof` as `Unknown`, not
    /// a bug. Elsewhere any `Unknown` also skips, but on Linux an `Unknown`
    /// from an lsof that did run fails the test (tachi#1978: every non-root
    /// Linux run used to resolve to `Unknown` because of lsof's tracefs
    /// start-up warning, which a skip would have hidden).
    #[test]
    fn e2e_probe_sees_external_holder_and_clears_after_release() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("held.db");
        std::fs::write(&db_path, b"placeholder").expect("seed fixture file");

        let mut holder = match Command::new("tail")
            .arg("-f")
            .arg(&db_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                eprintln!(
                    "skipping e2e_probe_sees_external_holder_and_clears_after_release: \
                     could not spawn external holder process ({e})"
                );
                return;
            }
        };

        // Poll: `spawn()` returning doesn't mean `tail` has opened the file
        // yet, and this same probe is what must observe `Unknown` if `lsof`
        // itself is unavailable in this environment.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = daemon_ownership(&db_path);
        while Instant::now() < deadline && observed != DbOwnership::Owned {
            if matches!(observed, DbOwnership::Unknown(_)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            observed = daemon_ownership(&db_path);
        }

        match observed {
            DbOwnership::Owned => {
                // Confirmed: lsof genuinely detects the external holder, and
                // self-PID exclusion (a different PID here) did not swallow it.
            }
            DbOwnership::Unknown(reason) => {
                let _ = holder.kill();
                let _ = holder.wait();
                // tachi#1978: on Linux the only acceptable Unknown here is a
                // host without lsof. Any other Unknown (the tracefs start-up
                // warning misread as an error was one) is the bug itself.
                #[cfg(target_os = "linux")]
                assert!(
                    reason.starts_with("lsof unavailable:"),
                    "lsof ran but the probe could not decide on Linux: {reason}"
                );
                eprintln!(
                    "skipping e2e_probe_sees_external_holder_and_clears_after_release: \
                     lsof unavailable in this environment ({reason})"
                );
                return;
            }
            DbOwnership::NotOwned => {
                let _ = holder.kill();
                let _ = holder.wait();
                panic!(
                    "expected Owned (external holder present, or eventually \
                     Unknown if lsof is unavailable) but got NotOwned — \
                     external-holder detection is broken, not just self-exclusion"
                );
            }
        }

        holder.kill().expect("kill external holder process");
        holder
            .wait()
            .expect("wait for external holder process exit");

        // After the sole holder exits (fd closed), the probe must clear.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut cleared = daemon_ownership(&db_path);
        while Instant::now() < deadline && cleared != DbOwnership::NotOwned {
            std::thread::sleep(Duration::from_millis(50));
            cleared = daemon_ownership(&db_path);
        }
        assert_eq!(
            cleared,
            DbOwnership::NotOwned,
            "probe must clear to NotOwned after the external holder released the file"
        );
    }
}
