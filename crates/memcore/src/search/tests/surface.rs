//! memcore ranking rework Phase 2 PIECE 1: surface-scoped recall foundation.
//!
//! These tests exercise `SearchOptions.surface` end-to-end through
//! `hybrid_search`. They intentionally do NOT touch ranking/boosts/fixtures
//! -- only whether a row appears in the result set for a given surface
//! scope. The corpus below mirrors the packet's own contract: a research
//! note is neither a wiki entry nor `guide`-category, so it must classify
//! (and retrieve) as `Surface::Memory`, never `Surface::Docs`.

use super::*;
use crate::namespace::Surface;
use std::collections::HashSet;

const SHARED_PROBE_TERM: &str = "SurfaceScopeProbe20260722";

fn seed_surface_corpus(conn: &mut Connection) {
    // Docs surface: wiki entry (is_wiki_entry via path + category).
    let mut wiki = memory_entry(
        "surface-wiki-entry",
        &format!("{SHARED_PROBE_TERM} wiki reference page about the recall subsystem"),
        &[SHARED_PROBE_TERM],
    );
    wiki.path = "/wiki/recall-subsystem".to_string();
    wiki.category = "wiki".to_string();
    insert_entry(conn, wiki);

    // Docs surface: guide entry (category == "guide", path NOT under /wiki --
    // matches the golden_corpus convention of `/guide/*` guide rows).
    let mut guide = memory_entry(
        "surface-guide-entry",
        &format!("{SHARED_PROBE_TERM} guide walkthrough for onboarding the recall subsystem"),
        &[SHARED_PROBE_TERM],
    );
    guide.path = "/guide/recall-onboarding".to_string();
    guide.category = "guide".to_string();
    insert_entry(conn, guide);

    // Memory surface: a research note. Owner-ratified as Memory -- category
    // is NOT wiki/guide and path is NOT under /wiki, so neither is_wiki_entry
    // nor the guide-category rule fires.
    let mut research_note = memory_entry(
        "surface-research-note",
        &format!(
            "{SHARED_PROBE_TERM} research note documenting an experiment on the recall subsystem"
        ),
        &[SHARED_PROBE_TERM],
    );
    research_note.path = "/notes/research/recall-experiment".to_string();
    research_note.category = "decision".to_string();
    insert_entry(conn, research_note);
}

fn surface_opts(surface: Option<Surface>) -> SearchOptions {
    SearchOptions {
        top_k: 10,
        candidates_per_channel: 10,
        record_access: false,
        surface,
        ..Default::default()
    }
}

#[test]
fn surface_memory_excludes_wiki_and_guide_but_includes_research_note() {
    let mut conn = setup();
    seed_surface_corpus(&mut conn);

    let opts = surface_opts(Some(Surface::Memory));
    let results = hybrid_search(&conn, SHARED_PROBE_TERM, &opts).unwrap();
    let ids: Vec<&str> = results.iter().map(|r| r.entry.id.as_str()).collect();

    assert!(
        ids.contains(&"surface-research-note"),
        "research note (category=decision, path=/notes/...) is owner-ratified \
         Memory and must survive Surface::Memory scoping; got {ids:?}"
    );
    assert!(
        !ids.contains(&"surface-wiki-entry"),
        "wiki entry must be excluded from Surface::Memory; got {ids:?}"
    );
    assert!(
        !ids.contains(&"surface-guide-entry"),
        "guide-category entry must be excluded from Surface::Memory; got {ids:?}"
    );
}

#[test]
fn surface_docs_returns_only_wiki_and_guide() {
    let mut conn = setup();
    seed_surface_corpus(&mut conn);

    let opts = surface_opts(Some(Surface::Docs));
    let results = hybrid_search(&conn, SHARED_PROBE_TERM, &opts).unwrap();
    let ids: Vec<&str> = results.iter().map(|r| r.entry.id.as_str()).collect();

    assert!(
        ids.contains(&"surface-wiki-entry"),
        "wiki entry must be included in Surface::Docs; got {ids:?}"
    );
    assert!(
        ids.contains(&"surface-guide-entry"),
        "guide-category entry must be included in Surface::Docs; got {ids:?}"
    );
    assert!(
        !ids.contains(&"surface-research-note"),
        "research note must NOT appear under Surface::Docs; got {ids:?}"
    );
}

/// `surface: None` (the default) is the compatibility guarantee: it must
/// apply NO surface predicate at all, i.e. return the exact union of what
/// `Some(Memory)` and `Some(Docs)` each return -- not a subset, not a
/// reordering. Since every row classifies as exactly one of the two
/// surfaces (`surface_of` is a total, disjoint partition), that union IS
/// "the fused pool", so this is a direct behavior-preservation check
/// without needing a pre-change binary to diff against.
#[test]
fn surface_none_reproduces_the_fused_union_of_memory_and_docs() {
    let mut conn = setup();
    seed_surface_corpus(&mut conn);

    let fused = hybrid_search(&conn, SHARED_PROBE_TERM, &surface_opts(None)).unwrap();
    let memory_only = hybrid_search(
        &conn,
        SHARED_PROBE_TERM,
        &surface_opts(Some(Surface::Memory)),
    )
    .unwrap();
    let docs_only =
        hybrid_search(&conn, SHARED_PROBE_TERM, &surface_opts(Some(Surface::Docs))).unwrap();

    let fused_ids: HashSet<&str> = fused.iter().map(|r| r.entry.id.as_str()).collect();
    let mut union_ids: HashSet<&str> = memory_only.iter().map(|r| r.entry.id.as_str()).collect();
    union_ids.extend(docs_only.iter().map(|r| r.entry.id.as_str()));

    assert_eq!(
        fused_ids, union_ids,
        "surface=None must return exactly the disjoint union of Memory ∪ Docs \
         (today's fused pool), no more and no less"
    );
    // All three seeded rows must be present and none silently dropped by the
    // (absent) surface predicate.
    assert_eq!(
        fused_ids.len(),
        3,
        "expected all 3 seeded rows in the fused pool"
    );
}

