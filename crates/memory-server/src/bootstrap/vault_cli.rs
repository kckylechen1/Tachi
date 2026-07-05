use super::{open_cli_store, open_cli_store_read_only};
use std::path::{Path, PathBuf};

mod credential_actions;
mod daemon;
mod intake;
mod keys;
mod output;
mod password;
mod secret_actions;
mod session_actions;
mod sync_actions;

pub(super) use keys::{vault_init_with_password, vault_upsert_secret_with_key};
pub(super) use output::lease_api_key_from_store;
pub(super) use password::read_vault_password;

// ─── `tachi vault` handler ──────────────────────────────────────────────────

pub(super) async fn run_vault_command(
    global_db_path: &PathBuf,
    app_home: &Path,
    action: tachi_bootstrap::cli::VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    use tachi_bootstrap::cli::VaultAction;

    match action {
        action @ (VaultAction::Materialize { .. }
        | VaultAction::Cleanup { .. }
        | VaultAction::Doctor { .. }) => {
            credential_actions::run_credential_action(global_db_path, action)
        }
        action @ (VaultAction::SyncStatus { .. }
        | VaultAction::SetupKeys { .. }
        | VaultAction::SyncExport { .. }
        | VaultAction::SyncImport { .. }) => sync_actions::run_sync_action(global_db_path, action),
        action @ (VaultAction::Status
        | VaultAction::Init { .. }
        | VaultAction::Lock
        | VaultAction::Unlock { .. }
        | VaultAction::List { .. }) => {
            session_actions::run_session_action(global_db_path, app_home, action).await
        }
        action @ (VaultAction::Set { .. }
        | VaultAction::SetPool { .. }
        | VaultAction::Lease { .. }
        | VaultAction::RecordKeyResult { .. }
        | VaultAction::Get { .. }
        | VaultAction::Remove { .. }) => {
            secret_actions::run_secret_action(global_db_path, app_home, action).await
        }
        action @ VaultAction::Intake { .. } => {
            intake::run_intake_action(global_db_path, app_home, action)
        }
    }
}

#[cfg(test)]
mod tests;
