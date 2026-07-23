//! Cross-harness injection-surface doctor (#1307 first slice).
//!
//! Report-only: inventories MCP / plugin / credential / environment / density
//! planes from a local fleet registry. Never writes, never chmods, never
//! uninstalls, and never opens credential file contents.

mod command;
mod print;
mod report;
#[cfg(test)]
mod tests;

pub(super) use command::run_injection_surface_command;

use serde::Serialize;

const SCHEMA_VERSION: &str = "tachi.injection_surface.doctor.v1";

const PLANE_NAMES: &[&str] = &["mcp", "plugin", "credential", "environment", "density"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct Finding {
    pub harness_id: String,
    pub plane: String,
    pub check_kind: String,
    pub evidence_path: String,
    pub severity: String,
    pub remediation_owner: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PlaneAccount {
    pub harness_id: String,
    pub plane: String,
    /// `scanned` | `unscanned`
    pub status: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct DoctorSummary {
    pub harnesses: usize,
    pub planes_scanned: usize,
    pub planes_unscanned: usize,
    pub findings: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct InjectionSurfaceReport {
    pub schema_version: String,
    pub generated_at: String,
    pub registry_path: String,
    pub home: Option<String>,
    pub summary: DoctorSummary,
    pub plane_accounts: Vec<PlaneAccount>,
    pub findings: Vec<Finding>,
}
