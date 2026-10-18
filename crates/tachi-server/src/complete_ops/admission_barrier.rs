struct ManagedCompletionAdmissionBarrier {
    entered: std::sync::mpsc::SyncSender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

pub(super) struct ManagedCompletionAdmissionBarrierGuard(String);

impl Drop for ManagedCompletionAdmissionBarrierGuard {
    fn drop(&mut self) {
        if let Some(barriers) = MANAGED_COMPLETION_ADMISSION_BARRIERS.get() {
            barriers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.0);
        }
    }
}

static MANAGED_COMPLETION_ADMISSION_BARRIERS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, ManagedCompletionAdmissionBarrier>>,
> = std::sync::OnceLock::new();

pub(super) fn install_managed_completion_admission_barrier(
    dispatch_id: &str,
) -> (
    ManagedCompletionAdmissionBarrierGuard,
    std::sync::mpsc::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    assert!(
        MANAGED_COMPLETION_ADMISSION_BARRIERS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                dispatch_id.to_string(),
                ManagedCompletionAdmissionBarrier {
                    entered: entered_tx,
                    release: release_rx,
                },
            )
            .is_none(),
        "managed completion admission barrier already installed"
    );
    (
        ManagedCompletionAdmissionBarrierGuard(dispatch_id.to_string()),
        entered_rx,
        release_tx,
    )
}

pub(super) async fn pause_managed_completion_after_admission(dispatch_id: Option<&str>) {
    let Some(dispatch_id) = dispatch_id else {
        return;
    };
    let barrier = MANAGED_COMPLETION_ADMISSION_BARRIERS
        .get()
        .and_then(|barriers| {
            barriers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(dispatch_id)
        });
    if let Some(barrier) = barrier {
        let _ = barrier.entered.send(());
        let _ = barrier.release.await;
    }
}

#[test]
fn held_admission_allows_background_progress_on_one_runtime_thread() {
    use std::time::Duration;

    let dispatch_id = "completion-barrier-runtime-progress";
    let (_guard, entered, release) = install_managed_completion_admission_barrier(dispatch_id);
    let (start_background, background_started) = tokio::sync::oneshot::channel();
    let (progress, observed_progress) = std::sync::mpsc::sync_channel(1);
    // Release from outside the runtime even on failure: a blocking barrier must
    // fail this assertion rather than hang the test or leave a worker behind.
    let observer = std::thread::spawn(move || {
        entered
            .recv_timeout(Duration::from_secs(8))
            .expect("admission entered");
        start_background
            .send(())
            .expect("start background task after admission");
        let progressed_while_held = observed_progress
            .recv_timeout(Duration::from_secs(8))
            .is_ok();
        release.send(()).expect("release admission");
        progressed_while_held
    });
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("single-thread runtime")
        .block_on(async {
            let completion = tokio::spawn(async move {
                pause_managed_completion_after_admission(Some(dispatch_id)).await;
            });
            let background = tokio::spawn(async move {
                background_started.await.expect("background start signal");
                let _ = progress.send(());
            });
            completion.await.expect("completion join");
            background.await.expect("background join");
        });
    assert!(
        observer.join().expect("observer join"),
        "background task must progress before completion admission is released"
    );
}
