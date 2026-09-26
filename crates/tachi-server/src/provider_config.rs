//! Provider runtime bootstrap: API key resolution (Vault, config.env `vault:`
//! aliases, env fallbacks) and the env→catalog deployment import that records
//! what those chains resolved to.
//!
//! Single path for daemon, MCP, CLI backfill, and vector sweep so background jobs
//! do not re-implement Keychain/Vault reads with different behavior.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::vault_ops::{classify_vault_read_error, VaultMaterializationRevision, VaultReadState};
use crate::MemoryServer;
pub use tachi_llm::{
    group_api_key_values_by_configured_rotations, is_vault_alias, parse_rotation_member_name,
    parse_vault_alias, vault_alias_line, MaterializeReport, VAULT_ALIAS_PREFIX,
};
use tachi_llm::{
    AliasSkipClass, LaneConfigOverlay, LaneFieldOverlay, LlmClient, ProviderSecret,
    VaultSourceAvailability,
};

mod health_publication;
use health_publication::{fenced_provider_health_recheck, validate_admitted_provider_health};

#[derive(Default)]
pub(crate) struct LaneConfigValues(Vec<(String, String)>);

impl LaneConfigValues {
    pub(crate) fn push(&mut self, value: (String, String)) {
        self.0.push(value);
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn into_values(mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for LaneConfigValues {
    fn drop(&mut self) {
        for (_, value) in &mut self.0 {
            crate::vault_crypto::zero_string(value);
        }
    }
}

/// #1680/D3: the LLM materialization allowlist — `ModelApi`-class names only.
/// This is the compile-time allowlist consulted by
/// `materialize_provider_secrets_from_durable_source`; it deliberately
/// excludes SearchApi (Exa/Tavily/Google Search) so a Vault-stored search key
/// can never enter the LLM provider cache through the env-name filter. The
/// filter alone is insufficient against a Vault-stored search key entering
/// through the *pool* seam (`resolve_vault_pools` loads every `*_API_KEY`
/// pool, class-blind) — see [`filter_model_provider_pools`], which is the
/// actual enforcement point.
pub(crate) fn provider_env_keys() -> HashSet<String> {
    crate::status_ops::status_health::model_provider_env_names()
}

/// #1680/D3: the all-class admitted-secret-name surface — lane env injection,
/// providers-doctor admission, and the plaintext secret scanner all need to
/// recognize every provider key class (not just ModelApi), so they must not
/// share `provider_env_keys()`'s narrowed allowlist.
pub(crate) fn admitted_provider_env_keys() -> HashSet<String> {
    crate::status_ops::status_health::admitted_env_secret_names()
}

/// #1680/D3 (codex finding 1, the sharpest catch of the cross-vendor review):
/// the env-name split alone cannot stop a Vault-stored search key from
/// reaching the LLM provider cache, because `resolve_vault_pools` seeds its
/// map from the *entire* loaded Vault pool set — Vault pool loading admits
/// any standalone `*_API_KEY` entry regardless of class
/// (`vault_ops::access::load_unlocked_api_key_secret_pools`). This is the
/// seam that closes the gap: every pool handed to
/// `materialize_provider_secrets_from_durable_source` is filtered down to
/// `ModelApi`-class pool names before it reaches `tachi_llm`. `tachi-llm`'s
/// signature and internals are untouched — the filter lives entirely on the
/// tachi-server side of the existing closure seam.
///
/// codex NEEDS-FIXES BUG-1: a bare `allowed.contains(name)` check missed
/// rotation-member-shaped pool keys. When a `*_API_KEY_N` name has no
/// *configured* rotation row, `group_api_key_values_by_configured_rotations`
/// (tachi-llm) does not fold it under its logical prefix — it stores the pool
/// under the raw member name itself (e.g. `VOYAGE_API_KEY_2`), which never
/// exact-matches `model_provider_env_names()`'s primary/alias names and would
/// be wrongly dropped as if it were unregistered. Fix: reuse the exact same
/// parsed-prefix primitive `doctor::secrets::is_provider_secret_name` already
/// uses for this — `parse_rotation_member_name` — rather than hand-rolling a
/// second parser. A member name is admitted iff its own name OR its parsed
/// prefix is ModelApi-registered, so `VOYAGE_API_KEY_2` (prefix
/// `VOYAGE_API_KEY`, ModelApi) survives while `TAVILY_API_KEY_2` (prefix
/// `TAVILY_API_KEY`, SearchApi) is still rejected.
pub(crate) fn filter_model_provider_pools(
    pools: HashMap<String, Vec<ProviderSecret>>,
) -> HashMap<String, Vec<ProviderSecret>> {
    pools
        .into_iter()
        .filter(|(name, _)| is_model_provider_pool_name(name))
        .collect()
}

pub(crate) fn is_model_provider_pool_name(name: &str) -> bool {
    let allowed = provider_env_keys();
    allowed.contains(name)
        || parse_rotation_member_name(name).is_some_and(|(prefix, _)| allowed.contains(prefix))
}

/// Syntactic API-key names admitted by both unlocked-server and Keychain
/// Vault scans. Registration/class filtering happens after this scan.
pub(crate) fn is_provider_api_key_name(name: &str) -> bool {
    name.ends_with("_API_KEY")
        || parse_rotation_member_name(name).is_some_and(|(prefix, _)| prefix.ends_with("_API_KEY"))
}

struct VaultSourceLoad {
    load: tachi_llm::DurableVaultLoad,
    lane_config_values: LaneConfigValues,
    acl_revision: Option<VaultMaterializationRevision>,
}

fn vault_api_key_pool_load_from_server(server: &MemoryServer) -> Result<VaultSourceLoad, String> {
    let scan = crate::vault_ops::load_validated_unlocked_api_key_secret_pools_with_drops(server)?;
    let source_generation = Some(scan.acl_revision.contents);
    Ok(VaultSourceLoad {
        load: tachi_llm::DurableVaultLoad {
            pools: scan.pools,
            listed_drops: scan.dropped,
            availability: VaultSourceAvailability::Readable,
            source_generation,
        },
        lane_config_values: scan.lane_config_values,
        acl_revision: Some(scan.acl_revision),
    })
}

fn vault_api_key_load_from_keychain(global_db_path: &Path) -> Result<VaultSourceLoad, String> {
    let scan = crate::status_ops::status_health::load_keychain_vault_api_key_scan(global_db_path)
        .map_err(|err| format!("Keychain Vault provider read failed: {err}"))?;
    Ok(durable_load_from_keychain_scan(scan))
}

#[cfg(feature = "vault-test-api")]
fn vault_api_key_load_with_password_for_tests(
    global_db_path: &Path,
    password: Option<&str>,
) -> Result<VaultSourceLoad, String> {
    let scan = crate::status_ops::status_health::load_keychain_vault_api_key_scan_with_reader(
        global_db_path,
        || {
            password
                .map(str::to_owned)
                .ok_or_else(|| "no vault password found in Keychain (test fixture)".to_string())
        },
    )
    .map_err(|err| format!("Keychain Vault provider read failed: {err}"))?;
    Ok(durable_load_from_keychain_scan(scan))
}

fn durable_load_from_keychain_scan(
    scan: crate::status_ops::status_health::KeychainApiKeyScan,
) -> VaultSourceLoad {
    let availability = if scan.source_readable {
        VaultSourceAvailability::Readable
    } else {
        VaultSourceAvailability::LockedOrUnavailable
    };
    let mut pools =
        group_api_key_values_by_configured_rotations(scan.values, &scan.rotation_prefixes);
    // Keep provider health and lease attribution on the actual account in the
    // Keychain path, just as in the unlocked-server path.
    for (slot, account) in scan.slot_accounts {
        if let Some(entries) = pools.get_mut(&slot) {
            for entry in entries {
                entry.key_id = account.clone();
            }
        }
    }
    let mut listed_drops = scan.dropped;
    promote_configured_rotation_prefix_drops(&mut listed_drops, &pools, &scan.rotation_prefixes);
    let source_generation = scan.acl_revision.as_ref().map(|revision| revision.contents);
    VaultSourceLoad {
        load: tachi_llm::DurableVaultLoad {
            pools,
            listed_drops,
            availability,
            source_generation,
        },
        lane_config_values: scan.lane_config_values,
        acl_revision: scan.acl_revision,
    }
}

fn should_use_default_vault_fallback(load: &tachi_llm::DurableVaultLoad) -> bool {
    load.availability == VaultSourceAvailability::Readable
        || !load.pools.is_empty()
        || !load.listed_drops.is_empty()
}

/// Prefix aliases resolve `VOYAGE_API_KEY`, not `VOYAGE_API_KEY_1`. Copy a
/// member drop onto a configured rotation prefix only when that prefix has
/// no admitted pool — unconfigured member names must not invent prefix
/// integrity.
fn promote_configured_rotation_prefix_drops(
    dropped: &mut HashMap<String, AliasSkipClass>,
    pools: &HashMap<String, Vec<ProviderSecret>>,
    rotation_prefixes: &HashSet<String>,
) {
    let mut extra: Vec<(String, usize, String, AliasSkipClass)> = dropped
        .iter()
        .filter_map(|(name, class)| {
            let (prefix, member_index) = parse_rotation_member_name(name)?;
            if rotation_prefixes.contains(prefix) && !pools.contains_key(prefix) {
                Some((prefix.to_string(), member_index, name.clone(), *class))
            } else {
                None
            }
        })
        .collect();
    extra.sort_by(|left, right| (&left.0, &left.1, &left.2).cmp(&(&right.0, &right.1, &right.2)));
    for (prefix, _, _, class) in extra {
        dropped.entry(prefix).or_insert(class);
    }
}

/// Resolve the Vault-backed provider pools, and report whether the source was
/// actually readable.
///
/// The availability half is load-bearing, not diagnostic: materialization uses
/// it to decide whether a missing `vault:` alias target means "revoked" (drop
/// the cached pool) or "cannot tell right now" (retain it). See
/// [`tachi_llm::VaultSourceAvailability`].
struct ResolvedVaultLoad {
    load: tachi_llm::DurableVaultLoad,
    lane_config_values: LaneConfigValues,
    source_path: std::path::PathBuf,
    acl_revision: Option<VaultMaterializationRevision>,
}

fn resolved_vault_load(source: VaultSourceLoad, source_path: &Path) -> ResolvedVaultLoad {
    ResolvedVaultLoad {
        load: source.load,
        lane_config_values: source.lane_config_values,
        source_path: source_path.to_path_buf(),
        acl_revision: source.acl_revision,
    }
}

#[cfg(test)]
fn resolve_vault_pools(
    server: Option<&MemoryServer>,
    global_db_path: &Path,
) -> Result<ResolvedVaultLoad, String> {
    resolve_vault_pools_with_keychain_loader(
        server,
        global_db_path,
        &vault_api_key_load_from_keychain,
    )
}

fn resolve_vault_pools_with_keychain_loader<F>(
    server: Option<&MemoryServer>,
    global_db_path: &Path,
    keychain_loader: &F,
) -> Result<ResolvedVaultLoad, String>
where
    F: Fn(&Path) -> Result<VaultSourceLoad, String>,
{
    // Starts unavailable and is only promoted by a read that actually
    // succeeded: an unproven source must never license retention.
    let mut availability = VaultSourceAvailability::LockedOrUnavailable;
    if let Some(server) = server {
        match vault_api_key_pool_load_from_server(server) {
            Ok(source)
                if !source.load.pools.is_empty()
                    || !source.load.listed_drops.is_empty()
                    || !source.lane_config_values.is_empty() =>
            {
                return Ok(resolved_vault_load(source, global_db_path));
            }
            // Unlocked and genuinely empty: the Vault answered, it just has
            // nothing. That is a readable source.
            Ok(_) => availability = VaultSourceAvailability::Readable,
            // A benign locked miss may still use the standalone Keychain
            // fallback below. Real auth/unknown failures must stay loud.
            //
            // Classification goes through the one shared classifier rather than
            // a local string chain: `vault_ops::resolver`'s module doc records
            // that per-host `starts_with("Vault is locked")` predicates already
            // drifted once. An `==` chain here would drift the same way, and its
            // fail direction is an outage — a reworded message would turn every
            // locked start-up into a refused refresh with no Keychain fallback.
            Err(err)
                if matches!(
                    classify_vault_read_error(&err),
                    VaultReadState::Locked
                        | VaultReadState::AutoLocked
                        | VaultReadState::NotInitialized
                ) => {}
            Err(err) => return Err(format!("Failed to unlock Vault provider secrets: {err}")),
        }
    }
    let keychain = keychain_loader(global_db_path)?;
    if keychain.load.availability == VaultSourceAvailability::Readable {
        return Ok(resolved_vault_load(keychain, global_db_path));
    }
    if !keychain.load.pools.is_empty()
        || !keychain.load.listed_drops.is_empty()
        || !keychain.lane_config_values.is_empty()
    {
        return Ok(resolved_vault_load(keychain, global_db_path));
    }
    if vault_config_exists(global_db_path)? {
        return Ok(resolved_vault_load(
            VaultSourceLoad {
                load: tachi_llm::DurableVaultLoad::from_pools(keychain.load.pools, availability),
                lane_config_values: keychain.lane_config_values,
                acl_revision: None,
            },
            global_db_path,
        ));
    }

    let default_global = default_global_db_path();
    if paths_equal(global_db_path, &default_global) {
        return Ok(resolved_vault_load(
            VaultSourceLoad {
                load: tachi_llm::DurableVaultLoad::from_pools(keychain.load.pools, availability),
                lane_config_values: keychain.lane_config_values,
                acl_revision: None,
            },
            global_db_path,
        ));
    }

    let fallback = keychain_loader(&default_global)?;
    if should_use_default_vault_fallback(&fallback.load) {
        tracing::warn!(
            "[provider] global DB {} has no initialized Vault; using default Vault DB {} for provider key materialization",
            global_db_path.display(),
            default_global.display()
        );
        return Ok(resolved_vault_load(fallback, &default_global));
    }
    if vault_config_exists(&default_global)? {
        return Ok(resolved_vault_load(
            VaultSourceLoad {
                load: tachi_llm::DurableVaultLoad::from_pools(
                    fallback.load.pools,
                    VaultSourceAvailability::LockedOrUnavailable,
                ),
                lane_config_values: fallback.lane_config_values,
                acl_revision: None,
            },
            &default_global,
        ));
    }
    Ok(resolved_vault_load(
        VaultSourceLoad {
            load: tachi_llm::DurableVaultLoad::from_pools(fallback.load.pools, availability),
            lane_config_values: fallback.lane_config_values,
            acl_revision: None,
        },
        &default_global,
    ))
}

pub(crate) fn default_global_db_path() -> std::path::PathBuf {
    crate::status_ops::resolve_app_home()
        .join("global")
        .join(memcore::MEMORY_DB_FILENAME)
}

fn vault_config_exists(global_db_path: &Path) -> Result<bool, String> {
    if !global_db_path.exists() {
        return Ok(false);
    }
    let path = global_db_path
        .to_str()
        .ok_or_else(|| "Vault DB path is not valid UTF-8".to_string())?;
    let store = memcore::MemoryStore::open_read_only(path)
        .map_err(|err| format!("Failed to open Vault DB for provider refresh: {err}"))?;
    store
        .vault_get_config()
        .map(|config| config.is_some())
        .map_err(|err| format!("Failed to read Vault config for provider refresh: {err}"))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .map(|(left, right)| left == right)
        .unwrap_or(false)
}

/// Apply Vault + config.env aliases into `LlmClient` without mutating process env.
#[cfg(test)]
pub fn materialize_provider_secrets(
    llm: &LlmClient,
    vault_pools: &HashMap<String, Vec<ProviderSecret>>,
) -> Result<MaterializeReport, String> {
    tachi_llm::materialize_provider_secrets(llm, vault_pools, provider_env_keys())
        .map_err(format_provider_materialization_error)
}

fn format_provider_materialization_error(err: String) -> String {
    // Match the two unresolved-alias shapes by their semantic tails, not by one
    // shared sentence: #1393 split that sentence into a revoked case and an
    // unreadable case, and a matcher keyed on the old wording would silently
    // stop attaching remediation to both.
    let unresolved_alias = err.starts_with("Config key ")
        && err.contains("references a Vault alias")
        && !err.contains("not a valid Vault secret name");
    let missing_or_unreadable = err.contains("could not be read")
        || err.contains("absent from a readable Vault")
        || err.contains("could not be resolved from secret");
    if ((err.starts_with("provider alias ") && err.contains(" could not be resolved from secret "))
        || unresolved_alias)
        && missing_or_unreadable
    {
        format!(
            "{err} The alias came from config.env or process env. \
             Run vault_unlock if the Vault is locked, or vault_set to store the key \
             in Vault under the referenced secret name."
        )
    } else {
        err
    }
}

/// #1279: per-alias tolerance means a missing `vault:` alias degrades to a
/// `MaterializeReport.skipped_aliases` entry instead of an `Err`, so the raw
/// skip reason from `tachi-llm` no longer flows through
/// `format_provider_materialization_error`. Every consumer that surfaces a skip
/// (server refresh log, bootstrap summary, vault-op response, backfill fail-fast)
/// reattaches the same `vault_unlock`/`vault_set` remediation here — keeping the
/// remediation wording single-sourced in `tachi-server` rather than duplicated
/// per call site or leaked into `tachi-llm`.
pub fn format_skipped_alias_reason(reason: &str) -> String {
    format_provider_materialization_error(reason.to_string())
}

/// Render one skipped alias with its cache disposition. Inputs are metadata
/// only: the logical key name, whether that key's prior pool was retained, and
/// the typed skip class. The warning rebuilds from [`AliasSkipClass`] instead
/// of forwarding report text, so a contaminated caller cannot inject an alias
/// target (tachi#1854: listed-row classes must not print revocation wording).
pub fn format_skipped_alias_warning(key: &str, retained: bool, class: AliasSkipClass) -> String {
    let cache_disposition = if retained {
        "retained last-known-good provider pool"
    } else {
        "no last-known-good provider pool retained"
    };
    format!(
        "[provider] skipped alias for '{key}'; {cache_disposition}: {}",
        format_skipped_alias_reason(&class.operator_reason(key))
    )
}

/// Render `MaterializeReport.skipped_aliases` into one operator-facing message
/// with per-alias remediation. Used by fail-loud consumers (vault-op response,
/// backfill fail-fast) that need a single string; the accessor refresh path logs
/// each alias on its own line instead.
pub fn describe_skipped_aliases(skipped: &[(String, String)]) -> String {
    let details = skipped
        .iter()
        .map(|(_key, reason)| format_skipped_alias_reason(reason))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{} provider alias(es) skipped during materialization: {details}",
        skipped.len()
    )
}

/// Metadata-only aggregate for health/probe output. Reconstruct each entry
/// from the typed skip class, logical key, and retained disposition; never
/// forward raw reasons or alias targets.
pub fn describe_skipped_alias_report(report: &MaterializeReport) -> String {
    let details = report
        .skipped_aliases
        .iter()
        .map(|(key, _reason)| {
            let reason = report.skip_class_for(key).operator_reason(key);
            let disposition = if report
                .retained_from_last_known_good
                .iter()
                .any(|retained_key| retained_key == key)
            {
                "retained last-known-good provider pool"
            } else {
                "no last-known-good provider pool retained"
            };
            format!("{key}: {disposition}; {reason}")
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{} provider alias(es) skipped during materialization: {details}",
        report.skipped_aliases.len()
    )
}

pub fn materialize_for_server(server: &MemoryServer) -> Result<MaterializeReport, String> {
    materialize_for_server_inner(server, None, &vault_api_key_load_from_keychain)
}

fn materialize_for_server_inner(
    server: &MemoryServer,
    after_vault_pools_resolved: Option<Box<dyn FnOnce() + Send>>,
    keychain_loader: &impl Fn(&Path) -> Result<VaultSourceLoad, String>,
) -> Result<MaterializeReport, String> {
    let global = server.global_db_path_buf();
    let lane_config_values = std::cell::RefCell::new(None);
    let publication_fence = std::cell::RefCell::new(None);
    let health_recheck = std::cell::RefCell::new(None);
    let materialize_result =
        tachi_llm::materialize_provider_secrets_from_durable_source_with_snapshot(
            server.llm.as_ref(),
            provider_env_keys(),
            || {
                let resolved = resolve_vault_pools_with_keychain_loader(
                    Some(server),
                    &global,
                    keychain_loader,
                )?;
                let expected_revision = resolved.acl_revision;
                let source_path = resolved.source_path.clone();
                *lane_config_values.borrow_mut() = Some(resolved.lane_config_values);
                let mut load = resolved.load;
                annotate_non_model_drops(&load.pools, &mut load.listed_drops);
                load.pools = filter_model_provider_pools(load.pools);
                if let Some(hook) = after_vault_pools_resolved {
                    hook();
                }
                if source_path.exists() {
                    let fence = memcore::store::vault::VaultMutationFence::acquire(&source_path)
                        .map_err(|error| {
                            format!("Failed to fence Vault provider publication: {error}")
                        })?;
                    if let Some(expected_revision) = expected_revision {
                        *health_recheck.borrow_mut() =
                            fenced_provider_health_recheck(fence.connection(), expected_revision)?;
                    }
                    *publication_fence.borrow_mut() = Some(fence);
                } else if expected_revision.is_some() {
                    return Err(format!(
                        "Vault source {} disappeared before provider publication",
                        source_path.display()
                    ));
                }
                Ok(load)
            },
            |provider_snapshot| {
                validate_admitted_provider_health(
                    &provider_snapshot,
                    health_recheck.borrow().as_deref(),
                )?;
                let values = lane_config_values
                    .borrow_mut()
                    .take()
                    .ok_or_else(|| "Vault lane-config snapshot was not captured".to_string())?;
                let snapshot = prepare_vault_runtime_snapshot(server, provider_snapshot, values)?;
                if snapshot.source_availability != snapshot.provider.report().source_availability {
                    return Err(
                        "Vault runtime snapshot source availability changed during preparation"
                            .to_string(),
                    );
                }
                Ok(snapshot.into_provider_publication())
            },
            |catalog| {
                let Some(fence) = publication_fence.borrow_mut().take() else {
                    return commit_env_catalog_projection(server, &catalog).map(|_| ());
                };
                write_env_catalog_projection(fence.connection(), &catalog)?;
                fence
                    .commit()
                    .map_err(|error| format!("Failed to commit fenced Vault publication: {error}"))
            },
        );
    match materialize_result {
        Ok(report) => Ok(report),
        Err(error) => Err(format_provider_materialization_error(error)),
    }
}

fn annotate_non_model_drops(
    pools: &HashMap<String, Vec<ProviderSecret>>,
    drops: &mut HashMap<String, tachi_llm::AliasSkipClass>,
) {
    let allowed = provider_env_keys();
    for (name, members) in pools {
        let admitted = allowed.contains(name)
            || parse_rotation_member_name(name).is_some_and(|(prefix, _)| allowed.contains(prefix));
        if !admitted {
            drops
                .entry(name.clone())
                .or_insert(tachi_llm::AliasSkipClass::ListedNotModelProvider);
            for member in members {
                drops
                    .entry(member.key_id.clone())
                    .or_insert(tachi_llm::AliasSkipClass::ListedNotModelProvider);
            }
        }
    }
}

/// One complete, validated runtime projection held between provider-pool
/// preparation and publication. The provider snapshot owns the resolved pools;
/// this companion owns the source availability, lane overlay, and catalog
/// projection derived from the same effective runtime config.
struct VaultRuntimeSnapshot {
    provider: tachi_llm::ProviderMaterializationSnapshot,
    source_availability: VaultSourceAvailability,
    lane_config_overlay: Option<LaneConfigOverlay>,
    catalog: EnvCatalogProjection,
}

impl VaultRuntimeSnapshot {
    fn into_provider_publication(
        self,
    ) -> (
        tachi_llm::ProviderMaterializationSnapshot,
        Option<LaneConfigOverlay>,
        EnvCatalogProjection,
    ) {
        (self.provider, self.lane_config_overlay, self.catalog)
    }
}

fn prepare_vault_runtime_snapshot(
    server: &MemoryServer,
    provider: tachi_llm::ProviderMaterializationSnapshot,
    lane_config_values: LaneConfigValues,
) -> Result<VaultRuntimeSnapshot, String> {
    let source_availability = provider.report().source_availability;
    let lane_config_overlay = if source_availability == VaultSourceAvailability::Readable {
        Some(lane_config_overlay_from_values(lane_config_values)?)
    } else {
        // An unreadable source cannot prove revocation or a replacement. `None`
        // tells the publisher to retain the already-published overlay.
        None
    };
    let effective_config = match lane_config_overlay.as_ref() {
        Some(overlay) => provider.validated_runtime_config(server.llm.as_ref(), overlay)?,
        None => server.llm.runtime_config(),
    };
    let catalog = prepare_env_catalog_projection(&effective_config)?;
    Ok(VaultRuntimeSnapshot {
        provider,
        source_availability,
        lane_config_overlay,
        catalog,
    })
}

fn lane_config_overlay_from_values(values: LaneConfigValues) -> Result<LaneConfigOverlay, String> {
    let mut map = std::collections::HashMap::new();
    for (name, value) in values.into_values() {
        if let Some(mut replaced) = map.insert(name, value) {
            crate::vault_crypto::zero_string(&mut replaced);
        }
    }
    let mut overlay = LaneConfigOverlay::default();
    let result = (|| {
        fill_lane_overlay(&mut overlay.extract, "EXTRACT", &mut map)?;
        fill_lane_overlay(&mut overlay.summary, "SUMMARY", &mut map)?;
        fill_lane_overlay(&mut overlay.distill, "DISTILL", &mut map)?;
        fill_lane_overlay(&mut overlay.reasoning, "REASONING", &mut map)?;
        Ok(())
    })();
    for value in map.values_mut() {
        crate::vault_crypto::zero_string(value);
    }
    match result {
        Ok(()) => Ok(overlay),
        Err(error) => {
            zero_lane_config_overlay(&mut overlay);
            Err(error)
        }
    }
}

fn zero_lane_config_overlay(overlay: &mut LaneConfigOverlay) {
    for fields in [
        &mut overlay.extract,
        &mut overlay.summary,
        &mut overlay.distill,
        &mut overlay.reasoning,
    ] {
        if let Some(value) = fields.base_url.as_mut() {
            crate::vault_crypto::zero_string(value);
        }
        if let Some(value) = fields.model.as_mut() {
            crate::vault_crypto::zero_string(value);
        }
    }
}

fn fill_lane_overlay(
    fields: &mut LaneFieldOverlay,
    prefix: &str,
    vault: &mut std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let url_name = format!("{prefix}_BASE_URL");
    let model_name = format!("{prefix}_MODEL");
    if let Some(url) = vault.remove(&url_name) {
        warn_if_env_conflicts(&url_name, &url);
        fields.base_url = Some(validate_vault_lane_url(&url_name, url)?);
    }
    if let Some(model) = vault.remove(&model_name) {
        warn_if_env_conflicts(&model_name, &model);
        fields.model = Some(validate_vault_lane_model(&model_name, model)?);
    }
    Ok(())
}

fn validate_vault_lane_url(name: &str, mut value: String) -> Result<String, String> {
    let trimmed = value.trim().to_string();
    crate::vault_crypto::zero_string(&mut value);
    value = trimmed;
    let result = (|| {
        // Reject credential-shaped material before handing the string to a URL
        // parser that may allocate an additional copy of userinfo/query data.
        if let Some(leak) = memcore::catalog::endpoint::endpoint_credential_leak(&value) {
            return Err(format!(
                "Vault lane config '{name}' refused before publication: {leak}; prior runtime state left unchanged"
            ));
        }
        let url = reqwest::Url::parse(&value).map_err(|_| {
            format!(
                "Vault lane config '{name}' has a malformed URL; provider refresh refused and prior runtime state left unchanged"
            )
        })?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(format!(
                "Vault lane config '{name}' must use an HTTP(S) URL with a host; provider refresh refused and prior runtime state left unchanged"
            ));
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(value),
        Err(error) => {
            crate::vault_crypto::zero_string(&mut value);
            Err(error)
        }
    }
}

fn validate_vault_lane_model(name: &str, mut value: String) -> Result<String, String> {
    let trimmed = value.trim().to_string();
    crate::vault_crypto::zero_string(&mut value);
    value = trimmed;
    if value.is_empty() || value.chars().any(char::is_control) {
        crate::vault_crypto::zero_string(&mut value);
        return Err(format!(
            "Vault lane config '{name}' has an invalid model; provider refresh refused and prior runtime state left unchanged"
        ));
    }
    Ok(value)
}

fn warn_if_env_conflicts(name: &str, vault_value: &str) {
    let Ok(env_val) = std::env::var(name) else {
        return;
    };
    let trimmed = env_val.trim();
    if trimmed.is_empty() || is_vault_alias(trimmed) {
        return;
    }
    if trimmed != vault_value {
        tracing::warn!(
            "[provider] env/config.env value ignored for {name} — vault wins; if your env value is fresher: tachi vault set {name}"
        );
    }
}

#[cfg(any(test, feature = "vault-test-api"))]
pub fn materialize_for_server_with_hook_for_tests(
    server: &MemoryServer,
    after_vault_pools_resolved: impl FnOnce() + Send + 'static,
) -> Result<MaterializeReport, String> {
    materialize_for_server_inner(
        server,
        Some(Box::new(after_vault_pools_resolved)),
        &vault_api_key_load_from_keychain,
    )
}

/// Explicit missing-Keychain fixture; ordinary server calls keep the real reader.
#[cfg(feature = "vault-test-api")]
pub(crate) fn materialize_for_server_without_keychain_for_tests(
    server: &MemoryServer,
) -> Result<MaterializeReport, String> {
    materialize_for_server_inner(server, None, &|path| {
        vault_api_key_load_with_password_for_tests(path, None)
    })
}

pub fn materialize_standalone(
    llm: &LlmClient,
    global_db_path: &Path,
) -> Result<MaterializeReport, String> {
    materialize_standalone_inner(llm, global_db_path, None, &vault_api_key_load_from_keychain)
}

/// Exercise durable custody with an explicit password, never a process-wide override.
#[cfg(feature = "vault-test-api")]
pub(crate) fn materialize_standalone_with_password_for_tests(
    llm: &LlmClient,
    global_db_path: &Path,
    password: &str,
    after_vault_pools_resolved: Option<Box<dyn FnOnce() + Send>>,
) -> Result<MaterializeReport, String> {
    materialize_standalone_inner(llm, global_db_path, after_vault_pools_resolved, &|path| {
        vault_api_key_load_with_password_for_tests(path, Some(password))
    })
}

fn materialize_standalone_inner(
    llm: &LlmClient,
    global_db_path: &Path,
    after_vault_pools_resolved: Option<Box<dyn FnOnce() + Send>>,
    keychain_loader: &impl Fn(&Path) -> Result<VaultSourceLoad, String>,
) -> Result<MaterializeReport, String> {
    let lane_config_values = std::cell::RefCell::new(None);
    let publication_fence = std::cell::RefCell::new(None);
    let health_recheck = std::cell::RefCell::new(None);
    tachi_llm::materialize_provider_secrets_from_durable_source_with_snapshot(
        llm,
        provider_env_keys(),
        || {
            let resolved =
                resolve_vault_pools_with_keychain_loader(None, global_db_path, keychain_loader)?;
            let expected_revision = resolved.acl_revision;
            let source_path = resolved.source_path.clone();
            *lane_config_values.borrow_mut() = Some(resolved.lane_config_values);
            let mut load = resolved.load;
            annotate_non_model_drops(&load.pools, &mut load.listed_drops);
            load.pools = filter_model_provider_pools(load.pools);
            if let Some(hook) = after_vault_pools_resolved {
                hook();
            }
            if source_path.exists() {
                let fence = memcore::store::vault::VaultMutationFence::acquire(&source_path)
                    .map_err(|error| {
                        format!("Failed to fence standalone Vault publication: {error}")
                    })?;
                if let Some(expected_revision) = expected_revision {
                    *health_recheck.borrow_mut() =
                        fenced_provider_health_recheck(fence.connection(), expected_revision)?;
                }
                *publication_fence.borrow_mut() = Some(fence);
            } else if expected_revision.is_some() {
                return Err(format!(
                    "Vault source {} disappeared before standalone publication",
                    source_path.display()
                ));
            }
            Ok(load)
        },
        |provider_snapshot| {
            validate_admitted_provider_health(
                &provider_snapshot,
                health_recheck.borrow().as_deref(),
            )?;
            if provider_snapshot.report().source_availability != VaultSourceAvailability::Readable {
                return Ok((provider_snapshot, None, ()));
            }
            let values = lane_config_values
                .borrow_mut()
                .take()
                .ok_or_else(|| "Vault lane-config snapshot was not captured".to_string())?;
            let overlay = lane_config_overlay_from_values(values)?;
            provider_snapshot.validated_runtime_config(llm, &overlay)?;
            Ok((provider_snapshot, Some(overlay), ()))
        },
        |()| {
            let Some(fence) = publication_fence.borrow_mut().take() else {
                return Ok(());
            };
            fence
                .commit()
                .map_err(|error| format!("Failed to commit standalone Vault fence: {error}"))
        },
    )
    .map_err(format_provider_materialization_error)
}

#[cfg(test)]
pub(crate) fn materialize_standalone_with_hook_for_tests(
    llm: &LlmClient,
    global_db_path: &Path,
    after_vault_pools_resolved: impl FnOnce() + Send + 'static,
) -> Result<MaterializeReport, String> {
    materialize_standalone_inner(
        llm,
        global_db_path,
        Some(Box::new(after_vault_pools_resolved)),
        &vault_api_key_load_from_keychain,
    )
}

/// Check whether the macOS Keychain contains the background auto-unlock entry
/// (`tachi-vault` / `default`) without reading its value.
pub fn keychain_vault_password_entry_available() -> Result<bool, String> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }

    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "tachi-vault",
            "-a",
            "default",
        ])
        .output()
        .map_err(|e| format!("keychain status check failed: {e}"))?;
    Ok(output.status.success())
}

