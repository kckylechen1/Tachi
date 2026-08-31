use super::super::{open_cli_store, open_cli_store_read_only};
use super::daemon::daemon_matches_vault_db;
use super::keys::{
    canonical_provider_key_defs, decrypt_named_secret_value,
    derive_verified_vault_key_from_password, vault_init_with_password,
    vault_upsert_secret_with_key,
};
use super::output::{lease_api_key_from_store, vault_get_output};
use super::password::{
    read_password_file, read_vault_init_password, read_vault_init_password_stdin_lines,
    read_vault_password,
};
use super::secret_actions::ZeroizingSecretString;
use crate::test_support::EnvRestore;
use std::io::{Cursor, Read};
use std::path::Path;

#[cfg(unix)]
fn make_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("set owner-only permissions");
}

#[cfg(not(unix))]
fn make_owner_only(_path: &Path) {}

#[cfg(unix)]
fn make_group_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640))
        .expect("set group-readable permissions");
}

fn config_for_password(password: &str) -> memcore::vault::VaultConfig {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let salt = crate::vault_crypto::generate_salt();
    let key =
        crate::vault_crypto::DerivedVaultKey::derive(password, &salt).expect("derive test key");
    let verifier = crate::vault_crypto::create_verifier(key.bytes()).expect("create verifier");
    memcore::vault::VaultConfig {
        salt: B64.encode(salt),
        verifier,
        kdf_algorithm: "argon2id".to_string(),
        kdf_params: crate::vault_crypto::active_kdf_params_json().to_string(),
        cipher: memcore::vault::VaultCipher::Aes256Gcm,
        created_at: "2026-06-14T00:00:00Z".to_string(),
        updated_at: "2026-06-14T00:00:00Z".to_string(),
    }
}

fn string_is_zeroed(value: &str) -> bool {
    value.as_bytes().iter().all(|byte| *byte == 0)
}

#[test]
fn cli_secret_guard_zeroes_plaintext_on_early_error() {
    let mut secret = "entered-secret-that-must-not-survive".to_string();
    let result: Result<(), &str> = {
        let _secret = ZeroizingSecretString(&mut secret);
        Err("simulated store failure")
    };

    assert_eq!(result, Err("simulated store failure"));
    assert!(
        string_is_zeroed(&secret),
        "CLI secret buffer was not zeroed on early return"
    );
}

fn daemon_info(global_db: Option<&Path>) -> crate::cli_client::DaemonInfo {
    crate::cli_client::DaemonInfo {
        url: "http://127.0.0.1:6919/mcp".to_string(),
        global_db: global_db.map(|path| path.display().to_string()),
        project_db: None,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        pid: Some(std::process::id() as i64),
    }
}

#[test]
fn vault_cli_daemon_forwarding_requires_matching_global_db() {
    let tachi_db = Path::new("/tmp/tachi/global/memory.db");
    let openclaw_db = Path::new("/tmp/openclaw/agents/main/memory.db");

    assert!(daemon_matches_vault_db(
        &daemon_info(Some(tachi_db)),
        tachi_db
    ));
    assert!(!daemon_matches_vault_db(
        &daemon_info(Some(openclaw_db)),
        tachi_db
    ));
}

#[test]
fn vault_cli_daemon_forwarding_accepts_legacy_missing_global_db() {
    let tachi_db = Path::new("/tmp/tachi/global/memory.db");

    assert!(daemon_matches_vault_db(&daemon_info(None), tachi_db));
}

#[test]
fn stdin_init_password_reads_two_lines_without_waiting_for_eof() {
    let mut input =
        Cursor::new("correct horse battery staple\ncorrect horse battery staple\nextra\n");
    let (password, confirm) =
        read_vault_init_password_stdin_lines(&mut input, None, false).expect("stdin lines");

    assert_eq!(password, "correct horse battery staple");
    assert_eq!(confirm, "correct horse battery staple");
    let mut remaining = String::new();
    input
        .read_to_string(&mut remaining)
        .expect("read remaining stdin");
    assert_eq!(remaining, "extra\n");
}

