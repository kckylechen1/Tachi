use memcore::{PackAssetRef, PackManifest};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "browse",
    "scripts",
    "test",
    "benchmark",
    "docs",
    "lib",
    "bin",
];
pub(super) const COMMON_OVERLAY_DIRS: &[&str] = &["commands", "hooks", "agents"];
pub(super) const MANIFEST_FILE_NAMES: &[&str] = &["tachi-pack.json"];

#[derive(Debug, Clone)]
pub(super) struct PackDescriptor {
    pub(super) manifest_path: Option<PathBuf>,
    pub(super) manifest: Option<PackManifest>,
    pub(super) services: Vec<String>,
    pub(super) workflow_assets: Vec<PackAssetRef>,
    pub(super) runtime_assets: Vec<PackAssetRef>,
    pub(super) common_overlay_assets: Vec<PackAssetRef>,
    pub(super) agent_overlay_assets: BTreeMap<String, Vec<PackAssetRef>>,
    pub(super) skill_count: u32,
    pub(super) metadata: Value,
}

#[derive(Debug, Clone)]
pub(super) struct SkillFile {
    pub(super) source: PathBuf,
    pub(super) relative_target: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ProjectionSummary {
    pub(super) path: String,
    pub(super) skill_count: u32,
    pub(super) workflow_count: u32,
    pub(super) overlay_count: u32,
    pub(super) runtime_count: u32,
    pub(super) projection_manifest: String,
}
