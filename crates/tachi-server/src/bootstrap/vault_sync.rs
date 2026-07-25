use std::path::{Path, PathBuf};

use chrono::Utc;
use memcore::vault::{VaultConfig, VaultEntry, VaultKeyRotation};
use serde::{Deserialize, Serialize};

use super::{open_cli_store, open_cli_store_read_only};

// ---------------------------------------------------------------------------
// SECURITY: residual offline-guessing risk (#576)
// ---------------------------------------------------------------------------
// The sync bundle is *signed* (AES-256-GCM-AAD), not *encrypted*. It carries
// `vault_config` (salt + verifier) in cleartext, and every entry's AEAD
// ciphertext + signature tag also serves as a password-guess verification
// oracle. Anyone who obtains the bundle can run offline password-guessing
// attacks.
//
// File permissions (0600) and the `--allow-cloud` gate reduce accidental
// exposure but do NOT eliminate the threat. The real fix requires
// recipient-key encryption (X25519/HPKE/age) so that only the holder of a
// private key can decrypt — this is tracked as a separate design effort.
//
// A password-derived AEAD wrapper is NOT sufficient: its tag is itself an
// offline verification oracle (owner directive, #576).
//
// Until recipient-key encryption ships, this module operates under an
// explicitly-accepted residual offline-guessing risk that must be
// acknowledged by the owner/adjudicator, not self-ratified here.
// ---------------------------------------------------------------------------

const BUNDLE_TYPE: &str = "tachi.vault.bundle";
const BUNDLE_VERSION: u32 = 1;
const BUNDLE_SIGNATURE_ALGORITHM: &str = "aes-256-gcm-aad-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultSyncBundle {
    bundle_type: String,
    version: u32,
    exported_at: String,
    /// `None` for entries-only bundles (see `--entries-only`), which omit the
    /// salt/verifier to reduce the offline-guessing surface (#576).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vault_config: Option<VaultConfig>,
    entries: Vec<VaultEntry>,
    rotations: Vec<VaultKeyRotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature: Option<VaultSyncSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultSyncSignature {
    algorithm: String,
    nonce: String,
    tag: String,
}

#[derive(Debug, Clone)]
pub(super) struct VaultSyncStatus {
    pub path: PathBuf,
    pub exists: bool,
    pub size_bytes: Option<u64>,
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct VaultSyncImportReport {
    pub path: String,
    pub entries_imported: usize,
    pub rotations_imported: usize,
    pub initialized_vault: bool,
}

pub(super) fn default_vault_sync_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("Could not resolve home directory")?;
    Ok(home
        .join(".tachi")
        .join("sync")
        .join("vault")
        .join("vault.bundle.json"))
}

pub(super) fn resolve_vault_sync_path(
    path: Option<PathBuf>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    path.map(Ok).unwrap_or_else(default_vault_sync_path)
}

pub(super) fn export_vault_bundle(
    global_db_path: &PathBuf,
    output: &Path,
    allow_cloud: bool,
    entries_only: bool,
    signing_key: &[u8; 32],
) -> Result<VaultSyncStatus, Box<dyn std::error::Error>> {
    ensure_cloud_export_allowed(output, allow_cloud)?;

    let store = open_cli_store_read_only(global_db_path)?;
    let vault_config = store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?;
    let rotations = store
        .vault_list_rotations()
        .map_err(|e| format!("vault_list_rotations: {e}"))?;

    // entries-only bundles omit vault_config (salt + verifier) to reduce the
    // offline-guessing surface. The import side must already have a matching
    // vault initialized; entries-only bundles cannot bootstrap a new vault.
    let config_for_bundle = if entries_only {
        None
    } else {
        Some(vault_config)
    };

    let mut bundle = VaultSyncBundle {
        bundle_type: BUNDLE_TYPE.to_string(),
        version: BUNDLE_VERSION,
        exported_at: Utc::now().to_rfc3339(),
        vault_config: config_for_bundle,
        entries,
        rotations,
        signature: None,
    };
    sign_bundle(&mut bundle, signing_key)?;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create sync bundle parent {}: {e}", parent.display()))?;
    }
    let tmp = output.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&bundle)?;
    write_owner_only_file(&tmp, body.as_bytes())?;
    set_owner_only_permissions(&tmp)?;
    std::fs::rename(&tmp, output)
        .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), output.display()))?;
    set_owner_only_permissions(output)?;

    vault_sync_status(output)
}

fn ensure_cloud_export_allowed(
    output: &Path,
    allow_cloud: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !allow_cloud && vault_sync_path_requires_cloud_ack(output) {
        return Err(format!(
            "Refusing to export Vault sync bundle to cloud-synced path {} without --allow-cloud. The signed bundle still contains encrypted Vault entries plus password verifier material that enables offline password guessing; choose --output outside cloud storage or re-run with --allow-cloud.",
            output.display()
        )
        .into());
    }
    Ok(())
}