#[test]
fn stdin_init_password_rejects_missing_confirmation_file_after_first_line() {
    let mut input = Cursor::new("correct horse battery staple\n");
    let missing = std::env::temp_dir().join(format!(
        "tachi-missing-confirm-password-{}",
        uuid::Uuid::new_v4()
    ));

    let err = read_vault_init_password_stdin_lines(&mut input, Some(&missing), false)
        .expect_err("missing confirmation file must fail after reading the password");
    assert!(
        err.to_string().contains("Failed to inspect password file"),
        "{err}"
    );
}

#[test]
fn derive_verified_vault_key_zeroes_password_on_success() {
    let config = config_for_password("correct horse battery staple");
    let mut password = "correct horse battery staple".to_string();

    let _key = derive_verified_vault_key_from_password(&config, &mut password)
        .expect("verified password should derive key");

    assert!(
        string_is_zeroed(&password),
        "password buffer was not zeroed"
    );
}

#[test]
fn derive_verified_vault_key_zeroes_password_on_wrong_password() {
    let config = config_for_password("correct horse battery staple");
    let mut password = "wrong horse battery staple".to_string();

    let err = match derive_verified_vault_key_from_password(&config, &mut password) {
        Ok(_) => panic!("wrong password should fail verification"),
        Err(err) => err,
    };

    assert!(err.to_string().contains("Wrong password"), "{err}");
    assert!(
        string_is_zeroed(&password),
        "password buffer was not zeroed"
    );
}

#[test]
fn derive_verified_vault_key_zeroes_password_on_invalid_salt() {
    let mut config = config_for_password("correct horse battery staple");
    config.salt = "not-valid-base64%%%".to_string();
    let mut password = "correct horse battery staple".to_string();

    let err = derive_verified_vault_key_from_password(&config, &mut password)
        .expect_err("invalid salt must fail");
    assert!(err.to_string().contains("Invalid vault salt"), "{err}");
    assert!(
        string_is_zeroed(&password),
        "password buffer was not zeroed after invalid salt"
    );
}

#[test]
fn setup_keys_init_and_upsert_roundtrip() {
    // `tachi vault setup-keys` (and the wizard funnel) rely on
    // vault_init_with_password + vault_upsert_secret_with_key. Verify the
    // value is stored encrypted and decrypts back to the original.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");

    let key = vault_init_with_password(&db_path, "correct horse battery staple".to_string())
        .expect("init vault");

    let is_new = vault_upsert_secret_with_key(
        &db_path,
        &key,
        "SILICONFLOW_API_KEY",
        "api_key",
        "",
        "sk-siliconflow-secret".to_string(),
    )
    .expect("upsert secret");
    assert!(is_new, "first write should create a new entry");

    let store = open_cli_store_read_only(&db_path).expect("open store");
    let entry = store
        .vault_get_entry("SILICONFLOW_API_KEY")
        .expect("get entry")
        .expect("entry exists");
    assert_eq!(entry.secret_type, "api_key");
    let decrypted = crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)
        .expect("decrypt");
    assert_eq!(
        String::from_utf8(decrypted).expect("utf8"),
        "sk-siliconflow-secret"
    );

    // Re-upsert updates in place (is_new == false), value still recovers.
    let is_new_again = vault_upsert_secret_with_key(
        &db_path,
        &key,
        "SILICONFLOW_API_KEY",
        "api_key",
        "",
        "sk-rotated".to_string(),
    )
    .expect("re-upsert");
    assert!(!is_new_again, "second write should update existing entry");
}

#[test]
fn legacy_vault_upsert_rejects_agent_restricted_existing_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = vault_init_with_password(&db_path, "correct horse battery staple".to_string())
        .expect("init vault");
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(key.bytes(), b"restricted-original").expect("encrypt");
    open_cli_store(&db_path)
        .expect("open fixture")
        .vault_upsert_entry(&memcore::vault::VaultEntry {
            name: "RESTRICTED_SETUP_API_KEY".to_string(),
            encrypted_value,
            nonce,
            secret_type: "api_key".to_string(),
            description: String::new(),
            allowed_agents: Some(vec!["agent-a".to_string()]),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            accessed_at: String::new(),
            access_count: 0,
        })
        .expect("seed restricted entry");

    let error = vault_upsert_secret_with_key(
        &db_path,
        &key,
        "RESTRICTED_SETUP_API_KEY",
        "api_key",
        "",
        "must-not-overwrite".to_string(),
    )
    .expect_err("identity-less setup helper must reject restricted entry")
    .to_string();
    assert!(error.contains("agent-restricted"), "{error}");
    assert!(!error.contains("must-not-overwrite"), "{error}");
    let retained = open_cli_store_read_only(&db_path)
        .expect("reopen fixture")
        .vault_get_entry("RESTRICTED_SETUP_API_KEY")
        .expect("read entry")
        .expect("entry remains");
    assert_eq!(retained.allowed_agents, Some(vec!["agent-a".to_string()]));
}

