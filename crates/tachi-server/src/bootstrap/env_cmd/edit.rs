use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::BufRead;
use std::path::{Component, Path, PathBuf};

use super::super::{open_cli_store, open_cli_store_read_only, vault_cli};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretWriteOutcome {
    Created,
    Updated,
    ReusedExisting,
}

pub(super) struct EnvSetArgs<'a> {
    pub global_db_path: &'a PathBuf,
    pub cwd: &'a Path,
    pub key: &'a str,
    pub secret: Option<&'a str>,
    pub file: Option<&'a Path>,
    pub secret_type: Option<&'a str>,
    pub description: Option<&'a str>,
    pub value_stdin: bool,
    pub replace: bool,
    pub stdin_password: bool,
    pub keychain: bool,
    pub password_file: Option<&'a Path>,
    pub insecure_password_file: bool,
}

pub(super) struct EnvSealArgs<'a> {
    pub global_db_path: &'a PathBuf,
    pub cwd: &'a Path,
    pub file: Option<&'a Path>,
    pub keys: &'a [String],
    pub dry_run: bool,
    pub replace: bool,
    pub stdin_password: bool,
    pub keychain: bool,
    pub password_file: Option<&'a Path>,
    pub insecure_password_file: bool,
}

fn has_vault_prefix(s: &str) -> bool {
    s.get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("vault:"))
}

pub(super) fn run_env_set(args: EnvSetArgs<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    run_env_set_with_reader(args, &mut stdin)
}

pub(super) fn run_env_set_with_reader(
    args: EnvSetArgs<'_>,
    stdin: &mut impl BufRead,
) -> Result<(), Box<dyn std::error::Error>> {
    if args.value_stdin && args.stdin_password {
        return Err(
            "Cannot combine --stdin (secret value) with --stdin-password (Vault password)".into(),
        );
    }
    if !crate::utils::is_shell_env_name(args.key) {
        return Err(format!(
            "Environment variable '{}' must be a valid shell identifier (e.g. OPENAI_API_KEY)",
            args.key
        )
        .into());
    }
    let secret_name = args.secret.unwrap_or(args.key);
    crate::vault_crypto::validate_secret_name(secret_name)?;

    let secret_type = args
        .secret_type
        .map(memcore::normalize_secret_type)
        .unwrap_or_else(|| memcore::infer_vault_secret_type(secret_name))
        .to_string();
    crate::vault_ops::validate_lane_slot_secret_type(secret_name, &secret_type)?;
    memcore::reject_api_key_type_for_lane_config(secret_name, &secret_type)?;

    let extra_file = match args.file {
        Some(path) => Some(resolve_project_file_path(args.cwd, path, true)?),
        None => None,
    };

    let config = vault_cli::read_vault_config_for_key(args.global_db_path)?;
    let vault_key = vault_cli::read_verified_vault_key(
        &config,
        args.stdin_password,
        args.keychain,
        args.password_file,
        args.insecure_password_file,
    )?;

    let mut raw_secret = crate::vault_crypto::ZeroizingString::new(String::new());
    if args.value_stdin {
        stdin.read_line(raw_secret.as_mut_string())?;
    } else {
        let prompted = rpassword::prompt_password(format!("Value for {}: ", args.key))?;
        *raw_secret.as_mut_string() = prompted;
    }

    let trimmed = raw_secret.trim();
    if trimmed.is_empty() {
        return Err("Secret value cannot be empty".into());
    }
    if !crate::vault_ops::is_lane_slot_secret_name(secret_name)
        && (crate::provider_config::parse_vault_alias(trimmed).is_some()
            || has_vault_prefix(trimmed))
    {
        return Err("Secret value cannot be a recursive 'vault:' pointer".into());
    }

    let outcome = upsert_single_secret_transactional(
        args.global_db_path,
        &vault_key,
        secret_name,
        &secret_type,
        args.description.unwrap_or(""),
        trimmed,
        args.replace,
    )?;
    let vault_action = match outcome {
        SecretWriteOutcome::Created => "created",
        SecretWriteOutcome::Updated => "updated",
        SecretWriteOutcome::ReusedExisting => "reused existing",
    };

    let mut binding_updates = BTreeMap::new();
    binding_updates.insert(args.key.to_string(), secret_name.to_string());

    let vault_env_path = ensure_project_vault_env_bindings(args.cwd, &binding_updates)?;
    if let Some(extra_path) = extra_file.as_ref() {
        if !paths_refer_to_same_file(&vault_env_path, extra_path) {
            upsert_dotenv_pointer_bindings(extra_path, &binding_updates)?;
        }
    }

    println!(
        "Project secret '{}' -> vault:{} ({vault_action} in Vault; bound in {}).",
        args.key,
        secret_name,
        vault_env_path.display()
    );
    if let Some(extra_path) = extra_file.as_ref() {
        if !paths_refer_to_same_file(&vault_env_path, extra_path) {
            println!("  Updated pointer in {}", extra_path.display());
        }
    }
    println!("  Verify with: tachi env plan");
    Ok(())
}

