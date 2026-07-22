//! memcore ranking rework Phase 2 PIECE 1: SQL-vs-Rust surface classifier
//! agreement.
//!
//! `DOCS_SURFACE_SQL_WHERE` (namespace.rs) is a hand-mirrored SQL predicate
//! of the Rust `surface_of` classifier, following the same pattern as
//! `RECALL_CACHE_SQL_WHERE` vs `is_recall_cache_entry`. This test asserts
//! the two never disagree over a mixed corpus (memory + wiki + guide +
//! research-note rows) so the SQL and Rust sides cannot silently drift.
//!
//! Checks BOTH `surface_sql_clause` directions (`Some(Docs)` and
//! `Some(Memory)`), not just a raw `AND (docs_where)` probe: a NULL-poisoned
//! `docs_where` (see `surface_sql_clause`'s doc comment on the `COALESCE`
//! guard) happens to still evaluate "excluded" for the Docs direction, so a
//! Docs-only probe cannot catch a regression that only breaks the Memory
//! direction (`AND NOT (docs_where)`) -- which is exactly the bug this
//! module was extended to catch (a research note with a NULL `domain`
//! column was wrongly excluded from `Surface::Memory`).
//!
//! The corpus also covers every `metadata.wiki` JSON-type variant (boolean
//! `true`/`false`, numeric `1`, string `"1"`) -- `is_wiki_entry`'s Rust side
//! accepts only the JSON boolean `true`, so the SQL mirror must use
//! `json_type(...) = 'true'`, not a numeric `= 1` equality (see
//! `DOCS_SURFACE_SQL_WHERE`'s doc comment); the numeric-`1` row is the exact
//! shape of a real Rust/SQL disagreement this corpus caught.

use super::*;
use crate::namespace::{surface_of, surface_sql_clause, Surface};

/// Whether `id`'s row is selected by the production `surface_sql_clause` for
/// `surface` (unqualified / bare-`memories` form, matching the table-scan
/// symbolic channel and this test's plain `SELECT ... FROM memories`).
fn matches_surface_scope(conn: &Connection, id: &str, surface: Surface) -> bool {
    let clause = surface_sql_clause(Some(surface), false);
    let count: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM memories WHERE id = ?1 {clause}"),
            params![id],
            |row| row.get(0),
        )
        .unwrap();
    count > 0
}

fn other_surface(surface: Surface) -> Surface {
    match surface {
        Surface::Docs => Surface::Memory,
        Surface::Memory => Surface::Docs,
    }
}