#[test]
fn vault_upsert_rejects_empty_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = vault_init_with_password(&db_path, "correct horse battery staple".to_string())
        .expect("init vault");
    let err = vault_upsert_secret_with_key(
        &db_path,
        &key,
        "VOYAGE_API_KEY",
        "api_key",
        "",
        String::new(),
    )
    .expect_err("empty value should be rejected");
    assert!(err.to_string().contains("empty"), "{err}");
}

#[test]
fn legacy_vault_upsert_rejects_lane_slot_bypass() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = vault_init_with_password(&db_path, "correct horse battery staple".to_string())
        .expect("init vault");
    let error = vault_upsert_secret_with_key(
        &db_path,
        &key,
        "EXTRACT_API_KEY",
        "api_key",
        "",
        "must-not-bypass-rebind".to_string(),
    )
    .expect_err("legacy helper must not write lane slots")
    .to_string();

    assert!(error.contains("EXTRACT_API_KEY"), "{error}");
    assert!(error.contains("--rebind"), "{error}");
    assert!(!error.contains("must-not-bypass-rebind"), "{error}");
}

#[test]
fn legacy_vault_upsert_rejects_new_credential_bearing_lane_url() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = vault_init_with_password(&db_path, "correct horse battery staple".to_string())
        .expect("init vault");
    let error = vault_upsert_secret_with_key(
        &db_path,
        &key,
        "EXTRACT_BASE_URL",
        "config",
        "",
        "https://user:pass@proxy.example.test/v1/chat".to_string(),
    )
    .expect_err("legacy helper must reject a new credential-bearing lane URL")
    .to_string();

    assert!(error.contains("EXTRACT_BASE_URL"), "{error}");
    assert!(
        error.contains("userinfo") || error.contains("credential"),
        "{error}"
    );
    assert!(!error.contains("user:pass"), "{error}");
    assert!(open_cli_store_read_only(&db_path)
        .expect("reopen fixture")
        .vault_get_entry("EXTRACT_BASE_URL")
        .expect("read refused URL")
        .is_none());
}

#[test]
fn legacy_vault_upsert_refuses_rotation_count_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = vault_init_with_password(&db_path, "correct horse battery staple".to_string())
        .expect("init vault");
    for idx in 1..=2 {
        vault_upsert_secret_with_key(
            &db_path,
            &key,
            &format!("LEGACY_POOL_API_KEY_{idx}"),
            "api_key",
            "",
            format!("key-{idx}"),
        )
        .expect("seed member");
    }
    let store = open_cli_store(&db_path).expect("open writable fixture");
    store
        .vault_set_rotation(&memcore::vault::VaultKeyRotation {
            prefix: "LEGACY_POOL_API_KEY".to_string(),
            current_index: 1,
            total_keys: 2,
            rotation_strategy: "round_robin".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        })
        .expect("seed rotation");
    drop(store);

    let error = vault_upsert_secret_with_key(
        &db_path,
        &key,
        "LEGACY_POOL_API_KEY_3",
        "api_key",
        "",
        "key-three".to_string(),
    )
    .expect_err("legacy upsert must not leave total_keys stale")
    .to_string();
    assert!(
        error.contains("declares 2 keys") && error.contains("has 3"),
        "{error}"
    );
    assert!(open_cli_store_read_only(&db_path)
        .expect("reopen fixture")
        .vault_get_entry("LEGACY_POOL_API_KEY_3")
        .expect("read refused append")
        .is_none());
}