struct SealCandidate {
    line_no: usize,
    export_prefix: bool,
    key: String,
    secret_name: String,
    secret_type: String,
    value: crate::vault_crypto::ZeroizingString,
}

struct ExistingPointer {
    line_no: usize,
    key: String,
    secret_name: String,
}

pub(super) fn run_env_seal(args: EnvSealArgs<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let target_file = match args.file {
        Some(path) => resolve_project_file_path(args.cwd, path, false)?,
        None => {
            let default_path = args.cwd.join(".env.credentials");
            if !default_path.exists() {
                return Err(
                    "No .env.credentials found in current project; specify a file explicitly (e.g. `tachi env seal .env --key OPENAI_API_KEY`)"
                        .into(),
                );
            }
            resolve_project_file_path(args.cwd, Path::new(".env.credentials"), false)?
        }
    };

    let is_dedicated_credentials_file = target_file
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name == ".env.credentials" || name == "vault.env");

    if !is_dedicated_credentials_file && args.keys.is_empty() {
        return Err(format!(
            "Refusing to bulk-seal mixed dotenv file {} without explicit --key <KEY> selection (use .env.credentials for bulk sealing, or pass --key KEY)",
            target_file.display()
        )
        .into());
    }

    let mut requested_keys = HashSet::new();
    for k in args.keys {
        let trimmed = k.trim();
        if !crate::utils::is_shell_env_name(trimmed) {
            return Err(
                format!("Invalid --key '{trimmed}': must be a valid shell variable name").into(),
            );
        }
        requested_keys.insert(trimmed.to_string());
    }

    let initial_meta = std::fs::metadata(&target_file)
        .map_err(|e| format!("Failed to inspect {}: {e}", target_file.display()))?;
    let raw_content = crate::vault_crypto::ZeroizingString::new(
        std::fs::read_to_string(&target_file)
            .map_err(|e| format!("Failed to read {}: {e}", target_file.display()))?,
    );
    let dotenv = super::dotenv_lines::DotenvText::parse(&raw_content)
        .map_err(|e| format!("{}: {e}", target_file.display()))?;

    let mut seen_selected_keys: HashMap<String, usize> = HashMap::new();
    let mut candidates: Vec<SealCandidate> = Vec::new();
    let mut existing_pointers: Vec<ExistingPointer> = Vec::new();
    let mut rewritten_lines: Vec<String> = Vec::new();

    for entry in &dotenv.entries {
        let line_no = entry.line_no;
        let raw_line: &str = &entry.text;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            rewritten_lines.push(raw_line.to_string());
            continue;
        }
        let (export_prefix, body) = match trimmed.strip_prefix("export ") {
            Some(rest) => (true, rest.trim_start()),
            None => (false, trimmed),
        };
        let Some((raw_key, raw_rhs)) = body.split_once('=') else {
            if requested_keys.is_empty() {
                return Err(format!(
                    "{}:{line_no}: malformed assignment line (expected KEY=VALUE)",
                    target_file.display()
                )
                .into());
            }
            rewritten_lines.push(raw_line.to_string());
            continue;
        };
        let key = raw_key.trim();
        let selected = if requested_keys.is_empty() {
            true
        } else {
            requested_keys.contains(key)
        };
        if !selected {
            rewritten_lines.push(raw_line.to_string());
            continue;
        }
        if !crate::utils::is_shell_env_name(key) {
            return Err(format!(
                "{}:{line_no}: invalid environment variable name '{key}'",
                target_file.display()
            )
            .into());
        }
        if let Some(prev_line) = seen_selected_keys.insert(key.to_string(), line_no) {
            return Err(format!(
                "{}:{line_no}: duplicate definition of '{key}' (previously defined on line {prev_line})",
                target_file.display()
            )
            .into());
        }

        let parsed_val = parse_dotenv_rhs_strict(raw_rhs, &target_file, line_no)?;
        if parsed_val.is_empty()
            || (requested_keys.is_empty()
                && key.ends_with("_COUNT")
                && parsed_val.trim().chars().all(|c| c.is_ascii_digit()))
        {
            if !requested_keys.is_empty() {
                return Err(format!(
                    "{}:{line_no}: selected key '{key}' has an empty value",
                    target_file.display()
                )
                .into());
            }
            rewritten_lines.push(raw_line.to_string());
            continue;
        }

        if let Some(secret_ref) = crate::provider_config::parse_vault_alias(parsed_val.trim()) {
            let secret_name = secret_ref.trim().to_string();
            crate::vault_crypto::validate_secret_name(&secret_name)?;
            existing_pointers.push(ExistingPointer {
                line_no,
                key: key.to_string(),
                secret_name,
            });
            rewritten_lines.push(raw_line.to_string());
            continue;
        }
        if has_vault_prefix(parsed_val.trim()) {
            return Err(format!(
                "{}:{line_no}: malformed vault: pointer for '{key}'",
                target_file.display()
            )
            .into());
        }

        crate::vault_crypto::validate_secret_name(key)?;
        let secret_type = memcore::infer_vault_secret_type(key).to_string();
        crate::vault_ops::validate_lane_slot_secret_type(key, &secret_type)?;
        memcore::reject_api_key_type_for_lane_config(key, &secret_type)?;

        let prefix_str = if export_prefix { "export " } else { "" };
        let comment = super::dotenv_lines::trailing_comment(raw_rhs);
        rewritten_lines.push(format!("{prefix_str}{key}=vault:{key}{comment}"));
        candidates.push(SealCandidate {
            line_no,
            export_prefix,
            key: key.to_string(),
            secret_name: key.to_string(),
            secret_type,
            value: parsed_val,
        });
    }
    drop(raw_content);

    for req in &requested_keys {
        if !seen_selected_keys.contains_key(req) {
            return Err(format!(
                "Requested --key '{req}' was not found in {}",
                target_file.display()
            )
            .into());
        }
    }

    if args.dry_run {
        println!("Dry-run seal plan for {}:", target_file.display());
        println!("  plaintext keys to seal: {}", candidates.len());
        for item in &candidates {
            let export_note = if item.export_prefix { " (export)" } else { "" };
            println!(
                "  - line {}: {} -> vault:{}{}",
                item.line_no, item.key, item.secret_name, export_note
            );
        }
        if !existing_pointers.is_empty() {
            println!(
                "  existing vault: pointers kept: {}",
                existing_pointers.len()
            );
        }
        return Ok(());
    }

    // Verify existing pointers in Vault before mutating anything
    {
        let store_ro = open_cli_store_read_only(args.global_db_path)?;
        let entries = store_ro
            .vault_list_entries()
            .map_err(|e| format!("vault_list_entries: {e}"))?;
        let entry_names: HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        for ptr in &existing_pointers {
            let exists = entry_names.contains(ptr.secret_name.as_str())
                || store_ro
                    .vault_get_rotation(&ptr.secret_name)
                    .map_err(|e| format!("vault_get_rotation: {e}"))?
                    .is_some();
            if !exists {
                return Err(format!(
                    "{}:{}: existing pointer '{}'=vault:{} references a missing Vault secret",
                    target_file.display(),
                    ptr.line_no,
                    ptr.key,
                    ptr.secret_name
                )
                .into());
            }
        }
    }

    if candidates.is_empty() {
        let mut binding_updates = BTreeMap::new();
        for ptr in &existing_pointers {
            binding_updates.insert(ptr.key.clone(), ptr.secret_name.clone());
        }
        if !binding_updates.is_empty() {
            let vault_env_path = ensure_project_vault_env_bindings(args.cwd, &binding_updates)?;
            println!(
                "No plaintext keys to seal in {} ({} existing vault: pointer(s) verified; bindings in {}).",
                target_file.display(),
                existing_pointers.len(),
                vault_env_path.display()
            );
        } else {
            println!(
                "No eligible keys found to seal in {}.",
                target_file.display()
            );
        }
        return Ok(());
    }

    let config = vault_cli::read_vault_config_for_key(args.global_db_path)?;
    let vault_key = vault_cli::read_verified_vault_key(
        &config,
        args.stdin_password,
        args.keychain,
        args.password_file,
        args.insecure_password_file,
    )?;

    // Re-verify target_file snapshot before committing to Vault so a concurrent file edit
    // during interactive password entry aborts before any mutation.
    verify_file_unchanged(&target_file, &initial_meta)?;

    // Step 1: Commit all candidate secrets to Vault atomically
    let (written_count, reused_count) = upsert_seal_candidates_transactional(
        args.global_db_path,
        &vault_key,
        &target_file,
        &candidates,
        args.replace,
    )?;

    // Step 2: Atomically replace target_file first (verifying initial_meta snapshot)
    let new_body = dotenv.render(&rewritten_lines);
    drop(dotenv);
    atomic_replace_checked_file(&target_file, &initial_meta, new_body.as_bytes())?;

    // Step 3: Update .tachi/vault.env bindings (if it does not resolve to target_file itself)
    let mut binding_updates = BTreeMap::new();
    for ptr in &existing_pointers {
        binding_updates.insert(ptr.key.clone(), ptr.secret_name.clone());
    }
    for item in &candidates {
        binding_updates.insert(item.key.clone(), item.secret_name.clone());
    }
    let vault_env_path = ensure_project_vault_env_bindings_skipping_target(
        args.cwd,
        &binding_updates,
        Some(&target_file),
    )?;

    println!(
        "Sealed {} key(s) in {} ({written_count} stored in Vault, {reused_count} reused; project bindings in {}).",
        candidates.len(),
        target_file.display(),
        vault_env_path.display()
    );
    println!("  Verify with: tachi env plan");
    Ok(())
}

