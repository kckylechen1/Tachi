use super::run_sync_action;
use crate::bootstrap::vault_cli::keys::{vault_init_with_password, vault_upsert_secret_with_key};
use crate::bootstrap::{open_cli_store, open_cli_store_read_only};
use crate::test_support::EnvRestore;
use memcore::vault::VaultEntry;
use std::io::Write;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::VaultAction;

const FIXTURE_PASSWORD: &str = "sync-cli-fixture-password";

struct BundleFixture {
    dir: tempfile::TempDir,
    source_db: PathBuf,
    bundle: PathBuf,
    key: crate::vault_crypto::DerivedVaultKey,
}

impl BundleFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("fixture tempdir");
        let source_db = dir.path().join("source.db");
        let bundle = dir.path().join("bundle.json");
        let key = vault_init_with_password(&source_db, FIXTURE_PASSWORD.to_string())
            .expect("initialize source vault");
        Self {
            dir,
            source_db,
            bundle,
            key,
        }
    }

    fn seed_opaque_entry(&self) {
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(self.key.bytes(), b"opaque-fixture-bytes")
                .expect("encrypt opaque fixture");
        open_cli_store(&self.source_db)
            .expect("open source vault")
            .vault_upsert_entry(&VaultEntry {
                name: "UNTRACKED_OPAQUE_PAYLOAD".to_string(),
                encrypted_value,
                nonce,
                secret_type: "opaque".to_string(),
                description: "opaque fixture".to_string(),
                allowed_agents: None,
                created_at: "2026-08-31T00:00:00Z".to_string(),
                updated_at: "2026-08-31T00:00:00Z".to_string(),
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("seed opaque fixture");
    }

    fn seed_account_entry(&self) {
        vault_upsert_secret_with_key(
            &self.source_db,
            &self.key,
            "DEEPSEEK_API_KEY",
            "api_key",
            "account fixture",
            "account-fixture-bytes".to_string(),
        )
        .expect("seed tracked account fixture");
    }

    fn seed_lane_slot(&self) {
        vault_upsert_secret_with_key(
            &self.source_db,
            &self.key,
            "EXTRACT_API_KEY",
            "api_key",
            "lane slot fixture",
            "account-fixture-bytes".to_string(),
        )
        .expect("seed lane slot fixture");
    }

    fn export(&self, unsigned: bool) {
        crate::bootstrap::vault_sync::export_vault_bundle(
            &self.source_db,
            &self.bundle,
            false,
            false,
            self.key.bytes(),
        )
        .expect("export fixture bundle");
        if unsigned {
            let raw = std::fs::read_to_string(&self.bundle).expect("read signed fixture bundle");
            let mut json: serde_json::Value =
                serde_json::from_str(&raw).expect("parse signed fixture bundle");
            json["signature"] = serde_json::Value::Null;
            std::fs::write(
                &self.bundle,
                serde_json::to_vec_pretty(&json).expect("serialize unsigned fixture bundle"),
            )
            .expect("write unsigned fixture bundle");
        }
    }

    fn remove_lane_slots_from_bundle(&self) {
        let raw = std::fs::read_to_string(&self.bundle).expect("read unsigned fixture bundle");
        let mut json: serde_json::Value =
            serde_json::from_str(&raw).expect("parse unsigned fixture bundle");
        json["entries"]
            .as_array_mut()
            .expect("fixture bundle entries")
            .retain(|entry| {
                let name = entry["name"].as_str().expect("fixture entry name");
                !crate::vault_ops::is_lane_slot_secret_name(name)
            });
        std::fs::write(
            &self.bundle,
            serde_json::to_vec_pretty(&json).expect("serialize slot-free fixture bundle"),
        )
        .expect("write slot-free fixture bundle");
    }
}

fn sync_import_action(input: &Path, password_file: Option<PathBuf>, keychain: bool) -> VaultAction {
    VaultAction::SyncImport {
        input: Some(input.to_path_buf()),
        allow_unsigned: true,
        stdin_password: false,
        keychain,
        password_file,
        insecure_password_file: false,
    }
}

fn write_private_password(path: &Path) {
    let mut file = std::fs::File::create(path).expect("create fixture password file");
    file.write_all(FIXTURE_PASSWORD.as_bytes())
        .expect("write fixture password file");
    file.write_all(b"\n")
        .expect("terminate fixture password file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("set fixture password permissions");
    }
}

fn assert_entry_bytes_preserved(source_db: &Path, target_db: &Path, name: &str) {
    let source =
        open_cli_store_read_only(&source_db.to_path_buf()).expect("open source read-only vault");
    let target =
        open_cli_store_read_only(&target_db.to_path_buf()).expect("open target read-only vault");
    let source_entry = source
        .vault_get_entry(name)
        .expect("read source entry")
        .expect("source entry exists");
    let target_entry = target
        .vault_get_entry(name)
        .expect("read target entry")
        .expect("target entry exists");
    assert_eq!(target_entry.encrypted_value, source_entry.encrypted_value);
    assert_eq!(target_entry.nonce, source_entry.nonce);
    assert_eq!(target_entry.secret_type, source_entry.secret_type);
    assert_eq!(target_entry.description, source_entry.description);
}

#[test]
fn unsigned_full_bundle_untracked_opaque_import_skips_password_and_keychain_at_cli_wrapper() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _keychain_missing = EnvRestore::set("TACHI_TEST_FORCE_KEYCHAIN_MISSING", "1");

    let fixture = BundleFixture::new();
    fixture.seed_opaque_entry();
    fixture.export(true);

    let missing_password_file = fixture.dir.path().join("password-must-not-be-read");
    let password_target = fixture.dir.path().join("password-target.db");
    run_sync_action(
        &password_target,
        sync_import_action(&fixture.bundle, Some(missing_password_file), false),
    )
    .expect("unsigned opaque import must not read the missing password file");
    assert_entry_bytes_preserved(
        &fixture.source_db,
        &password_target,
        "UNTRACKED_OPAQUE_PAYLOAD",
    );

    let keychain_target = fixture.dir.path().join("keychain-target.db");
    run_sync_action(
        &keychain_target,
        sync_import_action(&fixture.bundle, None, true),
    )
    .expect("unsigned opaque import must not read the Keychain");
    assert_entry_bytes_preserved(
        &fixture.source_db,
        &keychain_target,
        "UNTRACKED_OPAQUE_PAYLOAD",
    );
}