fn read_background_keychain_password() -> Result<Option<String>, String> {
    match crate::vault_crypto::read_password_from_macos_keychain() {
        Ok(password) => Ok(Some(password)),
        Err(err)
            if err.starts_with("no vault password found in Keychain")
                || err == "Keychain entry for tachi-vault/default is empty" =>
        {
            Ok(None)
        }
        Err(err) => Err(format!("keychain read failed: {err}")),
    }
}

/// Install and validate the Vault key from Keychain without refreshing the
/// provider cache. Callers must explicitly own the following refresh, if any.
pub(crate) fn auto_unlock_vault_key_from_keychain(server: &MemoryServer) -> Result<bool, String> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    #[cfg(test)]
    if std::env::var_os("TACHI_TEST_ALLOW_KEYCHAIN_AUTO_UNLOCK").is_none() {
        return Ok(false);
    }

    let Some(config) = server
        .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))?
    else {
        return Ok(false);
    };

    let Some(mut password) = read_background_keychain_password()? else {
        return Ok(false);
    };

    // tachi#1080: derive+verify through the shared in-process seam so a stored
    // kdf_params failure stays loud and typed (never misread as "wrong
    // password", never silently unlocks with a compile-time-derived key).
    let key_result =
        crate::vault_crypto::derive_verified_key_from_stored_config(&config, &password)
            .map_err(|e| e.to_string());
    crate::vault_crypto::zero_string(&mut password);
    let key = key_result?;

    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(key.bytes()));
        v.unlock_time = Some(std::time::Instant::now());
    }

    Ok(true)
}

