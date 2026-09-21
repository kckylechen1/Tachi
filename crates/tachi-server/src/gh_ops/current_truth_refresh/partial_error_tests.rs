//! Actual hardened runner → refresh adapter → persisted privacy discriminators.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const FAKE_GH_SCRIPT: &str = r#"#!/bin/sh
set -eu
[ "$#" -ge 2 ] || exit 93
[ "$1" = api ] && [ "$2" = graphql ] || exit 94
root=${0%/*}
printf '%s\n' call >> "$root/calls"
/bin/cat "$root/payload"
/bin/cat "$root/stderr" >&2
code=$(/bin/cat "$root/exit")
exit "$code"
"#;

struct GhFixture {
    _root: tempfile::TempDir,
    bin: PathBuf,
    previous_path: Option<std::ffi::OsString>,
}

impl GhFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("gh's fixture");
        std::fs::create_dir(&bin).unwrap();
        let executable = bin.join("gh");
        std::fs::write(&executable, FAKE_GH_SCRIPT).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let previous_path = std::env::var_os("PATH");
        let mut paths = vec![bin.clone()];
        if let Some(previous) = &previous_path {
            paths.extend(std::env::split_paths(previous));
        }
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        Self {
            _root: root,
            bin,
            previous_path,
        }
    }

    fn respond(&self, payload: &Value, exit: i32) {
        self.respond_bytes(payload.to_string().as_bytes(), exit);
    }

    fn respond_bytes(&self, payload: &[u8], exit: i32) {
        std::fs::write(self.bin.join("payload"), payload).unwrap();
        std::fs::write(self.bin.join("exit"), exit.to_string()).unwrap();
        std::fs::write(self.bin.join("stderr"), b"private-diagnostic-sentinel\n").unwrap();
    }

    fn call_count(&self) -> usize {
        std::fs::read_to_string(self.bin.join("calls"))
            .unwrap()
            .lines()
            .count()
    }

    fn executable(&self) -> &Path {
        &self.bin
    }
}

impl Drop for GhFixture {
    fn drop(&mut self) {
        match &self.previous_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
    }
}

fn complete_payload(visibility: &str) -> Value {
    json!({"data": {"repository": {
        "visibility": visibility,
        "issueOrPullRequest": {
            "__typename": "Issue",
            "number": 42,
            "state": "OPEN",
            "updatedAt": "2026-09-01T00:00:00Z",
            "timelineItems": {"pageInfo": {"hasNextPage": false}, "nodes": []}
        }
    }}})
}

async fn refresh(server: &MemoryServer, repo: &str) -> Value {
    let response = handle_current_truth_refresh_with_floor(
        server,
        &TachiGhParams {
            action: "current_truth_refresh".to_string(),
            repo: Some(repo.to_string()),
            number: Some(42),
            ..TachiGhParams::default()
        },
        Duration::ZERO,
    )
    .await
    .unwrap();
    assert!(!response.contains("private-diagnostic-sentinel"));
    assert!(!response.contains("private-payload-sentinel"));
    serde_json::from_str(&response).unwrap()
}

fn assert_private_response(response: &Value) {
    assert_eq!(response["fresh"], json!(false));
    assert_eq!(response["work_status"], json!([]));
    assert_eq!(response["posture"]["last_fresh_revision"], Value::Null);
    assert_eq!(response["posture"]["last_fresh_at"], Value::Null);
    assert_eq!(
        response["posture"]["unavailable_reason"],
        REASON_UNAVAILABLE
    );
    assert!(!response.to_string().contains("issue:42"));
    assert!(!response.to_string().contains("github-issue"));
}

// PATH is process-global; retain the repository's shared serialization guard
// across all asynchronous calls, including the fixture's restoration on drop.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn nonzero_private_and_internal_observations_hide_public_history_then_survive_denial() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = GhFixture::new();
    assert!(fixture.executable().join("gh").is_file());
    let mut expected_calls = 0;
    for visibility in ["PRIVATE", "INTERNAL"] {
        for shape in ["complete", "partial-error", "null", "malformed"] {
            let server = crate::tests::make_server();
            fixture.respond(&complete_payload("PUBLIC"), 0);
            let public = refresh(&server, "Owner/Repo").await;
            expected_calls += 1;
            assert_eq!(public["fresh"], json!(true));
            assert_eq!(
                public["work_status"][0]["work_token"],
                json!("owner/repo#issue:42")
            );
            let mut payload = complete_payload(visibility);
            match shape {
                "partial-error" => {
                    payload["errors"] = json!([{"message": "private-payload-sentinel"}]);
                }
                "null" => payload["data"]["repository"]["issueOrPullRequest"] = Value::Null,
                "malformed" => {
                    payload["data"]["repository"]["issueOrPullRequest"]["state"] = Value::Null;
                }
                _ => {}
            }
            fixture.respond(&payload, 7);
            let private = refresh(&server, "OWNER/repo").await;
            expected_calls += 1;
            assert_private_response(&private);
            fixture.respond_bytes(b"", 1);
            let denied = refresh(&server, "owner/REPO").await;
            expected_calls += 1;
            assert_private_response(&denied);
            server
                .with_current_truth_store(|store| {
                    assert_eq!(
                        store.repository_visibility("OWNER/Repo").unwrap(),
                        Some(VisibilityClassV1::Private)
                    );
                    assert_eq!(store.assertion_count("owner/repo").unwrap(), 3);
                    assert_eq!(store.refresh_debt_repos().unwrap(), 1);
                    Ok(())
                })
                .unwrap();
            assert_eq!(fixture.call_count(), expected_calls);
        }
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn nonzero_public_cannot_relax_private_visibility_or_mint_fresh_truth() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = GhFixture::new();
    let server = crate::tests::make_server();
    fixture.respond(&complete_payload("PRIVATE"), 0);
    assert_private_response(&refresh(&server, "owner/repo").await);
    fixture.respond(&complete_payload("PUBLIC"), 7);
    assert_private_response(&refresh(&server, "owner/repo").await);
    server
        .with_current_truth_store(|store| {
            assert_eq!(
                store.repository_visibility("owner/repo").unwrap(),
                Some(VisibilityClassV1::Private)
            );
            assert_eq!(store.assertion_count("owner/repo").unwrap(), 3);
            Ok(())
        })
        .unwrap();

    let fresh_server = crate::tests::make_server();
    let failed = refresh(&fresh_server, "owner/repo").await;
    assert_eq!(failed["fresh"], json!(false));
    fresh_server
        .with_current_truth_store(|store| {
            assert_eq!(store.assertion_count("owner/repo").unwrap(), 0);
            assert_eq!(store.repository_visibility("owner/repo").unwrap(), None);
            Ok(())
        })
        .unwrap();
    assert_eq!(fixture.call_count(), 3);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn strict_runner_and_refresh_failure_carrier_never_disguise_nonzero_exit_as_success() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = GhFixture::new();
    let server = crate::tests::make_server();
    let payload = complete_payload("PRIVATE");
    fixture.respond(&payload, 7);
    let args = vec!["api".to_string(), "graphql".to_string()];
    let strict = run_gh_json_bounded(&server, args.clone(), CURRENT_TRUTH_GH_TIMEOUT, "fixture")
        .await
        .expect_err("the existing strict consumer must still fail");
    assert!(strict.contains("exit 7"));
    assert!(!strict.contains("issueOrPullRequest"));
    let failure = run_gh_json_observed_bounded(&server, args, CURRENT_TRUTH_GH_TIMEOUT, "fixture")
        .await
        .expect_err("the observed-error interface must also fail");
    assert_eq!(failure.observed_json(), Some(&payload));
    assert!(!format!("{failure:?}").contains("PRIVATE"));
    assert_eq!(fixture.call_count(), 2);
}

#[test]
fn only_typed_visibility_at_the_expected_repository_path_can_be_retained() {
    for value in [
        json!(null),
        json!({"visibility": "PRIVATE"}),
        json!({"errors": [{"message": "PRIVATE"}]}),
        json!({"data": {"repository": {"visibility": true}}}),
        json!({"data": {"repository": {"visibility": ["PRIVATE"]}}}),
        json!({"data": {"repository": {"visibility": " private "}}}),
    ] {
        assert_eq!(graphql::repository_visibility(&value), None);
    }
    for visibility in ["PRIVATE", "private", "INTERNAL", "internal"] {
        assert_eq!(
            graphql::repository_visibility(&complete_payload(visibility)),
            Some(VisibilityClassV1::Private)
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn corrupt_private_history_has_the_same_public_response_as_denied_read() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = GhFixture::new();
    let denied_server = crate::tests::make_server();
    fixture.respond_bytes(b"", 1);
    let mut denied = refresh(&denied_server, "owner/repo").await;
    denied["posture"]["last_attempt_at"] = Value::Null;
    for visibility in ["PRIVATE", "PUBLIC"] {
        for column in ["predicate", "value_json"] {
            let server = crate::tests::make_server();
            fixture.respond(&complete_payload("PRIVATE"), 0);
            assert_private_response(&refresh(&server, "owner/repo").await);
            let secret = format!("private-corrupt-{visibility}-{column}-canary");
            let count = crate::test_support::with_unrestricted_fixture_connection(&server.global_db_path_buf(), |conn| {
                conn.execute(&format!("UPDATE current_truth_assertions SET {column} = ?1 WHERE assertion_id = (SELECT assertion_id FROM current_truth_assertions WHERE visibility = 'private' ORDER BY assertion_id LIMIT 1)"), [&secret])
            }).unwrap();
            assert!(
                count > 0,
                "the corruption must reach existing private authority rows"
            );
            let before = server
                .with_current_truth_store(|store| {
                    store
                        .assertion_count("owner/repo")
                        .map_err(|error| error.to_string())
                })
                .unwrap();
            fixture.respond(&complete_payload(visibility), 0);
            let response = handle_current_truth_refresh_with_floor(
                &server,
                &TachiGhParams {
                    action: "current_truth_refresh".to_string(),
                    repo: Some("owner/repo".to_string()),
                    number: Some(42),
                    ..TachiGhParams::default()
                },
                Duration::ZERO,
            )
            .await;
            assert!(
                !format!("{response:?}").contains(&secret),
                "private row data must not cross the handler's error boundary"
            );
            let mut response: Value = serde_json::from_str(
                &response.expect("private corruption must not become an existence-shaped error"),
            )
            .unwrap();
            assert_private_response(&response);
            response["posture"]["last_attempt_at"] = Value::Null;
            assert_eq!(
                response, denied,
                "public response shape must match a denied/missing source"
            );
            server
                .with_current_truth_store(|store| {
                    assert_eq!(
                        store.assertion_count("owner/repo").unwrap(),
                        before,
                        "failed decoding must mint no assertions"
                    );
                    let posture = store.refresh_posture_row("owner/repo").unwrap().unwrap();
                    assert!(
                        !posture.fresh,
                        "the failed fresh observation must leave refresh debt"
                    );
                    assert_eq!(
                        posture.unavailable_reason.as_deref(),
                        Some(REASON_UNAVAILABLE)
                    );
                    if visibility == "PRIVATE" {
                        assert_eq!(
                            posture.repository_visibility,
                            Some(VisibilityClassV1::Private)
                        );
                    }
                    Ok(())
                })
                .unwrap();
        }
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn privacy_metadata_and_debt_write_failures_keep_the_denied_public_shape() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = GhFixture::new();
    let denied_server = crate::tests::make_server();
    fixture.respond_bytes(b"", 1);
    let mut denied = refresh(&denied_server, "owner/repo").await;
    denied["posture"]["last_attempt_at"] = Value::Null;
    for failure in ["identity_utf8", "debt_update"] {
        let server = crate::tests::make_server();
        fixture.respond(&complete_payload("PRIVATE"), 0);
        assert_private_response(&refresh(&server, "owner/repo").await);
        let before = server
            .with_current_truth_store(|store| {
                Ok(store.refresh_posture_row("owner/repo").unwrap().unwrap())
            })
            .unwrap();
        assert!(before.fresh);
        crate::test_support::with_unrestricted_fixture_connection(&server.global_db_path_buf(), |conn| {
            if failure == "identity_utf8" {
                conn.execute("UPDATE current_truth_assertions SET subject_id = CAST(X'FF' AS TEXT)
                    WHERE assertion_id = (SELECT assertion_id FROM current_truth_assertions WHERE visibility = 'private' ORDER BY assertion_id LIMIT 1)", [])?;
            } else {
                conn.execute("UPDATE current_truth_assertions SET predicate = 'private-corrupt-debt-canary'
                    WHERE assertion_id = (SELECT assertion_id FROM current_truth_assertions WHERE visibility = 'private' ORDER BY assertion_id LIMIT 1)", [])?;
                conn.execute_batch("CREATE TRIGGER fail_private_refresh_debt BEFORE UPDATE OF fresh ON current_truth_refresh
                    WHEN NEW.fresh = 0
                    BEGIN SELECT RAISE(ABORT, 'private-debt-write-canary'); END;")?;
            }
            Ok(())
        }).unwrap();
        // PUBLIC forces the identity-only private-history lookup to decode
        // the malformed token; PRIVATE exercises the debt-update error.
        fixture.respond(
            &complete_payload(if failure == "identity_utf8" {
                "PUBLIC"
            } else {
                "PRIVATE"
            }),
            0,
        );
        let mut response = refresh(&server, "owner/repo").await;
        assert_private_response(&response);
        response["posture"]["last_attempt_at"] = Value::Null;
        assert_eq!(response, denied);
        if failure == "debt_update" {
            server.with_current_truth_store(|store| {
                let after = store.refresh_posture_row("owner/repo").unwrap().unwrap();
                assert_eq!(after.fresh, before.fresh, "the failing debt write did not persist; the public unavailable response is not a durable-write claim");
                assert_eq!(after.last_attempt_at, before.last_attempt_at);
                assert_eq!(after.last_fresh_revision, before.last_fresh_revision);
                assert_eq!(after.repository_visibility, Some(VisibilityClassV1::Private));
                Ok(())
            }).unwrap();
        }
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn corrupt_private_history_without_posture_retains_new_repository_restriction() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = GhFixture::new();
    for field in ["subject_id", "predicate", "value_json"] {
        let server = crate::tests::make_server();
        let mut public_payload = complete_payload("PUBLIC");
        public_payload["data"]["repository"]["issueOrPullRequest"]["number"] = json!(43);
        fixture.respond(&public_payload, 0);
        let public = handle_current_truth_refresh_with_floor(
            &server,
            &TachiGhParams {
                action: "current_truth_refresh".to_string(),
                repo: Some("owner/repo".to_string()),
                number: Some(43),
                ..TachiGhParams::default()
            },
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(
            public.contains("owner/repo#issue:43"),
            "seed an independent public subject before repository restriction"
        );
        fixture.respond(&complete_payload("PRIVATE"), 0);
        assert_private_response(&refresh(&server, "owner/repo").await);
        crate::test_support::with_unrestricted_fixture_connection(&server.global_db_path_buf(), |conn| {
            let corrupt_value = if field == "subject_id" { "CAST(X'FF' AS TEXT)" } else { "'private-missing-posture-canary'" };
            conn.execute(&format!("UPDATE current_truth_assertions SET {field} = {corrupt_value}
                WHERE assertion_id = (SELECT assertion_id FROM current_truth_assertions WHERE visibility = 'private' ORDER BY assertion_id LIMIT 1)"), [])?;
            assert!(conn.execute("DELETE FROM current_truth_refresh", [])? > 0);
            Ok(())
        }).unwrap();
        server
            .with_current_truth_store(|store| {
                assert!(store.refresh_posture_row("owner/repo").unwrap().is_none());
                Ok(())
            })
            .unwrap();
        fixture.respond(&complete_payload("PRIVATE"), 0);
        assert_private_response(&refresh(&server, "owner/repo").await);
        server.with_current_truth_store(|store| {
            let posture = store.refresh_posture_row("owner/repo").unwrap().expect("failed private refresh must establish debt without decoding the corrupt identity");
            assert!(!posture.fresh);
            assert_eq!(posture.repository_visibility, Some(VisibilityClassV1::Private), "a newly created debt row must receive the independently observed restriction");
            assert!(consumer::read_view(store, "owner/repo", CallerAuthorizationV1 { sees_private: false }).unwrap().subjects.is_empty(), "the repository restriction must hide the independent old public subject too");
            Ok(())
        }).unwrap();
        fixture.respond_bytes(b"", 1);
        let denied = refresh(&server, "owner/repo").await;
        assert_private_response(&denied);
        assert!(!denied.to_string().contains("issue:43"));
        server
            .with_current_truth_store(|store| {
                assert_eq!(
                    store.repository_visibility("owner/repo").unwrap(),
                    Some(VisibilityClassV1::Private)
                );
                assert!(consumer::read_view(
                    store,
                    "owner/repo",
                    CallerAuthorizationV1 {
                        sees_private: false
                    }
                )
                .unwrap()
                .subjects
                .is_empty());
                Ok(())
            })
            .unwrap();
    }
}

async fn seed_private_repo_with_public_sibling(fixture: &GhFixture, server: &MemoryServer) {
    let mut sibling = complete_payload("PUBLIC");
    sibling["data"]["repository"]["issueOrPullRequest"]["number"] = json!(43);
    fixture.respond(&sibling, 0);
    let response = handle_current_truth_refresh_with_floor(
        server,
        &TachiGhParams {
            action: "current_truth_refresh".to_string(),
            repo: Some("owner/repo".to_string()),
            number: Some(43),
            ..TachiGhParams::default()
        },
        Duration::ZERO,
    )
    .await
    .unwrap();
    assert!(response.contains("owner/repo#issue:43"));
    fixture.respond(&complete_payload("PUBLIC"), 0);
    assert_eq!(refresh(server, "owner/repo").await["fresh"], json!(true));
    fixture.respond(&complete_payload("PRIVATE"), 0);
    assert_private_response(&refresh(server, "owner/repo").await);
}

fn assert_private_repository_and_hidden_sibling(server: &MemoryServer) {
    server
        .with_current_truth_store(|store| {
            assert_eq!(
                store.repository_visibility("owner/repo").unwrap(),
                Some(VisibilityClassV1::Private),
                "failed PUBLIC observation must not relax known PRIVATE metadata"
            );
            let view = consumer::read_view(
                store,
                "owner/repo",
                CallerAuthorizationV1 {
                    sees_private: false,
                },
            )
            .unwrap();
            assert!(
                view.subjects.is_empty(),
                "independent historical PUBLIC sibling must remain hidden"
            );
            assert!(!view.posture.fresh, "failed attempt must not remain fresh");
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn failed_public_payloads_cannot_relax_private_repository() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = GhFixture::new();
    for failure in [
        "errors",
        "malformed",
        "incomplete",
        "stale",
        "contradictory",
    ] {
        let server = crate::tests::make_server();
        seed_private_repo_with_public_sibling(&fixture, &server).await;
        let mut payload = complete_payload("PUBLIC");
        match failure {
            "errors" => payload["errors"] = json!([{"message":"private-public-error-canary"}]),
            "malformed" => {
                payload["data"]["repository"]["issueOrPullRequest"]["state"] = Value::Null
            }
            "incomplete" => {
                payload["data"]["repository"]["issueOrPullRequest"]["timelineItems"]["pageInfo"]
                    ["hasNextPage"] = json!(true)
            }
            "stale" => {
                payload["data"]["repository"]["issueOrPullRequest"]["updatedAt"] =
                    json!("2026-08-01T00:00:00Z")
            }
            "contradictory" => {
                crate::test_support::with_unrestricted_fixture_connection(&server.global_db_path_buf(), |conn| {
                assert_eq!(conn.execute("UPDATE current_truth_assertions SET content_digest = 'contradictory-fixture' WHERE assertion_id = (SELECT assertion_id FROM current_truth_assertions WHERE visibility = 'public' AND subject_id = '42' ORDER BY assertion_id LIMIT 1)", [])?, 1);
                Ok(())
            }).unwrap();
            }
            _ => unreachable!(),
        }
        fixture.respond(&payload, 0);
        let response = refresh(&server, "owner/repo").await;
        assert_private_repository_and_hidden_sibling(&server);
        server
            .with_current_truth_store(|store| {
                let expected_reason = match failure {
                    "errors" => REASON_UNAVAILABLE,
                    "malformed" => REASON_MALFORMED,
                    "incomplete" => REASON_INCOMPLETE,
                    "stale" => REASON_STALE,
                    "contradictory" => REASON_CONTRADICTORY,
                    _ => unreachable!(),
                };
                assert_eq!(
                    store
                        .refresh_posture_row("owner/repo")
                        .unwrap()
                        .unwrap()
                        .unavailable_reason
                        .as_deref(),
                    Some(expected_reason),
                    "the fixture must reach its intended failure branch"
                );
                Ok(())
            })
            .unwrap();
        assert_private_response(&response);
        fixture.respond_bytes(b"", 1);
        let mut denied = refresh(&server, "owner/repo").await;
        let mut response = response;
        response["posture"]["last_attempt_at"] = Value::Null;
        denied["posture"]["last_attempt_at"] = Value::Null;
        assert_eq!(
            response, denied,
            "{failure} must have the entire denied response shape"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn complete_public_with_corrupt_private_history_cannot_relax_repository() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = GhFixture::new();
    for field in ["predicate", "value_json", "subject_id"] {
        let server = crate::tests::make_server();
        seed_private_repo_with_public_sibling(&fixture, &server).await;
        crate::test_support::with_unrestricted_fixture_connection(&server.global_db_path_buf(), |conn| {
            let value = if field == "subject_id" { "CAST(X'FF' AS TEXT)" } else { "'private-public-corruption-canary'" };
            assert_eq!(conn.execute(&format!("UPDATE current_truth_assertions SET {field} = {value} WHERE assertion_id = (SELECT assertion_id FROM current_truth_assertions WHERE visibility = 'private' ORDER BY assertion_id LIMIT 1)"), [])?, 1);
            Ok(())
        }).unwrap();
        fixture.respond(&complete_payload("PUBLIC"), 0);
        let response = refresh(&server, "owner/repo").await;
        assert_private_repository_and_hidden_sibling(&server);
        assert_private_response(&response);
        assert!(!response
            .to_string()
            .contains("private-public-corruption-canary"));
        fixture.respond_bytes(b"", 1);
        let mut denied = refresh(&server, "owner/repo").await;
        let mut response = response;
        response["posture"]["last_attempt_at"] = Value::Null;
        denied["posture"]["last_attempt_at"] = Value::Null;
        assert_eq!(response, denied);
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn completed_fresh_public_may_relax_repository_without_revealing_private_subjects() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = GhFixture::new();
    let server = crate::tests::make_server();
    seed_private_repo_with_public_sibling(&fixture, &server).await;
    fixture.respond(&complete_payload("PUBLIC"), 0);
    // Private subject history still makes the public handler receipt opaque.
    assert_private_response(&refresh(&server, "owner/repo").await);
    server
        .with_current_truth_store(|store| {
            assert_eq!(
                store.repository_visibility("owner/repo").unwrap(),
                Some(VisibilityClassV1::Public)
            );
            let view = consumer::read_view(
                store,
                "owner/repo",
                CallerAuthorizationV1 {
                    sees_private: false,
                },
            )
            .unwrap();
            assert!(view.posture.fresh);
            assert_eq!(
                view.subjects
                    .iter()
                    .map(|s| s.subject_token.as_str())
                    .collect::<Vec<_>>(),
                vec!["owner/repo#issue:43"]
            );
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn public_visibility_rolls_back_if_downstream_consumer_fails() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = GhFixture::new();
    let server = crate::tests::make_server();
    seed_private_repo_with_public_sibling(&fixture, &server).await;
    let before = server
        .with_current_truth_store(|store| Ok(store.assertions_for_repo("owner/repo").unwrap()))
        .unwrap();
    crate::test_support::with_unrestricted_fixture_connection(&server.global_db_path_buf(), |conn| {
        // This sanctioned second connection injects a read failure only once
        // PUBLIC is provisionally written, after successful source ingest.
        conn.execute_batch("CREATE TRIGGER fail_public_visibility_consumer AFTER UPDATE OF repository_visibility ON current_truth_refresh
            WHEN NEW.repository_visibility = 'public'
            BEGIN UPDATE current_truth_assertions SET predicate = 'private-consumer-fault-canary'
                WHERE assertion_id = (SELECT assertion_id FROM current_truth_assertions WHERE visibility = 'public' AND subject_id = '43' ORDER BY assertion_id LIMIT 1); END;")
    }).unwrap();
    fixture.respond(&complete_payload("PUBLIC"), 0);
    let response = refresh(&server, "owner/repo").await;
    assert_private_repository_and_hidden_sibling(&server);
    assert_private_response(&response);
    assert!(!response
        .to_string()
        .contains("private-consumer-fault-canary"));
    server.with_current_truth_store(|store| {
        assert_eq!(store.assertions_for_repo("owner/repo").unwrap(), before, "failed visibility transaction must roll back the injected authority-row mutation too");
        Ok(())
    }).unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn late_public_attempt_cannot_borrow_another_attempts_fresh_posture() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let fixture = GhFixture::new();
    let server = crate::tests::make_server();
    seed_private_repo_with_public_sibling(&fixture, &server).await;
    crate::test_support::with_unrestricted_fixture_connection(&server.global_db_path_buf(), |conn| {
        assert_eq!(conn.execute("UPDATE current_truth_refresh SET last_attempt_at = '2099-01-01T00:00:00Z' WHERE subject_token = 'owner/repo#issue:42'", [])?, 1);
        Ok(())
    }).unwrap();
    fixture.respond(&complete_payload("PUBLIC"), 0);
    assert_private_response(&refresh(&server, "owner/repo").await);
    server
        .with_current_truth_store(|store| {
            let posture = store.refresh_posture_row("owner/repo").unwrap().unwrap();
            assert!(posture.fresh, "preserve the later committed posture");
            assert_eq!(posture.last_attempt_at, "2099-01-01T00:00:00Z");
            assert_eq!(
                posture.repository_visibility,
                Some(VisibilityClassV1::Private)
            );
            assert!(consumer::read_view(
                store,
                "owner/repo",
                CallerAuthorizationV1 {
                    sees_private: false
                }
            )
            .unwrap()
            .subjects
            .is_empty());
            Ok(())
        })
        .unwrap();
}
