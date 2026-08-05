//! Credential profile planning and application for Vault materialization.
//!
//! Reports remain redacted, while apply can prepare child-process env values and
//! write guarded credential/config files after the caller supplies decrypted
//! Vault values.

mod apply;
mod cleanup;
mod doctor;
mod io;
mod managed;
mod plan;
mod render;
mod safety;
mod types;

#[cfg(test)]
mod test_fixtures;
#[cfg(test)]
mod tests;

pub const CREDENTIAL_MATERIALIZATION_NAMESPACE: &str = "credential_materialization";

pub use apply::apply_credential_materialization;
pub use cleanup::{
    cleanup_ephemeral_credential_materializations, cleanup_managed_credential_materializations,
};
pub use doctor::doctor_credential_profile;
pub use io::{
    default_credentials_dir, find_credential_profile, load_credential_profile_from_path,
};
pub use plan::{
    credential_materialize_report_json, plan_credential_materialization,
    plan_credential_materialization_with_run_dir, profile_secret_names,
};
// Production callers use these types on the crate::credential_profile::* surface.
pub use types::{
    CredentialApplyOptions, CredentialCleanupOptions, CredentialMaterializeReport,
    CredentialProfile,
};
// Test constructors used via `crate::credential_profile::*` in credential_tests.
#[cfg(test)]
pub use types::{AllowedConsumers, CredentialMaterializer, CredentialProfileDocument};
