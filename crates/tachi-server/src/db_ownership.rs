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

/// Best-effort detection: does any running tachi-tachi-server have an
/// open file handle on `db_path`? Uses `lsof` on Unix. Only a clean lsof
/// run (readable exit status + output) may resolve to `Owned`/`NotOwned`;
/// canonicalize failure, a missing/erroring `lsof`, and non-unix platforms
/// all return `Unknown` — the caller must fail closed on that, not fall
/// through to the unguarded copy path.
#[cfg(unix)]
fn probe_db_ownership(db_path: &Path) -> DbOwnership {
    use std::process::Command;
    let abs = match db_path.canonicalize() {
        Ok(p) => p,
        Err(e) => return DbOwnership::Unknown(format!("canonicalize failed: {e}")),
    };
    let abs_str = abs.to_string_lossy().to_string();
    // Using `--` to terminate options before the path argument so paths
    // beginning with `-` are treated literally.
    let output = Command::new("lsof").arg("--").arg(&abs_str).output();
    match output {
        Ok(o) => classify_lsof_output(
            o.status.code(),
            &String::from_utf8_lossy(&o.stdout),
            &String::from_utf8_lossy(&o.stderr),
        ),
        Err(e) => DbOwnership::Unknown(format!("lsof unavailable: {e}")),
    }
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
/// Signatures:
/// - `Some(0)` + >1 stdout line → `Owned` (header + at least one holder line)
/// - `Some(0)` + ≤1 stdout line → `NotOwned` (header only / no output)
/// - `Some(n)` (n != 0) + both streams empty → `NotOwned` (lsof's normal,
///   silent "no holders" exit-1 signature)
/// - `None` (signal death) → `Unknown("lsof terminated by signal")`,
///   regardless of stream contents
/// - `Some(n)` (n != 0) + diagnostic text on either stream → `Unknown`
///   with the first diagnostic line surfaced
#[cfg(unix)]
fn classify_lsof_output(status_code: Option<i32>, stdout: &str, stderr: &str) -> DbOwnership {
    match status_code {
        Some(0) => {
            // lsof prints a header line + one line per holder; >1 line means held.
            if stdout.lines().count() > 1 {
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

#[cfg(not(unix))]
fn probe_db_ownership(_db_path: &Path) -> DbOwnership {
    DbOwnership::Unknown("unsupported platform (no ownership probe on non-unix)".to_string())
}

// Test inject: force `daemon_ownership` to return a specific result without
// depending on real lsof behavior (a genuinely erroring lsof is not
// reliably reproducible across CI environments). `pub(crate)` so every
// call-site module's own tests can drive it via `set_ownership_inject_for_test`.
#[cfg(all(test, unix))]
thread_local! {
    static INJECT_OWNERSHIP: std::cell::RefCell<Option<DbOwnership>> =
        const { std::cell::RefCell::new(None) };
}

/// Probe whether a live daemon holds `db_path` open. Every non-atomic,
/// multi-file DB copy site in this crate must route through this function
/// (not `probe_db_ownership` directly) so tests can deterministically force
/// `Owned`/`Unknown` without a real lsof/daemon fixture.
pub(crate) fn daemon_ownership(db_path: &Path) -> DbOwnership {
    #[cfg(all(test, unix))]
    {
        if let Some(forced) = INJECT_OWNERSHIP.with(|c| c.borrow_mut().take()) {
            return forced;
        }
    }
    probe_db_ownership(db_path)
}

/// Test-only hook: force the next `daemon_ownership` call (on any thread-local
/// caller in this test) to return `value`, or clear the override with `None`.
#[cfg(all(test, unix))]
pub(crate) fn set_ownership_inject_for_test(value: Option<DbOwnership>) {
    INJECT_OWNERSHIP.with(|c| *c.borrow_mut() = value);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn classify_success_multiple_holders_is_owned() {
        let header_plus_holder = "COMMAND  PID USER  FD TYPE\ntachi  123 kyle  10r REG\n";
        assert_eq!(
            classify_lsof_output(Some(0), header_plus_holder, ""),
            DbOwnership::Owned
        );
    }

    #[test]
    fn classify_success_no_holder_lines_is_not_owned() {
        // Some(0) with header-only (or empty) stdout: exit succeeded but
        // nothing matched.
        assert_eq!(classify_lsof_output(Some(0), "", ""), DbOwnership::NotOwned);
        assert_eq!(
            classify_lsof_output(Some(0), "COMMAND  PID USER  FD TYPE\n", ""),
            DbOwnership::NotOwned
        );
    }

    #[test]
    fn classify_nonzero_exit_both_streams_empty_is_not_owned() {
        // lsof's normal, silent "no holders found" signature: exit 1, not a
        // fault, both streams empty.
        assert_eq!(classify_lsof_output(Some(1), "", ""), DbOwnership::NotOwned);
    }

    #[test]
    fn classify_signal_death_is_unknown_even_with_empty_streams() {
        // The bug this test guards: a signal-killed lsof (SIGKILL/OOM) also
        // leaves both streams empty, which is indistinguishable from the
        // "no holders" signature by stream contents alone. `status_code()
        // == None` must be checked first and must never resolve to
        // `NotOwned`.
        assert_eq!(
            classify_lsof_output(None, "", ""),
            DbOwnership::Unknown("lsof terminated by signal".to_string())
        );
        // Also must not be swayed by incidental non-empty output preceding
        // the kill — signal death always wins.
        assert_eq!(
            classify_lsof_output(None, "COMMAND  PID USER  FD TYPE\n", "some partial text"),
            DbOwnership::Unknown("lsof terminated by signal".to_string())
        );
    }

    #[test]
    fn classify_nonzero_exit_with_diagnostic_is_unknown() {
        let unknown = classify_lsof_output(
            Some(1),
            "",
            "lsof: status error on /x: No such file or directory\n",
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
            classify_lsof_output(None, "", ""),
            classify_lsof_output(Some(1), "", "")
        );
    }
}