fn vault_sync_path_requires_cloud_ack(path: &Path) -> bool {
    let mut saw_mobile_documents = false;
    for component in path.components() {
        let Some(name) = component.as_os_str().to_str() else {
            continue;
        };
        if name == "Mobile Documents" {
            saw_mobile_documents = true;
            continue;
        }
        if saw_mobile_documents && name == "com~apple~CloudDocs" {
            return true;
        }
    }
    false
}

pub(super) fn import_vault_bundle(
    global_db_path: &PathBuf,
    input: &Path,
    verification_key: Option<&[u8; 32]>,
    allow_unsigned: bool,
) -> Result<VaultSyncImportReport, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(input)
        .map_err(|e| format!("read sync bundle {}: {e}", input.display()))?;
    let bundle: VaultSyncBundle = serde_json::from_str(&raw)
        .map_err(|e| format!("parse sync bundle {}: {e}", input.display()))?;
    validate_bundle(&bundle)?;
    verify_bundle_signature(&bundle, verification_key, allow_unsigned)?;

    // entries-only bundles (no vault_config) cannot bootstrap a new vault.
    // The target must already be initialized with a matching config.
    let bundle_config = bundle.vault_config.as_ref().ok_or_else(|| {
        "This is an entries-only sync bundle (no vault_config). \
         Initialize the Vault on this machine first with `tachi vault init`, \
         then re-run the import."
            .to_string()
    })?;

    let initialized_vault = import_validated_vault_bundle(
        global_db_path,
        bundle_config,
        &bundle.entries,
        &bundle.rotations,
    )?;

    Ok(VaultSyncImportReport {
        path: input.display().to_string(),
        entries_imported: bundle.entries.len(),
        rotations_imported: bundle.rotations.len(),
        initialized_vault,
    })
}

/// The single validating import path for persisting a `VaultConfig` +
/// entries + rotation rows into a target store (tachi#1110). `tachi-server`
/// is the crypto-aware layer, so this is where `kdf_algorithm`/`kdf_params`
/// validation belongs — `memcore`'s `MemoryStore::vault_import_bundle_unchecked`
/// is a storage-leaf primitive that intentionally has no `vault-kit`
/// dependency (the #1106 layering ruling) and persists `config` verbatim.
/// Every caller that wants to import a `VaultConfig` into a store MUST route
/// through this function rather than calling the `_unchecked` primitive
/// directly — that primitive's name exists precisely to make a future bypass
/// visible in review, not to be convenient to call around. `pub(super)`
/// (reachable throughout `bootstrap`, matching this module's other
/// entry points like `import_vault_bundle`/`open_cli_store`) rather than
/// `pub(crate)`: both `mod bootstrap` (in `lib.rs`) and `mod vault_sync`
/// (in `bootstrap/mod.rs`, private child module) are private, so a wider
/// visibility modifier would not actually reach further — this stays
/// consistent with the module's existing convention instead of overclaiming
/// crate-wide reach it cannot deliver.
///
/// Returns whether the target Vault was uninitialized before this call
/// (bootstrap vs. merge into an existing Vault). Does not return the opened
/// store: the sole current caller (`import_vault_bundle` above) has no use
/// for it once import completes, and returning an unused connection just to
/// have it would widen this function's surface for no reason.
///
/// # Ordering (tachi#1080 day-one brick fix)
///
/// The KDF gate runs BEFORE the target store is opened. `config` here is
/// caller-supplied and has no dependency on the target store, so this gate
/// can — and must — run before `open_cli_store` below. `MemoryStore::open`
/// itself creates the target DB file and runs schema init/migrations on it
/// (see `memcore::store::open::MemoryStore::open_with_label_inner`), so
/// validating only after opening would still leave a rejected import having
/// created (or migrated) the target DB file, even though it never got as far
/// as writing `vault_config`. Without this gate, persisting unconditionally
/// once the store is open would import an unsupported/corrupted KDF profile
/// (e.g. a hand-edited `{"m":1,"t":1,"p":1}`) cleanly and then permanently
/// fail every subsequent unlock: the stored-config KDF gate wired elsewhere
/// in #1080 (`parse_stored_kdf_params`) refuses to derive against it. That is
/// not a decryption failure, it's a vault that is initialized but can never
/// again be opened.
pub(super) fn import_validated_vault_bundle(
    global_db_path: &PathBuf,
    config: &VaultConfig,
    entries: &[VaultEntry],
    rotations: &[VaultKeyRotation],
) -> Result<bool, Box<dyn std::error::Error>> {
    ensure_importable_kdf(config)?;

    let mut store = open_cli_store(global_db_path)?;
    let local_config = store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?;

    let initialized_vault = local_config.is_none();
    if let Some(local_config) = local_config.as_ref() {
        ensure_same_vault(local_config, config)?;
    }

    store
        .vault_import_bundle_unchecked(config, entries, rotations)
        .map_err(|e| format!("vault_import_bundle_unchecked: {e}"))?;

    Ok(initialized_vault)
}

