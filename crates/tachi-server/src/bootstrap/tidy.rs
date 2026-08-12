mod apply;
mod classify;
mod command;
mod migration;
mod render;
mod report;

pub(super) use command::run_tidy_command;

// Feature-gated test surface: `bootstrap/mod.rs` forwards these only when the
// external bootstrap test crate explicitly enables `bootstrap-test-api`.
#[cfg(feature = "bootstrap-test-api")]
pub(crate) use apply::execute_tidy_apply;
#[cfg(feature = "bootstrap-test-api")]
pub use migration::MigrationConfig;
#[cfg(feature = "bootstrap-test-api")]
pub(crate) use migration::{
    authorized_migration_sources, build_migration_plan, execute_tidy_migrations,
    force_boundary_failure_after_archive_stage, update_manifest_after_migration,
};
#[cfg(feature = "bootstrap-test-api")]
pub(crate) use report::build_tidy_report;
