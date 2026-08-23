use super::dispatch::DispatchResult;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{mpsc, Semaphore};

const DEFAULT_OPENCODE_SOP_MAX_CONCURRENCY: usize = 2;
const MAX_OPENCODE_SOP_CONCURRENCY: usize = 32;
const PROCESS_GROUP_TERM_GRACE: Duration = Duration::from_millis(1500);

static OPENCODE_SOP_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

#[cfg(test)]
static MANAGED_CANCEL_PROBE_FAILURE_RUN_DIRS: OnceLock<
    std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
> = OnceLock::new();

#[cfg(test)]
static MANAGED_PANIC_AFTER_SPAWN_RUN_DIRS: OnceLock<
    std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
> = OnceLock::new();

#[cfg(test)]
static MANAGED_CANCEL_CHILD_PID_OBSERVERS: OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<std::path::PathBuf, std::sync::mpsc::SyncSender<u32>>,
    >,
> = OnceLock::new();

#[cfg(test)]
struct ManagedCancelDequeueBarrier {
    entered: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
    observation: std::sync::mpsc::SyncSender<ManagedCancelTryWaitObservation>,
}

#[cfg(test)]
pub(crate) struct ManagedCancelDequeueBarrierGuard {
    run_dir: std::path::PathBuf,
}

#[cfg(test)]
impl Drop for ManagedCancelDequeueBarrierGuard {
    fn drop(&mut self) {
        if let Some(barriers) = MANAGED_CANCEL_DEQUEUE_BARRIERS.get() {
            barriers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.run_dir);
        }
    }
}

#[cfg(test)]
pub(crate) struct ManagedCancelChildPidObserverGuard {
    run_dir: std::path::PathBuf,
}

#[cfg(test)]
impl Drop for ManagedCancelChildPidObserverGuard {
    fn drop(&mut self) {
        if let Some(observers) = MANAGED_CANCEL_CHILD_PID_OBSERVERS.get() {
            observers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.run_dir);
        }
    }
}

#[cfg(test)]
pub(crate) struct ManagedCancelTryWaitObservation {
    pub(crate) run_dir: std::path::PathBuf,
    pub(crate) child_pid: Option<u32>,
    pub(crate) try_wait_result: &'static str,
    pub(crate) sigterm_result: &'static str,
    pub(crate) reap_result: &'static str,
    pub(crate) group_absent: bool,
    pub(crate) runner_error: Option<String>,
    pub(crate) termination_proof: Option<&'static str>,
    pub(crate) finalization_directive: bool,
    pub(crate) finalization_result: Option<&'static str>,
    pub(crate) canonical_receipt: Option<String>,
    pub(crate) canonical_reason: Option<String>,
    pub(crate) canonical_state: Option<String>,
    completion: std::sync::mpsc::SyncSender<ManagedCancelTryWaitObservation>,
}

#[cfg(test)]
impl ManagedCancelTryWaitObservation {
    fn new(
        completion: std::sync::mpsc::SyncSender<Self>,
        run_dir: &std::path::Path,
        child_pid: Option<u32>,
        try_wait_result: &'static str,
    ) -> Self {
        Self {
            run_dir: run_dir.to_path_buf(),
            child_pid,
            try_wait_result,
            sigterm_result: "not_attempted",
            reap_result: "not_attempted",
            group_absent: false,
            runner_error: None,
            termination_proof: None,
            finalization_directive: false,
            finalization_result: None,
            canonical_receipt: None,
            canonical_reason: None,
            canonical_state: None,
            completion,
        }
    }

