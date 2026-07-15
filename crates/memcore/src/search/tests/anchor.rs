//! tachi#773 item 4 kill-test (sol v3 correction 1): anchor rows must never
//! surface as ordinary recall candidates, at any of the three candidate
//! channels (vector / FTS / symbolic), even when their text/summary
//! literally matches the query.

use super::*;
use crate::db::{ensure_anchor, AnchorKind};
use crate::search::candidates::collect_candidates;

#[test]
fn collect_candidates_never_surfaces_an_anchor_even_on_exact_text_match() {
    let mut conn = setup();
    // A real memory whose text overlaps what we'll query for, so the query
    // has a legitimate non-anchor hit to find (proves the query isn't simply
    // returning nothing).
    insert(
        &mut conn,
        "real-773-note",
        "tachi issue 773 memorygraph S1 ontology write validation",
        &["773"],
    );

    // The anchor's summary/id both textually match "773" / "issue" / "tachi"
    // — if anchor exclusion were missing or only applied at ranking time
    // (not before each channel's LIMIT, per sol's kill-test), this row would
    // leak into vec_scores / fts_scores / symbolic candidates.
    let anchor_id = ensure_anchor(&conn, AnchorKind::Issue, "kckylechen1/tachi:773").unwrap();
    assert_eq!(anchor_id, "anchor:issue:kckylechen1/tachi:773");

    let opts = SearchOptions {
        top_k: 10,
        candidates_per_channel: 50,
        record_access: false,
        ..Default::default()
    };
    let as_of_utc: Option<String> = None;
    let (candidates, _) = collect_candidates(
        &conn,
        "issue 773 tachi",
        &opts,
        false,
        as_of_utc.as_deref(),
        false,
    )
    .unwrap();

    assert!(
        !candidates.candidate_ids.contains(&anchor_id),
        "anchor id must never appear in collect_candidates' candidate_ids, got {:?}",
        candidates.candidate_ids
    );
    assert!(
        !candidates.fts_scores.contains_key(&anchor_id),
        "anchor id must never appear in fts_scores"
    );
    assert!(
        !candidates.vec_scores.contains_key(&anchor_id),
        "anchor id must never appear in vec_scores"
    );
    assert!(
        candidates
            .candidate_ids
            .contains(&"real-773-note".to_string()),
        "the real non-anchor memory must still be a candidate (query has a genuine hit)"
    );
}

#[test]
fn hybrid_search_end_to_end_never_returns_an_anchor() {
    let mut conn = setup();
    insert(
        &mut conn,
        "real-anchor-e2e-note",
        "anchor kind seat wizard dispatch note",
        &["anchor"],
    );
    let anchor_id = ensure_anchor(&conn, AnchorKind::Seat, "wizard").unwrap();

    let opts = SearchOptions {
        top_k: 10,
        record_access: false,
        ..Default::default()
    };
    let results = hybrid_search(&conn, "anchor seat wizard dispatch", &opts).unwrap();

    assert!(
        results.iter().all(|r| r.entry.id != anchor_id),
        "hybrid_search must never surface an anchor row, got ids {:?}",
        results.iter().map(|r| &r.entry.id).collect::<Vec<_>>()
    );
}
