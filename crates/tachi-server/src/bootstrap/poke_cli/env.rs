use std::path::Path;
#[cfg(not(test))]
use std::sync::OnceLock;

pub(super) struct PokeEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    original_tachi_home: Option<std::ffi::OsString>,
    original_tachi_run_root: Option<std::ffi::OsString>,
    original_search_disable_query_embedding: Option<std::ffi::OsString>,
    /// `Some(original HOME)` while a test-only HOME override is applied.
    #[cfg(test)]
    replaced_home: Option<Option<std::ffi::OsString>>,
}

// Test-only HOME seam (tachi#1978). The skill_surface probe discovers host
// skills under `$HOME/.agents/skills` / `$HOME/.codex/skills` by design, so a
// test that leaves HOME alone passes or fails on the developer's home
// contents. `HOME` must be swapped while holding the same global test lock this
// guard takes, which the caller cannot hold across `run_poke_smoke_suite`
// (the lock is not reentrant); the override is therefore handed in here and
// applied inside the lock. Thread-local: `#[tokio::test]` runs the suite on
// the test's own thread, and no other test can observe it. Production builds
// never touch HOME.
#[cfg(test)]
thread_local! {
    static TEST_HOME_OVERRIDE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Make every `PokeEnvGuard` created on this thread point `HOME` at `home`
/// (restored on drop), or stop doing so with `None`.
#[cfg(test)]
pub(super) fn set_test_home_override(home: Option<std::path::PathBuf>) {
    TEST_HOME_OVERRIDE.with(|cell| *cell.borrow_mut() = home);
}

impl PokeEnvGuard {
    pub(super) fn new(tachi_home: &Path, run_root: &Path) -> Self {
        let lock = poke_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original_tachi_home = std::env::var_os("TACHI_HOME");
        let original_tachi_run_root = std::env::var_os("TACHI_RUN_ROOT");
        let original_search_disable_query_embedding =
            std::env::var_os("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING");
        std::env::set_var("TACHI_HOME", tachi_home);
        std::env::set_var("TACHI_RUN_ROOT", run_root);
        std::env::set_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
        #[cfg(test)]
        let replaced_home = TEST_HOME_OVERRIDE
            .with(|cell| cell.borrow().clone())
            .map(|home| {
                let original = std::env::var_os("HOME");
                std::env::set_var("HOME", home);
                original
            });
        Self {
            _lock: lock,
            original_tachi_home,
            original_tachi_run_root,
            original_search_disable_query_embedding,
            #[cfg(test)]
            replaced_home,
        }
    }
}

fn poke_env_lock() -> &'static std::sync::Mutex<()> {
    #[cfg(test)]
    {
        crate::utils::global_test_lock()
    }
    #[cfg(not(test))]
    {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }
}

impl Drop for PokeEnvGuard {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(original) = self.replaced_home.take() {
            match original {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        if let Some(value) = self.original_tachi_home.as_ref() {
            std::env::set_var("TACHI_HOME", value);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
        if let Some(value) = self.original_tachi_run_root.as_ref() {
            std::env::set_var("TACHI_RUN_ROOT", value);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
        if let Some(value) = self.original_search_disable_query_embedding.as_ref() {
            std::env::set_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", value);
        } else {
            std::env::remove_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING");
        }
    }
}
