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

pub(crate) const CREDENTIAL_MATERIALIZATION_NAMESPACE: &str = "credential_materialization";

#[allow(unused_imports)]
pub(crate) use apply::apply_credential_materialization;
#[allow(unused_imports)]
pub(crate) use cleanup::{
    cleanup_ephemeral_credential_materializations, cleanup_managed_credential_materializations,
};
#[allow(unused_imports)]
pub(crate) use doctor::doctor_credential_profile;
#[allow(unused_imports)]
pub(crate) use io::{
    default_credentials_dir, find_credential_profile, load_credential_profile_from_path,
};
#[allow(unused_imports)]
pub(crate) use plan::{
    credential_materialize_report_json, plan_credential_materialization,
    plan_credential_materialization_with_run_dir, profile_secret_names,
};
#[allow(unused_imports)]
pub(crate) use types::{
    AllowedConsumers, CredentialApplyOptions, CredentialApplyResult, CredentialCleanupOptions,
    CredentialCleanupReport, CredentialDoctorIssue, CredentialDoctorReport,
    CredentialDoctorSummary, CredentialMaterializeReport, CredentialMaterializeStepReport,
    CredentialMaterializer, CredentialProfile, CredentialProfileDocument,
};
