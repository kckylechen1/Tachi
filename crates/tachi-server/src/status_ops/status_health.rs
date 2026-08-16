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

pub(crate) use api_keys::{collect_api_key_status_with_probe_cache, KeyClass, API_KEY_DEFS};
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
pub(crate) use vault::load_keychain_vault_api_key_values;

// ─── Stable internal facades for cross-module callers ───────────────────────
//
// These thin wrappers give `provider_config` and the doctor/manifest CLI a
// stable call surface so they no longer reach into status_health submodules
// directly. Behavior is unchanged — each delegates to the existing internal
// implementation.

/// Stable internal API for `provider_config`'s LLM materialization allowlist:
/// every `KeyClass::ModelApi` env-var name (primary keys plus aliases)
/// recognized by the status layer, flattened into a set. #1680/D3: this is
/// the *narrow* view — SearchApi (Exa/Tavily/Google Search) and any future
/// Infra names are deliberately excluded, because this set gates which names
/// are eligible to enter the LLM provider secret cache
/// (`materialize_provider_secrets_from_durable_source`). Use
/// [`admitted_env_secret_names`] for "is this name in Tachi's provider
/// vocabulary at all" surfaces (lane env injection, providers-doctor
/// admission, the plaintext secret scanner) — those must see every class.
pub(crate) fn model_provider_env_names() -> HashSet<String> {
    let mut names = HashSet::new();
    for def in api_keys::API_KEY_DEFS {
        if def.class != KeyClass::ModelApi {
            continue;
        }
        names.insert(def.key.to_string());
        for alias in def.aliases {
            names.insert((*alias).to_string());
        }
    }
    names
}

/// Stable internal API for the all-class "is this a name Tachi's provider
/// registry recognizes" surfaces: lane env injection
/// (`vault_ops::env::load_unlocked_provider_env_secrets`), providers-doctor
/// admission, and the plaintext secret scanner. Unlike
/// [`model_provider_env_names`], this includes every [`KeyClass`] — a
/// Search/Infra key must still be recognized as an admitted provider secret
/// name for those consumers (env injection must keep delivering rotated
/// search keys to lane subprocesses; the scanner must still flag a plaintext
/// search key on disk), it just must never reach the LLM materialization
/// allowlist.
pub(crate) fn admitted_env_secret_names() -> HashSet<String> {
    let mut names = HashSet::new();
    for def in api_keys::API_KEY_DEFS {
        names.insert(def.key.to_string());
        for alias in def.aliases {
            names.insert((*alias).to_string());
        }
    }
    names
}

/// The canonical provider family an admitted env-var name belongs to, or
/// `None` if the name is not in the registry at all.
///
/// This is the third derived view over `API_KEY_DEFS`, and the one #1680 D2's
/// fingerprints are keyed by: `fp1` mixes in the *provider kind*, never an
/// env-var name, which is exactly what lets two names of one family holding one
/// secret collapse into a single account while the same value under two
/// different vendors stays uncorrelated. Resolution prefers an exact primary-key
/// match and falls back to the alias tables, and
/// `provider_kind_assignment_is_unambiguous` pins that no name can resolve two
/// ways — an ambiguous name would silently split or merge accounts depending on
/// registry declaration order.
///
/// Consumed by #1680 PR-C's reconcile pipeline; declared here, with the other
/// two derived views, because a fourth private copy of "which family is this
/// name" is precisely the drift D3 deleted `intake::alias_family` to end.
pub(crate) fn provider_kind_for_env_name(name: &str) -> Option<&'static str> {
    registry_def_for_env_name(name).map(|def| def.provider_kind)
}

/// The account class an admitted env-var name belongs to — the stored form of
/// the `KeyClass` boundary D3 introduced, so an account row keeps saying
/// "search credential" after the process that read the registry is gone.
///
/// Returns `None` for a name the registry does not recognize at all, which is
/// the answer reconcile needs: an unrecognized name is not eligible to become
/// an account of any class.
pub(crate) fn account_class_for_env_name(name: &str) -> Option<memcore::AccountClass> {
    registry_def_for_env_name(name).map(|def| match def.class {
        KeyClass::ModelApi => memcore::AccountClass::ModelApi,
        KeyClass::SearchApi => memcore::AccountClass::SearchApi,
        KeyClass::Infra => memcore::AccountClass::Infra,
    })
}

/// The canonical primary key of the registry entry an admitted name belongs
/// to. Reconcile uses it to pick which of a group's interchangeable names is
/// the one custody points at, so that choice is the registry's rather than an
/// accident of iteration order.
pub(crate) fn canonical_key_for_env_name(name: &str) -> Option<&'static str> {
    registry_def_for_env_name(name).map(|def| def.canonical_key)
}

/// Every interchangeable env-var name of the registry entry an admitted name
/// belongs to — the entry's own key plus its aliases — or `None` when the name
/// is not in the registry.
///
/// This is what `intake`'s advisory alias-family label is derived from. It
/// hands out the *names* rather than the `ApiKeyDef` so the registry row type
/// stays inside this module: a caller that can see the row can start depending
/// on fields the derived views deliberately do not expose.
pub(crate) fn family_env_names_for_env_name(name: &str) -> Option<Vec<&'static str>> {
    registry_def_for_env_name(name).map(|def| {
        std::iter::once(def.key)
            .chain(def.aliases.iter().copied())
            .collect()
    })
}

/// The documented authentication-probe target for an admitted env-var name,
/// or `None` when the name is unknown to the registry or its family has no
/// owner-verified, non-generating probe endpoint (#1680 D6).
///
/// This is the view that makes probing registry-driven: a caller holding a
/// vault logical name asks the registry "may this be probed, and as what",
/// and hands the answer to `tachi_llm`'s
/// `LlmClient::probe_member_auth_and_record`. The *hosts* stay compile-time
/// constants in `tachi_llm` — widening what a credential-bearing request may
/// dial is a code change in the module that dials it, never a registry edit
/// and never a DB write. What the registry decides is only which names are in
/// scope, which is exactly the half it owns.
///
/// Alias names resolve through their family, so probing
/// `EXTRACT_API_KEY` probes SiliconFlow, as it should.
pub(crate) fn auth_probe_descriptor_for_env_name(
    name: &str,
) -> Option<&'static tachi_llm::ProviderProbeDescriptor> {
    registry_def_for_env_name(name).and_then(|def| def.probe)
}

/// The single registry lookup the name-keyed views above share: primary-key
/// match first (never shadowed by another entry that merely lists the name as
/// an alias), then the alias tables.
fn registry_def_for_env_name(name: &str) -> Option<&'static api_keys::ApiKeyDef> {
    api_keys::API_KEY_DEFS
        .iter()
        .find(|def| def.key == name)
        .or_else(|| {
            api_keys::API_KEY_DEFS
                .iter()
                .find(|def| def.aliases.contains(&name))
        })
}

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
