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

/// Non-recall-cache half of the SQL classifier for the user-facing Wiki
/// corpus. Compose it through [`user_facing_wiki_sql_where`]; keep it aligned
/// with [`is_user_facing_wiki_entry`].
///
/// The anchor terms (`id GLOB 'anchor:*'`, `path = '/anchors'`, `path GLOB
/// '/anchors/*'`) are the negation of `vector_backfill::ANCHOR_SQL_WHERE`.
/// They are deliberately **case-sensitive** `GLOB`, not `lower(id) GLOB`, so
/// they match [`is_anchor_entry`]'s `id.starts_with("anchor:")` byte-for-byte
/// — the same case-sensitivity `vector_backfill`'s
/// `anchor_membership_is_case_sensitive_like_the_rust_classifier` freezes.
/// (Contrast the `wiki-rem:` term, which *is* `lower(...)`-folded because
/// [`is_reserved_wiki_rem_id`] is ASCII-case-insensitive by design.)
/// `namespace::tests::anchor_sql_and_rust_classifiers_agree` asserts the two
/// sides cannot drift.
///
/// tachi#1569: the predicate is authored as two halves because the
/// recall-cache half is separately escapable. [`is_namespace_search_noise`]
/// has always let a caller opt back into recall-cache rows by naming them in
/// `path_prefix` ([`path_prefix_opts_into_recall_cache`]); the SQL side had
/// no such escape, which was harmless only while the clause fired on
/// `path_prefix` alone (a `/recall-cache` prefix is not a `/wiki` prefix, so
/// the clause never ran on an opted-in query). Once the clause is keyed on
/// *store identity* it runs on every read of the wiki store, including an
/// opted-in one, so it must honour the same escape or it would delete rows
/// in SQL that the Rust classifier was about to hand back. Composition is
/// done by [`user_facing_wiki_sql_where`] rather than by `const` string
/// concatenation, which Rust only allows over literals (`concat!` takes
/// literals, not const idents).
pub const USER_FACING_WIKI_SQL_WHERE_EXCEPT_RECALL_CACHE: &str = r#"
    path != '/wiki/_log'
    AND path NOT GLOB '/wiki/_log/*'
    AND lower(topic) != 'wiki_log'
    AND lower(id) NOT GLOB 'wiki-rem:*'
    AND id NOT GLOB 'anchor:*'
    AND path != '/anchors'
    AND path NOT GLOB '/anchors/*'
    AND COALESCE(json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '$.wiki_log'), 0) != 1
"#;

/// `m.`-qualified counterpart to
/// [`USER_FACING_WIKI_SQL_WHERE_EXCEPT_RECALL_CACHE`].
pub const USER_FACING_WIKI_SQL_WHERE_EXCEPT_RECALL_CACHE_M: &str = r#"
    m.path != '/wiki/_log'
    AND m.path NOT GLOB '/wiki/_log/*'
    AND lower(m.topic) != 'wiki_log'
    AND lower(m.id) NOT GLOB 'wiki-rem:*'
    AND m.id NOT GLOB 'anchor:*'
    AND m.path != '/anchors'
    AND m.path NOT GLOB '/anchors/*'
    AND COALESCE(json_extract(CASE WHEN json_valid(m.metadata) THEN m.metadata ELSE '{}' END, '$.wiki_log'), 0) != 1
"#;

/// The recall-cache exclusion half of the user-facing Wiki predicate. Keep
/// aligned with [`is_recall_cache_entry`] — this is its SQL negation, and it
/// is deliberately `lower(...)`-folded because that classifier compares with
/// `eq_ignore_ascii_case`. (It is therefore *stricter* than
/// [`RECALL_CACHE_SQL_WHERE`], which is case-sensitive; the two are not
/// interchangeable.)
pub const NOT_RECALL_CACHE_SQL_WHERE: &str = r#"
    lower(id) != 'foundry_recall_rerank_cache'
    AND lower(id) NOT GLOB 'foundry:recall-cache:*'
    AND path NOT GLOB '*/recall-cache'
    AND path NOT GLOB '*/recall-cache/*'
    AND instr(path, 'foundry_recall_rerank_cache') = 0
    AND lower(source) != 'foundry_recall_rerank_cache'
    AND lower(topic) != 'foundry_recall_rerank_cache'
    AND lower(topic) != 'recall_rerank_cache'
    AND COALESCE(json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '$.recall_rerank_cache'), 0) != 1
    AND lower(COALESCE(json_extract(CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '$.cache_key'), '')) != 'foundry_recall_rerank_cache'
