// pack.rs — Pack system types for Tachi
//
// A "Pack" is a collection of skills/tools from a source (e.g. a GitHub repo).
// Packs are installed once in Tachi and projected to any agent in its native format.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Known agent types that Tachi can project skills to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Cursor,
    Gemini,
    OpenClaw,
    Antigravity,
    Trae,
    Kiro,
    Generic,
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Cursor => "cursor",
            AgentKind::Gemini => "gemini",
            AgentKind::OpenClaw => "openclaw",
            AgentKind::Antigravity => "antigravity",
            AgentKind::Trae => "trae",
            AgentKind::Kiro => "kiro",
            AgentKind::Generic => "generic",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(AgentKind::Claude),
            "codex" => Some(AgentKind::Codex),
            "cursor" => Some(AgentKind::Cursor),
            "gemini" => Some(AgentKind::Gemini),
            "openclaw" => Some(AgentKind::OpenClaw),
            "antigravity" => Some(AgentKind::Antigravity),
            "trae" => Some(AgentKind::Trae),
            "kiro" => Some(AgentKind::Kiro),
            "generic" => Some(AgentKind::Generic),
            _ => None,
        }
    }

    /// Skill file path pattern for this agent.
    /// Returns (base_dir, file_extension_or_pattern).
    pub fn skill_target(&self) -> (&'static str, &'static str) {
        match self {
            AgentKind::Claude => ("~/.claude/skills", "SKILL.md"),
            AgentKind::Codex => ("~/.codex/skills", "SKILL.md"),
            AgentKind::Cursor => ("~/.cursor/rules", ".mdc"),
            AgentKind::Gemini => ("~/.gemini/skills", "SKILL.md"),
            AgentKind::OpenClaw => ("~/.openclaw/plugins", "tachi-projection.json"),
            AgentKind::Antigravity => ("~/.claude/skills", "SKILL.md"), // shares Claude format
            AgentKind::Trae => ("~/.trae/skills", "SKILL.md"),
            AgentKind::Kiro => ("~/.kiro/skills", "SKILL.md"),
            AgentKind::Generic => ("~/.tachi/skills", "SKILL.md"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackManifest {
    #[serde(default)]
    pub schema_version: String,

    #[serde(default)]
    pub pack: PackManifestMeta,

    #[serde(default)]
    pub services: Vec<String>,

    #[serde(default)]
    pub skills: Vec<PackAssetRef>,

    #[serde(default)]
    pub workflows: Vec<PackAssetRef>,

    #[serde(default)]
    pub runtime: Vec<PackAssetRef>,

    #[serde(default)]
    pub overlays: BTreeMap<String, PackOverlay>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackManifestMeta {
    #[serde(default)]
    pub id: Option<String>,

    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub version: Option<String>,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackAssetRef {
    pub path: String,

    #[serde(default)]
    pub target: Option<String>,

    #[serde(default)]
    pub kind: Option<String>,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackOverlay {
    #[serde(default)]
    pub files: Vec<PackAssetRef>,

    #[serde(default)]
    pub commands: Vec<PackAssetRef>,

    #[serde(default)]
    pub hooks: Vec<PackAssetRef>,

    #[serde(default)]
    pub agents: Vec<PackAssetRef>,

    #[serde(default)]
    pub manifest: Option<Value>,
}

/// A registered skill pack in the Tachi registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pack {
    /// Unique pack identifier, e.g. "obra/superpowers", "garrytan/gstack"
    pub id: String,

    /// Human-readable display name
    pub name: String,

    /// Source URI: "github:garrytan/gstack", "local:/path/to/pack"
    pub source: String,

    /// Installed version string (git tag, commit sha, or semver)
    pub version: String,

    /// Short description
    pub description: String,

    /// Number of skills in this pack
    pub skill_count: u32,

    /// Whether this pack is enabled
    pub enabled: bool,

    /// Local filesystem path where the pack is cloned/stored
    pub local_path: String,

    /// JSON metadata (author, license, tags, etc.)
    pub metadata: String,

    /// ISO 8601 timestamp of installation
    pub installed_at: String,

    /// ISO 8601 timestamp of last update/sync
    pub updated_at: String,
}

/// An agent projection record — tracks which packs are projected to which agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProjection {
    /// Agent kind string
    pub agent: String,

    /// Pack id
    pub pack_id: String,

    /// Whether this projection is active
    pub enabled: bool,

    /// Local path where skills were projected for this agent
    pub projected_path: String,

    /// Number of skills projected
    pub skill_count: u32,

    /// Last sync timestamp
    pub synced_at: String,
}
