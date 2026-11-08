use std::collections::VecDeque;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const READ_CHUNK_BYTES: usize = 8192;
const TEXT_OUTPUT_LIMIT: usize = 1024 * 1024;
const PROTOCOL_STDOUT_LIMIT: usize = 8 * 1024 * 1024;

pub(in crate::dispatch_ops) const STDERR_SEPARATOR: &str = "\n\n--- stderr ---\n";

#[derive(Clone, Copy)]
pub(in crate::dispatch_ops) enum SubprocessOutputProjection {
    /// Ordinary text retains a bounded tail from each channel.
    Combined,
    /// Structured stdout must be complete; diagnostics retain the legacy projection.
    Structured,
    /// DSH needs complete stdout and an explicit boundary even for stderr alone.
    PreserveChannels,
}

impl SubprocessOutputProjection {
    pub(in crate::dispatch_ops) fn for_backend(agent: &str, transport: &str) -> Self {
        if agent == "dsh" {
            Self::PreserveChannels
        } else if matches!(agent, "claude" | "codex" | "grok" | "kimi")
            || super::super::acpx::is_acpx_transport(transport)
        {
            Self::Structured
        } else {
            Self::Combined
        }
    }

    fn stdout_capture(self) -> CaptureMode {
        match self {
            Self::Combined => CaptureMode::Tail,
            Self::Structured | Self::PreserveChannels => CaptureMode::CompleteStdout,
        }
    }
}

pub(super) fn project_output(
    stdout: String,
    stderr: String,
    projection: SubprocessOutputProjection,
) -> String {
    if stderr.is_empty() {
        stdout
    } else if stdout.is_empty()
        && !matches!(projection, SubprocessOutputProjection::PreserveChannels)
    {
        stderr
    } else {
        format!("{stdout}{STDERR_SEPARATOR}{stderr}")
    }
}

#[derive(Clone, Copy)]
enum CaptureMode {
    Tail,
    CompleteStdout,
}

impl CaptureMode {
    fn limit(self) -> usize {
        match self {
            Self::Tail => TEXT_OUTPUT_LIMIT,
            Self::CompleteStdout => PROTOCOL_STDOUT_LIMIT,
        }
    }
}

struct CapturedOutput {
    bytes: VecDeque<u8>,
    mode: CaptureMode,
    channel: &'static str,
    total_bytes: u64,
    overflowed: bool,
}

impl CapturedOutput {
    fn new(mode: CaptureMode, channel: &'static str) -> Self {
        Self {
            bytes: VecDeque::with_capacity(mode.limit()),
            mode,
            channel,
            total_bytes: 0,
            overflowed: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let limit = self.mode.limit();
        self.total_bytes = self.total_bytes.saturating_add(chunk.len() as u64);
        match self.mode {
            CaptureMode::CompleteStdout => {
                if self.overflowed {
                    return;
                }
                if self.bytes.len() + chunk.len() > limit {
                    self.overflowed = true;
                    self.bytes = VecDeque::new();
                    return;
                }
                self.bytes.extend(chunk);
            }
            CaptureMode::Tail => {
                let discarded = (self.bytes.len() + chunk.len()).saturating_sub(limit);
                self.overflowed |= discarded > 0;
                if chunk.len() >= limit {
                    self.bytes.clear();
                    self.bytes.extend(&chunk[chunk.len() - limit..]);
                } else {
                    self.bytes.drain(..discarded);
                    self.bytes.extend(chunk);
                }
            }
        }
    }

    fn into_string(self) -> Result<String, String> {
        if matches!(self.mode, CaptureMode::CompleteStdout) && self.overflowed {
            return Err(format!(
                "Agent stdout protocol output exceeded {} bytes; complete stdout is required",
                self.mode.limit()
            ));
        }
        let bytes: Vec<u8> = self.bytes.into_iter().collect();
        match self.mode {
            CaptureMode::CompleteStdout => String::from_utf8(bytes)
                .map_err(|_| "Agent stdout protocol output is not valid UTF-8".to_string()),
            CaptureMode::Tail => {
                let text = String::from_utf8_lossy(&bytes);
                if self.overflowed {
                    Ok(format!(
                        "[tachi: {} truncated; retained last {} bytes; discarded {} bytes]\n{text}",
                        self.channel,
                        bytes.len(),
                        self.total_bytes.saturating_sub(bytes.len() as u64)
                    ))
                } else {
                    Ok(text.into_owned())
                }
            }
        }
    }
}

/// The runner owns readers even when it exits before collecting output.
pub(super) struct PipeReader(tokio::task::JoinHandle<Result<CapturedOutput, String>>);

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) fn spawn_output_readers(
    stdout: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    stderr: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    projection: SubprocessOutputProjection,
) -> (PipeReader, PipeReader) {
    (
        spawn_pipe_reader(stdout, projection.stdout_capture(), "stdout"),
        spawn_pipe_reader(stderr, CaptureMode::Tail, "stderr"),
    )
}

