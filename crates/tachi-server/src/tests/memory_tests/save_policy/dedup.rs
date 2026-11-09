use super::*;
use crate::memory_search_ops::handle_save_memory;
use crate::tool_params::SaveMemoryParams;
use serde_json::Value;

#[tokio::test]
async fn save_memory_rejects_exact_path_text_duplicate_without_force() {
    let server = make_server();
    let path = format!("/scratch/tachi/dedup-test-{}", uuid::Uuid::new_v4());
    let text = "Deterministic dedup regression payload for save_memory guard.".to_string();

    let params = SaveMemoryParams {
        text: text.clone(),
        summary: String::new(),
        path: path.clone(),
        importance: 0.7,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: None,
        force: false,
        auto_link: false,
        project: None,
        project_explicit: false,
        retention_policy: None,
        domain: Some("scratch".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
        supersedes: Vec::new(),
    };

    let first = handle_save_memory(&server, params.clone())
        .await
        .expect("first save");
    let first_json: Value = serde_json::from_str(&first).expect("first json");
    assert!(
        first_json["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("saved")),
        "{first_json:#}"
    );

    let second = handle_save_memory(&server, params)
        .await
        .expect("second save");
    let second_json: Value = serde_json::from_str(&second).expect("second json");
    assert_eq!(second_json["status"], "duplicate");
    assert_eq!(second_json["saved"], false);
    assert_eq!(second_json["id"], first_json["id"]);
}

#[tokio::test]
async fn save_memory_strips_internal_rem_metadata_but_keeps_ordinary_metadata() {
    let server = make_server();
    let id = format!("public-rem-metadata-{}", uuid::Uuid::new_v4());
    let mut params = SaveMemoryParams {
        text: "Public memory metadata cannot reset internal REM processing state.".to_string(),
        summary: String::new(),
        path: format!("/scratch/tachi/{id}"),
        importance: 0.7,
        category: "fact".to_string(),
        topic: "rem-metadata-boundary".to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: Some(id.clone()),
        force: true,
        auto_link: false,
        project: None,
        project_explicit: false,
        retention_policy: None,
        domain: Some("scratch".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: Some(serde_json::json!({
            "caller_marker": "preserved",
            "rem": {"processed": 0, "processed_by": "forged"}
        })),
        emit_continuity: false,
        supersedes: Vec::new(),
    };

    handle_save_memory(&server, params.clone())
        .await
        .expect("save public metadata");
    let initial = server
        .with_global_store_read(|store| store.get(&id).map_err(|error| error.to_string()))
        .expect("read saved memory")
        .expect("saved memory exists");
    assert_eq!(initial.metadata["caller_marker"], "preserved");
    assert!(initial.metadata.get("rem").is_none());

    server
        .with_global_store(|store| {
            store
                .mark_rem_processed_for_draft_at_revisions(
                    &[(id.clone(), initial.revision)],
                    "2026-07-31T01:00:00Z",
                    "wiki-rem:real-operation",
                )
                .map_err(|error| error.to_string())
        })
        .expect("seed trusted REM marker");

    params.text = "A public update must preserve the trusted REM marker.".to_string();
    params.metadata = Some(serde_json::json!({
        "caller_marker": "updated",
        "rem": {"processed": 0, "processed_by": "forged"}
    }));
    handle_save_memory(&server, params)
        .await
        .expect("update public metadata");
    let updated = server
        .with_global_store_read(|store| store.get(&id).map_err(|error| error.to_string()))
        .expect("read updated memory")
        .expect("updated memory exists");
    assert_eq!(updated.metadata["caller_marker"], "updated");
    assert_eq!(updated.metadata["rem"]["processed"], 1);
    assert_eq!(
        updated.metadata["rem"]["processed_by"],
        "wiki-rem:real-operation"
    );
    assert_eq!(
        updated.metadata["rem"]["processed_revision"],
        initial.revision
    );
}

#[tokio::test]
async fn save_memory_allows_a_second_row_when_caller_supplies_its_own_id() {
    // An explicit `id=` (not `force`) is what makes a second row with the
    // same path+text land as a distinct save: `find_exact_path_text_duplicate`
    // is only ever consulted when the caller omits `id` (see #1041 S3 below
    // for why `force` alone must NOT also skip it).
    let server = make_server();
    let path = format!("/scratch/tachi/dedup-force-{}", uuid::Uuid::new_v4());
    let text = "An explicit id, not force, is what bypasses the dedup guard.".to_string();

    let mut base = SaveMemoryParams {
        text: text.clone(),
        summary: String::new(),
        path: path.clone(),
        importance: 0.7,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: None,
        force: false,
        auto_link: false,
        project: None,
        project_explicit: false,
        retention_policy: None,
        domain: Some("scratch".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
        supersedes: Vec::new(),
    };

    handle_save_memory(&server, base.clone())
        .await
        .expect("seed save");

    base.force = true;
    base.id = Some(uuid::Uuid::new_v4().to_string());
    let forced = handle_save_memory(&server, base)
        .await
        .expect("forced save");
    let forced_json: Value = serde_json::from_str(&forced).expect("forced json");
    assert!(
        forced_json["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("saved")),
        "{forced_json:#}"
    );
}

// ── #1041 S3: sync-pipe write-side idempotency ──────────────────────────────
//
// Root cause: a periodic writer (real-time position sync / post-market
// summary) commonly passes `force=true` (to clear the noise filter on short
// factual content) with no client-supplied `id`. The dedup guard used to be
// gated on `!params.force && params.id.is_none()`, so `force=true` silently
// disabled the exact path+text duplicate check too — every retry minted a
// fresh random id, which is how one fact ends up as 121 duplicate rows. The
// fix decouples `force` (content-quality bypass) from dedup (identity
// check): dedup now fires for every id-less save regardless of `force`.

#[tokio::test]
async fn sync_save_with_force_and_no_id_dedupes_same_path_and_text() {
    let server = make_server();
    let path = format!("/trading/sync/{}", uuid::Uuid::new_v4());
    let text = "300502.SZ synced shares=100".to_string();

    let params = SaveMemoryParams {
        text: text.clone(),
        summary: String::new(),
        path: path.clone(),
        importance: 0.7,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: None,
        force: true,
        auto_link: false,
        project: None,
        project_explicit: false,
        retention_policy: None,
        domain: Some("scratch".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
        supersedes: Vec::new(),
    };

    let first = handle_save_memory(&server, params.clone())
        .await
        .expect("first sync save");
    let first_json: Value = serde_json::from_str(&first).expect("first json");
    assert!(
        first_json["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("saved")),
        "{first_json:#}"
    );

    // Same day, same content, retried (e.g. the next periodic tick) — must
    // collapse onto the same row, not mint a new UUID.
    let second = handle_save_memory(&server, params.clone())
        .await
        .expect("second sync save");
    let second_json: Value = serde_json::from_str(&second).expect("second json");
    assert_eq!(second_json["status"], "duplicate", "{second_json:#}");
    assert_eq!(second_json["saved"], false);
    assert_eq!(second_json["id"], first_json["id"]);

    // A third retry, same everything — still one row.
    let third = handle_save_memory(&server, params)
        .await
        .expect("third sync save");
    let third_json: Value = serde_json::from_str(&third).expect("third json");
    assert_eq!(third_json["status"], "duplicate", "{third_json:#}");
    assert_eq!(third_json["id"], first_json["id"]);
}

#[tokio::test]
async fn sync_save_with_force_and_no_id_on_a_new_path_is_a_distinct_row() {
    // A new calendar day's sync bucket typically means a new path (the
    // caller's own date-bucketing) — that must still land as its own row,
    // not get swallowed by the dedup guard.
    let server = make_server();
    let text = "300502.SZ synced shares=100".to_string();

    let day_one = SaveMemoryParams {
        text: text.clone(),
        summary: String::new(),
        path: "/trading/sync/2026-07-13/300502".to_string(),
        importance: 0.7,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: None,
        force: true,
        auto_link: false,
        project: None,
        project_explicit: false,
        retention_policy: None,
        domain: Some("scratch".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
        supersedes: Vec::new(),
    };
    let mut day_two = day_one.clone();
    day_two.path = "/trading/sync/2026-07-14/300502".to_string();

    let first = handle_save_memory(&server, day_one)
        .await
        .expect("day one sync save");
    let first_json: Value = serde_json::from_str(&first).expect("first json");
    assert!(first_json["status"]
        .as_str()
        .is_some_and(|status| status.starts_with("saved")));

    let second = handle_save_memory(&server, day_two)
        .await
        .expect("day two sync save");
    let second_json: Value = serde_json::from_str(&second).expect("second json");
    assert!(
        second_json["status"]
            .as_str()
            .is_some_and(|status| status.starts_with("saved")),
        "{second_json:#}"
    );
    assert_ne!(second_json["id"], first_json["id"]);
}

fn near_dup_params(path: &str, text: &str) -> SaveMemoryParams {
    SaveMemoryParams {
        text: text.to_string(),
        summary: String::new(),
        path: path.to_string(),
        importance: 0.7,
        category: "fact".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: "global".to_string(),
        vector: None,
        id: None,
        force: false,
        auto_link: false,
        project: None,
        project_explicit: false,
        retention_policy: None,
        domain: Some("scratch".to_string()),
        timestamp: None,
        valid_from: None,
        valid_until: None,
        metadata: None,
        emit_continuity: false,
        supersedes: Vec::new(),
    }
}

/// Save one text at two different paths on a fresh server; returns both receipts.
async fn save_same_text_at_two_paths(retention_policy: Option<&str>) -> (Value, Value) {
    let server = make_server();
    let text = "Cross path near duplicate receipt payload alpha bravo charlie delta echo \
                foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo";

    let mut first_params = near_dup_params("/scratch/tachi/near-dup-a", text);
    first_params.retention_policy = retention_policy.map(str::to_string);
    let first: Value = serde_json::from_str(
        &handle_save_memory(&server, first_params)
            .await
            .expect("first save"),
    )
    .expect("first json");
    assert_eq!(first["status"], "saved", "{first:#}");
    assert!(first.get("near_duplicate_of").is_none(), "{first:#}");

    // Back-dated validity so the merge's "valid_from must precede write time"
    // guard cannot race the wall clock.
    let mut second_params = near_dup_params("/scratch/tachi/near-dup-b", text);
    second_params.retention_policy = retention_policy.map(str::to_string);
    second_params.valid_from = Some("2026-01-01T00:00:00Z".to_string());
    let second: Value = serde_json::from_str(
        &handle_save_memory(&server, second_params)
            .await
            .expect("second save"),
    )
    .expect("second json");
    assert_ne!(second["id"], first["id"]);
    (first, second)
}

/// tachi#2241: identical text saved at a different path. The write-time
/// near-duplicate policy already knows the earlier row; the receipt must say so
/// (`near_duplicate_of`) instead of a bare `saved`, and say whether it folded
/// the new row in (`near_duplicate_merged`). Default `fact` saves are
/// lifecycle-protected (`durable`), so both rows stay active and `merged` is
/// false; an unprotected (`ephemeral`) save is folded and `merged` is true.
#[tokio::test]
async fn save_memory_reports_near_duplicate_of_for_identical_text_at_different_path() {
    let (first, second) = save_same_text_at_two_paths(None).await;
    assert_eq!(second["status"], "saved", "{second:#}");
    assert_eq!(second["near_duplicate_of"], first["id"], "{second:#}");
    assert_eq!(second["near_duplicate_merged"], false, "{second:#}");

    let (first, second) = save_same_text_at_two_paths(Some("ephemeral")).await;
    assert_eq!(second["near_duplicate_of"], first["id"], "{second:#}");
    assert_eq!(second["near_duplicate_merged"], true, "{second:#}");
}

/// tachi#2241: `enrichment.metadata_pending` is how a receipt says whether the
/// LLM keyword/entity extract lane still owes this row work. Supplying BOTH
/// keywords and entities skips it; supplying only one does not.
#[tokio::test]
async fn save_memory_receipt_says_extract_lane_skipped_only_when_keywords_and_entities_given() {
    let server = make_server();
    let unique = uuid::Uuid::new_v4();
    let mut both = near_dup_params(
        &format!("/scratch/tachi/extract-both-{unique}"),
        &format!("Extract lane skip receipt both supplied {unique}"),
    );
    both.keywords = vec!["receipt".to_string()];
    both.entities = vec!["tachi".to_string()];
    let mut entities_only = near_dup_params(
        &format!("/scratch/tachi/extract-entities-{unique}"),
        &format!("Extract lane skip receipt entities only {unique} zulu yankee xray"),
    );
    entities_only.entities = vec!["tachi".to_string()];

    let both: Value = serde_json::from_str(
        &handle_save_memory(&server, both)
            .await
            .expect("save with keywords+entities"),
    )
    .expect("json");
    let entities_only: Value = serde_json::from_str(
        &handle_save_memory(&server, entities_only)
            .await
            .expect("save with entities only"),
    )
    .expect("json");
    assert_eq!(both["enrichment"]["metadata_pending"], false, "{both:#}");
    assert_eq!(
        entities_only["enrichment"]["metadata_pending"], true,
        "{entities_only:#}"
    );
}
