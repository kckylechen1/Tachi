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
    auth_probe_descriptor_for_host, auth_probe_descriptor_for_provider_kind, CompletionStatusV1,
    Generated, LlmClient, ModelEngineKindV1, ModelInvocationLaneV1,
    PersistedModelInvocationReceiptV1, ProviderAuthProbeClass, ProviderAuthProbeFamily,
    ProviderAuthProbeResult, ProviderInvocationFailure, ProviderInvocationFailureClass,
    ProviderInvocationOutcome, ProviderInvocationReceipt, ProviderProbeDescriptor, ProviderSecret,
    ReasoningOutcome, RerankConfig, RerankProviderKind, AUTH_PROBE_DESCRIPTORS,
    DEEPSEEK_AUTH_PROBE, LLM_OUTPUT_TRUNCATED, MODEL_INVOCATION_SCHEMA_V1,
    PROVIDER_HEALTH_PERSIST_CANCELLED_CAUSE, PROVIDER_HEALTH_PERSIST_SQLITE_DEADLINE_CAUSE,
    RERANK_LOCAL_ENDPOINT_ENV, RERANK_PROVIDER_ENV, RERANK_VOYAGE_ENDPOINT_ENV,
    SILICONFLOW_AUTH_PROBE, ZAI_AUTH_PROBE, ZAI_BIGMODEL_AUTH_PROBE,
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
