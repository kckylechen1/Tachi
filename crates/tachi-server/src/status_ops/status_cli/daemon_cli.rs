use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tachi_bootstrap::cli::DaemonAction;

pub(crate) async fn run_daemon(
    action: DaemonAction,
    app_home: &Path,
    global_db_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        DaemonAction::Status { json: json_out } => {
            let daemon = crate::status_ops::collect_daemon_status(app_home, global_db_path);
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::to_value(&daemon)?)?
                );
            } else {
                match daemon {
                    crate::status_ops::DaemonStatus::Running { pid, lock_path } => {
                        println!("[OK] daemon running pid={pid} lock={}", lock_path.display())
                    }
                    crate::status_ops::DaemonStatus::Foreign {
                        pid,
                        lock_path,
                        reason,
                        version,
                        port,
                        global_db,
                    } => {
                        println!(
                            "[!] foreign daemon pid={pid} lock={} reason={reason}",
                            lock_path.display()
                        );
                        println!(
                            "    version={} port={} global_db={}",
                            version.as_deref().unwrap_or("unknown"),
                            port.map(|p| p.to_string())
                                .unwrap_or_else(|| "unknown".to_string()),
                            global_db.as_deref().unwrap_or("unknown")
                        );
                    }
                    crate::status_ops::DaemonStatus::StalePid { pid, lock_path } => {
                        println!(
                            "[!] stale pid file pid={pid} at {} (process not alive)",
                            lock_path.display()
                        )
                    }
                    crate::status_ops::DaemonStatus::None => println!(
                        "[i] no daemon running for this DB scope; background workers are paused. Stdio MCP clients may still be active; run `tachi daemon reap --json` to inspect live/stale clients."
                    ),
                }
            }
            Ok(())
        }
        DaemonAction::Kill { force } => {
            match crate::status_ops::collect_daemon_status(app_home, global_db_path) {
                crate::status_ops::DaemonStatus::Running { pid, .. } => {
                    #[cfg(unix)]
                    {
                        // SAFETY: `kill(pid, SIGTERM)` sends a signal to an OS
                        // pid; it passes no pointers across the FFI boundary and
                        // aliases no Rust memory. A stale/invalid pid yields
                        // ESRCH and is handled below.
                        let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                        if r == 0 {
                            println!("[OK] sent SIGTERM to daemon pid={pid}");
                        } else {
                            let err = std::io::Error::last_os_error();
                            return Err(format!("kill({pid}) failed: {err}").into());
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        return Err("daemon kill is only implemented on unix".into());
                    }
                }
                crate::status_ops::DaemonStatus::StalePid { pid, lock_path } => {
                    if force {
                        let _ = std::fs::remove_file(&lock_path);
                        println!(
                            "[OK] removed stale lock {} (pid {pid} was not alive)",
                            lock_path.display()
                        );
                    } else {
                        println!(
                            "[!] pid {pid} in {} is not alive; rerun with --force to unlink the stale lock",
                            lock_path.display()
                        );
                    }
                }
                crate::status_ops::DaemonStatus::Foreign { reason, .. } => {
                    println!("[!] refusing to kill foreign daemon for current DB scope: {reason}");
                }
                crate::status_ops::DaemonStatus::None => {
                    println!("[OK] no daemon to kill for current DB scope");
                }
            }
            Ok(())
        }
        DaemonAction::Reap { apply, json } => reap_stale_processes(app_home, apply, json),
    }
}

