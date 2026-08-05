use super::types::{
    AllowedConsumers, CredentialMaterializeReport, CredentialMaterializeStepReport,
    CredentialProfile,
};
use memcore::MemoryStore;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

fn consumer_allowed(allowed: &AllowedConsumers, consumer: &str) -> bool {
    if allowed.agents.is_empty() && allowed.profiles.is_empty() {
        return true;
    }
    allowed.agents.iter().any(|agent| agent == consumer)
        || allowed.profiles.iter().any(|profile| profile == consumer)
}

fn entry_allows_consumer(entry_allowed_agents: Option<&[String]>, consumer: &str) -> bool {
    entry_allowed_agents
        .map(|agents| agents.iter().any(|agent| agent == consumer))
        .unwrap_or(true)
}

pub(super) fn resolve_source(profile: &CredentialProfile, source: &str) -> String {
    profile
        .entries
        .get(source)
        .cloned()
        .unwrap_or_else(|| source.to_string())
}

fn display_target(target: &str) -> String {
    if let Some(rest) = target.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    target.to_string()
}

fn materializer_output(kind: &str, target: &str) -> String {
    match kind {
        "env" => format!("env:{target}"),
        "file_copy" => format!("file:{target}"),
        "config_overlay" => format!("config_overlay:{target}"),
        "config_patch" => format!("config_patch:{target}"),
        "config_content_env" => format!("config_content_env:{target}"),
        _ => format!("{kind}:{target}"),
    }
}

pub(super) fn is_env_target(target: &str) -> bool {
    let mut chars = target.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

pub(super) fn materializer_writes_file(kind: &str, target: &str) -> bool {
    matches!(kind, "file_copy" | "config_patch")
        || (kind == "config_overlay" && !is_env_target(target))
}

fn expand_target(target: &str, run_dir: Option<&Path>) -> String {
    let mut expanded = display_target(target);
    if let Some(run_dir) = run_dir {
        let run_dir = run_dir.to_string_lossy();
        let credentials_dir = Path::new(run_dir.as_ref())
            .join("credentials")
            .to_string_lossy()
            .to_string();
        expanded = expanded
            .replace("{credentials_dir}", &credentials_dir)
            .replace("{run_dir}", run_dir.as_ref());
    }
    expanded
}

pub(crate) fn profile_secret_names(profile: &CredentialProfile) -> Vec<String> {
    let mut names = profile
        .materializers
        .iter()
        .map(|materializer| resolve_source(profile, &materializer.source))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

pub(crate) fn plan_credential_materialization(
    profile_name: &str,
    profile: &CredentialProfile,
    consumer: &str,
    store: &MemoryStore,
) -> Result<CredentialMaterializeReport, String> {
    plan_credential_materialization_with_run_dir(profile_name, profile, consumer, store, None)
}

pub(crate) fn plan_credential_materialization_with_run_dir(
    profile_name: &str,
    profile: &CredentialProfile,
    consumer: &str,
    store: &MemoryStore,
    run_dir: Option<&Path>,
) -> Result<CredentialMaterializeReport, String> {
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect::<HashMap<_, _>>();

    let allowed = consumer_allowed(&profile.allowed_consumers, consumer);
    let mut missing_secrets = Vec::new();
    let mut denied_secrets = Vec::new();
    let mut warnings = Vec::new();
    let mut steps = Vec::new();

    if !allowed {
        warnings.push(format!(
            "consumer '{consumer}' is not allowed by credential profile '{profile_name}'"
        ));
    }

    for materializer in &profile.materializers {
        let resolved_secret = resolve_source(profile, &materializer.source);
        let target = expand_target(&materializer.target, run_dir);
        let known_kind = matches!(
            materializer.kind.as_str(),
            "env" | "file_copy" | "config_overlay" | "config_patch" | "config_content_env"
        );
        if !known_kind {
            warnings.push(format!(
                "unsupported materializer type '{}' for target '{}'",
                materializer.kind, materializer.target
            ));
        }

        let status = match entries.get(&resolved_secret) {
            None => {
                if !missing_secrets.contains(&resolved_secret) {
                    missing_secrets.push(resolved_secret.clone());
                }
                "missing_secret"
            }
            Some(entry) if !entry_allows_consumer(entry.allowed_agents.as_deref(), consumer) => {
                if !denied_secrets.contains(&resolved_secret) {
                    denied_secrets.push(resolved_secret.clone());
                }
                "denied_secret"
            }
            Some(_) if !allowed => "denied_consumer",
            Some(_) if !known_kind => "unsupported",
            Some(_) => "ready",
        };

        steps.push(CredentialMaterializeStepReport {
            materializer_type: materializer.kind.clone(),
            source: materializer.source.clone(),
            resolved_secret,
            target: target.clone(),
            output: materializer_output(&materializer.kind, &target),
            status: status.to_string(),
            redacted: true,
            would_write: materializer_writes_file(&materializer.kind, &target),
            applied: false,
            chmod: materializer.chmod.clone(),
        });
    }

    Ok(CredentialMaterializeReport {
        profile: profile_name.to_string(),
        consumer: consumer.to_string(),
        dry_run: true,
        provider: profile.provider.clone(),
        allowed,
        steps,
        missing_secrets,
        denied_secrets,
        warnings,
    })
}

pub(crate) fn credential_materialize_report_json(
    report: &CredentialMaterializeReport,
) -> serde_json::Value {
    json!(report)
}
