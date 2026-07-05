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

pub fn parse_rotation_member_name(name: &str) -> Option<(&str, u32)> {
    let (prefix, suffix) = name.rsplit_once('_')?;
    let index = suffix.parse::<u32>().ok()?;
    if prefix.is_empty() {
        None
    } else {
        Some((prefix, index))
    }
}
