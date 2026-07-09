// types.rs — Unified data types for memcore
//
// This schema is the **single source of truth** for all consumers:
//   - OpenClaw / tachi-node (Node.js via NAPI)
//   - Rust native (tachi-server MCP/CLI)
//
// Design: serde only, NO binding-specific macros (#[napi], #[pyclass]).
// Bindings use JSON string serialization for maximum compatibility.

use serde::{Deserialize, Serialize};

mod continuity;
mod entry;
mod results;
pub use continuity::*;
pub use entry::*;
pub use results::*;

// ─── Retention Policy ────────────────────────────────────────────────────────

/// Memory retention policy controlling GC behavior.
///
/// - `ephemeral`: short-lived, GC aggressively (low importance threshold)
/// - `durable`:   default; standard GC thresholds apply
/// - `permanent`: never auto-archived by GC (can still be manually archived)
/// - `pinned`:    never auto-archived AND boosted in search results
///
/// NULL in the database is treated as `durable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPolicy {
    Ephemeral,
    #[default]
    Durable,
    Permanent,
    Pinned,
}

impl RetentionPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::Durable => "durable",
            Self::Permanent => "permanent",
            Self::Pinned => "pinned",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s {
            Some("ephemeral") => Self::Ephemeral,
            Some("durable") => Self::Durable,
            Some("permanent") => Self::Permanent,
            Some("pinned") => Self::Pinned,
            _ => Self::Durable, // NULL or unrecognized → durable
        }
    }

    /// Whether GC should skip this policy entirely.
    pub fn is_gc_exempt(&self) -> bool {
        matches!(self, Self::Permanent | Self::Pinned)
    }
}

impl std::fmt::Display for RetentionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Memory Source ──────────────────────────────────────────────────────────

/// Canonical write provenance for memory entries.
///
/// User-controlled callers (e.g. ingest_source) may submit arbitrary strings;
/// those should be funneled through [`MemorySource::parse_or_external`] which
/// either returns a canonical name or an `external:<sanitized>` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    Manual,
    Extraction,
    Migration,
    Auto,
    FoundryDistill,
    FoundryRecallRerankCache,
    Handoff,
    Kanban,
    Wiki,
    Ghost,
    IngestEvent,
}

impl MemorySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Extraction => "extraction",
            Self::Migration => "migration",
            Self::Auto => "auto",
            Self::FoundryDistill => "foundry_distill",
            Self::FoundryRecallRerankCache => "foundry_recall_rerank_cache",
            Self::Handoff => "handoff",
            Self::Kanban => "kanban",
            Self::Wiki => "wiki",
            Self::Ghost => "ghost",
            Self::IngestEvent => "ingest_event",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim() {
            "manual" => Self::Manual,
            "extraction" => Self::Extraction,
            "migration" => Self::Migration,
            "auto" => Self::Auto,
            "foundry_distill" => Self::FoundryDistill,
            "foundry_recall_rerank_cache" => Self::FoundryRecallRerankCache,
            "handoff" => Self::Handoff,
            "kanban" => Self::Kanban,
            "wiki" => Self::Wiki,
            "ghost" => Self::Ghost,
            "ingest_event" => Self::IngestEvent,
            _ => Self::Manual,
        }
    }

    /// Returns canonical source name if `s` matches an enum variant; otherwise
    /// returns `external:<sanitized>` (lowercased, non `[a-z0-9_-]` → `_`).
    /// Idempotent: passing in `external:foo` returns `external:foo` unchanged.
    /// Truncates the suffix to 64 chars.
    pub fn parse_or_external(s: &str) -> String {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Self::Manual.as_str().to_string();
        }
        let lower = trimmed.to_ascii_lowercase();
        // Match canonical
        match lower.as_str() {
            "manual"
            | "extraction"
            | "migration"
            | "auto"
            | "foundry_distill"
            | "foundry_recall_rerank_cache"
            | "handoff"
            | "kanban"
            | "wiki"
            | "ghost"
            | "ingest_event" => return lower,
            _ => {}
        }
        // Already external:?
        if let Some(rest) = lower.strip_prefix("external:") {
            let san = sanitize_source_suffix(rest);
            return format!("external:{}", san);
        }
        let san = sanitize_source_suffix(&lower);
        format!("external:{}", san)
    }

    /// Whether `s` is an acceptable canonical or external: source value.
    pub fn is_canonical(s: &str) -> bool {
        matches!(
            s,
            "manual"
                | "extraction"
                | "migration"
                | "auto"
                | "foundry_distill"
                | "foundry_recall_rerank_cache"
                | "handoff"
                | "kanban"
                | "wiki"
                | "ghost"
                | "ingest_event"
        ) || (s.starts_with("external:") && {
            let rest = &s["external:".len()..];
            !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })
    }
}