#[test]
fn canonical_provider_keys_dedup_and_exclude_deprecated() {
    let defs = canonical_provider_key_defs(false);
    let names: Vec<&str> = defs.iter().map(|(k, _)| *k).collect();
    assert!(names.contains(&"VOYAGE_API_KEY"));
    assert!(names.contains(&"SILICONFLOW_API_KEY"));
    // No duplicates.
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "keys must be deduped");
    // Deprecated keys (e.g. MINIMAX_API_KEY) excluded by default.
    assert!(!names.contains(&"MINIMAX_API_KEY"));
    // Lane slots are not provider accounts; setup-keys must not mint them.
    assert!(!names.contains(&"EXTRACT_API_KEY"));
    assert!(!names.contains(&"SUMMARY_API_KEY"));
    assert!(!names.contains(&"DISTILL_API_KEY"));
    assert!(!names.contains(&"REASONING_API_KEY"));
    let with_dep = canonical_provider_key_defs(true);
    assert!(with_dep.len() >= defs.len());
}

#[test]
fn vault_upsert_secret_with_key_binds_lane_slot_instead_of_copying() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = vault_init_with_password(&db_path, "correct horse battery staple".to_string())
        .expect("init vault");
    vault_upsert_secret_with_key(
        &db_path,
        &key,
        "DEEPSEEK_API_KEY",
        "api_key",
        "",
        "deepseek-secret".to_string(),
    )
    .expect("account");
    let created = vault_upsert_secret_with_key(
        &db_path,
        &key,
        "EXTRACT_API_KEY",
        "api_key",
        "",
        "deepseek-secret".to_string(),
    )
    .expect("slot bind");
    assert!(created);
    let store = open_cli_store_read_only(&db_path).expect("open store");
    let entry = store
        .vault_get_entry("EXTRACT_API_KEY")
        .expect("get")
        .expect("row");
    let decrypted = crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)
        .expect("decrypt");
    assert_eq!(
        String::from_utf8(decrypted).expect("utf8"),
        "vault:DEEPSEEK_API_KEY"
    );
    let unmatched = vault_upsert_secret_with_key(
        &db_path,
        &key,
        "DISTILL_API_KEY",
        "api_key",
        "",
        "orphan-secret".to_string(),
    )
    .expect_err("unmatched slot bytes must not copy");
    assert!(
        unmatched.to_string().contains("second copy")
            || unmatched.to_string().contains("provider account"),
        "{unmatched}"
    );

    let store = open_cli_store(&db_path).expect("open rw");
    let (_, key_id, value) =
        lease_api_key_from_store(&store, key.bytes(), "EXTRACT_API_KEY").expect("lease slot");
    assert_eq!(key_id, "DEEPSEEK_API_KEY");
    assert_eq!(value, "deepseek-secret");
    assert_ne!(value, "vault:DEEPSEEK_API_KEY");
    let profile_value =
        decrypt_named_secret_value(&store, key.bytes(), "EXTRACT_API_KEY").expect("profile");
    assert_eq!(profile_value, "deepseek-secret");

    store
        .vault_upsert_key_health(&memcore::vault::VaultKeyHealth {
            logical_name: "DEEPSEEK_API_KEY".to_string(),
            key_id: "DEEPSEEK_API_KEY".to_string(),
            status: "disabled".to_string(),
            disabled: true,
            updated_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        })
        .expect("disable account");
    let err = lease_api_key_from_store(&store, key.bytes(), "EXTRACT_API_KEY")
        .expect_err("disabled target must not lease through the slot");
    assert!(err.to_string().contains("No usable API key"), "{err}");
}

