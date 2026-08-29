use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use memcore::vault::{VaultEntry, VaultKeyHealth, SECRET_TYPE_API_KEY};
use tachi_llm::AliasSkipClass;

fn derive_status_vault_key(
    config: &memcore::vault::VaultConfig,
    password: &str,
) -> Result<
    Option<crate::vault_crypto::DerivedVaultKey>,
    crate::vault_crypto::StoredVaultKeyDerivationError,
> {
    match crate::vault_crypto::derive_verified_key_from_stored_config(config, password) {
        Ok(key) => Ok(Some(key)),
        Err(crate::vault_crypto::StoredVaultKeyDerivationError::WrongPassword) => Ok(None),
        Err(err) => Err(err),
    }
}

pub(crate) struct KeychainApiKeyScan {
    pub values: Vec<(String, String)>,
    pub dropped: HashMap<String, AliasSkipClass>,
    pub rotation_prefixes: HashSet<String>,
    pub source_readable: bool,
}

pub(crate) fn load_keychain_vault_api_key_values(
    vault_db_path: &Path,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    Ok(load_keychain_vault_api_key_scan(vault_db_path)?.values)
}

pub(crate) fn load_keychain_vault_lane_config_values(
    vault_db_path: &Path,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    Ok(load_keychain_vault_values(vault_db_path, |entry| {
        matches!(
            entry.name.as_str(),
            "EXTRACT_BASE_URL"
                | "EXTRACT_MODEL"
                | "SUMMARY_BASE_URL"
                | "SUMMARY_MODEL"
                | "DISTILL_BASE_URL"
                | "DISTILL_MODEL"
                | "REASONING_BASE_URL"
                | "REASONING_MODEL"
        ) && !entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
    })?
    .values)
}

pub(crate) fn keychain_vault_source_readable(
    vault_db_path: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(load_keychain_vault_values(vault_db_path, |_| false)?.readable)
}

struct KeychainVaultRead {
    readable: bool,
    values: Vec<(String, String)>,
}

fn load_keychain_vault_values(
    vault_db_path: &Path,
    include_entry: impl Fn(&VaultEntry) -> bool,
) -> Result<KeychainVaultRead, Box<dyn std::error::Error>> {
    let mut password = match crate::vault_crypto::read_password_from_macos_keychain() {
        Ok(password) => password,
        Err(err)
            if !cfg!(target_os = "macos")
                || err.starts_with("no vault password found in Keychain")
                || err == "Keychain entry for tachi-vault/default is empty" =>
        {
            return Ok(KeychainVaultRead {
                readable: false,
                values: Vec::new(),
            });
        }
        Err(err) => {
            return Err(Box::new(std::io::Error::other(err)));
        }
    };
    let result = load_keychain_vault_values_with_password(vault_db_path, &password, include_entry);
    crate::vault_crypto::zero_string(&mut password);
    result
}

fn load_keychain_vault_values_with_password(
    vault_db_path: &Path,
    password: &str,
    include_entry: impl Fn(&VaultEntry) -> bool,
) -> Result<KeychainVaultRead, Box<dyn std::error::Error>> {
    if !vault_db_path.exists() {
        return Ok(KeychainVaultRead {
            readable: false,
            values: Vec::new(),
        });
    }
    let vault_db_str = vault_db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Vault DB path contains invalid UTF-8: {}",
                vault_db_path.display()
            ),
        )
    })?;
    let store = memcore::MemoryStore::open_read_only(vault_db_str)?;
    let Some(config) = store.vault_get_config()? else {
        return Ok(KeychainVaultRead {
            readable: false,
            values: Vec::new(),
        });
    };
    let Some(key) = derive_status_vault_key(&config, password)? else {
        return Ok(KeychainVaultRead {
            readable: false,
            values: Vec::new(),
        });
    };

    let mut values = Vec::new();
    for entry in store.vault_list_entries()? {
        if !include_entry(&entry) {
            continue;
        }
        let decrypted =
            crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)?;
        let value = crate::vault_crypto::decode_utf8_zeroizing(
            decrypted,
            crate::vault_ops::VAULT_MATERIALIZATION_INVALID_UTF8,
        )
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        values.push((entry.name, value));
    }
    Ok(KeychainVaultRead {
        readable: true,
        values,
    })
}

fn record_keychain_listed_drop(
    dropped: &mut HashMap<String, AliasSkipClass>,
    name: &str,
    class: AliasSkipClass,
) {
    dropped.entry(name.to_string()).or_insert(class);
}

fn empty_keychain_scan() -> KeychainApiKeyScan {
    KeychainApiKeyScan {
        values: Vec::new(),
        dropped: HashMap::new(),
        rotation_prefixes: HashSet::new(),
        source_readable: false,
    }
}