    pub(crate) fn complete(
        mut self,
        run_dir: &std::path::Path,
        completion: Option<&crate::managed_run_control::CancelCompletion>,
    ) {
        self.finalization_result = completion.map(|completion| match completion {
            crate::managed_run_control::CancelCompletion::Confirmed { .. } => {
                "cancellation_confirmed"
            }
            crate::managed_run_control::CancelCompletion::Unconfirmed => "termination_unconfirmed",
            crate::managed_run_control::CancelCompletion::Unavailable(_) => {
                "cancellation_unavailable"
            }
        });
        if let Ok(Some(status)) =
            crate::task_lifecycle::read_json_file(&run_dir.join("status.json"))
        {
            self.canonical_receipt = status
                .get("cancellation")
                .and_then(serde_json::Value::as_object)
                .and_then(|receipt| receipt.get("receipt"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            self.canonical_reason = status
                .get("cancellation")
                .and_then(serde_json::Value::as_object)
                .and_then(|receipt| receipt.get("reason"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            self.canonical_state = status
                .get("state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }
        let completion = self.completion.clone();
        let _ = completion.send(self);
    }
}

#[cfg(test)]
static MANAGED_CANCEL_DEQUEUE_BARRIERS: OnceLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, ManagedCancelDequeueBarrier>>,
> = OnceLock::new();

#[cfg(test)]
type ManagedPreSpawnBarrierKey = (std::path::PathBuf, std::path::PathBuf);
#[cfg(test)]
type ManagedPreSpawnBarrier = (
    std::sync::mpsc::SyncSender<()>,
    std::sync::mpsc::Receiver<()>,
);
#[cfg(test)]
type ManagedPreSpawnBarriers =
    std::sync::Mutex<std::collections::HashMap<ManagedPreSpawnBarrierKey, ManagedPreSpawnBarrier>>;
#[cfg(test)]
static MANAGED_PRE_SPAWN_BARRIERS: OnceLock<ManagedPreSpawnBarriers> = OnceLock::new();

#[cfg(test)]
pub(crate) struct ManagedPreSpawnBarrierGuard {
    key: ManagedPreSpawnBarrierKey,
}

#[cfg(test)]
impl Drop for ManagedPreSpawnBarrierGuard {
    fn drop(&mut self) {
        if let Some(barriers) = MANAGED_PRE_SPAWN_BARRIERS.get() {
            barriers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&self.key);
        }
    }
}

#[cfg(test)]
pub(crate) fn install_managed_pre_spawn_barrier(
    home: &std::path::Path,
    run_root: &std::path::Path,
) -> (
    ManagedPreSpawnBarrierGuard,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::SyncSender<()>,
) {
    let key = (home.to_path_buf(), run_root.to_path_buf());
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    assert!(MANAGED_PRE_SPAWN_BARRIERS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key.clone(), (entered_tx, release_rx))
        .is_none());
    (ManagedPreSpawnBarrierGuard { key }, entered_rx, release_tx)
}

#[cfg(test)]
type ManagedBeforeSelectKey = (std::path::PathBuf, std::path::PathBuf);

#[cfg(test)]
struct ManagedBeforeSelectBarrier {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
pub(crate) struct ManagedBeforeSelectBarrierGuard(ManagedBeforeSelectKey);

#[cfg(test)]
impl Drop for ManagedBeforeSelectBarrierGuard {
    fn drop(&mut self) {
        if let Some(barriers) = MANAGED_BEFORE_SELECT_BARRIERS.get() {
            barriers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.0);
        }
    }
}

#[cfg(test)]
static MANAGED_BEFORE_SELECT_BARRIERS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<ManagedBeforeSelectKey, ManagedBeforeSelectBarrier>>,
> = std::sync::OnceLock::new();

/// Stops one runner immediately before its managed select loop. The key binds
/// the fixture's isolated home and run root, so parallel tests cannot borrow a
/// different run's deadline race.
#[cfg(test)]
pub(crate) fn install_managed_before_select_barrier(
    home: &std::path::Path,
    run_root: &std::path::Path,
) -> (
    ManagedBeforeSelectBarrierGuard,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let key = (home.to_path_buf(), run_root.to_path_buf());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    assert!(
        MANAGED_BEFORE_SELECT_BARRIERS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                key.clone(),
                ManagedBeforeSelectBarrier {
                    entered: entered_tx,
                    release: release_rx,
                },
            )
            .is_none(),
        "managed before-select barrier already installed for isolated run root"
    );
    (ManagedBeforeSelectBarrierGuard(key), entered_rx, release_tx)
}

#[cfg(test)]
fn pause_managed_before_select(_run_dir: &std::path::Path) {
    let Some(home) = std::env::var_os("TACHI_HOME") else {
        return;
    };
    let Some(run_root) = std::env::var_os("TACHI_RUN_ROOT") else {
        return;
    };
    let key = (
        std::path::PathBuf::from(home),
        std::path::PathBuf::from(run_root),
    );
    let barrier = MANAGED_BEFORE_SELECT_BARRIERS.get().and_then(|barriers| {
        barriers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&key)
    });
    if let Some(barrier) = barrier {
        let _ = barrier.entered.send(());
        let _ = barrier.release.recv();
    }
}

#[cfg(test)]
fn pause_managed_pre_spawn() {
    let Some(home) = std::env::var_os("TACHI_HOME") else {
        return;
    };
    let Some(run_root) = std::env::var_os("TACHI_RUN_ROOT") else {
        return;
    };
    let key = (
        std::path::PathBuf::from(home),
        std::path::PathBuf::from(run_root),
    );
    let barrier = MANAGED_PRE_SPAWN_BARRIERS.get().and_then(|barriers| {
        barriers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&key)
    });
    if let Some((entered, release)) = barrier {
        let _ = entered.send(());
        let _ = release.recv();
    }
}

pub(super) async fn run_agent_subprocess(
    mut cmd: Command,
    timeout: Duration,
) -> Result<DispatchResult, String> {
    run_agent_subprocess_inner(&mut cmd, timeout, None).await
}

pub(super) async fn run_managed_custom_subprocess_outcome(
    mut cmd: Command,
    timeout: Duration,
    mut cancellations: mpsc::Receiver<crate::managed_run_control::ManagedCancelCommand>,
    run_dir: &std::path::Path,
) -> ManagedSubprocessOutcome {
    #[cfg(not(unix))]
    {
        let _ = cancellations;
        let _ = run_dir;
        // A platform without process-group control still runs an ordinary
        // custom LaunchSpec. Only the cancellation control plane is absent.
        return ManagedSubprocessOutcome::plain(run_agent_subprocess(cmd, timeout).await);
    }
    #[cfg(unix)]
    {
        #[cfg(test)]
        pause_managed_pre_spawn();
        if let Ok(command) = cancellations.try_recv() {
            return finish_pre_spawn_cancellation(command);
        }
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // This guard owns managed termination and synchronous reaping on
        // unwind, so tokio must not later signal a recycled numeric PID.
        cmd.kill_on_drop(false);
        configure_process_group(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                return ManagedSubprocessOutcome::plain(Err(format!(
                    "Failed to spawn agent process: {error}"
                )));
            }
        };
        let pid = child.id();
        let mut process_group = ManagedProcessGroupGuard::arm(pid);
        #[cfg(test)]
        record_managed_cancel_child_pid(run_dir, pid);
        #[cfg(test)]
        panic_after_managed_spawn(run_dir);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_task = tokio::spawn(read_pipe(stdout));
        let stderr_task = tokio::spawn(read_pipe(stderr));
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match observe_managed_root_exit_without_reap(pid, run_dir) {
                Ok(true) => {
                    // The leader is a zombie, so its PID still owns the group
                    // identity while descendant cleanup is performed.
                    let sigterm = process_group.signal(libc::SIGTERM);
                    process_group.prepare_group_for_root_reap(sigterm).await;
                    let status = reap_managed_root(&mut child, &mut process_group).await;
                    let group_absent = process_group.prove_absence_after_root_reaped().await;
                    if !group_absent {
                        drain_managed_output(stdout_task, stderr_task).await;
                        return ManagedSubprocessOutcome::plain(Err(
                            "termination_unconfirmed".to_string()
                        ));
                    }
                    return ManagedSubprocessOutcome::plain(match status {
                        Ok(status) => finish_managed_output(status, stdout_task, stderr_task).await,
                        Err(error) => {
                            drain_managed_output(stdout_task, stderr_task).await;
                            Err(format!("Agent process error: {error}"))
                        }
                    });
                }
                Ok(false) => {}
                Err(error) => {
                    let sigterm = process_group.signal(libc::SIGTERM);
                    process_group.prepare_group_for_root_reap(sigterm).await;
                    let _ = reap_managed_root(&mut child, &mut process_group).await;
                    drain_managed_output(stdout_task, stderr_task).await;
                    return ManagedSubprocessOutcome::plain(Err(format!(
                        "managed child probe failed: {error}"
                    )));
                }
            }
            // `biased` below otherwise gives a simultaneously-ready cancel
            // precedence over the watchdog. Check before receiving a command
            // so a request that arrives after the deadline cannot signal a
            // process whose timeout already owns terminalization.
            if tokio::time::Instant::now() >= deadline {
                if matches!(
                    observe_managed_root_exit_without_reap(pid, run_dir),
                    Ok(true)
                ) {
                    continue;
                }
                let sigterm = process_group.signal(libc::SIGTERM);
                process_group.prepare_group_for_root_reap(sigterm).await;
                let _ = reap_managed_root(&mut child, &mut process_group).await;
                drain_managed_output(stdout_task, stderr_task).await;
                return ManagedSubprocessOutcome::plain(Err("process timed out".to_string()));
            }
            #[cfg(test)]
            pause_managed_before_select(run_dir);
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => {
                    if matches!(observe_managed_root_exit_without_reap(pid, run_dir), Ok(true)) {
                        continue;
                    }
                    let sigterm = process_group.signal(libc::SIGTERM);
                    process_group.prepare_group_for_root_reap(sigterm).await;
                    let _ = reap_managed_root(&mut child, &mut process_group).await;
                    drain_managed_output(stdout_task, stderr_task).await;
                    return ManagedSubprocessOutcome::plain(Err(format!(
                        "Agent process timed out after {}s (process group killed)", timeout.as_secs())));
                }
                command = cancellations.recv() => {
                    let Some(command) = command else {
                        let sigterm = process_group.signal(libc::SIGTERM);
                        process_group.prepare_group_for_root_reap(sigterm).await;
                        let _ = reap_managed_root(&mut child, &mut process_group).await;
                        drain_managed_output(stdout_task, stderr_task).await;
                        return ManagedSubprocessOutcome::plain(Err("managed cancellation channel closed".to_string()));
                    };
                    if tokio::time::Instant::now() >= deadline {
                        let sigterm = process_group.signal(libc::SIGTERM);
                        process_group.prepare_group_for_root_reap(sigterm).await;
                        let _ = reap_managed_root(&mut child, &mut process_group).await;
                        drain_managed_output(stdout_task, stderr_task).await;
                        return ManagedSubprocessOutcome::plain(Err(format!(
                            "Agent process timed out after {}s (process group killed)", timeout.as_secs())));
                    }
                    #[cfg(test)]
                    let mut command = command;
                    #[cfg(test)]
                    let observation = pause_managed_cancel_after_dequeue(run_dir);
                    let exited = match try_wait_for_managed_cancellation(pid, run_dir) {
                        Ok(exited) => exited,
                        Err(error) => {
                            let sigterm = process_group.signal(libc::SIGTERM);
                            process_group.prepare_group_for_root_reap(sigterm).await;
                            let _ = reap_managed_root(&mut child, &mut process_group).await;
                            drain_managed_output(stdout_task, stderr_task).await;
                            return ManagedSubprocessOutcome::dequeued(
                                Err(format!("managed cancellation child probe failed: {error}")), command);
                        }
                    };
                    #[cfg(test)]
                    let mut observation = observation.map(|completion| {
                        ManagedCancelTryWaitObservation::new(
                            completion, run_dir, pid,
                            if exited { "exited" } else { "running" },
                        )
                    });
                    if exited {
                        let sigterm = process_group.signal(libc::SIGTERM);
                        process_group.prepare_group_for_root_reap(sigterm).await;
                        let status = reap_managed_root(&mut child, &mut process_group).await;
                        let group_absent = process_group.prove_absence_after_root_reaped().await;
                        if !group_absent {
                            #[cfg(test)]
                            if let Some(mut observation) = observation {
                                observation.reap_result = "root_reaped";
                                observation.group_absent = false;
                                observation.runner_error = Some("termination_unconfirmed".to_string());
                                observation.finalization_directive = true;
                                command.test_observation = Some(observation);
                            }
                            drain_managed_output(stdout_task, stderr_task).await;
                            return ManagedSubprocessOutcome::dequeued(
                                Err("termination_unconfirmed".to_string()), command);
                        }
                        #[cfg(test)]
                        if let Some(mut observation) = observation {
                            observation.reap_result = "root_reaped";
                            observation.group_absent = group_absent;
                            let result = match status {
                                Ok(status) => finish_managed_output(status, stdout_task, stderr_task).await,
                                Err(error) => Err(format!("Agent process error: {error}")),
                            };
                            observation.runner_error = result.as_ref().err().cloned();
                            observation.finalization_directive = true;
                            command.test_observation = Some(observation);
                            return ManagedSubprocessOutcome::dequeued(result, command);
                        }
                        return ManagedSubprocessOutcome::dequeued(
                            match status {
                                Ok(status) => finish_managed_output(status, stdout_task, stderr_task).await,
                                Err(error) => Err(format!("Agent process error: {error}")),
                            }, command);
                    }
                    let sigterm = process_group.signal(libc::SIGTERM);
                    #[cfg(test)]
                    if let Some(observation) = observation.as_mut() {
                        observation.sigterm_result = sigterm.as_str();
                    }
                    process_group.prepare_group_for_root_reap(sigterm).await;
                    let status = reap_managed_root(&mut child, &mut process_group).await;
                    let group_absent = process_group.prove_absence_after_root_reaped().await;
                    #[cfg(test)]
                    if let Some(mut observation) = observation {
                        observation.reap_result = "root_reaped";
                        observation.group_absent = group_absent;
                        if !group_absent {
                            observation.runner_error = Some("termination_unconfirmed".to_string());
                            observation.finalization_directive = true;
                            command.test_observation = Some(observation);
                            drain_managed_output(stdout_task, stderr_task).await;
                            return ManagedSubprocessOutcome::dequeued(Err("termination_unconfirmed".to_string()), command);
                        }
                        let _ = status;
                        drain_managed_output(stdout_task, stderr_task).await;
                        observation.runner_error = Some("managed_cancelled".to_string());
                        observation.termination_proof = Some("unix_process_group_absent");
                        observation.finalization_directive = true;
                        command.test_observation = Some(observation);
                        return ManagedSubprocessOutcome::dequeued_with_proof(Err("managed_cancelled".to_string()), command, "unix_process_group_absent");
                    }
                    let _ = status;
                    drain_managed_output(stdout_task, stderr_task).await;
                    return if group_absent {
                        ManagedSubprocessOutcome::dequeued_with_proof(Err("managed_cancelled".to_string()), command, "unix_process_group_absent")
                    } else {
                        ManagedSubprocessOutcome::dequeued(Err("termination_unconfirmed".to_string()), command)
                    };
                }
                _ = tokio::time::sleep_until(deadline) => {
                    // A final non-reaping observation keeps a late natural
                    // completion ahead of timeout signalling.
                    if matches!(observe_managed_root_exit_without_reap(pid, run_dir), Ok(true)) {
                        continue;
                    }
                    let sigterm = process_group.signal(libc::SIGTERM);
                    process_group.prepare_group_for_root_reap(sigterm).await;
                    let _ = reap_managed_root(&mut child, &mut process_group).await;
                    drain_managed_output(stdout_task, stderr_task).await;
                    return ManagedSubprocessOutcome::plain(Err(format!(
                        "Agent process timed out after {}s (process group killed)", timeout.as_secs())));
                }
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    }
}

