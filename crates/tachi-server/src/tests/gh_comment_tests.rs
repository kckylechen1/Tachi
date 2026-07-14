use super::make_server;
use serde_json::{json, Value};
use std::path::Path;

struct PathEnvGuard {
    original: Option<std::ffi::OsString>,
}

impl PathEnvGuard {
    fn prepend(dir: &Path) -> Self {
        let original = std::env::var_os("PATH");
        let mut paths = vec![dir.to_path_buf()];
        if let Some(value) = original.as_ref() {
            paths.extend(std::env::split_paths(value));
        }
        let joined = std::env::join_paths(paths).expect("join PATH");
        std::env::set_var("PATH", joined);
        Self { original }
    }
}

impl Drop for PathEnvGuard {
    fn drop(&mut self) {
        if let Some(path) = self.original.as_ref() {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
    }
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .expect("shim metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod shim");
    }
}

/// Phase 1: the write-back verb. dry_run must preview the comment body and
/// NOT touch the network (returns before `gh` is ever invoked).
#[tokio::test]
async fn gh_issue_comment_dry_run_previews_without_posting() {
    let server = make_server();
    let resp = crate::gh_ops::handle_gh_comment(
        &server,
        "issue",
        crate::tool_params::GhCommentParams {
            repo: "owner/repo".to_string(),
            number: 42,
            body: Some("hello from close_loop".to_string()),
            dry_run: true,
        },
    )
    .await
    .expect("dry-run comment should succeed offline");
    let json: Value = serde_json::from_str(&resp).expect("json");
    assert_eq!(json["dry_run"], json!(true));
    assert_eq!(json["number"], json!(42));
    assert_eq!(json["preview_body"], json!("hello from close_loop"));
    assert_eq!(json["tool"], json!("tachi_gh_issue_comment"));
}

/// A comment with no body is a usage error, surfaced before any gh call.
#[tokio::test]
async fn gh_comment_rejects_empty_body() {
    let server = make_server();
    let err = crate::gh_ops::handle_gh_comment(
        &server,
        "pr",
        crate::tool_params::GhCommentParams {
            repo: "owner/repo".to_string(),
            number: 7,
            body: None,
            dry_run: true,
        },
    )
    .await
    .unwrap_err();
    assert!(err.contains("non-empty 'body'"), "got: {err}");
}

/// gh shim parity: comment verbs must pass body via `--body-file`, never inline `--body`.
#[tokio::test]
async fn gh_comment_uses_body_file_not_inline_body() {
    let fake_bin = tempfile::tempdir().expect("fake bin");
    let gh_path = fake_bin.path().join("gh");
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _path = {
        write_executable(
            &gh_path,
            "#!/bin/sh\n\
             while [ $# -gt 0 ]; do\n\
               if [ \"$1\" = \"--body\" ]; then\n\
                 echo \"rejected: inline --body\" >&2\n\
                 exit 2\n\
               fi\n\
               if [ \"$1\" = \"--body-file\" ]; then\n\
                 shift\n\
                 cat \"$1\"\n\
                 exit 0\n\
               fi\n\
               shift\n\
             done\n\
             echo \"missing --body-file\" >&2\n\
             exit 1\n",
        );
        PathEnvGuard::prepend(fake_bin.path())
    };

    let server = make_server();
    let body = "verbatim\ncomment body";
    let resp = crate::gh_ops::handle_gh_comment(
        &server,
        "issue",
        crate::tool_params::GhCommentParams {
            repo: "owner/repo".to_string(),
            number: 99,
            body: Some(body.to_string()),
            dry_run: false,
        },
    )
    .await
    .expect("shim accepting --body-file should succeed");
    let json: Value = serde_json::from_str(&resp).expect("json");
    assert_eq!(json["tool"], json!("tachi_gh_issue_comment"));
    assert_eq!(json["result"], json!(body));
}
