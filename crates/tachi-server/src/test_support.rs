use std::path::{Path, PathBuf};

/// Run deliberate fixture DDL on an unguarded, offline connection.
///
/// This exists only because a normal [`memcore::MemoryStore`] connection
/// correctly rejects arbitrary schema mutation. Do not use it for `memories`
/// row writes or authority-column updates; those must go through typed store
/// APIs so the v23 authorization boundary remains exercised.
pub(crate) fn with_unrestricted_fixture_connection<T>(
    path: &Path,
    operation: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let connection = rusqlite::Connection::open(path)?;
    operation(&connection)
}

/// Create repo-local DB fixtures outside OS temp roots and git worktrees.
/// Production skip logic intentionally drops `/.tachi/memory.db` under
/// `/tmp` and `/private/tmp`; root-resolution tests additionally need a path
/// that cannot walk upward into a repository through Cargo's in-tree target.
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
        .map(absolutize)
        .find(|candidate| !has_tmp_skip_prefix(candidate) && !has_git_ancestor(candidate))
        .expect("repo-local DB tests need a fixture base outside temp roots and git worktrees")
}

/// Resolve a candidate to an absolute path *before* it is fed to
/// [`has_git_ancestor`]. `CARGO_TARGET_DIR` can legally hold a relative value
/// (e.g. `target/fixture-base`); this crate's own build convention points it
/// at a shared external cache (see repo `AGENTS.md`), so a checkout routinely
/// has no local `target/` directory at all. When that's true, `has_git_ancestor`'s
/// `path.ancestors().find(|c| c.exists())` walk — run on the relative path
/// exactly as given — finds nothing that exists to canonicalize and reports
/// "no git ancestor", even though the same relative path resolves squarely
/// inside the current git worktree once `std::fs::create_dir_all` joins it
/// against the process cwd at use time. Absolutizing against cwd first makes
/// the ancestor walk see the same directories the filesystem will actually
/// use, so an existing ancestor (at minimum cwd itself) is always available
/// to canonicalize and check for `.git`.
fn absolutize(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

fn has_tmp_skip_prefix(path: &Path) -> bool {
    let path_lower = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    path_lower.starts_with("/tmp/") || path_lower.starts_with("/private/tmp/")
}

fn has_git_ancestor(path: &Path) -> bool {
    path.ancestors()
        .find(|candidate| candidate.exists())
        .and_then(|candidate| crate::utils::find_git_root_from(candidate))
        .is_some()
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

    /// Set `key` to a filesystem path, returning a guard that restores the
    /// prior value. #1096 leaf-2a: added so the ~16 hand-rolled
    /// `EnvGuard`/`EnvVarGuard` test structs scattered across this crate
    /// (each a byte-for-byte-near-identical copy of this one, several with a
    /// `set_path` constructor `set` alone can't express without an extra
    /// `.to_str()` round-trip) have one shared implementation to migrate to.
    pub(crate) fn set_path(key: &'static str, value: &Path) -> Self {
        let old = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, old }
    }

    /// Set `key` to a raw `OsStr` value, returning a guard that restores the
    /// prior value. For values that are not guaranteed UTF-8 (e.g. a PATH
    /// rebuilt via `join_paths`) — `set`'s `&str` parameter would force a
    /// lossy/panicking conversion that the old hand-rolled guards never did.
    pub(crate) fn set_os(key: &'static str, value: &std::ffi::OsStr) -> Self {
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

/// RAII guard that restores the process current directory to its prior
/// value on drop.
///
/// Same class of bug as [`EnvRestore`]/`with_tachi_home`'s panic-safety fix
/// (#1096 leaf-2a), but for `std::env::current_dir` instead of an env var:
/// a test that does a bare `set_current_dir(new)` ... `set_current_dir(old)`
/// leaks the changed cwd into every test that runs afterward in the same
/// process if an assertion between the two calls panics — the restore
/// statement is simply never reached. Process cwd is global mutable state
/// shared by the whole test binary, so that leak doesn't just corrupt the
/// panicking test's own next run; it can make an unrelated, otherwise-correct
/// test fail later in the same `cargo test` invocation (e.g. a cwd-walking
/// git-root resolver, or `std::env::current_dir()` itself erroring because
/// the leaked directory was a tempdir that has since been deleted). Restoring
/// via `Drop` survives unwinding, so the original cwd comes back regardless
/// of how the guarded section exits.
pub(crate) struct CwdRestore {
    original: PathBuf,
}

impl CwdRestore {
    /// Switch to `path`, returning a guard that restores the original cwd
    /// (captured at construction, before the switch) when dropped.
    pub(crate) fn set(path: &Path) -> Self {
        let original = std::env::current_dir().expect("capture current dir");
        std::env::set_current_dir(path).expect("set current dir");
        Self { original }
    }
}

impl Drop for CwdRestore {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

/// Bind a fresh, isolated Tachi home directory for the duration of `f`.
///
/// #1096 leaf-2a: this crate had THREE independent local copies of this
/// exact helper (`complete_ops::dispatch_outcome`, `bootstrap::serve::stdio`,
/// `memory_search_ops::routing_config`), none panic-safe — each restored env
/// with plain post-call statements, so a panic inside `f` would skip
/// restoration and leak the temp `TACHI_HOME` into every test that runs
/// after it in the same process. This version restores via `EnvRestore`'s
/// `Drop`, which also runs during unwinding, so it survives an assertion
/// failure inside `f`. Also clears `SIGIL_HOME`/`TACHI_APP_HOME` (only one of
/// the three prior copies did) so no ambient override from either of the
/// funnel's other two keys leaks into the isolated call.
///
/// Holds [`crate::utils::global_test_lock`] for the duration of `f` — same
/// convention the three copies this replaces already followed.
pub(crate) fn with_tachi_home<T>(f: impl FnOnce(&Path) -> T) -> T {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().expect("tachi home tempdir");
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", temp.path());
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");
    f(temp.path())
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

#[cfg(test)]
mod tests {
    use super::{non_skipped_fixture_base, with_tachi_home, CwdRestore, EnvRestore};
    use std::path::Path;

    /// #1096 leaf-2a round-2 (codex C5): pins the panic-safety claim in
    /// `with_tachi_home`'s doc comment. A closure that panics inside `f`
    /// must still leave `TACHI_HOME`/`SIGIL_HOME`/`TACHI_APP_HOME` restored
    /// to their pre-call values — that's the entire reason this helper
    /// restores via `EnvRestore`'s `Drop` instead of a postlude statement.
    /// `catch_unwind` lets this test observe the post-panic env state
    /// in-process instead of just trusting the Drop impl reads correctly.
    ///
    /// Deliberately does NOT wrap the whole test in `global_test_lock()`:
    /// `with_tachi_home` acquires that same (non-reentrant) `std::sync::Mutex`
    /// itself for the swap-and-restore critical section, so holding it here
    /// across the call would deadlock the test on its own thread. The
    /// sentinel set/read around the call is the same brief, lock-free window
    /// every other caller in this crate already accepts immediately before
    /// and after a `with_tachi_home`/`EnvRestore`-guarded critical section.
    #[test]
    fn with_tachi_home_restores_all_three_keys_after_panic() {
        // Seed pre-call sentinel values so we can prove restoration, not
        // just absence.
        let _pre_tachi = EnvRestore::set("TACHI_HOME", "/sentinel/pre-call-tachi-home");
        let _pre_sigil = EnvRestore::set("SIGIL_HOME", "/sentinel/pre-call-sigil-home");
        let _pre_app = EnvRestore::set("TACHI_APP_HOME", "/sentinel/pre-call-app-home");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_tachi_home(|_home| {
                panic!("intentional panic inside with_tachi_home closure");
            })
        }));
        assert!(
            result.is_err(),
            "the inner closure must have actually panicked for this test to mean anything"
        );

        assert_eq!(
            std::env::var("TACHI_HOME").as_deref(),
            Ok("/sentinel/pre-call-tachi-home"),
            "TACHI_HOME must be restored to its pre-call value even after a panic"
        );
        assert_eq!(
            std::env::var("SIGIL_HOME").as_deref(),
            Ok("/sentinel/pre-call-sigil-home"),
            "SIGIL_HOME must be restored to its pre-call value even after a panic"
        );
        assert_eq!(
            std::env::var("TACHI_APP_HOME").as_deref(),
            Ok("/sentinel/pre-call-app-home"),
            "TACHI_APP_HOME must be restored to its pre-call value even after a panic"
        );
    }

    /// #1272 cross-vendor review: `has_git_ancestor`'s ancestor walk ran on
    /// the candidate exactly as given, so a *relative* `CARGO_TARGET_DIR`
    /// (e.g. `target/fixture-base`) whose components don't exist yet at
    /// check time found no existing ancestor to canonicalize and reported
    /// "no git ancestor" — even when the same relative path resolves inside
    /// the current git worktree once `create_dir_all` joins it against cwd.
    /// `project_db_ops::rejects_path_outside_git_repo_when_cargo_target_dir_is_in_a_git_repo`
    /// only exercised a pre-created ABSOLUTE `CARGO_TARGET_DIR`, so it never
    /// caught this. Repro: cwd a fake git repo with no local `target/` dir
    /// (this crate's own convention — `CARGO_TARGET_DIR` points at a shared
    /// external cache, see repo `AGENTS.md`), set a relative
    /// `CARGO_TARGET_DIR`, and prove the chosen fixture base does not
    /// resolve inside that repo.
    #[test]
    fn relative_cargo_target_dir_resolving_inside_a_git_repo_is_rejected() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let sandbox = tempfile::tempdir().expect("sandbox");
        let repo = sandbox.path().join("fixture-repo");
        std::fs::create_dir_all(repo.join(".git")).expect("fake git repo");
        let _cwd = CwdRestore::set(&repo);

        let relative_candidate = Path::new("target/fixture-base");
        assert!(
            !relative_candidate.exists()
                && !relative_candidate.parent().expect("has parent").exists(),
            "fixture must exercise the case where no ancestor of the relative \
             candidate exists yet at check time — that's the exact condition \
             that let it slip past the pre-fix ancestor walk"
        );
        let _cargo_target = EnvRestore::set_path("CARGO_TARGET_DIR", relative_candidate);

        let base = non_skipped_fixture_base();
        let resolved = if base.is_absolute() {
            base.clone()
        } else {
            repo.join(&base)
        };
        assert!(
            !resolved.starts_with(&repo),
            "a relative CARGO_TARGET_DIR that resolves inside a git repo via \
             cwd must not be selected as the fixture base, got: {}",
            base.display()
        );
    }
}
