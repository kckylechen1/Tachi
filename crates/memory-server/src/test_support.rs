use std::path::{Path, PathBuf};

/// Create repo-local DB fixtures outside OS temp roots. Production skip logic
/// intentionally drops `/.tachi/memory.db` under `/tmp` and `/private/tmp`.
pub(crate) fn non_skipped_fixture_tempdir(prefix: &str) -> tempfile::TempDir {
    let base = non_skipped_fixture_base().join("repo-local-db-fixtures");
    std::fs::create_dir_all(&base).expect("repo-local DB fixture base");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&base)
        .expect("repo-local DB fixture tempdir")
}

fn non_skipped_fixture_base() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_target = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(|repo| repo.join("target"));

    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain(repo_target)
        .chain(dirs::home_dir().map(|home| home.join(".cache/sigil-repo-local-db-fixtures")))
        .find(|candidate| !has_tmp_skip_prefix(candidate))
        .unwrap_or_else(|| PathBuf::from("target"))
}

fn has_tmp_skip_prefix(path: &Path) -> bool {
    let path_lower = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    path_lower.starts_with("/tmp/") || path_lower.starts_with("/private/tmp/")
}

pub(crate) fn assert_repo_local_db_fixture_not_skipped(path: &Path) {
    assert!(
        crate::manifest::should_skip_path(path).is_none(),
        "repo-local DB test fixture must not be hidden by temporary-workspace skip rules: {}",
        path.display()
    );
}

/// RAII guard that restores an environment variable to its prior value on drop.
///
/// Each `EnvRestore` captures the original value (if any) at construction and
/// restores it when dropped, so a test that mutates a process-wide env var
/// cannot leak that change into sibling tests. Holds the value as an
/// `OsString` (the more general form) so it round-trips any valid env value.
///
/// Tests that mutate env vars MUST either use unique var names per test or
/// hold [`crate::utils::global_test_lock`] for the guard's lifetime, because
/// cargo runs `#[test]` fns in parallel by default.
pub(crate) struct EnvRestore {
    key: &'static str,
    old: Option<std::ffi::OsString>,
}

impl EnvRestore {
    /// Set `key` to `value`, returning a guard that restores the prior value.
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, old }
    }

    /// Remove `key`, returning a guard that restores the prior value.
    pub(crate) fn remove(key: &'static str) -> Self {
        let old = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, old }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        if let Some(value) = self.old.as_ref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

/// Minimal OpenAPI document used by OpenCode harness-probe tests across
/// `dispatch_ops` and `dispatch_profile`. Centralised so the three call sites
/// that previously carried byte-identical copies share one definition.
pub(crate) const OPENCODE_DOC_FIXTURE: &str = r#"{
    "openapi":"3.1.0",
    "info":{"title":"opencode","version":"1.0.0"},
    "paths":{
        "/api/session":{"post":{}},
        "/api/session/{sessionID}/prompt":{"post":{}},
        "/api/session/{sessionID}/wait":{"post":{}},
        "/api/model":{"get":{}},
        "/api/provider":{"get":{}}
    }
}"#;

/// Spawn a one-shot TCP server that replies to `GET /doc` with
/// [`OPENCODE_DOC_FIXTURE`] and any other request with an OpenCode-style
/// `<title>OpenCode</title>` body. Returns the server URL and the thread
/// handle (caller must `join` it before asserting).
///
/// Serves exactly two requests: the doc fetch and the subsequent probe. This
/// matches the handshake the harness status probe performs (doc → session
/// smoke). Used by the OpenCode transport/profile tests.
pub(crate) fn spawn_opencode_probe_server() -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe server");
    let port = listener.local_addr().expect("local addr").port();
    let handle = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept probe");
            let mut buf = [0_u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let body = if request.starts_with("GET /doc ") {
                OPENCODE_DOC_FIXTURE
            } else {
                "<title>OpenCode</title>"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
    });
    (format!("http://127.0.0.1:{port}"), handle)
}