/// Auto-unlock from Keychain and refresh provider secrets exactly once.
///
/// Use [`auto_unlock_vault_key_from_keychain`] instead when an outer provider
/// materialization transaction already owns the refresh.
pub fn auto_unlock_vault_from_keychain(server: &MemoryServer) -> Result<bool, String> {
    if !auto_unlock_vault_key_from_keychain(server)? {
        return Ok(false);
    }
    let loaded = server.refresh_llm_provider_secrets_from_vault()?.loaded;
    tracing::info!("[vault] auto-unlocked from Keychain ({loaded} provider key(s))");
    server.requeue_auth_failed_enrichment_retries("Keychain auto-unlock");
    Ok(true)
}

/// Keychain auto-unlock (best effort) + provider secret materialization for any
/// short-lived server instance (CLI one-shots, MCP stdio, daemon startup).
pub fn bootstrap_provider_runtime(server: &MemoryServer) {
    let auto_unlocked = match auto_unlock_vault_key_from_keychain(server) {
        Ok(true) => {
            tracing::info!("[vault] auto-unlocked from Keychain");
            true
        }
        Ok(false) => {
            tracing::debug!(
                "[vault] auto-unlock skipped (no keychain entry or vault not initialized)"
            );
            false
        }
        Err(err) => {
            tracing::warn!("[vault] auto-unlock skipped: {err}");
            false
        }
    };
    let refresh = server.refresh_llm_provider_secrets_from_vault();
    if auto_unlocked && refresh.is_ok() {
        server.requeue_auth_failed_enrichment_retries("Keychain auto-unlock");
    }
    match refresh {
        // #1279: name the specific bad aliases at the bootstrap surface instead of
        // the old generic "Vault locked or empty". The full per-alias remediation is
        // logged by `refresh_llm_provider_secrets_from_vault`; this summary lists the
        // alias keys and points operators at those warnings.
        Ok(report) if !report.skipped_aliases.is_empty() => {
            let keys = report
                .skipped_aliases
                .iter()
                .map(|(key, _reason)| key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            tracing::warn!(
                "[provider] {} provider key(s) ready, {} alias(es) skipped ({keys}); see '[provider] skipped alias' warnings for per-alias remediation",
                report.loaded,
                report.skipped_aliases.len()
            );
        }
        Ok(report) if report.loaded > 0 => {
            tracing::info!(
                "[provider] {} provider key(s) ready for LLM/embed",
                report.loaded
            )
        }
        Ok(_) => {
            tracing::debug!("[provider] no provider keys materialized (Vault locked or empty)")
        }
        Err(err) => tracing::warn!("[provider] secret materialization failed: {err}"),
    }
}

/// What one env→catalog import actually did. Counts only — the caller logs
/// this at boot, and a deployment row's contents are not boot-log material.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct EnvCatalogImport {
    /// Rows imported: the four chat lanes, plus the embedding lane when its
    /// configuration resolved.
    pub rows: usize,
    /// How many of those were a create or a revision advance. Zero is the
    /// steady state — a restart that resolves the same chains is not a catalog
    /// change and must append nothing.
    pub changed: usize,
    /// Set when `EmbeddingConfig::from_env()` refused, so the embedding row
    /// was deliberately not written. Reported, never silently swallowed: the
    /// catalog must not claim an embedding deployment the process refused to
    /// configure.
    pub embedding_refused: Option<String>,
}