pub(super) struct ManagedSubprocessOutcome {
    pub(super) result: Result<DispatchResult, String>,
    pub(super) cancellation: Option<crate::managed_run_control::ManagedCancelCommand>,
    pub(super) termination_proof: Option<&'static str>,
}

impl ManagedSubprocessOutcome {
    pub(super) fn plain(result: Result<DispatchResult, String>) -> Self {
        Self {
            result,
            cancellation: None,
            termination_proof: None,
        }
    }

    fn dequeued(
        result: Result<DispatchResult, String>,
        command: crate::managed_run_control::ManagedCancelCommand,
    ) -> Self {
        Self {
            result,
            cancellation: Some(command),
            termination_proof: None,
        }
    }

    fn dequeued_with_proof(
        result: Result<DispatchResult, String>,
        command: crate::managed_run_control::ManagedCancelCommand,
        termination_proof: &'static str,
    ) -> Self {
        Self {
            result,
            cancellation: Some(command),
            termination_proof: Some(termination_proof),
        }
    }
}

/// Direct runner compatibility for focused process-lifecycle tests. Production
/// dispatches use `run_managed_custom_subprocess_outcome` and defer the reply
/// to the background terminal writer.
#[cfg(test)]
pub(crate) async fn run_managed_custom_subprocess(
    cmd: Command,
    timeout: Duration,
    cancellations: mpsc::Receiver<crate::managed_run_control::ManagedCancelCommand>,
    run_dir: &std::path::Path,
) -> Result<DispatchResult, String> {
    let outcome = run_managed_custom_subprocess_outcome(cmd, timeout, cancellations, run_dir).await;
    if let Some(command) = outcome.cancellation {
        let completion = crate::managed_run_control::finalize_dequeued_managed_cancellation(
            run_dir,
            command.expected_status_revision,
            outcome.result.as_ref().err().map(String::as_str),
            outcome.termination_proof,
        );
        let _ = command.response.send(completion);
    }
    outcome.result
}

#[cfg(test)]
pub(crate) fn install_managed_cancel_dequeue_barrier(
    run_dir: &std::path::Path,
) -> (
    ManagedCancelDequeueBarrierGuard,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::SyncSender<()>,
    std::sync::mpsc::Receiver<ManagedCancelTryWaitObservation>,
) {
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let (observation_tx, observation_rx) = std::sync::mpsc::sync_channel(1);
    let run_dir = run_dir.to_path_buf();
    let barriers = MANAGED_CANCEL_DEQUEUE_BARRIERS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    assert!(
        barriers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                run_dir.clone(),
                ManagedCancelDequeueBarrier {
                    entered: entered_tx,
                    release: release_rx,
                    observation: observation_tx,
                },
            )
            .is_none(),
        "managed cancellation dequeue barrier already installed for run"
    );
    (
        ManagedCancelDequeueBarrierGuard { run_dir },
        entered_rx,
        release_tx,
        observation_rx,
    )
}

#[cfg(test)]
fn pause_managed_cancel_after_dequeue(
    run_dir: &std::path::Path,
) -> Option<std::sync::mpsc::SyncSender<ManagedCancelTryWaitObservation>> {
    let barrier = MANAGED_CANCEL_DEQUEUE_BARRIERS.get().and_then(|barriers| {
        barriers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(run_dir)
    })?;
    let _ = barrier.entered.send(());
    let _ = barrier.release.recv();
    Some(barrier.observation)
}

