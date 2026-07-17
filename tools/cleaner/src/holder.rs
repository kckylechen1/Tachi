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
//! `exec_env_reaper.rs` under #1062. This module only ever detects; it
//! never signals or kills a process.

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
    let lsof = Command::new("lsof").arg("+D").arg(path).output();
    let out = match lsof {
        Ok(out) => out,
        Err(err) => return HolderEvidence::Unknown(format!("lsof unavailable: {err}")),
    };

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // lsof's documented idiom: exit code 1 with no data rows means "no
    // open files found" (clear). Any other non-zero exit is inconclusive,
    // never clear — fail closed on holder evidence.
    if !out.status.success() {
        if out.status.code() == Some(1) && stdout.trim().is_empty() {
            return HolderEvidence::Clear;
        }
        return HolderEvidence::Unknown(if stderr.trim().is_empty() {
            format!("lsof exited with {}", out.status)
        } else {
            stderr.trim().to_string()
        });
    }

    let mut pids: Vec<i32> = stdout
        .lines()
        .skip(1) // header row: COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|pid_str| pid_str.parse::<i32>().ok())
        .collect();
    pids.sort_unstable();
    pids.dedup();

    if pids.is_empty() {
        return HolderEvidence::Clear;
    }

    HolderEvidence::Held(pids.into_iter().map(attribute_pid).collect())
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
}
