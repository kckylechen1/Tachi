pub mod claude_pool;
pub mod llm;

mod backend_tier;
mod default_prompts;
mod path;
mod provider_materialization;
pub mod provider_names;
mod runtime_files;

#[cfg(test)]
mod test_support;

pub use llm::{LlmClient, ProviderSecret};
pub use provider_materialization::{
    group_api_key_values_by_configured_rotations, materialize_provider_secrets, MaterializeReport,
};
pub use provider_names::{
    is_vault_alias, parse_rotation_member_name, parse_vault_alias, vault_alias_line,
    VAULT_ALIAS_PREFIX,
};
