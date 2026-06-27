use super::super::open_cli_store_read_only;
use super::daemon::daemon_matches_vault_db;
use super::keys::{
    canonical_provider_key_defs, derive_verified_vault_key_from_password, vault_init_with_password,
    vault_upsert_secret_with_key,
};
use super::output::vault_get_output;
use super::password::{
    read_password_file, read_vault_init_password, read_vault_init_password_stdin_lines,
};
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

fn config_for_password(password: &str) -> memory_core::vault::VaultConfig {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let salt = crate::vault_crypto::generate_salt();
    let key =
        crate::vault_crypto::DerivedVaultKey::derive(password, &salt).expect("derive test key");
    let verifier = crate::vault_crypto::create_verifier(key.bytes()).expect("create verifier");
    memory_core::vault::VaultConfig {
        salt: B64.encode(salt),
        verifier,
        kdf_algorithm: "argon2id".to_string(),
        kdf_params: crate::vault_crypto::active_kdf_params_json().to_string(),
        cipher: memory_core::vault::VaultCipher::Aes256Gcm,
        created_at: "2026-06-14T00:00:00Z".to_string(),
        updated_at: "2026-06-14T00:00:00Z".to_string(),
    }
}

fn string_is_zeroed(value: &str) -> bool {
    value.as_bytes().iter().all(|byte| *byte == 0)
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
    let with_dep = canonical_provider_key_defs(true);
    assert!(with_dep.len() >= defs.len());
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
