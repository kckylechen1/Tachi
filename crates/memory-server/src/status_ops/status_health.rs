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

pub(crate) use api_keys::{
    collect_api_key_status, collect_api_key_status_with_probe_cache,
    collect_api_key_status_with_value_compare, API_KEY_DEFS,
};
pub(crate) use inference::{
    apply_inferred_provider_failures, format_elapsed, infer_provider_from_failed_job,
};
pub(crate) use model::{model_lanes_json, provider_key_status_json};
pub(crate) use probe_cache::{
    read_provider_probe_cache, refresh_provider_probe_cache, write_provider_probe_cache_report,
};
pub(crate) use probes::{run_provider_probe_report, run_provider_probes};
pub(crate) use readiness::{agent_readiness_json, format_backfill_command};
pub(crate) use scoring::calculate_health_score;
pub(crate) use types::{
    ProviderProbeCache, ProviderProbeReport, ProviderProbeResult, ProviderRotationGroupProbe,
};
pub(crate) use vault::load_keychain_vault_api_key_values;

#[cfg(test)]
use super::{ApiKeyRotationMemberStatus, ApiKeyStatus};
#[cfg(test)]
use api_keys::collect_api_key_status_from_sources;
#[cfg(test)]
pub(crate) use inference::infer_provider_from_auth_error;
#[cfg(test)]
use rotation::RotationSourceStatus;
#[cfg(test)]
use std::collections::{HashMap, HashSet};
