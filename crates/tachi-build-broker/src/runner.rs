//! The executor seat's hands: checkout, run, wipe (#894 S2c item 3).
//!
//! Behind a trait for two reasons. One, the broker's *decisions* (serialize,
//! pick a target, quarantine on interrupt) are the part worth testing, and they
//! must be testable without a Rust toolchain in the loop. Two, the real
//! implementation is three subprocesses and an `rm -rf`, which is exactly the
//! kind of code that should be a leaf, not a dependency of the policy.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::Path;
use std::process::{ChildStderr, ChildStdout, Stdio};
use std::sync::mpsc;

use serde::{Deserialize, Serialize};

use super::ticket::{BuildTicket, SourceIdentity};

/// How a build ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildOutcome {
    /// Exit 0.
    Success,
    /// Non-zero exit — a real compile/test failure. The target dir is in a
    /// well-defined state (cargo finished writing) and keeps its generation.
    Failed,
    /// Killed by a signal / never reached an exit status. The target dir was
    /// being written to when the process died: its contents are NOT trustworthy
    /// and it gets quarantined (#894 S2c item 4).
    Interrupted,
}

/// What one build run produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRun {
    pub outcome: BuildOutcome,
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

/// The seat's hands.
pub trait BuildRunner {
    /// Put the seat's fixed checkout on the ticket's source commit.
    ///
    /// MUST refuse a dirty checkout: the seat's tree is a shared machine
    /// resource, and checking out over someone's uncommitted work is the
    /// clobber this whole design exists to avoid.
    fn prepare_checkout(&self, checkout: &Path, source: &SourceIdentity) -> Result<(), String>;

    /// Run the ticket's command in `checkout` with `CARGO_TARGET_DIR=target_dir`.
    fn run(
        &self,
        ticket: &BuildTicket,
        checkout: &Path,
        target_dir: &Path,
    ) -> Result<BuildRun, String>;

    /// Wipe a target dir. Returns the bytes freed. Must be idempotent (wiping an
    /// already-empty/absent dir frees 0 and is not an error).
    fn clear_target(&self, target_dir: &Path) -> Result<i64, String>;
}

/// The real seat: git + a subprocess + `remove_dir_all`.
pub struct ProcessBuildRunner;

impl BuildRunner for ProcessBuildRunner {
    fn prepare_checkout(&self, checkout: &Path, source: &SourceIdentity) -> Result<(), String> {
        let dirty = git_stdout(checkout, &["status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(format!(
                "executor seat checkout '{}' is dirty; refusing to check out {} over it — the \
                 seat holds ONE fixed clean checkout and never clobbers work in it (#894 S2c)",
                checkout.display(),
                source.head_sha
            ));
        }
        // `--detach <sha>`: the seat never sits on a branch, so it can never
        // "just pull" or drift. The sha was validated as a hex object id by
        // `BuildTicket::new`, so it cannot be a flag or a ref name.
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["checkout", "--detach", &source.head_sha])
            .status()
            .map_err(|e| format!("git checkout: {e}"))?;
        if !status.success() {
            return Err(format!(
                "git checkout --detach {} in '{}' failed ({status})",
                source.head_sha,
                checkout.display()
            ));
        }
        // Verify we landed where the ticket said. A silent mismatch here would
        // build the wrong tree and attribute it to this ticket.
        let head = git_stdout(checkout, &["rev-parse", "HEAD"])?;
        let head = head.trim();
        if !head.starts_with(&source.head_sha) && !source.head_sha.starts_with(head) {
            return Err(format!(
                "executor seat HEAD is {head} after checking out {}: refusing to build a tree \
                 that is not the ticket's source (#894 S2c)",
                source.head_sha
            ));
        }
        Ok(())
    }

    fn run(
        &self,
        ticket: &BuildTicket,
        checkout: &Path,
        target_dir: &Path,
    ) -> Result<BuildRun, String> {
        std::fs::create_dir_all(target_dir)
            .map_err(|e| format!("create target dir {}: {e}", target_dir.display()))?;
        std::thread::scope(|scope| {
            let (stdout_tx, stdout_rx) = mpsc::sync_channel::<ChildStdout>(1);
            let (stderr_tx, stderr_rx) = mpsc::sync_channel::<ChildStderr>(1);
            let (result_tx, result_rx) = mpsc::channel();
            let stdout_result = result_tx.clone();
            // Create both drainers before launching a build, so thread creation
            // failure cannot leave a compiler running without pipe consumers.
            std::thread::Builder::new()
                .name("build-stdout".into())
                .spawn_scoped(scope, move || {
                    drain_pipe(stdout_rx, stdout_result, "stdout");
                })
                .map_err(|e| format!("start stdout reader: {e}"))?;
            std::thread::Builder::new()
                .name("build-stderr".into())
                .spawn_scoped(scope, move || {
                    drain_pipe(stderr_rx, result_tx, "stderr");
                })
                .map_err(|e| format!("start stderr reader: {e}"))?;

            let mut child = std::process::Command::new(&ticket.command.program)
                .args(&ticket.command.args)
                .current_dir(checkout)
                .env("CARGO_TARGET_DIR", target_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("spawn {}: {e}", ticket.command.program))?;

            let captured = (|| {
                // Own the senders in this closure: every error closes any pipe
                // handoff still waiting in a reader before the scope joins it.
                let stdout_tx = stdout_tx;
                let stderr_tx = stderr_tx;
                stdout_tx
                    .send(child.stdout.take().ok_or("missing build stdout pipe")?)
                    .map_err(|_| "build stdout reader stopped")?;
                stderr_tx
                    .send(child.stderr.take().ok_or("missing build stderr pipe")?)
                    .map_err(|_| "build stderr reader stopped")?;

                let mut stdout_tail = String::new();
                let mut stderr_tail = String::new();
                for _ in 0..2 {
                    let (stream, result) = result_rx
                        .recv()
                        .map_err(|_| "build output reader stopped without a result")?;
                    let output = result?;
                    if stream == "stdout" {
                        stdout_tail = output;
                    } else {
                        stderr_tail = output;
                    }
                }
                let status = child
                    .wait()
                    .map_err(|e| format!("wait for {}: {e}", ticket.command.program))?;
                let outcome = match status.code() {
                    Some(0) => BuildOutcome::Success,
                    Some(_) => BuildOutcome::Failed,
                    None => BuildOutcome::Interrupted,
                };
                Ok(BuildRun {
                    outcome,
                    exit_code: status.code(),
                    stdout_tail,
                    stderr_tail,
                })
            })();
            if captured.is_err() {
                // A capture error must not release the executor slot while the
                // direct child is still writing into its target directory.
                let _ = child.kill();
                let _ = child.wait();
            }
            captured
        })
    }

    fn clear_target(&self, target_dir: &Path) -> Result<i64, String> {
        if !target_dir.exists() {
            return Ok(0);
        }
        let bytes = dir_size(target_dir);
        std::fs::remove_dir_all(target_dir)
            .map_err(|e| format!("clear target dir {}: {e}", target_dir.display()))?;
        std::fs::create_dir_all(target_dir)
            .map_err(|e| format!("recreate target dir {}: {e}", target_dir.display()))?;
        Ok(bytes)
    }
}

