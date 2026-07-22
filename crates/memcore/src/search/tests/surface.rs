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