fn spawn_pipe_reader(
    pipe: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    mode: CaptureMode,
    channel: &'static str,
) -> PipeReader {
    PipeReader(tokio::spawn(async move {
        let mut output = CapturedOutput::new(mode, channel);
        let Some(mut pipe) = pipe else {
            return Ok(output);
        };
        let mut chunk = [0; READ_CHUNK_BYTES];
        loop {
            let count = pipe
                .read(&mut chunk)
                .await
                .map_err(|error| format!("Agent {channel} output read failed: {error}"))?;
            if count == 0 {
                return Ok(output);
            }
            // Keep draining after overflow so the child can finish both pipes.
            output.push(&chunk[..count]);
        }
    }))
}

pub(super) async fn collect_pipe(mut task: PipeReader) -> Result<String, String> {
    match tokio::time::timeout(OUTPUT_DRAIN_TIMEOUT, &mut task.0).await {
        Ok(result) => result
            .map_err(|error| format!("Agent output reader task failed: {error}"))?
            .and_then(CapturedOutput::into_string),
        Err(_) => {
            task.0.abort();
            let _ = (&mut task.0).await;
            Err("Agent output drain timed out after root termination; a descendant may retain a pipe".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn text_capture_retains_marked_tail_without_growing_past_its_budget() {
        let mut capture = CapturedOutput::new(CaptureMode::Tail, "stdout");
        let input = format!("{}tail-终点", "0123456789".repeat(TEXT_OUTPUT_LIMIT / 4));
        for chunk in input.as_bytes().chunks(997) {
            capture.push(chunk);
            assert!(capture.bytes.len() <= TEXT_OUTPUT_LIMIT);
            assert!(capture.bytes.capacity() <= TEXT_OUTPUT_LIMIT);
        }
        let output = capture.into_string().unwrap();
        let tail = String::from_utf8_lossy(&input.as_bytes()[input.len() - TEXT_OUTPUT_LIMIT..]);
        assert_eq!(
            output,
            format!(
                "[tachi: stdout truncated; retained last {TEXT_OUTPUT_LIMIT} bytes; discarded {} bytes]\n{tail}",
                input.len() - TEXT_OUTPUT_LIMIT
            )
        );
    }

    #[tokio::test]
    async fn overflowed_protocol_and_stderr_are_drained_to_eof_with_bounded_capacity() {
        let (stdout, mut stdout_writer) = tokio::io::duplex(4096);
        let (stderr, mut stderr_writer) = tokio::io::duplex(4096);
        let (mut stdout, mut stderr) = spawn_output_readers(
            Some(stdout),
            Some(stderr),
            SubprocessOutputProjection::PreserveChannels,
        );
        let writer = tokio::spawn(async move {
            stdout_writer.write_all(b"{\"text\":\"").await.unwrap();
            for _ in 0..=(PROTOCOL_STDOUT_LIMIT / READ_CHUNK_BYTES) {
                stdout_writer
                    .write_all(&[b'x'; READ_CHUNK_BYTES])
                    .await
                    .unwrap();
                stderr_writer
                    .write_all(&[b'e'; READ_CHUNK_BYTES])
                    .await
                    .unwrap();
            }
            stdout_writer.write_all(b"\"}\n").await.unwrap();
            stderr_writer.write_all(b"stderr-tail").await.unwrap();
        });
        tokio::time::timeout(Duration::from_secs(5), writer)
            .await
            .expect("both pipes must keep draining after their output limits")
            .unwrap();
        let stdout = (&mut stdout.0).await.unwrap().unwrap();
        let stderr = (&mut stderr.0).await.unwrap().unwrap();
        assert!(stdout.bytes.capacity() <= PROTOCOL_STDOUT_LIMIT);
        assert!(stderr.bytes.capacity() <= TEXT_OUTPUT_LIMIT);
        assert_eq!(stderr.bytes.len(), TEXT_OUTPUT_LIMIT);
        let error = stdout.into_string().unwrap_err();
        assert!(error.contains("stdout protocol output exceeded"));
        assert!(error.contains(&PROTOCOL_STDOUT_LIMIT.to_string()));
        let stderr = stderr.into_string().unwrap();
        assert!(stderr.starts_with("[tachi: stderr truncated;"));
        assert!(stderr.ends_with("stderr-tail"));
    }

    #[test]
    fn structured_stdout_is_exact_up_to_the_limit_and_overflow_never_returns_partial_json() {
        let mut capture = CapturedOutput::new(CaptureMode::CompleteStdout, "stdout");
        let json = format!("\"{}\"", "x".repeat(PROTOCOL_STDOUT_LIMIT - 2));
        for chunk in json.as_bytes().chunks(READ_CHUNK_BYTES) {
            capture.push(chunk);
        }
        assert_eq!(capture.into_string().unwrap(), json);

        for (agent, transport) in [
            ("claude", "subprocess"),
            ("codex", "subprocess"),
            ("grok", "subprocess"),
            ("kimi", "subprocess"),
            ("dsh", "dsh_headless"),
            ("custom", "acpx"),
            ("opencode", "acp"),
        ] {
            let projection = SubprocessOutputProjection::for_backend(agent, transport);
            let mut capture = CapturedOutput::new(projection.stdout_capture(), "stdout");
            capture.push(json.as_bytes());
            // Whitespace keeps this a valid JSON document, but it no longer fits.
            capture.push(b"\n");
            assert!(capture.bytes.capacity() <= PROTOCOL_STDOUT_LIMIT);
            let error = capture.into_string().expect_err(agent);
            assert!(
                error.contains("stdout protocol output exceeded"),
                "{agent}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn protocol_capture_keeps_utf8_split_across_reads_and_rejects_invalid_bytes() {
        let expected = "{\"text\":\"完整输出\"}\n";
        let (read, mut write) = tokio::io::duplex(1);
        let reader = spawn_pipe_reader(Some(read), CaptureMode::CompleteStdout, "stdout");
        let writer =
            tokio::spawn(async move { write.write_all(expected.as_bytes()).await.unwrap() });
        writer.await.unwrap();
        assert_eq!(collect_pipe(reader).await.unwrap(), expected);

        let reader = spawn_pipe_reader(
            Some(std::io::Cursor::new(b"{\"text\":\"\xff\"}".to_vec())),
            CaptureMode::CompleteStdout,
            "stdout",
        );
        assert!(collect_pipe(reader)
            .await
            .unwrap_err()
            .contains("not valid UTF-8"));
    }

    const FLOOD_CHILD: &str = "dispatch_ops::subprocess::pipe_output::tests::output_flood_child";

    #[test]
    fn output_flood_child() {
        let Some(limit) = std::env::var("TACHI_TEST_OUTPUT_FLOOD_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
        else {
            return;
        };
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        for _ in 0..=(limit / READ_CHUNK_BYTES) {
            stdout.write_all(&[b'o'; READ_CHUNK_BYTES]).unwrap();
            stderr.write_all(&[b'e'; READ_CHUNK_BYTES]).unwrap();
        }
        stdout.write_all(b"stdout-tail\n").unwrap();
        stderr.write_all(b"stderr-tail\n").unwrap();
        stdout.flush().unwrap();
        stderr.flush().unwrap();
    }

    async fn run_flood(
        managed: bool,
        projection: SubprocessOutputProjection,
    ) -> Result<super::super::DispatchResult, String> {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command.args([FLOOD_CHILD, "--exact", "--nocapture"]).env(
            "TACHI_TEST_OUTPUT_FLOOD_LIMIT",
            projection.stdout_capture().limit().to_string(),
        );
        let run = async {
            if managed {
                let directory = tempfile::tempdir().unwrap();
                let (_sender, cancellations) = tokio::sync::mpsc::channel(1);
                super::super::run_managed_custom_subprocess_outcome(
                    command,
                    Duration::from_secs(10),
                    cancellations,
                    directory.path(),
                    false,
                    None,
                    projection,
                )
                .await
                .result
            } else {
                super::super::run_agent_subprocess_with_liveness(
                    command,
                    Duration::from_secs(10),
                    false,
                    None,
                    projection,
                )
                .await
                .result
            }
        };
        tokio::time::timeout(Duration::from_secs(15), run)
            .await
            .expect("bounded output must not block child termination")
    }

    #[tokio::test]
    async fn both_runners_finish_after_text_floods_with_marked_channel_tails() {
        for managed in [false, true] {
            let result = run_flood(managed, SubprocessOutputProjection::Combined)
                .await
                .expect("text overflow retains a bounded result");
            assert_eq!(result.exit_code, Some(0));
            let (stdout, stderr) = result.output.split_once(STDERR_SEPARATOR).unwrap();
            assert!(stdout.starts_with("[tachi: stdout truncated;"));
            assert!(stderr.starts_with("[tachi: stderr truncated;"));
            assert!(stdout.contains("stdout-tail"));
            assert!(stderr.contains("stderr-tail"));
            assert!(stdout.len() <= TEXT_OUTPUT_LIMIT + 160);
            assert!(stderr.len() <= TEXT_OUTPUT_LIMIT + 160);
        }
    }

    #[tokio::test]
    async fn both_runners_reject_protocol_overflow_after_draining_both_pipes() {
        for managed in [false, true] {
            for projection in [
                SubprocessOutputProjection::Structured,
                SubprocessOutputProjection::PreserveChannels,
            ] {
                let error = run_flood(managed, projection)
                    .await
                    .err()
                    .expect("protocol overflow must fail");
                assert!(error.contains("stdout protocol output exceeded"), "{error}");
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stderr_only_projection_preserves_legacy_output_and_dsh_channel() {
        for projection in [
            SubprocessOutputProjection::Combined,
            SubprocessOutputProjection::Structured,
            SubprocessOutputProjection::PreserveChannels,
        ] {
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("printf 'loader failure\\n' >&2; exit 1");
            let outcome = super::super::run_agent_subprocess_with_liveness(
                command,
                Duration::from_secs(5),
                false,
                None,
                projection,
            )
            .await;
            let result = outcome.result.expect("stderr-only child completed");
            assert_eq!(result.exit_code, Some(1));
            match projection {
                SubprocessOutputProjection::Combined | SubprocessOutputProjection::Structured => {
                    assert_eq!(result.output, "loader failure\n");
                }
                SubprocessOutputProjection::PreserveChannels => {
                    assert_eq!(result.output, format!("{STDERR_SEPARATOR}loader failure\n"));
                }
            }
        }
    }

    #[tokio::test]
    async fn retained_pipe_times_out_and_reader_is_joined() {
        let (read, mut write) = tokio::io::duplex(32);
        let error = collect_pipe(spawn_pipe_reader(Some(read), CaptureMode::Tail, "stdout"))
            .await
            .unwrap_err();
        assert!(error.contains("output drain timed out"));
        assert!(write.write_all(b"late descendant output").await.is_err());
    }

    #[tokio::test]
    async fn early_runner_exit_aborts_owned_reader() {
        let (read, mut write) = tokio::io::duplex(32);
        drop(spawn_pipe_reader(Some(read), CaptureMode::Tail, "stdout"));
        tokio::task::yield_now().await;
        assert!(write.write_all(b"late descendant output").await.is_err());
    }
}

#[cfg(all(test, unix))]
mod escaped_descendant_tests {
    use super::super::*;
    const CHILD: &str =
        "dispatch_ops::subprocess::pipe_output::escaped_descendant_tests::escaped_pipe_child";

    struct StopDescendant(std::path::PathBuf);
    impl Drop for StopDescendant {
        fn drop(&mut self) {
            std::fs::write(&self.0, b"stop").unwrap();
            // The child closes its inherited pipes before publishing done.
            for _ in 0..200 {
                if self.0.with_extension("done").exists() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn escaped_pipe_child() {
        let Some(stop) = std::env::var_os("TACHI_TEST_ESCAPED_PIPE_STOP") else {
            return;
        };
        let stop_path = std::path::PathBuf::from(stop);
        use std::os::unix::ffi::OsStrExt;
        let stop = std::ffi::CString::new(stop_path.as_os_str().as_bytes()).unwrap();
        let done = std::ffi::CString::new(stop_path.with_extension("done").as_os_str().as_bytes())
            .unwrap();
        let mut ready = [0; 2];
        assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
        // The fork child uses only libc until _exit; no allocator or Rust locks.
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            unsafe {
                libc::close(ready[0]);
                if libc::setsid() < 0 {
                    libc::_exit(71);
                }
                libc::write(ready[1], b"r".as_ptr().cast(), 1);
                libc::close(ready[1]);
                for _ in 0..1500 {
                    if libc::access(stop.as_ptr(), libc::F_OK) == 0 {
                        break;
                    }
                    libc::usleep(20000);
                }
                libc::close(libc::STDOUT_FILENO);
                libc::close(libc::STDERR_FILENO);
                let fd = libc::open(done.as_ptr(), libc::O_WRONLY | libc::O_CREAT, 0o600);
                if fd >= 0 {
                    libc::close(fd);
                }
                libc::_exit(0);
            }
        }
        unsafe {
            libc::close(ready[1]);
        }
        let mut byte = 0u8;
        assert_eq!(
            unsafe { libc::read(ready[0], (&mut byte as *mut u8).cast(), 1) },
            1
        );
        unsafe {
            libc::close(ready[0]);
        }
        assert_eq!(byte, b'r');
    }

    #[tokio::test]
    async fn escaped_descendant_holding_output_does_not_stall_terminalization() {
        let dir = tempfile::tempdir().unwrap();
        let stop = dir.path().join("stop");
        let release = StopDescendant(stop.clone());
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([CHILD, "--exact", "--nocapture"])
            .env("TACHI_TEST_ESCAPED_PIPE_STOP", &stop);
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            run_agent_subprocess_with_liveness(
                command,
                Duration::from_secs(5),
                false,
                None,
                SubprocessOutputProjection::Combined,
            ),
        )
        .await;
        drop(release);
        assert!(
            stop.with_extension("done").exists(),
            "owned escaped fixture must release its pipes"
        );
        let outcome = outcome.expect("root exit must not leave an unbounded pipe wait");
        assert!(
            matches!(outcome.result, Err(ref error) if error.contains("output drain timed out"))
        );
        assert!(matches!(
            outcome.liveness,
            crate::exec_env_postflight::RunnerLivenessEvidence::Indeterminate { .. }
        ));
    }
}
