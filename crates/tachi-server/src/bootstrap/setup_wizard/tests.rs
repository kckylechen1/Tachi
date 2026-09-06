use super::super::open_cli_store_read_only;
use super::agent_rules::{
    agent_memory_rules_block, merge_managed_block, AGENT_RULES_END, AGENT_RULES_START,
};
use super::env::{is_secret_key, looks_like_api_key, mask_secret, merge_config_env};
use super::vault::upsert_keys_and_rewrite_aliases;

#[test]
fn merge_appends_new_keys() {
    let body = "FOO=bar\n";
    let updates = vec![("BAZ".to_string(), "qux".to_string())];
    let out = merge_config_env(body, &updates);
    assert_eq!(out, "FOO=bar\nBAZ=qux\n");
}

#[test]
fn merge_replaces_existing_key_in_place() {
    let body = "VOYAGE_API_KEY=old\nENABLE_PIPELINE=false\n";
    let updates = vec![("VOYAGE_API_KEY".to_string(), "new".to_string())];
    let out = merge_config_env(body, &updates);
    assert_eq!(out, "VOYAGE_API_KEY=new\nENABLE_PIPELINE=false\n");
}

#[test]
fn merge_replaces_commented_key() {
    let body = "# VOYAGE_API_KEY=placeholder\nOTHER=1\n";
    let updates = vec![("VOYAGE_API_KEY".to_string(), "voy_real".to_string())];
    let out = merge_config_env(body, &updates);
    assert!(out.contains("VOYAGE_API_KEY=voy_real"));
    assert!(!out.contains("# VOYAGE_API_KEY=placeholder"));
    assert!(out.contains("OTHER=1"));
}

#[test]
fn merge_always_ends_with_newline() {
    let body = "FOO=bar"; // no trailing newline
    let updates = vec![("BAZ".to_string(), "qux".to_string())];
    let out = merge_config_env(body, &updates);
    assert!(out.ends_with('\n'));
}

#[test]
fn merge_handles_empty_existing_file() {
    let updates = vec![("A".to_string(), "1".to_string())];
    let out = merge_config_env("", &updates);
    assert_eq!(out, "A=1\n");
}

#[test]
fn merge_preserves_unrelated_lines_and_order() {
    let body = "# comment\nA=1\nB=2\nC=3\n";
    let updates = vec![("B".to_string(), "200".to_string())];
    let out = merge_config_env(body, &updates);
    assert_eq!(out, "# comment\nA=1\nB=200\nC=3\n");
}

#[test]
fn mask_secret_long_value() {
    let m = mask_secret("voy_1234567890abcdef");
    assert!(m.starts_with("voy"));
    assert!(m.ends_with("cdef"));
    assert!(m.contains("•"));
    assert!(!m.contains("12345"));
}

#[test]
fn mask_secret_short_values() {
    assert_eq!(mask_secret(""), "");
    assert_eq!(mask_secret("ab"), "••");
    assert_eq!(mask_secret("abcdef"), "••••ef");
}

#[test]
fn is_secret_key_recognises_common_prefixes() {
    assert!(is_secret_key("VOYAGE_API_KEY"));
    assert!(is_secret_key("GITHUB_TOKEN"));
    assert!(is_secret_key("OPENAI_SECRET"));
    assert!(is_secret_key("VAULT_PASSWORD"));
    assert!(!is_secret_key("ENABLE_PIPELINE"));
    assert!(!is_secret_key("TACHI_DAEMON_PORT"));
}

#[test]
fn looks_like_api_key_basic_rules() {
    assert!(!looks_like_api_key("short"));
    assert!(!looks_like_api_key("has spaces in it nope"));
    assert!(looks_like_api_key("voy_1234567890abcdef"));
    assert!(looks_like_api_key("sk-proj-ABCDEFG1234567890"));
}

#[test]
fn mcp_server_instructions_requires_end_of_task_save() {
    let text = crate::server_instructions::mcp_server_instructions();
    assert!(text.contains("action='save'"));
    assert!(text.contains("action='status'"));
    assert!(text.contains("agent_end"));
    assert!(
        text.contains("HTTP direct-connect"),
        "instructions must mention HTTP direct-connect (#732)"
    );
    assert!(
        text.contains("Bound sessions should omit project")
            && text.contains("same-DB alias")
            && text.contains("other-project writes and destructive actions remain forbidden"),
        "instructions must explain #1061 normalization without weakening write isolation: {text}"
    );
    assert!(
        !text.contains("project='<git-project>'"),
        "bound-session example must prefer omission: {text}"
    );
}