impl std::fmt::Display for MemorySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn sanitize_source_suffix(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(64));
    for c in s.chars() {
        let mapped = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            c
        } else if c.is_ascii_uppercase() {
            c.to_ascii_lowercase()
        } else {
            '_'
        };
        out.push(mapped);
        if out.len() >= 64 {
            break;
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_deserialize_openclaw_format() {
        let json = r#"{
            "entry_id": "m_001",
            "lossless_restatement": "Rust is fast",
            "summary": "Rust",
            "keywords": ["rust"],
            "timestamp": "2026-01-01T00:00:00Z",
            "location": "",
            "persons": ["Kyle"],
            "entities": ["OpenClaw"],
            "topic": "tech",
            "scope": "project",
            "path": "/openclaw",
            "category": "fact",
            "importance": 0.8,
            "source_refs": []
        }"#;
        let entry: MemoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "m_001");
        assert_eq!(entry.text, "Rust is fast");
        assert_eq!(entry.persons, vec!["Kyle"]);
    }

    #[test]
    fn test_deserialize_mcp_format() {
        let json = r#"{
            "id": "abc123",
            "text": "记忆系统重写",
            "created_at": "2026-02-23T12:00:00Z",
            "path": "/",
            "importance": 0.7,
            "keywords": ["记忆"]
        }"#;
        let entry: MemoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "abc123");
        assert_eq!(entry.timestamp, "2026-02-23T12:00:00Z");
        assert_eq!(entry.category, "fact"); // default
    }

    #[test]
    fn fold_persons_into_entities_dedups_case_insensitive() {
        let mut entry = MemoryEntry {
            id: "fold-test".into(),
            path: "/test".into(),
            summary: String::new(),
            text: "x".into(),
            importance: 0.5,
            timestamp: "2026-01-01T00:00:00Z".into(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: String::new(),
            keywords: vec![],
            persons: vec!["Kyle".to_string(), "kyle".to_string()],
            entities: vec!["Sigil".to_string()],
            location: String::new(),
            source: "manual".into(),
            scope: "general".into(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata: json!({}),
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        };
        entry.fold_persons_into_entities();
        assert!(entry.persons.is_empty());
        assert!(entry.entities.iter().any(|e| e == "Sigil"));
        assert!(entry.entities.iter().any(|e| e == "Kyle"));
        assert!(entry.entities.iter().any(|e| e == "user"));
        assert!(!entry.entities.iter().any(|e| e == "kyle"));
    }

    #[test]
    fn push_entity_name_maps_kyle_to_user() {
        let mut entities = Vec::new();
        push_entity_name(&mut entities, "Kyle");
        assert_eq!(entities, vec!["Kyle".to_string(), "user".to_string()]);
    }

    #[test]
    fn location_relocation_preserves_non_object_metadata() {
        let mut metadata = json!(["legacy"]);
        let path = apply_location_relocation("/facts/y", "Shanghai", &mut metadata);
        assert_eq!(path, "/facts/y");
        assert_eq!(metadata["geo"], "Shanghai");
        assert_eq!(metadata["legacy_metadata"], json!(["legacy"]));

        let mut metadata = json!("legacy note");
        let path = apply_location_relocation("/notes/x", "/code-review/sigil", &mut metadata);
        assert_eq!(path, "/notes/x");
        assert_eq!(metadata["context_path"], "/code-review/sigil");
        assert_eq!(metadata["legacy_metadata"], "legacy note");
    }

    #[test]
    fn location_relocation_keeps_slash_place_names_as_geo() {
        let mut metadata = json!({});
        let path = apply_location_relocation("/", "Shanghai/Pudong", &mut metadata);
        assert_eq!(path, "/");
        assert_eq!(metadata["geo"], "Shanghai/Pudong");

        assert!(!is_path_like_location("Shanghai/Pudong"));
        assert!(!is_path_like_location("St. Louis"));
        assert!(!is_path_like_location("Mt. Fuji"));
        assert!(!is_path_like_location("Washington, D.C."));
        assert!(is_path_like_location("/code-review/sigil"));
        assert!(is_path_like_location("./notes/x"));
        assert!(is_path_like_location("../notes/x"));
        assert!(is_path_like_location("~/notes/x"));
        assert!(is_path_like_location("../secrets"));
        assert!(is_path_like_location("notes/runbook.md"));
        assert!(is_path_like_location("C:/Users/kyle/notes.txt"));
    }

    #[test]
    fn test_deserialize_legacy_indexed_tags_as_keywords() {
        let json = r#"{
            "id": "ht_001",
            "text": "Hypertachi compatibility test",
            "timestamp": "2026-05-29T12:00:00Z",
            "indexed_tags": ["Alice", "Bob"],
            "domain_key": "hyperion"
        }"#;
        let entry: MemoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "ht_001");
        assert_eq!(entry.keywords, vec!["Alice".to_string(), "Bob".to_string()]);
        assert!(entry.persons.is_empty());
        assert_eq!(entry.domain.as_deref(), Some("hyperion"));
        assert_eq!(entry.location, "");
    }

    #[test]
    fn test_retention_policy_roundtrip() {
        assert_eq!(
            RetentionPolicy::from_str_opt(None),
            RetentionPolicy::Durable
        );
        assert_eq!(
            RetentionPolicy::from_str_opt(Some("ephemeral")),
            RetentionPolicy::Ephemeral
        );
        assert_eq!(
            RetentionPolicy::from_str_opt(Some("pinned")),
            RetentionPolicy::Pinned
        );
        assert!(RetentionPolicy::Permanent.is_gc_exempt());
        assert!(RetentionPolicy::Pinned.is_gc_exempt());
        assert!(!RetentionPolicy::Durable.is_gc_exempt());
        assert!(!RetentionPolicy::Ephemeral.is_gc_exempt());
    }

    #[test]
    fn test_memory_source_roundtrip() {
        for src in [
            MemorySource::Manual,
            MemorySource::Extraction,
            MemorySource::Migration,
            MemorySource::Auto,
            MemorySource::FoundryDistill,
            MemorySource::FoundryRecallRerankCache,
            MemorySource::Handoff,
            MemorySource::Kanban,
            MemorySource::Wiki,
            MemorySource::Ghost,
            MemorySource::IngestEvent,
        ] {
            let s = src.as_str();
            assert_eq!(MemorySource::from_str_opt(Some(s)), src, "roundtrip {s}");
            assert!(MemorySource::is_canonical(s));
            assert_eq!(MemorySource::parse_or_external(s), s);
        }
        // Unknown -> Manual fallback
        assert_eq!(
            MemorySource::from_str_opt(Some("garbage")),
            MemorySource::Manual
        );
        assert_eq!(MemorySource::from_str_opt(None), MemorySource::Manual);
    }

    #[test]
    fn test_parse_or_external_canonical() {
        assert_eq!(MemorySource::parse_or_external("manual"), "manual");
        assert_eq!(MemorySource::parse_or_external("Manual"), "manual");
        assert_eq!(MemorySource::parse_or_external("  ghost  "), "ghost");
    }

    #[test]
    fn test_parse_or_external_non_canonical() {
        let r = MemorySource::parse_or_external("Hub Tool / X");
        assert!(r.starts_with("external:"), "got {r}");
        assert!(MemorySource::is_canonical(&r));
        // Idempotent
        assert_eq!(MemorySource::parse_or_external(&r), r);
    }

    #[test]
    fn test_parse_or_external_empty() {
        assert_eq!(MemorySource::parse_or_external(""), "manual");
        assert_eq!(MemorySource::parse_or_external("   "), "manual");
    }

    #[test]
    fn test_parse_or_external_truncates() {
        let long = "a".repeat(200);
        let r = MemorySource::parse_or_external(&long);
        assert!(r.starts_with("external:"));
        // `external:` (9) + 64 chars = 73 max
        assert!(r.len() <= 73, "len={}", r.len());
    }

    #[test]
    fn test_memory_category_normalize() {
        assert_eq!(MemoryCategory::normalize("fact"), "fact");
        assert_eq!(MemoryCategory::normalize("Decision"), "decision");
        assert_eq!(MemoryCategory::normalize("WeirdThing"), "other");
        assert_eq!(MemoryCategory::normalize(""), "other");
        // Subsystem categories must round-trip cleanly.
        assert_eq!(MemoryCategory::normalize("kanban"), "kanban");
        assert_eq!(MemoryCategory::normalize("Handoff"), "handoff");
        assert_eq!(MemoryCategory::normalize("GHOST"), "ghost");
        assert_eq!(MemoryCategory::normalize("wiki"), "wiki");
        assert_eq!(MemoryCategory::normalize("Guide"), "guide");
        assert_eq!(MemoryCategory::normalize("eval"), "eval");
    }

    #[test]
    fn test_memory_entry_guide_helpers() {
        let entry = MemoryEntry {
            id: "g1".to_string(),
            path: "/guide/fix_pattern/rust".to_string(),
            summary: "".to_string(),
            text: "fix rust build".to_string(),
            importance: 0.7,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "guide".to_string(),
            topic: "".to_string(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: "".to_string(),
            source: "foundry_distill".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
            metadata: serde_json::json!({
                "guide": true,
                "guide_type": "fix_pattern",
                "file_patterns": ["crates/*/src/*.rs"],
                "error_patterns": ["linker error"]
            }),
        };
        assert!(entry.is_guide());
        assert_eq!(entry.guide_type(), Some("fix_pattern"));
        assert_eq!(entry.file_patterns(), vec!["crates/*/src/*.rs"]);
        assert_eq!(entry.error_patterns(), vec!["linker error"]);
    }

    #[test]
    fn test_memory_scope_normalize() {
        assert_eq!(MemoryScope::normalize("user"), "user");
        assert_eq!(MemoryScope::normalize("PROJECT"), "project");
        assert_eq!(MemoryScope::normalize("general"), "general");
        // Reject 'self' / 'other_agent:*' -> general
        assert_eq!(MemoryScope::normalize("self"), "general");
        assert_eq!(MemoryScope::normalize("other_agent:foo"), "general");
        assert_eq!(MemoryScope::normalize(""), "general");
    }

    #[test]
    fn test_default_retention_matrix() {
        assert_eq!(
            default_retention_for("/handoff/foo", "manual"),
            Some("pinned")
        );
        assert_eq!(default_retention_for("/handoff", "manual"), Some("pinned"));
        assert_eq!(default_retention_for("/kanban/x", "manual"), Some("pinned"));
        assert_eq!(
            default_retention_for("/wiki/lessons", "manual"),
            Some("permanent")
        );
        assert_eq!(
            default_retention_for("/notes/2026", "foundry_distill"),
            Some("permanent")
        );
        assert_eq!(default_retention_for("/notes/2026", "manual"), None);
        assert_eq!(default_retention_for("/", "manual"), None);
    }
}