pub(super) fn read_bundle_vault_config(
    input: &Path,
) -> Result<Option<VaultConfig>, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(input)
        .map_err(|e| format!("read sync bundle {}: {e}", input.display()))?;
    let bundle: VaultSyncBundle = serde_json::from_str(&raw)
        .map_err(|e| format!("parse sync bundle {}: {e}", input.display()))?;
    validate_bundle(&bundle)?;
    Ok(bundle.vault_config)
}

pub(super) fn bundle_has_signature(input: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(input)
        .map_err(|e| format!("read sync bundle {}: {e}", input.display()))?;
    let bundle: VaultSyncBundle = serde_json::from_str(&raw)
        .map_err(|e| format!("parse sync bundle {}: {e}", input.display()))?;
    validate_bundle(&bundle)?;
    Ok(bundle.signature.is_some())
}

fn sign_bundle(
    bundle: &mut VaultSyncBundle,
    key: &[u8; 32],
) -> Result<(), Box<dyn std::error::Error>> {
    bundle.signature = None;
    let aad = canonical_bundle_bytes(bundle)?;
    let (tag, nonce) = crate::vault_crypto::authenticate(key, &aad)?;
    bundle.signature = Some(VaultSyncSignature {
        algorithm: BUNDLE_SIGNATURE_ALGORITHM.to_string(),
        nonce,
        tag,
    });
    Ok(())
}

fn verify_bundle_signature(
    bundle: &VaultSyncBundle,
    key: Option<&[u8; 32]>,
    allow_unsigned: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(signature) = bundle.signature.as_ref() else {
        if allow_unsigned {
            return Ok(());
        }
        return Err(
            "Vault sync bundle is unsigned; refusing to import without --allow-unsigned".into(),
        );
    };
    if signature.algorithm != BUNDLE_SIGNATURE_ALGORITHM {
        return Err(format!(
            "Unsupported vault sync bundle signature algorithm '{}' (expected '{}')",
            signature.algorithm, BUNDLE_SIGNATURE_ALGORITHM
        )
        .into());
    }
    let Some(key) = key else {
        return Err(
            "Vault sync bundle is signed; provide the Vault password to verify integrity".into(),
        );
    };
    let aad = canonical_bundle_bytes(bundle)?;
    crate::vault_crypto::verify_authentication(key, &aad, &signature.nonce, &signature.tag)
        .map_err(|e| format!("Vault sync bundle integrity verification failed: {e}"))?;
    Ok(())
}

fn canonical_bundle_bytes(bundle: &VaultSyncBundle) -> Result<Vec<u8>, serde_json::Error> {
    let mut unsigned = bundle.clone();
    unsigned.signature = None;
    serde_json::to_vec(&unsigned)
}

pub(super) fn vault_sync_status(
    path: &Path,
) -> Result<VaultSyncStatus, Box<dyn std::error::Error>> {
    let metadata = std::fs::metadata(path).ok();
    let modified_at = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .map(chrono::DateTime::<Utc>::from)
        .map(|ts| ts.to_rfc3339());
    Ok(VaultSyncStatus {
        path: path.to_path_buf(),
        exists: metadata.is_some(),
        size_bytes: metadata.as_ref().map(|metadata| metadata.len()),
        modified_at,
    })
}

fn validate_bundle(bundle: &VaultSyncBundle) -> Result<(), Box<dyn std::error::Error>> {
    if bundle.bundle_type != BUNDLE_TYPE {
        return Err(format!(
            "Unsupported vault sync bundle type '{}' (expected '{}')",
            bundle.bundle_type, BUNDLE_TYPE
        )
        .into());
    }
    if bundle.version != BUNDLE_VERSION {
        return Err(format!(
            "Unsupported vault sync bundle version {} (expected {})",
            bundle.version, BUNDLE_VERSION
        )
        .into());
    }
    // vault_config is optional (entries-only bundles). When present, it must
    // have non-empty salt/verifier.
    if let Some(config) = &bundle.vault_config {
        if config.salt.trim().is_empty() || config.verifier.trim().is_empty() {
            return Err("Vault sync bundle vault_config has empty salt/verifier".into());
        }
    }
    Ok(())
}

