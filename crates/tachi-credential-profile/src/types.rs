use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialProfileDocument {
    pub credential_profiles: HashMap<String, CredentialProfile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialProfile {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub entries: HashMap<String, String>,
    #[serde(default)]
    pub allowed_consumers: AllowedConsumers,
    #[serde(default)]
    pub materializers: Vec<CredentialMaterializer>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedConsumers {
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialMaterializer {
    #[serde(rename = "type")]
    pub kind: String,
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub chmod: Option<String>,
    #[serde(default)]
    pub template: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CredentialMaterializeReport {
    pub profile: String,
    pub consumer: String,
    pub dry_run: bool,
    pub provider: Option<String>,
    pub allowed: bool,
    pub steps: Vec<CredentialMaterializeStepReport>,
    pub missing_secrets: Vec<String>,
    pub denied_secrets: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CredentialMaterializeStepReport {
    pub materializer_type: String,
    pub source: String,
    pub resolved_secret: String,
    pub target: String,
    pub output: String,
    pub status: String,
    pub redacted: bool,
    pub would_write: bool,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chmod: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CredentialDoctorReport {
    pub profile: String,
    pub consumer: String,
    pub issues: Vec<CredentialDoctorIssue>,
    pub summary: CredentialDoctorSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct CredentialDoctorIssue {
    pub severity: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CredentialDoctorSummary {
    pub issue_count: usize,
    pub high_count: usize,
    pub medium_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CredentialApplyOptions {
    pub allow_existing: bool,
    pub run_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CredentialApplyResult {
    pub report: CredentialMaterializeReport,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CredentialCleanupReport {
    pub run_dir: String,
    pub credentials_dir: String,
    pub dry_run: bool,
    pub profile: Option<String>,
    pub consumer: Option<String>,
    pub mark_only: bool,
    pub would_mark: Vec<String>,
    pub marked: Vec<String>,
    pub would_remove: Vec<String>,
    pub removed: Vec<String>,
    pub missing: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CredentialCleanupOptions {
    pub run_dir: Option<PathBuf>,
    pub profile: Option<String>,
    pub consumer: Option<String>,
    pub dry_run: bool,
    pub mark_only: bool,
}
