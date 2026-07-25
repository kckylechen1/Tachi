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
    let proposal_prefix = "lifecycle:merge_into:";
    assert!(proposal_id.starts_with(proposal_prefix));
    assert_eq!(proposal_id.len(), proposal_prefix.len() + 64);
    assert!(proposal_id[proposal_prefix.len()..]
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
    assert!(
        !proposal_id.contains("life-old-1") && !proposal_id.contains("life-new-1"),
        "bounded proposal IDs must not embed raw endpoint IDs: {proposal_id}"
    );
    let apply_payload = merge["apply_payload"]
        .as_object()
        .expect("v2 proposal stores typed apply_payload");
    assert_eq!(merge["schema_version"], json!(2));
    assert_eq!(merge["policy_version"], json!("memory-lifecycle-v2"));
    assert_eq!(apply_payload["schema_version"], json!(2));
    assert_eq!(
        apply_payload["policy_version"],
        json!("memory-lifecycle-v2")
    );
    assert_eq!(apply_payload["lifecycle_action"], json!("merge_into"));
    let identity = merge["identity"].as_str().expect("v2 proposal identity");
    assert_eq!(identity.len(), 64, "identity is a full SHA-256 hex digest");
    let typed_payload: memcore::store::memory_lifecycle::LifecycleApplyPayload =
        serde_json::from_value(merge["apply_payload"].clone()).expect("typed payload");
    assert_eq!(
        identity,
        memcore::store::memory_lifecycle::compute_lifecycle_identity(&typed_payload),
        "stored identity must hash the stored apply_payload"
    );

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
    assert!(
        review_json["proposal"]["expires_at"].is_null(),
        "#1342 follow-up: an approved-but-not-yet-applied proposal must stay \
         TTL-less until its own terminal (applied) write: {review_json}"
    );

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
    // #1342 follow-up: `applied` is terminal, so this write must carry a TTL.
    let expires_at = apply_json["proposal"]["expires_at"]
        .as_str()
        .expect("applied proposal must carry expires_at");
    assert!(
        chrono::DateTime::parse_from_rfc3339(expires_at).is_ok(),
        "expires_at must be a valid RFC3339 timestamp: {expires_at}"
    );

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

/// #1342 follow-up: a rejected proposal is terminal (it will never be
/// applied), so its review write must carry a 30-day TTL immediately —
/// unlike `approved`, which must wait for `handle_apply`'s own terminal write.
#[tokio::test]
async fn consolidate_reject_stamps_a_ttl_immediately() {
    let server = make_server();
    let older = seed_scratch(
        "life-rej-old",
        "/scratch/sigil/reject-topic",
        "Older draft of the same decision note about a rejected consolidation",
        10,
    );
    let newer = seed_scratch(
        "life-rej-new",
        "/scratch/sigil/reject-topic",
        "Newer draft of the same decision note about a rejected consolidation",
        1,
    );
    server
        .with_global_store(|store| {
            store.upsert(&older).map_err(|e| e.to_string())?;
            store.upsert(&newer).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed scratch duplicates");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose");
    let parsed: Value = serde_json::from_str(&body).expect("propose json");
    let proposals = parsed["generated"].as_array().cloned().unwrap_or_default();
    let merge = proposals
        .iter()
        .find(|p| {
            p.get("source_id").and_then(Value::as_str) == Some("life-rej-old")
                && p.get("target_id").and_then(Value::as_str) == Some("life-rej-new")
        })
        .expect("a proposal covering the seeded pair");
    let proposal_id = merge["proposal_id"]
        .as_str()
        .expect("proposal_id")
        .to_string();

    let mut review = tachi_memory_params("consolidate");
    review.format = Some("json".to_string());
    review.proposal_id = Some(proposal_id);
    review.review_status = Some("rejected".to_string());
    let review_body = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("review");
    let review_json: Value = serde_json::from_str(&review_body).expect("review json");
    assert_eq!(review_json["proposal"]["status"], json!("rejected"));

    let rejected_id = review_json["proposal"]["proposal_id"]
        .as_str()
        .expect("proposal id remains stored");
    let mut retry = tachi_memory_params("consolidate");
    retry.format = Some("json".to_string());
    retry.proposal_id = Some(rejected_id.to_string());
    retry.review_status = Some("approved".to_string());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, retry)
        .await
        .expect_err("rejected proposal cannot transition back to approved");
    assert!(
        err.contains("terminal/already-reviewed"),
        "unexpected error: {err}"
    );

    let expires_at = review_json["proposal"]["expires_at"]
        .as_str()
        .expect("a rejected (terminal) proposal must carry expires_at immediately");
    assert!(
        chrono::DateTime::parse_from_rfc3339(expires_at).is_ok(),
        "expires_at must be a valid RFC3339 timestamp: {expires_at}"
    );
}

#[tokio::test]
async fn consolidate_legacy_v1_proposals_list_but_review_and_apply_refuse_loudly() {
    let server = make_server();
    let legacy_id = "lifecycle:archive:legacy-listable";
    server
        .with_global_store(|store| {
            store
                .set_state(
                    "memory_lifecycle_proposals",
                    legacy_id,
                    r#"{"proposal_id":"lifecycle:archive:legacy-listable","status":"pending","source_id":"missing"}"#,
                )
                .map_err(|e| e.to_string())
        })
        .expect("seed legacy proposal");

    let mut list = tachi_memory_params("consolidate");
    list.format = Some("json".to_string());
    let listed: Value = serde_json::from_str(
        &crate::facade_memory_ops::handle_tachi_memory(&server, list)
            .await
            .expect("legacy proposals remain listable"),
    )
    .expect("list json");
    assert!(listed["proposals"].as_array().is_some_and(|rows| rows
        .iter()
        .any(|row| { row["proposal_id"] == json!(legacy_id) })));

    let mut review = tachi_memory_params("consolidate");
    review.proposal_id = Some(legacy_id.to_string());
    review.review_status = Some("approved".to_string());
    let review_err = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect_err("legacy review must refuse");
    assert!(
        review_err.contains("legacy v1"),
        "unexpected error: {review_err}"
    );

    let mut apply = tachi_memory_params("consolidate");
    apply.proposal_id = Some(legacy_id.to_string());
    apply.confirm = true;
    let apply_err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("legacy apply must refuse");
    assert!(
        apply_err.contains("legacy v1"),
        "unexpected error: {apply_err}"
    );
}

#[tokio::test]
async fn consolidate_apply_refuses_source_drift_and_keeps_approved_proposal() {
    let server = make_server();
    let source = seed_scratch(
        "life-source-drift-old",
        "/scratch/drift/source",
        "same lifecycle source text",
        10,
    );
    let target = seed_scratch(
        "life-source-drift-new",
        "/scratch/drift/source",
        "same lifecycle target text",
        1,
    );
    server
        .with_global_store(|store| {
            store.upsert(&source).map_err(|e| e.to_string())?;
            store.upsert(&target).map_err(|e| e.to_string())
        })
        .expect("seed source drift pair");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch/drift".to_string());
    let proposed: Value = serde_json::from_str(
        &crate::facade_memory_ops::handle_tachi_memory(&server, propose)
            .await
            .expect("propose"),
    )
    .expect("json");
    let proposal_id = proposed["generated"]
        .as_array()
        .unwrap()
        .iter()
        .find(|proposal| proposal["source_id"] == json!("life-source-drift-old"))
        .expect("source proposal")["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut review = tachi_memory_params("consolidate");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve");

    let target_revision_before = server
        .with_global_store_read(|store| {
            store
                .get("life-source-drift-new")
                .map_err(|e| e.to_string())
                .map(|row| row.unwrap().revision)
        })
        .expect("target revision");
    server
        .with_global_store(|store| {
            let mut drifted = store
                .get("life-source-drift-old")
                .map_err(|e| e.to_string())?
                .unwrap();
            drifted.text.push_str(" changed after approval");
            store.upsert(&drifted).map_err(|e| e.to_string())
        })
        .expect("introduce source drift");

    let mut apply = tachi_memory_params("consolidate");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("source drift must refuse apply");
    assert!(err.contains("identity mismatch"), "unexpected error: {err}");
    server
        .with_global_store_read(|store| {
            let source = store
                .get("life-source-drift-old")
                .map_err(|e| e.to_string())?
                .unwrap();
            let target = store
                .get("life-source-drift-new")
                .map_err(|e| e.to_string())?
                .unwrap();
            let (proposal, _) = store
                .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .unwrap();
            assert!(!source.archived, "failed apply must not archive source");
            assert_eq!(
                target.revision, target_revision_before,
                "failed apply must not mutate target"
            );
            assert_eq!(
                serde_json::from_str::<Value>(&proposal).unwrap()["status"],
                json!("approved")
            );
            Ok(())
        })
        .expect("verify failed apply remains atomic");
}

#[tokio::test]
async fn consolidate_apply_refuses_target_drift_and_keeps_source_unchanged() {
    let server = make_server();
    let source = seed_scratch(
        "life-target-drift-old",
        "/scratch/drift/target",
        "same lifecycle source text",
        10,
    );
    let target = seed_scratch(
        "life-target-drift-new",
        "/scratch/drift/target",
        "same lifecycle target text",
        1,
    );
    server
        .with_global_store(|store| {
            store.upsert(&source).map_err(|e| e.to_string())?;
            store.upsert(&target).map_err(|e| e.to_string())
        })
        .expect("seed target drift pair");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch/drift".to_string());
    let proposed: Value = serde_json::from_str(
        &crate::facade_memory_ops::handle_tachi_memory(&server, propose)
            .await
            .expect("propose"),
    )
    .expect("json");
    let proposal_id = proposed["generated"]
        .as_array()
        .unwrap()
        .iter()
        .find(|proposal| proposal["source_id"] == json!("life-target-drift-old"))
        .expect("target proposal")["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut review = tachi_memory_params("consolidate");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve");

    server
        .with_global_store(|store| {
            let mut drifted = store
                .get("life-target-drift-new")
                .map_err(|e| e.to_string())?
                .unwrap();
            drifted.text.push_str(" changed after approval");
            store.upsert(&drifted).map_err(|e| e.to_string())
        })
        .expect("introduce target drift");
    let mut apply = tachi_memory_params("consolidate");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("target drift must refuse apply");
    assert!(err.contains("identity mismatch"), "unexpected error: {err}");
    server
        .with_global_store_read(|store| {
            let source = store
                .get("life-target-drift-old")
                .map_err(|e| e.to_string())?
                .unwrap();
            let (proposal, _) = store
                .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .unwrap();
            assert!(
                !source.archived,
                "failed target-drift apply must not archive source"
            );
            assert_eq!(
                serde_json::from_str::<Value>(&proposal).unwrap()["status"],
                json!("approved")
            );
            Ok(())
        })
        .expect("verify target drift preserves approved proposal");
}

#[tokio::test]
async fn consolidate_apply_refuses_source_no_revision_supersession_drift_without_overwriting_c() {
    let server = make_server();
    let source = seed_scratch(
        "life-no-revision-source",
        "/scratch/no-revision/source",
        "outdated regional vendor policy that should yield to the newer baseline",
        10,
    );
    let target_b = seed_scratch(
        "life-no-revision-target-b",
        "/scratch/no-revision/source",
        "unrelated executive planning baseline selected as proposal target B",
        1,
    );
    let target_c = seed_scratch(
        "life-no-revision-target-c",
        "/scratch/no-revision/canonical",
        "canonical replacement C chosen after the proposal was approved",
        0,
    );
    server
        .with_global_store(|store| {
            store.upsert(&source).map_err(|e| e.to_string())?;
            store.upsert(&target_b).map_err(|e| e.to_string())?;
            store.upsert(&target_c).map_err(|e| e.to_string())
        })
        .expect("seed no-revision source drift rows");

    let mut propose = tachi_memory_params("consolidate");
    propose.path_prefix = Some("/scratch/no-revision/source".to_string());
    let proposed: Value = serde_json::from_str(
        &crate::facade_memory_ops::handle_tachi_memory(&server, propose)
            .await
            .expect("propose"),
    )
    .expect("json");
    let proposal_id = proposed["generated"]
        .as_array()
        .unwrap()
        .iter()
        .find(|proposal| proposal["source_id"] == json!("life-no-revision-source"))
        .expect("source proposal")["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        proposed["generated"]
            .as_array()
            .unwrap()
            .iter()
            .find(|proposal| proposal["proposal_id"] == json!(proposal_id))
            .unwrap()["lifecycle_action"],
        json!("supersede"),
        "the regression must exercise B overwriting a later C supersession"
    );

    let mut review = tachi_memory_params("consolidate");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve");

    let closing_at = "2026-07-25T00:00:00.000Z";
    let (source_revision, target_b_revision) = server
        .with_global_store(|store| {
            let source_revision = store
                .get("life-no-revision-source")
                .map_err(|e| e.to_string())?
                .expect("source before drift")
                .revision;
            let target_b_revision = store
                .get("life-no-revision-target-b")
                .map_err(|e| e.to_string())?
                .expect("target B before drift")
                .revision;
            assert_eq!(
                store
                    .mark_superseded_closing_validity(
                        "life-no-revision-source",
                        "life-no-revision-target-c",
                        closing_at,
                    )
                    .map_err(|e| e.to_string())?,
                1,
                "the no-revision supersession writer must land"
            );
            let source_state = store
                .connection()
                .query_row(
                    "SELECT superseded_by, valid_until, revision FROM memories WHERE id = ?1",
                    ["life-no-revision-source"],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(source_state.0.as_deref(), Some("life-no-revision-target-c"));
            assert_eq!(source_state.1.as_deref(), Some(closing_at));
            assert_eq!(
                source_state.2, source_revision,
                "the regression requires lifecycle drift with no revision bump"
            );
            Ok((source_revision, target_b_revision))
        })
        .expect("install no-revision source supersession");

    let mut apply = tachi_memory_params("consolidate");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("newer C supersession must refuse stale B apply");
    assert!(err.contains("identity mismatch"), "unexpected error: {err}");

    server
        .with_global_store_read(|store| {
            let source_state = store
                .connection()
                .query_row(
                    "SELECT archived, superseded_by, valid_until, revision FROM memories WHERE id = ?1",
                    ["life-no-revision-source"],
                    |row| {
                        Ok((
                            row.get::<_, bool>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .map_err(|e| e.to_string())?;
            let target_b = store
                .get("life-no-revision-target-b")
                .map_err(|e| e.to_string())?
                .expect("target B remains active");
            let (proposal, _) = store
                .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("approved proposal remains");
            assert!(!source_state.0, "refused apply must not archive source");
            assert_eq!(source_state.1.as_deref(), Some("life-no-revision-target-c"));
            assert_eq!(source_state.2.as_deref(), Some(closing_at));
            assert_eq!(source_state.3, source_revision);
            assert_eq!(target_b.revision, target_b_revision);
            assert_eq!(serde_json::from_str::<Value>(&proposal).unwrap()["status"], json!("approved"));
            Ok(())
        })
        .expect("refused source drift preserves C state and approved proposal");
}

#[tokio::test]
async fn consolidate_apply_refuses_target_no_revision_supersession_drift_without_mutation() {
    let server = make_server();
    let source = seed_scratch(
        "life-no-revision-target-source",
        "/scratch/no-revision/target",
        "outdated regional vendor policy that should yield to target B",
        10,
    );
    let target_b = seed_scratch(
        "life-no-revision-target-b",
        "/scratch/no-revision/target",
        "unrelated executive planning baseline selected as target B",
        1,
    );
    let target_c = seed_scratch(
        "life-no-revision-target-c",
        "/scratch/no-revision/target-canonical",
        "canonical replacement C chosen after approval",
        0,
    );
    server
        .with_global_store(|store| {
            store.upsert(&source).map_err(|e| e.to_string())?;
            store.upsert(&target_b).map_err(|e| e.to_string())?;
            store.upsert(&target_c).map_err(|e| e.to_string())
        })
        .expect("seed no-revision target drift rows");

    let mut propose = tachi_memory_params("consolidate");
    propose.path_prefix = Some("/scratch/no-revision/target".to_string());
    let proposed: Value = serde_json::from_str(
        &crate::facade_memory_ops::handle_tachi_memory(&server, propose)
            .await
            .expect("propose"),
    )
    .expect("json");
    let proposal_id = proposed["generated"]
        .as_array()
        .unwrap()
        .iter()
        .find(|proposal| proposal["source_id"] == json!("life-no-revision-target-source"))
        .expect("source proposal")["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut review = tachi_memory_params("consolidate");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve");

    let closing_at = "2026-07-25T00:00:01.000Z";
    let (source_revision, target_b_revision) = server
        .with_global_store(|store| {
            let source_revision = store
                .get("life-no-revision-target-source")
                .map_err(|e| e.to_string())?
                .expect("source before drift")
                .revision;
            let target_b_revision = store
                .get("life-no-revision-target-b")
                .map_err(|e| e.to_string())?
                .expect("target B before drift")
                .revision;
            assert_eq!(
                store
                    .mark_superseded_closing_validity(
                        "life-no-revision-target-b",
                        "life-no-revision-target-c",
                        closing_at,
                    )
                    .map_err(|e| e.to_string())?,
                1,
                "the no-revision target supersession writer must land"
            );
            let target_state = store
                .connection()
                .query_row(
                    "SELECT superseded_by, valid_until, revision FROM memories WHERE id = ?1",
                    ["life-no-revision-target-b"],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(target_state.0.as_deref(), Some("life-no-revision-target-c"));
            assert_eq!(target_state.1.as_deref(), Some(closing_at));
            assert_eq!(
                target_state.2, target_b_revision,
                "the target's no-revision state must retain its original revision"
            );
            Ok((source_revision, target_b_revision))
        })
        .expect("install no-revision target supersession");

    let mut apply = tachi_memory_params("consolidate");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("target C supersession must refuse stale apply");
    assert!(err.contains("identity mismatch"), "unexpected error: {err}");

    server
        .with_global_store_read(|store| {
            let source = store
                .get("life-no-revision-target-source")
                .map_err(|e| e.to_string())?
                .expect("source remains active");
            let target_state = store
                .connection()
                .query_row(
                    "SELECT archived, superseded_by, valid_until, revision FROM memories WHERE id = ?1",
                    ["life-no-revision-target-b"],
                    |row| {
                        Ok((
                            row.get::<_, bool>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .map_err(|e| e.to_string())?;
            let (proposal, _) = store
                .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("approved proposal remains");
            assert!(!source.archived, "refused target drift must not archive source");
            assert_eq!(source.revision, source_revision);
            assert!(!target_state.0, "refused apply must not archive target B");
            assert_eq!(target_state.1.as_deref(), Some("life-no-revision-target-c"));
            assert_eq!(target_state.2.as_deref(), Some(closing_at));
            assert_eq!(target_state.3, target_b_revision);
            assert_eq!(serde_json::from_str::<Value>(&proposal).unwrap()["status"], json!("approved"));
            Ok(())
        })
        .expect("refused target drift preserves C state and approved proposal");
}

#[tokio::test]
async fn consolidate_review_refuses_tampered_display_copies_without_mutation() {
    for (field, tampered_value) in [
        ("path", json!("/tampered/display-path")),
        ("rationale", json!("tampered rationale")),
        ("evidence", json!({"tampered": true})),
    ] {
        let server = make_server();
        let source_id = format!("life-review-display-{field}-source");
        let target_id = format!("life-review-display-{field}-target");
        let source = seed_scratch(
            &source_id,
            "/scratch/tamper/display-review",
            "older display tamper proposal source",
            10,
        );
        let target = seed_scratch(
            &target_id,
            "/scratch/tamper/display-review",
            "newer display tamper proposal target",
            1,
        );
        server
            .with_global_store(|store| {
                store.upsert(&source).map_err(|e| e.to_string())?;
                store.upsert(&target).map_err(|e| e.to_string())
            })
            .expect("seed review display tamper pair");

        let mut propose = tachi_memory_params("consolidate");
        propose.path_prefix = Some("/scratch/tamper/display-review".to_string());
        let proposed: Value = serde_json::from_str(
            &crate::facade_memory_ops::handle_tachi_memory(&server, propose)
                .await
                .expect("propose"),
        )
        .expect("json");
        let proposal_id = proposed["generated"]
            .as_array()
            .unwrap()
            .iter()
            .find(|proposal| proposal["source_id"] == json!(source_id))
            .expect("source proposal")["proposal_id"]
            .as_str()
            .unwrap()
            .to_string();

        server
            .with_global_store(|store| {
                let (raw, _) = store
                    .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                    .map_err(|e| e.to_string())?
                    .expect("persisted proposal");
                let mut tampered: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                tampered[field] = tampered_value.clone();
                store
                    .set_state(
                        "memory_lifecycle_proposals",
                        &proposal_id,
                        &serde_json::to_string(&tampered).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())
            })
            .expect("tamper displayed proposal copy");

        let mut review = tachi_memory_params("consolidate");
        review.proposal_id = Some(proposal_id.clone());
        review.review_status = Some("approved".to_string());
        let err = crate::facade_memory_ops::handle_tachi_memory(&server, review)
            .await
            .expect_err("display-copy tampering must refuse review");
        assert!(
            err.contains(&format!("top-level {field} differs")),
            "unexpected {field} error: {err}"
        );
        server
            .with_global_store_read(|store| {
                let source = store
                    .get(&source_id)
                    .map_err(|e| e.to_string())?
                    .expect("source remains active");
                let (raw, _) = store
                    .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                    .map_err(|e| e.to_string())?
                    .expect("proposal remains");
                let stored: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                assert!(!source.archived, "refused review must not mutate source");
                assert_eq!(stored["status"], json!("pending"));
                assert_eq!(
                    stored[field], tampered_value,
                    "refusal must not correct {field}"
                );
                Ok(())
            })
            .expect("review refusal preserves the tampered row");
    }
}

#[tokio::test]
async fn consolidate_apply_refuses_tampered_display_copies_without_mutation() {
    for (field, tampered_value) in [
        ("path", json!("/tampered/display-path")),
        ("rationale", json!("tampered rationale")),
        ("evidence", json!({"tampered": true})),
    ] {
        let server = make_server();
        let source_id = format!("life-apply-display-{field}-source");
        let target_id = format!("life-apply-display-{field}-target");
        let source = seed_scratch(
            &source_id,
            "/scratch/tamper/display-apply",
            "older display tamper proposal source",
            10,
        );
        let target = seed_scratch(
            &target_id,
            "/scratch/tamper/display-apply",
            "newer display tamper proposal target",
            1,
        );
        server
            .with_global_store(|store| {
                store.upsert(&source).map_err(|e| e.to_string())?;
                store.upsert(&target).map_err(|e| e.to_string())
            })
            .expect("seed apply display tamper pair");

        let mut propose = tachi_memory_params("consolidate");
        propose.path_prefix = Some("/scratch/tamper/display-apply".to_string());
        let proposed: Value = serde_json::from_str(
            &crate::facade_memory_ops::handle_tachi_memory(&server, propose)
                .await
                .expect("propose"),
        )
        .expect("json");
        let proposal_id = proposed["generated"]
            .as_array()
            .unwrap()
            .iter()
            .find(|proposal| proposal["source_id"] == json!(source_id))
            .expect("source proposal")["proposal_id"]
            .as_str()
            .unwrap()
            .to_string();

        let mut review = tachi_memory_params("consolidate");
        review.proposal_id = Some(proposal_id.clone());
        review.review_status = Some("approved".to_string());
        crate::facade_memory_ops::handle_tachi_memory(&server, review)
            .await
            .expect("approve intact proposal");

        let target_revision = server
            .with_global_store(|store| {
                let target_revision = store
                    .get(&target_id)
                    .map_err(|e| e.to_string())?
                    .expect("target before apply")
                    .revision;
                let (raw, _) = store
                    .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                    .map_err(|e| e.to_string())?
                    .expect("approved proposal");
                let mut tampered: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                tampered[field] = tampered_value.clone();
                store
                    .set_state(
                        "memory_lifecycle_proposals",
                        &proposal_id,
                        &serde_json::to_string(&tampered).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                Ok(target_revision)
            })
            .expect("tamper approved display copy");

        let mut apply = tachi_memory_params("consolidate");
        apply.proposal_id = Some(proposal_id.clone());
        apply.confirm = true;
        let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
            .await
            .expect_err("display-copy tampering must refuse apply");
        assert!(
            err.contains(&format!("top-level {field} differs")),
            "unexpected {field} error: {err}"
        );
        server
            .with_global_store_read(|store| {
                let source = store
                    .get(&source_id)
                    .map_err(|e| e.to_string())?
                    .expect("source remains active");
                let target = store
                    .get(&target_id)
                    .map_err(|e| e.to_string())?
                    .expect("target remains active");
                let (raw, _) = store
                    .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                    .map_err(|e| e.to_string())?
                    .expect("proposal remains");
                let stored: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                assert!(!source.archived, "refused apply must not mutate source");
                assert_eq!(target.revision, target_revision);
                assert_eq!(stored["status"], json!("approved"));
                assert_eq!(
                    stored[field], tampered_value,
                    "refusal must not correct {field}"
                );
                Ok(())
            })
            .expect("apply refusal preserves the tampered row");
    }
}

#[tokio::test]
async fn consolidate_review_refuses_tampered_top_level_execution_field_without_mutation() {
    let server = make_server();
    let source = seed_scratch(
        "life-review-tamper-source",
        "/scratch/tamper/review",
        "older lifecycle review tamper note",
        10,
    );
    let target = seed_scratch(
        "life-review-tamper-target",
        "/scratch/tamper/review",
        "newer lifecycle review tamper note",
        1,
    );
    server
        .with_global_store(|store| {
            store.upsert(&source).map_err(|e| e.to_string())?;
            store.upsert(&target).map_err(|e| e.to_string())
        })
        .expect("seed review tamper pair");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch/tamper/review".to_string());
    let proposed: Value = serde_json::from_str(
        &crate::facade_memory_ops::handle_tachi_memory(&server, propose)
            .await
            .expect("propose"),
    )
    .expect("json");
    let proposal_id = proposed["generated"]
        .as_array()
        .unwrap()
        .iter()
        .find(|proposal| proposal["source_id"] == json!("life-review-tamper-source"))
        .expect("source proposal")["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();

    server
        .with_global_store(|store| {
            let (raw, _) = store
                .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("persisted proposal");
            let mut tampered: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            tampered["lifecycle_action"] = json!("archive");
            store
                .set_state(
                    "memory_lifecycle_proposals",
                    &proposal_id,
                    &serde_json::to_string(&tampered).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())
        })
        .expect("tamper top-level action");

    let mut review = tachi_memory_params("consolidate");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect_err("top-level execution tampering must refuse review");
    assert!(
        err.contains("top-level lifecycle_action differs"),
        "unexpected error: {err}"
    );
    server
        .with_global_store_read(|store| {
            let source = store
                .get("life-review-tamper-source")
                .map_err(|e| e.to_string())?
                .expect("source remains visible");
            let (raw, _) = store
                .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("proposal remains");
            assert!(!source.archived, "rejected review must not mutate source");
            assert_eq!(
                serde_json::from_str::<Value>(&raw).unwrap()["status"],
                json!("pending")
            );
            Ok(())
        })
        .expect("verify no review mutation");
}

#[tokio::test]
async fn consolidate_apply_refuses_tampered_top_level_execution_field_without_mutation() {
    let server = make_server();
    let source = seed_scratch(
        "life-apply-tamper-source",
        "/scratch/tamper/apply",
        "older lifecycle apply tamper note",
        10,
    );
    let target = seed_scratch(
        "life-apply-tamper-target",
        "/scratch/tamper/apply",
        "newer lifecycle apply tamper note",
        1,
    );
    server
        .with_global_store(|store| {
            store.upsert(&source).map_err(|e| e.to_string())?;
            store.upsert(&target).map_err(|e| e.to_string())
        })
        .expect("seed apply tamper pair");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch/tamper/apply".to_string());
    let proposed: Value = serde_json::from_str(
        &crate::facade_memory_ops::handle_tachi_memory(&server, propose)
            .await
            .expect("propose"),
    )
    .expect("json");
    let proposal_id = proposed["generated"]
        .as_array()
        .unwrap()
        .iter()
        .find(|proposal| proposal["source_id"] == json!("life-apply-tamper-source"))
        .expect("source proposal")["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut review = tachi_memory_params("consolidate");
    review.proposal_id = Some(proposal_id.clone());
    review.review_status = Some("approved".to_string());
    crate::facade_memory_ops::handle_tachi_memory(&server, review)
        .await
        .expect("approve intact proposal");

    server
        .with_global_store(|store| {
            let (raw, _) = store
                .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("approved proposal");
            let mut tampered: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            tampered["source_id"] = json!("life-apply-tamper-target");
            store
                .set_state(
                    "memory_lifecycle_proposals",
                    &proposal_id,
                    &serde_json::to_string(&tampered).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())
        })
        .expect("tamper approved proposal");

    let mut apply = tachi_memory_params("consolidate");
    apply.proposal_id = Some(proposal_id.clone());
    apply.confirm = true;
    let err = crate::facade_memory_ops::handle_tachi_memory(&server, apply)
        .await
        .expect_err("top-level execution tampering must refuse apply");
    assert!(
        err.contains("top-level source_id differs"),
        "unexpected error: {err}"
    );
    server
        .with_global_store_read(|store| {
            let source = store
                .get("life-apply-tamper-source")
                .map_err(|e| e.to_string())?
                .expect("source remains visible");
            let (raw, _) = store
                .get_state_kv("memory_lifecycle_proposals", &proposal_id)
                .map_err(|e| e.to_string())?
                .expect("proposal remains");
            assert!(!source.archived, "rejected apply must not mutate source");
            assert_eq!(
                serde_json::from_str::<Value>(&raw).unwrap()["status"],
                json!("approved")
            );
            Ok(())
        })
        .expect("verify no apply mutation");
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
async fn consolidate_accounts_protected_rows_and_allows_an_eligible_peer() {
    let server = make_server();
    let mut wiki = make_entry("life-wiki-1");
    wiki.path = "/scope/protected".to_string();
    wiki.category = "wiki".to_string();
    wiki.importance = 0.2;
    wiki.access_count = 0;
    wiki.timestamp = (Utc::now() - Duration::days(400)).to_rfc3339();
    wiki.summary = "Old wiki page".into();
    wiki.text = "Old wiki page body".into();
    let mut eligible = make_entry("life-eligible-stale-1");
    eligible.path = "/scope/eligible".to_string();
    eligible.importance = 0.2;
    eligible.access_count = 0;
    eligible.timestamp = (Utc::now() - Duration::days(400)).to_rfc3339();
    server
        .with_global_store(|store| {
            store.upsert(&wiki).map_err(|e| e.to_string())?;
            store.upsert(&eligible).map_err(|e| e.to_string())
        })
        .expect("seed wiki");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scope".to_string());
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
    assert!(
        generated
            .iter()
            .any(|p| p.get("source_id").and_then(Value::as_str) == Some("life-eligible-stale-1")),
        "an eligible peer must still be proposed: {parsed}"
    );
    assert_eq!(parsed["scope_accounting"]["examined"], json!(2));
    assert_eq!(parsed["scope_accounting"]["evaluated"], json!(1));
    assert_eq!(
        parsed["scope_accounting"]["expected_exclusions"]["count"],
        json!(1)
    );
    assert!(
        parsed["scope_accounting"]["expected_exclusions"]["samples"]
            .as_array()
            .is_some_and(|samples| samples.iter().any(|sample| {
                sample["id"] == "life-wiki-1" && sample["reason"] == "wiki_category"
            })),
        "the protected row must be named in scope accounting: {parsed}"
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
        .with_global_store_read(|store| store.get("wrapper-source-1").map_err(|e| e.to_string()))
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

#[tokio::test]
async fn consolidate_propose_near_dup_merge_for_cross_path_raw_twins() {
    let server = make_server();
    let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                  kilo lima mike november oscar papa quebec romeo sierra";
    let mut lower = seed_scratch(
        "near-dup-low",
        "/scratch/sigil/topic-a",
        &format!("{shared} tango"),
        12,
    );
    lower.importance = 0.3;
    let mut higher = seed_scratch(
        "near-dup-high",
        "/scratch/sigil/topic-b",
        &format!("{shared} uniform"),
        2,
    );
    higher.importance = 0.8;
    server
        .with_global_store(|store| {
            store.upsert(&lower).map_err(|e| e.to_string())?;
            store.upsert(&higher).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed cross-path near-dup twins");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    let proposals = parsed["generated"].as_array().cloned().unwrap_or_default();
    let near_dup = proposals
        .iter()
        .find(|p| {
            p.get("lifecycle_action").and_then(Value::as_str) == Some("near_dup_merge")
                && p.get("source_id").and_then(Value::as_str) == Some("near-dup-low")
                && p.get("target_id").and_then(Value::as_str) == Some("near-dup-high")
        })
        .expect("near_dup_merge proposal for cross-path raw twins");
    let proposal_id = near_dup["proposal_id"].as_str().unwrap().to_string();

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
        json!("near_dup_merge")
    );
    assert_eq!(apply_json["apply_result"]["archived"], json!(true));
    assert_eq!(
        apply_json["apply_result"]["target_id"],
        json!("near-dup-high")
    );

    let source_after = server
        .with_global_store_read(|store| {
            store
                .get_with_options("near-dup-low", true)
                .map_err(|e| e.to_string())
                .map(|e| e.expect("source still present for provenance"))
        })
        .expect("read source");
    assert!(
        source_after.archived,
        "near_dup_merge apply must archive the source row"
    );

    let target_after = server
        .with_global_store_read(|store| {
            store
                .get("near-dup-high")
                .map_err(|e| e.to_string())
                .map(|e| e.expect("target exists"))
        })
        .expect("read target");
    assert!(!target_after.archived, "survivor must stay active");
}

#[tokio::test]
async fn consolidate_near_dup_merge_never_proposes_protected_rows() {
    let server = make_server();
    let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                  kilo lima mike november oscar papa quebec romeo sierra";
    let mut pinned = seed_scratch(
        "near-dup-pinned",
        "/scratch/sigil/pinned-twin",
        &format!("{shared} tango"),
        20,
    );
    pinned.retention_policy = Some("pinned".to_string());
    let eligible = seed_scratch(
        "near-dup-eligible",
        "/scratch/sigil/eligible-twin",
        &format!("{shared} uniform"),
        5,
    );
    server
        .with_global_store(|store| {
            store.upsert(&pinned).map_err(|e| e.to_string())?;
            store.upsert(&eligible).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed pinned twin");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    let proposals = parsed["generated"].as_array().cloned().unwrap_or_default();
    assert!(
        !proposals.iter().any(|p| {
            p.get("source_id").and_then(Value::as_str) == Some("near-dup-pinned")
                || p.get("target_id").and_then(Value::as_str) == Some("near-dup-pinned")
        }),
        "protected pinned rows must never appear in near_dup_merge proposals: {parsed}"
    );
    assert_eq!(parsed["scope_accounting"]["evaluated"], json!(1));
    assert_eq!(
        parsed["scope_accounting"]["expected_exclusions"]["count"],
        json!(1)
    );
}

#[tokio::test]
async fn consolidate_no_op_sibling_star_merges_remain_applicable() {
    let server = make_server();
    let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                  kilo lima mike november oscar papa quebec romeo sierra";
    let mut a = seed_scratch(
        "near-dup-chain-a",
        "/scratch/sigil/chain-a",
        &format!("{shared} tango"),
        20,
    );
    a.importance = 0.4;
    let mut b = seed_scratch(
        "near-dup-chain-b",
        "/scratch/sigil/chain-b",
        &format!("{shared} uniform"),
        10,
    );
    b.importance = 0.5;
    let mut c = seed_scratch(
        "near-dup-chain-c",
        "/scratch/sigil/chain-c",
        &format!("{shared} victor"),
        1,
    );
    c.importance = 0.9;
    server
        .with_global_store(|store| {
            store.upsert(&a).map_err(|e| e.to_string())?;
            store.upsert(&b).map_err(|e| e.to_string())?;
            store.upsert(&c).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed transitive near-dup chain");
    let target_revision_before = server
        .with_global_store_read(|store| {
            store
                .get("near-dup-chain-c")
                .map_err(|e| e.to_string())
                .map(|entry| entry.expect("survivor exists").revision)
        })
        .expect("read survivor revision");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    let near_dups: Vec<&Value> = parsed["generated"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p.get("lifecycle_action").and_then(Value::as_str) == Some("near_dup_merge"))
        .collect();
    assert_eq!(
        near_dups.len(),
        2,
        "A~B~C clique must collapse to two star edges into one survivor: {parsed}"
    );
    let proposal_ids: std::collections::HashSet<&str> = near_dups
        .iter()
        .map(|p| p.get("proposal_id").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(
        proposal_ids.len(),
        near_dups.len(),
        "star edges must have distinct proposal_ids: {near_dups:?}"
    );
    assert!(
        proposal_ids.iter().all(|proposal_id| {
            let prefix = "lifecycle:near_dup_merge:";
            proposal_id.starts_with(prefix)
                && proposal_id.len() == prefix.len() + 64
                && proposal_id[prefix.len()..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                && !proposal_id.contains("near-dup-chain-a")
                && !proposal_id.contains("near-dup-chain-b")
                && !proposal_id.contains("near-dup-chain-c")
        }),
        "proposal IDs must be bounded identities rather than raw endpoint IDs: {proposal_ids:?}"
    );
    assert!(
        near_dups
            .iter()
            .all(|p| p.get("target_id").and_then(Value::as_str) == Some("near-dup-chain-c")),
        "every near_dup_merge must target the highest-importance survivor: {near_dups:?}"
    );
    let mut sources = near_dups
        .iter()
        .map(|p| p.get("source_id").and_then(Value::as_str).unwrap())
        .collect::<Vec<_>>();
    sources.sort_unstable();
    assert_eq!(sources, vec!["near-dup-chain-a", "near-dup-chain-b"]);
    let source_set: std::collections::HashSet<&str> = sources.iter().copied().collect();
    let target_set: std::collections::HashSet<&str> = near_dups
        .iter()
        .map(|p| p.get("target_id").and_then(Value::as_str).unwrap())
        .collect();
    assert!(
        source_set.is_disjoint(&target_set),
        "no id may appear as both source and target: sources={source_set:?} targets={target_set:?}"
    );

    for proposal in &near_dups {
        let proposal_id = proposal["proposal_id"].as_str().unwrap().to_string();
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
        crate::facade_memory_ops::handle_tachi_memory(&server, apply)
            .await
            .expect("batch apply all near_dup_merge proposals");
    }

    let survivor = server
        .with_global_store_read(|store| {
            store
                .get("near-dup-chain-c")
                .map_err(|e| e.to_string())
                .map(|e| e.expect("survivor exists"))
        })
        .expect("read survivor");
    assert!(!survivor.archived);
    assert_eq!(
        survivor.revision, target_revision_before,
        "no-op sibling merges must not rewrite/bump the shared star target"
    );
    for source_id in ["near-dup-chain-a", "near-dup-chain-b"] {
        let archived = server
            .with_global_store_read(|store| {
                store
                    .get_with_options(source_id, true)
                    .map_err(|e| e.to_string())
                    .map(|e| e.expect("source retained for provenance").archived)
            })
            .expect("read source");
        assert!(archived, "{source_id} must be archived after star apply");
    }
}

#[tokio::test]
async fn consolidate_same_path_twins_do_not_emit_near_dup_merge() {
    let server = make_server();
    let shared = "alpha bravo charlie delta echo foxtrot golf hotel india juliet \
                  kilo lima mike november oscar papa quebec romeo sierra";
    let older = seed_scratch(
        "same-path-old",
        "/scratch/sigil/same-path-twins",
        &format!("{shared} tango"),
        10,
    );
    let newer = seed_scratch(
        "same-path-new",
        "/scratch/sigil/same-path-twins",
        &format!("{shared} uniform"),
        1,
    );
    server
        .with_global_store(|store| {
            store.upsert(&older).map_err(|e| e.to_string())?;
            store.upsert(&newer).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed same-path twins");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    let proposals = parsed["generated"].as_array().cloned().unwrap_or_default();
    assert!(
        proposals.iter().any(|p| {
            p.get("lifecycle_action").and_then(Value::as_str) == Some("merge_into")
                && p.get("source_id").and_then(Value::as_str) == Some("same-path-old")
                && p.get("target_id").and_then(Value::as_str) == Some("same-path-new")
        }),
        "same-path twins belong to merge_into: {parsed}"
    );
    assert!(
        !proposals
            .iter()
            .any(|p| p.get("lifecycle_action").and_then(Value::as_str) == Some("near_dup_merge")),
        "same-path twins must not also emit near_dup_merge: {parsed}"
    );
}

#[tokio::test]
async fn consolidate_propose_near_dup_merge_for_chinese_cross_path_twins() {
    let server = make_server();
    let shared = "数据库迁移需要先备份再执行脚本检查索引状态确认无误后再同步配置并记录变更摘要完毕";
    let mut lower = seed_scratch(
        "near-dup-zh-low",
        "/scratch/sigil/zh-a",
        &format!("{shared}提交"),
        8,
    );
    lower.importance = 0.35;
    let mut higher = seed_scratch(
        "near-dup-zh-high",
        "/scratch/sigil/zh-b",
        &format!("{shared}归档"),
        2,
    );
    higher.importance = 0.75;
    server
        .with_global_store(|store| {
            store.upsert(&lower).map_err(|e| e.to_string())?;
            store.upsert(&higher).map_err(|e| e.to_string())?;
            Ok(())
        })
        .expect("seed Chinese near-dup twins");

    let mut propose = tachi_memory_params("consolidate");
    propose.format = Some("json".to_string());
    propose.path_prefix = Some("/scratch".to_string());
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, propose)
        .await
        .expect("propose");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    let proposals = parsed["generated"].as_array().cloned().unwrap_or_default();
    assert!(
        proposals.iter().any(|p| {
            p.get("lifecycle_action").and_then(Value::as_str) == Some("near_dup_merge")
                && p.get("source_id").and_then(Value::as_str) == Some("near-dup-zh-low")
                && p.get("target_id").and_then(Value::as_str) == Some("near-dup-zh-high")
        }),
        "Chinese cross-path twins must surface near_dup_merge: {parsed}"
    );
}
