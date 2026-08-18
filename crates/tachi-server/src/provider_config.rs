//! Provider runtime bootstrap: API key resolution (Vault, config.env `vault:`
//! aliases, env fallbacks) and the env→catalog deployment import that records
//! what those chains resolved to.
//!
//! Single path for daemon, MCP, CLI backfill, and vector sweep so background jobs
//! do not re-implement Keychain/Vault reads with different behavior.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::vault_ops::load_unlocked_api_key_secret_pools;
use crate::vault_ops::{classify_vault_read_error, VaultReadState};
use crate::MemoryServer;
pub use tachi_llm::{
    group_api_key_values_by_configured_rotations, is_vault_alias, parse_rotation_member_name,
    parse_vault_alias, vault_alias_line, MaterializeReport, VAULT_ALIAS_PREFIX,
};
use tachi_llm::{LlmClient, ProviderSecret, VaultSourceAvailability};

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
    let allowed = provider_env_keys();
    pools
        .into_iter()
        .filter(|(name, _)| {
            allowed.contains(name)
                || parse_rotation_member_name(name)
                    .is_some_and(|(prefix, _)| allowed.contains(prefix))
        })
        .collect()
}

/// Load API keys from an unlocked in-process Vault session.
pub fn vault_api_key_pools_from_server(
    server: &MemoryServer,
) -> Result<HashMap<String, Vec<ProviderSecret>>, String> {
    load_unlocked_api_key_secret_pools(server)
}

/// Load API keys via macOS Keychain + global DB (daemon/CLI when memory unlock is empty).
pub fn vault_api_key_pools_from_keychain(
    global_db_path: &Path,
) -> HashMap<String, Vec<ProviderSecret>> {
    let rotation_prefixes = rotation_prefixes_from_global_db(global_db_path);
    let values = match crate::status_ops::status_health::load_keychain_vault_api_key_values(
        global_db_path,
    ) {
        Ok(values) => values,
        Err(err) => {
            tracing::warn!(
                "[vault] keychain vault read failed during provider key resolution: {err}"
            );
            Vec::new()
        }
    };
    group_api_key_values_by_configured_rotations(values, &rotation_prefixes)
}

/// Resolve the Vault-backed provider pools, and report whether the source was
/// actually readable.
///
/// The availability half is load-bearing, not diagnostic: materialization uses
/// it to decide whether a missing `vault:` alias target means "revoked" (drop
/// the cached pool) or "cannot tell right now" (retain it). See
/// [`tachi_llm::VaultSourceAvailability`].
fn resolve_vault_pools(
    server: Option<&MemoryServer>,
    global_db_path: &Path,
) -> Result<
    (
        HashMap<String, Vec<ProviderSecret>>,
        VaultSourceAvailability,
    ),
    String,
