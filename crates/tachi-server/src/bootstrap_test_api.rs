//! Feature-gated friend surface for `tachi-bootstrap-tests` (#1714).
//!
//! This module is not part of the product API. It keeps the external test
//! binary on native bootstrap result types while retaining mutation authority,
//! daemon locks, manifest internals, and ownership probes behind opaque guards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use crate::bootstrap::{
    MigrationConfig, SetupItem, SetupReport, TidyAppliedStep, TidyApplySummary, TidyExecuteSummary,
    TidyFinding, TidyGroupSummary, TidyMigration, TidyMigrationOutcome, TidyPlanStep, TidyReport,
};
pub use crate::physical_db_identity::{
    InventoryFailureKind, OpenPathBasis, PhysicalDbStore, PhysicalStoreMutationState,
};

/// Opaque scan-captured authority bundle. Raw physical mutation authority is
/// deliberately never exposed across the test facade.
pub struct AuthorizedMigrationSources {
    inner: BTreeMap<String, crate::physical_db_identity::PhysicalMutationAuthority>,
}

impl AuthorizedMigrationSources {
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

pub fn build_setup_report(
    home: &Path,
    app_home: &Path,
    global_db_path: &PathBuf,
    project_db_path: Option<&PathBuf>,
    git_root: Option<&PathBuf>,
    env_vars: &std::collections::HashMap<String, String>,
) -> Result<SetupReport, Box<dyn std::error::Error>> {
    crate::bootstrap::build_setup_report(
        home,
        app_home,
        global_db_path,
        project_db_path,
        git_root,
        env_vars,
    )
}

pub fn build_tidy_report(
    roots: &[PathBuf],
    git_root: Option<&PathBuf>,
) -> Result<TidyReport, Box<dyn std::error::Error>> {
    crate::bootstrap::build_tidy_report(roots, git_root)
}

pub fn build_migration_plan(
    report: &TidyReport,
    target_db: &Path,
    archive_root: &Path,
    home: &Path,
) -> Vec<TidyMigration> {
    crate::bootstrap::build_migration_plan(report, target_db, archive_root, home)
}

pub fn authorized_migration_sources(report: &TidyReport) -> AuthorizedMigrationSources {
    AuthorizedMigrationSources {
        inner: crate::bootstrap::authorized_migration_sources(report),
    }
}

/// Capture authority for tests that construct a migration plan directly.
/// The capability remains opaque and can only be consumed by the executor
/// wrapper below.
pub fn capture_migration_sources(
    plan: &[TidyMigration],
) -> Result<AuthorizedMigrationSources, Box<dyn std::error::Error>> {
    let inner = plan
        .iter()
        .map(|migration| {
            crate::physical_db_identity::PhysicalMutationAuthority::capture(Path::new(
                &migration.source_path,
            ))
            .map(|authority| (migration.source_path.clone(), authority))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(AuthorizedMigrationSources { inner })
}

pub fn execute_tidy_migrations(
    plan: &[TidyMigration],
    cfg: &MigrationConfig,
    authorized_sources: &AuthorizedMigrationSources,
) -> Result<TidyExecuteSummary, Box<dyn std::error::Error>> {
    crate::bootstrap::execute_tidy_migrations(plan, cfg, &authorized_sources.inner)
}

pub fn update_manifest_after_migration(
    cfg: &MigrationConfig,
    outcomes: &[TidyMigrationOutcome],
) -> Result<(), Box<dyn std::error::Error>> {
    crate::bootstrap::update_manifest_after_migration(cfg, outcomes)
}

pub fn execute_tidy_apply(
    app_home: &Path,
    report: &TidyReport,
) -> Result<TidyApplySummary, Box<dyn std::error::Error>> {
    crate::bootstrap::execute_tidy_apply(app_home, report)
}

pub fn force_boundary_failure_after_archive_stage(enabled: bool) {
    crate::bootstrap::force_boundary_failure_after_archive_stage(enabled);
}

/// The six inventory fields whose parity with `TidyReport` is frozen by the
/// moved hardlink/symlink test. The real doctor scan is executed below; these
/// values are not reconstructed from the tidy report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoctorInventoryCounts {
    pub total_databases: usize,
    pub total_memories: usize,
    pub total_aliases: usize,
    pub resolved_aliases: usize,
    pub unresolved_paths: usize,
    pub path_appearances: usize,
}

pub fn scan_doctor_inventory_counts(
    roots: &[PathBuf],
    quarantine_dir: &Path,
) -> DoctorInventoryCounts {
    let report = crate::doctor::scan(roots, quarantine_dir, crate::doctor::ScanOptions::default());
    DoctorInventoryCounts {
        total_databases: report.summary.total_databases,
        total_memories: report.summary.total_memories,
        total_aliases: report.summary.total_aliases,
        resolved_aliases: report.summary.resolved_aliases,
        unresolved_paths: report.summary.unresolved_paths,
        path_appearances: report.summary.path_appearances,
    }
}

#[cfg(unix)]
pub enum OwnershipInjection {
    Owned,
    Unknown(String),
}

/// Clears the thread-local injection even if an assertion unwinds.
#[cfg(unix)]
pub struct OwnershipInjectionGuard;

#[cfg(unix)]
impl Drop for OwnershipInjectionGuard {
    fn drop(&mut self) {
        crate::db_ownership::set_ownership_inject_for_test(None);
    }
}

#[cfg(unix)]
pub fn inject_ownership(value: OwnershipInjection) -> OwnershipInjectionGuard {
    let value = match value {
        OwnershipInjection::Owned => crate::db_ownership::DbOwnership::Owned,
        OwnershipInjection::Unknown(reason) => crate::db_ownership::DbOwnership::Unknown(reason),
    };
    crate::db_ownership::set_ownership_inject_for_test(Some(value));
    OwnershipInjectionGuard
}

enum BootstrapTestLockInner {
    Source {
        _guard: crate::daemon_lock::DaemonLock,
    },
    Outer {
        _guard: crate::daemon_lock::DualDaemonLock,
    },
}

/// Opaque RAII guard; lock implementation and paths remain private.
pub struct BootstrapTestLock {
    _inner: BootstrapTestLockInner,
}

fn require_distinct_scopes(
    app_home: &Path,
    source_db: &Path,
    target_db: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let source = crate::daemon_lock::scoped_daemon_lock_path(app_home, source_db);
    let target = crate::daemon_lock::scoped_daemon_lock_path(app_home, target_db);
    if source == target {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "fixture must exercise distinct source and target daemon scopes",
        )
        .into());
    }
    Ok(source)
}

pub fn hold_source_scope_lock(
    app_home: &Path,
    source_db: &Path,
    target_db: &Path,
) -> Result<BootstrapTestLock, Box<dyn std::error::Error>> {
    let source_path = require_distinct_scopes(app_home, source_db, target_db)?;
    let guard = crate::daemon_lock::DaemonLock::acquire(&source_path)?;
    Ok(BootstrapTestLock {
        _inner: BootstrapTestLockInner::Source { _guard: guard },
    })
}

pub fn hold_outer_target_lock(
    app_home: &Path,
    target_db: &Path,
    source_db: &Path,
) -> Result<BootstrapTestLock, Box<dyn std::error::Error>> {
    require_distinct_scopes(app_home, source_db, target_db)?;
    let guard = crate::daemon_lock::DualDaemonLock::acquire(app_home, target_db)?;
    Ok(BootstrapTestLock {
        _inner: BootstrapTestLockInner::Outer { _guard: guard },
    })
}
