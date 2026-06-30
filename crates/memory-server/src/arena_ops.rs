//! `tachi_arena` - tracked worker-mission document ledger.
//!
//! Arena deliberately separates run documents from memory. Prompt, plan,
//! result, status, stdout, and stderr live under `.tachi/arena/<arena_id>/`;
//! Tachi memory/wiki should only receive distilled conclusions after close.

mod actions;
mod dispatch_bridge;
mod lane;
mod render;
mod state;

pub(crate) use actions::handle_tachi_arena;

pub(crate) fn tachi_arena_root_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests;