> {
    // Starts unavailable and is only promoted by a read that actually
    // succeeded: an unproven source must never license retention.
    let mut availability = VaultSourceAvailability::LockedOrUnavailable;
    if let Some(server) = server {
        match vault_api_key_pools_from_server(server) {
            Ok(map) if !map.is_empty() => return Ok((map, VaultSourceAvailability::Readable)),
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
    let pools = vault_api_key_pools_from_keychain(global_db_path);
    if !pools.is_empty() {
        return Ok((pools, VaultSourceAvailability::Readable));
    }
    if vault_config_exists(global_db_path) {
        return Ok((pools, availability));
    }

    let default_global = default_global_db_path();
    if paths_equal(global_db_path, &default_global) {
        return Ok((pools, availability));
    }

    let fallback = vault_api_key_pools_from_keychain(&default_global);
    if !fallback.is_empty() {
        tracing::warn!(
            "[provider] global DB {} has no initialized Vault; using default Vault DB {} for provider key materialization",
            global_db_path.display(),
            default_global.display()
        );
        return Ok((fallback, VaultSourceAvailability::Readable));
    }
    Ok((fallback, availability))
}

pub(crate) fn default_global_db_path() -> std::path::PathBuf {
    crate::status_ops::resolve_app_home()
        .join("global")
        .join(memcore::MEMORY_DB_FILENAME)
}

fn vault_config_exists(global_db_path: &Path) -> bool {
    let Some(path) = global_db_path.to_str() else {
        return false;
    };
    let Ok(store) = memcore::MemoryStore::open_read_only(path) else {
        return false;
    };
    store.vault_get_config().ok().flatten().is_some()
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

fn rotation_prefixes_from_global_db(global_db_path: &Path) -> HashSet<String> {
    let Some(path) = global_db_path.to_str() else {
        return HashSet::new();
    };
    let Ok(store) = memcore::MemoryStore::open_read_only(path) else {
        return HashSet::new();
    };
    store
        .vault_list_rotations()
        .unwrap_or_default()
        .into_iter()
        .map(|rotation| rotation.prefix)
        .collect()
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
        && (err.contains("absent from a readable Vault") || err.contains("could not be read"));
    if (err.starts_with("provider alias ") && err.contains(" could not be resolved from secret "))
        || unresolved_alias
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
/// only: the logical key name and whether that key's prior pool was retained.
/// The warning deliberately rebuilds a constant reason instead of forwarding
/// report text, so even a contaminated caller cannot inject an alias target.
pub fn format_skipped_alias_warning(
    key: &str,
    retained: bool,
    availability: VaultSourceAvailability,
) -> String {
    let cache_disposition = if retained {
        "retained last-known-good provider pool"
    } else {
        "no last-known-good provider pool retained"
    };
    // Derived from the same value materialization decided on, so the operator
    // line cannot disagree with what actually happened to the cache.
    let safe_reason = match availability {
        VaultSourceAvailability::Readable => format!(
            "Config key '{key}' references a Vault alias whose secret is absent from a readable Vault."
        ),
        VaultSourceAvailability::LockedOrUnavailable => format!(
            "Config key '{key}' references a Vault alias, but the Vault could not be read."
        ),
    };
    format!(
        "[provider] skipped alias for '{key}'; {cache_disposition}: {}",
        format_skipped_alias_reason(&safe_reason)
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
/// from the logical key and retained disposition; never forward raw reasons.
pub fn describe_skipped_alias_report(report: &MaterializeReport) -> String {
    let details = report
        .skipped_aliases
        .iter()
        .map(|(key, _reason)| {
            let disposition = if report
                .retained_from_last_known_good
                .iter()
                .any(|retained_key| retained_key == key)
            {
                "retained last-known-good provider pool"
            } else {
                "no last-known-good provider pool retained"
            };
            format!("{key}: {disposition}")
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{} provider alias(es) skipped during materialization: {details}",
        report.skipped_aliases.len()
    )
}

pub fn materialize_for_server(server: &MemoryServer) -> Result<MaterializeReport, String> {
    materialize_for_server_inner(server, None)
}

fn materialize_for_server_inner(
    server: &MemoryServer,
    after_vault_pools_resolved: Option<Box<dyn FnOnce() + Send>>,
) -> Result<MaterializeReport, String> {
    let global = server.global_db_path_buf();
    tachi_llm::materialize_provider_secrets_from_durable_source(
        server.llm.as_ref(),
        provider_env_keys(),
        || {
            let (pools, availability) = resolve_vault_pools(Some(server), &global)?;
            let pools = filter_model_provider_pools(pools);
            if let Some(hook) = after_vault_pools_resolved {
                hook();
            }
            Ok((pools, availability))
        },
    )
    .map_err(format_provider_materialization_error)
}

#[cfg(test)]
pub(crate) fn materialize_for_server_with_hook_for_tests(
    server: &MemoryServer,
    after_vault_pools_resolved: impl FnOnce() + Send + 'static,
) -> Result<MaterializeReport, String> {
    materialize_for_server_inner(server, Some(Box::new(after_vault_pools_resolved)))
}

pub fn materialize_standalone(
    llm: &LlmClient,
    global_db_path: &Path,
) -> Result<MaterializeReport, String> {
    tachi_llm::materialize_provider_secrets_from_durable_source(llm, provider_env_keys(), || {
        let (pools, availability) = resolve_vault_pools(None, global_db_path)?;
        Ok((filter_model_provider_pools(pools), availability))
    })
    .map_err(format_provider_materialization_error)
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
                "[provider] {} provider key(s) ready, {} alias(es) skipped ({keys}); see '[provider] skipped alias' warnings for vault_unlock/vault_set remediation",
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
    use memcore::db::model_catalog::{upsert_model_deployment, DeploymentWrite};

    let config = server.llm.runtime_config();
    let embedding = tachi_llm::EmbeddingConfig::from_env();
    let embeddings_endpoint = tachi_llm::voyage_embeddings_endpoint();
    let observed_at = memcore::db::now_utc_iso();

    let chat_deployments = tachi_llm::env_chat_lane_deployments(&config, &observed_at)
        .map_err(|err| err.to_string())?;
    let mut embedding_deployment = None;
    let mut embedding_refused = None;
    match &embedding {
        Ok(resolved) => {
            embedding_deployment = Some(
                tachi_llm::env_embedding_deployment(resolved, &embeddings_endpoint, &observed_at)
                    .map_err(|err| err.to_string())?,
            );
        }
        Err(err) => embedding_refused = Some(err.clone()),
    }

    server.with_global_store(|store| {
        let conn = store.connection();
        let transaction = conn
            .unchecked_transaction()
            .map_err(|e| format!("open catalog import transaction: {e}"))?;

        let mut summary = EnvCatalogImport {
            embedding_refused,
            ..EnvCatalogImport::default()
        };

        for lane in &chat_deployments {
            let write = upsert_model_deployment(&transaction, &lane.deployment)
                .map_err(|e| e.to_string())?;
            summary.rows += 1;
            if !matches!(write, DeploymentWrite::Unchanged { .. }) {
                summary.changed += 1;
            }
        }

        if let Some(row) = &embedding_deployment {
            let write = upsert_model_deployment(&transaction, &row.deployment)
                .map_err(|e| e.to_string())?;
            summary.rows += 1;
            if !matches!(write, DeploymentWrite::Unchanged { .. }) {
                summary.changed += 1;
            }
        }

        transaction
            .commit()
            .map_err(|e| format!("commit catalog import: {e}"))?;
        Ok(summary)
    })
}

/// Parse `~/.tachi/config.env` (and peers) into key → value (non-empty values only).
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
pub fn collect_config_env_values(resolved_home: Option<&Path>) -> HashMap<String, String> {
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

    let mut values = HashMap::new();
    for path in paths {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();
                if !key.is_empty() && !value.is_empty() {
                    values.insert(key, value);
                }
            }
        }
    }
    values
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
    fn keychain_loader_only_groups_configured_rotation_members() {
        let mut rotations = HashSet::new();
        rotations.insert("VOYAGE_API_KEY".to_string());

        let grouped = group_api_key_values_by_configured_rotations(
            vec![
                ("VOYAGE_API_KEY_1".to_string(), "voyage-a".to_string()),
                ("VOYAGE_API_KEY_2".to_string(), "voyage-b".to_string()),
                ("SOME_API_KEY_2".to_string(), "standalone".to_string()),
            ],
            &rotations,
        );

        let voyage = grouped
            .get("VOYAGE_API_KEY")
            .expect("configured rotation members should be grouped");
        assert_eq!(voyage.len(), 2);
        assert_eq!(voyage[0].key_id, "VOYAGE_API_KEY_1");
        assert_eq!(voyage[1].key_id, "VOYAGE_API_KEY_2");
        assert!(!grouped.contains_key("SOME_API_KEY"));
        assert_eq!(
            grouped
                .get("SOME_API_KEY_2")
                .and_then(|entries| entries.first())
                .map(|entry| entry.value.as_str()),
            Some("standalone")
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
    fn skipped_alias_warning_distinguishes_retained_pool_from_no_cache() {
        let alias_sentinel = "MISSING_VOYAGE_MUST_NOT_LEAK";
        let value_sentinel = "VOYAGE_SECRET_MUST_NOT_LEAK";

        let retained = format_skipped_alias_warning(
            "VOYAGE_API_KEY",
            true,
            VaultSourceAvailability::LockedOrUnavailable,
        );
        let no_cache = format_skipped_alias_warning(
            "VOYAGE_API_KEY",
            false,
            VaultSourceAvailability::Readable,
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
}
