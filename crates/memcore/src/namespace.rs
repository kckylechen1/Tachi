//! Namespace classification helpers for memory rows.
//!
//! These helpers keep cache/wiki/task namespace rules in one place so search,
//! status, and repair surfaces do not each grow their own partial string list.

use crate::types::{MemoryEntry, ProjectionKind};

pub const FOUNDRY_RECALL_CACHE_SOURCE: &str = "foundry_recall_rerank_cache";
pub const WIKI_REM_OPERATION_ID_PREFIX: &str = "wiki-rem:";

/// Whether `id` occupies the internal Wiki REM operation namespace.
///
/// SQLite's REM recovery queries use ASCII-case-insensitive `LIKE`, so every
/// generic Rust write/mutation guard must reserve the same case-folded prefix.
/// Canonical producer-owned REM ids remain lowercase.
pub fn is_reserved_wiki_rem_id(id: &str) -> bool {
    id.get(..WIKI_REM_OPERATION_ID_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(WIKI_REM_OPERATION_ID_PREFIX))
}

/// SQL predicate for rows that belong to the non-durable recall-cache
/// namespace. Keep this aligned with [`is_recall_cache_entry`].
pub const RECALL_CACHE_SQL_WHERE: &str = r#"
    id = 'foundry_recall_rerank_cache'
    OR id LIKE 'foundry:recall-cache:%'
    OR source = 'foundry_recall_rerank_cache'
    OR topic = 'foundry_recall_rerank_cache'
    OR topic = 'recall_rerank_cache'
    OR path = '/recall-cache'
    OR path LIKE '%/recall-cache'
    OR path LIKE '%/recall-cache/%'
    OR path LIKE '%foundry_recall_rerank_cache%'
    OR COALESCE(json_extract(metadata, '$.recall_rerank_cache'), 0) = 1
    OR COALESCE(json_extract(metadata, '$.cache_key'), '') = 'foundry_recall_rerank_cache'
"#;

/// Same predicate as [`RECALL_CACHE_SQL_WHERE`], `m.`-qualified for queries
/// that join `memories AS m` against another table.
pub const RECALL_CACHE_SQL_WHERE_M: &str = r#"
    m.id = 'foundry_recall_rerank_cache'
    OR m.id LIKE 'foundry:recall-cache:%'
    OR m.source = 'foundry_recall_rerank_cache'
    OR m.topic = 'foundry_recall_rerank_cache'
    OR m.topic = 'recall_rerank_cache'
    OR m.path = '/recall-cache'
    OR m.path LIKE '%/recall-cache'
    OR m.path LIKE '%/recall-cache/%'
    OR m.path LIKE '%foundry_recall_rerank_cache%'
    OR COALESCE(json_extract(m.metadata, '$.recall_rerank_cache'), 0) = 1
    OR COALESCE(json_extract(m.metadata, '$.cache_key'), '') = 'foundry_recall_rerank_cache'
"#;

/// SQL classifier for the user-facing Wiki corpus. Keep this aligned with
/// [`is_user_facing_wiki_entry`].
pub const USER_FACING_WIKI_SQL_WHERE: &str = r#"
    path != '/wiki/_log'
    AND path NOT GLOB '/wiki/_log/*'
    AND lower(topic) != 'wiki_log'
    AND lower(id) NOT GLOB 'wiki-rem:*'
    AND lower(id) != 'foundry_recall_rerank_cache'
    AND lower(id) NOT GLOB 'foundry:recall-cache:*'
    AND path NOT GLOB '*/recall-cache'
    AND path NOT GLOB '*/recall-cache/*'
    AND instr(path, 'foundry_recall_rerank_cache') = 0
    AND lower(source) != 'foundry_recall_rerank_cache'
    AND lower(topic) != 'foundry_recall_rerank_cache'
    AND lower(topic) != 'recall_rerank_cache'
    AND COALESCE(json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '$.wiki_log'), 0) != 1
    AND COALESCE(json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '$.recall_rerank_cache'), 0) != 1
    AND lower(COALESCE(json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '$.cache_key'), '')) != 'foundry_recall_rerank_cache'
"#;

