pub const VAULT_ALIAS_PREFIX: &str = "vault:";

/// `vault:VOYAGE_API_KEY` -> `Some("VOYAGE_API_KEY")`
pub fn parse_vault_alias(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    trimmed
        .strip_prefix(VAULT_ALIAS_PREFIX)
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

pub fn is_vault_alias(value: &str) -> bool {
    parse_vault_alias(value).is_some()
}

/// Recommended config.env line for a provider key stored in Vault.
pub fn vault_alias_line(env_key: &str) -> String {
    format!("{env_key}={VAULT_ALIAS_PREFIX}{env_key}")
}

/// Character/length rules for a Vault secret name referenced by a `vault:`
/// alias. Mirrors `tachi-server::vault_crypto::validate_secret_name`
/// (crates/tachi-server/src/vault_crypto.rs:383-399), which is the single
/// source of truth for what Vault accepts when a secret is written. `tachi-llm`
/// cannot depend on `tachi-server` (the crate dependency runs the other way —
/// tachi-server depends on tachi-llm), so this is a mirrored copy, not a shared
/// call. If the Vault-side rule ever changes, update both.
///
/// tachi#1287: a `vault:` alias whose referenced name fails this check is a
/// config typo (space, stray punctuation, oversized name) — that must fail
/// loudly, not be swallowed as a tolerable "secret missing" skip.
pub fn validate_vault_alias_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Vault alias name cannot be empty".to_string());
    }
    if name.len() > 128 {
        return Err("Vault alias name too long (max 128 chars)".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
    {
        return Err(
            "Vault alias name contains invalid characters (allowed: a-z, A-Z, 0-9, _, ., -)"
                .to_string(),
        );
    }
    Ok(())
}

pub fn parse_rotation_member_name(name: &str) -> Option<(&str, u32)> {
    let (prefix, suffix) = name.rsplit_once('_')?;
    let index = suffix.parse::<u32>().ok()?;
    if prefix.is_empty() {
        None
    } else {
        Some((prefix, index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_vault_alias_name_accepts_valid_names() {
        assert!(validate_vault_alias_name("VOYAGE_API_KEY").is_ok());
        assert!(validate_vault_alias_name("some.key-name_1").is_ok());
    }

    #[test]
    fn validate_vault_alias_name_rejects_bad_characters() {
        assert!(validate_vault_alias_name("invalid key with spaces").is_err());
        assert!(validate_vault_alias_name("key!with$symbols").is_err());
    }

    #[test]
    fn validate_vault_alias_name_rejects_oversized_names() {
        let oversized = "A".repeat(129);
        assert!(validate_vault_alias_name(&oversized).is_err());
        let boundary = "A".repeat(128);
        assert!(validate_vault_alias_name(&boundary).is_ok());
    }

    #[test]
    fn validate_vault_alias_name_rejects_empty() {
        assert!(validate_vault_alias_name("").is_err());
    }
}