"#;

/// `m.`-qualified counterpart to [`NOT_RECALL_CACHE_SQL_WHERE`].
pub const NOT_RECALL_CACHE_SQL_WHERE_M: &str = r#"
    lower(m.id) != 'foundry_recall_rerank_cache'
    AND lower(m.id) NOT GLOB 'foundry:recall-cache:*'
    AND m.path NOT GLOB '*/recall-cache'
    AND m.path NOT GLOB '*/recall-cache/*'
    AND instr(m.path, 'foundry_recall_rerank_cache') = 0
    AND lower(m.source) != 'foundry_recall_rerank_cache'
    AND lower(m.topic) != 'foundry_recall_rerank_cache'
    AND lower(m.topic) != 'recall_rerank_cache'
    AND COALESCE(json_extract(CASE WHEN json_valid(m.metadata) THEN m.metadata ELSE '{}' END, '$.recall_rerank_cache'), 0) != 1
    AND lower(COALESCE(json_extract(CASE WHEN json_valid(m.metadata) THEN m.metadata ELSE '{}' END, '$.cache_key'), '')) != 'foundry_recall_rerank_cache'
"#;

/// The user-facing Wiki corpus predicate.
///
/// `qualified` picks the `m.`-qualified column names for queries that join
/// `memories AS m`; both forms must classify identically.
/// `allow_recall_cache` drops the recall-cache half — the SQL mirror of
/// [`is_namespace_search_noise`]'s `path_prefix` opt-in. With it set, the
/// result is the SQL counterpart of
/// [`is_user_facing_wiki_entry_allowing_recall_cache`]; without it, of
/// [`is_user_facing_wiki_entry`].
pub fn user_facing_wiki_sql_where(qualified: bool, allow_recall_cache: bool) -> String {
    let (base, cache) = if qualified {
        (
            USER_FACING_WIKI_SQL_WHERE_EXCEPT_RECALL_CACHE_M,
            NOT_RECALL_CACHE_SQL_WHERE_M,
        )
    } else {
        (
            USER_FACING_WIKI_SQL_WHERE_EXCEPT_RECALL_CACHE,
            NOT_RECALL_CACHE_SQL_WHERE,
        )
    };
    if allow_recall_cache {
        base.to_string()
    } else {
        format!("{base}    AND{cache}")
    }
}

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

/// Rust counterpart of [`user_facing_wiki_sql_where`]. Every internal class the
/// SQL predicate excludes must be excluded here too, or a row filtered out of
/// the SQL projection can still walk back in through a Rust-side `.filter()`
/// (and vice versa).
///
/// tachi#1561: anchors were the drifted class — [`is_namespace_search_noise`]
/// has always dropped them unconditionally and every search query carries an
/// `anchor:`-id exclusion, but this classifier and its SQL mirror did not, so
/// wiki-corpus read surfaces (`list_user_facing_wiki_entries` and everything
/// built on it: browse, read, lint, obsidian export) still projected anchor
/// plumbing rows.
pub fn is_user_facing_wiki_entry(entry: &MemoryEntry) -> bool {
    is_user_facing_wiki_entry_allowing_recall_cache(entry) && !is_recall_cache_entry(entry)
}

