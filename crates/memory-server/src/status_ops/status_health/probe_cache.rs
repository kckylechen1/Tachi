use std::path::Path;

use super::probes::run_provider_probe_report;
use super::types::{ProviderProbeCache, ProviderProbeReport};

const PROVIDER_PROBE_CACHE_TTL_SECS: i64 = 24 * 60 * 60;

pub(crate) async fn refresh_provider_probe_cache(
    app_home: &Path,
    global_db_path: &Path,
) -> Result<ProviderProbeCache, String> {
    let report = run_provider_probe_report(global_db_path).await;
    write_provider_probe_cache_report(app_home, report)
}

pub(crate) fn read_provider_probe_cache(app_home: &Path) -> Option<ProviderProbeCache> {
    let raw = std::fs::read_to_string(provider_probe_cache_path(app_home)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub(crate) fn write_provider_probe_cache_report(
    app_home: &Path,
    report: ProviderProbeReport,
) -> Result<ProviderProbeCache, String> {
    let cache = ProviderProbeCache {
        last_probe_at: chrono::Utc::now().to_rfc3339(),
        ttl_seconds: PROVIDER_PROBE_CACHE_TTL_SECS,
        probes: report.probes,
        rotation_groups: report.rotation_groups,
    };
    let path = provider_probe_cache_path(app_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create probe cache dir: {e}"))?;
    }
    let serialized = serde_json::to_string_pretty(&cache)
        .map_err(|e| format!("serialize provider probe cache: {e}"))?;
    crate::utils::write_owner_only_file_atomic(&path, serialized.as_bytes())?;
    Ok(cache)
}

fn provider_probe_cache_path(app_home: &Path) -> std::path::PathBuf {
    app_home.join("status").join("provider-probes.json")
}