#[test]
fn lease_api_key_from_store_ignores_legacy_slot_rotation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = vault_init_with_password(&db_path, "correct horse battery staple".to_string())
        .expect("init vault");
    vault_upsert_secret_with_key(
        &db_path,
        &key,
        "DEEPSEEK_API_KEY",
        "api_key",
        "",
        "deepseek-secret".to_string(),
    )
    .expect("account");
    vault_upsert_secret_with_key(
        &db_path,
        &key,
        "EXTRACT_API_KEY",
        "api_key",
        "",
        "deepseek-secret".to_string(),
    )
    .expect("bind");
    let store = open_cli_store(&db_path).expect("open rw");
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(key.bytes(), b"leftover-rotation-member").expect("encrypt");
    let now = chrono::Utc::now().to_rfc3339();
    store
        .vault_upsert_entry(&memcore::vault::VaultEntry {
            name: "EXTRACT_API_KEY_1".to_string(),
            encrypted_value,
            nonce,
            secret_type: "api_key".to_string(),
            description: String::new(),
            allowed_agents: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            accessed_at: String::new(),
            access_count: 0,
        })
        .expect("member");
    store
        .vault_set_rotation(&memcore::vault::VaultKeyRotation {
            prefix: "EXTRACT_API_KEY".to_string(),
            current_index: 1,
            total_keys: 1,
            rotation_strategy: "round_robin".to_string(),
            created_at: now.clone(),
            updated_at: now,
        })
        .expect("rotation");
    let (_, key_id, value) =
        lease_api_key_from_store(&store, key.bytes(), "EXTRACT_API_KEY").expect("lease");
    assert_eq!(key_id, "DEEPSEEK_API_KEY");
    assert_eq!(value, "deepseek-secret");
}

#[test]
fn vault_get_output_redacts_by_default() {
    let out =
        vault_get_output("GH_TOKEN", "ghp_secret_value", false, false).expect("format output");
    assert!(out.contains("GH_TOKEN"));
    assert!(out.contains("--reveal"));
    assert!(
        !out.contains("ghp_secret_value"),
        "default get output must not reveal the secret: {out}"
    );
}

#[test]
fn vault_get_output_reveals_only_when_requested() {
    let plain =
        vault_get_output("GH_TOKEN", "ghp_secret_value", true, false).expect("plain output");
    assert_eq!(plain, "ghp_secret_value\n");

    let json = vault_get_output("GH_TOKEN", "ghp_secret_value", false, true).expect("json output");
    assert!(json.contains("\"revealed\": false"));
    assert!(json.contains("<redacted>"));
    assert!(
        !json.contains("ghp_secret_value"),
        "redacted JSON must not reveal the secret: {json}"
    );
}

#[test]
fn noninteractive_init_password_file_requires_confirmation_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let password_file = dir.path().join("password.txt");
    std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
    make_owner_only(&password_file);

    let err = read_vault_init_password(false, false, Some(&password_file), None, false)
        .expect_err("missing confirmation file should fail");
    assert!(err.to_string().contains("--confirm-password-file"), "{err}");
}

#[test]
fn noninteractive_init_password_file_must_match_confirmation_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let password_file = dir.path().join("password.txt");
    let confirm_file = dir.path().join("confirm.txt");
    std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
    std::fs::write(&confirm_file, "wrong horse battery staple\n").expect("confirm file");
    make_owner_only(&password_file);
    make_owner_only(&confirm_file);

    let err = read_vault_init_password(
        false,
        false,
        Some(&password_file),
        Some(&confirm_file),
        false,
    )
    .expect_err("mismatched confirmation should fail");
    assert!(err.to_string().contains("Passwords do not match"), "{err}");

    std::fs::write(&confirm_file, "correct horse battery staple\n").expect("confirm file");
    make_owner_only(&confirm_file);
    let password = read_vault_init_password(
        false,
        false,
        Some(&password_file),
        Some(&confirm_file),
        false,
    )
    .expect("matching confirmation should succeed");
    assert_eq!(password, "correct horse battery staple");
}

#[test]
#[cfg(unix)]
fn password_file_rejects_group_or_other_readable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let password_file = dir.path().join("password.txt");
    std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
    make_group_readable(&password_file);

    let err = read_password_file(&password_file, false)
        .expect_err("group-readable password file should be rejected");
    let msg = err.to_string();
    assert!(msg.contains("readable by group/other"), "{msg}");
    assert!(msg.contains("--insecure-password-file"), "{msg}");
    assert!(
        !msg.contains("correct horse battery staple"),
        "password content leaked"
    );
}

