use super::*;
#[cfg(test)]
use tokio::process::Command;

#[cfg(test)]
pub(crate) fn apply_unlocked_vault_env(
    cmd: &mut Command,
    server: &MemoryServer,
    cwd: Option<&std::path::Path>,
) -> usize {
    let env = unlocked_vault_child_env_map(server, cwd);
    for (name, value) in &env {
        cmd.env(name, value);
    }
    env.len()
}

pub(super) fn unlocked_vault_child_env_map(
    server: &MemoryServer,
    cwd: Option<&std::path::Path>,
) -> HashMap<String, String> {
    let Ok(secrets) = server.unlocked_env_secrets_for_child_env(cwd) else {
        return HashMap::new();
    };

    let override_existing = std::env::var("TACHI_VAULT_CHILD_ENV")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "all" | "full" | "legacy" | "legacy_all" | "override"
            )
        })
        .unwrap_or(false);
    let mut env = HashMap::new();
    for (name, value) in secrets {
        if !override_existing && std::env::var_os(&name).is_some() {
            continue;
        }
        env.insert(name, value);
    }
    env
}

pub(super) fn credential_search_dirs(cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();

    if let Some(cwd) = cwd {
        for ancestor in cwd.ancestors() {
            let dir = ancestor.join(default_credentials_dir());
            let key = dir.to_string_lossy().to_string();
            if seen.insert(key) {
                dirs.push(dir);
            }
        }
    }

    let default_dir = default_credentials_dir();
    let key = default_dir.to_string_lossy().to_string();
    if seen.insert(key) {
        dirs.push(default_dir);
    }

    dirs
}

pub(super) fn find_dispatch_credential_profile(
    profile_name: &str,
    cwd: Option<&Path>,
) -> Result<(PathBuf, tachi_credential_profile::CredentialProfile), String> {
    let mut searched = Vec::new();
    let mut skipped = Vec::new();
    for dir in credential_search_dirs(cwd) {
        searched.push(dir.display().to_string());
        if !dir.exists() {
            continue;
        }
        match find_credential_profile(&dir, profile_name) {
            Ok(found) => return Ok(found),
            Err(err) => skipped.push(err),
        }
    }

    let mut msg = format!(
        "Credential profile '{}' not found. Searched: {}",
        profile_name,
        searched.join(", ")
    );
    if !skipped.is_empty() {
        msg.push_str(&format!("; skipped: {}", skipped.join(" | ")));
    }
    Err(msg)
}

pub(super) fn dispatch_credential_consumer(
    agent_norm: &str,
    selected_profile: Option<&str>,
    profile: &tachi_credential_profile::CredentialProfile,
) -> String {
    let allowed = &profile.allowed_consumers;
    if allowed.agents.is_empty() && allowed.profiles.is_empty() {
        return agent_norm.to_string();
    }
    if allowed.agents.iter().any(|agent| agent == agent_norm) {
        return agent_norm.to_string();
    }
    if let Some(selected_profile) = selected_profile {
        if allowed
            .profiles
            .iter()
            .any(|profile| profile == selected_profile)
        {
            return selected_profile.to_string();
        }
    }
    selected_profile.unwrap_or(agent_norm).to_string()
}

pub(super) struct DispatchCredentialMaterialization {
    pub(super) reports: Vec<CredentialMaterializeReport>,
    pub(super) env: HashMap<String, String>,
}

pub(super) fn credential_report_ready(report: &CredentialMaterializeReport) -> bool {
    report.allowed
        && report.missing_secrets.is_empty()
        && report.denied_secrets.is_empty()
        && report.steps.iter().all(|step| step.status == "ready")
}

pub(super) fn materialize_dispatch_credentials(
    server: &MemoryServer,
    grant: &tachi_params::ExecutionGrant,
    agent_norm: &str,
    selected_profile: Option<&str>,
    run_dir: &Path,
) -> Result<DispatchCredentialMaterialization, String> {
    if grant.credential_profiles.is_empty() {
        return Ok(DispatchCredentialMaterialization {
            reports: Vec::new(),
            env: HashMap::new(),
        });
    }

    let cwd = grant.allowed_cwd.as_deref();
    let mut profile_names = grant
        .credential_profiles
        .iter()
        .map(|profile| profile.trim().to_string())
        .filter(|profile| !profile.is_empty())
        .collect::<Vec<_>>();
    crate::skill_policy::dedupe_preserve_order(&mut profile_names);

    let mut reports = Vec::new();
    let mut env = HashMap::new();
    for profile_name in profile_names {
        let (_, profile) = find_dispatch_credential_profile(&profile_name, cwd)?;
        let consumer = dispatch_credential_consumer(agent_norm, selected_profile, &profile);
        let plan = server.with_global_store(|store| {
            plan_credential_materialization_with_run_dir(
                &profile_name,
                &profile,
                &consumer,
                store,
                Some(run_dir),
            )
        })?;
        if !credential_report_ready(&plan) {
            let plan_json = serde_json::to_string(&credential_materialize_report_json(&plan))
                .map_err(|e| format!("serialize credential plan: {e}"))?;
            return Err(format!(
                "Credential profile '{}' is not ready for consumer '{}': {}",
                profile_name, consumer, plan_json
            ));
        }

        let secret_names = profile_secret_names(&profile);
        let mut secret_values = HashMap::new();
        for secret_name in secret_names {
            let value = read_unlocked_vault_secret(server, &secret_name, Some(&consumer), false)
                .map_err(|err| {
                    format!(
                        "Credential profile '{}' requires unlocked Vault secret '{}' for consumer '{}': {}",
                        profile_name, secret_name, consumer, err
                    )
                })?;
            secret_values.insert(secret_name, value);
        }

        let result = server.with_global_store(|store| {
            apply_credential_materialization(
                &profile_name,
                &profile,
                &consumer,
                store,
                &secret_values,
                &CredentialApplyOptions {
                    allow_existing: false,
                    run_dir: Some(run_dir.to_path_buf()),
                },
            )
        })?;
        env.extend(result.env);
        reports.push(result.report);
    }

    Ok(DispatchCredentialMaterialization { reports, env })
}
