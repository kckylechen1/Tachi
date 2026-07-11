use super::*;

/// Regression for tachi#773: a single shared entity, with no vector support,
/// must NOT produce a `related_to` auto-link edge (the "fog floor").
///
/// Seeds a candidate that shares exactly one entity with the saved memory
/// (so the entity search WILL find it as a candidate — proving any absence
/// of an edge is the fog-floor gate firing, not a missed candidate), with
/// `vector: None` on both sides so `should_reinforce`/vector-assisted
/// `related_to` cannot fire either. After a bounded wait, no edge to the
/// seeded entry may exist.
#[tokio::test]
async fn save_memory_single_shared_entity_no_vector_suppresses_related_to() {
    let server = make_server();

    let shared_entity = format!("fogent-{}", uuid::Uuid::new_v4());
    let seeded_id = format!("auto-link-fog-target-{}", uuid::Uuid::new_v4());
    let mut seeded = make_entry(&seeded_id);
    seeded.entities = vec![shared_entity.clone()];
    seeded.text = format!("Seeded note mentioning {shared_entity} alone");
    seeded.path = "/fog-seed".to_string();
    seeded.vector = None;
    server
        .with_global_store(|store| store.upsert(&seeded).map_err(|e| format!("seed: {e}")))
        .expect("seed entry");

    let save_response = server
        .save_memory(Parameters(SaveMemoryParams {
            text: format!("New observation also mentioning {shared_entity}"),
            summary: String::new(),
            path: "/fog-new".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![shared_entity.clone()],
            location: String::new(),
            scope: "general".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
        }))
        .await
        .expect("save_memory should succeed");
    let saved: serde_json::Value =
        serde_json::from_str(&save_response).expect("save response should be valid JSON");
    let saved_id = saved["id"]
        .as_str()
        .expect("save response should include id")
        .to_string();

    // Bounded wait for the auto-link background task to run to completion.
    // We cannot poll for "edge appears" (that's exactly what must NOT
    // happen), so instead give the spawned task ample time to finish, then
    // assert no edge landed.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let has_fog_edge = server
        .with_global_store_read(|store| {
            store
                .get_edges(&saved_id, "outgoing", None)
                .map(|edges| edges.iter().any(|edge| edge.target_id == seeded_id))
                .map_err(|e| format!("edges: {e}"))
        })
        .expect("read auto-link edges");

    assert!(
        !has_fog_edge,
        "single shared entity ({shared_entity}) with no vector support must NOT \
         produce a related_to auto-link edge (tachi#773 fog floor)"
    );
}

/// Regression for "auto-link bumps access stats" bug.
///
/// `save_memory` with `auto_link: true` spawns a background search keyed by
/// each entity to discover related memories. That search is a write-side side
/// effect, NOT a user read — it must use `SearchOptions { record_access: false,
/// .. }` so the matched-but-not-actually-read entries do not get their
/// `access_count` / `last_access` bumped (which would inflate ACT-R frequency,
/// suppress the `access_count = 0` GC prune path, and bias promotion ranking).

#[tokio::test]
async fn save_memory_auto_link_does_not_bump_target_access_count() {
    let server = make_server();

    // Seed an entry tagged with the entity we will later search via auto-link.
    let seeded_id = format!("auto-link-target-{}", uuid::Uuid::new_v4());
    let mut seeded = make_entry(&seeded_id);
    // Two shared entities so the #773 related_to floor is met. The seed and
    // the save below use DIFFERENT path roots ("/alpha" vs "/beta") so that
    // `should_supersede`'s path_root-equality check fails and the edge
    // actually exercises the `related_to` path, not `supersedes` (same path
    // root + 2 shared entities + newer timestamp would otherwise route to
    // supersede).
    seeded.entities = vec!["sigil".to_string(), "tachi-server".to_string()];
    seeded.text = "Original notes about sigil tachi-server internals".to_string();
    seeded.path = "/alpha".to_string();
    server
        .with_global_store(|store| store.upsert(&seeded).map_err(|e| format!("seed: {e}")))
        .expect("seed entry");

    // Sanity: freshly-written entry has access_count = 0.
    let pre = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&seeded_id, false)
                .map_err(|e| format!("get: {e}"))
        })
        .expect("pre-read")
        .expect("seeded entry exists");
    assert_eq!(
        pre.access_count, 0,
        "fresh seed should have access_count=0, got {}",
        pre.access_count
    );
    assert!(
        pre.last_access.is_none(),
        "fresh seed should have no last_access"
    );

    // Trigger save_memory with auto_link=true and an entity that matches the
    // seeded entry. Auto-link will search global store for "sigil", which will
    // return the seeded entry as a candidate.
    let save_response = server
        .save_memory(Parameters(SaveMemoryParams {
            text: "New observation about sigil rotation".to_string(),
            summary: String::new(),
            path: "/beta".to_string(),
            importance: 0.7,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec!["sigil".to_string(), "tachi-server".to_string()],
            location: String::new(),
            scope: "general".to_string(),
            vector: None,
            id: None,
            force: true,
            auto_link: true,
            project: None,
            retention_policy: None,
            domain: None,
            timestamp: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            emit_continuity: false,
        }))
        .await
        .expect("save_memory should succeed");
    let saved: serde_json::Value =
        serde_json::from_str(&save_response).expect("save response should be valid JSON");
    let saved_id = saved["id"]
        .as_str()
        .expect("save response should include id")
        .to_string();

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    let related_to_edge: memcore::MemoryEdge = loop {
        let found = server
            .with_global_store_read(|store| {
                store
                    .get_edges(&saved_id, "outgoing", None)
                    .map(|edges| edges.into_iter().find(|edge| edge.target_id == seeded_id))
                    .map_err(|e| format!("edges: {e}"))
            })
            .expect("read auto-link edges");
        if let Some(edge) = found {
            break edge;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "auto-link did not persist an edge before timeout"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };

    // Different path roots (seed "/alpha" vs save "/beta") mean
    // should_supersede's path_root-equality check fails, so the 2
    // shared-entity edge must be `related_to`, not `supersedes`.
    assert_eq!(
        related_to_edge.relation, "related_to",
        "2 shared entities across different path roots must produce a related_to edge, got {:?}",
        related_to_edge.relation
    );

    // Re-read the seeded entry. Its access_count MUST still be 0 — auto-link
    // is a write-side side effect, not a user read.
    let post = server
        .with_global_store_read(|store| {
            store
                .get_with_options(&seeded_id, false)
                .map_err(|e| format!("get: {e}"))
        })
        .expect("post-read")
        .expect("seeded entry still exists");

    assert_eq!(
        post.access_count, 0,
        "auto-link search must NOT bump access_count of matched entries; got {}",
        post.access_count
    );
    assert!(
        post.last_access.is_none(),
        "auto-link search must NOT set last_access on matched entries; got {:?}",
        post.last_access
    );
}
