pub mod llm;
pub mod llm_recorder;

mod backend_tier;
mod default_prompts;
mod provider_materialization;
pub mod provider_names;
mod runtime_files;

#[cfg(test)]
mod test_support;

pub use llm::{
    LlmClient, ProviderAuthProbeClass, ProviderAuthProbeFamily, ProviderAuthProbeResult,
    ProviderInvocationFailure, ProviderInvocationFailureClass, ProviderInvocationOutcome,
    ProviderInvocationReceipt, ProviderSecret, ReasoningOutcome, RerankConfig, RerankProviderKind,
    RERANK_LOCAL_ENDPOINT_ENV, RERANK_PROVIDER_ENV, RERANK_VOYAGE_ENDPOINT_ENV,
};
/// Test-only entry: pre-resolved pools cannot report [`VaultSourceAvailability`],
/// so production materialization must use the durable-source entry above.
#[cfg(any(test, feature = "test-support"))]
pub use provider_materialization::materialize_provider_secrets;
pub use provider_materialization::{
    group_api_key_values_by_configured_rotations, materialize_provider_secrets_from_durable_source,
    MaterializeReport, VaultSourceAvailability,
};
pub use provider_names::{
    is_vault_alias, parse_rotation_member_name, parse_vault_alias, vault_alias_line,
    VAULT_ALIAS_PREFIX,
};

// ── TLS crypto provider ──────────────────────────────────────────────────────
//
// reqwest is built with `rustls-no-provider`, so rustls ships WITHOUT a default
// crypto backend. We install `ring` once per process so the first
// HTTPS call doesn't panic with "no process-level CryptoProvider available".
// This replaces the heavier aws-lc-rs backend that reqwest's `rustls` feature
// pulls in by default, removing aws-lc-sys from cold builds.
//
// Idempotent: the `static Once` guarantees the provider is installed at most
// once even when called from multiple entry points (run_cli, tests, daemon).

/// Install the rustls `ring` crypto provider as the process default.
/// Safe to call more than once; only the first call has an effect.
pub fn install_tls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}
