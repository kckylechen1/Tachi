use std::time::Duration;
use tokio::io::AsyncReadExt;

const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// The runner owns readers even when it exits before collecting output.
pub(super) struct PipeReader(tokio::task::JoinHandle<Vec<u8>>);

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) fn spawn_pipe_reader(
    pipe: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
) -> PipeReader {
    PipeReader(tokio::spawn(async move {
        let Some(mut pipe) = pipe else {
            return Vec::new();
        };
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes).await;
        bytes
    }))
}

pub(super) async fn collect_pipe(mut task: PipeReader) -> Result<String, String> {
    match tokio::time::timeout(OUTPUT_DRAIN_TIMEOUT, &mut task.0).await {
        Ok(result) => result
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            .map_err(|error| format!("Agent output reader task failed: {error}")),
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

    #[tokio::test]
    async fn retained_pipe_times_out_and_reader_is_joined() {
        let (read, mut write) = tokio::io::duplex(32);
        let error = collect_pipe(spawn_pipe_reader(Some(read)))
            .await
            .unwrap_err();
        assert!(error.contains("output drain timed out"));
        use tokio::io::AsyncWriteExt;
        assert!(write.write_all(b"late descendant output").await.is_err());
    }

    #[tokio::test]
    async fn early_runner_exit_aborts_owned_reader() {
        let (read, mut write) = tokio::io::duplex(32);
        drop(spawn_pipe_reader(Some(read)));
        tokio::task::yield_now().await;
        use tokio::io::AsyncWriteExt;
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
        assert!(outcome
            .result
            .unwrap_err()
            .contains("output drain timed out"));
        assert!(matches!(
            outcome.liveness,
            crate::exec_env_postflight::RunnerLivenessEvidence::Indeterminate { .. }
        ));
    }
}