#[cfg(unix)]
fn process_group_absent(pid: Option<u32>) -> bool {
    let Some(pid) = pid else {
        return true;
    };
    let rc = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
    rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
fn wait_for_process_group_absence_after_reap(pid: Option<u32>) {
    while !process_group_absent(pid) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(unix)]
async fn wait_for_process_group_absence(pid: Option<u32>) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if process_group_absent(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    process_group_absent(pid)
}

#[cfg(unix)]
fn finish_pre_spawn_cancellation(
    command: crate::managed_run_control::ManagedCancelCommand,
) -> ManagedSubprocessOutcome {
    ManagedSubprocessOutcome::dequeued_with_proof(
        Err("managed_cancelled".to_string()),
        command,
        "spawn_suppressed",
    )
}

async fn finish_managed_output(
    status: std::process::ExitStatus,
    stdout_task: tokio::task::JoinHandle<Vec<u8>>,
    stderr_task: tokio::task::JoinHandle<Vec<u8>>,
) -> Result<DispatchResult, String> {
    let stdout = collect_pipe(stdout_task).await?;
    let stderr = collect_pipe(stderr_task).await?;
    let output = if stdout.is_empty() && !stderr.is_empty() {
        stderr
    } else if !stderr.is_empty() {
        format!("{stdout}\n\n--- stderr ---\n{stderr}")
    } else {
        stdout
    };
    Ok(DispatchResult {
        output,
        exit_code: status.code(),
        observed_model: None,
    })
}

/// Managed cancellation owns the child process group and both pipe readers.
/// After reaping the group, await the readers before publishing an outcome so a
/// confirmed cancellation cannot leave detached reader tasks behind.
async fn drain_managed_output(
    stdout_task: tokio::task::JoinHandle<Vec<u8>>,
    stderr_task: tokio::task::JoinHandle<Vec<u8>>,
) {
    let _ = tokio::join!(collect_pipe(stdout_task), collect_pipe(stderr_task));
}

#[cfg(unix)]
fn observe_managed_root_exit_without_reap(
    child_pid: Option<u32>,
    _run_dir: &std::path::Path,
) -> std::io::Result<bool> {
    let Some(child_pid) = child_pid else {
        return Ok(true);
    };
    // `WNOWAIT` observes a zombie without consuming it. Keeping the leader
    // unreaped reserves its numeric PGID until all group signals are complete.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            child_pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(unix)]
fn try_wait_for_managed_cancellation(
    child_pid: Option<u32>,
    run_dir: &std::path::Path,
) -> std::io::Result<bool> {
    #[cfg(test)]
    {
        let configured_run_dirs = MANAGED_CANCEL_PROBE_FAILURE_RUN_DIRS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if configured_run_dirs.contains(run_dir) {
            return Err(std::io::Error::other(
                "injected managed cancellation probe failure",
            ));
        }
    }
    observe_managed_root_exit_without_reap(child_pid, run_dir)
}

#[cfg(unix)]
async fn reap_managed_root(
    child: &mut tokio::process::Child,
    process_group: &mut ManagedProcessGroupGuard,
) -> std::io::Result<std::process::ExitStatus> {
    let status = child.wait().await;
    // This must happen even for a failed wait: the Child has been asked to
    // reap, so Drop and every guard API become signal-free permanently.
    process_group.mark_root_reaped();
    status
}

#[cfg(unix)]
struct ManagedProcessGroupGuard {
    state: ManagedProcessGroupState,
}

#[cfg(unix)]
enum ManagedProcessGroupState {
    RootLive(Option<u32>),
    RootReaped(Option<u32>),
}

#[cfg(unix)]
impl ManagedProcessGroupGuard {
    fn arm(pid: Option<u32>) -> Self {
        Self {
            state: ManagedProcessGroupState::RootLive(pid),
        }
    }

    fn signal(&self, signal: libc::c_int) -> ProcessGroupSignal {
        match self.state {
            ManagedProcessGroupState::RootLive(pid) => signal_process_group(pid, signal),
            ManagedProcessGroupState::RootReaped(_) => ProcessGroupSignal::RootReaped,
        }
    }

    async fn prepare_group_for_root_reap(&self, sigterm: ProcessGroupSignal) {
        if matches!(sigterm, ProcessGroupSignal::Absent) {
            return;
        }
        if wait_for_process_group_absence(self.pid()).await {
            return;
        }
        let _ = self.signal(libc::SIGKILL);
    }

    async fn prove_absence_after_root_reaped(&self) -> bool {
        match self.state {
            ManagedProcessGroupState::RootLive(_) => false,
            ManagedProcessGroupState::RootReaped(pid) => wait_for_process_group_absence(pid).await,
        }
    }

    fn mark_root_reaped(&mut self) {
        let pid = self.pid();
        self.state = ManagedProcessGroupState::RootReaped(pid);
    }

    fn pid(&self) -> Option<u32> {
        match self.state {
            ManagedProcessGroupState::RootLive(pid) => pid,
            ManagedProcessGroupState::RootReaped(_) => None,
        }
    }
}

#[cfg(unix)]
impl Drop for ManagedProcessGroupGuard {
    fn drop(&mut self) {
        if let ManagedProcessGroupState::RootLive(pid) = self.state {
            // The guard is declared after tokio's Child, so it drops first on
            // unwind and retains reaping ownership until this returns.
            terminate_process_group(pid, libc::SIGKILL);
            if let Some(pid) = pid {
                let mut status = 0;
                loop {
                    let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
                    if waited >= 0
                        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                    {
                        break;
                    }
                }
            }
            self.state = ManagedProcessGroupState::RootReaped(pid);
            wait_for_process_group_absence_after_reap(pid);
        }
    }
}

#[cfg(all(test, unix))]
#[test]
fn issue_1825_reaped_root_guard_never_signals_a_reused_numeric_group() {
    // `foreign_group_pid` models a numeric PGID that has been reused after the
    // managed root was reaped. The type state rejects it before libc::kill can
    // observe that number, so an unrelated process group cannot be signalled.
    let foreign_group_pid = Some(42);
    let mut guard = ManagedProcessGroupGuard::arm(foreign_group_pid);
    guard.mark_root_reaped();
    assert!(matches!(
        guard.signal(libc::SIGTERM),
        ProcessGroupSignal::RootReaped
    ));
    assert!(matches!(
        guard.signal(libc::SIGKILL),
        ProcessGroupSignal::RootReaped
    ));
}

#[cfg(test)]
struct ManagedCancelProbeFailureGuard(std::path::PathBuf);

#[cfg(test)]
impl Drop for ManagedCancelProbeFailureGuard {
    fn drop(&mut self) {
        let mut configured_run_dirs = MANAGED_CANCEL_PROBE_FAILURE_RUN_DIRS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        configured_run_dirs.remove(&self.0);
    }
}

#[cfg(test)]
fn inject_managed_cancel_probe_failure(
    run_dir: &std::path::Path,
) -> ManagedCancelProbeFailureGuard {
    let run_dir = run_dir.to_path_buf();
    let mut configured_run_dirs = MANAGED_CANCEL_PROBE_FAILURE_RUN_DIRS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        configured_run_dirs.insert(run_dir.clone()),
        "managed cancellation probe failure already configured"
    );
    ManagedCancelProbeFailureGuard(run_dir)
}

#[cfg(test)]
pub(crate) fn install_managed_cancel_child_pid_observer(
    run_dir: &std::path::Path,
) -> (
    ManagedCancelChildPidObserverGuard,
    std::sync::mpsc::Receiver<u32>,
) {
    let run_dir = run_dir.to_path_buf();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let observers = MANAGED_CANCEL_CHILD_PID_OBSERVERS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    assert!(
        observers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(run_dir.clone(), sender)
            .is_none(),
        "managed cancellation child observer already installed for run"
    );
    (ManagedCancelChildPidObserverGuard { run_dir }, receiver)
}

#[cfg(test)]
fn record_managed_cancel_child_pid(run_dir: &std::path::Path, pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    let Some(sender) = MANAGED_CANCEL_CHILD_PID_OBSERVERS
        .get()
        .and_then(|observers| {
            observers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(run_dir)
        })
    else {
        return;
    };
    let _ = sender.send(pid);
}

#[cfg(test)]
struct ManagedPanicAfterSpawnGuard(std::path::PathBuf);

#[cfg(test)]
type ManagedPanicAfterSpawnRootKey = (std::path::PathBuf, std::path::PathBuf);

#[cfg(test)]
pub(crate) struct ManagedPanicAfterSpawnRootGuard(ManagedPanicAfterSpawnRootKey);

#[cfg(test)]
static MANAGED_PANIC_AFTER_SPAWN_ROOTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashSet<ManagedPanicAfterSpawnRootKey>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
impl Drop for ManagedPanicAfterSpawnRootGuard {
    fn drop(&mut self) {
        if let Some(roots) = MANAGED_PANIC_AFTER_SPAWN_ROOTS.get() {
            roots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.0);
        }
    }
}

#[cfg(test)]
pub(crate) fn install_managed_panic_after_spawn_for_run_root(
    home: &std::path::Path,
    run_root: &std::path::Path,
) -> ManagedPanicAfterSpawnRootGuard {
    let key = (home.to_path_buf(), run_root.to_path_buf());
    assert!(
        MANAGED_PANIC_AFTER_SPAWN_ROOTS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key.clone()),
        "managed panic-after-spawn root injection already installed"
    );
    ManagedPanicAfterSpawnRootGuard(key)
}

#[cfg(test)]
impl Drop for ManagedPanicAfterSpawnGuard {
    fn drop(&mut self) {
        let mut configured = MANAGED_PANIC_AFTER_SPAWN_RUN_DIRS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        configured.remove(&self.0);
    }
}