fn upsert_single_secret_transactional(
    global_db_path: &PathBuf,
    vault_key: &crate::vault_crypto::DerivedVaultKey,
    secret_name: &str,
    secret_type: &str,
    description: &str,
    secret_value: &str,
    replace: bool,
) -> Result<SecretWriteOutcome, Box<dyn std::error::Error>> {
    let mut store = open_cli_store(global_db_path)?;
    let transaction = store
        .begin_vault_transaction()
        .map_err(|e| format!("begin vault transaction: {e}"))?;
    let outcome = upsert_entry_in_transaction(
        &transaction,
        vault_key,
        secret_name,
        secret_type,
        description,
        secret_value,
        replace,
        None,
    )?;
    transaction
        .commit()
        .map_err(|e| format!("commit vault transaction: {e}"))?;
    Ok(outcome)
}

fn upsert_seal_candidates_transactional(
    global_db_path: &PathBuf,
    vault_key: &crate::vault_crypto::DerivedVaultKey,
    target_file: &Path,
    candidates: &[SealCandidate],
    replace: bool,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    if candidates.is_empty() {
        return Ok((0, 0));
    }

    let mut regular_indices = Vec::new();
    let mut lane_slot_indices = Vec::new();
    for (idx, item) in candidates.iter().enumerate() {
        if crate::vault_ops::is_lane_slot_secret_name(&item.secret_name) {
            lane_slot_indices.push(idx);
        } else {
            regular_indices.push(idx);
        }
    }

    let mut written_count = 0usize;
    let mut reused_count = 0usize;
    let mut store = open_cli_store(global_db_path)?;
    let transaction = store
        .begin_vault_transaction()
        .map_err(|e| format!("begin vault transaction: {e}"))?;

    for idx in regular_indices.into_iter().chain(lane_slot_indices) {
        let item = &candidates[idx];
        let outcome = upsert_entry_in_transaction(
            &transaction,
            vault_key,
            &item.secret_name,
            &item.secret_type,
            "",
            &item.value,
            replace,
            Some((target_file, item.line_no)),
        )?;
        match outcome {
            SecretWriteOutcome::Created | SecretWriteOutcome::Updated => written_count += 1,
            SecretWriteOutcome::ReusedExisting => reused_count += 1,
        }
    }

    transaction
        .commit()
        .map_err(|e| format!("commit vault transaction: {e}"))?;

    Ok((written_count, reused_count))
}

