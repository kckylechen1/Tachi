use super::*;

// #1680/D3 fixture repair: this test's key used to be a synthetic sentinel
// (`TACHI_ENV_FALLBACK_API_KEY`) that was never a registered provider name
// under any `KeyClass`. It only materialized into the LLM provider cache
// because pre-#1680 `materialize_for_server_inner` admitted every
// Vault-stored `*_API_KEY` pool unconditionally — the exact discrimination-2
// violation PR-A's `filter_model_provider_pools` (provider_config.rs) fixes
// by admitting only registered `KeyClass::ModelApi` pool names. The test's
// semantic assertion (locking the vault clears only the Vault override; the
// env fallback survives) is orthogonal to which specific key is used, so the
// fix is to re-register the sentinel onto a real ModelApi name —
// `ZHIPUAI_API_KEY`, chosen because nothing else in this crate mutates it
// via raw process env (grepped repo-wide), so it carries the same
// collision-free property the old synthetic name had. Assertion expected
// values and priority logic are byte-for-byte unchanged; only the key name
// changed, plus a `global_test_lock` guard added (this file's established
// convention for any test mutating ambient process env — see
// `rotation.rs`/`child_policy.rs` — which this test was missing even under
// its old unique sentinel name).
//
// codex NEEDS-FIXES OK-BUT-6: the raw `std::env::set_var`/`remove_var` pair
// leaked `ZHIPUAI_API_KEY` into the process environment on panic (the
// unconditional `remove_var` at the end never runs if an `.expect(...)`
// above it fails). Switched to `crate::test_support::EnvRestore` (this
// file's sibling `rotation.rs` already uses it) — its `Drop` impl restores
// the prior value unconditionally, including on panic/early-return.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-wide env across async vault setup
async fn vault_lock_preserves_env_provider_fallback() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _env = crate::test_support::EnvRestore::set("ZHIPUAI_API_KEY", "env-secret");
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "env-fallback-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "ZHIPUAI_API_KEY".to_string(),
            value: "vault-secret".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "env fallback test".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set should succeed");

    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["ZHIPUAI_API_KEY"])
            .as_deref(),
        Some("vault-secret")
    );

    server
        .vault_lock()
        .await
        .expect("vault_lock should succeed");

    assert_eq!(
        server
            .llm
            .provider_secret_for_tests(&["ZHIPUAI_API_KEY"])
            .as_deref(),
        Some("env-secret"),
        "locking vault must clear only vault overrides and preserve env fallback"
    );
    // `_env` (EnvRestore) restores/removes ZHIPUAI_API_KEY on drop, including
    // on an earlier panic — no explicit remove_var needed or wanted here.
}

/// #1680/D3 default-deny discriminator (positive contract): a synthetic
/// `*_API_KEY` name with no `API_KEY_DEFS` entry under any `KeyClass` must
/// never reach the LLM provider cache via Vault materialization — even
/// though Vault pool loading itself is registry-blind
/// (`vault_ops::access::load_unlocked_api_key_secret_pools` admits any
/// standalone `*_API_KEY` entry). This pins
/// `provider_config::filter_model_provider_pools`'s compile-time allowlist
/// semantics as a permanent, positive guarantee: a future change that
/// quietly widened the seam back to "admit everything" (the exact
/// pre-#1680 behavior `vault_lock_preserves_env_provider_fallback` used to
/// incidentally depend on) fails loudly here instead of only being caught
/// by the absence of a warning.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // serializes process-wide env across async vault setup
async fn vault_set_of_unregistered_synthetic_key_never_materializes_into_provider_cache() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let server = make_server();

    server
        .vault_init(Parameters(VaultInitParams {
            password: "default-deny-password".to_string(),
        }))
        .await
        .expect("vault_init should succeed");
    server
        .vault_set(Parameters(VaultSetParams {
            name: "TACHI_UNREGISTERED_SYNTHETIC_API_KEY".to_string(),
            value: "should-never-materialize".to_string(),
            agent_id: None,
            secret_type: "api_key".to_string(),
            description: "default-deny discriminator: absent from API_KEY_DEFS".to_string(),
            allowed_agents: None,
            enable_rotation: false,
            rotation_strategy: None,
            rebind: false,
        }))
        .await
        .expect("vault_set should succeed");

    assert!(
        server
            .llm
            .provider_secret_for_tests(&["TACHI_UNREGISTERED_SYNTHETIC_API_KEY"])
            .is_none(),
        "an unregistered *_API_KEY name must never reach the LLM provider cache \
         (default-deny: only KeyClass::ModelApi-registered names are admitted)"
    );
}