#[test]
#[cfg(unix)]
fn password_file_allows_insecure_opt_in() {
    let dir = tempfile::tempdir().expect("tempdir");
    let password_file = dir.path().join("password.txt");
    std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
    make_group_readable(&password_file);

    let password = read_password_file(&password_file, true)
        .expect("group-readable password file should be accepted with --insecure-password-file");
    assert_eq!(password, "correct horse battery staple");
}

#[test]
fn password_file_accepts_owner_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let password_file = dir.path().join("password.txt");
    std::fs::write(&password_file, "correct horse battery staple\n").expect("password file");
    make_owner_only(&password_file);

    let password = read_password_file(&password_file, false)
        .expect("owner-only password file should be accepted");
    assert_eq!(password, "correct horse battery staple");
}

#[test]
#[cfg(unix)]
fn password_file_rejects_symlink_without_reading_target() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target.txt");
    let link = dir.path().join("password.txt");
    let sentinel = "R12-SYMLINK-PASSWORD-MUST-NOT-LEAK";
    std::fs::write(&target, format!("{sentinel}\n")).expect("target password file");
    make_owner_only(&target);
    symlink(&target, &link).expect("password symlink");

    let err = read_password_file(&link, false).expect_err("password symlink must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("symlink"), "{msg}");
    assert!(!msg.contains(sentinel), "password content leaked in error");
}

#[test]
fn password_file_rejects_non_regular_file_before_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = read_password_file(dir.path(), true)
        .expect_err("a directory must not be accepted as a password file");
    let msg = err.to_string();
    assert!(msg.contains("regular file"), "{msg}");
    assert!(!msg.contains("password content"), "content leaked in error");
}

// tachi#1175: without a TTY, `rpassword::prompt_password` fails opening
// /dev/tty with the raw errno text "Device not configured (os error 6)" —
// meaningless to an agent shell with no controlling terminal. These assert
// the CLI now reports the real cause and names the actual non-interactive
// flags (verified against the `VaultAction` clap definitions in
// `tachi-bootstrap/src/cli/vault_actions.rs`) instead of leaking that OS
// errno. Uses the `TACHI_TEST_FORCE_NO_TTY` injection seam rather than
// detaching a real terminal from the test process.
//
// codex 3.1 (fix-round): `can_prompt_interactively` now probes `/dev/tty`
// directly (the same channel `rpassword` itself prompts on) instead of
// `stdin().is_terminal()`. The `TACHI_TEST_FORCE_NO_TTY` seam still short-
// circuits before that probe runs, so these tests stay deterministic
// regardless of whether the `cargo test` process happens to have a
// controlling terminal of its own.
#[test]
fn read_vault_password_reports_no_tty_hint_instead_of_os_error() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _guard = EnvRestore::set("TACHI_TEST_FORCE_NO_TTY", "1");

    let err = read_vault_password(false, false, None, false)
        .expect_err("no TTY and no non-interactive flag should fail");
    let msg = err.to_string();

    assert!(
        !msg.contains("os error 6") && !msg.contains("Device not configured"),
        "raw rpassword/TTY errno must not leak through: {msg}"
    );
    assert!(msg.contains("--keychain"), "{msg}");
    assert!(msg.contains("--stdin-password"), "{msg}");
    assert!(msg.contains("--password-file"), "{msg}");
}

#[test]
fn read_vault_init_password_reports_no_tty_hint_instead_of_os_error() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _guard = EnvRestore::set("TACHI_TEST_FORCE_NO_TTY", "1");

    let err = read_vault_init_password(false, false, None, None, false)
        .expect_err("no TTY and no non-interactive flag should fail");
    let msg = err.to_string();

    assert!(
        !msg.contains("os error 6") && !msg.contains("Device not configured"),
        "raw rpassword/TTY errno must not leak through: {msg}"
    );
    assert!(msg.contains("--keychain"), "{msg}");
    assert!(msg.contains("--stdin-password"), "{msg}");
    assert!(msg.contains("--password-file"), "{msg}");
    // codex 3.2: `vault init`'s no-TTY hint must also point at
    // --confirm-password-file — a bare --password-file is not enough for
    // init (there's nothing to check it against), unlike unlock where one
    // password file suffices on its own.
    assert!(msg.contains("--confirm-password-file"), "{msg}");
}
