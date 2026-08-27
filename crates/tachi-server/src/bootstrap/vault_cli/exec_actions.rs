use super::super::VaultExecExit;
use super::password::read_vault_password;
use crate::server_state::MemoryServer;
use crate::vault_ops::{
    handle_vault_unlock, load_unlocked_env_secrets_for_child_env_with_consumer, VaultUnlockParams,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
struct RequiredEnvironmentUnavailable {
    missing: Vec<(String, String)>,
}

impl std::fmt::Display for RequiredEnvironmentUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let missing = self
            .missing
            .iter()
            .map(|(name, reason)| format!("{name} ({reason})"))
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            f,
            "vault exec refused before spawn: required environment variable(s) unavailable: {missing}"
        )
    }
}

impl std::error::Error for RequiredEnvironmentUnavailable {}

pub(super) async fn run_exec_action(
    global_db_path: &PathBuf,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
    consumer: Option<&str>,
    require: &[String],
    allow_unauthenticated: bool,
    command: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let require = normalize_requirements(require)?;
    let cwd = std::env::current_dir()?;
    let server = match unlock_cli_server(
        global_db_path,
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )
    .await
    {
        Ok(server) => server,
        Err(err) if require.is_empty() && allow_unauthenticated => {
            // Explicit opt-in (#1413 concern 4): run credential-less with the
            // inherited environment, matching the historical fail-open path.
            eprintln!(
                "vault exec: injected no vault variables: vault unavailable ({err}); executing with inherited environment (--allow-unauthenticated)"
            );
            return run_command(command, &[]);
        }
        Err(err) if require.is_empty() => {
            // Default: refuse to spawn a credential-less child before exec, so
            // a caller that only checks exit status never silently gets a
            // Vault-less run (#1413 concern 4).
            return Err(format!(
                "vault exec refused before spawn: vault unavailable ({err}); pass --allow-unauthenticated to run the child with the inherited environment"
            )
            .into());
        }
        Err(err) => {
            return Err(Box::new(RequiredEnvironmentUnavailable {
                missing: require
                    .into_iter()
                    .map(|name| (name, format!("vault unavailable: {err}")))
                    .collect(),
            }));
        }
    };

    run_with_unlocked_server(
        &server,
        &cwd,
        consumer,
        &require,
        allow_unauthenticated,
        command,
    )
}