/// `m.`-qualified counterpart to [`USER_FACING_WIKI_SQL_WHERE`].
pub const USER_FACING_WIKI_SQL_WHERE_M: &str = r#"
    m.path != '/wiki/_log'
    AND m.path NOT GLOB '/wiki/_log/*'
    AND lower(m.topic) != 'wiki_log'
    AND lower(m.id) NOT GLOB 'wiki-rem:*'
    AND lower(m.id) != 'foundry_recall_rerank_cache'
    AND lower(m.id) NOT GLOB 'foundry:recall-cache:*'
    AND m.path NOT GLOB '*/recall-cache'
    AND m.path NOT GLOB '*/recall-cache/*'
    AND instr(m.path, 'foundry_recall_rerank_cache') = 0
    AND lower(m.source) != 'foundry_recall_rerank_cache'
    AND lower(m.topic) != 'foundry_recall_rerank_cache'
    AND lower(m.topic) != 'recall_rerank_cache'
    AND COALESCE(json_extract(CASE WHEN json_valid(m.metadata) THEN m.metadata ELSE '{}' END, '$.wiki_log'), 0) != 1
    AND COALESCE(json_extract(CASE WHEN json_valid(m.metadata) THEN m.metadata ELSE '{}' END, '$.recall_rerank_cache'), 0) != 1
    AND lower(COALESCE(json_extract(CASE WHEN json_valid(m.metadata) THEN m.metadata ELSE '{}' END, '$.cache_key'), '')) != 'foundry_recall_rerank_cache'
"#;

