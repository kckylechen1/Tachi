// tests.rs — llm module tests

use super::embedding::{non_empty_rerank_documents, parse_voyage_batch_embeddings};
use super::provider_health::{
    ClaudeCliFailureKind, KeyAvailability, CLAUDE_CLI_FAILURE_COOLDOWN, HEALTH_OK,
    HEALTH_RATE_LIMITED,
};
use super::{LlmClient, ProviderSecret};
use chrono::Utc;
use memory_core::vault::VaultKeyHealth;
use reqwest::header::AUTHORIZATION;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

// NOTE: each test below MUST use a unique env-var name. Cargo runs
// `#[test]` fns in parallel by default, so sharing a process-wide env var
// causes ordering-dependent flakes (e.g. one test setting the var while
// another asserts it's unset). See:
//   crates/memory-server/src/tests.rs::home_test_lock for the pattern we
//   use when an env var (HOME) genuinely cannot be uniquified.

struct EnvRestore {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let original = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, original }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        if let Some(value) = self.original.as_ref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

mod chat_lanes;
mod client_env;
mod config_json;
mod embedding_rerank;
mod provider_key_persistence;
mod provider_pool;
