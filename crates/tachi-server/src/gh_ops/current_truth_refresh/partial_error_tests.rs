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
    assert_eq!(response["posture"]["unavailable_reason"], REASON_UNAVAILABLE);
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
    let failure =
        run_gh_json_observed_bounded(&server, args, CURRENT_TRUTH_GH_TIMEOUT, "fixture")
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