#[cfg(test)]
fn inject_managed_panic_after_spawn(run_dir: &std::path::Path) -> ManagedPanicAfterSpawnGuard {
    let run_dir = run_dir.to_path_buf();
    let mut configured = MANAGED_PANIC_AFTER_SPAWN_RUN_DIRS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(configured.insert(run_dir.clone()));
    ManagedPanicAfterSpawnGuard(run_dir)
}

#[cfg(test)]
fn panic_after_managed_spawn(run_dir: &std::path::Path) {
    let configured = MANAGED_PANIC_AFTER_SPAWN_RUN_DIRS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if configured.contains(run_dir) {
        // The fixture writes this marker only after its descendant is live.
        // This is test-only and proves the guard covers an unwind after
        // ownership, rather than merely a root that never ran.
        let ready = run_dir.join("panic-after-spawn.ready");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "panic-after-spawn fixture was not ready");
        panic!("injected managed panic after spawn");
    }
    let Some(home) = std::env::var_os("TACHI_HOME") else {
        return;
    };
    let Some(run_root) = std::env::var_os("TACHI_RUN_ROOT") else {
        return;
    };
    if MANAGED_PANIC_AFTER_SPAWN_ROOTS.get().is_some_and(|roots| {
        roots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&(
                std::path::PathBuf::from(home),
                std::path::PathBuf::from(run_root),
            ))
    }) {
        panic!("injected managed panic after spawn for isolated run root");
    }
}

#[cfg(all(test, unix))]
mod issue_1825_tests {
    use super::*;
    use serde_json::{json, Value};

    fn write_managed_status(run_dir: &std::path::Path, dispatch_id: &str) {
        std::fs::create_dir_all(run_dir).expect("run directory");
        std::fs::write(
            run_dir.join("status.json"),
            json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_WORKING",
                "status_revision": 1,
                "execution_classification": "managed_custom",
                "lifecycle_owner": "memory_server_managed_custom",
            })
            .to_string(),
        )
        .expect("managed status");
    }

    async fn wait_for_file(path: &std::path::Path) {
        for _ in 0..120 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("worker did not reach {}", path.display());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn natural_child_exit_after_cancel_dequeue_keeps_terminal_truthful() {
        let _serial = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("home");
        let runs = tempfile::tempdir().expect("runs");
        let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
        let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
        let dispatch_id = "20260823T182514Z-custom-deadbeef";
        let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
        write_managed_status(&run_dir, dispatch_id);
        let release = run_dir.join("release");
        let exited = run_dir.join("exited");
        let server =
            crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
        let (receiver, run_guard) = server
            .managed_run_controls
            .register(dispatch_id)
            .expect("registry");
        let (_dequeue_guard, entered, continue_cancel, _observation) =
            install_managed_cancel_dequeue_barrier(&run_dir);
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!(
                "while [ ! -f '{}' ]; do :; done; touch '{}'",
                release.display(),
                exited.display()
            ),
        ]);
        let runner_dir = run_dir.clone();
        let runner = tokio::spawn(async move {
            run_managed_custom_subprocess(command, Duration::from_secs(20), receiver, &runner_dir)
                .await
        });
        let cancel_server = server.clone();
        let cancel = tokio::spawn(async move {
            crate::managed_run_control::request_managed_custom_cancel(
                &cancel_server,
                dispatch_id,
                1,
            )
            .await
            .expect("cancel")
        });
        tokio::task::spawn_blocking(move || entered.recv().expect("dequeued"))
            .await
            .expect("barrier join");
        std::fs::write(&release, b"release").expect("release child");
        wait_for_file(&exited).await;
        crate::dispatch_ops::write_status_json(
            &run_dir,
            dispatch_id,
            false,
            None,
            None,
            "n/a",
            Some(1),
            None,
            None,
            None,
            Some(
                serde_json::json!({"state":"TASK_STATE_COMPLETED", "result_written":true, "result":"natural exit"}),
            ),
        );
        continue_cancel.send(()).expect("continue cancel");
        assert!(runner.await.expect("runner").is_ok());
        let response: Value =
            serde_json::from_str(&cancel.await.expect("cancel task")).expect("response");
        let status: Value =
            serde_json::from_slice(&std::fs::read(run_dir.join("status.json")).expect("status"))
                .expect("JSON");
        assert_eq!(status["state"], "TASK_STATE_COMPLETED");
        assert_ne!(status["state"], "TASK_STATE_CANCELED");
        assert_eq!(response["receipt"], status["cancellation"]["receipt"]);
        assert_eq!(response["reason"], status["cancellation"]["reason"]);
        drop(run_guard);
        assert!(!server.managed_run_controls.contains(dispatch_id));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn issue_1825_request_cancel_reconciles_injected_child_probe_failure_through_the_real_runner(
    ) {
        let _serial = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("home");
        let runs = tempfile::tempdir().expect("runs");
        let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", home.path());
        let _runs = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", runs.path());
        let dispatch_id = "20260823T182500Z-custom-deadbeef";
        let run_dir = crate::dispatch_ops::dispatch_runs_root().join(dispatch_id);
        write_managed_status(&run_dir, dispatch_id);
        let server =
            crate::MemoryServer::new(home.path().join("server.sqlite"), None).expect("server");
        let (receiver, run_guard) = server
            .managed_run_controls
            .register(dispatch_id)
            .expect("managed control registration");
        let started = run_dir.join("started");
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &format!("touch '{}' && sleep 30", started.display())]);
        let runner_run_dir = run_dir.clone();
        let (_pid_guard, pid_observer) = install_managed_cancel_child_pid_observer(&run_dir);
        let runner = tokio::spawn(async move {
            run_managed_custom_subprocess(
                command,
                Duration::from_secs(60),
                receiver,
                &runner_run_dir,
            )
            .await
        });
        wait_for_file(&started).await;
        let pid = pid_observer.recv().expect("managed child pid");
        let _failure = inject_managed_cancel_probe_failure(&run_dir);
        let response =
            crate::managed_run_control::request_managed_custom_cancel(&server, dispatch_id, 1)
                .await
                .expect("cancellation response");
        let runner_error = match runner.await.expect("runner task") {
            Ok(_) => panic!(
                "injected child probe failure must leave the production runner loudly failed"
            ),
            Err(error) => error,
        };
        assert!(runner_error.contains("managed cancellation child probe failed"));
        let response: Value = serde_json::from_str(&response).expect("cancellation JSON");
        assert_eq!(response["receipt"], "termination_unconfirmed");
        let status: Value = serde_json::from_slice(
            &std::fs::read(run_dir.join("status.json")).expect("canonical status"),
        )
        .expect("canonical JSON");
        assert_eq!(status["state"], "TASK_STATE_FAILED");
        assert_eq!(status["status_revision"], 3);
        assert_eq!(status["cancellation"]["receipt"], "termination_unconfirmed");
        assert_ne!(status["state"], "TASK_STATE_CANCELED");
        drop(run_guard);
        assert!(
            !server.managed_run_controls.contains(dispatch_id),
            "failed termination proof must release the managed cancellation registry"
        );
        assert!(
            process_group_absent(Some(pid)),
            "probe failure must reap the production child process group"
        );
    }
}

#[cfg(all(test, not(unix)))]
mod issue_1825_non_unix_tests {
    use super::*;

    #[tokio::test]
    async fn managed_custom_wrapper_runs_the_ordinary_command_when_cancel_is_unavailable() {
        let (_sender, receiver) = mpsc::channel(1);
        let mut command = Command::new("cmd");
        command.args(["/C", "exit 0"]);
        let result = run_managed_custom_subprocess(
            command,
            Duration::from_secs(5),
            receiver,
            std::path::Path::new("."),
        )
        .await
        .expect("non-Unix managed wrapper must preserve ordinary custom execution");
        assert_eq!(result.exit_code, Some(0));
    }
}

pub(super) async fn run_opencode_sop_subprocess(
    mut cmd: Command,
    timeout: Duration,
    sop_label: &str,
) -> Result<DispatchResult, String> {
    let _permit = opencode_sop_semaphore()
        .acquire()
        .await
        .map_err(|e| format!("opencode SOP subprocess limiter closed: {e}"))?;
    run_agent_subprocess_inner(&mut cmd, timeout, Some(sop_label)).await
}