fn metadata_bool(entry: &MemoryEntry, key: &str) -> bool {
    entry
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn metadata_bool_or_one(entry: &MemoryEntry, key: &str) -> bool {
    match entry.metadata.get(key) {
        Some(serde_json::Value::Bool(true)) => true,
        Some(serde_json::Value::Number(number)) => {
            number.as_i64() == Some(1) || number.as_u64() == Some(1) || number.as_f64() == Some(1.0)
        }
        _ => false,
    }
}

fn metadata_str_eq(entry: &MemoryEntry, key: &str, expected: &str) -> bool {
    entry
        .metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

pub fn path_in_namespace(path: &str, namespace: &str) -> bool {
    let namespace = namespace.trim_end_matches('/');
    path == namespace || path.starts_with(&format!("{namespace}/"))
}

pub fn path_contains_recall_cache(path: &str) -> bool {
    path == "/recall-cache"
        || path.ends_with("/recall-cache")
        || path.contains("/recall-cache/")
        || path.contains("foundry_recall_rerank_cache")
}

pub fn path_prefix_opts_into_recall_cache(path_prefix: Option<&str>) -> bool {
    path_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .is_some_and(path_contains_recall_cache)
}

pub fn is_recall_cache_entry(entry: &MemoryEntry) -> bool {
    entry
        .source
        .eq_ignore_ascii_case(FOUNDRY_RECALL_CACHE_SOURCE)
        || entry.id.eq_ignore_ascii_case(FOUNDRY_RECALL_CACHE_SOURCE)
        || entry
            .topic
            .eq_ignore_ascii_case(FOUNDRY_RECALL_CACHE_SOURCE)
        || entry.topic.eq_ignore_ascii_case("recall_rerank_cache")
        || entry
            .id
            .to_ascii_lowercase()
            .starts_with("foundry:recall-cache:")
        || path_contains_recall_cache(&entry.path)
        || metadata_bool_or_one(entry, "recall_rerank_cache")
        || metadata_str_eq(entry, "cache_key", FOUNDRY_RECALL_CACHE_SOURCE)
}

/// Internal Wiki bookkeeping that must never consume a user-facing Wiki
/// retrieval budget.
pub fn is_wiki_log_entry(entry: &MemoryEntry) -> bool {
    entry.path == "/wiki/_log"
        || entry.path.starts_with("/wiki/_log/")
        || entry.topic.eq_ignore_ascii_case("wiki_log")
        || metadata_bool_or_one(entry, "wiki_log")
}

pub fn is_user_facing_wiki_entry(entry: &MemoryEntry) -> bool {
    !is_reserved_wiki_rem_id(&entry.id)
        && !is_wiki_log_entry(entry)
        && !is_recall_cache_entry(entry)
}

pub fn is_wiki_entry(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/wiki")
        || entry.source.eq_ignore_ascii_case("wiki")
        || entry.category.eq_ignore_ascii_case("wiki")
        || entry
            .domain
            .as_deref()
            .is_some_and(|domain| domain.eq_ignore_ascii_case("wiki"))
        || metadata_bool(entry, "wiki")
}

// ─── Retrieval surface (memcore ranking rework Phase 2, PIECE 1) ───────────
//
// The owner decided memory search should separate "memory" (experiential:
// decisions, research notes, patterns) from "docs" (reference: wiki, guide)
// into distinct retrieval surfaces instead of fusing them in one ranking
// pool. This piece only adds the *capability* to scope recall by surface
// (`SearchOptions.surface`); it does not change ranking, boosts, or
// fixtures — `surface = None` must reproduce today's fused behavior
// exactly. Research notes are owner-ratified as `Surface::Memory`: they are
// neither wiki entries nor `guide`-category rows, so [`surface_of`]
// classifies them as `Memory` without any special-casing.

/// Retrieval surface a memory row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Reference material: wiki pages and `guide`-category entries.
    Docs,
    /// Everything else: decisions, research notes, patterns, facts, etc.
    Memory,
}

/// Rust classifier for [`Surface`]. Keep aligned with
/// [`DOCS_SURFACE_SQL_WHERE`] / [`DOCS_SURFACE_SQL_WHERE_M`] below — see
/// their doc comment for exactly which signals the SQL mirror covers.
pub fn surface_of(entry: &MemoryEntry) -> Surface {
    if is_wiki_entry(entry) || entry.category.eq_ignore_ascii_case("guide") {
        Surface::Docs
    } else {
        Surface::Memory
    }
}

/// SQL predicate for rows that belong to the `Docs` surface. Mirrors
/// [`surface_of`] (`is_wiki_entry(entry) || category == "guide"`), covering
/// every signal [`is_wiki_entry`] checks: path namespace, `source`,
/// `category`, `domain`, and the `metadata.wiki` flag — plus the
/// `guide`-category rule `surface_of` adds on top. `LOWER(...)` mirrors the
/// Rust classifier's `eq_ignore_ascii_case` case-insensitivity so the SQL
/// and Rust sides cannot silently drift on casing either.
///
/// The `metadata.wiki` term uses `json_type(metadata, '$.wiki') = 'true'`,
/// NOT `COALESCE(json_extract(...), 0) = 1`: `is_wiki_entry`'s Rust side
/// (`metadata_bool`, via `Value::as_bool`) accepts ONLY a JSON boolean
/// `true` -- a numeric `1` or string `"1"` is not `Some(true)`. But
/// `json_extract` collapses JSON `true` AND numeric `1` to the same integer
/// `1`, so `= 1` would make the SQL side match `{"wiki":1}` while Rust
/// classifies it `Memory` -- a real Rust/SQL disagreement, not just a
/// cosmetic one. `json_type` instead returns the JSON value's *type* name:
/// `'true'` only for the JSON boolean `true`, `'integer'` for `1`,
/// `'text'` for `"1"`, and SQL `NULL` when `$.wiki` is absent -- so `=
/// 'true'` matches exactly `as_bool() == Some(true)` for every case, and
/// the absent-key `NULL` is caught by the outer `COALESCE((docs_where), 0)`
/// wrap in [`surface_sql_clause`] (no new three-valued-logic trap). Keep
/// this aligned with [`surface_of`] — `db::tests::surface_ops` asserts the
/// two never disagree over a mixed corpus, including boolean/numeric/string
/// `metadata.wiki` variants.
pub const DOCS_SURFACE_SQL_WHERE: &str = r#"
    path = '/wiki'
    OR path LIKE '/wiki/%'
    OR LOWER(source) = 'wiki'
    OR LOWER(category) = 'wiki'
    OR LOWER(category) = 'guide'
    OR LOWER(domain) = 'wiki'
    OR json_type(metadata, '$.wiki') = 'true'
"#;

/// Same predicate as [`DOCS_SURFACE_SQL_WHERE`], `m.`-qualified for queries
/// that join `memories AS m` against another table (the vector/FTS/trigram
/// channel queries below).
pub const DOCS_SURFACE_SQL_WHERE_M: &str = r#"
    m.path = '/wiki'
    OR m.path LIKE '/wiki/%'
    OR LOWER(m.source) = 'wiki'
    OR LOWER(m.category) = 'wiki'
    OR LOWER(m.category) = 'guide'
    OR LOWER(m.domain) = 'wiki'
    OR json_type(m.metadata, '$.wiki') = 'true'
"#;

/// Build the `AND (...)` clause to splice into a channel query's `WHERE`,
/// gated on `surface`. `None` returns an empty string (no predicate at all —
/// the compatibility guarantee for callers that leave `SearchOptions.surface`
/// unset). `qualified` selects the `m.`-prefixed predicate for queries that
/// join `memories AS m`; pass `false` for the bare-`memories` table scan.
///
/// `COALESCE((docs_where), 0)` guards against SQL three-valued logic: none
/// of `docs_where`'s individual terms are NULL-guarded (the `json_type(...)
/// = 'true'` wiki-metadata term below returns SQL NULL, not FALSE, when
/// `metadata.wiki` is absent -- exactly like a bare `=` comparison would).
/// `domain TEXT` (unlike `source`/`category`, which are `NOT NULL` with
/// schema CHECK constraints and so can never actually be SQL NULL) has no
/// such constraint -- a plain memory/research-note row that never set it
/// (the common case) carries a real NULL `domain` column, making
/// `LOWER(m.domain) = 'wiki'` evaluate to SQL `NULL`, not `FALSE`. `FALSE OR
/// NULL` is `NULL`, so the whole OR-chain collapses to `NULL` for that row
/// (not `FALSE`) unless some other term is `TRUE`. `WHERE ... AND
/// (docs_where)` already drops `NULL` rows (correct for Docs -- no positive
/// signal, so exclude), but `WHERE ... AND NOT (docs_where)` also drops them
/// (`NOT NULL` is `NULL`, still not `TRUE`) -- wrongly excluding a genuine
/// Memory row from `Surface::Memory` scoping. Wrapping in `COALESCE(_, 0)`
/// collapses `NULL` to `0` (`FALSE`) *before* the `NOT`, so `Docs` keeps
/// excluding NULL rows (unchanged) and `Memory` now includes them (fixed).
pub fn surface_sql_clause(surface: Option<Surface>, qualified: bool) -> String {
    let docs_where = if qualified {
        DOCS_SURFACE_SQL_WHERE_M
    } else {
        DOCS_SURFACE_SQL_WHERE
    };
    match surface {
        None => String::new(),
        Some(Surface::Docs) => format!("AND COALESCE(({docs_where}), 0)"),
        Some(Surface::Memory) => format!("AND NOT COALESCE(({docs_where}), 0)"),
    }
}

/// [`surface_sql_clause`], front-padded with a single space when non-empty.
/// Channel query templates splice this directly onto the end of the
/// preceding SQL token (no template-owned newline/indent of its own for the
/// placeholder), so the `None` case -- empty string, zero characters added
/// -- makes the generated query text byte-identical to the pre-surface
/// query, instead of leaving a stray blank/whitespace-only line.
pub(crate) fn surface_sql_splice(surface: Option<Surface>, qualified: bool) -> String {
    let clause = surface_sql_clause(surface, qualified);
    if clause.is_empty() {
        clause
    } else {
        format!(" {clause}")
    }
}

pub fn is_kanban_entry(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/kanban")
        || entry.source.eq_ignore_ascii_case("kanban")
        || entry.category.eq_ignore_ascii_case("kanban")
}

pub fn is_handoff_entry(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/handoff")
        || entry.source.eq_ignore_ascii_case("handoff")
        || entry.category.eq_ignore_ascii_case("handoff")
}

pub fn is_eval_entry(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/eval") || entry.category.eq_ignore_ascii_case("eval")
}

/// tachi#773 item 4: anchor rows (`ensure_anchor`, `/anchors/<kind>/...`,
/// `anchor:`-prefixed ids). Unlike kanban/handoff/wiki, anchors have no
/// scoped "browse anchors as regular search results" use case — they are
/// pinned plumbing rows for the memory graph's entity endpoints, never
/// content a user is searching for. So there is no `path_prefix` override
/// here; [`is_namespace_search_noise`] excludes them unconditionally.
pub fn is_anchor_entry(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/anchors") || entry.id.starts_with("anchor:")
}

pub fn is_continuity_projection_path(path: &str) -> bool {
    ProjectionKind::ALL
        .iter()
        .any(|projection| path_in_namespace(path, projection.path_prefix()))
}

pub fn is_continuity_projection_entry(entry: &MemoryEntry) -> bool {
    entry
        .metadata
        .get("projection_kind")
        .and_then(serde_json::Value::as_str)
        .is_some()
        || is_continuity_projection_path(&entry.path)
}

pub fn path_prefix_opts_into_continuity_projection(path: &str, path_prefix: Option<&str>) -> bool {
    path_prefix.is_some_and(|prefix| {
        ProjectionKind::ALL.iter().any(|projection| {
            let namespace = projection.path_prefix();
            path_in_namespace(prefix, namespace) && path_in_namespace(path, namespace)
        })
    })
}

pub fn is_namespace_search_noise(entry: &MemoryEntry, path_prefix: Option<&str>) -> bool {
    let kanban_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/kanban"));
    let handoff_scoped = path_prefix.is_some_and(|prefix| prefix.starts_with("/handoff"));
    is_wiki_log_entry(entry)
        || (!path_prefix_opts_into_recall_cache(path_prefix) && is_recall_cache_entry(entry))
        || (!kanban_scoped && is_kanban_entry(entry))
        || (!handoff_scoped && is_handoff_entry(entry))
        || (!path_prefix_opts_into_continuity_projection(&entry.path, path_prefix)
            && is_continuity_projection_entry(entry))
        || is_anchor_entry(entry)
        // Generic-surface backstop for reserved Wiki REM operation rows. The
        // wiki-scoped surface already excludes these via
        // `USER_FACING_WIKI_SQL_WHERE`, but that clause only fires when a
        // `/wiki` path_prefix is in play; an unscoped search routed into the
        // wiki store by project-name inference has no such filter, so this
        // function is the only remaining backstop.
        || is_reserved_wiki_rem_id(&entry.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rem_operation_namespace_matches_sqlite_ascii_case_folding() {
        assert!(is_reserved_wiki_rem_id("wiki-rem:canonical"));
        assert!(is_reserved_wiki_rem_id("Wiki-Rem:spoof"));
        assert!(is_reserved_wiki_rem_id("WIKI-REM:spoof"));
        assert!(!is_reserved_wiki_rem_id("wiki-rem"));
        assert!(!is_reserved_wiki_rem_id("wiki:ordinary"));
    }

    fn fixture_entry(id: &str, path: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: path.to_string(),
            summary: content.to_string(),
            text: content.to_string(),
            importance: 0.7,
            timestamp: "2026-07-28T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "fixture".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn namespace_search_noise_excludes_reserved_wiki_rem_rows() {
        let rem_draft = fixture_entry(
            "wiki-rem:9f2c1a",
            "/wiki/drafts/9f2c1a",
            "unreviewed REM draft body",
        );
        assert!(
            is_namespace_search_noise(&rem_draft, None),
            "unscoped search must treat reserved wiki-rem ids as noise, \
             not surface unreviewed draft bodies"
        );
    }

    #[test]
    fn namespace_search_noise_keeps_user_facing_wiki_rows() {
        let user_wiki_entry = fixture_entry(
            "d290f1ee-6c54-4b01-90e6-d701748f0851",
            "/wiki/xxx",
            "ordinary user-facing wiki content",
        );
        assert!(
            !is_namespace_search_noise(&user_wiki_entry, None),
            "the wiki-rem backstop must not classify ordinary user-facing \
             wiki entries as noise"
        );
    }
}
