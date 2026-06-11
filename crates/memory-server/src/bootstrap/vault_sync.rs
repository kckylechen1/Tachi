use std::path::{Path, PathBuf};

use chrono::Utc;
use memory_core::vault::{VaultConfig, VaultEntry, VaultKeyRotation};
use serde::{Deserialize, Serialize};

use super::{open_cli_store, open_cli_store_read_only};

const BUNDLE_TYPE: &str = "tachi.vault.bundle";
const BUNDLE_VERSION: u32 = 1;
const BUNDLE_SIGNATURE_ALGORITHM: &str = "aes-256-gcm-aad-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultSyncBundle {
    bundle_type: String,
    version: u32,
    exported_at: String,
    vault_config: VaultConfig,
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
    #[cfg(target_os = "macos")]
    {
        Ok(home
            .join("Library")
            .join("Mobile Documents")
            .join("com~apple~CloudDocs")
            .join("Tachi")
            .join("vault")
            .join("vault.bundle.json"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(home
            .join(".tachi")
            .join("sync")
            .join("vault")
            .join("vault.bundle.json"))
    }
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

    let mut bundle = VaultSyncBundle {
        bundle_type: BUNDLE_TYPE.to_string(),
        version: BUNDLE_VERSION,
        exported_at: Utc::now().to_rfc3339(),
        vault_config,
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
            "Refusing to export Vault sync bundle to cloud-synced path {} without --allow-cloud. The bundle contains encrypted Vault entries plus password verifier material and is not integrity signed; choose --output outside cloud storage or re-run with --allow-cloud.",
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

    let store = open_cli_store(global_db_path)?;
    let local_config = store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?;
    let initialized_vault = local_config.is_none();
    if let Some(local_config) = local_config.as_ref() {
        ensure_same_vault(local_config, &bundle.vault_config)?;
    }

    store
        .vault_set_config(&bundle.vault_config)
        .map_err(|e| format!("vault_set_config: {e}"))?;
    for entry in &bundle.entries {
        store
            .vault_upsert_entry(entry)
            .map_err(|e| format!("vault_upsert_entry({}): {e}", entry.name))?;
    }
    for rotation in &bundle.rotations {
        store
            .vault_set_rotation(rotation)
            .map_err(|e| format!("vault_set_rotation({}): {e}", rotation.prefix))?;
    }

    Ok(VaultSyncImportReport {
        path: input.display().to_string(),
        entries_imported: bundle.entries.len(),
        rotations_imported: bundle.rotations.len(),
        initialized_vault,
    })
}

pub(super) fn read_bundle_vault_config(
    input: &Path,
) -> Result<VaultConfig, Box<dyn std::error::Error>> {
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
    if bundle.vault_config.salt.trim().is_empty() || bundle.vault_config.verifier.trim().is_empty()
    {
        return Err("Vault sync bundle is missing vault_config salt/verifier".into());
    }
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
        std::env::temp_dir().join(format!("tachi-vault-sync-{}.sqlite", uuid::Uuid::new_v4()))
    }

    fn sample_config() -> VaultConfig {
        VaultConfig {
            salt: "salt".to_string(),
            verifier: "verifier".to_string(),
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: r#"{"m":1,"t":1,"p":1}"#.to_string(),
            cipher: "aes-256-gcm".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
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

        let status = export_vault_bundle(&source_db, &bundle_path, false, &[7u8; 32])
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
        assert!(target
            .vault_get_entry("VOYAGE_API_KEY_1")
            .expect("target entry")
            .is_some());
        assert!(target
            .vault_get_rotation("VOYAGE_API_KEY")
            .expect("target rotation")
            .is_some());

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
        export_vault_bundle(&source_db, &bundle_path, false, &[7u8; 32])
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
    fn vault_sync_unsigned_bundle_requires_explicit_override() {
        let target_db = temp_db_path();
        let dir = tempfile::tempdir().expect("tempdir");
        let bundle_path = dir.path().join("vault.bundle.json");
        let unsigned = VaultSyncBundle {
            bundle_type: BUNDLE_TYPE.to_string(),
            version: BUNDLE_VERSION,
            exported_at: "2026-01-01T00:00:00Z".to_string(),
            vault_config: sample_config(),
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
            .expect("explicit unsigned import should remain available");
        assert!(report.initialized_vault);
        assert_eq!(report.entries_imported, 0);

        let _ = std::fs::remove_file(target_db);
    }
}
