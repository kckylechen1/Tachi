use std::collections::HashSet;
use std::path::Path;

mod api_keys;
mod inference;
mod model;
mod probe_cache;
mod probes;
mod readiness;
mod rotation;
mod scoring;
mod types;
mod vault;

#[cfg(test)]
mod tests;

pub(crate) use api_keys::{collect_api_key_status_with_probe_cache, API_KEY_DEFS};
pub(crate) use inference::{
    apply_inferred_provider_failures, format_elapsed, infer_provider_from_failed_job,
};
pub(crate) use model::{model_lanes_json, provider_key_status_json};
pub(crate) use probe_cache::{
    read_provider_probe_cache, refresh_provider_probe_cache, write_provider_probe_cache_report,
};
pub(crate) use probes::{run_provider_probe_report, PROVIDER_HEALTH_PERSIST_PHASE};
pub(crate) use readiness::{agent_readiness_json, format_backfill_command};
#[cfg(test)]
pub(crate) use scoring::calculate_health_score;
pub(crate) use scoring::{calculate_health_deductions, health_score_from_deductions};
pub(crate) use types::{
    HealthDeduction, ProviderProbeCache, ProviderProbeReport, ProviderProbeResult,
    ProviderRotationGroupProbe,
};
pub(crate) use vault::load_keychain_vault_api_key_values;

// ─── Stable internal facades for cross-module callers ───────────────────────
//
// These thin wrappers give `provider_config` and the doctor/manifest CLI a
// stable call surface so they no longer reach into status_health submodules
// directly. Behavior is unchanged — each delegates to the existing internal
// implementation.

/// Stable internal API for `provider_config`: every provider API-key env-var
/// name (primary keys plus aliases) recognized by the status layer, flattened
/// into a set for secret-materialization lookups.
pub(crate) fn provider_api_key_env_names() -> HashSet<String> {
    let mut names = HashSet::new();
    for def in api_keys::API_KEY_DEFS {
        names.insert(def.key.to_string());
        for alias in def.aliases {
            names.insert((*alias).to_string());
        }
    }
    names
}

/// Stable internal API for the doctor daily pipeline: refresh the on-disk
/// provider probe cache and return the updated cache.
pub(crate) async fn refresh_doctor_probe_cache(
    app_home: &Path,
    global_db_path: &Path,
    schema_migration: &memcore::MigrationAuthority,
) -> Result<ProviderProbeCache, String> {
    let report = probes::run_provider_probe_report_with_migration_authority(
        global_db_path,
        schema_migration,
    )
    .await;
    probe_cache::write_provider_probe_cache_report(app_home, global_db_path, report)
}

/// Stable internal API for the doctor key report: collect API-key status rows
/// (optionally with vault-value comparison) and live provider probes in one
/// call. Returns `(key_status_rows, probe_results)`.
pub(crate) async fn collect_doctor_provider_key_report(
    global_db_path: &Path,
    probe_keys: bool,
) -> (Vec<super::ApiKeyStatus>, Vec<ProviderProbeResult>) {
    let keys = if probe_keys {
        api_keys::collect_api_key_status_with_value_compare(global_db_path)
    } else {
        api_keys::collect_api_key_status(global_db_path)
    };
    let probes = if probe_keys {
        probes::run_provider_probes(global_db_path).await
    } else {
        Vec::new()
    };
    (keys, probes)
}

#[cfg(test)]
use super::{ApiKeyRotationMemberStatus, ApiKeyStatus};
#[cfg(test)]
use api_keys::collect_api_key_status_from_sources;
#[cfg(test)]
pub(crate) use inference::infer_provider_from_auth_error;
#[cfg(test)]
use rotation::RotationSourceStatus;
#[cfg(test)]
use std::collections::HashMap;
