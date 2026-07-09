mod daemon_cli;
mod foundry;
mod status_render;
mod watcher;

pub(crate) use daemon_cli::run_daemon;
pub(crate) use foundry::run_foundry;
// Test-only re-export (`status_ops/tests` uses `status_cli::is_checkpoint_fixture_path`).
#[cfg(test)]
pub(crate) use foundry::is_checkpoint_fixture_path;
pub(crate) use status_render::run_status;
pub(crate) use watcher::run_watcher;