/// [`is_user_facing_wiki_entry`] with the recall-cache class allowed through:
/// the Rust counterpart of `user_facing_wiki_sql_where(_, true)`, and the
/// classifier a caller who explicitly addressed `/recall-cache` in
/// `path_prefix` is entitled to (tachi#1569 — same escape hatch
/// [`is_namespace_search_noise`] has always honoured).
pub fn is_user_facing_wiki_entry_allowing_recall_cache(entry: &MemoryEntry) -> bool {
    !is_reserved_wiki_rem_id(&entry.id) && !is_wiki_log_entry(entry) && !is_anchor_entry(entry)
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

/// tachi#1561 residual: whether `entry` is a `/wiki`-namespaced row whose
/// derived lifecycle is not default-retrievable (a draft, a row explicitly
/// marked `pending_review`/`stale`/`superseded`/`rejected`/`candidate`, or a
/// present-but-malformed `metadata.lifecycle` value — the closed
/// `WikiLifecycleV1` vocabulary (tachi-params) fails closed for anything
/// that is not exactly `Active`).
///
/// This is a minimal, memcore-local mirror of tachi-params'
/// `derive_wiki_lifecycle` / `WikiLifecycleV1::is_default_retrievable`
/// (`crates/tachi-params/src/knowledge_artifact.rs`). It is a second
/// implementation, not a re-export or a shared helper crate function,
/// because the dependency edge between the two crates runs the *other*
/// way: `tachi-params/Cargo.toml` depends on `memcore`, so memcore calling
/// back into tachi-params would be a circular crate dependency. Byte-equal
/// parity with the tachi-params original is pinned by
/// `tachi_params::knowledge_artifact::tests::
/// memcore_wiki_lifecycle_gate_matches_derive_wiki_lifecycle` — the only
/// crate that can see both sides of the mirror, since tachi-params already
/// depends on memcore.
///
/// Scope, matching `derive_wiki_lifecycle`'s own path-based defense in
/// depth: only rows under `/wiki` are asked this question at all — a row
/// outside that namespace was never a Wiki artifact, so it never earned a
/// `metadata.lifecycle`/`metadata.review_status` marker from the Wiki
/// writer in the first place. This mirrors how [`is_namespace_search_noise`]
/// composes its own path-scoped classes (kanban, handoff, continuity
/// projection) rather than trying every predicate against every row
/// unconditionally.
pub fn is_non_default_retrievable_wiki_row(entry: &MemoryEntry) -> bool {
    path_in_namespace(&entry.path, "/wiki") && !wiki_row_lifecycle_is_default_retrievable(entry)
}

/// The retrievability half of the `derive_wiki_lifecycle` mirror — see
/// [`is_non_default_retrievable_wiki_row`] for why this is a duplicate, not
/// a re-export.
fn wiki_row_lifecycle_is_default_retrievable(entry: &MemoryEntry) -> bool {
    if let Some(explicit) = entry.metadata.get("lifecycle") {
        // Only the literal `"active"` string is default-retrievable — any
        // other string, and any non-string/malformed value, mirrors
        // `derive_wiki_lifecycle`'s fail-closed `PendingReview` fallback for
        // a present-but-unparseable `metadata.lifecycle`.
        return explicit.as_str().map(str::trim) == Some("active");
    }
    if entry
        .metadata
        .get("review_status")
        .and_then(|v| v.as_str())
        .is_some_and(|status| status.eq_ignore_ascii_case("pending"))
    {
        return false;
    }
    if entry.path == "/wiki/drafts" || entry.path.starts_with("/wiki/drafts/") {
        return false;
    }
    true
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
        // `user_facing_wiki_sql_where`, but that clause only fires when a
        // `/wiki` path_prefix is in play; an unscoped search routed into the
        // wiki store by project-name inference has no such filter, so this
        // function is the only remaining backstop.
        || is_reserved_wiki_rem_id(&entry.id)
}

/// Whether `entry` is bookkeeping the store itself owns — never content a
/// caller who already knows the id should be able to fetch verbatim.
///
/// tachi#1561 wave2 follow-up: `get`/id-addressed reads initially reused
/// [`is_namespace_search_noise`] wholesale (same predicate `search` filters
/// candidates through), but that predicate also drops kanban cards, handoff
/// notes, and continuity projections whenever the caller passes no
/// `path_prefix` — and an id-addressed lookup has no `path_prefix` to opt
/// back in with. Those three classes are the owner's own content (kanban
/// board state, session handoffs, continuity-projection snapshots); they are
/// only noise on *listing/search* surfaces where nothing asked for them by
/// name. Withholding them from a precise `get(id)` is over-tightening: the
/// leak this closes is "searchable without being asked for", not "must never
/// be retrievable by the id the caller already holds".
///
/// So the two predicates split by **surface**, not by strictness:
///
/// - [`is_namespace_search_noise`] answers "should this row surface in a
///   listing/search result the caller did not address by id" — wide, and
///   deliberately excludes the owner's own projection-shaped content unless
///   the query's `path_prefix` opts back in.
/// - `is_internal_only_row` answers "is this a row the store's internal
///   machinery would never want handed back to *any* id-addressed caller" —
///   narrow, and covers only rows nothing outside the store's own storage
///   layer ever produced on purpose: Wiki REM recovery drafts, the Wiki
///   operation log, the recall-rerank cache, and anchor plumbing rows. It
///   takes no `path_prefix` because there is no opt-in shape for an
///   id-addressed read — the id itself is the address.
///
/// `readable_entry` in `tachi-server`'s `get` surface is the current caller;
/// any future id-addressed read surface should reach for this, not
/// [`is_namespace_search_noise`].
pub fn is_internal_only_row(entry: &MemoryEntry) -> bool {
    is_wiki_log_entry(entry)
        || is_reserved_wiki_rem_id(&entry.id)
        || is_recall_cache_entry(entry)
        || is_anchor_entry(entry)
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

    /// Evaluate a wiki-corpus SQL classifier over `entries` in a throwaway
    /// in-memory table, returning the ids the predicate keeps. `qualified`
    /// picks the `m.`-qualified form — both forms must classify identically,
    /// and both must match the Rust classifier.
    fn wiki_sql_kept_ids(entries: &[MemoryEntry], qualified: bool) -> Vec<String> {
        wiki_sql_kept_ids_with_recall_cache(entries, qualified, false)
    }

    fn wiki_sql_kept_ids_with_recall_cache(
        entries: &[MemoryEntry],
        qualified: bool,
        allow_recall_cache: bool,
    ) -> Vec<String> {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 path TEXT NOT NULL,
                 topic TEXT NOT NULL,
                 source TEXT NOT NULL,
                 metadata TEXT NOT NULL
             );",
        )
        .expect("create classifier fixture table");
        {
            let mut stmt = conn
                .prepare(
                    "INSERT INTO memories (id, path, topic, source, metadata)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .expect("prepare fixture insert");
            for entry in entries {
                stmt.execute(rusqlite::params![
                    entry.id,
                    entry.path,
                    entry.topic,
                    entry.source,
                    entry.metadata.to_string(),
                ])
                .expect("insert classifier fixture row");
            }
        }
        let predicate = user_facing_wiki_sql_where(qualified, allow_recall_cache);
        let sql = if qualified {
            format!(
                "SELECT m.id FROM memories AS m
                 WHERE ({predicate}) ORDER BY m.id"
            )
        } else {
            format!(
                "SELECT id FROM memories
                 WHERE ({predicate}) ORDER BY id"
            )
        };
        let mut stmt = conn.prepare(&sql).expect("prepare wiki classifier");
        stmt.query_map([], |row| row.get::<_, String>(0))
            .expect("run wiki classifier")
            .collect::<Result<Vec<String>, _>>()
            .expect("collect wiki classifier rows")
    }

    /// tachi#1561 item 1: anchor plumbing rows must be classified internal by
    /// *every* wiki/search classifier, not just [`is_namespace_search_noise`].
    /// Before this fix the two SQL predicates and
    /// [`is_user_facing_wiki_entry`] all kept them, so `/wiki`-scoped read
    /// surfaces projected anchor rows verbatim.
    #[test]
    fn anchor_sql_and_rust_classifiers_agree() {
        let rows = vec![
            // Anchor by id, parked under a wiki path — the exact leak shape.
            fixture_entry(
                "anchor:issue:kckylechen1/tachi:773",
                "/wiki/agent/tachi",
                "anchor plumbing row",
            ),
            // Anchor by path.
            fixture_entry(
                "6f1c0a2e-anchor-by-path",
                "/anchors/issue/kckylechen1/tachi:773",
                "anchor plumbing row",
            ),
            // Anchor namespace root itself.
            fixture_entry("anchors-root", "/anchors", "anchor namespace root"),
            // Ordinary user-facing wiki page.
            fixture_entry(
                "d290f1ee-6c54-4b01-90e6-d701748f0851",
                "/wiki/xxx",
                "ordinary user-facing wiki content",
            ),
            // Near-miss: `anchor`-shaped text that is NOT the anchor
            // namespace. Must stay user-facing on both sides.
            fixture_entry(
                "anchorage-notes",
                "/wiki/anchors-explained",
                "a page about anchors, not an anchor",
            ),
        ];

        for entry in &rows[..3] {
            assert!(
                is_anchor_entry(entry),
                "fixture must be an anchor: {entry:?}"
            );
            assert!(
                is_namespace_search_noise(entry, None),
                "anchors are unconditional search noise: {entry:?}"
            );
            assert!(
                is_namespace_search_noise(entry, Some("/wiki")),
                "a wiki-scoped read must not opt back into anchors: {entry:?}"
            );
            assert!(
                !is_user_facing_wiki_entry(entry),
                "anchors must not be user-facing wiki corpus: {entry:?}"
            );
        }
        for entry in &rows[3..] {
            assert!(
                !is_anchor_entry(entry),
                "fixture must not be an anchor: {entry:?}"
            );
            assert!(
                is_user_facing_wiki_entry(entry),
                "the anchor exclusion must not eat ordinary wiki rows: {entry:?}"
            );
        }

        let expected = rows[3..]
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        assert_eq!(
            wiki_sql_kept_ids(&rows, false),
            expected,
            "the unqualified wiki predicate must agree with is_user_facing_wiki_entry"
        );
        assert_eq!(
            wiki_sql_kept_ids(&rows, true),
            expected,
            "the m.-qualified wiki predicate must agree with is_user_facing_wiki_entry"
        );
    }

    /// tachi#1569 frozen decision: the SQL clause honours the same
    /// recall-cache opt-in [`is_namespace_search_noise`] does. Without the
    /// opt-in a cache row is filtered in SQL; with it the row survives SQL —
    /// and the *other* internal classes stay filtered either way, so the
    /// escape hatch cannot be used to fish out REM drafts or the wiki log.
    #[test]
    fn recall_cache_opt_in_is_honoured_by_sql_and_rust_alike() {
        let mut cache_by_source = fixture_entry(
            "83a3f7d1-cache-by-source",
            "/wiki/notes/cached",
            "rendered recall rows",
        );
        cache_by_source.source = FOUNDRY_RECALL_CACHE_SOURCE.to_string();
        let rows = vec![
            fixture_entry(
                "0c1d5b2a-cache-by-path",
                "/recall-cache/2026-07-28",
                "rendered recall rows",
            ),
            cache_by_source,
            fixture_entry(
                "wiki-rem:pending",
                "/wiki/drafts/pending",
                "unreviewed draft",
            ),
            fixture_entry("2b7e6f90-wiki-log", "/wiki/_log", "wiki operation log"),
            fixture_entry(
                "d290f1ee-6c54-4b01-90e6-d701748f0851",
                "/wiki/xxx",
                "ordinary user-facing wiki content",
            ),
        ];

        // Rust side: the opt-in moves exactly the two cache rows.
        for entry in &rows[..2] {
            assert!(is_recall_cache_entry(entry), "fixture must be cache row");
            assert!(!is_user_facing_wiki_entry(entry));
            assert!(is_user_facing_wiki_entry_allowing_recall_cache(entry));
            assert!(
                is_namespace_search_noise(entry, None),
                "cache rows are noise when nobody asked for them"
            );
            assert!(
                !is_namespace_search_noise(entry, Some("/recall-cache")),
                "an explicit /recall-cache prefix opts back in"
            );
        }
        for entry in &rows[2..4] {
            assert!(
                !is_user_facing_wiki_entry_allowing_recall_cache(entry),
                "the cache opt-in must not release REM drafts or the wiki log: {entry:?}"
            );
        }

        let ordinary = vec![rows[4].id.clone()];
        let with_cache = {
            let mut ids = vec![rows[0].id.clone(), rows[1].id.clone(), rows[4].id.clone()];
            ids.sort();
            ids
        };
        for qualified in [false, true] {
            assert_eq!(
                wiki_sql_kept_ids_with_recall_cache(&rows, qualified, false),
                ordinary,
                "without the opt-in the SQL clause filters cache rows (qualified={qualified})"
            );
            assert_eq!(
                wiki_sql_kept_ids_with_recall_cache(&rows, qualified, true),
                with_cache,
                "with the opt-in the SQL clause must return cache rows, and only those \
                 (qualified={qualified})"
            );
        }
    }

    /// The anchor terms are case-sensitive `GLOB`, matching
    /// `is_anchor_entry`'s `starts_with("anchor:")` — an `Anchor:`-cased id is
    /// *not* an anchor on either side. Frozen so nobody "hardens" one side
    /// into `lower(id)` without the other.
    #[test]
    fn anchor_membership_is_case_sensitive_on_both_sides() {
        let rows = vec![fixture_entry(
            "Anchor:Not-The-Reserved-Namespace",
            "/wiki/general/mixed-case",
            "mixed-case id is not an anchor",
        )];
        assert!(!is_anchor_entry(&rows[0]));
        assert!(is_user_facing_wiki_entry(&rows[0]));
        assert_eq!(wiki_sql_kept_ids(&rows, false), vec![rows[0].id.clone()]);
        assert_eq!(wiki_sql_kept_ids(&rows, true), vec![rows[0].id.clone()]);
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