async fn unlock_cli_server(
    global_db_path: &PathBuf,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<MemoryServer, Box<dyn std::error::Error>> {
    let server = crate::cli_client::build_in_process_server(global_db_path, None)?;
    let params = if keychain {
        // Match `vault get` precedence: --keychain wins over the other
        // password sources, while the server path keeps the password out of
        // the argument object.
        VaultUnlockParams {
            password: String::new(),
            password_fifo_path: None,
            use_keychain: true,
        }
    } else {
        VaultUnlockParams {
            password: read_vault_password(
                stdin_password,
                false,
                password_file,
                insecure_password_file,
            )?,
            password_fifo_path: None,
            use_keychain: false,
        }
    };
    handle_vault_unlock(&server, params)
        .await
        .map_err(|err| format!("vault unlock failed: {err}"))?;
    Ok(server)
}

fn run_with_unlocked_server(
    server: &MemoryServer,
    cwd: &Path,
    consumer: Option<&str>,
    require: &[String],
    allow_unauthenticated: bool,
    command: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let report = match load_unlocked_env_secrets_for_child_env_with_consumer(
        server,
        Some(cwd),
        consumer,
    ) {
        Ok(report) => report,
        Err(err) if require.is_empty() && allow_unauthenticated => {
            // Explicit opt-in (#1413 concern 4): run credential-less with the
            // inherited environment, matching the historical fail-open path.
            eprintln!(
                "vault exec: injected no vault variables: vault environment unavailable ({err}); executing with inherited environment (--allow-unauthenticated)"
            );
            return run_command(command, &[]);
        }
        Err(err) if require.is_empty() => {
            // Default: refuse to spawn a credential-less child before exec.
            return Err(format!(
                "vault exec refused before spawn: vault environment unavailable ({err}); pass --allow-unauthenticated to run the child with the inherited environment"
            )
            .into());
        }
        Err(err) => {
            return Err(Box::new(RequiredEnvironmentUnavailable {
                missing: require
                    .iter()
                    .cloned()
                    .map(|name| (name, format!("vault environment unavailable: {err}")))
                    .collect(),
            }));
        }
    };

    let mut injected = Vec::new();
    let mut child_env = Vec::new();
    for (name, value) in report.secrets {
        if std::env::var_os(&name).is_none() {
            injected.push(name.clone());
            child_env.push((name, value));
        }
    }

    let missing = require
        .iter()
        .filter(|name| {
            std::env::var_os(name).is_none() && !injected.iter().any(|injected| injected == *name)
        })
        .cloned()
        .map(|name| {
            let reason = report.unavailable.get(&name).cloned().unwrap_or_else(|| {
                "not present in the caller environment and no matching unlocked Vault entry or project binding was resolved"
                    .to_string()
            });
            (name, reason)
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(Box::new(RequiredEnvironmentUnavailable { missing }));
    }

    if injected.is_empty() {
        eprintln!(
            "vault exec: injected no vault variables (caller environment already supplied them or no matching entries were available)"
        );
    } else {
        eprintln!("vault exec: injected {}", injected.join(", "));
    }
    run_command(command, &child_env)
}

fn normalize_requirements(require: &[String]) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for raw in require {
        let name = raw.trim();
        if !crate::utils::is_shell_env_name(name) {
            return Err(format!("--require expects shell environment names; got '{name}'").into());
        }
        if seen.insert(name.to_string()) {
            normalized.push(name.to_string());
        }
    }
    Ok(normalized)
}

fn run_command(
    command: &[String],
    child_env: &[(String, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((program, args)) = command.split_first() else {
        return Err("No command provided".into());
    };
    let mut child = Command::new(program);
    child.args(args);
    for (name, value) in child_env {
        child.env(name, value);
    }
    let status = child.status()?;
    if status.success() {
        Ok(())
    } else {
        eprintln!("vault exec: child process exited with {status}");
        Err(Box::new(VaultExecExit::from_status(status)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvRestore;
    use crate::vault_ops::{VaultInitParams, VaultSetParams};
    use rmcp::handler::server::wrapper::Parameters;

    async fn unlocked_server() -> (tempfile::TempDir, MemoryServer) {
        let dir = tempfile::tempdir().expect("temp vault db");
        let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");
        server
            .vault_init(Parameters(VaultInitParams {
                password: "vault-exec-test-password".to_string(),
            }))
            .await
            .expect("initialize vault");
        (dir, server)
    }

    async fn set_secret(server: &MemoryServer, name: &str, value: &str) {
        server
            .vault_set(Parameters(VaultSetParams {
                name: name.to_string(),
                value: value.to_string(),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: "vault exec test".to_string(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
                rebind: false,
            }))
            .await
            .expect("store test secret");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn exec_requires_missing_name_before_spawning() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let (dir, server) = unlocked_server().await;
        let marker = dir.path().join("must-not-exist");
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("touch {}", marker.display()),
        ];
        let err = run_with_unlocked_server(
            &server,
            dir.path(),
            None,
            &["NOSUCHKEY_XYZ".to_string()],
            false,
            &command,
        )
        .expect_err("missing requirement must refuse before spawn");

        assert!(err.to_string().contains("NOSUCHKEY_XYZ"), "{err}");
        assert!(!marker.exists(), "required-env failure spawned the child");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn exec_preserves_caller_environment_over_vault_value() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _mode = EnvRestore::set("TACHI_VAULT_CHILD_ENV", "all");
        let _owner = EnvRestore::set("TACHI_VAULT_EXEC_FILL_API_KEY", "owner-value");
        let (dir, server) = unlocked_server().await;
        set_secret(&server, "TACHI_VAULT_EXEC_FILL_API_KEY", "vault-value").await;

        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "test \"$TACHI_VAULT_EXEC_FILL_API_KEY\" = owner-value".to_string(),
        ];
        run_with_unlocked_server(&server, dir.path(), None, &[], false, &command)
            .expect("caller environment must win over the Vault value");
    }

    #[test]
    fn exec_locked_vault_refuses_required_name_before_spawning() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let dir = tempfile::tempdir().expect("temp vault db");
        let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");
        let marker = dir.path().join("must-not-exist");
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("touch {}", marker.display()),
        ];
        let err = run_with_unlocked_server(
            &server,
            dir.path(),
            None,
            &["LOCKED_REQUIRED_API_KEY".to_string()],
            false,
            &command,
        )
        .expect_err("locked Vault must refuse required name before spawn");

        assert!(err.to_string().contains("LOCKED_REQUIRED_API_KEY"), "{err}");
        assert!(err.to_string().contains("Vault"), "{err}");
        assert!(!marker.exists(), "locked-vault failure spawned the child");
    }

    // #1413 concern 4: with no `--require` names and no opt-in flag, a Vault
    // environment-load failure must REFUSE to spawn the child before exec
    // (fail-closed), so a caller that only checks exit status never silently
    // gets a credential-less run.
    //
    // Discrimination: on the pre-fix code this arm fail-opened — empty require
    // ran the child with the inherited environment and returned Ok — so the
    // `expect_err` and the `!marker.exists()` assertions both FAIL there. With
    // the opt-in flag the path is restored (see the test below).
    #[test]
    fn exec_env_load_failure_refuses_spawn_by_default() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let dir = tempfile::tempdir().expect("temp vault db");
        // Uninitialized/locked Vault: env loading fails inside the helper.
        let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");
        let marker = dir.path().join("must-not-exist");
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("touch {}", marker.display()),
        ];
        let err = run_with_unlocked_server(&server, dir.path(), None, &[], false, &command)
            .expect_err("default (no --allow-unauthenticated) must refuse before spawn");

        let msg = err.to_string();
        assert!(
            msg.contains("refused before spawn") && msg.contains("--allow-unauthenticated"),
            "default refusal must name the opt-in flag: {msg}"
        );
        assert!(!marker.exists(), "default refusal spawned the child");
    }

    // #1413 concern 4: the explicit opt-in restores the historical inherited-
    // environment execution on the same env-load failure. This is the
    // "inherited-environment execution" path the issue asks to keep available.
    #[test]
    fn exec_allow_unauthenticated_runs_inherited_on_env_load_failure() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let dir = tempfile::tempdir().expect("temp vault db");
        let server = MemoryServer::new(dir.path().join("memory.db"), None).expect("server");
        let marker = dir.path().join("must-exist");
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("touch {}", marker.display()),
        ];
        run_with_unlocked_server(&server, dir.path(), None, &[], true, &command)
            .expect("--allow-unauthenticated must run the child with the inherited environment");

        assert!(
            marker.exists(),
            "--allow-unauthenticated must spawn the child"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn exec_action_unlock_failure_refuses_spawn_by_default() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let dir = tempfile::tempdir().expect("temp vault db");
        let global_db_path = dir.path().join("memory.db");
        let password_file = dir.path().join("password.txt");
        std::fs::write(&password_file, "dummy-password\n").expect("write password file");
        let marker = dir.path().join("must-not-exist");
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("touch {}", marker.display()),
        ];
        let err = run_exec_action(
            &global_db_path,
            false,
            false,
            Some(&password_file),
            true,
            None,
            &[],
            false,
            &command,
        )
        .await
        .expect_err(
            "default (no --allow-unauthenticated) must refuse before spawn when vault unlock fails",
        );

        let msg = err.to_string();
        assert!(
            msg.contains("refused before spawn") && msg.contains("--allow-unauthenticated"),
            "default refusal must name the opt-in flag: {msg}"
        );
        assert!(
            msg.contains("vault unavailable"),
            "refusal must surface the unlock failure as 'vault unavailable': {msg}"
        );
        assert!(!marker.exists(), "default refusal spawned the child");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn exec_action_unlock_failure_allow_unauthenticated_runs_inherited() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _inherited = EnvRestore::set("TACHI_VAULT_EXEC_ACTION_INHERITED", "sentinel-value");
        let dir = tempfile::tempdir().expect("temp vault db");
        let global_db_path = dir.path().join("memory.db");
        let password_file = dir.path().join("password.txt");
        std::fs::write(&password_file, "dummy-password\n").expect("write password file");
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "test \"$TACHI_VAULT_EXEC_ACTION_INHERITED\" = sentinel-value".to_string(),
        ];
        run_exec_action(
            &global_db_path,
            false,
            false,
            Some(&password_file),
            true,
            None,
            &[],
            true,
            &command,
        )
        .await
        .expect(
            "--allow-unauthenticated must run the child with the inherited environment when vault unlock fails",
        );
    }

    #[test]
    fn exec_keeps_child_exit_code() {
        let err = run_command(
            &["sh".to_string(), "-c".to_string(), "exit 42".to_string()],
            &[],
        )
        .expect_err("non-zero child exit must be surfaced");
        let exit = err
            .downcast_ref::<VaultExecExit>()
            .expect("typed vault exec exit");
        assert_eq!(exit.code(), 42);
    }
}
