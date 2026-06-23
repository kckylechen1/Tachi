use super::types::CredentialMaterializeStepReport;
use super::CREDENTIAL_MATERIALIZATION_NAMESPACE;
use crate::utils::stable_hash;
use memory_core::MemoryStore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct ManagedCredentialMaterialization {
    pub(super) version: u32,
    pub(super) profile: String,
    pub(super) consumer: String,
    pub(super) materializer_type: String,
    pub(super) source: String,
    pub(super) resolved_secret: String,
    pub(super) target: String,
    pub(super) content_hash: String,
    pub(super) chmod: Option<String>,
    pub(super) managed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cleanup_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cleaned_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cleanup_run_dir: Option<String>,
}

fn managed_materialization_key(
    profile_name: &str,
    consumer: &str,
    materializer_type: &str,
    resolved_secret: &str,
    target: &str,
) -> String {
    format!(
        "managed:{}",
        stable_hash(&format!(
            "{profile_name}\u{1f}{consumer}\u{1f}{materializer_type}\u{1f}{resolved_secret}\u{1f}{target}"
        ))
    )
}

pub(super) fn managed_materialization_content_hash(
    profile_name: &str,
    consumer: &str,
    target: &str,
    content: &str,
) -> String {
    format!(
        "stable-fnv1a:{}",
        stable_hash(&format!(
            "{profile_name}\u{1f}{consumer}\u{1f}{target}\u{1f}{content}"
        ))
    )
}

pub(super) fn record_managed_materialization(
    store: &MemoryStore,
    profile_name: &str,
    consumer: &str,
    step: &CredentialMaterializeStepReport,
    content: &str,
) -> Result<(), String> {
    let key = managed_materialization_key(
        profile_name,
        consumer,
        &step.materializer_type,
        &step.resolved_secret,
        &step.target,
    );
    let metadata = ManagedCredentialMaterialization {
        version: 1,
        profile: profile_name.to_string(),
        consumer: consumer.to_string(),
        materializer_type: step.materializer_type.clone(),
        source: step.source.clone(),
        resolved_secret: step.resolved_secret.clone(),
        target: step.target.clone(),
        content_hash: managed_materialization_content_hash(
            profile_name,
            consumer,
            &step.target,
            content,
        ),
        chmod: step.chmod.clone(),
        managed_at: chrono::Utc::now().to_rfc3339(),
        cleanup_status: None,
        cleaned_at: None,
        cleanup_run_dir: None,
    };
    let value = serde_json::to_string(&metadata)
        .map_err(|e| format!("serialize credential materialization metadata: {e}"))?;
    store
        .set_state(CREDENTIAL_MATERIALIZATION_NAMESPACE, &key, &value)
        .map_err(|e| format!("record credential materialization metadata: {e}"))?;
    Ok(())
}

pub(super) fn read_managed_materialization(
    store: &MemoryStore,
    profile_name: &str,
    consumer: &str,
    step: &CredentialMaterializeStepReport,
) -> Result<Option<ManagedCredentialMaterialization>, String> {
    let key = managed_materialization_key(
        profile_name,
        consumer,
        &step.materializer_type,
        &step.resolved_secret,
        &step.target,
    );
    let Some((value_json, _version)) = store
        .get_state_kv(CREDENTIAL_MATERIALIZATION_NAMESPACE, &key)
        .map_err(|e| format!("read credential materialization metadata: {e}"))?
    else {
        return Ok(None);
    };
    let metadata = serde_json::from_str(&value_json)
        .map_err(|e| format!("parse credential materialization metadata: {e}"))?;
    Ok(Some(metadata))
}

pub(super) fn mark_metadata_cleaned(
    store: &MemoryStore,
    row_key: &str,
    metadata: &mut ManagedCredentialMaterialization,
    cleanup_run_dir: Option<&Path>,
) -> Result<(), String> {
    metadata.cleanup_status = Some("cleaned".to_string());
    metadata.cleaned_at = Some(chrono::Utc::now().to_rfc3339());
    metadata.cleanup_run_dir = cleanup_run_dir.map(|run_dir| run_dir.to_string_lossy().to_string());
    let value = serde_json::to_string(metadata)
        .map_err(|e| format!("serialize cleaned credential metadata: {e}"))?;
    store
        .set_state(CREDENTIAL_MATERIALIZATION_NAMESPACE, row_key, &value)
        .map_err(|e| format!("mark credential target cleaned: {e}"))?;
    Ok(())
}

pub(super) fn managed_target_current_hash(
    metadata: &ManagedCredentialMaterialization,
    target: &Path,
) -> Result<String, String> {
    let current = fs::read_to_string(target)
        .map_err(|e| format!("read managed credential target '{}': {e}", target.display()))?;
    Ok(managed_materialization_content_hash(
        &metadata.profile,
        &metadata.consumer,
        &metadata.target,
        &current,
    ))
}