#[test]
fn unsigned_cli_wrapper_requires_password_for_lane_slot_binding() {
    let fixture = BundleFixture::new();
    fixture.seed_account_entry();
    fixture.seed_lane_slot();
    fixture.export(true);

    let target = fixture.dir.path().join("lane-slot-target.db");
    let missing_password_file = fixture.dir.path().join("lane-slot-password-must-not-exist");
    let error = run_sync_action(
        &target,
        sync_import_action(&fixture.bundle, Some(missing_password_file), false),
    )
    .expect_err("unsigned lane-slot import must require a verified key")
    .to_string();
    assert!(error.contains("Failed to inspect password file"), "{error}");
    assert!(
        !target.exists(),
        "wrapper key guard must run before target open"
    );
}

#[test]
fn unsigned_cli_wrapper_requires_password_for_tracked_account_custody() {
    let fixture = BundleFixture::new();
    fixture.seed_account_entry();
    fixture.seed_lane_slot();
    fixture.export(true);
    fixture.remove_lane_slots_from_bundle();

    let raw = std::fs::read_to_string(&fixture.bundle).expect("read slot-free bundle");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("parse slot-free bundle");
    assert!(
        json["entries"]
            .as_array()
            .expect("slot-free bundle entries")
            .iter()
            .all(|entry| {
                let name = entry["name"].as_str().expect("slot-free entry name");
                !crate::vault_ops::is_lane_slot_secret_name(name)
            }),
        "tracked-account regression must not be satisfied by a slot shortcut"
    );
    let source =
        open_cli_store_read_only(&fixture.source_db).expect("open tracked source read-only vault");
    let transaction = source
        .begin_vault_read_transaction_shared()
        .expect("begin tracked source metadata snapshot");
    assert!(
        transaction
            .vault_account_entry_is_tracked("DEEPSEEK_API_KEY")
            .expect("read tracked account metadata"),
        "source DB must retain canonical account custody"
    );
    assert!(
        transaction
            .vault_get_entry("EXTRACT_API_KEY")
            .expect("read retained source slot")
            .is_some(),
        "source DB must retain the slot binding used to create custody"
    );
    let missing_password_file = fixture.dir.path().join("tracked-password-must-not-exist");
    let error = run_sync_action(
        &fixture.source_db,
        sync_import_action(&fixture.bundle, Some(missing_password_file), false),
    )
    .expect_err("unsigned tracked-account import must require a verified key")
    .to_string();
    assert!(error.contains("Failed to inspect password file"), "{error}");
}

#[test]
fn signed_cli_wrapper_still_rejects_invalid_signature() {
    let fixture = BundleFixture::new();
    fixture.seed_opaque_entry();
    fixture.export(false);

    let raw = std::fs::read_to_string(&fixture.bundle).expect("read signed fixture bundle");
    let mut json: serde_json::Value = serde_json::from_str(&raw).expect("parse signed bundle");
    json["entries"][0]["description"] = serde_json::Value::String("tampered".to_string());
    std::fs::write(
        &fixture.bundle,
        serde_json::to_vec_pretty(&json).expect("serialize tampered bundle"),
    )
    .expect("write tampered bundle");

    let password_file = fixture.dir.path().join("signed-password.txt");
    write_private_password(&password_file);
    let target = fixture.dir.path().join("signed-invalid-target.db");
    let error = run_sync_action(
        &target,
        sync_import_action(&fixture.bundle, Some(password_file), false),
    )
    .expect_err("signed bundle with invalid signature must be rejected")
    .to_string();
    assert!(error.contains("integrity verification failed"), "{error}");
    assert!(
        !target.exists(),
        "signature failure must precede target open"
    );
}
