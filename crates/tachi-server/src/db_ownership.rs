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
    let abs_str = abs.to_string_lossy().to_string();
    // Using `--` to terminate options before the path argument so paths
    // beginning with `-` are treated literally.
    let output = Command::new(lsof).arg("--").arg(&abs_str).output();
    match output {
        Ok(o) => classify_lsof_run(
            o.status.code(),
            &String::from_utf8_lossy(&o.stdout),
            &String::from_utf8_lossy(&o.stderr),
            &abs,
            std::process::id(),
        ),
        Err(e) => DbOwnership::Unknown(format!("lsof unavailable: {e}")),
    }
}

/// Classify a raw `lsof -- <target>` run. stderr is first reduced to the
/// diagnostics that bear on `target` (tachi#1978): lsof's Linux dialect prints
/// a warning for every mount-table entry it cannot stat() — `tracefs` at
/// `/sys/kernel/debug/tracing` for every non-root user — on every run, which
/// turned lsof's ordinary silent "no holders" exit 1 into `Unknown` on every
/// Linux host. Only a mount warning proven disjoint from `target` is dropped
/// (`tachi_clean::lsof_stderr`); any other diagnostic reaches
/// [`classify_lsof_output`] unchanged and still fails closed.
#[cfg(unix)]
fn classify_lsof_run(
    status_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    target: &Path,
    self_pid: u32,
) -> DbOwnership {
    classify_lsof_output(
        status_code,
        stdout,
        &tachi_clean::lsof_stderr::relevant_lsof_stderr(stderr, target),
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
/// Signatures:
/// - `Some(0)` + at least one non-self holder line → `Owned`
/// - `Some(0)` + header only, or only self-owned holder lines → `NotOwned`
/// - `Some(n)` (n != 0) + both streams empty → `NotOwned` (lsof's normal,
///   silent "no holders" exit-1 signature)
/// - `None` (signal death) → `Unknown("lsof terminated by signal")`,
///   regardless of stream contents
/// - `Some(n)` (n != 0) + diagnostic text on either stream → `Unknown`
///   with the first diagnostic line surfaced
#[cfg(unix)]
fn classify_lsof_output(
    status_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    self_pid: u32,
) -> DbOwnership {
    match status_code {
        Some(0) => {
            // lsof prints a header line + one line per holder.
            if count_other_holders(stdout, self_pid) > 0 {
                DbOwnership::Owned
            } else {
                DbOwnership::NotOwned
            }
        }
        None => DbOwnership::Unknown("lsof terminated by signal".to_string()),
        Some(_) => {
            // lsof's non-zero exit is ambiguous by design: it means either
            // "no holders found" (the common, harmless case — empty
            // stdout/stderr) or a genuine fault (permission denied, lsof
            // internal error, unexpected args). Only trust the "no holders"
            // reading when lsof stayed silent on both streams; any
            // diagnostic text means we cannot tell, so fail closed.
            if stderr.trim().is_empty() && stdout.trim().is_empty() {
                DbOwnership::NotOwned
            } else {
                let detail = stderr
                    .lines()
                    .next()
                    .filter(|l| !l.trim().is_empty())
                    .or_else(|| stdout.lines().next())
                    .unwrap_or("lsof exited non-zero")
                    .trim()
                    .to_string();
                DbOwnership::Unknown(format!("lsof error: {detail}"))
            }
        }
    }
}

/// Count lsof holder lines (skipping the header line) whose PID differs
/// from `self_pid`. A line whose PID field can't be parsed is counted as a
/// holder anyway — an unparsable line is not proof it's harmless, so this
/// fails toward `Owned`/blocking rather than silently excluding it.
#[cfg(unix)]
fn count_other_holders(stdout: &str, self_pid: u32) -> usize {
    stdout
        .lines()
        .skip(1) // header: "COMMAND  PID USER  FD TYPE ..."
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

    #[test]
    fn classify_success_multiple_holders_is_owned() {
        let header_plus_holder = "COMMAND  PID USER  FD TYPE\ntachi  123 kyle  10r REG\n";
        assert_eq!(
            classify_lsof_output(Some(0), header_plus_holder, "", OTHER_SELF_PID),
            DbOwnership::Owned
        );
    }

    #[test]
    fn classify_success_no_holder_lines_is_not_owned() {
        // Some(0) with header-only (or empty) stdout: exit succeeded but
        // nothing matched.
        assert_eq!(
            classify_lsof_output(Some(0), "", "", OTHER_SELF_PID),
            DbOwnership::NotOwned
        );
        assert_eq!(
            classify_lsof_output(Some(0), "COMMAND  PID USER  FD TYPE\n", "", OTHER_SELF_PID),
            DbOwnership::NotOwned
        );
    }

    #[test]
    fn classify_self_only_holder_is_not_owned() {
        // The caller (e.g. a migration's own read-only connection) may be
        // the only "holder" lsof reports for its own PID — that must not
        // register as a live daemon.
        let header_plus_self = "COMMAND  PID USER  FD TYPE\ntachi  4242 kyle  10r REG\n";
        assert_eq!(
            classify_lsof_output(Some(0), header_plus_self, "", 4242),
            DbOwnership::NotOwned
        );
    }

    #[test]
    fn classify_self_and_other_holder_is_owned() {
        // Self holds it AND some other process holds it too — the other
        // holder still makes this Owned.
        let header_self_and_other =
            "COMMAND  PID USER  FD TYPE\ntachi  4242 kyle  10r REG\ntachi  777 kyle  11r REG\n";
        assert_eq!(
            classify_lsof_output(Some(0), header_self_and_other, "", 4242),
            DbOwnership::Owned
        );
    }

    #[test]
    fn classify_unparsable_pid_field_counts_as_holder() {
        // A holder line whose PID field can't be parsed as u32 is not proof
        // it's harmless (e.g. self) — fail closed and count it.
        let header_plus_garbled = "COMMAND  PID USER  FD TYPE\ntachi  ??? kyle  10r REG\n";
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
            classify_lsof_output(
                None,
                "COMMAND  PID USER  FD TYPE\n",
                "some partial text",
                OTHER_SELF_PID
            ),
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
    const LINUX_TRACEFS_WARNING: &str = "lsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\n";

    #[test]
    fn classify_run_drops_only_an_unrelated_mount_warning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("memory.db");
        std::fs::write(&db, b"x").expect("seed");
        assert_eq!(
            classify_lsof_run(Some(1), "", LINUX_TRACEFS_WARNING, &db, OTHER_SELF_PID),
            DbOwnership::NotOwned,
            "lsof's silent no-holder exit plus the tracefs start-up warning is NotOwned"
        );
        // A holder is still Owned with the warning present.
        assert_eq!(
            classify_lsof_run(
                Some(0),
                "COMMAND  PID USER  FD TYPE\ntachi  123 kyle  10r REG\n",
                LINUX_TRACEFS_WARNING,
                &db,
                OTHER_SELF_PID
            ),
            DbOwnership::Owned
        );
        // A real error next to the warning stays Unknown and is surfaced.
        match classify_lsof_run(
            Some(1),
            "",
            &format!("{LINUX_TRACEFS_WARNING}lsof: status error on /x: Permission denied\n"),
            &db,
            OTHER_SELF_PID,
        ) {
            DbOwnership::Unknown(reason) => assert!(
                reason.contains("lsof: status error on /x"),
                "reason must surface the real diagnostic, got: {reason}"
            ),
            other => panic!("expected Unknown, got {other:?}"),
        }
        // A mount warning for the directory holding the DB is not disjoint.
        let containing = format!(
            "lsof: WARNING: can't stat() fuse file system {}\n",
            dir.path().canonicalize().unwrap().display()
        );
        assert!(matches!(
            classify_lsof_run(Some(1), "", &containing, &db, OTHER_SELF_PID),
            DbOwnership::Unknown(_)
        ));
        // Output on stdout with a non-zero exit is still never NotOwned.
        assert!(matches!(
            classify_lsof_run(
                Some(1),
                "garbage\n",
                LINUX_TRACEFS_WARNING,
                &db,
                OTHER_SELF_PID
            ),
            DbOwnership::Unknown(_)
        ));
    }

    /// The real call site driven with a stub `lsof`, so the target the stderr
    /// filter compares against is the canonical path actually probed.
    #[test]
    fn real_call_site_with_stub_lsof_classifies_linux_warnings() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("memory.db");
        std::fs::write(&db, b"x").expect("seed");
        let bin = tempfile::tempdir().expect("stub dir");
        let stub = |name: &str, body: &str| {
            // One file per stub: never rewrite a script another exec may hold.
            let path = bin.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        };
        let warn = "cat >&2 <<'EOF'\nlsof: WARNING: can't stat() tracefs file system /sys/kernel/debug/tracing\n      Output information may be incomplete.\nEOF\n";

        let clear = stub(
            "lsof-clear",
            &format!("[ \"$1\" = '--' ] || exit 93\n{warn}exit 1\n"),
        );
        assert_eq!(
            probe_db_ownership_with(clear.as_os_str(), &db),
            DbOwnership::NotOwned
        );

        let held = stub(
            "lsof-held",
            &format!("{warn}printf 'COMMAND PID USER FD TYPE\\ntachi 123 u 10r REG\\n'\nexit 0\n"),
        );
        assert_eq!(
            probe_db_ownership_with(held.as_os_str(), &db),
            DbOwnership::Owned
        );

        let ancestor = stub(
            "lsof-ancestor",
            "echo \"lsof: WARNING: can't stat() fuse file system $(dirname \"$2\")\" >&2\nexit 1\n",
        );
        assert!(matches!(
            probe_db_ownership_with(ancestor.as_os_str(), &db),
            DbOwnership::Unknown(_)
        ));

        let failing = stub(
            "lsof-failing",
            &format!(
                "{warn}echo 'lsof: status error on '\"$2\"': Permission denied' >&2\nexit 1\n"
            ),
        );
        assert!(matches!(
            probe_db_ownership_with(failing.as_os_str(), &db),
            DbOwnership::Unknown(_)
        ));
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