fn drain_pipe<R: Read>(
    input: mpsc::Receiver<R>,
    result: mpsc::Sender<(&'static str, Result<String, String>)>,
    stream: &'static str,
) {
    let Ok(reader) = input.recv() else { return };
    let tail = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_output_tail(reader)))
        .map_err(|_| format!("build {stream} reader panicked"))
        .and_then(|r| r.map_err(|e| format!("read build {stream}: {e}")));
    let _ = result.send((stream, tail));
}

const TAIL_BYTES: usize = 4096;

fn read_output_tail(mut reader: impl Read) -> io::Result<String> {
    // Keep a full codepoint of lookbehind. Decoding only the final 4096 raw
    // bytes could manufacture replacement characters at a cut UTF-8 boundary.
    // Lossy decoding never shrinks bytes; the extra three bytes keep that
    // possible boundary damage outside the final decoded tail.
    const RETAIN: usize = TAIL_BYTES + 3;
    let mut retained = VecDeque::with_capacity(RETAIN);
    let mut buffer = [0; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if read >= RETAIN {
            retained.clear();
            retained.extend(&buffer[read - RETAIN..read]);
        } else {
            let discard = (retained.len() + read).saturating_sub(RETAIN);
            retained.drain(..discard);
            retained.extend(&buffer[..read]);
        }
    }
    Ok(tail(&String::from_utf8_lossy(retained.make_contiguous())))
}

fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} in '{}' failed: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Last 4 KiB — enough to see the error, small enough to keep in a KV row.
fn tail(raw: &str) -> String {
    if raw.len() <= TAIL_BYTES {
        return raw.to_string();
    }
    let start = raw.len() - TAIL_BYTES;
    // Do not slice mid-codepoint.
    let start = (start..raw.len())
        .find(|i| raw.is_char_boundary(*i))
        .unwrap_or(raw.len());
    raw[start..].to_string()
}

