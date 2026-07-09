//! Single source of truth for classifying Vault secret-read errors and the
//! one env-fallback eligibility policy shared by every agent host.
//!
//! Historically each host (`mcp_connection` secret resolution, `gh_ops`
//! transport, enrichment defer logic) inlined its own
//! `starts_with("Vault is locked")` chain to decide "fall back to env" vs
//! "fail". Those predicates drifted and, critically, risked letting an
//! authorization failure silently fall back to an env var. This module
//! collapses them into ONE classifier plus ONE policy so the fail-closed
//! doctrine lives in exactly one place.

/// Machine-readable classification of a Vault secret-read error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaultReadState {
    /// Vault is locked (`unlock_time` is `None`). Recoverable via unlock.
    Locked,
    /// Vault auto-locked after its idle window elapsed. Recoverable.
    AutoLocked,
    /// Vault has never been initialized. Recoverable via init.
    NotInitialized,
    /// The specific secret does not exist in the vault.
    Missing,
    /// The caller is not authorized to read the secret. MUST fail closed.
    NotAuthorized,
    /// Anything we do not recognize. Fail closed by default.
    Unknown,
}

/// Classify a Vault read error message into a [`VaultReadState`].
///
/// Matching is case-insensitive and substring-based so a wrapped error
/// (e.g. `"secret materialization failed: Vault is locked"`) classifies the
/// same as the raw error.
///
/// Ordering matters:
/// * The unambiguous `"Secret not found:"` prefix is checked FIRST, so a secret
///   whose NAME contains an auth-ish word (`Secret not found: authorization_token`)
///   is a plain miss, not an authorization failure.
/// * Authorization failures are then matched by their real phrasings
///   (`Access denied …` from `vault_ops/access.rs`, plus not-authorized/
///   unauthorized/forbidden/not-permitted) BEFORE the lock states, so a message
///   mentioning both auth and a lock state fails closed. Any unrecognized auth
///   phrasing still lands in `Unknown`, which is also fail-closed.
pub(crate) fn classify_vault_read_error(err: &str) -> VaultReadState {
    let lower = err.to_ascii_lowercase();
    if lower.contains("secret not found:") {
        VaultReadState::Missing
    } else if lower.contains("access denied")
        || lower.contains("not authorized")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("not permitted")
    {
        VaultReadState::NotAuthorized
    } else if lower.contains("vault auto-locked") {
        VaultReadState::AutoLocked
    } else if lower.contains("vault is locked") {
        VaultReadState::Locked
    } else if lower.contains("vault not initialized") {
        VaultReadState::NotInitialized
    } else {
        VaultReadState::Unknown
    }
}

/// THE DOCTRINE: the single env-fallback eligibility policy.
///
/// Returns `true` only for recoverable "vault unavailable" states where
/// falling back to an env/config value is safe. `NotAuthorized` and
/// `Unknown` fail closed — an authorization failure must NEVER silently
/// fall back to an env var. Do not invert.
pub(crate) fn is_env_fallback_eligible(state: VaultReadState) -> bool {
    matches!(
        state,
        VaultReadState::Locked
            | VaultReadState::AutoLocked
            | VaultReadState::NotInitialized
            | VaultReadState::Missing
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // G-B1: each known error string maps to its documented state, and an
    // auth/forbidden string maps to NotAuthorized.
    #[test]
    fn classify_maps_known_strings_to_their_states() {
        assert_eq!(
            classify_vault_read_error("Vault is locked. Call vault_unlock first."),
            VaultReadState::Locked
        );
        assert_eq!(
            classify_vault_read_error("Vault auto-locked. Call vault_unlock first."),
            VaultReadState::AutoLocked
        );
        assert_eq!(
            classify_vault_read_error("Vault not initialized. Call vault_init first."),
            VaultReadState::NotInitialized
        );
        assert_eq!(
            classify_vault_read_error("Secret not found: GH_TOKEN"),
            VaultReadState::Missing
        );
        assert_eq!(
            classify_vault_read_error(
                "Agent tachi_gh_ops is not authorized to read secret GH_TOKEN"
            ),
            VaultReadState::NotAuthorized
        );
        assert_eq!(
            classify_vault_read_error("403 Forbidden"),
            VaultReadState::NotAuthorized
        );
        // The real vault ACL-denied phrasing (vault_ops/access.rs).
        assert_eq!(
            classify_vault_read_error("Access denied for agent 'codex' to secret 'GH_TOKEN'."),
            VaultReadState::NotAuthorized
        );
        assert_eq!(
            classify_vault_read_error("some unrelated failure"),
            VaultReadState::Unknown
        );
    }

    // G-B1 (regression, #460 review): a secret whose NAME contains an auth-ish
    // word must still classify as Missing (env-fallback-eligible), NOT as an
    // authorization failure — otherwise a legitimately-absent secret would fail
    // closed instead of falling back to env like it did before this refactor.
    #[test]
    fn classify_missing_secret_with_authish_name_is_still_missing() {
        assert_eq!(
            classify_vault_read_error("Secret not found: authorization_token"),
            VaultReadState::Missing
        );
        assert_eq!(
            classify_vault_read_error("Secret not found: forbidden_key"),
            VaultReadState::Missing
        );
    }

    // G-B1 (wrapped): a wrapped error classifies the same as the raw error.
    #[test]
    fn classify_matches_wrapped_error_strings() {
        assert_eq!(
            classify_vault_read_error("secret materialization failed: Vault is locked"),
            VaultReadState::Locked
        );
    }

    // G-B1 (fail-closed priority): a message mentioning both auth and a lock
    // state must classify as NotAuthorized.
    #[test]
    fn classify_prefers_not_authorized_over_lock_state() {
        assert_eq!(
            classify_vault_read_error("not authorized; Vault is locked"),
            VaultReadState::NotAuthorized
        );
    }

    // G-B2 (DOCTRINE): NotAuthorized (and Unknown) must NOT be env-fallback
    // eligible; the four recoverable states must be.
    #[test]
    fn fallback_policy_fails_closed_for_not_authorized() {
        assert!(!is_env_fallback_eligible(VaultReadState::NotAuthorized));
        assert!(!is_env_fallback_eligible(VaultReadState::Unknown));
        assert!(is_env_fallback_eligible(VaultReadState::Locked));
        assert!(is_env_fallback_eligible(VaultReadState::AutoLocked));
        assert!(is_env_fallback_eligible(VaultReadState::NotInitialized));
        assert!(is_env_fallback_eligible(VaultReadState::Missing));
    }
}
