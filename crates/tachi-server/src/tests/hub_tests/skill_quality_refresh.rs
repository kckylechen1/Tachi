//! audit G1: the skill-quality refresh writes only `definition.quality_guard`
//! (never a full-row upsert from its snapshot), writes nothing when the
//! quality result is unchanged, and runs off the `hub_feedback` path.

use super::*;
use crate::DbScope;

const DUPLICATE_CONTENT: &str =
    "Triage flaky integration suites: rerun the failing shard, capture logs, bisect the commit.";

fn duplicate_skill(id: &str, name: &str) -> HubCapability {
    let mut cap =
        crate::tests::make_skill_capability(id, name, "quality refresh fixture", "listed");
    cap.definition = json!({
        "content": DUPLICATE_CONTENT,
        "prompt": DUPLICATE_CONTENT,
        "policy": {"visibility": "listed"},
    })
    .to_string();
    cap
}

fn seed(server: &crate::MemoryServer, caps: &[HubCapability]) {
    server
        .with_global_store(|store| {
            for cap in caps {
                store.hub_register(cap).map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .expect("seed skills");
}

fn load(server: &crate::MemoryServer, id: &str) -> HubCapability {
    server
        .with_global_store_read(|store| store.hub_get(id).map_err(|e| e.to_string()))
        .expect("load capability")
        .expect("capability exists")
}

fn all_skill_rows(server: &crate::MemoryServer) -> Value {
    let rows = server
        .with_global_store_read(|store| {
            store
                .hub_list(Some("skill"), false)
                .map_err(|e| e.to_string())
        })
        .expect("list skills");
    serde_json::to_value(rows).expect("serialize rows")
}

fn merge_hint_ids(cap: &HubCapability) -> Vec<String> {
    let def: Value = serde_json::from_str(&cap.definition).expect("definition json");
    def["quality_guard"]["merge_hints"]
        .as_array()
        .map(|hints| {
            hints
                .iter()
                .filter_map(|hint| hint["skill_id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn has_quality_guard(cap: &HubCapability) -> bool {
    let def: Value = serde_json::from_str(&cap.definition).expect("definition json");
    def.get("quality_guard").is_some()
}

#[test]
fn quality_refresh_preserves_concurrent_health_stats_and_review_updates() {
    let server = make_server();
    let mut target = duplicate_skill("skill:g1-target", "g1-target");
    target.fail_streak = 3;
    target.health_status = "degraded".to_string();
    target.uses = 5;
    target.successes = 4;
    target.failures = 1;
    target.avg_rating = 4.0;
    seed(
        &server,
        &[target, duplicate_skill("skill:g1-twin", "g1-twin")],
    );

    let mut interleaved: Option<HubCapability> = None;
    let result = crate::wiki_ops::refresh_skill_quality_scope_interleaved_for_test(
        &server,
        DbScope::Global,
        || {
            // Runs after the refresh read its snapshot and before it writes:
            // circuit breaker 3 -> 4 (opens), feedback, and a review change.
            server
                .with_global_store(|store| {
                    store
                        .hub_record_call_outcome("skill:g1-target", false, Some("timeout"), 4)
                        .map_err(|e| e.to_string())?;
                    store
                        .hub_record_feedback("skill:g1-target", true, Some(2.0))
                        .map_err(|e| e.to_string())?;
                    store
                        .hub_set_review("skill:g1-target", "pending", Some(false))
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })
                .expect("concurrent updates");
            interleaved = Some(load(&server, "skill:g1-target"));
        },
    )
    .expect("quality refresh");
    let interleaved = interleaved.expect("interleaving ran");
    assert_eq!(interleaved.fail_streak, 4);
    assert_eq!(interleaved.health_status, "open");

    let after = load(&server, "skill:g1-target");
    assert!(
        result["updated_caps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == "skill:g1-target"),
        "quality guard must still be written: {result}"
    );
    assert_eq!(merge_hint_ids(&after), vec!["skill:g1-twin".to_string()]);

    // Every column except `definition` is exactly what the concurrent
    // writers left, including `updated_at`.
    let mut expected = serde_json::to_value(&interleaved).unwrap();
    expected["definition"] = json!(after.definition);
    assert_eq!(serde_json::to_value(&after).unwrap(), expected);
    assert_eq!(after.fail_streak, 4);
    assert_eq!(after.health_status, "open");
    assert_eq!(after.last_error.as_deref(), Some("timeout"));
    assert_eq!(after.uses, 6);
    assert_eq!(after.successes, 5);
    assert!(!after.enabled);
    assert_eq!(after.review_status, "pending");

    // Only quality_guard was added to the definition.
    let mut after_def: Value = serde_json::from_str(&after.definition).unwrap();
    after_def.as_object_mut().unwrap().remove("quality_guard");
    let interleaved_def: Value = serde_json::from_str(&interleaved.definition).unwrap();
    assert_eq!(after_def, interleaved_def);
}

#[test]
fn quality_refresh_skips_row_whose_content_changed_after_snapshot() {
    let server = make_server();
    seed(
        &server,
        &[
            duplicate_skill("skill:g1-edited", "g1-edited"),
            duplicate_skill("skill:g1-stable", "g1-stable"),
        ],
    );
    let mut edited = duplicate_skill("skill:g1-edited", "g1-edited");
    edited.definition = json!({
        "content": "Completely rewritten: summarize release notes for the changelog.",
        "policy": {"visibility": "listed"},
    })
    .to_string();
    let edited_definition = edited.definition.clone();

    let result = crate::wiki_ops::refresh_skill_quality_scope_interleaved_for_test(
        &server,
        DbScope::Global,
        || seed(&server, std::slice::from_ref(&edited)),
    )
    .expect("quality refresh");

    assert!(
        result["skipped_conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == "skill:g1-edited"),
        "stale result for edited skill must be skipped: {result}"
    );
    assert_eq!(
        load(&server, "skill:g1-edited").definition,
        edited_definition
    );
}

#[test]
fn unchanged_quality_refresh_writes_nothing() {
    let server = make_server();
    seed(
        &server,
        &[
            duplicate_skill("skill:g1-a", "g1-a"),
            duplicate_skill("skill:g1-b", "g1-b"),
        ],
    );
    let first = crate::wiki_ops::refresh_skill_quality_guards(&server).expect("first refresh");
    assert!(
        !first["global"]["updated_caps"]
            .as_array()
            .unwrap()
            .is_empty(),
        "first refresh stamps quality guards: {first}"
    );
    let before = all_skill_rows(&server);

    let second = crate::wiki_ops::refresh_skill_quality_guards(&server).expect("second refresh");
    assert_eq!(second["global"]["updated_caps"], json!([]), "{second}");
    assert_eq!(
        second["global"]["merge_hints"], first["global"]["merge_hints"],
        "same quality result"
    );
    assert_eq!(
        all_skill_rows(&server),
        before,
        "no row (definition, quality_guard.updated_at or updated_at column) may change"
    );
}

#[tokio::test]
async fn skill_content_change_schedules_refresh_that_updates_quality() {
    let server = make_server();
    seed(
        &server,
        &[duplicate_skill("skill:g1-existing", "g1-existing")],
    );
    crate::wiki_ops::refresh_skill_quality_guards(&server).expect("initial refresh");
    assert!(merge_hint_ids(&load(&server, "skill:g1-existing")).is_empty());
    let _ = server.run_pending_skill_quality_refreshes();

    server
        .hub_register(Parameters(HubRegisterParams {
            id: "skill:g1-new".to_string(),
            cap_type: "skill".to_string(),
            name: "g1-new".to_string(),
            description: "new duplicate".to_string(),
            definition: json!({
                "content": DUPLICATE_CONTENT,
                "prompt": DUPLICATE_CONTENT,
                "policy": {"visibility": "listed"},
            })
            .to_string(),
            version: 1,
            scope: "global".to_string(),
        }))
        .await
        .expect("hub_register skill");

    assert_eq!(
        server.skill_quality_refresh.pending_scopes(),
        vec![DbScope::Global],
        "content change requests a refresh of its scope"
    );
    assert!(!has_quality_guard(&load(&server, "skill:g1-new")));

    let results = server.run_pending_skill_quality_refreshes();
    assert_eq!(results.len(), 1);
    results[0].as_ref().expect("background refresh succeeds");
    assert_eq!(
        merge_hint_ids(&load(&server, "skill:g1-new")),
        vec!["skill:g1-existing".to_string()]
    );
    assert_eq!(
        merge_hint_ids(&load(&server, "skill:g1-existing")),
        vec!["skill:g1-new".to_string()]
    );
    assert!(server.skill_quality_refresh.pending_scopes().is_empty());
}

#[tokio::test]
async fn hub_feedback_defers_quality_refresh_and_throttles_it() {
    let server = make_server();
    seed(
        &server,
        &[
            duplicate_skill("skill:g1-fb-a", "g1-fb-a"),
            duplicate_skill("skill:g1-fb-b", "g1-fb-b"),
        ],
    );
    let before = load(&server, "skill:g1-fb-a");

    for _ in 0..2 {
        let response = server
            .hub_feedback(Parameters(HubFeedbackParams {
                id: "skill:g1-fb-a".to_string(),
                success: true,
                rating: Some(4.0),
            }))
            .await
            .expect("hub_feedback");
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            response,
            json!({"id": "skill:g1-fb-a", "recorded": true, "db": "global"})
        );
    }

    let after_feedback = load(&server, "skill:g1-fb-a");
    assert_eq!(after_feedback.uses, before.uses + 2);
    assert_eq!(
        after_feedback.definition, before.definition,
        "no pairwise refresh on the feedback request path"
    );
    assert_eq!(
        server.skill_quality_refresh.pending_scopes(),
        vec![DbScope::Global],
        "feedback keeps the refresh reachable as a deferred request"
    );

    let results = server.run_pending_skill_quality_refreshes();
    assert_eq!(
        results.len(),
        1,
        "two feedback calls coalesce into one refresh"
    );
    results[0].as_ref().expect("deferred refresh succeeds");
    assert_eq!(
        merge_hint_ids(&load(&server, "skill:g1-fb-a")),
        vec!["skill:g1-fb-b".to_string()]
    );

    server
        .hub_feedback(Parameters(HubFeedbackParams {
            id: "skill:g1-fb-a".to_string(),
            success: false,
            rating: None,
        }))
        .await
        .expect("hub_feedback");
    assert!(
        server.skill_quality_refresh.pending_scopes().is_empty(),
        "feedback-triggered refreshes are throttled per scope"
    );
}