async fn run_agent_subprocess_inner(
    cmd: &mut Command,
    timeout: Duration,
    opencode_sop_label: Option<&str>,
) -> Result<DispatchResult, String> {
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // If this task is dropped (daemon shutdown, spawning task cancelled, ...),
    // tokio kills the child via SIGKILL instead of leaving it orphaned.
    cmd.kill_on_drop(true);
    configure_process_group(cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn agent process: {e}"))?;
    let child_pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = tokio::spawn(read_pipe(stdout));
    let stderr_task = tokio::spawn(read_pipe(stderr));

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(res) => {
            let status = res.map_err(|e| format!("Agent process error: {e}"))?;
            terminate_process_group(child_pid, libc::SIGTERM);
            status
        }
        Err(_) => {
            reap_timed_out_child(&mut child, child_pid).await;
            if let Some(sop_label) = opencode_sop_label {
                tracing::warn!(
                    sop = sop_label,
                    elapsed_secs = timeout.as_secs(),
                    "daemon opencode SOP subprocess timed out; child process group killed"
                );
            }
            return Err(format!(
                "Agent process timed out after {}s (process group killed)",
                timeout.as_secs()
            ));
        }
    };

    let exit_code = status.code();
    let stdout = collect_pipe(stdout_task).await?;
    let stderr = collect_pipe(stderr_task).await?;

    let output_text = if stdout.is_empty() && !stderr.is_empty() {
        stderr
    } else if !stderr.is_empty() {
        format!("{}\n\n--- stderr ---\n{}", stdout, stderr)
    } else {
        stdout
    };

    Ok(DispatchResult {
        output: output_text,
        exit_code,
        observed_model: None,
    })
}

async fn read_pipe(pipe: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes).await;
    bytes
}

async fn collect_pipe(task: tokio::task::JoinHandle<Vec<u8>>) -> Result<String, String> {
    let bytes = task
        .await
        .map_err(|e| format!("Agent output reader task failed: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

async fn reap_timed_out_child(child: &mut tokio::process::Child, child_pid: Option<u32>) -> bool {
    terminate_process_group(child_pid, libc::SIGTERM);
    let child_exited = match tokio::time::timeout(PROCESS_GROUP_TERM_GRACE, child.wait()).await {
        Ok(Ok(_)) => true,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "failed while waiting for timed-out subprocess");
            false
        }
        Err(_) => false,
    };
    terminate_process_group(child_pid, libc::SIGKILL);
    if !child_exited {
        if let Err(err) = child.kill().await {
            tracing::warn!(error = %err, "failed to kill timed-out subprocess");
        }
        let _ = child.wait().await;
    }
    child_exited
}

fn opencode_sop_semaphore() -> &'static Semaphore {
    OPENCODE_SOP_SEMAPHORE.get_or_init(|| {
        Semaphore::new(env_usize(
            "TACHI_OPENCODE_SOP_MAX_CONCURRENCY",
            DEFAULT_OPENCODE_SOP_MAX_CONCURRENCY,
            MAX_OPENCODE_SOP_CONCURRENCY,
        ))
    })
}

fn env_usize(name: &str, default: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(1, max)
}

#[cfg(unix)]
fn configure_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum ProcessGroupSignal {
    Delivered,
    Absent,
    Failed,
    RootReaped,
}

#[cfg(all(test, unix))]
impl ProcessGroupSignal {
    fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Absent => "absent",
            Self::Failed => "failed",
            Self::RootReaped => "root_reaped",
        }
    }
}

#[cfg(unix)]
fn signal_process_group(child_pid: Option<u32>, signal: libc::c_int) -> ProcessGroupSignal {
    let Some(pid) = child_pid else {
        return ProcessGroupSignal::Absent;
    };
    let pgid = -(pid as libc::pid_t);
    // SAFETY: `kill(pgid, signal)` sends a signal to an OS process group; it
    // passes no pointers across the FFI boundary and aliases no Rust memory.
    // `pgid` is a negative pid_t derived from the child pid; an invalid group
    // yields ESRCH and is tolerated below.
    let rc = unsafe { libc::kill(pgid, signal) };
    if rc == 0 {
        return ProcessGroupSignal::Delivered;
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return ProcessGroupSignal::Absent;
    }
    if err.kind() != std::io::ErrorKind::NotFound {
        tracing::debug!(pid, signal, error = %err, "process group signal failed");
    }
    ProcessGroupSignal::Failed
}

#[cfg(unix)]
fn terminate_process_group(child_pid: Option<u32>, signal: libc::c_int) {
    let _ = signal_process_group(child_pid, signal);
}

#[cfg(not(unix))]
fn terminate_process_group(_child_pid: Option<u32>, _signal: libc::c_int) {}