#[allow(clippy::too_many_arguments)]
fn upsert_entry_in_transaction(
    transaction: &memcore::store::vault::VaultTransaction<'_>,
    vault_key: &crate::vault_crypto::DerivedVaultKey,
    secret_name: &str,
    secret_type: &str,
    description: &str,
    secret_value: &str,
    replace: bool,
    source_loc: Option<(&Path, usize)>,
) -> Result<SecretWriteOutcome, Box<dyn std::error::Error>> {
    if crate::vault_ops::is_lane_slot_secret_name(secret_name) {
        if secret_type != memcore::SECRET_TYPE_API_KEY {
            return Err(format!(
                "Lane slot '{secret_name}' must bind as {} (got {secret_type})",
                memcore::SECRET_TYPE_API_KEY
            )
            .into());
        }
        let (decided, created) =
            crate::vault_ops::account_bind::write_lane_slot_binding_in_transaction(
                transaction,
                vault_key.bytes(),
                secret_name,
                secret_value,
                replace,
                description,
                None,
                None,
            )?;
        return Ok(if created {
            SecretWriteOutcome::Created
        } else if decided.noop {
            SecretWriteOutcome::ReusedExisting
        } else {
            SecretWriteOutcome::Updated
        });
    }
    let existing = transaction
        .vault_get_entry(secret_name)
        .map_err(|e| format!("vault_get_entry: {e}"))?;
    if let Some(entry) = existing.as_ref() {
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            return Err(format!(
                "Access denied: direct CLI cannot overwrite agent-restricted secret '{secret_name}'"
            )
            .into());
        }
        let decrypted_bytes =
            crate::vault_crypto::decrypt(vault_key.bytes(), &entry.encrypted_value, &entry.nonce)?;
        let mut existing_plain = crate::vault_crypto::decode_utf8_zeroizing(
            decrypted_bytes,
            format!("Decrypted secret '{secret_name}' is not valid UTF-8"),
        )?;
        let is_same = existing_plain == secret_value;
        crate::vault_crypto::zero_string(&mut existing_plain);
        if is_same {
            return Ok(SecretWriteOutcome::ReusedExisting);
        }
        if !replace {
            return match source_loc {
                Some((file, line_no)) => Err(format!(
                    "{}:{line_no}: Vault secret '{secret_name}' already exists with a different value. Pass --replace to overwrite it.",
                    file.display()
                )
                .into()),
                None => Err(format!(
                    "Vault secret '{secret_name}' already exists with a different value. Pass --replace to overwrite it (updates all projects referencing vault:{secret_name}), or pass --secret <NAME> to use a distinct Vault secret name."
                )
                .into()),
            };
        }
    }

    let is_new = existing.is_none();
    if is_new && memcore::is_lane_config_url_name(secret_name) {
        if let Some(leak) = memcore::catalog::endpoint::endpoint_credential_leak(secret_value) {
            return Err(format!(
                "Vault name '{secret_name}' value embeds a credential in the endpoint ({leak}); refusing write"
            )
            .into());
        }
    }

    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(vault_key.bytes(), secret_value.as_bytes())?;
    let now = chrono::Utc::now().to_rfc3339();
    let entry = memcore::vault::VaultEntry {
        name: secret_name.to_string(),
        encrypted_value,
        nonce,
        secret_type: secret_type.to_string(),
        description: description.to_string(),
        allowed_agents: None,
        created_at: if is_new { now.clone() } else { String::new() },
        updated_at: now,
        accessed_at: String::new(),
        access_count: 0,
    };

    transaction
        .vault_upsert_entry(&entry)
        .map_err(|e| format!("vault_upsert_entry: {e}"))?;
    if let Some((prefix, _)) = crate::provider_config::parse_rotation_member_name(secret_name) {
        if let Some(rotation) = transaction
            .vault_get_rotation(prefix)
            .map_err(|e| format!("vault_get_rotation: {e}"))?
        {
            let entries = transaction
                .vault_list_entries()
                .map_err(|e| format!("vault_list_entries: {e}"))?;
            memcore::validate_api_key_rotation(&entries, &rotation)
                .map_err(|error| format!("{error}; refusing rotation member update"))?;
        }
    }
    crate::vault_ops::account_events::observe_account_entry(
        transaction,
        vault_key.bytes(),
        secret_name,
        secret_type,
        secret_value,
        false,
    )?;

    Ok(if is_new {
        SecretWriteOutcome::Created
    } else {
        SecretWriteOutcome::Updated
    })
}

