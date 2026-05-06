use super::*;

fn is_shell_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn is_upper_snake_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
}

// ─── `tachi env` handler ────────────────────────────────────────────────────

pub(super) async fn run_env_command(
    global_db_path: &PathBuf,
    filter: Option<&str>,
    env_only: bool,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_cli_store_read_only(global_db_path)?;

    // 1. Check vault is initialized
    let config = store
        .vault_get_config()
        .map_err(|e| format!("Failed to read vault config: {e}"))?
        .ok_or_else(|| {
            "Vault not initialized. Run `tachi serve` and call vault_init first, \
             or set up the vault via an MCP client."
                .to_string()
        })?;

    // 2. Resolve password from the requested portable source.
    let password = super::vault_cli::read_vault_password(stdin_password, keychain, password_file)?;

    // 3. Derive key and verify
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    let key = crate::vault_crypto::derive_key(&password, &salt)?;

    if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
        return Err("Wrong password".into());
    }

    // 4. List and decrypt all entries
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("Failed to list vault entries: {e}"))?;

    // Build glob pattern if provided
    let glob_pattern = filter.map(|f| glob::Pattern::new(f)).transpose()?;

    let mut emitted = 0usize;
    for entry in entries {
        // Skip agent-restricted secrets — those aren't meant for env injection
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }

        if !is_shell_env_name(&entry.name) {
            eprintln!(
                "WARNING: skipped secret '{}' because it is not a valid shell environment name",
                entry.name
            );
            continue;
        }

        // --env-only: skip names that don't look like env vars (UPPER_SNAKE_CASE)
        if env_only && !is_upper_snake_env_name(&entry.name) {
            continue;
        }

        // --filter: apply glob pattern
        if let Some(ref pat) = glob_pattern {
            if !pat.matches(&entry.name) {
                continue;
            }
        }

        let decrypted =
            match crate::vault_crypto::decrypt(&key, &entry.encrypted_value, &entry.nonce) {
                Ok(bytes) => bytes,
                Err(e) => {
                    eprintln!("WARNING: failed to decrypt '{}': {}", entry.name, e);
                    continue;
                }
            };
        let value = match String::from_utf8(decrypted) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARNING: secret '{}' is not valid UTF-8: {}", entry.name, e);
                continue;
            }
        };

        if value.trim().is_empty() {
            continue;
        }

        // Shell-safe escaping: wrap in single quotes, escape embedded single quotes
        let escaped = value.replace('\'', "'\\''");
        println!("export {}='{}'", entry.name, escaped);
        emitted += 1;
    }

    eprintln!("# tachi env: {} secret(s) emitted", emitted);
    Ok(())
}
