use super::*;

fn runs_env_lock() -> &'static std::sync::Mutex<()> {
    tachi_run_root_env_lock()
}

struct RunsRootGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    path: PathBuf,
}

impl std::ops::Deref for RunsRootGuard {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

fn temp_runs_root() -> RunsRootGuard {
    let guard = runs_env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let d = crate::utils::test_fixture_path(format!(
        "tachi-shell-test-{}",
        Utc::now().format("%Y%m%dT%H%M%S%fZ")
    ));
    std::fs::create_dir_all(&d).unwrap();
    // SAFETY: `set_var` is unsafe on edition 2021 because it can race with
    // other threads reading the same env key. This call is safe because:
    //   1. The `runs_env_lock` mutex is held for the entire lifetime of
    //      `RunsRootGuard`, serialising all `temp_runs_root()` callers.
    //   2. The `Drop` impl restores the original value under the same lock.
    //   3. No other code path mutates `TACHI_RUN_ROOT`.
    unsafe {
        std::env::set_var("TACHI_RUN_ROOT", &d);
    }
    RunsRootGuard {
        _guard: guard,
        path: d,
    }
}

mod flow_lifecycle;
mod instructions;