#[test]
fn wizard_vault_funnel_emits_aliases_not_plaintext() {
    // When the wizard funnels collected keys into the vault, the resulting
    // config.env entries must become `KEY=vault:KEY` aliases and the
    // plaintext value must be stored encrypted in the vault (not in env).
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");

    let key = super::super::vault_cli::vault_init_with_password(
        &db_path,
        "correct horse battery staple".to_string(),
    )
    .expect("init vault");

    let mut new_entries = vec![
        (
            "VOYAGE_API_KEY".to_string(),
            "voy_super_secret_value".to_string(),
        ),
        ("ENABLE_PIPELINE".to_string(), "true".to_string()),
    ];
    let key_names = vec!["VOYAGE_API_KEY".to_string()];

    let stored = upsert_keys_and_rewrite_aliases(&db_path, &key, &key_names, &mut new_entries)
        .expect("funnel keys into vault");
    assert_eq!(stored, 1);

    // The API key row is now a vault alias; no plaintext value remains.
    let voyage = new_entries
        .iter()
        .find(|(k, _)| k == "VOYAGE_API_KEY")
        .expect("voyage entry");
    assert_eq!(voyage.1, "vault:VOYAGE_API_KEY");
    assert!(
        !new_entries
            .iter()
            .any(|(_, v)| v.contains("voy_super_secret_value")),
        "plaintext secret value must not survive in config.env entries"
    );
    // Non-secret entries are untouched.
    assert!(new_entries
        .iter()
        .any(|(k, v)| k == "ENABLE_PIPELINE" && v == "true"));

    // The merged config.env body carries the alias line, never the secret.
    let merged = merge_config_env("", &new_entries);
    assert!(merged.contains("VOYAGE_API_KEY=vault:VOYAGE_API_KEY"));
    assert!(!merged.contains("voy_super_secret_value"));

    // And the value really is recoverable from the encrypted vault.
    let store = open_cli_store_read_only(&db_path).expect("open store");
    let entry = store
        .vault_get_entry("VOYAGE_API_KEY")
        .expect("get entry")
        .expect("entry exists");
    let decrypted = crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)
        .expect("decrypt");
    assert_eq!(
        String::from_utf8(decrypted).expect("utf8"),
        "voy_super_secret_value"
    );
}

#[test]
fn wizard_vault_funnel_preserves_plaintext_on_write_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = super::super::vault_cli::vault_init_with_password(
        &db_path,
        "correct horse battery staple".to_string(),
    )
    .expect("init vault");
    let missing_parent_db = dir.path().join("missing").join("memory.db");
    let mut new_entries = vec![(
        "VOYAGE_API_KEY".to_string(),
        "voy_super_secret_value".to_string(),
    )];
    let key_names = vec!["VOYAGE_API_KEY".to_string()];

    let err =
        upsert_keys_and_rewrite_aliases(&missing_parent_db, &key, &key_names, &mut new_entries)
            .expect_err("invalid db path should fail");

    assert!(
        err.to_string().contains("open")
            || err.to_string().contains("No such file")
            || err.to_string().contains("unable")
    );
    assert_eq!(new_entries[0].1, "voy_super_secret_value");
}

#[test]
fn wizard_vault_funnel_binds_lane_slot_after_account_in_same_batch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = super::super::vault_cli::vault_init_with_password(
        &db_path,
        "correct horse battery staple".to_string(),
    )
    .expect("init vault");
    let mut new_entries = vec![
        ("EXTRACT_API_KEY".to_string(), "deepseek-secret".to_string()),
        (
            "DEEPSEEK_API_KEY".to_string(),
            "deepseek-secret".to_string(),
        ),
    ];
    let key_names = vec![
        "EXTRACT_API_KEY".to_string(),
        "DEEPSEEK_API_KEY".to_string(),
    ];
    let stored = upsert_keys_and_rewrite_aliases(&db_path, &key, &key_names, &mut new_entries)
        .expect("funnel");
    assert_eq!(stored, 2);
    assert_eq!(new_entries[0].1, "vault:EXTRACT_API_KEY");
    assert_eq!(new_entries[1].1, "vault:DEEPSEEK_API_KEY");
    let store = open_cli_store_read_only(&db_path).expect("open store");
    let slot = store
        .vault_get_entry("EXTRACT_API_KEY")
        .expect("get slot")
        .expect("slot exists");
    let slot_plain = crate::vault_crypto::decrypt(key.bytes(), &slot.encrypted_value, &slot.nonce)
        .expect("decrypt slot");
    assert_eq!(
        String::from_utf8(slot_plain).expect("utf8"),
        "vault:DEEPSEEK_API_KEY"
    );
    let account = store
        .vault_get_entry("DEEPSEEK_API_KEY")
        .expect("get account")
        .expect("account exists");
    let account_plain =
        crate::vault_crypto::decrypt(key.bytes(), &account.encrypted_value, &account.nonce)
            .expect("decrypt account");
    assert_eq!(
        String::from_utf8(account_plain).expect("utf8"),
        "deepseek-secret"
    );
}

#[test]
fn wizard_vault_funnel_refuses_unmatched_lane_slot_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    let key = super::super::vault_cli::vault_init_with_password(
        &db_path,
        "correct horse battery staple".to_string(),
    )
    .expect("init vault");
    let mut new_entries = vec![(
        "EXTRACT_API_KEY".to_string(),
        "orphan-slot-secret".to_string(),
    )];
    let key_names = vec!["EXTRACT_API_KEY".to_string()];
    let err = upsert_keys_and_rewrite_aliases(&db_path, &key, &key_names, &mut new_entries)
        .expect_err("unmatched slot must not copy ciphertext");
    assert!(
        err.to_string().contains("second copy") || err.to_string().contains("provider account"),
        "{err}"
    );
    assert_eq!(new_entries[0].1, "orphan-slot-secret");
}

#[test]
fn merge_managed_block_appends_and_replaces() {
    let block = agent_memory_rules_block();
    let first = merge_managed_block("# Existing\n", &block);
    assert!(first.contains("# Existing"));
    assert!(first.contains(AGENT_RULES_START));
    assert!(first.contains("action=\"briefing\""));
    assert!(first.contains("action=\"status\""));

    let replacement = format!("{AGENT_RULES_START}\nold rules\n{AGENT_RULES_END}\n");
    let second = merge_managed_block(&first, &replacement);
    assert!(second.contains("old rules"));
    assert!(!second.contains("action=\"briefing\""));
    assert!(!second.contains("action=\"status\""));
    assert_eq!(second.matches(AGENT_RULES_START).count(), 1);
}