pub(super) fn tail_chars(text: &str, max_chars: usize) -> String {
    tachi_dispatch::tail_chars(text, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn run_agent_subprocess_closes_child_stdin() {
        let cmd = Command::new("/bin/cat");

        let result = run_agent_subprocess(cmd, Duration::from_secs(5))
            .await
            .expect("stdin reader should observe EOF and exit");

        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.output, "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_agent_subprocess_timeout_reaps_child_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script_path = temp.path().join("hung-opencode");
        let child_pid_path = temp.path().join("child.pid");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nsh -c 'trap \"\" TERM; sleep 60' &\necho $! > '{}'\nwait\n",
                child_pid_path.display()
            ),
        )
        .expect("write fake opencode");
        let mut perms = std::fs::metadata(&script_path)
            .expect("metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod");
        let mut cmd = Command::new(&script_path);
        let child_pid_path_for_wait = child_pid_path.clone();
        cmd.env(
            "PATH",
            std::env::var_os("PATH").expect("PATH must exist for shell fixture"),
        );

        // Root cause (issue #997, confirmed via tracing): the simulated
        // "agent hung" timeout passed to `run_agent_subprocess` shared the
        // same budget as the fixture's own startup latency (fork+exec `sh`,
        // background a sub-shell, `echo $! > file`). With a 1s simulated
        // timeout, `run_agent_subprocess_inner`'s internal `tokio::time::timeout`
        // occasionally fired and SIGTERM'd the fixture's process group BEFORE
        // the fixture reached its `echo $! > file` line under scheduler
        // latency (observed even with zero build contention, an isolated
        // target dir, and a quiet machine) — once the process group is
        // killed, the pid file can never be written, so no `wait_for_file`
        // deadline, however large, fixes this: the resource being polled for
        // genuinely never gets created. Widening the simulated timeout to 4s
        // gives the fixture's startup line real headroom to run before the
        // timeout fires, decoupling "time for the fixture to announce itself"
        // from "time until the simulated hang is treated as timed out" —
        // this still exercises a real reap (the fixture's `sleep 60` still
        // vastly outlasts 4s) without weakening what the test proves.
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            let run = tokio::spawn(run_agent_subprocess(cmd, Duration::from_secs(4)));
            wait_for_file(&child_pid_path_for_wait).await;
            run.await.expect("subprocess runner task should not panic")
        })
        .await
        .expect("runner must return instead of hanging forever");

        let Err(error) = result else {
            panic!("hung fake opencode should time out");
        };
        assert!(
            error.contains("timed out"),
            "timeout should surface as subprocess error: {error}"
        );
        let child_pid: libc::pid_t = std::fs::read_to_string(&child_pid_path)
            .expect("child pid")
            .trim()
            .parse()
            .expect("pid integer");
        assert!(
            wait_for_process_exit(child_pid).await,
            "hung child process {child_pid} should be reaped"
        );
    }

    #[cfg(unix)]
    async fn wait_for_file(path: &std::path::Path) {
        // 10s: a poll-with-deadline safety net on fork+exec/scheduler latency
        // for a trivial shell fixture (not a fixed sleep, and not a
        // functional wait — it normally resolves in well under a second).
        // The real fix for issue #997's "fixture did not write child pid
        // file" flake is the widened simulated-timeout in the caller above
        // (this deadline being too short was never the root cause: tracing
        // showed the fixture's process group was killed by the *simulated*
        // 1s timeout before it could reach `echo $! > file`, so the file
        // could never appear no matter how long this polled).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fixture did not write child pid file: {}", path.display());
    }

    #[cfg(unix)]
    async fn wait_for_process_exit(pid: libc::pid_t) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            // SAFETY: `kill(pid, 0)` performs a signal-0 existence probe — it
            // sends no signal, passes no pointers across the FFI boundary, and
            // aliases no Rust memory.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staff_cancel_managed_custom_process_confirms_group_termination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("managed-custom");
        let descendant = temp.path().join("descendant.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntrap '' TERM\nsh -c 'trap \"\" TERM; sleep 60' &\necho $! > '{}'\nwait\n",
                descendant.display()
            ),
        )
        .expect("write fixture");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": "20260823T010100Z-custom-deadbeef",
                "state": "TASK_STATE_WORKING",
                "status_revision": 1,
                "execution_classification": "managed_custom",
                "cancellation": {"receipt": "cancellation_requested"},
            })
            .to_string(),
        )
        .expect("status");

        let (sender, receiver) = mpsc::channel(1);
        let (reply, confirmed) = tokio::sync::oneshot::channel();
        let command = Command::new(&script);
        let run_dir_for_task = run_dir.clone();
        let run = tokio::spawn(async move {
            run_managed_custom_subprocess(
                command,
                Duration::from_secs(20),
                receiver,
                &run_dir_for_task,
            )
            .await
        });
        wait_for_file(&descendant).await;
        sender
            .send(crate::managed_run_control::ManagedCancelCommand {
                expected_status_revision: 1,
                response: reply,
                test_observation: None,
            })
            .await
            .expect("private cancellation channel is open");
        let result = run.await.expect("runner task does not panic");
        match result {
            Err(error) => assert_eq!(error, "managed_cancelled"),
            Ok(_) => panic!("cancellation must interrupt the managed root"),
        }
        assert!(matches!(
            confirmed.await.expect("cancellation outcome"),
            crate::managed_run_control::CancelCompletion::Confirmed {
                termination_proof: "unix_process_group_absent",
                ..
            }
        ));
        let pid: libc::pid_t = std::fs::read_to_string(&descendant)
            .expect("descendant pid")
            .trim()
            .parse()
            .expect("numeric descendant pid");
        assert!(
            wait_for_process_exit(pid).await,
            "descendant must be absent before cancellation confirmation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_custom_probe_failure_reaps_group_and_reports_unavailable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("managed-custom");
        let root = temp.path().join("root.pid");
        let descendant = temp.path().join("descendant.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntrap '' TERM\nprintf '%s\\n' \"$$\" > '{}'\nsh -c 'trap \"\" TERM; sleep 60' &\nprintf '%s\\n' \"$!\" > '{}'\nwait\n",
                root.display(),
                descendant.display()
            ),
        )
        .expect("write fixture");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": "20260823T010102Z-custom-deadbeef",
                "state": "TASK_STATE_WORKING",
                "status_revision": 1,
                "execution_classification": "managed_custom",
                "cancellation": {"receipt": "cancellation_requested"},
            })
            .to_string(),
        )
        .expect("status");

        let _probe_failure = inject_managed_cancel_probe_failure(&run_dir);
        let (sender, receiver) = mpsc::channel(1);
        let (reply, outcome) = tokio::sync::oneshot::channel();
        let command = Command::new(&script);
        let run_dir_for_task = run_dir.clone();
        let run = tokio::spawn(async move {
            run_managed_custom_subprocess(
                command,
                Duration::from_secs(20),
                receiver,
                &run_dir_for_task,
            )
            .await
        });
        wait_for_file(&root).await;
        wait_for_file(&descendant).await;
        sender
            .send(crate::managed_run_control::ManagedCancelCommand {
                expected_status_revision: 1,
                response: reply,
                test_observation: None,
            })
            .await
            .expect("private cancellation channel is open");

        let result = run.await.expect("runner task does not panic");
        let descendant_pid: libc::pid_t = std::fs::read_to_string(&descendant)
            .expect("descendant pid")
            .trim()
            .parse()
            .expect("numeric descendant pid");
        let descendant_reaped = wait_for_process_exit(descendant_pid).await;
        if !descendant_reaped {
            let root_pid: u32 = std::fs::read_to_string(&root)
                .expect("root pid")
                .trim()
                .parse()
                .expect("numeric root pid");
            terminate_process_group(Some(root_pid), libc::SIGKILL);
        }
        assert!(
            descendant_reaped,
            "probe failure must still reap the managed descendant process group"
        );
        assert!(matches!(
            result,
            Err(ref error) if error.contains("managed cancellation child probe failed")
        ));
        assert!(matches!(
            outcome.await.expect("explicit cancellation outcome"),
            crate::managed_run_control::CancelCompletion::Unconfirmed
        ));
        let status: serde_json::Value = serde_json::from_slice(
            &std::fs::read(run_dir.join("status.json")).expect("canonical status"),
        )
        .expect("canonical JSON");
        assert_eq!(status["state"], "TASK_STATE_FAILED");
        assert_eq!(status["cancellation"]["receipt"], "termination_unconfirmed");
    }
}

#[cfg(test)]
mod issue_1825_cancel_tests {
    use super::*;

    #[cfg(unix)]
    struct ManagedProcessGroupCleanup {
        root_pid: std::path::PathBuf,
        armed: bool,
    }

    #[cfg(unix)]
    impl ManagedProcessGroupCleanup {
        fn arm(root_pid: &std::path::Path) -> Self {
            Self {
                root_pid: root_pid.to_path_buf(),
                armed: true,
            }
        }

        fn disarm(&mut self) {
            self.armed = false;
        }
    }

    #[cfg(unix)]
    impl Drop for ManagedProcessGroupCleanup {
        fn drop(&mut self) {
            if !self.armed {
                return;
            }
            let Ok(pid) = std::fs::read_to_string(&self.root_pid)
                .ok()
                .as_deref()
                .unwrap_or_default()
                .trim()
                .parse::<libc::pid_t>()
            else {
                return;
            };
            // SAFETY: the fixture records its own process-group leader; the
            // negative pid cannot target an unrelated process outside it.
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }

    #[cfg(unix)]
    async fn wait_for_timeout_fixture_file(path: &std::path::Path) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timeout fixture did not write {}", path.display());
    }

    #[cfg(unix)]
    async fn timeout_fixture_process_is_absent(pid: libc::pid_t) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            // SAFETY: signal 0 is an existence probe and does not alter the
            // fixture process or pass memory across the FFI boundary.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_custom_cancel_does_not_kill_unrelated_process() {
        use std::os::unix::fs::PermissionsExt;