/// #1413 concern 2: graph expansion (hops=1) must not leak cross-surface
/// neighbors into a surface-scoped result set. A Docs entry and a Memory
/// entry share the probe term and are linked by an edge; under each surface
/// scope the in-surface entry is the only ranked seed (top_k=1), so the only
/// way the out-of-surface neighbor can appear is through graph expansion. The
/// `surface=None` control proves the edge is wired and expansion runs (both
/// nodes appear), making the scoped assertions a real red→green
/// discrimination: pre-fix the cross-surface neighbor leaks; post-fix the
/// canonical `surface_of` classifier rejects it.
#[test]
fn graph_expansion_does_not_leak_cross_surface_neighbors() {
    let mut conn = setup();

    // Docs node: wiki entry (is_wiki_entry via path + category).
    let mut docs_node = memory_entry(
        "graph-iso-docs",
        &format!("{SHARED_PROBE_TERM} wiki reference page about graph surface isolation"),
        &[SHARED_PROBE_TERM],
    );
    docs_node.path = "/wiki/graph-surface-isolation".to_string();
    docs_node.category = "wiki".to_string();
    insert_entry(&mut conn, docs_node);

    // Memory node: research note (not wiki, not guide).
    let mut mem_node = memory_entry(
        "graph-iso-mem",
        &format!("{SHARED_PROBE_TERM} research note about graph surface isolation"),
        &[SHARED_PROBE_TERM],
    );
    mem_node.path = "/notes/research/graph-surface-isolation".to_string();
    mem_node.category = "decision".to_string();
    insert_entry(&mut conn, mem_node);

    // Undirected edge linking the two surfaces (graph_expand traverses both
    // endpoints regardless of direction).
    add_edge(
        &conn,
        &MemoryEdge {
            source_id: "graph-iso-docs".to_string(),
            target_id: "graph-iso-mem".to_string(),
            relation: "similar_to".to_string(),
            weight: 1.0,
            metadata: json!({}),
            created_at: String::new(),
            valid_from: String::new(),
            valid_to: None,
        },
    )
    .unwrap();

    let opts = |surface: Option<Surface>| SearchOptions {
        top_k: 2,
        candidates_per_channel: 10,
        record_access: false,
        graph_expand_hops: 1,
        surface,
        ..Default::default()
    };

    // Control (surface=None): no surface predicate, so the cross-surface
    // neighbor IS expanded — both nodes must appear (one seed, one neighbor).
    let none_results = hybrid_search(&conn, SHARED_PROBE_TERM, &opts(None)).unwrap();
    let none_ids: HashSet<&str> = none_results.iter().map(|r| r.entry.id.as_str()).collect();
    assert!(
        none_ids.contains("graph-iso-docs") && none_ids.contains("graph-iso-mem"),
        "control (surface=None): cross-surface neighbor must be expanded when no \
         surface predicate is set; got {none_ids:?}"
    );

    // Docs scope: only the wiki node ranks as seed; the Memory neighbor reached
    // by the edge must NOT leak into the Docs-scoped results.
    let docs_results = hybrid_search(&conn, SHARED_PROBE_TERM, &opts(Some(Surface::Docs))).unwrap();
    let docs_ids: Vec<&str> = docs_results.iter().map(|r| r.entry.id.as_str()).collect();
    assert!(
        docs_ids.contains(&"graph-iso-docs"),
        "Docs seed must rank under Surface::Docs; got {docs_ids:?}"
    );
    assert!(
        !docs_ids.contains(&"graph-iso-mem"),
        "Surface::Docs must not leak the Memory neighbor via graph expansion; got {docs_ids:?}"
    );

    // Memory scope: only the research note ranks as seed; the Docs neighbor
    // reached by the edge must NOT leak into the Memory-scoped results.
    let mem_results =
        hybrid_search(&conn, SHARED_PROBE_TERM, &opts(Some(Surface::Memory))).unwrap();
    let mem_ids: Vec<&str> = mem_results.iter().map(|r| r.entry.id.as_str()).collect();
    assert!(
        mem_ids.contains(&"graph-iso-mem"),
        "Memory seed must rank under Surface::Memory; got {mem_ids:?}"
    );
    assert!(
        !mem_ids.contains(&"graph-iso-docs"),
        "Surface::Memory must not leak the Docs neighbor via graph expansion; got {mem_ids:?}"
    );
}
