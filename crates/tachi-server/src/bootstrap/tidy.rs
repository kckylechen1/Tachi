mod apply;
mod classify;
mod command;
mod migration;
mod render;
mod report;

pub(super) use command::run_tidy_command;

// Test-only surface: `bootstrap/mod.rs` re-exports these under `#[cfg(test)]`
// for `crate::bootstrap::*` callers in unit tests.
#[cfg(test)]
pub(crate) use apply::execute_tidy_apply;
#[cfg(test)]
pub(crate) use migration::{
    authorized_migration_sources, build_migration_plan, execute_tidy_migrations,
    update_manifest_after_migration, MigrationConfig,
};
#[cfg(test)]
pub(crate) use report::build_tidy_report;