pub(crate) fn load_keychain_vault_api_key_scan(
    vault_db_path: &Path,
) -> Result<KeychainApiKeyScan, Box<dyn std::error::Error>> {
    let mut password = match crate::vault_crypto::read_password_from_macos_keychain() {
        Ok(password) => password,
        Err(err)
            if !cfg!(target_os = "macos")
                || err.starts_with("no vault password found in Keychain")
                || err == "Keychain entry for tachi-vault/default is empty" =>
        {
            return Ok(empty_keychain_scan());
        }
        Err(err) => return Err(Box::new(std::io::Error::other(err))),
    };

    let result = load_keychain_vault_api_key_scan_with_password(vault_db_path, &password);
    crate::vault_crypto::zero_string(&mut password);
    result
}

fn load_keychain_vault_api_key_scan_with_password(
    vault_db_path: &Path,
    password: &str,
) -> Result<KeychainApiKeyScan, Box<dyn std::error::Error>> {
    if !vault_db_path.exists() {
        return Ok(empty_keychain_scan());
    }
    let vault_db_str = vault_db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Vault DB path contains invalid UTF-8: {}",
                vault_db_path.display()
            ),
        )
    })?;
    let store = memcore::MemoryStore::open_read_only(vault_db_str)?;
    let Some(config) = store.vault_get_config()? else {
        return Ok(empty_keychain_scan());
    };

    // tachi#1080: a wrong Keychain password is the sole benign miss. Invalid
    // salt, KDF format/parameters, derivation failure, and verifier corruption
    // are stored-config integrity failures and must stay loud.
    let Some(key) = derive_status_vault_key(&config, password)? else {
        return Ok(empty_keychain_scan());
    };

    let entries = store.vault_list_entries()?;
    let rotation_prefixes = store
        .vault_list_rotations()?
        .into_iter()
        .map(|rotation| rotation.prefix)
        .collect::<HashSet<_>>();
    let key_health_rows = store.vault_list_key_health(None)?;
    let mut scan = scan_keychain_api_key_entries(
        entries,
        &key,
        &rotation_prefixes,
        &key_health_rows,
        Utc::now(),
    )?;
    scan.rotation_prefixes = rotation_prefixes;
    Ok(scan)
}

fn scan_keychain_api_key_entries(
    entries: Vec<VaultEntry>,
    key: &crate::vault_crypto::DerivedVaultKey,
    rotation_prefixes: &HashSet<String>,
    key_health_rows: &[VaultKeyHealth],
    now: DateTime<Utc>,
) -> Result<KeychainApiKeyScan, Box<dyn std::error::Error>> {
    let mut values = Vec::new();
    let mut dropped = HashMap::new();
    for entry in entries {
        if entry.secret_type != SECRET_TYPE_API_KEY {
            record_keychain_listed_drop(&mut dropped, &entry.name, AliasSkipClass::ListedWrongType);
            continue;
        }
        if !crate::provider_config::is_provider_api_key_name(&entry.name) {
            record_keychain_listed_drop(
                &mut dropped,
                &entry.name,
                AliasSkipClass::ListedNotModelProvider,
            );
            continue;
        }
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            record_keychain_listed_drop(&mut dropped, &entry.name, AliasSkipClass::ListedFenced);
            continue;
        }
        if let Some(class) =
            keychain_unusable_skip_class(&entry.name, rotation_prefixes, key_health_rows, now)
        {
            record_keychain_listed_drop(&mut dropped, &entry.name, class);
            continue;
        }
        let decrypted =
            crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)?;
        let value = crate::vault_crypto::decode_utf8_zeroizing(
            decrypted,
            crate::vault_ops::VAULT_MATERIALIZATION_INVALID_UTF8,
        )
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if value.trim().is_empty() {
            record_keychain_listed_drop(&mut dropped, &entry.name, AliasSkipClass::ListedEmpty);
            continue;
        }
        values.push((entry.name, value));
    }
    Ok(KeychainApiKeyScan {
        values,
        dropped,
        rotation_prefixes: rotation_prefixes.clone(),
        source_readable: true,
    })
}