/// Pure, prevalidated env-catalog state. The writer below accepts only this
/// projection, so no URL/model refusal can occur after the first row write.
#[derive(Debug, Clone)]
struct EnvCatalogProjection {
    chat_deployments: Vec<tachi_llm::EnvLaneDeployment>,
    embedding_deployment: Option<tachi_llm::EnvLaneDeployment>,
    embedding_refused: Option<String>,
}

/// Import the live provider resolution into the catalog as
/// `catalog_source='env'` deployment rows (#1681 D7 PR-B).
///
/// This is the production caller the projection was written for. Without it,
/// `catalog_import` would be a function only tests call and the status catalog
/// would describe nothing the running process configured.
///
/// # Which config
///
/// The chat lanes come from [`tachi_llm::LlmClient::runtime_config`] — the
/// config *this server's client is running on* — not a fresh
/// `ProviderRuntimeConfig::from_env()`. Re-reading env here would make the
/// catalog describe an environment rather than a process, and the two diverge
/// exactly where it matters (any client built from an injected config).
///
/// The embedding lane is the opposite case and is read from env on purpose:
/// `LlmClient::embed_batch` resolves `EmbeddingConfig::from_env()` per call, so
/// there is no cached copy for the catalog to disagree with — env *is* the live
/// resolution for that lane.
///
/// # Idempotence
///
/// `upsert_model_deployment` compares content digests, so a restart that
/// resolves the same chains reports `Unchanged` for every row and appends no
/// events. One transaction wraps the whole write so a mid-way store failure
/// cannot leave a half-described catalog.
///
/// # Refusal precedes every write, not just the ones after it
///
/// Every lane's projection — the four chat lanes *and* the embedding lane —
/// is built before any connection is opened. `env_chat_lane_deployments` and
/// `env_embedding_deployment` are pure functions with no store access; their
/// only failure mode is a credential-bearing endpoint
/// (`CatalogImportError::EndpointCarriesCredential`, which covers userinfo in
/// the authority and credential-shaped query keys alike). Building all five here,
/// before `with_global_store` is even called, means a userinfo-carrying
/// `VOYAGE_BASE_URL` is refused with zero write calls having happened at
/// all — not "refused after the four chat rows were written into a
/// transaction that then rolled back." A prior revision built the embedding
/// projection *inside* the transaction, after the chat lanes had already
/// been upserted into it; correctness leaned on `unchecked_transaction`'s
/// rollback-on-drop to erase those writes, which is invisible from the
/// caller's `Result` but not equivalent to the writes never having been
/// issued.
pub(crate) fn import_env_catalog_deployments(
    server: &MemoryServer,
) -> Result<EnvCatalogImport, String> {
    let projection = prepare_env_catalog_projection(&server.llm.runtime_config())?;
    commit_env_catalog_projection(server, &projection)
}

