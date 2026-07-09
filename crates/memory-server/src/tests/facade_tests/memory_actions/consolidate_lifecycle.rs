//! Discrimination tests for consolidate propose → review → apply (#775).
//!
//! Pre-fix: consolidate always returned dry_run and never mutated rows.
//! Post-fix: same-path scratch duplicates produce a supersede proposal that
//! archives the older row only after approve + confirm=true.

use super::*;
use chrono::{Duration, Utc};

fn seed_scratch(id: &str, path: &str, text: &str, days_ago: i64) -> memory_core::MemoryEntry {
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

    let proposals = parsed["generated"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let supersede = proposals
        .iter()
        .find(|p| {
            p.get("lifecycle_action").and_then(Value::as_str) == Some("supersede")
                && p.get("source_id").and_then(Value::as_str) == Some("life-old-1")
                && p.get("target_id").and_then(Value::as_str) == Some("life-new-1")
        })
        .expect("supersede older→newer proposal");
    let proposal_id = supersede["proposal_id"]
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
    assert_eq!(apply_json["apply_result"]["lifecycle_action"], json!("supersede"));
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
    let hits_wiki = generated.iter().any(|p| {
        p.get("source_id").and_then(Value::as_str) == Some("life-wiki-1")
    });
    assert!(
        !hits_wiki,
        "wiki rows must not appear as archive/supersede sources: {parsed}"
    );
}