#[test]
fn sql_predicate_agrees_with_rust_classifier_over_mixed_corpus() {
    let mut conn = make_conn();

    let mut wiki = make_entry("surface-agree-wiki", "wiki reference row");
    wiki.path = "/wiki/agreement-check".into();
    wiki.category = "wiki".into();
    upsert(&mut conn, &wiki, false).unwrap();

    let mut guide = make_entry("surface-agree-guide", "guide walkthrough row");
    guide.path = "/guide/agreement-check".into();
    guide.category = "guide".into();
    upsert(&mut conn, &guide, false).unwrap();

    let mut wiki_by_source = make_entry("surface-agree-wiki-source", "wiki row via source field");
    wiki_by_source.path = "/some/other/path".into();
    wiki_by_source.source = "wiki".into();
    upsert(&mut conn, &wiki_by_source, false).unwrap();

    let mut wiki_by_domain = make_entry("surface-agree-wiki-domain", "wiki row via domain field");
    wiki_by_domain.path = "/domain-pack/thing".into();
    wiki_by_domain.domain = Some("wiki".into());
    upsert(&mut conn, &wiki_by_domain, false).unwrap();

    let mut wiki_by_metadata = make_entry("surface-agree-wiki-meta", "wiki row via metadata flag");
    wiki_by_metadata.path = "/metadata-flagged/thing".into();
    wiki_by_metadata.metadata = json!({ "wiki": true });
    upsert(&mut conn, &wiki_by_metadata, false).unwrap();

    // Regression rows for the Rust-vs-SQL `metadata.wiki` type divergence:
    // `is_wiki_entry`'s Rust classifier (`metadata_bool`, via `Value::as_bool`)
    // accepts ONLY the JSON boolean `true`. `json_type(metadata, '$.wiki') =
    // 'true'` mirrors that exactly (matches the value's JSON *type*, not a
    // numeric equality) -- unlike the old `COALESCE(json_extract(...), 0) =
    // 1`, which collapsed JSON `true` AND the numeric `1` to the same SQL
    // integer `1` and so wrongly classified `{"wiki":1}` as Docs. All three
    // of these must classify as `Memory` on BOTH sides.
    let mut wiki_meta_numeric_one = make_entry(
        "surface-agree-wiki-meta-numeric",
        "row with numeric metadata.wiki=1",
    );
    wiki_meta_numeric_one.path = "/metadata-numeric/thing".into();
    wiki_meta_numeric_one.metadata = json!({ "wiki": 1 });
    upsert(&mut conn, &wiki_meta_numeric_one, false).unwrap();

    let mut wiki_meta_string_one = make_entry(
        "surface-agree-wiki-meta-string",
        "row with string metadata.wiki=\"1\"",
    );
    wiki_meta_string_one.path = "/metadata-string/thing".into();
    wiki_meta_string_one.metadata = json!({ "wiki": "1" });
    upsert(&mut conn, &wiki_meta_string_one, false).unwrap();

    let mut wiki_meta_false = make_entry(
        "surface-agree-wiki-meta-false",
        "row with boolean metadata.wiki=false",
    );
    wiki_meta_false.path = "/metadata-false/thing".into();
    wiki_meta_false.metadata = json!({ "wiki": false });
    upsert(&mut conn, &wiki_meta_false, false).unwrap();

    let mut research_note = make_entry("surface-agree-research", "research note row");
    research_note.path = "/notes/research/agreement-check".into();
    research_note.category = "decision".into();
    upsert(&mut conn, &research_note, false).unwrap();

    let mut plain_fact = make_entry("surface-agree-fact", "plain fact row");
    plain_fact.path = "/scratch/plain".into();
    plain_fact.category = "fact".into();
    upsert(&mut conn, &plain_fact, false).unwrap();

    // Regression corpus row for the 3VL NULL trap (surface_sql_clause):
    // `domain` is the only nullable column `DOCS_SURFACE_SQL_WHERE` reads
    // (`source`/`category` are `NOT NULL` with CHECK constraints in the
    // schema, so they can never actually be SQL NULL -- `domain TEXT` has
    // no such constraint). `make_entry` already leaves `domain: None`
    // (real SQL NULL) by default; this row exists to make that NULL-domain
    // condition an explicit, named, non-incidental part of the corpus, on
    // a row that must classify as `Memory` -- exactly the row shape
    // (decision-category, non-wiki path, NULL domain) that a legacy or
    // plain-write memory/research-note row has.
    let mut plain_memory_null_domain = make_entry(
        "surface-agree-null-domain",
        "plain decision row with null domain/source",
    );
    plain_memory_null_domain.path = "/notes/decisions/agreement-check".into();
    plain_memory_null_domain.category = "decision".into();
    assert!(
        plain_memory_null_domain.domain.is_none(),
        "test corpus row must carry a real NULL domain column to exercise the 3VL guard"
    );
    upsert(&mut conn, &plain_memory_null_domain, false).unwrap();

    let cases: &[(&str, Surface)] = &[
        ("surface-agree-wiki", Surface::Docs),
        ("surface-agree-guide", Surface::Docs),
        ("surface-agree-wiki-source", Surface::Docs),
        ("surface-agree-wiki-domain", Surface::Docs),
        ("surface-agree-wiki-meta", Surface::Docs),
        ("surface-agree-wiki-meta-numeric", Surface::Memory),
        ("surface-agree-wiki-meta-string", Surface::Memory),
        ("surface-agree-wiki-meta-false", Surface::Memory),
        ("surface-agree-research", Surface::Memory),
        ("surface-agree-fact", Surface::Memory),
        ("surface-agree-null-domain", Surface::Memory),
    ];

    for (id, expected) in cases {
        let mut entries = fetch_by_ids(&conn, &[id.to_string()], false).unwrap();
        let entry = entries
            .remove(*id)
            .unwrap_or_else(|| panic!("seeded row {id} must be readable back"));

        let rust_surface = surface_of(&entry);
        assert_eq!(
            rust_surface, *expected,
            "surface_of({id}) classified {rust_surface:?}, expected {expected:?}"
        );

        // The row must be selected under its OWN surface's SQL scope...
        assert!(
            matches_surface_scope(&conn, id, rust_surface),
            "row {id} classified {rust_surface:?} by Rust but was NOT selected by \
             surface_sql_clause(Some({rust_surface:?})) -- SQL/Rust disagreement"
        );
        // ...and must NOT be selected under the opposite surface's SQL scope.
        let opposite = other_surface(rust_surface);
        assert!(
            !matches_surface_scope(&conn, id, opposite),
            "row {id} classified {rust_surface:?} by Rust was ALSO selected by \
             surface_sql_clause(Some({opposite:?})) -- the two surfaces must partition, \
             never overlap"
        );
    }
}