fn prepare_env_catalog_projection(
    config: &tachi_llm::ProviderRuntimeConfig,
) -> Result<EnvCatalogProjection, String> {
    let embedding = tachi_llm::EmbeddingConfig::from_env();
    let embeddings_endpoint = tachi_llm::voyage_embeddings_endpoint();
    let observed_at = memcore::db::now_utc_iso();

    let chat_deployments = tachi_llm::env_chat_lane_deployments(config, &observed_at)
        .map_err(|err| err.to_string())?;
    let (embedding_deployment, embedding_refused) = match &embedding {
        Ok(resolved) => (
            Some(
                tachi_llm::env_embedding_deployment(resolved, &embeddings_endpoint, &observed_at)
                    .map_err(|err| err.to_string())?,
            ),
            None,
        ),
        Err(err) => (None, Some(err.clone())),
    };

    Ok(EnvCatalogProjection {
        chat_deployments,
        embedding_deployment,
        embedding_refused,
    })
}

fn commit_env_catalog_projection(
    server: &MemoryServer,
    projection: &EnvCatalogProjection,
) -> Result<EnvCatalogImport, String> {
    server.with_global_store(|store| {
        let conn = store.connection();
        let transaction = conn
            .unchecked_transaction()
            .map_err(|e| format!("open catalog import transaction: {e}"))?;
        let summary = write_env_catalog_projection(&transaction, projection)?;
        transaction
            .commit()
            .map_err(|e| format!("commit catalog import: {e}"))?;
        Ok(summary)
    })
}

fn write_env_catalog_projection(
    connection: &rusqlite::Connection,
    projection: &EnvCatalogProjection,
) -> Result<EnvCatalogImport, String> {
    use memcore::db::model_catalog::{upsert_model_deployment, DeploymentWrite};

    let mut summary = EnvCatalogImport {
        embedding_refused: projection.embedding_refused.clone(),
        ..EnvCatalogImport::default()
    };
    for lane in &projection.chat_deployments {
        let write =
            upsert_model_deployment(connection, &lane.deployment).map_err(|e| e.to_string())?;
        summary.rows += 1;
        if !matches!(write, DeploymentWrite::Unchanged { .. }) {
            summary.changed += 1;
        }
    }
    if let Some(row) = &projection.embedding_deployment {
        let write =
            upsert_model_deployment(connection, &row.deployment).map_err(|e| e.to_string())?;
        summary.rows += 1;
        if !matches!(write, DeploymentWrite::Unchanged { .. }) {
            summary.changed += 1;
        }
    }
    Ok(summary)
}

/// Parse `~/.tachi/config.env` (and peers) into raw key/value claims. Raw keys
/// and duplicate claims are preserved so security surfaces can detect names
/// that collide only after normalization.
///
/// `resolved_home`, when given, is unioned into the scan locations alongside
/// every existing one — additive only, nothing below is removed. #1096
/// leaf-2a round-2 (codex B4-status): this scan set used to miss a
/// `SIGIL_HOME`-only (or `TACHI_APP_HOME`-only) deployment's home entirely —
/// `status`'s manifest/probe side reads the server's frozen, funnel-resolved
/// home (`MemoryServer::tachi_home_dir()`), but this function only ever
/// looked at `~/.tachi`, `~/.sigil`, a bare `TACHI_HOME` env re-read, and cwd,
/// so the same `status` snapshot's manifest side and provider-key side could
/// resolve to two different homes. Passing `Some(server.tachi_home_dir())`
/// from the `status` call site closes that gap without touching the other
/// call sites (which keep passing `None` and are byte-for-byte unchanged).
pub(crate) fn collect_config_env_claims(resolved_home: Option<&Path>) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".tachi").join("config.env"));
        paths.push(home.join(".sigil").join("config.env"));
    }
    if let Ok(home) = std::env::var("TACHI_HOME") {
        paths.push(std::path::PathBuf::from(home).join("config.env"));
    }
    paths.push(std::path::PathBuf::from(".tachi/config.env"));
    paths.push(std::path::PathBuf::from(".sigil/config.env"));
    if let Some(home) = resolved_home {
        paths.push(home.join("config.env"));
    }

    let mut values = Vec::new();
    for path in paths {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.to_string();
                let value = value.trim().to_string();
                if !key.trim().is_empty() && !value.is_empty() {
                    values.push((key, value));
                }
            }
        }
    }
    values
}

/// Effective key → value view used by legacy consumers. Exact normalized keys
/// keep the existing last-claim-wins behavior; callers that need collision
/// evidence use [`collect_config_env_claims`].
pub fn collect_config_env_values(resolved_home: Option<&Path>) -> HashMap<String, String> {
    collect_config_env_claims(resolved_home)
        .into_iter()
        .map(|(key, value)| (key.trim().to_string(), value))
        .collect()
}

#[cfg(test)]
mod catalog_import_tests {
    use super::*;
    use crate::test_support::EnvRestore;
    use memcore::catalog::CatalogSource;
    use memcore::db::model_catalog::{
        list_all_model_deployment_events, list_model_deployments_by_source,
    };
    use tachi_llm::llm::{ChatLaneConfig, ProviderRuntimeConfig};
    use tachi_llm::{RerankConfig, RerankProviderKind};