fn parse_dotenv_rhs_strict(
    raw_rhs: &str,
    path: &Path,
    line_no: usize,
) -> Result<crate::vault_crypto::ZeroizingString, Box<dyn std::error::Error>> {
    let trimmed = raw_rhs.trim();
    if trimmed.is_empty() {
        return Ok(crate::vault_crypto::ZeroizingString::new(String::new()));
    }
    if let Some(rest) = trimmed.strip_prefix('\'') {
        let Some((inner, trailing)) = rest.split_once('\'') else {
            return Err(format!(
                "{}:{line_no}: unclosed single quote in dotenv value",
                path.display()
            )
            .into());
        };
        let trailing = trailing.trim();
        if !trailing.is_empty() && !trailing.starts_with('#') {
            return Err(format!(
                "{}:{line_no}: unexpected trailing characters after single-quoted value",
                path.display()
            )
            .into());
        }
        return Ok(crate::vault_crypto::ZeroizingString::new(inner.to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        let mut out = crate::vault_crypto::ZeroizingString::new(String::new());
        let mut chars = rest.chars();
        let mut closed = false;
        while let Some(ch) = chars.next() {
            match ch {
                '\\' => {
                    let Some(escaped) = chars.next() else {
                        return Err(format!(
                            "{}:{line_no}: trailing backslash in double-quoted value",
                            path.display()
                        )
                        .into());
                    };
                    match escaped {
                        '"' | '\\' | '$' | '`' => out.as_mut_string().push(escaped),
                        'n' => out.as_mut_string().push('\n'),
                        'r' => out.as_mut_string().push('\r'),
                        't' => out.as_mut_string().push('\t'),
                        other => {
                            out.as_mut_string().push('\\');
                            out.as_mut_string().push(other);
                        }
                    }
                }
                '"' => {
                    closed = true;
                    break;
                }
                '`' | '$' => {
                    return Err(format!(
                        "{}:{line_no}: shell expansion character '{ch}' inside double quotes is ambiguous; use single quotes or `tachi env set`",
                        path.display()
                    )
                    .into());
                }
                other => out.as_mut_string().push(other),
            }
        }
        if !closed {
            return Err(format!(
                "{}:{line_no}: unclosed double quote in dotenv value",
                path.display()
            )
            .into());
        }
        let trailing = chars.as_str().trim();
        if !trailing.is_empty() && !trailing.starts_with('#') {
            return Err(format!(
                "{}:{line_no}: unexpected trailing characters after double-quoted value",
                path.display()
            )
            .into());
        }
        return Ok(out);
    }

    let without_comment = match trimmed.split_once(" #") {
        Some((before, _)) => before.trim(),
        None => trimmed,
    };
    if without_comment.contains('$')
        || without_comment.contains('`')
        || without_comment.contains('"')
        || without_comment.contains('\'')
    {
        return Err(format!(
            "{}:{line_no}: unquoted value contains ambiguous quote or expansion characters; quote the value or use `tachi env set`",
            path.display()
        )
        .into());
    }
    Ok(crate::vault_crypto::ZeroizingString::new(
        without_comment.to_string(),
    ))
}

fn resolve_project_file_path(
    cwd: &Path,
    file: &Path,
    allow_missing: bool,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let canon_cwd = cwd.canonicalize().map_err(|e| {
        format!(
            "Project directory {} does not exist or cannot be resolved: {e}",
            cwd.display()
        )
    })?;
    for component in file.components() {
        if matches!(component, Component::ParentDir) {
            return Err(format!(
                "Refusing path '{}' with '..' traversal outside project cwd",
                file.display()
            )
            .into());
        }
    }
    let joined = if file.is_absolute() {
        file.to_path_buf()
    } else {
        canon_cwd.join(file)
    };
    let parent = joined
        .parent()
        .ok_or_else(|| format!("Invalid target file path {}", joined.display()))?;
    let canon_parent = parent.canonicalize().map_err(|e| {
        format!(
            "Target directory {} does not exist or cannot be resolved: {e}",
            parent.display()
        )
    })?;
    if !canon_parent.starts_with(&canon_cwd) {
        return Err(format!(
            "Target file {} is outside project directory {}",
            joined.display(),
            canon_cwd.display()
        )
        .into());
    }
    let file_name = joined
        .file_name()
        .ok_or_else(|| format!("Invalid file name in {}", joined.display()))?;
    let resolved = canon_parent.join(file_name);

    match std::fs::symlink_metadata(&resolved) {
        Ok(meta) => {
            if meta.file_type().is_symlink() && !resolved.exists() {
                return Err(
                    format!("Target file {} is a dangling symlink", resolved.display()).into(),
                );
            }
            let canon_target = resolved.canonicalize()?;
            if !canon_target.starts_with(&canon_cwd) {
                return Err(format!(
                    "Target file {} resolves outside project directory {}",
                    resolved.display(),
                    canon_cwd.display()
                )
                .into());
            }
            // Return the canonical real file path so atomic rewrites update the backing
            // file directly and never leave an unsealed symlink target behind.
            Ok(canon_target)
        }
        Err(_) if allow_missing => Ok(resolved),
        Err(_) => Err(format!("File {} does not exist", resolved.display()).into()),
    }
}

fn ensure_project_vault_env_bindings(
    cwd: &Path,
    updates: &BTreeMap<String, String>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    ensure_project_vault_env_bindings_skipping_target(cwd, updates, None)
}

fn ensure_project_vault_env_bindings_skipping_target(
    cwd: &Path,
    updates: &BTreeMap<String, String>,
    already_updated_target: Option<&Path>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let canon_cwd = cwd.canonicalize()?;
    let tachi_dir = canon_cwd.join(".tachi");
    if let Ok(meta) = std::fs::symlink_metadata(&tachi_dir) {
        if meta.file_type().is_symlink() {
            let canon_dir = tachi_dir.canonicalize().map_err(|e| {
                format!(
                    ".tachi directory symlink in {} cannot be resolved: {e}",
                    canon_cwd.display()
                )
            })?;
            if !canon_dir.starts_with(&canon_cwd) {
                return Err(format!(
                    ".tachi directory resolves outside project directory {}",
                    canon_cwd.display()
                )
                .into());
            }
        }
    }
    std::fs::create_dir_all(&tachi_dir)?;
    let canon_tachi_dir = tachi_dir.canonicalize()?;
    if !canon_tachi_dir.starts_with(&canon_cwd) {
        return Err(format!(
            ".tachi directory resolves outside project directory {}",
            canon_cwd.display()
        )
        .into());
    }

    let vault_env = canon_tachi_dir.join("vault.env");
    let target = match std::fs::symlink_metadata(&vault_env) {
        Ok(meta) => {
            if meta.file_type().is_symlink() && !vault_env.exists() {
                return Err(format!(
                    ".tachi/vault.env in {} is a dangling symlink",
                    canon_cwd.display()
                )
                .into());
            }
            let canon = vault_env.canonicalize()?;
            if !canon.starts_with(&canon_cwd) {
                return Err(format!(
                    ".tachi/vault.env resolves outside project directory {}",
                    canon_cwd.display()
                )
                .into());
            }
            canon
        }
        Err(_) => vault_env.clone(),
    };

    if already_updated_target.is_some_and(|done| paths_refer_to_same_file(done, &target)) {
        return Ok(vault_env);
    }
    upsert_dotenv_pointer_bindings(&target, updates)?;
    Ok(vault_env)
}

fn upsert_dotenv_pointer_bindings(
    path: &Path,
    updates: &BTreeMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (existing_meta, existing_content) = if path.exists() {
        let meta = std::fs::metadata(path)?;
        let content = crate::vault_crypto::ZeroizingString::new(std::fs::read_to_string(path)?);
        (Some(meta), content)
    } else {
        (
            None,
            crate::vault_crypto::ZeroizingString::new(String::new()),
        )
    };

    let dotenv = super::dotenv_lines::DotenvText::parse(&existing_content)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    drop(existing_content);

    let mut seen_updated: HashSet<String> = HashSet::new();
    let mut out_lines: Vec<String> = Vec::new();
    for entry in &dotenv.entries {
        let raw_line: &str = &entry.text;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out_lines.push(raw_line.to_string());
            continue;
        }
        let (export_prefix, body) = match trimmed.strip_prefix("export ") {
            Some(rest) => ("export ", rest.trim_start()),
            None => ("", trimmed),
        };
        if let Some((raw_key, raw_rhs)) = body.split_once('=') {
            let key = raw_key.trim();
            if let Some(secret_name) = updates.get(key) {
                if seen_updated.insert(key.to_string()) {
                    let comment = super::dotenv_lines::trailing_comment(raw_rhs);
                    out_lines.push(format!("{export_prefix}{key}=vault:{secret_name}{comment}"));
                }
                // Drop any subsequent duplicate definition of the same updated key
                continue;
            }
        }
        out_lines.push(raw_line.to_string());
    }

    for (key, secret_name) in updates {
        if !seen_updated.contains(key) {
            out_lines.push(format!("{key}=vault:{secret_name}"));
        }
    }

    let body = dotenv.render(&out_lines);
    drop(dotenv);
    if let Some(meta) = existing_meta {
        atomic_replace_checked_file(path, &meta, body.as_bytes())
    } else {
        atomic_write_new_or_replace(path, body.as_bytes())
    }
}

fn paths_refer_to_same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

fn verify_file_unchanged(
    target: &Path,
    initial_meta: &std::fs::Metadata,
) -> Result<(), Box<dyn std::error::Error>> {
    let current_meta = std::fs::metadata(target)
        .map_err(|e| format!("Failed to re-stat {} before write: {e}", target.display()))?;
    if !same_file_snapshot(initial_meta, &current_meta) {
        return Err(format!(
            "Refusing to overwrite {}: file was modified concurrently during operation",
            target.display()
        )
        .into());
    }
    Ok(())
}

fn atomic_replace_checked_file(
    target: &Path,
    initial_meta: &std::fs::Metadata,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    verify_file_unchanged(target, initial_meta)?;
    atomic_write_new_or_replace(target, bytes)
}

#[cfg(unix)]
fn same_file_snapshot(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
}

#[cfg(not(unix))]
fn same_file_snapshot(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

fn atomic_write_new_or_replace(
    target: &Path,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    crate::utils::write_owner_only_file_atomic(target, bytes)
        .map_err(|e| format!("Failed to atomically replace {}: {e}", target.display()).into())
}