fn dir_size(path: &Path) -> i64 {
    let mut total: i64 = 0;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&entry.path()));
        } else {
            total = total.saturating_add(meta.len() as i64);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Chunked<'a> {
        remaining: &'a [u8],
        chunk: usize,
        interrupted: bool,
    }

    impl Read for Chunked<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.interrupted {
                self.interrupted = false;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let len = self.remaining.len().min(self.chunk).min(output.len());
            output[..len].copy_from_slice(&self.remaining[..len]);
            self.remaining = &self.remaining[len..];
            Ok(len)
        }
    }

    #[test]
    fn bounded_tail_preserves_full_lossy_decode_at_utf8_and_read_boundaries() {
        let mut cut_codepoint = vec![b'A'; 5000];
        cut_codepoint.extend_from_slice("🚀".as_bytes());
        cut_codepoint.extend(vec![b'B'; 4093]);
        // A raw 4096-byte cap would invent a replacement character here.
        assert_ne!(
            tail(&String::from_utf8_lossy(&cut_codepoint)),
            tail(&String::from_utf8_lossy(
                &cut_codepoint[cut_codepoint.len() - TAIL_BYTES..]
            ))
        );

        let mut mixed =
            b"ASCII-\xe4\xb8\xad\xe6\x96\x87-\xf0\x9f\x9a\x80-\xff-\xe2\x82-\xc0\xaf".repeat(1500);
        mixed.extend_from_slice(b"\xf0\x9f");
        let bytes: Vec<u8> = (0..20000).map(|i| ((i * 73 + 19) % 256) as u8).collect();
        for input in [
            &cut_codepoint[..],
            &mixed,
            &bytes,
            &mixed[..4095],
            &mixed[..4096],
            &mixed[..4099],
        ] {
            let expected = tail(&String::from_utf8_lossy(input));
            for chunk in [1, 2, 3, 7, 4096, 8192] {
                let reader = Chunked {
                    remaining: input,
                    chunk,
                    interrupted: true,
                };
                assert_eq!(read_output_tail(reader).unwrap(), expected, "chunk={chunk}");
            }
        }
    }

    #[test]
    fn pipe_reader_reports_read_failure_instead_of_a_successful_empty_tail() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("injected capture failure"))
            }
        }
        let (input_tx, input_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::channel();
        input_tx.send(Broken).unwrap();
        drain_pipe(input_rx, result_tx, "stderr");
        let (stream, result) = result_rx.recv().unwrap();
        assert_eq!(stream, "stderr");
        assert_eq!(
            result.unwrap_err(),
            "read build stderr: injected capture failure"
        );
    }

    #[cfg(unix)]
    fn shell_ticket(args: Vec<String>) -> BuildTicket {
        BuildTicket::new(
            "real-runner-test",
            SourceIdentity {
                repo_root: "/runner-test".into(),
                base_sha: "a".repeat(40),
                head_sha: "a".repeat(40),
            },
            super::super::ticket::BuildCommand {
                program: "sh".into(),
                args,
                features: vec![],
            },
            None,
            None,
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn process_runner_drains_both_large_streams_and_preserves_exit_and_tails() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path().canonicalize().unwrap();
        let target = checkout.join("target");
        let script = r#"
            [ "$PWD" = "$1" ] && [ "$CARGO_TARGET_DIR" = "$2" ] || exit 93
            if IFS= read -r ignored; then exit 94; fi
            i=0
            while [ "$i" -lt 8192 ]; do
                printf 'stdout-0123456789'
                printf 'stderr-0123456789' >&2
                i=$((i + 1))
            done
            printf '\nOUT-中文-🚀-\377-\342\202'
            printf '\nERR-终点-🌍-\300\257-\360\237' >&2
            exit 7
        "#;
        let ticket = shell_ticket(vec![
            "-c".into(),
            script.into(),
            "runner-test".into(),
            checkout.to_string_lossy().into_owned(),
            target.to_string_lossy().into_owned(),
        ]);
        let run = ProcessBuildRunner.run(&ticket, &checkout, &target).unwrap();
        let mut stdout = b"stdout-0123456789".repeat(8192);
        stdout.extend_from_slice(b"\nOUT-\xe4\xb8\xad\xe6\x96\x87-\xf0\x9f\x9a\x80-\xff-\xe2\x82");
        let mut stderr = b"stderr-0123456789".repeat(8192);
        stderr.extend_from_slice(
            b"\nERR-\xe7\xbb\x88\xe7\x82\xb9-\xf0\x9f\x8c\x8d-\xc0\xaf-\xf0\x9f",
        );
        assert_eq!(run.outcome, BuildOutcome::Failed);
        assert_eq!(run.exit_code, Some(7));
        assert_eq!(run.stdout_tail, tail(&String::from_utf8_lossy(&stdout)));
        assert_eq!(run.stderr_tail, tail(&String::from_utf8_lossy(&stderr)));
    }

    #[cfg(unix)]
    #[test]
    fn process_runner_keeps_signal_exit_distinct_and_spawn_failure_returns() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = directory.path();
        let target = checkout.join("target");
        let mut ticket = shell_ticket(vec![
            "-c".into(),
            "printf interrupted; kill -TERM $$".into(),
        ]);
        let run = ProcessBuildRunner.run(&ticket, checkout, &target).unwrap();
        assert_eq!(run.outcome, BuildOutcome::Interrupted);
        assert_eq!(run.exit_code, None);
        assert_eq!(run.stdout_tail, "interrupted");
        ticket.command.program = checkout
            .join("absent-command")
            .to_string_lossy()
            .into_owned();
        let error = ProcessBuildRunner
            .run(&ticket, checkout, &target)
            .unwrap_err();
        assert!(error.starts_with("spawn "), "{error}");
    }
}
