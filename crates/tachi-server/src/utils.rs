mod env;
mod file;
mod locks;
#[cfg(test)]
mod test_fixtures;
mod text;
mod validation;
mod workspace;

#[cfg(test)]
mod tests;

pub(super) use self::env::{parse_env_bool, parse_env_u64};
pub(super) use self::file::{
    append_owner_only_jsonl_line, append_run_event, read_to_string_allow_missing, sync_parent_dir,
    write_json_file_owner_only, write_owner_only_file, write_owner_only_file_atomic,
    write_run_status_file,
};
#[cfg(test)]
pub(crate) use self::locks::global_test_lock;
#[cfg(test)]
pub(crate) use self::test_fixtures::{test_fixture_path, test_fixture_root};
pub(super) use self::locks::{lock_or_recover, read_or_recover, write_or_recover};
pub(crate) use self::text::{compact_text_line, sanitize_safe_path_name};
pub(super) use self::text::{
    redact_sensitive_value, render_skill_prompt_template, stable_hash, value_to_template_text,
};
pub(super) use self::validation::{
    is_shell_env_name, is_trusted_command, is_trusted_mcp_command, normalize_supported_values,
};
// #1120 PR1: `find_git_root_from` is now also used by production code
// (`project_db_ops::resolve_or_register_workspace_root`, resolving an
// `X-Tachi-Workspace-Root` path to its git root), not just `utils/tests`
// (which uses `super::*`) — no longer test-only.
pub(super) use self::workspace::find_git_root_from;
// #1120 PR2: `find_git_root` (the caller-cwd-blind, no-argument form) lost
// its last crate-level caller when `project_db_ops::handle_tachi_init_project_db`
// stopped falling back to it — it is still used internally by
// `find_project_git_root` below (same module, no re-export needed there), so
// the function itself stays, only this now-dead re-export is dropped.
pub(super) use self::workspace::{find_project_git_root, is_active_global_rule, resolve_home_arg};
