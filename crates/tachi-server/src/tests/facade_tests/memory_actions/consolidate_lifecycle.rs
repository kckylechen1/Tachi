//! Discrimination tests for consolidate propose → review → apply (#775).
//!
//! Pre-fix: consolidate always returned dry_run and never mutated rows.
//! Post-fix: same-path scratch duplicates produce a supersede proposal that
//! archives the older row only after approve + confirm=true.

use super::*;
use chrono::{Duration, Utc};

fn seed_scratch(id: &str, path: &str, text: &str, days_ago: i64) -> memcore::MemoryEntry {
    let mut e = make_entry(id);
    e.path = path.to_string();
    e.category = "fact".to_string();
    e.summary = text.chars().take(60).collect();
    e.text = text.to_string();
    e.importance = 0.4;
    e.timestamp = (Utc::now() - Duration::days(days_ago)).to_rfc3339();
    e.tier = "raw".to_string();
    e.scope = "project".to_string();
    e
}

#[tokio::test]
async fn consolidate_propose_review_apply_supersedes_older_scratch_duplicate() {
    let server = make_server();
    // Two active rows on the same scratch path — older must yield to newer.
    let older = seed_scratch(
        "life-old-1",
        "/scratch/sigil/dup-topic",
        "Older draft of the same decision note about consolidate lifecycle",
        10,
    );
    let newer = seed_scratch(
        "life-new-1",
        "/scratch/sigil/dup-topic",
        "Newer draft of the same decision note about consolidate lifecycle",
        1,
    );
    server
        .with_global_store(|store| {
            store.upsert(&older).map_err(|e| e.to_string())?;
            store.upsert(&newer).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed scratch duplicates");

    // 1) Propose (dry_run)
    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose");
    let parsed: Value = serde_json::from_str(&body).expect("propose json");
    assert_eq!(parsed["status"], json!("dry_run"));
    assert!(
        parsed["generated_count"].as_u64().unwrap_or(0) >= 1,
        "expected at least one supersede proposal: {parsed}"
    );

    let proposals = parsed["generated"].as_array().cloned().unwrap_or_default();
    // Near-duplicate summaries → merge_into (not plain supersede).
    let merge = proposals
        .iter()
        .find(|p| {
            p.get("lifecycle_action").and_then(Value::as_str) == Some("merge_into")
                && p.get("source_id").and_then(Value::as_str) == Some("life-old-1")
                && p.get("target_id").and_then(Value::as_str) == Some("life-new-1")
        })
        .expect("merge_into older→newer proposal for near-dup summaries");
    let proposal_id = merge["proposal_id"]
        .as_str()
        .expect("proposal_id")
        .to_string();

    // Older still active before apply.
    let older_before = server
        .with_global_store_read(|store| {
            store
                .get("life-old-1")
                .map_err(|e| e.to_string())
                .map(|e| e.expect("older exists"))
        })
        .expect("read older");
    assert!(!older_before.archived);

    // 2) Review approve
    let mut review = tachi_memory_params("consolidate");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let review_body = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("review");
    let review_json: Value = serde_json::from_str(&review_body).expect("review json");
    assert_eq!(review_json["proposal"]["status"], json!("approved"));

    // 3) Apply without confirm must fail (discrimination)
    let mut apply_no = tachi_memory_params("consolidate");
    apply_no.format = Some("json".to_string());
    apply_no.proposal_id = Some(proposal_id.clone());
    apply_no.confirm = false;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply_no)
        .await
        .expect_err("apply without confirm must fail");
    assert!(
        err.contains("confirm=true"),
        "error should require confirm: {err}"
    );

    // 4) Apply with confirm
    let mut apply = tachi_memory_params("consolidate");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    let apply_body = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("apply");
    let apply_json: Value = serde_json::from_str(&apply_body).expect("apply json");
    assert_eq!(apply_json["status"], json!("completed"));
    assert_eq!(
        apply_json["apply_result"]["lifecycle_action"],
        json!("merge_into")
    );
    assert_eq!(apply_json["apply_result"]["archived"], json!(true));
    assert_eq!(apply_json["apply_result"]["target_id"], json!("life-new-1"));

    // Archived rows are hidden from default get; use include_archived.
    let older_after = server
        .with_global_store_read(|store| {
            store
                .get_with_options("life-old-1", true)
                .map_err(|e| e.to_string())
                .map(|e| e.expect("older still exists for provenance"))
        })
        .expect("read older after");
    assert!(
        older_after.archived,
        "older row must be archived after supersede apply"
    );

    let newer_after = server
        .with_global_store_read(|store| {
            store
                .get("life-new-1")
                .map_err(|e| e.to_string())
                .map(|e| e.expect("newer exists"))
        })
        .expect("read newer");
    assert!(!newer_after.archived, "survivor must stay active");

    // Default list under the path must not return the archived older row.
    let listed = server
        .with_global_store_read(|store| {
            store
                .list_by_path("/scratch/sigil/dup-topic", 20, false)
                .map_err(|e| e.to_string())
        })
        .expect("list path");
    assert!(
        listed.iter().all(|e| e.id != "life-old-1"),
        "archived older must be absent from default list: {:?}",
        listed.iter().map(|e| &e.id).collect::<Vec<_>>()
    );
    assert!(
        listed.iter().any(|e| e.id == "life-new-1"),
        "survivor must remain listed"
    );
}

#[tokio::test]
async fn consolidate_custom_path_prefix_proposes_supersede_outside_scratch() {
    // Discrimination: custom path_prefix must not be hard-locked to /scratch
    // (Gemini #904 review). Pre-fix silently dropped non-/scratch rows.
    let server = make_server();
    let older = seed_scratch(
        "life-review-old",
        "/code-review/sigil/lifecycle-note",
        "Older code-review note about consolidate custom prefix",
        12,
    );
    let newer = seed_scratch(
        "life-review-new",
        "/code-review/sigil/lifecycle-note",
        "Newer code-review note about consolidate custom prefix",
        2,
    );
    server
        .with_global_store(|store| {
            store.upsert(&older).map_err(|e| e.to_string())?;
            store.upsert(&newer).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed code-review duplicates");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/code-review".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose custom prefix");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    let proposals = parsed["generated"].as_array().cloned().unwrap_or_default();
    let lifecycle = proposals.iter().find(|p| {
        matches!(
            p.get("lifecycle_action").and_then(Value::as_str),
            Some("supersede" | "merge_into")
        ) && p.get("source_id").and_then(Value::as_str) == Some("life-review-old")
            && p.get("target_id").and_then(Value::as_str) == Some("life-review-new")
    });
    assert!(
        lifecycle.is_some(),
        "custom path_prefix=/code-review must yield supersede/merge_into for same-path dups: {parsed}"
    );
}

#[tokio::test]
async fn consolidate_promote_distilled_after_diverse_recall() {
    let server = make_server();
    let mut entry = seed_scratch(
        "life-promote-1",
        "/scratch/sigil/promote-me",
        "Raw scratch that earned diverse recall about promote distilled lifecycle",
        5,
    );
    entry.recall_count = 3;
    entry.query_diversity = 3;
    entry.tier = "raw".to_string();
    entry.importance = 0.7;
    server
        .with_global_store(|store| store.upsert(&entry).map_err(|e| e.to_string()))
        .expect("seed promote candidate");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    let proposals = parsed["generated"].as_array().cloned().unwrap_or_default();
    let promote = proposals
        .iter()
        .find(|p| {
            p.get("lifecycle_action").and_then(Value::as_str) == Some("promote_distilled")
                && p.get("source_id").and_then(Value::as_str) == Some("life-promote-1")
        })
        .expect("promote_distilled proposal");
    let proposal_id = promote["proposal_id"].as_str().unwrap().to_string();

    let mut review = tachi_memory_params("consolidate");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("review");

    let mut apply = tachi_memory_params("consolidate");
    apply.format = Some("json".to_string());
    apply.proposal_id = Some(proposal_id);
    apply.confirm = true;
    let apply_body = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect("apply");
    let apply_json: Value = serde_json::from_str(&apply_body).expect("json");
    assert_eq!(
        apply_json["apply_result"]["lifecycle_action"],
        json!("promote_distilled")
    );
    assert_eq!(
        apply_json["apply_result"]["tier_after"],
        json!("consolidated")
    );

    let after = server
        .with_global_store_read(|store| {
            store
                .get("life-promote-1")
                .map_err(|e| e.to_string())
                .map(|e| e.expect("exists"))
        })
        .expect("read");
    assert_eq!(after.tier, "consolidated");
}

#[tokio::test]
async fn consolidate_refuses_to_propose_archive_for_protected_wiki() {
    let server = make_server();
    let mut wiki = make_entry("life-wiki-1");
    wiki.path = "/wiki/general/important".to_string();
    wiki.category = "wiki".to_string();
    wiki.importance = 0.2;
    wiki.access_count = 0;
    wiki.timestamp = (Utc::now() - Duration::days(400)).to_rfc3339();
    wiki.summary = "Old wiki page".into();
    wiki.text = "Old wiki page body".into();
    server
        .with_global_store(|store| store.upsert(&wiki).map_err(|e| e.to_string()))
        .expect("seed wiki");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/wiki".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    let generated = parsed["generated"].as_array().cloned().unwrap_or_default();
    let hits_wiki = generated
        .iter()
        .any(|p| p.get("source_id").and_then(Value::as_str) == Some("life-wiki-1"));
    assert!(
        !hits_wiki,
        "wiki rows must not appear as archive/supersede sources: {parsed}"
    );
}

/// #1043 D3 terminal-review judgement test: the automated (non-human-review)
/// direct-call entry point used by the distill pre-pass is
/// `merge_into_for_project`, which has no `action` parameter at all — it can
/// only ever perform a `merge_into`. This asserts the *behavior* that
/// guarantee produces (the compiler already guarantees the shape): calling
/// it always returns `lifecycle_action: "merge_into"` and applies exactly
/// that mutation (source archived+superseded into target), never
/// `supersede`/`archive`/`promote_distilled`.
#[tokio::test]
async fn merge_into_for_project_cannot_reach_other_lifecycle_actions() {
    let server = make_server();
    let mut source = make_entry("wrapper-source-1");
    source.keywords = vec!["source-kw".to_string()];
    let mut target = make_entry("wrapper-target-1");
    target.keywords = vec!["target-kw".to_string()];
    server
        .with_global_store(|store| {
            store.upsert(&source).map_err(|e| e.to_string())?;
            store.upsert(&target).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed wrapper source/target");

    let result = crate::facade_memory_ops::consolidate_ops::merge_into_for_project(
        &server,
        None,
        "wrapper-source-1",
        "wrapper-target-1",
    )
    .expect("merge_into_for_project");

    // The wrapper always reports merge_into — there is no code path to any
    // other lifecycle_action from this entry point.
    assert_eq!(result["lifecycle_action"], json!("merge_into"));
    assert_eq!(result["source_id"], json!("wrapper-source-1"));
    assert_eq!(result["target_id"], json!("wrapper-target-1"));
    assert_eq!(result["superseded"], json!(true));
    assert_eq!(result["archived"], json!(true));

    // Archived rows are hidden from default `get` (fetch_by_ids filters
    // `archived = 0` — see memcore/src/db/memory_crud/read.rs); the plain-get
    // path exercised by consolidate_lifecycle's reviewed-apply test (above,
    // "Archived rows are hidden from default get") applies here too, so
    // assert both halves: default `get` returns None, and
    // `get_with_options(.., true)` proves the row is archived, not deleted.
    let source_hidden_from_default_get = server
        .with_global_store_read(|store| {
            store
                .get("wrapper-source-1")
                .map_err(|e| e.to_string())
        })
        .expect("read source via default get");
    assert!(
        source_hidden_from_default_get.is_none(),
        "archived source must be hidden from default get, same visibility rule as the reviewed apply path"
    );

    let source_after = server
        .with_global_store_read(|store| {
            store
                .get_with_options("wrapper-source-1", true)
                .map_err(|e| e.to_string())
                .map(|e| e.expect("source still present (soft-deleted, not hard-deleted)"))
        })
        .expect("read source with include_archived");
    assert!(
        source_after.archived,
        "merge_into must archive the source row, same as the reviewed apply path"
    );

    let target_after = server
        .with_global_store_read(|store| {
            store
                .get("wrapper-target-1")
                .map_err(|e| e.to_string())
                .map(|e| e.expect("target still present"))
        })
        .expect("read target");
    assert!(
        target_after.keywords.contains(&"source-kw".to_string()),
        "merge_into folds source keywords into the survivor"
    );
}
