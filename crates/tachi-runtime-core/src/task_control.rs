//! Task-control timeout policy shared by the shell task facade and the
//! daemon client (`tachi-server` shell/admin both consume this; the
//! constants previously lived in `tachi-server::tools`, then briefly in
//! `tachi-server::shared_defs` — #2075/#2077 PR B, moved on here).

/// `tachi_task(action='status')` timeout_secs default/cap for the acpx
/// control-plane call (see `task_facade::handle_tachi_task_status`).
/// Shared so `cli_client::transport::daemon_call_timeout` can derive the
/// outer RPC timeout from the *same* numbers instead of a hand-mirrored
/// copy that can drift out of sync (see #970, #1028). The `wait`/`cancel`
/// long-poll/control actions left `tachi_task` in #1319-C2; their timeout
/// consts were removed with them.
pub const TASK_CONTROL_TIMEOUT_DEFAULT_SECS: u64 = 30;
pub const TASK_CONTROL_TIMEOUT_CAP_SECS: u64 = 300;
