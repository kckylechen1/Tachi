use std::path::Path;
#[cfg(not(test))]
use std::sync::OnceLock;

pub(super) struct PokeEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    _arena_lock: std::sync::MutexGuard<'static, ()>,
    original_tachi_home: Option<std::ffi::OsString>,
    original_tachi_run_root: Option<std::ffi::OsString>,
    original_tachi_arena_root: Option<std::ffi::OsString>,
    original_search_disable_query_embedding: Option<std::ffi::OsString>,
}

impl PokeEnvGuard {
    pub(super) fn new(tachi_home: &Path, run_root: &Path) -> Self {
        let lock = poke_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let arena_lock = crate::arena_ops::tachi_arena_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original_tachi_home = std::env::var_os("TACHI_HOME");
        let original_tachi_run_root = std::env::var_os("TACHI_RUN_ROOT");
        let original_tachi_arena_root = std::env::var_os("TACHI_ARENA_ROOT");
        let original_search_disable_query_embedding =
            std::env::var_os("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING");
        std::env::set_var("TACHI_HOME", tachi_home);
        std::env::set_var("TACHI_RUN_ROOT", run_root);
        std::env::set_var("TACHI_ARENA_ROOT", tachi_home.join("arena"));
        std::env::set_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
        Self {
            _lock: lock,
            _arena_lock: arena_lock,
            original_tachi_home,
            original_tachi_run_root,
            original_tachi_arena_root,
            original_search_disable_query_embedding,
        }
    }
}

fn poke_env_lock() -> &'static std::sync::Mutex<()> {
    #[cfg(test)]
    {
        crate::shell_ops::tachi_run_root_env_lock()
    }
    #[cfg(not(test))]
    {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }
}

impl Drop for PokeEnvGuard {
    fn drop(&mut self) {
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
        if let Some(value) = self.original_tachi_arena_root.as_ref() {
            std::env::set_var("TACHI_ARENA_ROOT", value);
        } else {
            std::env::remove_var("TACHI_ARENA_ROOT");
        }
        if let Some(value) = self.original_search_disable_query_embedding.as_ref() {
            std::env::set_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", value);
        } else {
            std::env::remove_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING");
        }
    }
}