/// Resolve health under the same identity used by unlocked pool admission:
/// configured rotation members belong to the prefix pool, while standalone
/// entries use their own name as both logical name and key id. The rows and
/// `now` come from one scan so the returned drop class cannot be rewritten by
/// a later health read.
fn keychain_unusable_skip_class(
    entry_name: &str,
    rotation_prefixes: &HashSet<String>,
    key_health_rows: &[VaultKeyHealth],
    now: DateTime<Utc>,
) -> Option<AliasSkipClass> {
    let logical_name = crate::provider_config::parse_rotation_member_name(entry_name)
        .and_then(|(prefix, _)| rotation_prefixes.contains(prefix).then_some(prefix))
        .unwrap_or(entry_name);
    key_health_rows
        .iter()
        .find(|health| health.logical_name == logical_name && health.key_id == entry_name)
        .and_then(|health| crate::vault_ops::unusable_skip_class(health, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use chrono::Duration;
    use memcore::vault::{VaultCipher, VaultConfig};

    fn stored_config(password: &str) -> VaultConfig {
        let salt = crate::vault_crypto::generate_salt();
        let key = crate::vault_crypto::DerivedVaultKey::derive(password, &salt).expect("derive");
        let verifier = crate::vault_crypto::create_verifier(key.bytes()).expect("verifier");
        VaultConfig {
            salt: B64.encode(salt),
            verifier,
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: crate::vault_crypto::active_kdf_params_json().to_string(),
            cipher: VaultCipher::Aes256Gcm,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn test_key() -> crate::vault_crypto::DerivedVaultKey {
        crate::vault_crypto::DerivedVaultKey::derive("keychain-scan-test", &[7_u8; 32])
            .expect("derive test key")
    }

    fn encrypted_entry(
        name: &str,
        value: &str,
        key: &crate::vault_crypto::DerivedVaultKey,
    ) -> VaultEntry {
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(key.bytes(), value.as_bytes()).expect("encrypt entry");
        VaultEntry {
            name: name.to_string(),
            encrypted_value,
            nonce,
            secret_type: SECRET_TYPE_API_KEY.to_string(),
            ..VaultEntry::default()
        }
    }

    fn health_row(
        logical_name: &str,
        key_id: &str,
        status: &str,
        now: DateTime<Utc>,
    ) -> VaultKeyHealth {
        VaultKeyHealth {
            logical_name: logical_name.to_string(),
            key_id: key_id.to_string(),
            status: status.to_string(),
            updated_at: now.to_rfc3339(),
            ..VaultKeyHealth::default()
        }
    }

    #[test]
    fn keychain_scan_drops_persisted_unusable_health() {
        let key = test_key();
        let now = Utc::now();
        let entries = [
            "AUTH_FAILED_API_KEY",
            "DISABLED_API_KEY",
            "EXHAUSTED_API_KEY",
            "COOLING_API_KEY",
            "EXPIRED_COOLDOWN_API_KEY",
            "HEALTHY_API_KEY",
        ]
        .into_iter()
        .map(|name| encrypted_entry(name, "fixture-value", &key))
        .collect();

        let mut auth_failed = health_row(
            "AUTH_FAILED_API_KEY",
            "AUTH_FAILED_API_KEY",
            "auth_failed",
            now,
        );
        auth_failed.auth_failed = true;
        let mut disabled = health_row("DISABLED_API_KEY", "DISABLED_API_KEY", "ok", now);
        disabled.disabled = true;
        let exhausted = health_row("EXHAUSTED_API_KEY", "EXHAUSTED_API_KEY", "exhausted", now);
        let mut cooling = health_row("COOLING_API_KEY", "COOLING_API_KEY", "rate_limited", now);
        cooling.cooldown_until = Some((now + Duration::minutes(5)).to_rfc3339());
        let mut expired = health_row(
            "EXPIRED_COOLDOWN_API_KEY",
            "EXPIRED_COOLDOWN_API_KEY",
            "rate_limited",
            now,
        );
        expired.cooldown_until = Some((now - Duration::minutes(5)).to_rfc3339());

        let scan = scan_keychain_api_key_entries(
            entries,
            &key,
            &HashSet::new(),
            &[auth_failed, disabled, exhausted, cooling, expired],
            now,
        )
        .expect("scan");

        let admitted = scan
            .values
            .into_iter()
            .map(|(name, _)| name)
            .collect::<HashSet<_>>();
        assert_eq!(
            admitted,
            HashSet::from([
                "EXPIRED_COOLDOWN_API_KEY".to_string(),
                "HEALTHY_API_KEY".to_string(),
            ])
        );
        assert_eq!(
            scan.dropped.get("AUTH_FAILED_API_KEY"),
            Some(&AliasSkipClass::ListedUnusableAuthFailed)
        );
        assert_eq!(
            scan.dropped.get("DISABLED_API_KEY"),
            Some(&AliasSkipClass::ListedUnusableDisabled)
        );
        assert_eq!(
            scan.dropped.get("EXHAUSTED_API_KEY"),
            Some(&AliasSkipClass::ListedUnusableExhausted)
        );
        assert_eq!(
            scan.dropped.get("COOLING_API_KEY"),
            Some(&AliasSkipClass::ListedUnusableCooldown)
        );
    }

    #[test]
    fn keychain_scan_classifies_wrong_type_before_provider_name_shape() {
        let key = test_key();
        let mut entry = encrypted_entry("NOT_A_PROVIDER_SECRET", "fixture-value", &key);
        entry.secret_type = "other".to_string();

        let scan =
            scan_keychain_api_key_entries(vec![entry], &key, &HashSet::new(), &[], Utc::now())
                .expect("scan");

        assert_eq!(
            scan.dropped.get("NOT_A_PROVIDER_SECRET"),
            Some(&AliasSkipClass::ListedWrongType),
            "Keychain and unlocked scans must classify a wrong-type secret identically"
        );
    }

    #[test]
    fn keychain_scan_drops_all_unusable_configured_rotation_members() {
        let key = test_key();
        let now = Utc::now();
        let prefix = "ROTATION_API_KEY";
        let entries = vec![
            encrypted_entry("ROTATION_API_KEY_1", "fixture-one", &key),
            encrypted_entry("ROTATION_API_KEY_2", "fixture-two", &key),
        ];
        let mut member_one = health_row(prefix, "ROTATION_API_KEY_1", "auth_failed", now);
        member_one.auth_failed = true;
        let mut member_two = health_row(prefix, "ROTATION_API_KEY_2", "ok", now);
        member_two.disabled = true;

        let scan = scan_keychain_api_key_entries(
            entries,
            &key,
            &HashSet::from([prefix.to_string()]),
            &[member_one, member_two],
            now,
        )
        .expect("scan");

        assert!(
            scan.values.is_empty(),
            "unusable rotation members must not enter a pool"
        );
        assert_eq!(
            scan.dropped.get("ROTATION_API_KEY_1"),
            Some(&AliasSkipClass::ListedUnusableAuthFailed)
        );
        assert_eq!(
            scan.dropped.get("ROTATION_API_KEY_2"),
            Some(&AliasSkipClass::ListedUnusableDisabled)
        );
    }

    #[test]
    fn status_wrong_keychain_password_is_a_benign_miss() {
        let config = stored_config("correct-pw");
        let result = derive_status_vault_key(&config, "wrong-pw")
            .expect("wrong Keychain password must not fail status health");
        assert!(result.is_none(), "wrong password must map to empty status");
    }

    #[test]
    fn status_invalid_stored_salt_stays_loud() {
        let mut config = stored_config("correct-pw");
        config.salt = "not base64!".to_string();
        let err = derive_status_vault_key(&config, "correct-pw")
            .expect_err("invalid stored salt must fail status health");
        assert!(
            matches!(
                err,
                crate::vault_crypto::StoredVaultKeyDerivationError::InvalidSalt(_)
            ),
            "invalid salt must not degrade to empty status"
        );
    }

    #[test]
    fn status_corrupt_stored_verifier_stays_loud() {
        let mut config = stored_config("correct-pw");
        config.verifier = "not-a-verifier".to_string();
        let err = derive_status_vault_key(&config, "correct-pw")
            .expect_err("corrupt stored verifier must fail status health");
        assert!(
            matches!(
                err,
                crate::vault_crypto::StoredVaultKeyDerivationError::CorruptVerifier(_)
            ),
            "corrupt verifier must not degrade to empty status"
        );
    }

    #[test]
    fn keychain_health_store_failure_stays_loud() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("memory.db");
        let password = "keychain-health-failure";
        {
            let store = memcore::MemoryStore::open(db.to_str().expect("UTF-8 db path"))
                .expect("create store");
            store
                .vault_set_config(&stored_config(password))
                .expect("seed vault config");
        }

        // MemoryStore connections deny trigger DDL. Use the unrestricted
        // second connection required by the repository failure-injection rule.
        let raw = rusqlite::Connection::open(&db).expect("open unrestricted connection");
        raw.execute_batch("DROP TABLE vault_key_health;")
            .expect("remove health table");

        let err = match load_keychain_vault_api_key_scan_with_password(&db, password) {
            Ok(_) => panic!("health-store failure must not become a locked miss"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("vault_key_health"), "{err}");
    }

    #[test]
    fn keychain_password_with_missing_database_is_a_benign_miss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("missing.db");

        let scan = load_keychain_vault_api_key_scan_with_password(&db, "unused-password")
            .expect("missing DB must allow the caller's fallback path");

        assert!(scan.values.is_empty());
        assert!(scan.dropped.is_empty());
        assert!(scan.rotation_prefixes.is_empty());
    }
}
