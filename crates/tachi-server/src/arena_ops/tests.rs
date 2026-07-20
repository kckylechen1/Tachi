use super::state::{validate_arena_id, validate_mission_id};
use super::{handle_tachi_arena, tachi_arena_root_env_lock};
use crate::tool_params::SaveMemoryParams;
use crate::{MemoryServer, TachiArenaParams};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

mod completion_draft;
mod facade;
mod feedback_rules;
mod harness;
mod lifecycle;
mod validation;

struct ArenaRootGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    original: Option<std::ffi::OsString>,
    path: PathBuf,
}

impl Drop for ArenaRootGuard {
    fn drop(&mut self) {
        // SAFETY: arena tests serialize access to TACHI_ARENA_ROOT with
        // tachi_arena_root_env_lock(), so no concurrent env mutation occurs.
        unsafe {
            if let Some(value) = self.original.as_ref() {
                std::env::set_var("TACHI_ARENA_ROOT", value);
            } else {
                std::env::remove_var("TACHI_ARENA_ROOT");
            }
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn temp_arena_root() -> ArenaRootGuard {
    let guard = tachi_arena_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let original = std::env::var_os("TACHI_ARENA_ROOT");
    let path = std::env::temp_dir().join(format!("tachi-arena-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    // SAFETY: protected by tachi_arena_root_env_lock(); see Drop impl.
    unsafe {
        std::env::set_var("TACHI_ARENA_ROOT", &path);
    }
    ArenaRootGuard {
        _guard: guard,
        original,
        path,
    }
}

fn server() -> MemoryServer {
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-arena-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    MemoryServer::new(db_path, None).expect("test server")
}

fn params(action: &str) -> TachiArenaParams {
    TachiArenaParams {
        action: action.to_string(),
        format: None,
        arena_id: None,
        mission_id: None,
        title: None,
        objective: None,
        prompt: None,
        harness: None,
        role: None,
        cwd: None,
        skills: Vec::new(),
        scope: Vec::new(),
        permissions: Vec::new(),
        timeout_secs: None,
        launch: false,
        dispatch_reason: None,
        profile: None,
        model: None,
        project: None,
        flow_id: None,
        issue_ref: None,
        pr_ref: None,
        permission_profile: None,
        sandbox: None,
        credential_profiles: Vec::new(),
        tool_profile: None,
        auto_capability_bundle: None,
        reason: None,
        dry_run: None,
        force: false,
        require_collected: None,
    }
}