/// Reject an imported `vault_config` whose KDF algorithm/parameters are not
/// ones this build can actually derive against (tachi#1080 day-one brick
/// fix; see the call site in `import_vault_bundle` for the full incident).
fn ensure_importable_kdf(config: &VaultConfig) -> Result<(), Box<dyn std::error::Error>> {
    // `kdf_algorithm` is a non-`Option<String>` column (DDL:
    // `kdf_algorithm TEXT NOT NULL DEFAULT 'argon2id'`; Rust type `String`,
    // no `#[serde(default)]`), and every writer in this repo always sets it
    // to the literal "argon2id". A bundle whose JSON omits the field
    // entirely already fails to deserialize as `VaultSyncBundle` earlier in
    // `import_vault_bundle`, before this function ever runs — so the only
    // reachable "no algorithm recorded" shape is an explicit empty string in
    // an older/hand-crafted bundle. This is an exact-empty check, not a
    // trimmed one (tachi#1210): a whitespace-only value is not a reachable
    // "no algorithm recorded" shape from the schema default, so it falls
    // through to the unrecognized-algorithm rejection below rather than
    // silently widening the default carve-out. Treat the exact-empty shape
    // as the implicit historical default rather than rejecting it outright;
    // reject anything else that isn't "argon2id".
    let algorithm = if config.kdf_algorithm.is_empty() {
        "argon2id"
    } else {
        config.kdf_algorithm.as_str()
    };
    if algorithm != "argon2id" {
        return Err(format!(
            "Vault sync bundle's vault_config uses an unrecognized KDF algorithm '{}' (only 'argon2id' is supported); refusing to import.",
            config.kdf_algorithm
        )
        .into());
    }

    crate::vault_crypto::parse_stored_kdf_params(&config.kdf_params)
        .map_err(|e| {
            format!(
                "Vault sync bundle's vault_config uses unsupported KDF parameters; refusing to import (importing it would create a Vault that can never be unlocked again): {e}"
            )
        })?;
    Ok(())
}

fn ensure_same_vault(
    local: &VaultConfig,
    imported: &VaultConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let same = local.salt == imported.salt
        && local.verifier == imported.verifier
        && local.kdf_algorithm == imported.kdf_algorithm
        && local.kdf_params == imported.kdf_params
        && local.cipher == imported.cipher;
    if same {
        Ok(())
    } else {
        Err("Local Vault uses a different master-password configuration; refusing to import encrypted entries into an incompatible Vault.".into())
    }
}