    /// Four deliberately distinct lanes, none of which any env chain would
    /// resolve to. Distinctness is what lets the assertions below tell "the
    /// import projected the client" from "the import re-read the environment".
    fn injected_config() -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            extract: ChatLaneConfig {
                base_url: "https://injected-extract.test/v1/chat/completions".to_string(),
                model: "injected/extract-model".to_string(),
                api_key_envs: vec!["EXTRACT_API_KEY", "SILICONFLOW_API_KEY"],
            },
            summary: ChatLaneConfig {
                base_url: "https://injected-summary.test/v1/chat/completions".to_string(),
                model: "injected/summary-model".to_string(),
                api_key_envs: vec!["SUMMARY_API_KEY"],
            },
            reasoning: ChatLaneConfig {
                base_url: "https://injected-reasoning.test/chat/completions".to_string(),
                model: "injected/reasoning-model".to_string(),
                api_key_envs: vec!["DEEPSEEK_API_KEY"],
            },
            distill: ChatLaneConfig {
                base_url: "https://injected-distill.test/chat/completions".to_string(),
                model: "injected/distill-model".to_string(),
                api_key_envs: vec!["DISTILL_API_KEY"],
            },
            rerank: RerankConfig {
                provider: RerankProviderKind::Voyage,
                local_endpoint: None,
            },
        }
    }

    /// Pin the embedding lane to its built-in default so the row count below
    /// is a statement about this import, not about whatever the machine
    /// running the suite happens to export.
    fn default_embedding_env() -> (EnvRestore, EnvRestore) {
        (
            EnvRestore::remove(tachi_llm::EMBEDDING_MODEL_ENV),
            EnvRestore::remove(tachi_llm::EMBEDDING_DIMENSION_ENV),
        )
    }

    fn server_running_injected_config() -> crate::tests::TestServer {
        let mut server = crate::tests::make_server();
        server.replace_llm(
            tachi_llm::LlmClient::new_with_config(injected_config(), None)
                .expect("injected-config client"),
        );
        server
    }

    /// The production caller exists and reaches every lane rather than leaving
    /// the projection as a function only tests call.
    #[test]
    fn the_serve_path_import_records_every_lane_including_embedding() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _embedding_env = default_embedding_env();
        let server = server_running_injected_config();

        let summary =
            import_env_catalog_deployments(&server).expect("import runs against a live server");
        assert_eq!(
            summary.embedding_refused, None,
            "the default embedding configuration must resolve"
        );
        assert_eq!(summary.rows, 5, "four chat lanes plus the embedding lane");
        assert_eq!(summary.changed, 5, "a first import creates every row");

        let stored = server
            .with_global_store_read(|store| {
                list_model_deployments_by_source(store.connection(), CatalogSource::Env)
                    .map_err(|e| e.to_string())
            })
            .expect("rows read");
        let mut ids: Vec<&str> = stored
            .iter()
            .map(|row| row.deployment_id.as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![
                "env:distill",
                "env:embedding",
                "env:extract",
                "env:reasoning",
                "env:summary"
            ],
            "the embedding lane is not optional — it carries the dimension declaration"
        );
    }

    /// A daemon restart is not a catalog change. Without this, every boot
    /// appends five rows of audit noise forever.
    #[test]
    fn a_restart_that_resolves_the_same_chains_appends_nothing() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _embedding_env = default_embedding_env();
        let server = server_running_injected_config();

        import_env_catalog_deployments(&server).expect("first boot");
        let events_after_first = server
            .with_global_store_read(|store| {
                list_all_model_deployment_events(store.connection()).map_err(|e| e.to_string())
            })
            .expect("events read")
            .len();

        let second = import_env_catalog_deployments(&server).expect("second boot");
        assert_eq!(second.rows, 5, "the same five rows are still described");
        assert_eq!(
            second.changed, 0,
            "re-resolving the same chains must report no change"
        );

        let events_after_second = server
            .with_global_store_read(|store| {
                list_all_model_deployment_events(store.connection()).map_err(|e| e.to_string())
            })
            .expect("events read")
            .len();
        assert_eq!(
            events_after_first, events_after_second,
            "an unchanged re-import must append no events"
        );
    }

    /// The import describes the process, not the process's environment. A
    /// server whose client was built from an injected config must produce rows
    /// for *that* config even while the ambient env says something else —
    /// otherwise the catalog silently describes somebody else's resolution.
    #[test]
    fn the_import_projects_the_running_client_not_the_ambient_environment() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _extract_model = EnvRestore::set("EXTRACT_MODEL", "__ambient-must-not-be-imported");
        let _extract_base = EnvRestore::set("EXTRACT_BASE_URL", "https://ambient.test/v1");
        let server = server_running_injected_config();

        import_env_catalog_deployments(&server).expect("import");

        let stored = server
            .with_global_store_read(|store| {
                list_model_deployments_by_source(store.connection(), CatalogSource::Env)
                    .map_err(|e| e.to_string())
            })
            .expect("rows read");
        let extract = stored
            .iter()
            .find(|row| row.deployment_id == "env:extract")
            .expect("extract row");
        assert_eq!(
            extract.provider_model_id, "injected/extract-model",
            "the catalog must carry the running client's model"
        );
        assert_eq!(
            extract.endpoint_ref.as_deref(),
            Some("https://injected-extract.test/v1/chat/completions"),
            "and the running client's endpoint"
        );
    }

    /// A refusable chat config cannot become the server's running client.
    /// Refusal at client construction is stronger than the catalog import's
    /// later defense-in-depth check: no poisoned client exists to install.
    #[test]
    fn a_userinfo_chat_endpoint_is_refused_before_client_construction() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut config = injected_config();
        config.extract.base_url =
            "https://svc-account:sk-live-SECRET@proxy.internal:8443/v1/chat".to_string();
        let err = match tachi_llm::LlmClient::new_with_config(config, None) {
            Ok(_) => panic!("a userinfo base URL must be refused before client construction"),
            Err(err) => err,
        };
        assert!(!err.contains("sk-live-SECRET"), "{err}");
        assert!(!err.contains("proxy.internal"), "{err}");
        assert!(
            err.contains("extract"),
            "the refusal must name the lane: {err}"
        );
    }

    /// The mirror of the test above, with the credential on the *embedding*
    /// endpoint instead of a chat lane: the four clean chat lanes must not
    /// land either, and — the part a bare "is the store empty" assertion
    /// cannot tell apart from "wrote then rolled back" — no event may have
    /// been appended along the way. `upsert_model_deployment` appends an
    /// event on every create/change and only on those; if the four chat rows
    /// had actually been upserted into the transaction (as a prior revision
    /// did, before the embedding endpoint's userinfo check ran), each would
    /// append a `Create` event to the same in-transaction event log this read
    /// inspects after rollback. Rollback erases the *rows*; it does not
    /// retroactively make the write calls not have happened. Zero events is
    /// therefore evidence about the write path, not just about final state.
    #[test]
    fn a_userinfo_embedding_endpoint_writes_no_chat_rows_or_events_either() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _embedding_env = default_embedding_env();
        let _voyage_base = EnvRestore::set(
            "VOYAGE_BASE_URL",
            "https://svc-account:sk-live-SECRET@voyage.internal/v1",
        );
        let server = server_running_injected_config();

        let err = import_env_catalog_deployments(&server)
            .expect_err("a userinfo embedding endpoint must refuse the whole import");
        assert!(!err.contains("sk-live-SECRET"), "{err}");
        assert!(
            err.contains("embedding"),
            "the refusal must name the embedding lane: {err}"
        );

        let stored = server
            .with_global_store_read(|store| {
                list_model_deployments_by_source(store.connection(), CatalogSource::Env)
                    .map_err(|e| e.to_string())
            })
            .expect("rows read");
        assert!(
            stored.is_empty(),
            "the four clean chat lanes must not land either: {} row(s)",
            stored.len()
        );

        let events = server
            .with_global_store_read(|store| {
                list_all_model_deployment_events(store.connection()).map_err(|e| e.to_string())
            })
            .expect("events read");
        assert!(
            events.is_empty(),
            "no chat-lane upsert may have run at all, so none may have appended an \
             event: {} event(s)",
            events.len()
        );
    }

    /// The strongest form of the claim: the embedding refusal must be
    /// decidable with **no connection in scope at all**, not merely "decided
    /// early in a transaction that then gets rolled back". This test builds
    /// no `MemoryServer`, opens no store, and calls exactly the two pure
    /// projections `import_env_catalog_deployments` calls before it ever asks
    /// for one — `env_chat_lane_deployments` (which takes no connection) and
    /// `env_embedding_deployment` (likewise). Getting the refusal back here,
    /// with literally nothing to write to in scope, is proof by construction
    /// that no write is reachable before this check runs — a property a
    /// black-box "is the store empty afterward" assertion cannot distinguish
    /// from "wrote, then a transaction rolled the writes back."
    #[test]
    fn the_embedding_refusal_is_decidable_with_no_connection_in_scope() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _embedding_env = default_embedding_env();

        let config = injected_config();
        let observed_at = memcore::db::now_utc_iso();

        let chat_deployments = tachi_llm::env_chat_lane_deployments(&config, &observed_at)
            .expect("the four injected chat lanes carry no userinfo");
        assert_eq!(chat_deployments.len(), 4, "all four lanes projected");

        let embedding = tachi_llm::EmbeddingConfig::from_env()
            .expect("the default embedding configuration must resolve");
        let poisoned_endpoint = "https://svc-account:sk-live-SECRET@voyage.internal/v1";
        let err = tachi_llm::env_embedding_deployment(&embedding, poisoned_endpoint, &observed_at)
            .expect_err("a userinfo embedding endpoint must refuse");
        assert!(!err.to_string().contains("sk-live-SECRET"), "{err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvRestore;

    mod slot_keychain;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn standalone_keychain_publication_refuses_acl_revision_drift() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _password = EnvRestore::set("TACHI_TEST_KEYCHAIN_PASSWORD", "standalone-password");
        let _alias = EnvRestore::set("VOYAGE_API_KEY", "vault:VOYAGE_API_KEY");
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("memory.db");
        let server = MemoryServer::new(db_path.clone(), None).expect("server");
        crate::vault_ops::handle_vault_init(
            &server,
            crate::vault_ops::VaultInitParams {
                password: "standalone-password".to_string(),
            },
        )
        .await
        .expect("init vault");
        crate::vault_ops::handle_vault_set(
            &server,
            crate::vault_ops::VaultSetParams {
                name: "VOYAGE_API_KEY".to_string(),
                value: "stale-secret".to_string(),
                agent_id: None,
                secret_type: "api_key".to_string(),
                description: String::new(),
                allowed_agents: None,
                enable_rotation: false,
                rotation_strategy: None,
                rebind: false,
            },
        )
        .await
        .expect("seed secret");
        drop(server);

        let llm = LlmClient::new().expect("llm client");
        let writer_path = db_path.clone();
        let error = materialize_standalone_with_hook_for_tests(&llm, &db_path, move || {
            let store =
                memcore::MemoryStore::open(writer_path.to_str().expect("UTF-8 writer path"))
                    .expect("open writer");
            let mut entry = store
                .vault_get_entry("VOYAGE_API_KEY")
                .expect("read entry")
                .expect("entry exists");
            entry.allowed_agents = Some(vec!["agent-a".to_string()]);
            store.vault_upsert_entry(&entry).expect("revoke ACL");
        })
        .expect_err("standalone publication must reject ACL drift");
        assert!(error.contains("revision changed"), "{error}");
        assert!(
            llm.provider_secret_for_tests(&["VOYAGE_API_KEY"]).is_none(),
            "stale Keychain plaintext must not publish"
        );

        materialize_standalone(&llm, &db_path)
            .expect("retry should observe the restricted entry and complete safely");
        assert!(
            llm.provider_secret_for_tests(&["VOYAGE_API_KEY"]).is_none(),
            "restricted Keychain entry must remain absent after retry"
        );
    }

    #[test]
    fn vault_config_exists_fails_closed_for_corrupt_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory.db");
        std::fs::write(&path, b"not a sqlite database").expect("write corrupt database");

        let err = vault_config_exists(&path).expect_err("corrupt Vault DB must stay loud");

        assert!(
            err.contains("Failed to open Vault DB for provider refresh")
                || err.contains("Failed to read Vault config for provider refresh"),
            "{err}"
        );
    }

    #[test]
    fn non_model_rotation_members_keep_typed_drop_class() {
        let mut drops = HashMap::new();
        let pools = HashMap::from([(
            "TAVILY_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "TAVILY_API_KEY_1".to_string(),
                value: "not-materialized".to_string(),
            }],
        )]);

        annotate_non_model_drops(&pools, &mut drops);

        assert_eq!(
            drops.get("TAVILY_API_KEY"),
            Some(&tachi_llm::AliasSkipClass::ListedNotModelProvider)
        );
        assert_eq!(
            drops.get("TAVILY_API_KEY_1"),
            Some(&tachi_llm::AliasSkipClass::ListedNotModelProvider)
        );
    }

    #[test]
    fn non_model_rotation_member_alias_surfaces_typed_drop_at_materialization_boundary() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = EnvRestore::set("VOYAGE_API_KEY", "vault:TAVILY_API_KEY_1");
        let llm = LlmClient::new().expect("llm client");
        let mut pools = HashMap::from([(
            "TAVILY_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "TAVILY_API_KEY_1".to_string(),
                value: "not-materialized".to_string(),
            }],
        )]);
        let mut drops = HashMap::new();
        annotate_non_model_drops(&pools, &mut drops);
        pools = filter_model_provider_pools(pools);

        let report = tachi_llm::materialize_provider_secrets_from_durable_source(
            &llm,
            ["VOYAGE_API_KEY"],
            || {
                Ok(tachi_llm::DurableVaultLoad {
                    pools,
                    availability: VaultSourceAvailability::Readable,
                    source_generation: None,
                    listed_drops: drops,
                })
            },
        )
        .expect("typed non-model drop must remain a non-fatal alias skip");

        assert_eq!(
            report.skipped_alias_classes,
            vec![(
                "VOYAGE_API_KEY".to_string(),
                tachi_llm::AliasSkipClass::ListedNotModelProvider,
            )]
        );
        assert!(
            !report.skipped_aliases[0].1.contains("absent"),
            "{}",
            report.skipped_aliases[0].1
        );
    }

    #[test]
    fn default_vault_fallback_fails_closed_for_corrupt_database() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let _env = EnvRestore::set_path("TACHI_HOME", home.path());
        let default_db = default_global_db_path();
        std::fs::create_dir_all(default_db.parent().expect("default DB parent"))
            .expect("create default DB parent");
        std::fs::write(&default_db, b"not a sqlite database").expect("write corrupt database");
        let custom_db = home.path().join("custom").join(memcore::MEMORY_DB_FILENAME);

        let err = match resolve_vault_pools(None, &custom_db) {
            Ok(_) => panic!("corrupt fallback Vault DB must stay loud"),
            Err(err) => err,
        };

        assert!(
            err.contains("provider read failed") || err.contains("Vault DB"),
            "{err}"
        );
    }

    /// #1680/D3 (codex finding 1, the sharpest catch of the cross-vendor
    /// review): a Vault-stored search key must never reach the LLM provider
    /// cache, even though Vault pool loading is class-blind and admits any
    /// standalone `*_API_KEY` entry. This exercises the actual enforcement
    /// seam — `filter_model_provider_pools`, which every production caller of
    /// `materialize_provider_secrets_from_durable_source` routes its resolved
    /// pools through before handing them to `tachi_llm` — rather than the
    /// env-name allowlist alone, which #1680's frozen design explicitly
    /// states is insufficient.
    #[test]
    fn filter_model_provider_pools_drops_search_keys_keeps_model_keys() {
        let vault_pools = HashMap::from([
            (
                "TAVILY_API_KEY".to_string(),
                vec![ProviderSecret {
                    key_id: "TAVILY_API_KEY".to_string(),
                    value: "tavily-secret".to_string(),
                }],
            ),
            (
                "EXA_API_KEY".to_string(),
                vec![ProviderSecret {
                    key_id: "EXA_API_KEY".to_string(),
                    value: "exa-secret".to_string(),
                }],
            ),
            (
                "DEEPSEEK_API_KEY".to_string(),
                vec![ProviderSecret {
                    key_id: "DEEPSEEK_API_KEY".to_string(),
                    value: "deepseek-secret".to_string(),
                }],
            ),
        ]);

        let filtered = filter_model_provider_pools(vault_pools);
        // `ProviderSecret` deliberately does not implement `Debug` (secret-negative:
        // no accidental leak surface via `{:?}`), so failure messages reference the
        // pool's key names only, never the map/value contents.
        let mut filtered_keys: Vec<&str> = filtered.keys().map(String::as_str).collect();
        filtered_keys.sort_unstable();

        assert!(
            !filtered.contains_key("TAVILY_API_KEY"),
            "a Vault-stored search key must not reach the LLM materialization pools: {filtered_keys:?}"
        );
        assert!(
            !filtered.contains_key("EXA_API_KEY"),
            "a Vault-stored search key must not reach the LLM materialization pools: {filtered_keys:?}"
        );
        assert!(
            filtered.contains_key("DEEPSEEK_API_KEY"),
            "a Vault-stored ModelApi key must still reach the LLM materialization pools: {filtered_keys:?}"
        );
    }

    /// codex NEEDS-FIXES BUG-1: when a `*_API_KEY_N`-shaped name has no
    /// *configured* rotation row, `group_api_key_values_by_configured_rotations`
    /// (tachi-llm) does not fold it under its logical prefix — it stores the
    /// pool under the raw member name itself (e.g. `VOYAGE_API_KEY_2`). A
    /// bare exact-name lookup against `model_provider_env_names()` would
    /// never match that raw member name and would wrongly drop a real
    /// ModelApi credential. `filter_model_provider_pools` must also try the
    /// name's parsed rotation prefix (the same primitive
    /// `doctor::secrets::is_provider_secret_name` already uses for this
    /// exact purpose).
    #[test]
    fn filter_model_provider_pools_admits_model_rotation_member_rejects_search_rotation_member() {
        let vault_pools = HashMap::from([
            (
                "VOYAGE_API_KEY_2".to_string(),
                vec![ProviderSecret {
                    key_id: "VOYAGE_API_KEY_2".to_string(),
                    value: "voyage-member".to_string(),
                }],
            ),
            (
                "TAVILY_API_KEY_2".to_string(),
                vec![ProviderSecret {
                    key_id: "TAVILY_API_KEY_2".to_string(),
                    value: "tavily-member".to_string(),
                }],
            ),
        ]);

        let filtered = filter_model_provider_pools(vault_pools);
        let filtered_keys: Vec<&str> = filtered.keys().map(String::as_str).collect();

        assert!(
            filtered.contains_key("VOYAGE_API_KEY_2"),
            "a rotation-member-shaped ModelApi pool key (no configured rotation \
             row, so it arrives as its own standalone pool key) must still be \
             admitted via its parsed prefix: {filtered_keys:?}"
        );
        assert!(
            !filtered.contains_key("TAVILY_API_KEY_2"),
            "a rotation-member-shaped SearchApi pool key must still be rejected \
             via its parsed prefix: {filtered_keys:?}"
        );
    }

    /// codex NEEDS-FIXES BUG-2 (lock/readable asymmetry): the filter must
    /// treat a rotation-configured pool (loaded as a prefix-keyed pool with
    /// multiple `ProviderSecret` members — what a readable Vault with a
    /// configured rotation row produces) and the same underlying credential
    /// arriving unshaped by rotation config (member name used directly as
    /// the standalone pool key — exactly what
    /// `group_api_key_values_by_configured_rotations` produces when no
    /// `vault_setup_rotation` row is visible for that prefix, which a
    /// locked/Keychain-fallback read can observe) identically. The visible
    /// set after filtering must not depend on which loading path produced
    /// the map — BUG-1's fix (parsed-prefix fallback) makes both shapes
    /// agree; this pins that agreement so it can't silently regress.
    #[test]
    fn filter_model_provider_pools_is_symmetric_across_rotation_configured_and_unconfigured_shapes()
    {
        // Shape A: rotation configured — the vault pool loader groups both
        // members under the logical prefix key.
        let rotation_configured = HashMap::from([
            (
                "VOYAGE_API_KEY".to_string(),
                vec![
                    ProviderSecret {
                        key_id: "VOYAGE_API_KEY_1".to_string(),
                        value: "voyage-a".to_string(),
                    },
                    ProviderSecret {
                        key_id: "VOYAGE_API_KEY_2".to_string(),
                        value: "voyage-b".to_string(),
                    },
                ],
            ),
            (
                "TAVILY_API_KEY".to_string(),
                vec![ProviderSecret {
                    key_id: "TAVILY_API_KEY_1".to_string(),
                    value: "tavily-a".to_string(),
                }],
            ),
        ]);
        // Shape B: no rotation row configured for either prefix — the loader
        // stores each member under its own raw name instead (the shape a
        // locked/Keychain-fallback read, or a readable Vault with no
        // `vault_setup_rotation` row, actually produces).
        let rotation_unconfigured = HashMap::from([
            (
                "VOYAGE_API_KEY_2".to_string(),
                vec![ProviderSecret {
                    key_id: "VOYAGE_API_KEY_2".to_string(),
                    value: "voyage-b".to_string(),
                }],
            ),
            (
                "TAVILY_API_KEY_2".to_string(),
                vec![ProviderSecret {
                    key_id: "TAVILY_API_KEY_2".to_string(),
                    value: "tavily-b".to_string(),
                }],
            ),
        ]);

        let filtered_configured = filter_model_provider_pools(rotation_configured);
        let filtered_unconfigured = filter_model_provider_pools(rotation_unconfigured);
        let unconfigured_keys: Vec<&str> =
            filtered_unconfigured.keys().map(String::as_str).collect();

        assert!(filtered_configured.contains_key("VOYAGE_API_KEY"));
        assert!(!filtered_configured.contains_key("TAVILY_API_KEY"));
        assert!(
            filtered_unconfigured.contains_key("VOYAGE_API_KEY_2"),
            "a ModelApi rotation member surfaced as a standalone pool key (the \
             unconfigured-rotation shape) must survive filtering identically to \
             the configured shape: {unconfigured_keys:?}"
        );
        assert!(
            !filtered_unconfigured.contains_key("TAVILY_API_KEY_2"),
            "a SearchApi rotation member must be rejected in the unconfigured \
             shape exactly as its prefix-keyed pool would be: {unconfigured_keys:?}"
        );
    }

    /// #1680/D3: `provider_env_keys()` (the materialization allowlist) and
    /// `admitted_provider_env_keys()` (the all-class admitted-secret-name
    /// surface) must disagree on exactly the SearchApi names — that
    /// divergence is the whole point of the split. A regression that
    /// re-merges the two views would make this test start failing at the
    /// `assert!(!...)` lines.
    #[test]
    fn model_and_admitted_provider_env_keys_diverge_on_search_api_names() {
        let model_only = provider_env_keys();
        let admitted = admitted_provider_env_keys();

        assert!(model_only.contains("DEEPSEEK_API_KEY"));
        assert!(admitted.contains("DEEPSEEK_API_KEY"));

        assert!(
            !model_only.contains("TAVILY_API_KEY"),
            "materialization allowlist must not admit a SearchApi name"
        );
        assert!(
            !model_only.contains("EXA_API_KEY"),
            "materialization allowlist must not admit a SearchApi name"
        );
        assert!(
            !model_only.contains("GOOGLE_SEARCH_API_KEY"),
            "materialization allowlist must not admit a SearchApi name"
        );

        assert!(
            admitted.contains("TAVILY_API_KEY"),
            "the all-class admitted set must still recognize SearchApi names"
        );
        assert!(
            admitted.contains("EXA_API_KEY"),
            "the all-class admitted set must still recognize SearchApi names"
        );
        assert!(
            admitted.contains("GOOGLE_SEARCH_API_KEY"),
            "the all-class admitted set must still recognize SearchApi names"
        );
    }

    /// #1096 leaf-2a round-2 (codex B4-status): `resolved_home` is additive —
    /// `None` reproduces the exact pre-existing scan set (unchanged for every
    /// caller but `status`), and `Some(path)` unions in `path/config.env` on
    /// top of it. This is the SIGIL_HOME-only-deployment regression codex
    /// flagged: without the resolved-home union, a `config.env` living only
    /// under a `SIGIL_HOME`-style custom root was invisible to this scan even
    /// though `status`'s manifest side already resolved to that root.
    #[test]
    fn collect_config_env_values_unions_in_resolved_home() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let custom_home = tempfile::tempdir().expect("custom home tempdir");
        std::fs::write(
            custom_home.path().join("config.env"),
            "TACHI_ROUND2_B4_PROBE=custom-value\n",
        )
        .expect("write custom config.env");

        let without_home = collect_config_env_values(None);
        assert!(
            !without_home.contains_key("TACHI_ROUND2_B4_PROBE"),
            "a config.env under an arbitrary path must not be scanned without resolved_home"
        );

        let with_home = collect_config_env_values(Some(custom_home.path()));
        assert_eq!(
            with_home.get("TACHI_ROUND2_B4_PROBE").map(String::as_str),
            Some("custom-value"),
            "resolved_home's config.env must be unioned into the scan set"
        );
    }

    #[test]
    fn parse_vault_alias_accepts_colon_form() {
        assert_eq!(
            parse_vault_alias("vault:VOYAGE_API_KEY"),
            Some("VOYAGE_API_KEY")
        );
        assert_eq!(parse_vault_alias("  vault:foo  "), Some("foo"));
        assert!(parse_vault_alias("sk-live").is_none());
    }

    #[test]
    fn readable_empty_keychain_scan_stays_readable() {
        let load =
            durable_load_from_keychain_scan(crate::status_ops::status_health::KeychainApiKeyScan {
                values: Vec::new(),
                slot_accounts: HashMap::new(),
                lane_config_values: LaneConfigValues::default(),
                dropped: HashMap::new(),
                rotation_prefixes: HashSet::new(),
                source_readable: true,
                acl_revision: Some(
                    crate::vault_ops::vault_materialization_acl_revision_from_rows(&[], &[], &[]),
                ),
            });
        assert!(load.load.pools.is_empty());
        assert!(load.load.listed_drops.is_empty());
        assert_eq!(load.load.availability, VaultSourceAvailability::Readable);
    }

    #[test]
    fn readable_empty_default_vault_fallback_stays_readable() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let _env = EnvRestore::set_path("TACHI_HOME", home.path());
        let default_db = default_global_db_path();
        let custom_db = home.path().join("custom").join(memcore::MEMORY_DB_FILENAME);
        let loader = |path: &Path| {
            Ok(VaultSourceLoad {
                load: tachi_llm::DurableVaultLoad {
                    pools: HashMap::new(),
                    listed_drops: HashMap::new(),
                    source_generation: None,
                    availability: if paths_equal(path, &default_db) {
                        VaultSourceAvailability::Readable
                    } else {
                        VaultSourceAvailability::LockedOrUnavailable
                    },
                },
                lane_config_values: LaneConfigValues::default(),
                acl_revision: None,
            })
        };
        let resolved = resolve_vault_pools_with_keychain_loader(None, &custom_db, &loader)
            .expect("resolve default fallback");
        let load = resolved.load;
        assert!(load.pools.is_empty());
        assert_eq!(
            load.availability,
            VaultSourceAvailability::Readable,
            "the actual resolver must preserve readable-empty fallback authority"
        );
    }

    #[test]
    fn configured_rotation_prefix_drop_uses_lowest_member_deterministically() {
        let mut drops = HashMap::from([
            (
                "VOYAGE_API_KEY_10".to_string(),
                tachi_llm::AliasSkipClass::ListedFenced,
            ),
            (
                "VOYAGE_API_KEY_1".to_string(),
                tachi_llm::AliasSkipClass::ListedWrongType,
            ),
        ]);
        promote_configured_rotation_prefix_drops(
            &mut drops,
            &HashMap::new(),
            &HashSet::from(["VOYAGE_API_KEY".to_string()]),
        );
        assert_eq!(
            drops.get("VOYAGE_API_KEY"),
            Some(&tachi_llm::AliasSkipClass::ListedWrongType)
        );
    }

    #[test]
    fn configured_rotation_prefix_drop_ties_break_by_member_name() {
        let mut drops = HashMap::from([
            (
                "VOYAGE_API_KEY_1".to_string(),
                tachi_llm::AliasSkipClass::ListedFenced,
            ),
            (
                "VOYAGE_API_KEY_01".to_string(),
                tachi_llm::AliasSkipClass::ListedWrongType,
            ),
        ]);
        promote_configured_rotation_prefix_drops(
            &mut drops,
            &HashMap::new(),
            &HashSet::from(["VOYAGE_API_KEY".to_string()]),
        );
        assert_eq!(
            drops.get("VOYAGE_API_KEY"),
            Some(&tachi_llm::AliasSkipClass::ListedWrongType),
            "equal numeric indices must use the lexical member name as a stable tie-break"
        );
    }

    #[test]
    fn materialize_provider_secrets_preserves_vault_alias_env() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = EnvRestore::set("VOYAGE_API_KEY", "vault:VOYAGE_API_KEY");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([(
            "VOYAGE_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "VOYAGE_API_KEY".to_string(),
                value: "vault-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools).expect("materialize");

        assert_eq!(report.from_alias, 1);
        assert_eq!(report.env_fallbacks_bypassed, 1);
        assert_eq!(
            llm.provider_secret_for_tests(&["VOYAGE_API_KEY"])
                .as_deref(),
            Some("vault-secret")
        );
        assert_eq!(
            std::env::var("VOYAGE_API_KEY").as_deref(),
            Ok("vault:VOYAGE_API_KEY")
        );
    }

    #[test]
    fn materialize_provider_secrets_preserves_duplicate_plaintext_env_when_vault_wins() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = EnvRestore::set("OPENAI_API_KEY", "env-secret");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([(
            "OPENAI_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "OPENAI_API_KEY".to_string(),
                value: "vault-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools).expect("materialize");

        assert_eq!(report.from_alias, 0);
        assert_eq!(report.env_fallbacks_bypassed, 1);
        assert_eq!(
            llm.provider_secret_for_tests(&["OPENAI_API_KEY"])
                .as_deref(),
            Some("vault-secret")
        );
        assert_eq!(std::env::var("OPENAI_API_KEY").as_deref(), Ok("env-secret"));
    }

    // #1279: a missing `vault:` alias no longer aborts the whole batch with an
    // `Err`; it degrades to a per-alias entry in `report.skipped_aliases`. This
    // test preserves the frozen guarantee that the skip stays observable AND
    // carries `vault_unlock`/`vault_set` remediation (never a naked value) — now
    // enforced at the consumer surface via `format_skipped_alias_reason`, the
    // single source consumers use to reattach remediation. Discrimination: if a
    // consumer surfaced the raw skip reason without remediation, the
    // `vault_unlock`/`vault_set` assertions below would fail.
    #[test]
    fn materialize_provider_secrets_formats_missing_alias_remediation() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Keep this provider-shaped key unique so the test remains independent
        // of the VOYAGE_API_KEY alias cases under default parallel execution.
        let _env = EnvRestore::set("ANTHROPIC_API_KEY", "vault:MISSING_ANTHROPIC");
        let llm = LlmClient::new().expect("llm client");
        let report = materialize_provider_secrets(&llm, &HashMap::new())
            .expect("missing alias must degrade to a skip, not abort the batch");

        let (key, reason) = report
            .skipped_aliases
            .iter()
            .find(|(key, _reason)| key == "ANTHROPIC_API_KEY")
            .expect("missing Anthropic alias must be recorded in skipped_aliases");
        assert_eq!(key, "ANTHROPIC_API_KEY");

        let surfaced = format_skipped_alias_reason(reason);
        assert!(surfaced.contains("Config key 'ANTHROPIC_API_KEY' references a Vault alias"));
        // The test-only pre-resolved entry reports LockedOrUnavailable, since a
        // caller that hands over already-resolved pools cannot say whether the
        // Vault was readable.
        assert!(surfaced.contains("could not be read"), "{surfaced}");
        assert!(surfaced.contains("vault_unlock"));
        assert!(surfaced.contains("vault_set"));
        assert!(!surfaced.contains("ANTHROPIC_API_KEY=vault:MISSING_ANTHROPIC"));
        assert!(!surfaced.contains("MISSING_ANTHROPIC"));
    }

    #[test]
    fn malformed_alias_error_does_not_advise_vault_unlock_or_set() {
        let err = format_provider_materialization_error(
            "Config key 'ANTHROPIC_API_KEY' references a Vault alias that is not a valid Vault secret name; provider refresh refused and prior provider cache left unchanged"
                .to_string(),
        );
        assert!(err.contains("not a valid Vault secret name"), "{err}");
        assert!(
            !err.contains("vault_unlock") && !err.contains("vault_set"),
            "malformed alias names are config typos: {err}"
        );
    }

    #[test]
    fn listed_integrity_error_does_not_advise_vault_unlock_or_set() {
        let err = format_provider_materialization_error(
            "Config key 'SILICONFLOW_API_KEY' references a Vault alias whose listed secret is unusable (auth_failed)."
                .to_string(),
        );
        assert!(err.contains("unusable (auth_failed)"), "{err}");
        assert!(
            !err.contains("vault_unlock") && !err.contains("vault_set"),
            "listed integrity is not a missing/locked secret: {err}"
        );
    }

    #[test]
    fn skipped_alias_warning_distinguishes_retained_pool_from_no_cache() {
        let alias_sentinel = "MISSING_VOYAGE_MUST_NOT_LEAK";
        let value_sentinel = "VOYAGE_SECRET_MUST_NOT_LEAK";

        let retained = format_skipped_alias_warning(
            "VOYAGE_API_KEY",
            true,
            AliasSkipClass::from_availability(VaultSourceAvailability::LockedOrUnavailable),
        );
        let no_cache = format_skipped_alias_warning(
            "VOYAGE_API_KEY",
            false,
            AliasSkipClass::from_availability(VaultSourceAvailability::Readable),
        );

        assert!(retained.contains("retained last-known-good provider pool"));
        assert!(no_cache.contains("no last-known-good provider pool retained"));
        // The wording must follow the source, not just the cache outcome.
        assert!(retained.contains("could not be read"), "{retained}");
        assert!(
            no_cache.contains("absent from a readable Vault"),
            "{no_cache}"
        );
        for warning in [&retained, &no_cache] {
            assert!(warning.contains("VOYAGE_API_KEY"));
            assert!(warning.contains("vault_unlock"));
            assert!(warning.contains("vault_set"));
            assert!(!warning.contains(alias_sentinel));
            assert!(!warning.contains(value_sentinel));
        }
    }

    #[test]
    fn skipped_alias_warning_listed_unusable_does_not_say_absent() {
        let warning = format_skipped_alias_warning(
            "SILICONFLOW_API_KEY",
            false,
            AliasSkipClass::ListedUnusableAuthFailed,
        );
        assert!(
            warning.contains("listed secret is unusable (auth_failed)"),
            "{warning}"
        );
        assert!(
            !warning.contains("absent from a readable Vault"),
            "{warning}"
        );
        assert!(!warning.contains("vault:"));
        assert!(
            !warning.contains("vault_set") && !warning.contains("vault_unlock"),
            "listed integrity is not a missing/locked secret: {warning}"
        );
    }
}