/// True when `pid` names a live process this user can see. `EPERM` means the
/// process exists but is owned by someone else (still "alive").
#[cfg(unix)]
fn process_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill(pid, 0)` is a signal-0 existence probe — it sends no
    // signal, passes no pointers across the FFI boundary, and aliases no
    // Rust memory.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Extract the token following `flag` from a ps command line.
fn flag_value(command: &str, flag: &str) -> Option<String> {
    let mut it = command.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == flag {
            return it.next().map(|s| s.to_string());
        }
    }
    None
}

fn truncate_command(command: &str) -> String {
    command.chars().take(160).collect()
}

fn host_hint(command: Option<&str>) -> &'static str {
    let lower = command.unwrap_or_default().to_ascii_lowercase();
    if lower.contains("codex") {
        "codex"
    } else if lower.contains("claude") {
        "claude"
    } else if lower.contains("cursor") {
        "cursor"
    } else if lower.contains("gemini") || lower.contains("antigravity") {
        "gemini"
    } else if lower.contains("openclaw") {
        "openclaw"
    } else if lower.contains("opencode") {
        "opencode"
    } else if lower.contains("windsurf") {
        "windsurf"
    } else if lower.contains("node") {
        "node"
    } else if lower.contains("tmux") {
        "tmux"
    } else if lower.contains("zsh") || lower.contains("bash") || lower.contains("fish") {
        "shell"
    } else if command.is_some() {
        "unknown_live_parent"
    } else {
        "unknown"
    }
}

/// One `ps`-derived tachi candidate, mutable across the classification passes
/// below: baseline kind/reap/reason (unchanged #520-era logic), then #1273
/// Gap 3 (version-skew) and Gap 2 (per-parent dedup) may each override a
/// still-`kept` `"stdio"` candidate into a reap candidate with a more
/// specific kind/reason. Order matters: skew runs first (a precise,
/// per-process signal), then dedup recomputes its per-parent counts only
/// over whatever is *still* kept, so a process already reaped for running a
/// stale binary never also occupies a parent's live-adapter budget.
struct Candidate {
    pid: i64,
    ppid: i64,
    etimes: u64,
    command: String,
    kind: &'static str,
    reap: bool,
    reason: &'static str,
}

/// #1273 Gap 2: "parent alive" alone is too coarse — a single live host
/// (observed: 8 adapters under one live codex parent, 5 leaked from
/// finished Saturday sub-runs) can accumulate stdio adapters left over from
/// sub-sessions whose specific stdin pipe the host already abandoned while
/// the host process itself keeps running other work. There is no portable,
/// race-free way to prove "the OTHER end of THIS pipe is gone" from the
/// reaper's vantage point on Darwin (`lsof` shows this process's own pipe
/// endpoint, not whether any live process still holds the peer/write end)
/// that is safe enough to power an unattended `--apply` kill of a
/// technically-still-alive process. The defensible, testable proxy used
/// here instead: cap how many "live-parent" stdio adapters a single ppid is
/// allowed to keep. A host legitimately running a handful of concurrent MCP
/// sessions never needs to rack up 8+ for one live parent — that count is
/// only explained by leaked, logically-finished sub-sessions never exiting
/// (the same #1273 Gap 1 bug: they *should* have died on their own, whether
/// via stdin EOF or a signal, and didn't). Only the OLDEST adapters beyond
/// the cap (lowest pid == started earliest, since pids are monotonic within
/// a boot cycle) become reap candidates; the newest `cap` are always kept,
/// so an actively-busy host is never starved mid-burst.
fn per_parent_dedup_candidates(
    live_parent_stdio: &[(i64, i64)],
    cap: usize,
) -> std::collections::BTreeSet<i64> {
    use std::collections::BTreeMap;
    let mut by_parent: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for &(pid, ppid) in live_parent_stdio {
        by_parent.entry(ppid).or_default().push(pid);
    }
    let mut reap = std::collections::BTreeSet::new();
    for (_ppid, mut pids) in by_parent {
        if pids.len() <= cap {
            continue;
        }
        pids.sort_unstable();
        let excess = pids.len() - cap;
        for pid in pids.into_iter().take(excess) {
            reap.insert(pid);
        }
    }
    reap
}

/// #1273 Gap 3 — pure version-skew decision, OS-call-free so it is
/// unit-testable without a live process table or filesystem. Two
/// independent signals, either sufficient:
///
/// 1. `exe_mtime` (the on-disk binary this pid was resolved to, when
///    resolvable at all) is newer than the process's own start time by more
///    than `grace` — the binary was rebuilt out from under a still-running
///    process. This is the precise #1273 skew class ("Wednesday adapters
///    ran a pre-Jul-17-rebuild binary image for 4 days"). `grace` absorbs
///    the race where a rebuild lands moments before/after a brand-new
///    process's start timestamp is sampled, so a process that started
///    seconds before a same-window rebuild is never misclassified.
/// 2. No exe path was resolvable at all (missing `lsof`, process already
///    gone, unexpected output) AND the process exceeds `max_age` — the
///    pragmatic backstop #1273 calls for when the precise signal can't be
///    computed: an unverifiable stdio adapter that has been running for
///    days is still worth flagging even without binary-identity proof.
fn stdio_version_skew_reason(
    age: Duration,
    exe_mtime: Option<SystemTime>,
    process_started_at: SystemTime,
    grace: Duration,
    max_age: Duration,
) -> Option<&'static str> {
    match exe_mtime {
        Some(mtime) => match mtime.duration_since(process_started_at) {
            Ok(delta) if delta > grace => {
                Some("on-disk tachi binary was rebuilt after this process started (version skew)")
            }
            _ => None,
        },
        None if age > max_age => Some(
            "stdio process exceeds max age and its running binary image could not be verified \
             (version-skew backstop; install/rebuild lsof-based identity check to sharpen this)",
        ),
        None => None,
    }
}

/// #1273 Gap 3 (best-effort, OS-probing): resolve the on-disk executable
/// path a live pid was exec'd from via `lsof`'s `txt` (text/code segment) fd
/// — the standard way to recover a running process's binary path on Darwin
/// (there is no `/proc/<pid>/exe`). Any failure (missing `lsof`, process
/// gone, unexpected output) yields `None`; callers MUST treat `None` as
/// "cannot determine", never as a positive skew signal — see
/// `stdio_version_skew_reason`'s age-only backstop for that case.
#[cfg(unix)]
fn process_executable_path(pid: i64) -> Option<PathBuf> {
    let out = std::process::Command::new("lsof")
        .args(["-p", &pid.to_string(), "-a", "-d", "txt", "-Fn"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // `-Fn` field output: the file-name field is a line starting with `n`.
    text.lines()
        .find_map(|line| line.strip_prefix('n'))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Env-tunable knobs for the #1273 Gap 2/3 criteria, all with fail-safe
/// (conservative, "keep it" leaning) defaults.
fn reap_stdio_parent_cap() -> usize {
    std::env::var("TACHI_REAP_STDIO_PARENT_CAP")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&cap| cap > 0)
        .unwrap_or(3)
}

fn reap_stdio_skew_grace() -> Duration {
    Duration::from_secs(
        crate::utils::parse_env_u64("TACHI_REAP_STDIO_SKEW_GRACE_SECS").unwrap_or(300),
    )
}

fn reap_stdio_max_age() -> Duration {
    Duration::from_secs(
        crate::utils::parse_env_u64("TACHI_REAP_STDIO_MAX_AGE_SECS").unwrap_or(48 * 3600),
    )
}

/// Machine-wide sweep for stale tachi processes. Reaps the unambiguously
/// dead (stdio servers whose launching host died, reparented to pid 1;
/// daemons whose backing global DB no longer exists) plus the #1273 Gap 2/3
/// criteria layered on top of the base "live parent" bucket: adapters beyond
/// a per-parent-host cap (leaked sub-sessions under one still-live host) and
/// adapters running a provably-stale (or unverifiably ancient) binary image.
/// Healthy live daemons/servers, healthy stdio adapters within the cap
/// running a current binary, and this very process are left alone. Dry-run
/// by default.
#[cfg(unix)]
fn reap_stale_processes(
    app_home: &Path,
    apply: bool,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let self_pid = std::process::id() as i64;
    let out = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,etimes=,command="])
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut process_table = std::collections::BTreeMap::<i64, String>::new();
    let mut candidates = Vec::<Candidate>::new();

    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 4 {
            continue;
        }
        let (Ok(pid), Ok(ppid), Ok(etimes)) = (
            tokens[0].parse::<i64>(),
            tokens[1].parse::<i64>(),
            tokens[2].parse::<u64>(),
        ) else {
            continue;
        };
        let command = tokens[3..].join(" ");
        process_table.insert(pid, command.clone());
        if pid == self_pid {
            continue;
        }
        // Only consider processes whose argv[0] basename is a tachi binary.
        let argv0 = command.split_whitespace().next().unwrap_or_default();
        let base = argv0.rsplit('/').next().unwrap_or(argv0);
        if base != "tachi" && base != "tachi-server" {
            continue;
        }

        let is_daemon = command.contains("--daemon");
        let (kind, reap, reason): (&'static str, bool, &'static str) =
            if ppid == 1 && !is_daemon {
                (
                    "orphan-stdio",
                    true,
                    "stdio server was reparented to pid 1; launching MCP host is gone",
                )
            } else if is_daemon {
                match flag_value(&command, "--global-db") {
                    Some(db) if !Path::new(&db).exists() => {
                        ("dead-db-daemon", true, "daemon global DB path no longer exists")
                    }
                    _ => ("daemon", false, "daemon is live for an existing DB scope"),
                }
            } else {
                ("stdio", false, "stdio MCP client has a live parent process")
            };

        candidates.push(Candidate {
            pid,
            ppid,
            etimes,
            command,
            kind,
            reap,
            reason,
        });
    }

    // #1273 Gap 3: version skew. Runs first so a stale-binary adapter is
    // reclassified before the Gap 2 dedup pass counts it against its
    // parent's live-adapter budget.
    let skew_grace = reap_stdio_skew_grace();
    let max_age = reap_stdio_max_age();
    let now = SystemTime::now();
    for c in candidates.iter_mut() {
        if c.kind != "stdio" || c.reap {
            continue;
        }
        let age = Duration::from_secs(c.etimes);
        let Some(started_at) = now.checked_sub(age) else {
            continue;
        };
        let exe_mtime = process_executable_path(c.pid)
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        if let Some(reason) =
            stdio_version_skew_reason(age, exe_mtime, started_at, skew_grace, max_age)
        {
            c.kind = "stdio-version-skew";
            c.reap = true;
            c.reason = reason;
        }
    }

    // #1273 Gap 2: per-parent dedup, over whatever the skew pass left kept.
    let parent_cap = reap_stdio_parent_cap();
    let still_kept_stdio: Vec<(i64, i64)> = candidates
        .iter()
        .filter(|c| c.kind == "stdio" && !c.reap)
        .map(|c| (c.pid, c.ppid))
        .collect();
    let dedup_reap = per_parent_dedup_candidates(&still_kept_stdio, parent_cap);
    for c in candidates.iter_mut() {
        if c.kind == "stdio" && !c.reap && dedup_reap.contains(&c.pid) {
            c.kind = "stdio-dedup-cap";
            c.reap = true;
            c.reason = "parent host exceeded the live stdio-adapter cap; oldest excess adapters reaped (#1273)";
        }
    }

    let mut findings: Vec<serde_json::Value> = Vec::new();
    let mut reaped = 0usize;
    let mut kept = 0usize;
    let mut active_stdio_clients = 0usize;
    let mut active_daemons = 0usize;

    for c in candidates {
        let Candidate {
            pid,
            ppid,
            command,
            kind,
            reap,
            reason,
            ..
        } = c;
        let is_daemon = kind == "daemon" || kind == "dead-db-daemon";
        if is_daemon && !reap {
            active_daemons += 1;
        } else if !is_daemon && !reap {
            active_stdio_clients += 1;
        }

        if reap {
            reaped += 1;
            if apply {
                // SAFETY: `kill(pid, SIGTERM)` sends a signal to an OS pid; it
                // passes no pointers across the FFI boundary and aliases no
                // Rust memory. Best-effort reap — a stale pid yields ESRCH
                // which is tolerated (result intentionally discarded).
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            }
        } else {
            kept += 1;
        }
        let parent_command = process_table
            .get(&ppid)
            .map(|command| truncate_command(command));
        findings.push(serde_json::json!({
            "pid": pid,
            "ppid": ppid,
            "kind": kind,
            "reap": reap,
            "state": if reap { "reap_candidate" } else { "kept_healthy" },
            "reason": reason,
            "command": truncate_command(&command),
            "parent": {
                "pid": ppid,
                "host_hint": host_hint(parent_command.as_deref()),
                "command": parent_command,
            },
            "db_scope": {
                "global_db": flag_value(&command, "--global-db"),
                "project_db": flag_value(&command, "--project-db"),
                "no_project_db": command.split_whitespace().any(|token| token == "--no-project-db"),
            }
        }));
    }

    // Stale daemon discovery files (pid recorded but no longer alive).
    let mut stale_files: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(app_home) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if !(name.starts_with("daemon") && name.ends_with(".pid")) {
                continue;
            }
            let path = ent.path();
            let alive = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.get("pid").and_then(|p| p.as_i64()))
                .map(process_alive)
                .unwrap_or(false);
            if !alive {
                stale_files.push(name);
                if apply {
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(path.with_extension("lock"));
                }
            }
        }
    }

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "applied": apply,
                "reaped": reaped,
                "kept": kept,
                "summary": {
                    "applied": apply,
                    "would_reap": if apply { 0 } else { reaped },
                    "reaped": if apply { reaped } else { 0 },
                    "kept_healthy": kept,
                    "active_stdio_clients": active_stdio_clients,
                    "active_daemons": active_daemons,
                    "stale_file_count": stale_files.len(),
                    "process_count": findings.len(),
                    "message": "Healthy stdio processes are live MCP clients owned by their parent host; they are not daemon conflicts and are not safe to kill from reap.",
                },
                "stale_files": stale_files,
                "processes": findings,
            }))?
        );
        return Ok(());
    }

    let verb = if apply { "reaped" } else { "would reap" };
    println!(
        "tachi process sweep: {verb} {reaped}, kept {kept} healthy{}",
        if apply {
            ""
        } else {
            " (dry-run; pass --apply to act)"
        }
    );
    for f in &findings {
        let mark = if f["reap"].as_bool().unwrap_or(false) {
            "KILL"
        } else {
            "keep"
        };
        println!(
            "  [{mark}] pid={} ppid={} {} parent={} :: {}",
            f["pid"],
            f["ppid"],
            f["kind"].as_str().unwrap_or(""),
            f["parent"]["host_hint"].as_str().unwrap_or("unknown"),
            f["command"].as_str().unwrap_or("")
        );
    }
    if kept > 0 {
        println!(
            "  note: kept stdio processes have live parent hosts; use --json to inspect parent.host_hint and DB scope."
        );
    }
    if !stale_files.is_empty() {
        let fverb = if apply { "removed" } else { "stale" };
        println!("  {fverb} lock/pid files: {}", stale_files.join(", "));
    }
    Ok(())
}

#[cfg(not(unix))]
fn reap_stale_processes(
    _app_home: &Path,
    _apply: bool,
    _json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("daemon reap is only implemented on unix".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_hint_classifies_known_mcp_parent_hosts() {
        assert_eq!(host_hint(Some("/usr/local/bin/codex --model gpt")), "codex");
        assert_eq!(host_hint(Some("Claude Desktop Helper")), "claude");
        assert_eq!(
            host_hint(Some("/Applications/Cursor.app/Contents/MacOS/Cursor")),
            "cursor"
        );
        assert_eq!(host_hint(Some("openclaw mcp serve")), "openclaw");
        assert_eq!(host_hint(Some("zsh -l")), "shell");
    }

    #[test]
    fn host_hint_distinguishes_missing_and_unknown_live_parent() {
        assert_eq!(host_hint(None), "unknown");
        assert_eq!(host_hint(Some("/usr/bin/launchd")), "unknown_live_parent");
    }

    #[test]
    fn truncate_command_keeps_output_bounded() {
        let command = "x".repeat(200);
        assert_eq!(truncate_command(&command).chars().count(), 160);
    }

    // ---- #1273 Gap 2: per-parent dedup ------------------------------------

    #[test]
    fn dedup_keeps_all_adapters_within_cap() {
        let live = [(100, 1), (101, 1), (102, 1)];
        assert!(per_parent_dedup_candidates(&live, 3).is_empty());
    }

    #[test]
    fn dedup_reaps_oldest_excess_beyond_cap_keeping_newest() {
        // One live parent (ppid 500) with 8 adapters: the exact #1273
        // forensics shape (3 live + 5 leaked). Cap 3 must reap the 5 lowest
        // pids (oldest) and keep the 3 highest (newest).
        let live: Vec<(i64, i64)> = vec![10, 11, 12, 13, 14, 20, 21, 22]
            .into_iter()
            .map(|pid| (pid, 500))
            .collect();
        let reap = per_parent_dedup_candidates(&live, 3);
        assert_eq!(reap.len(), 5);
        for pid in [10, 11, 12, 13, 14] {
            assert!(reap.contains(&pid), "expected oldest pid {pid} reaped");
        }
        for pid in [20, 21, 22] {
            assert!(!reap.contains(&pid), "newest pid {pid} must be kept");
        }
    }

    #[test]
    fn dedup_is_scoped_per_parent_not_global() {
        // Two different live parents, each under cap on their own, must not
        // be reaped even though the combined total exceeds the cap.
        let live = [(1, 700), (2, 700), (3, 800), (4, 800)];
        assert!(per_parent_dedup_candidates(&live, 2).is_empty());
    }

    // ---- #1273 Gap 3: version-skew decision -------------------------------

    fn secs_ago(now: SystemTime, secs: u64) -> SystemTime {
        now.checked_sub(Duration::from_secs(secs)).expect("secs_ago")
    }

    #[test]
    fn skew_flags_binary_rebuilt_after_process_started() {
        let now = SystemTime::now();
        let started = secs_ago(now, 4 * 24 * 3600); // 4 days old, matching forensics
        let rebuilt_after_start = secs_ago(now, 2 * 24 * 3600); // rebuilt 2 days ago
        let reason = stdio_version_skew_reason(
            Duration::from_secs(4 * 24 * 3600),
            Some(rebuilt_after_start),
            started,
            Duration::from_secs(300),
            Duration::from_secs(48 * 3600),
        );
        assert!(reason.is_some(), "newer on-disk binary must flag skew");
        assert!(reason.unwrap().contains("rebuilt"));
    }

    #[test]
    fn skew_ignores_binary_older_than_process_start() {
        let now = SystemTime::now();
        let started = secs_ago(now, 3600);
        let older_binary = secs_ago(now, 7200); // binary predates the process
        let reason = stdio_version_skew_reason(
            Duration::from_secs(3600),
            Some(older_binary),
            started,
            Duration::from_secs(300),
            Duration::from_secs(48 * 3600),
        );
        assert!(reason.is_none(), "a binary older than the process is not skewed");
    }

    #[test]
    fn skew_grace_window_absorbs_a_rebuild_landing_at_startup() {
        let now = SystemTime::now();
        let started = secs_ago(now, 60);
        let rebuilt_moments_later = secs_ago(now, 30); // 30s newer than start
        let reason = stdio_version_skew_reason(
            Duration::from_secs(60),
            Some(rebuilt_moments_later),
            started,
            Duration::from_secs(300), // 5 min grace absorbs a 30s delta
            Duration::from_secs(48 * 3600),
        );
        assert!(
            reason.is_none(),
            "a rebuild within the grace window must not false-positive"
        );
    }

    #[test]
    fn skew_backstop_flags_unverifiable_ancient_process() {
        let reason = stdio_version_skew_reason(
            Duration::from_secs(72 * 3600), // 72h, older than the 48h backstop
            None,                           // exe path could not be resolved
            SystemTime::now(),
            Duration::from_secs(300),
            Duration::from_secs(48 * 3600),
        );
        assert!(reason.is_some(), "unverifiable ancient process must backstop-flag");
        assert!(reason.unwrap().contains("backstop"));
    }

    #[test]
    fn skew_backstop_never_flags_within_max_age() {
        let reason = stdio_version_skew_reason(
            Duration::from_secs(3600), // 1h old, well under the 48h backstop
            None,
            SystemTime::now(),
            Duration::from_secs(300),
            Duration::from_secs(48 * 3600),
        );
        assert!(
            reason.is_none(),
            "an unverifiable but young process must not be reaped"
        );
    }

    // ---- #1273 Gap 2/3: env knob defaults ---------------------------------

    #[test]
    fn reap_stdio_defaults_are_conservative() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for var in [
            "TACHI_REAP_STDIO_PARENT_CAP",
            "TACHI_REAP_STDIO_SKEW_GRACE_SECS",
            "TACHI_REAP_STDIO_MAX_AGE_SECS",
        ] {
            std::env::remove_var(var);
        }
        assert_eq!(reap_stdio_parent_cap(), 3);
        assert_eq!(reap_stdio_skew_grace(), Duration::from_secs(300));
        assert_eq!(reap_stdio_max_age(), Duration::from_secs(48 * 3600));
    }

    #[test]
    fn reap_stdio_parent_cap_rejects_zero_and_garbage() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("TACHI_REAP_STDIO_PARENT_CAP", "0");
        assert_eq!(reap_stdio_parent_cap(), 3, "zero must fall back to the default");
        std::env::set_var("TACHI_REAP_STDIO_PARENT_CAP", "not-a-number");
        assert_eq!(reap_stdio_parent_cap(), 3, "garbage must fall back to the default");
        std::env::set_var("TACHI_REAP_STDIO_PARENT_CAP", "5");
        assert_eq!(reap_stdio_parent_cap(), 5);
        std::env::remove_var("TACHI_REAP_STDIO_PARENT_CAP");
    }
}