fn set_owner_only_permissions(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, permissions)
            .map_err(|e| format!("set 0600 on {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn write_owner_only_file(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("create {} with 0600: {e}", path.display()))?;
        file.write_all(bytes)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

pub(super) fn print_status(status: &VaultSyncStatus) {
    println!("Vault sync bundle:");
    println!("  path: {}", status.path.display());
    println!("  exists: {}", status.exists);
    if let Some(size) = status.size_bytes {
        println!("  size_bytes: {size}");
    }
    if let Some(modified_at) = status.modified_at.as_deref() {
        println!("  modified_at: {modified_at}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path() -> PathBuf {
        crate::utils::test_fixture_path(format!("tachi-vault-sync-{}.sqlite", uuid::Uuid::new_v4()))
    }

    #[test]
    fn default_vault_sync_path_is_local_only() {
        let path = default_vault_sync_path().expect("default sync path");
        let rendered = path.display().to_string();

        assert!(
            rendered.contains(".tachi"),
            "default sync path should stay under local Tachi state: {rendered}"
        );
        assert!(
            !vault_sync_path_requires_cloud_ack(&path),
            "default sync path must not require cloud acknowledgement: {rendered}"
        );
    }

    /// The KDF profile this repo has ever actually written in production
    /// (`KdfParams::PRODUCTION`), always in the supported set regardless of
    /// build cfg — the safe default for fixtures that need a config which
    /// imports successfully.
    const VALID_KDF_PARAMS: &str = r#"{"m":65536,"t":3,"p":4}"#;

    /// tachi#1080: an unsupported KDF profile a bundle could carry (e.g.
    /// corrupted/hand-edited). Used by tests that assert the day-one-brick
    /// import gate rejects it — see `ensure_importable_kdf`.
    const UNSUPPORTED_KDF_PARAMS: &str = r#"{"m":1,"t":1,"p":1}"#;

    fn sample_config() -> VaultConfig {
        sample_config_with_kdf_params(VALID_KDF_PARAMS)
    }

    fn sample_config_with_kdf_params(kdf_params: &str) -> VaultConfig {
        VaultConfig {
            salt: "salt".to_string(),
            verifier: "verifier".to_string(),
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: kdf_params.to_string(),
            cipher: memcore::vault::VaultCipher::Aes256Gcm,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// tachi#1210: a WHITESPACE-ONLY stored `kdf_algorithm` is not the
    /// schema's "no algorithm recorded" shape (the column is `NOT NULL
    /// DEFAULT 'argon2id'` with no serde default, so the only reachable
    /// empty-shape row is an exact empty string) and must NOT silently widen
    /// the exact-empty default carve-out. It must be refused as an
    /// unrecognized KDF algorithm, the same fail-closed rejection as any
    /// other unsupported label — mirrors `vault_crypto`'s
    /// `seam_whitespace_only_kdf_algorithm_is_refused_not_defaulted` (fixed
    /// by a7449319).
    #[test]
    fn ensure_importable_kdf_rejects_whitespace_only_algorithm() {
        let mut config = sample_config();
        config.kdf_algorithm = " ".to_string();

        let err = ensure_importable_kdf(&config)
            .expect_err("whitespace-only kdf_algorithm must not silently default to argon2id");
        assert!(
            err.to_string().contains("unrecognized KDF"),
            "whitespace-only kdf_algorithm must be rejected as unrecognized, not defaulted: {err}"
        );
    }

    /// tachi#1080/#1210: an exact-empty stored `kdf_algorithm` IS the
    /// schema's reachable "no algorithm recorded" shape (an
    /// older/hand-crafted bundle) and must still fall back to the implicit
    /// `argon2id` default — this is the one carve-out `ensure_importable_kdf`
    /// intentionally keeps.
    #[test]
    fn ensure_importable_kdf_defaults_exact_empty_algorithm_to_argon2id() {
        let mut config = sample_config();
        config.kdf_algorithm = String::new();

        ensure_importable_kdf(&config)
            .expect("exact-empty kdf_algorithm must still default to argon2id");
    }

    #[test]
    fn vault_sync_round_trips_encrypted_rows_without_unlocking() {
        let source_db = temp_db_path();
        let target_db = temp_db_path();
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle_path = dir.path().join("vault.bundle.json");

        let source = open_cli_store(&source_db).expect("source store");
        source
            .vault_set_config(&sample_config())
            .expect("set source config");
        source
            .vault_upsert_entry(&VaultEntry {
                name: "VOYAGE_API_KEY_1".to_string(),
                encrypted_value: "ciphertext".to_string(),
                nonce: "nonce".to_string(),
                secret_type: "api_key".to_string(),
                description: "encrypted voyage key".to_string(),
                allowed_agents: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("upsert source entry");
        source
            .vault_set_rotation(&VaultKeyRotation {
                prefix: "VOYAGE_API_KEY".to_string(),
                current_index: 1,
                total_keys: 1,
                rotation_strategy: "round_robin".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            })
            .expect("set source rotation");

        let status = export_vault_bundle(&source_db, &bundle_path, false, false, &[7u8; 32])
            .expect("export vault sync bundle");
        assert!(status.exists);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&bundle_path)
                .expect("bundle metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        let report = import_vault_bundle(&target_db, &bundle_path, Some(&[7u8; 32]), false)
            .expect("import vault sync bundle");
        assert_eq!(report.entries_imported, 1);
        assert_eq!(report.rotations_imported, 1);
        assert!(report.initialized_vault);

        let target = open_cli_store_read_only(&target_db).expect("target store");
        assert!(target.vault_get_config().expect("target config").is_some());
        assert!(
            target
                .vault_get_entry("VOYAGE_API_KEY_1")
                .expect("target entry")
                .is_some()
        );
        assert!(
            target
                .vault_get_rotation("VOYAGE_API_KEY")
                .expect("target rotation")
                .is_some()
        );

        let _ = std::fs::remove_file(source_db);
        let _ = std::fs::remove_file(target_db);
    }

    #[test]
    fn vault_sync_cloud_path_requires_explicit_allowance() {
        let cloud_path = PathBuf::from(
            "/Users/me/Library/Mobile Documents/com~apple~CloudDocs/Tachi/vault/vault.bundle.json",
        );

        let err = ensure_cloud_export_allowed(&cloud_path, false)
            .expect_err("cloud export should require explicit allowance");
        assert!(err.to_string().contains("--allow-cloud"), "{err}");

        ensure_cloud_export_allowed(&cloud_path, true)
            .expect("explicit cloud allowance should pass");
    }

    #[test]
    fn vault_sync_rejects_tampered_signed_bundle() {
        let source_db = temp_db_path();
        let target_db = temp_db_path();
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle_path = dir.path().join("vault.bundle.json");

        let source = open_cli_store(&source_db).expect("source store");
        source
            .vault_set_config(&sample_config())
            .expect("set source config");
        source
            .vault_upsert_entry(&VaultEntry {
                name: "OPENAI_API_KEY_1".to_string(),
                encrypted_value: "ciphertext".to_string(),
                nonce: "nonce".to_string(),
                secret_type: "api_key".to_string(),
                description: "original".to_string(),
                allowed_agents: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("upsert source entry");
        export_vault_bundle(&source_db, &bundle_path, false, false, &[7u8; 32])
            .expect("export vault sync bundle");

        let raw = std::fs::read_to_string(&bundle_path).expect("read bundle");
        let mut json: serde_json::Value = serde_json::from_str(&raw).expect("parse bundle json");
        json["entries"][0]["encrypted_value"] = serde_json::json!("attacker-ciphertext");
        std::fs::write(&bundle_path, serde_json::to_string_pretty(&json).unwrap())
            .expect("write tampered bundle");

        let err = import_vault_bundle(&target_db, &bundle_path, Some(&[7u8; 32]), false)
            .expect_err("tampered bundle should fail integrity verification");
        assert!(err.to_string().contains("integrity"), "{err}");

        let _ = std::fs::remove_file(source_db);
        let _ = std::fs::remove_file(target_db);
    }

    #[test]
    fn vault_sync_import_rolls_back_when_rotation_write_fails() {
        let source_db = temp_db_path();
        let target_db = temp_db_path();
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle_path = dir.path().join("vault.bundle.json");

        let source = open_cli_store(&source_db).expect("source store");
        source
            .vault_set_config(&sample_config())
            .expect("set source config");
        source
            .vault_upsert_entry(&VaultEntry {
                name: "ROLLBACK_API_KEY_1".to_string(),
                encrypted_value: "ciphertext".to_string(),
                nonce: "nonce".to_string(),
                secret_type: "api_key".to_string(),
                description: "rollback import fixture".to_string(),
                allowed_agents: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("upsert source entry");
        source
            .vault_set_rotation(&VaultKeyRotation {
                prefix: "ROLLBACK_API_KEY".to_string(),
                current_index: 1,
                total_keys: 1,
                rotation_strategy: "round_robin".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            })
            .expect("set source rotation");
        export_vault_bundle(&source_db, &bundle_path, false, false, &[7u8; 32])
            .expect("export vault sync bundle");

        let target = open_cli_store(&target_db).expect("target store");
        drop(target);
        crate::test_support::with_unrestricted_fixture_connection(&target_db, |connection| {
            connection.execute_batch(
                r#"
                DROP TABLE vault_key_rotations;
                CREATE TABLE vault_key_rotations (
                    prefix              TEXT PRIMARY KEY CHECK (prefix != 'ROLLBACK_API_KEY'),
                    current_index       INTEGER NOT NULL DEFAULT 1,
                    total_keys          INTEGER NOT NULL DEFAULT 0,
                    rotation_strategy   TEXT NOT NULL DEFAULT 'round_robin',
                    created_at          TEXT NOT NULL DEFAULT '',
                    updated_at          TEXT NOT NULL DEFAULT ''
                );
                "#,
            )
        })
        .expect("install rotation write fault");

        let err = import_vault_bundle(&target_db, &bundle_path, Some(&[7u8; 32]), false)
            .expect_err("rotation failure should abort import");
        assert!(err.to_string().contains("vault_import_bundle"), "{err}");

        let target = open_cli_store_read_only(&target_db).expect("target read store");
        assert!(
            target.vault_get_config().expect("target config").is_none(),
            "failed import must not initialize target vault config"
        );
        assert!(
            target
                .vault_get_entry("ROLLBACK_API_KEY_1")
                .expect("target entry")
                .is_none(),
            "failed import must not leave imported entries behind"
        );

        let _ = std::fs::remove_file(source_db);
        let _ = std::fs::remove_file(target_db);
    }

    #[test]
    fn vault_sync_unsigned_bundle_requires_explicit_override() {
        let target_db = temp_db_path();
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle_path = dir.path().join("vault.bundle.json");
        let unsigned = VaultSyncBundle {
            bundle_type: BUNDLE_TYPE.to_string(),
            version: BUNDLE_VERSION,
            exported_at: "2026-01-01T00:00:00Z".to_string(),
            vault_config: Some(sample_config()),
            entries: Vec::new(),
            rotations: Vec::new(),
            signature: None,
        };
        std::fs::write(
            &bundle_path,
            serde_json::to_string_pretty(&unsigned).expect("serialize unsigned bundle"),
        )
        .expect("write unsigned bundle");

        let err = import_vault_bundle(&target_db, &bundle_path, None, false)
            .expect_err("unsigned bundle should be rejected by default");
        assert!(err.to_string().contains("--allow-unsigned"), "{err}");

        let report = import_vault_bundle(&target_db, &bundle_path, None, true)
            .expect("explicit unsigned import with supported KDF params should remain available");
        assert!(report.initialized_vault);
        assert_eq!(report.entries_imported, 0);

        let _ = std::fs::remove_file(target_db);
    }

    /// tachi#1080 day-one brick fix: `--allow-unsigned` overrides the
    /// *signature* requirement only, not the KDF-parameter gate. An unsigned
    /// bundle whose `vault_config` carries an unsupported `kdf_params`
    /// profile must still be rejected even with the explicit override —
    /// before this fix the bootstrap path (`local_config.is_none()`) wrote
    /// it unconditionally, which would create a Vault that can never again
    /// be unlocked (see `ensure_importable_kdf`).
    ///
    /// The KDF gate now runs before the target store is ever opened (the gate
    /// moved ahead of `open_cli_store` in `import_vault_bundle`), so a
    /// rejected import must leave the target DB file itself absent — not
    /// merely absent a `vault_config` row in an already-created file. Assert
    /// against the file, not a read-only re-open, because `MemoryStore::
    /// open_read_only` on a path that was never created would itself error
    /// (`SQLITE_OPEN_READ_ONLY` on a missing file), which would mask the very
    /// regression this test exists to catch.
    #[test]
    fn vault_sync_unsigned_bundle_with_unsupported_kdf_is_rejected_even_with_override() {
        let target_db = temp_db_path();
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle_path = dir.path().join("vault.bundle.json");
        let unsigned = VaultSyncBundle {
            bundle_type: BUNDLE_TYPE.to_string(),
            version: BUNDLE_VERSION,
            exported_at: "2026-01-01T00:00:00Z".to_string(),
            vault_config: Some(sample_config_with_kdf_params(UNSUPPORTED_KDF_PARAMS)),
            entries: Vec::new(),
            rotations: Vec::new(),
            signature: None,
        };
        std::fs::write(
            &bundle_path,
            serde_json::to_string_pretty(&unsigned).expect("serialize unsigned bundle"),
        )
        .expect("write unsigned bundle");

        let err = import_vault_bundle(&target_db, &bundle_path, None, true)
            .expect_err("unsupported kdf_params must be rejected even with --allow-unsigned");
        assert!(
            err.to_string().contains("unsupported KDF parameters"),
            "{err}"
        );

        assert!(
            !target_db.exists(),
            "rejected import must not create/open the target DB file at all \
             (would brick the vault): {}",
            target_db.display()
        );

        let _ = std::fs::remove_file(target_db);
    }

    /// tachi#1080 day-one brick fix, end to end: importing a bundle whose
    /// `vault_config` uses the supported (default) KDF profile succeeds;
    /// importing one whose `kdf_params` is an unsupported/corrupted value
    /// (e.g. hand-edited `{"m":1,"t":1,"p":1}`) is rejected BEFORE anything
    /// is written — and, per the follow-up fix that moved the gate ahead of
    /// `open_cli_store`, before the target store is even opened. Before that
    /// follow-up the gate ran *after* `open_cli_store`, which itself creates
    /// the target DB file and runs schema init/migrations — so a rejected
    /// import still left a freshly-created (or freshly-migrated) DB file
    /// behind, even though `vault_config` itself was never written. Before
    /// the original #1080 fix the case "succeeded" outright — the bundle
    /// imported cleanly and initialized a vault_config row — and only later,
    /// at unlock time, would every attempt fail because the stored-config
    /// KDF gate wired elsewhere in #1080 (`parse_stored_kdf_params`) refuses
    /// to derive against the unsupported params: a vault that is
    /// initialized but can never again be opened (a silent brick, not a
    /// decrypt failure). Asserting the target DB file's existence (not just
    /// its `vault_config` row) is what catches that intermediate
    /// too-late-a-gate regression red.
    #[test]
    fn vault_sync_import_rejects_unsupported_kdf_params_before_persisting() {
        // Success case: a supported KDF profile imports and bootstraps fine.
        let source_db = temp_db_path();
        let target_db = temp_db_path();
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle_path = dir.path().join("vault-supported.bundle.json");

        let source = open_cli_store(&source_db).expect("source store");
        source
            .vault_set_config(&sample_config())
            .expect("set source config");
        export_vault_bundle(&source_db, &bundle_path, false, false, &[7u8; 32])
            .expect("export vault sync bundle");

        let report = import_vault_bundle(&target_db, &bundle_path, Some(&[7u8; 32]), false)
            .expect("importing a bundle with supported KDF params must succeed");
        assert!(report.initialized_vault);

        let _ = std::fs::remove_file(source_db);
        let _ = std::fs::remove_file(target_db);

        // Failure case: an unsupported kdf_params profile must be rejected
        // before persisting anything, not merely fail at a later unlock.
        let source_db = temp_db_path();
        let target_db = temp_db_path();
        let bundle_path = dir.path().join("vault-unsupported.bundle.json");

        let source = open_cli_store(&source_db).expect("source store");
        source
            .vault_set_config(&sample_config_with_kdf_params(UNSUPPORTED_KDF_PARAMS))
            .expect("set source config with unsupported kdf_params");
        export_vault_bundle(&source_db, &bundle_path, false, false, &[7u8; 32])
            .expect("export vault sync bundle");

        let err = import_vault_bundle(&target_db, &bundle_path, Some(&[7u8; 32]), false)
            .expect_err("unsupported kdf_params must be rejected before persisting");
        assert!(
            err.to_string().contains("unsupported KDF parameters"),
            "{err}"
        );

        // `target_db` is a brand-new random path that has never been opened
        // before this call. If the KDF gate ran after `open_cli_store` (the
        // bug this test guards against), that call alone would have created
        // the file and run schema init/migrations on it — so asserting only
        // that `vault_config` is unset would pass even with the gate
        // mis-ordered. Assert the file itself was never created.
        assert!(
            !target_db.exists(),
            "rejected import must not create/open the target DB file at all \
             (would brick the vault): {}",
            target_db.display()
        );

        let _ = std::fs::remove_file(source_db);
        let _ = std::fs::remove_file(target_db);
    }

    /// tachi#1110: direct unit coverage of `import_validated_vault_bundle`
    /// itself — the single validating import wrapper `import_vault_bundle`
    /// (and any future tachi-server caller) must route through — rather than
    /// only exercising it transitively via a signed/parsed bundle file. An
    /// unsupported `kdf_params` profile must be rejected before the wrapper
    /// ever calls `MemoryStore::vault_import_bundle_unchecked`, and the
    /// target DB file must not even be created (mirrors the day-one-brick
    /// invariant `vault_sync_import_rejects_unsupported_kdf_params_before_persisting`
    /// already pins for the file-based entry point above).
    ///
    /// Structural-discrimination note: `import_validated_vault_bundle` is a
    /// function extracted by #1110 itself (it did not exist under any name
    /// on pre-#1110 `origin/main`), so a literal red-before/green-after run
    /// against that exact symbol is impossible — there is nothing to invoke
    /// pre-fix. The behavior it encapsulates (validate before persist, gate
    /// before store-open) is not new; it is the same sequence
    /// `import_vault_bundle` already performed inline, and remains covered
    /// end-to-end by `vault_sync_import_rejects_unsupported_kdf_params_before_persisting`
    /// above (unchanged, still green). This test instead pins the newly
    /// extracted wrapper's own contract directly, so a future edit that
    /// reorders validation after the persist call inside this specific
    /// function goes red without needing to route through file I/O/signature
    /// verification to detect it.
    #[test]
    fn import_validated_vault_bundle_rejects_unsupported_kdf_params_before_persist() {
        let target_db = temp_db_path();
        let config = sample_config_with_kdf_params(UNSUPPORTED_KDF_PARAMS);

        let err = import_validated_vault_bundle(&target_db, &config, &[], &[])
            .expect_err("unsupported kdf_params must be rejected before persisting");
        assert!(
            err.to_string().contains("unsupported KDF parameters"),
            "{err}"
        );
        assert!(
            !target_db.exists(),
            "rejected import must not create/open the target DB file at all \
             (would brick the vault): {}",
            target_db.display()
        );

        let _ = std::fs::remove_file(target_db);
    }

    #[test]
    fn entries_only_bundle_omits_vault_config() {
        let source_db = temp_db_path();
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle_path = dir.path().join("vault.bundle.json");

        let source = open_cli_store(&source_db).expect("source store");
        source
            .vault_set_config(&sample_config())
            .expect("set source config");
        source
            .vault_upsert_entry(&VaultEntry {
                name: "TEST_KEY_1".to_string(),
                encrypted_value: "ciphertext".to_string(),
                nonce: "nonce".to_string(),
                secret_type: "api_key".to_string(),
                description: "test".to_string(),
                allowed_agents: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("upsert source entry");

        let status = export_vault_bundle(&source_db, &bundle_path, false, true, &[7u8; 32])
            .expect("export entries-only bundle");
        assert!(status.exists);

        // The bundle must NOT contain vault_config (no salt/verifier).
        let raw = std::fs::read_to_string(&bundle_path).expect("read bundle");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse bundle json");
        assert!(
            json.get("vault_config").is_none() || json["vault_config"].is_null(),
            "entries-only bundle must omit vault_config, got: {raw}"
        );

        // read_bundle_vault_config must return None for entries-only.
        let config = read_bundle_vault_config(&bundle_path).expect("read config");
        assert!(
            config.is_none(),
            "entries-only bundle should have None vault_config"
        );

        let _ = std::fs::remove_file(source_db);
    }

    #[test]
    fn entries_only_bundle_cannot_bootstrap_new_vault() {
        let source_db = temp_db_path();
        let target_db = temp_db_path();
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle_path = dir.path().join("vault.bundle.json");

        let source = open_cli_store(&source_db).expect("source store");
        source
            .vault_set_config(&sample_config())
            .expect("set source config");
        source
            .vault_upsert_entry(&VaultEntry {
                name: "TEST_KEY_1".to_string(),
                encrypted_value: "ciphertext".to_string(),
                nonce: "nonce".to_string(),
                secret_type: "api_key".to_string(),
                description: "test".to_string(),
                allowed_agents: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("upsert source entry");

        export_vault_bundle(&source_db, &bundle_path, false, true, &[7u8; 32])
            .expect("export entries-only bundle");

        // Target has NO vault initialized — entries-only import must fail.
        let err = import_vault_bundle(&target_db, &bundle_path, Some(&[7u8; 32]), false)
            .expect_err("entries-only bundle should fail on uninitialized target");
        assert!(
            err.to_string().contains("entries-only"),
            "error should explain entries-only constraint, got: {err}"
        );

        let _ = std::fs::remove_file(source_db);
        let _ = std::fs::remove_file(target_db);
    }
}