        let mut unrelated = Command::new("/bin/sh");
        unrelated.arg("-c").arg("sleep 30");
        let mut unrelated = unrelated.spawn().expect("start unrelated fixture");
        let unrelated_pid = unrelated.id().expect("unrelated pid");

        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("managed-live-cancel.sh");
        let root = temp.path().join("managed-root.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{}'\ntrap '' TERM\nsleep 60\n",
                root.display(),
            ),
        )
        .expect("write managed fixture");
        let mut permissions = std::fs::metadata(&script)
            .expect("managed fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("managed fixture chmod");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": "20260823T010101Z-custom-deadbeef",
                "state": "TASK_STATE_WORKING",
                "status_revision": 1,
                "execution_classification": "managed_custom",
                "cancellation": {"receipt": "cancellation_requested"},
            })
            .to_string(),
        )
        .expect("status");
        let (sender, receiver) = mpsc::channel(1);
        let (reply, outcome) = tokio::sync::oneshot::channel();
        let run_dir_for_task = run_dir.clone();
        let run = tokio::spawn(async move {
            run_managed_custom_subprocess(
                Command::new(script),
                Duration::from_secs(20),
                receiver,
                &run_dir_for_task,
            )
            .await
        });
        wait_for_timeout_fixture_file(&root).await;
        sender
            .send(crate::managed_run_control::ManagedCancelCommand {
                expected_status_revision: 1,
                response: reply,
                test_observation: None,
            })
            .await
            .expect("queue live cancellation");
        let result = run.await.expect("runner task does not panic");
        match result {
            Err(error) => assert_eq!(error, "managed_cancelled"),
            Ok(_) => panic!("pre-spawn cancellation interrupts run"),
        }
        assert!(matches!(
            outcome.await.expect("outcome"),
            crate::managed_run_control::CancelCompletion::Confirmed {
                termination_proof: "unix_process_group_absent",
                ..
            }
        ));
        let managed_pid: libc::pid_t = std::fs::read_to_string(&root)
            .expect("managed root pid")
            .trim()
            .parse()
            .expect("numeric managed root pid");
        assert!(
            timeout_fixture_process_is_absent(managed_pid).await,
            "the live managed process group must be absent"
        );
        let alive = unsafe { libc::kill(unrelated_pid as libc::pid_t, 0) } == 0;
        let _ = unrelated.kill().await;
        let _ = unrelated.wait().await;
        assert!(
            alive,
            "managed cancellation must not target an unrelated PID"
        );
    }

    /// Timeout wins in the real managed runner before a later cancellation
    /// request. The production terminal writer must retain that winner while
    /// the registry cleanup makes cancellation explicitly unavailable.
    #[cfg(unix)]
    #[tokio::test]
    async fn managed_custom_timeout_wins_over_late_cancellation_without_receipt_mutation() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("managed-custom");
        let root = temp.path().join("root.pid");
        let descendant = temp.path().join("descendant.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ntrap '' TERM\nprintf '%s\\n' \"$$\" > '{}'\nsh -c 'trap \"\" TERM; sleep 60' &\nprintf '%s\\n' \"$!\" > '{}'\nwait\n",
                root.display(),
                descendant.display(),
            ),
        )
        .expect("write fixture");
        let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod");
        let dispatch_id = "20260823T010106Z-custom-timeout-winner";
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(
            run_dir.join("status.json"),
            serde_json::json!({
                "dispatch_id": dispatch_id,
                "state": "TASK_STATE_WORKING",
                "status_revision": 1,
                "execution_classification": "managed_custom",
            })
            .to_string(),
        )
        .expect("working status");
        let server = crate::MemoryServer::new(temp.path().join("server.sqlite"), None)
            .expect("managed control server");
        let (receiver, run_guard) = server
            .managed_run_controls
            .register(dispatch_id)
            .expect("managed control registry");
        let mut cleanup_guard = ManagedProcessGroupCleanup::arm(&root);
        let command = Command::new(&script);
        let run_dir_for_task = run_dir.clone();
        let run = tokio::spawn(async move {
            run_managed_custom_subprocess(
                command,
                Duration::from_secs(4),
                receiver,
                &run_dir_for_task,
            )
            .await
        });
        wait_for_timeout_fixture_file(&root).await;
        wait_for_timeout_fixture_file(&descendant).await;
        let result = run.await.expect("runner task does not panic");
        assert!(matches!(result, Err(ref error) if error.contains("timed out")));

        let root_pid: libc::pid_t = std::fs::read_to_string(&root)
            .expect("root pid")
            .trim()
            .parse()
            .expect("numeric root pid");
        let descendant_pid: libc::pid_t = std::fs::read_to_string(&descendant)
            .expect("descendant pid")
            .trim()
            .parse()
            .expect("numeric descendant pid");
        assert!(
            timeout_fixture_process_is_absent(root_pid).await,
            "timed-out root process group must be absent"
        );
        assert!(
            timeout_fixture_process_is_absent(descendant_pid).await,
            "timed-out descendant process group must be absent"
        );

        let terminal_result = "managed custom worker timed out";
        std::fs::write(run_dir.join("result.md"), terminal_result).expect("terminal result");
        crate::dispatch_ops::write_status_json(
            &run_dir,
            dispatch_id,
            false,
            None,
            None,
            "n/a",
            Some(1),
            None,
            None,
            None,
            Some(serde_json::json!({
                "state": "TASK_STATE_FAILED",
                "result_written": true,
                "result": terminal_result,
            })),
        );
        let terminal: serde_json::Value = serde_json::from_slice(
            &std::fs::read(run_dir.join("status.json")).expect("terminal status"),
        )
        .expect("terminal status JSON");
        assert_eq!(terminal["state"], "TASK_STATE_FAILED");
        let terminal_revision = terminal["status_revision"]
            .as_u64()
            .expect("terminal revision");
        drop(run_guard);
        assert!(
            !server.managed_run_controls.contains(dispatch_id),
            "terminal cleanup must remove managed cancellation authority"
        );

        let cancellation: serde_json::Value = serde_json::from_str(
            &crate::managed_run_control::request_managed_custom_cancel(
                &server,
                dispatch_id,
                terminal_revision,
            )
            .await
            .expect("late cancellation response"),
        )
        .expect("late cancellation JSON");
        assert_eq!(cancellation["receipt"], "cancellation_unavailable");
        let after: serde_json::Value = serde_json::from_slice(
            &std::fs::read(run_dir.join("status.json")).expect("post-cancel status"),
        )
        .expect("post-cancel status JSON");
        assert_eq!(
            after, terminal,
            "late cancel must not replace timeout winner"
        );
        assert_eq!(after["status_revision"], terminal_revision);
        assert_eq!(
            std::fs::read_to_string(run_dir.join("result.md")).expect("post-cancel result"),
            terminal_result,
            "late cancel must not replace result.md"
        );
        cleanup_guard.disarm();
    }
}

#[cfg(all(test, unix))]
mod managed_process_group_regression_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    async fn wait_for_file(path: &std::path::Path) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fixture did not write {}", path.display());
    }

    async fn process_absent(pid: libc::pid_t) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            // SAFETY: signal 0 is an existence probe for this fixture PID.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn descendant_fixture(temp: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
        let script = temp.path().join("root-exits-descendant-live.sh");
        let descendant = temp.path().join("descendant.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nsh -c 'sleep 60' &\nprintf '%s\\n' \"$!\" > '{}'\nexit 0\n",
                descendant.display(),
            ),
        )
        .expect("write fixture");
        let mut permissions = std::fs::metadata(&script)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("fixture chmod");
        (script, descendant)
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn managed_natural_root_exit_reaps_live_descendant_before_return() {
        let _serial = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run directory");
        let (script, descendant) = descendant_fixture(&temp);
        let (_sender, receiver) = mpsc::channel(1);

        let outcome = run_managed_custom_subprocess(
            Command::new(script),
            Duration::from_secs(5),
            receiver,
            &run_dir,
        )
        .await;
        assert_eq!(outcome.expect("root exit").exit_code, Some(0));
        wait_for_file(&descendant).await;
        let pid = std::fs::read_to_string(descendant)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");
        assert!(
            process_absent(pid).await,
            "natural root exit left descendant alive"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn managed_panic_after_spawn_kills_owned_process_group() {
        let _serial = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run directory");
        let (script, descendant) = descendant_fixture(&temp);
        let root = temp.path().join("root.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{}'\nsh -c 'sleep 60' &\nprintf '%s\\n' \"$!\" > '{}'\ntouch '{}'\nwait\n",
                root.display(),
                descendant.display(),
                run_dir.join("panic-after-spawn.ready").display(),
            ),
        )
        .expect("write panic fixture");
        let _panic = inject_managed_panic_after_spawn(&run_dir);
        let (_sender, receiver) = mpsc::channel(1);
        let run_dir_for_task = run_dir.clone();
        let task = tokio::spawn(async move {
            run_managed_custom_subprocess(
                Command::new(script),
                Duration::from_secs(5),
                receiver,
                &run_dir_for_task,
            )
            .await
        });
        assert!(matches!(task.await, Err(error) if error.is_panic()));
        wait_for_file(&descendant).await;
        let root_pid = std::fs::read_to_string(root)
            .expect("root pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric root pid");
        let pid = std::fs::read_to_string(descendant)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");
        assert!(
            process_group_absent(Some(root_pid as u32)),
            "panic JoinError must be delayed until the owned process group is absent"
        );
        assert!(
            process_absent(pid).await,
            "panic unwind left descendant alive"
        );
        let mut status = 0;
        let waited = unsafe { libc::waitpid(root_pid, &mut status, libc::WNOHANG) };
        assert_eq!(waited, -1, "panic guard must reap its owned root");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD),
            "panic unwind must not leave a root zombie"
        );
    }
}
