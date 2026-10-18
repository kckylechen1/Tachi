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

#[cfg(test)]
mod tests;

pub(crate) use api_keys::collect_api_key_status_with_probe_cache;
pub(crate) use inference::{
    apply_inferred_provider_failures, format_elapsed, infer_provider_from_failed_job,
};
pub(crate) use model::{model_lanes_json, model_lanes_json_for_running_client};
pub(crate) use probe_cache::{
    read_provider_probe_cache, refresh_provider_probe_cache, write_provider_probe_cache_report,
};
pub(crate) use probes::{run_provider_probe_report, PROVIDER_HEALTH_PERSIST_PHASE};
pub(crate) use readiness::{agent_readiness_json, format_backfill_command};
#[cfg(test)]
pub(crate) use scoring::calculate_health_score;
pub(crate) use scoring::{calculate_health_deductions, health_score_from_deductions};
pub(crate) use types::{
    DoctorProbeCacheRefresh, HealthDeduction, ProviderProbeCache, ProviderProbeReport,
    ProviderProbeResult, ProviderRotationGroupProbe,
};

/// Stable internal API for the doctor daily pipeline: refresh the on-disk
/// provider probe cache and return the updated cache.
pub(crate) async fn refresh_doctor_probe_cache(
    app_home: &Path,
    global_db_path: &Path,
    schema_migration: &memcore::MigrationAuthority,
) -> DoctorProbeCacheRefresh {
    let report = probes::run_provider_probe_report_with_migration_authority(
        global_db_path,
        schema_migration,
    )
    .await;
    let cache_write =
        probe_cache::write_provider_probe_cache_report(app_home, global_db_path, report.clone());
    DoctorProbeCacheRefresh {
        report,
        cache_write,
    }
}

/// Stable internal API for the doctor key report: collect API-key status rows
/// (optionally with vault-value comparison) and live provider probes in one
/// call. Returns `(key_status_rows, probe_results)`.
pub(crate) async fn collect_doctor_provider_key_report(
    global_db_path: &Path,
    probe_keys: bool,
    strict_read_only: bool,
) -> (Vec<super::ApiKeyStatus>, Vec<ProviderProbeResult>) {
    let keys = if strict_read_only {
        api_keys::collect_api_key_status_immutable(global_db_path)
    } else if probe_keys {
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
