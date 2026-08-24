use crate::server_state::MemoryServer;
use chrono::Utc;
use memcore::{HubCapability, MemoryEntry, MemoryStore, MigrationAuthority};
use rusqlite::params;
use serde_json::json;

fn ensure_test_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if std::env::var_os("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST").is_none() {
            std::env::set_var("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
        }
        // Unit tests exercise local search semantics. Do not let a real or
        // placeholder Voyage key turn those tests into network/provider-health
        // tests; vector-specific tests pass explicit query vectors.
        std::env::set_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
        std::env::set_var("TACHI_TEST_DISABLE_RECALL_CONFIG", "1");
        std::env::set_var("TACHI_WIKI_INGEST_ALLOW_ANY_LOCAL_FILE", "1");
        // close_loop's wiki drafting prefers a backend-model distill; force the
        // deterministic result.md fallback in tests so the suite never makes a
        // network call. (Real LLM drafting is exercised in production.)
        std::env::set_var("TACHI_DISABLE_LLM_DRAFT", "1");
        // Tests use a single global DB and seed paths across the canonical
        // layout (wiki, project, etc.). Disable path-routing validation so
        // those fixtures don't have to opt into cross-project routing.
        std::env::set_var("TACHI_DISABLE_PATH_VALIDATION", "1");
    });
}

/// Wait for a generated timestamp to enter a different UTC second.
///
/// The monotonic deadline keeps a frozen clock from hanging a test forever;
/// any changed second counts as progress, including a backward clock jump.
pub(crate) async fn wait_for_distinct_utc_second() {
    const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
    let initial_second = Utc::now().timestamp();
    let deadline = std::time::Instant::now() + MAX_WAIT;
    loop {
        if std::time::Instant::now() >= deadline {
            panic!(
                "UTC timestamp second did not change within {MAX_WAIT:?}; initial_second={initial_second}"
            );
        }
        if Utc::now().timestamp() != initial_second {
            return;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        tokio::time::sleep(remaining.min(std::time::Duration::from_millis(10))).await;
    }
}

fn home_test_lock() -> &'static std::sync::Mutex<()> {
    crate::utils::global_test_lock()
}

/// Tests that spawn real subprocesses sensitive to `HOME` (e.g. `npx`, which
/// reads `~/.npm` for cache and registry config) MUST acquire this lock.
/// Otherwise a concurrent `TempHomeGuard` (used by other tests) can repoint
/// `HOME` mid-spawn, breaking the subprocess in non-deterministic ways.
fn acquire_real_home_lock() -> std::sync::MutexGuard<'static, ()> {
    home_test_lock().lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) struct TempHomeGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    original_home: Option<std::ffi::OsString>,
    original_tachi_home: Option<std::ffi::OsString>,
    original_tachi_run_root: Option<std::ffi::OsString>,
    original_sigil_home: Option<std::ffi::OsString>,
    temp_home: std::path::PathBuf,
}

impl TempHomeGuard {
    fn new() -> Self {
        let lock = home_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let original_home = std::env::var_os("HOME");
        let original_tachi_home = std::env::var_os("TACHI_HOME");
        let original_tachi_run_root = std::env::var_os("TACHI_RUN_ROOT");
        let original_sigil_home = std::env::var_os("SIGIL_HOME");
        let temp_home =
            crate::utils::test_fixture_path(format!("tachi-test-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_home).expect("create temp home");
        std::env::set_var("HOME", &temp_home);
        std::env::set_var("TACHI_HOME", temp_home.join(".tachi"));
        std::env::set_var("TACHI_RUN_ROOT", temp_home.join(".tachi/runs"));
        std::env::remove_var("SIGIL_HOME");
        Self {
            _lock: lock,
            original_home,
            original_tachi_home,
            original_tachi_run_root,
            original_sigil_home,
            temp_home,
        }
    }
}

impl Drop for TempHomeGuard {
    fn drop(&mut self) {
        if let Some(home) = self.original_home.as_ref() {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(value) = self.original_tachi_home.as_ref() {
            std::env::set_var("TACHI_HOME", value);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
        if let Some(value) = self.original_tachi_run_root.as_ref() {
            std::env::set_var("TACHI_RUN_ROOT", value);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
        if let Some(value) = self.original_sigil_home.as_ref() {
            std::env::set_var("SIGIL_HOME", value);
        } else {
            std::env::remove_var("SIGIL_HOME");
        }
        let _ = std::fs::remove_dir_all(&self.temp_home);
    }
}

/// Test-only wrapper that deletes the temporary global/project SQLite fixture
/// directory (including sidecars) when the test scope ends. Historically
/// `make_server` handed back a bare `MemoryServer` and the temp file was never
/// removed, so every test run leaked a `memory-server-test-*.sqlite` into the
/// system temp dir (25k+ files / ~23 GB observed on a dev machine). Deref lets
/// the ~250 existing `server.method()` call sites keep working unchanged; the inner
/// server is held in an `Option` so consumers that need ownership (e.g.
/// `call_tool_via_server`) can `take()` it while cleanup still runs on drop.
pub(crate) struct TestServer {
    server: Option<MemoryServer>,
    fixture_root: std::path::PathBuf,
}

impl TestServer {
    pub(crate) fn replace_llm(&mut self, llm: tachi_llm::LlmClient) {
        self.server
            .as_mut()
            .expect("TestServer used after its inner server was taken")
            .llm = std::sync::Arc::new(llm);
    }
}

impl std::ops::Deref for TestServer {
    type Target = MemoryServer;
    fn deref(&self) -> &Self::Target {
        self.server
            .as_ref()
            .expect("TestServer used after its inner server was taken")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Drop the server first so the SQLite connection closes before we
        // remove the files (otherwise an open handle can recreate the WAL).
        let _ = self.server.take();
        let _ = std::fs::remove_dir_all(&self.fixture_root);
    }
}

/// Path to a schema/migrations-only SQLite file, built once per test executable
/// artifact, that every `make_server*` call below copies from instead of
/// paying `MemoryServer::new`'s full DDL + `ensure_column` + data-migration
/// chain on a brand-new empty file. Constructor-seeded hub capabilities and
/// sandbox policies are removed before publication so each copied store gets
/// only the seed state appropriate to its role when the server opens it.
/// `run_data_migrations` is already
/// exercised twice-in-a-row by memcore's own migration tests (see
/// `crates/memcore/src/db/migrations.rs`), so re-running the same
/// startup path against an already-migrated file is a normal, supported
/// state (identical to a real restart against an existing `~/.tachi` DB) —
/// not a special case invented for this fixture (issue #682 template-DB
/// fixture, G2).
/// The template lives outside `test_fixture_root` at a stable path keyed by
/// identity compiled into the running image: Git SHA, package version, schema
/// version, and embedded template/schema/migration source bytes. `current_exe`
/// selects only the cache parent; mutable executable-path metadata does not
/// define identity. Nextest runs each test in its own process, so a
/// process-local cache would rebuild the template for every test. The
/// guard-only environment override gives cross-process regression tests an
/// isolated cache without changing the production default.
struct TemplateDbCache {
    path: std::path::PathBuf,
    _use_lock: TemplateCacheUseLock,
}

macro_rules! embedded_template_source {
    ($path:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), $path))
    };
}

// `GIT_SHA` is `"unknown"` in source archives and stays unchanged for dirty
// local builds. These bytes are compiled into the running image, so changes
// to the template builder or complete schema/migration chain still select a
// new cache identity without reopening the mutable executable pathname.
const TEMPLATE_IDENTITY_SOURCE_BLOBS: &[&[u8]] = &[
    embedded_template_source!("/src/tests/mod.rs"),
    embedded_template_source!("/src/server_state/init.rs"),
    embedded_template_source!("/src/builtins.rs"),
    embedded_template_source!("/src/builtins/coding.rs"),
    embedded_template_source!("/src/builtins/helpers.rs"),
    embedded_template_source!("/src/builtins/mcp.rs"),
    embedded_template_source!("/src/builtins/seed.rs"),
    embedded_template_source!("/src/builtins/superpowers.rs"),
    embedded_template_source!("/src/builtins/trading.rs"),
    embedded_template_source!("/src/builtins/waza.rs"),
    embedded_template_source!("/../memcore/src/db/store_profile.rs"),
    embedded_template_source!("/../memcore/src/db/schema.rs"),
    embedded_template_source!("/../memcore/src/db/schema/ddl.rs"),
    embedded_template_source!("/../memcore/src/db/migrations.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/a2a_body_retention.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/basic.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/cross_db.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/dispatch_adjudications.rs"),
    embedded_template_source!(
        "/../memcore/src/db/migrations/dispatch_outcomes_attribution_basis.rs"
    ),
    embedded_template_source!(
        "/../memcore/src/db/migrations/dispatch_outcomes_identity_receipt.rs"
    ),
    embedded_template_source!("/../memcore/src/db/migrations/dispatch_outcomes_reported.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/domain_retire.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/exec_env_class.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/hard_state_index.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/harness_session_attachments.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/identity_workclaim_spine.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/idless_identity.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/legacy_columns.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/mirror_eval.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/pack_retire.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/sentinel.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/session_claims_identity.rs"),
    embedded_template_source!("/../memcore/src/db/migrations/symbolic_fts.rs"),
];

fn extend_template_cache_fingerprint(hash: &mut u64, bytes: &[u8]) {
    // Deterministic FNV-1a with component lengths to preserve boundaries.
    for byte in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn template_cache_fingerprint_for_sources(sources: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    extend_template_cache_fingerprint(&mut hash, crate::build_info::GIT_SHA.as_bytes());
    extend_template_cache_fingerprint(&mut hash, env!("CARGO_PKG_VERSION").as_bytes());
    extend_template_cache_fingerprint(
        &mut hash,
        &memcore::db::migrations::EXPECTED_SCHEMA_VERSION.to_le_bytes(),
    );
    for source in sources {
        extend_template_cache_fingerprint(&mut hash, source);
    }
    hash
}

fn template_cache_fingerprint() -> u64 {
    template_cache_fingerprint_for_sources(TEMPLATE_IDENTITY_SOURCE_BLOBS)
}

#[derive(Debug, PartialEq, Eq)]
enum PublishedTemplateState {
    Missing,
    RegularFile,
}

fn published_template_state(path: &std::path::Path) -> std::io::Result<PublishedTemplateState> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(PublishedTemplateState::RegularFile),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "test template cache path is not a direct regular file: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PublishedTemplateState::Missing)
        }
        Err(error) => Err(template_cache_io_error(
            "inspect published test template",
            path,
            error,
        )),
    }
}

fn template_db_path() -> &'static std::path::PathBuf {
    static TEMPLATE: std::sync::OnceLock<TemplateDbCache> = std::sync::OnceLock::new();
    let cache = TEMPLATE.get_or_init(|| {
        ensure_test_env();
        let executable = std::env::current_exe().expect("test executable path");
        let fingerprint = template_cache_fingerprint();
        let cache_root = std::env::var_os(TEMPLATE_CACHE_ROOT_ENV)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                executable
                    .parent()
                    .expect("test executable parent")
                    .join(".tachi-server-template-cache")
            });
        std::fs::create_dir_all(&cache_root).expect("create test template cache directory");
        let path = cache_root.join(format!(
            "{TEMPLATE_PUBLISHED_PREFIX}{fingerprint:016x}.sqlite"
        ));
        let mut use_lock = TemplateCacheUseLock::acquire(&cache_root)
            .expect("acquire test template cache use lock");
        use_lock
            .reap_stale_entries(
                &cache_root,
                &path,
                TEMPLATE_CACHE_MAX_AGE,
                std::time::SystemTime::now(),
            )
            .expect("reap stale test template cache entries");
        use_lock
            .retain_shared()
            .expect("retain shared test template cache use lock");
        // Lock ordering is global use lock (shared for process lifetime), then
        // the one persistent builder-election lock. Cleanup needs the global
        // exclusive lock and never takes the builder lock, so there is no
        // reverse edge. Every worker re-checks the direct entry only after
        // winning builder election.
        let builder_lock = acquire_template_builder_lock(&cache_root)
            .expect("acquire test template builder-election lock");
        match published_template_state(&path).unwrap_or_else(|error| {
            panic!(
                "validate published test template {}: {error}",
                path.display()
            )
        }) {
            PublishedTemplateState::RegularFile => {
                return TemplateDbCache {
                    path,
                    _use_lock: use_lock,
                };
            }
            PublishedTemplateState::Missing => {}
        }

        // Build at a unique scratch path first, then atomically publish into
        // place so no process observes a half-written template. The persistent
        // builder lock serializes all fingerprints without leaking one lock
        // file per rebuild; the re-check above makes exactly one cold worker
        // perform this work.
        let build = cache_root.join(format!(
            "{TEMPLATE_BUILD_PREFIX}{}-{}.sqlite",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        {
            let server =
                MemoryServer::new(build.clone(), None).expect("build template test db fixture");
            // Fold the WAL back into the main file and truncate it so a plain
            // `fs::copy` of the base path is a complete, self-contained
            // snapshot — no `-wal`/`-shm` sidecars required.
            server
                .db
                .global_store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .checkpoint_wal_truncate()
                .expect("checkpoint template test db fixture");
        } // `server` (and its connections) drop here before the rename.
        sanitize_template_for_copy(&build);
        for suffix in ["-wal", "-shm", ".migration-marker"] {
            let mut sidecar = build.clone().into_os_string();
            sidecar.push(suffix);
            let sidecar = std::path::PathBuf::from(sidecar);
            match std::fs::remove_file(&sidecar) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!(
                    "remove test template sidecar {}: {error}",
                    sidecar.display()
                ),
            }
        }
        if let Some(receipt_path) = std::env::var_os(TEMPLATE_BUILD_RECEIPT_ENV) {
            use std::io::Write;
            let receipt_path = std::path::PathBuf::from(receipt_path);
            let mut receipt = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&receipt_path)
                .unwrap_or_else(|error| {
                    panic!(
                        "open template build receipt {}: {error}",
                        receipt_path.display()
                    )
                });
            writeln!(
                receipt,
                "pid={} build={}",
                std::process::id(),
                build.display()
            )
            .unwrap_or_else(|error| {
                panic!(
                    "write template build receipt {}: {error}",
                    receipt_path.display()
                )
            });
        }
        match std::fs::rename(&build, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match published_template_state(&path).unwrap_or_else(|state_error| {
                    panic!(
                        "validate template publish race {}: {state_error}",
                        path.display()
                    )
                }) {
                    PublishedTemplateState::RegularFile => {}
                    PublishedTemplateState::Missing => panic!(
                        "template publish reported AlreadyExists but path is missing: {}",
                        path.display()
                    ),
                }
                std::fs::remove_file(&build).expect("remove losing concurrent test template build");
            }
            Err(error) => panic!("publish test template {}: {error}", path.display()),
        }
        drop(builder_lock);
        TemplateDbCache {
            path,
            _use_lock: use_lock,
        }
    });
    &cache.path
}

/// Strip constructor-seeded state and the template's `store_identity/role`
/// stamp before publication.
///
/// The template is a schema/migrations **shape**, but it is produced by a real
/// `MemoryServer::new`, and that constructor legitimately confers the role
/// `"global"` and seeds builtin hub capabilities and sandbox policies in its
/// own global store (`server_state/init.rs`). Since
/// tachi#1579 that conferral is a write-once row *inside the file*, so
/// [`copy_template_db`]'s `fs::copy` would clone one server's identity into
/// every fixture database below — including project and named-project stores.
/// A later open that declares those stores' real roles then fails the open
/// with `StoreRoleConflict` against a role no fixture ever meant to confer.
/// That is the stamp working as designed: cloning a store's bytes into a new
/// role IS the forgery #1579 refuses to resolve silently.
///
/// So the clone source carries no role, exactly like the documented operator
/// unstamp procedure (#1585 D6), and each copy takes its identity from its
/// own FIRST declared open — the global fixture from `MemoryServer::new`, a
/// named-project fixture from the manifest-resolved project name — which is
/// what a real database does. The `store_identity/profile` row deliberately
/// stays: every copy really is the `tachi_full` shape the template was built
/// with, and profile is not a per-copy fact.
///
/// This scratch database is brand-new and dedicated to the test template, so
/// whole-table deletion is intentional: a published template must contain no
/// cached builtin rows that a copied project store could use to shadow the
/// freshly seeded global definitions.
fn sanitize_template_for_copy(build: &std::path::Path) {
    let mut conn =
        rusqlite::Connection::open(build).expect("open template test db fixture for sanitizing");
    let tx = conn
        .transaction()
        .expect("begin template test db sanitization");
    tx.execute("DELETE FROM sandbox_policies", [])
        .expect("clear template sandbox policies");
    tx.execute("DELETE FROM hub_capabilities", [])
        .expect("clear template hub capabilities");
    tx.execute(
        "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
        params![
            memcore::db::store_profile::STORE_IDENTITY_NAMESPACE,
            memcore::db::store_profile::STORE_ROLE_KEY
        ],
    )
    .expect("clear template store-identity role stamp");
    tx.commit().expect("commit template test db sanitization");
    // Fold these writes into the base file too: the caller removes the `-wal`
    // sidecar next, so unflushed deletes would be silently discarded and the
    // published template would still carry constructor seed state or its role.
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .expect("checkpoint template test db fixture after sanitizing");
}

/// Copy the schema/migrations-only template into `dest` so the caller's
/// subsequent `MemoryServer::new` finds an already-migrated file. The
/// template is checkpointed with `wal_checkpoint(TRUNCATE)` before publish,
/// so the base file alone is a complete snapshot (no sidecars to copy), and
/// carries neither constructor-seeded hub/sandbox rows nor a `store_identity`
/// role (see [`sanitize_template_for_copy`]).
fn copy_template_db(dest: &std::path::Path) {
    std::fs::copy(template_db_path(), dest).expect("seed test db from template fixture");
}

const TEMPLATE_CACHE_ROOT_ENV: &str = "TACHI_TEST_TEMPLATE_CACHE_ROOT";
const TEMPLATE_GUARD_HELPER_ENV: &str = "TACHI_TEST_TEMPLATE_GUARD_HELPER";
const TEMPLATE_GUARD_OUTPUT_ENV: &str = "TACHI_TEST_TEMPLATE_GUARD_OUTPUT";
const TEMPLATE_BUILD_RECEIPT_ENV: &str = "TACHI_TEST_TEMPLATE_BUILD_RECEIPT";
const TEMPLATE_LIFETIME_HELPER_ENV: &str = "TACHI_TEST_TEMPLATE_LIFETIME_HELPER";
const TEMPLATE_LIFETIME_ROOT_ENV: &str = "TACHI_TEST_TEMPLATE_LIFETIME_ROOT";
const TEMPLATE_LIFETIME_NOW_NANOS_ENV: &str = "TACHI_TEST_TEMPLATE_LIFETIME_NOW_NANOS";
const TEMPLATE_PUBLISHED_PREFIX: &str = "memory-server-test-template-";
const TEMPLATE_BUILD_PREFIX: &str = "memory-server-test-template-build-";

/// A prior executable fingerprint older than one day is outside an ordinary
/// test/build handoff, so this retention window time-bounds rebuild
/// accumulation. Correctness does not depend on that age assumption: every
/// process retains a shared cache-use lock with its cached path, and cleanup
/// requires the exclusive lock. Unique build files additionally require a
/// dead PID owner; the current published template is always preserved.
const TEMPLATE_CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
const TEMPLATE_CACHE_LOCK_NAME: &str = "memory-server-test-template-cache.lock";
const TEMPLATE_BUILDER_LOCK_NAME: &str = "memory-server-test-template-builder.lock";

fn acquire_template_builder_lock(cache_root: &std::path::Path) -> std::io::Result<std::fs::File> {
    let lock_path = cache_root.join(TEMPLATE_BUILDER_LOCK_NAME);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            template_cache_io_error("open template builder lock", &lock_path, error)
        })?;
    file.lock().map_err(|error| {
        template_cache_io_error("acquire template builder lock", &lock_path, error)
    })?;
    Ok(file)
}

struct TemplateCacheUseLock {
    file: std::fs::File,
    cleanup_permitted: bool,
}

impl TemplateCacheUseLock {
    fn acquire(cache_root: &std::path::Path) -> std::io::Result<Self> {
        let lock_path = cache_root.join(TEMPLATE_CACHE_LOCK_NAME);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| {
                template_cache_io_error("open template cache use lock", &lock_path, error)
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                file,
                cleanup_permitted: true,
            }),
            Err(std::fs::TryLockError::WouldBlock) => {
                // Another active cache user or cleaner owns the conflicting
                // lock. Wait only for its short exclusive cleanup phase, then
                // retain a shared lock for this cached path's lifetime.
                file.lock_shared().map_err(|error| {
                    template_cache_io_error(
                        "acquire shared template cache use lock",
                        &lock_path,
                        error,
                    )
                })?;
                Ok(Self {
                    file,
                    cleanup_permitted: false,
                })
            }
            Err(std::fs::TryLockError::Error(error)) => Err(template_cache_io_error(
                "acquire exclusive template cache cleanup lock",
                &lock_path,
                error,
            )),
        }
    }

    fn cleanup_permitted(&self) -> bool {
        self.cleanup_permitted
    }

    fn reap_stale_entries(
        &self,
        cache_root: &std::path::Path,
        current_template: &std::path::Path,
        max_age: std::time::Duration,
        now: std::time::SystemTime,
    ) -> std::io::Result<usize> {
        if !self.cleanup_permitted {
            return Ok(0);
        }
        reap_stale_template_cache_entries(cache_root, current_template, max_age, now)
    }

    fn retain_shared(&mut self) -> std::io::Result<()> {
        if !self.cleanup_permitted {
            return Ok(());
        }
        // Release the short cleanup lock before retaining shared use. Another
        // cleaner may run in this gap, but this process has not inspected or
        // cached its template path yet; once the shared lock is acquired, the
        // subsequent path check/build remains protected for process lifetime.
        self.file.unlock()?;
        self.file.lock_shared()?;
        self.cleanup_permitted = false;
        Ok(())
    }
}

enum TemplateCacheEntry {
    Published,
    Build { owner_pid: i32 },
}

fn recognized_template_cache_entry(name: &str) -> Option<TemplateCacheEntry> {
    if let Some(build_name) = name.strip_prefix(TEMPLATE_BUILD_PREFIX) {
        let identity = [
            ".sqlite-wal",
            ".sqlite-shm",
            ".sqlite.migration-marker",
            ".sqlite",
        ]
        .into_iter()
        .find_map(|suffix| build_name.strip_suffix(suffix))?;
        let (pid, build_id) = identity.split_once('-')?;
        let owner_pid = pid.parse::<i32>().ok().filter(|pid| *pid > 1)?;
        let parsed_build_id = uuid::Uuid::parse_str(build_id).ok()?;
        if parsed_build_id.hyphenated().to_string() != build_id {
            return None;
        }
        return Some(TemplateCacheEntry::Build { owner_pid });
    }

    let fingerprint = name
        .strip_prefix(TEMPLATE_PUBLISHED_PREFIX)?
        .strip_suffix(".sqlite")?;
    if fingerprint.len() == 16
        && fingerprint
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        Some(TemplateCacheEntry::Published)
    } else {
        None
    }
}

fn template_cache_io_error(
    action: &str,
    path: &std::path::Path,
    error: std::io::Error,
) -> std::io::Error {
    std::io::Error::new(
        error.kind(),
        format!("{action} {}: {error}", path.display()),
    )
}

fn reap_stale_template_cache_entries(
    cache_root: &std::path::Path,
    current_template: &std::path::Path,
    max_age: std::time::Duration,
    now: std::time::SystemTime,
) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(template_cache_io_error(
                "read template cache directory",
                cache_root,
                error,
            ));
        }
    };

    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let path = entry.path();
        if path == current_template {
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(kind) = recognized_template_cache_entry(file_name) else {
            continue;
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(template_cache_io_error(
                    "inspect template cache entry type",
                    &path,
                    error,
                ));
            }
        };
        if !file_type.is_file() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(template_cache_io_error(
                    "inspect template cache entry metadata",
                    &path,
                    error,
                ));
            }
        };
        let modified = metadata.modified().map_err(|error| {
            template_cache_io_error("read template cache entry mtime", &path, error)
        })?;
        let Some(age) = now.duration_since(modified).ok() else {
            continue;
        };
        if age <= max_age {
            continue;
        }
        if matches!(kind, TemplateCacheEntry::Build { owner_pid } if crate::daemon_lock::process_alive(owner_pid))
        {
            continue;
        }

        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(template_cache_io_error(
                    "remove stale template cache entry",
                    &path,
                    error,
                ));
            }
        }
    }
    Ok(removed)
}

#[test]
fn template_cache_fingerprint_uses_only_compiled_image_inputs() {
    assert_eq!(
        template_cache_fingerprint(),
        template_cache_fingerprint_for_sources(TEMPLATE_IDENTITY_SOURCE_BLOBS)
    );

    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tests/mod.rs"));
    let identity_source_list = source
        .split_once("const TEMPLATE_IDENTITY_SOURCE_BLOBS: &[&[u8]] = &[")
        .expect("template identity source list")
        .1
        .split_once("];\n\nfn extend_template_cache_fingerprint")
        .expect("template identity source list boundary")
        .0;
    let path_function = source
        .split_once("fn template_db_path()")
        .expect("template_db_path source")
        .1
        .split_once("/// Strip constructor-seeded state")
        .expect("template_db_path source boundary")
        .0;
    assert!(path_function.contains("template_cache_fingerprint()"));
    assert!(!path_function.contains("std::fs::metadata"));
    assert!(!path_function.contains(".modified()"));

    let builtins_source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/builtins.rs"));
    let builtins_root_path = format!("/src/{}.rs", "builtins");
    assert!(
        identity_source_list.contains(&builtins_root_path),
        "builtin seed root is missing from the compiled template fingerprint"
    );

    let mut cfg_test = false;
    let mut saw_test_module = false;
    for line in builtins_source.lines() {
        let line = line.trim();
        if line == "#[cfg(test)]" {
            cfg_test = true;
            continue;
        }
        let Some(module) = line
            .strip_prefix("mod ")
            .and_then(|module| module.strip_suffix(';'))
        else {
            continue;
        };
        let embedded_path = format!("/src/builtins/{module}.rs");
        if cfg_test {
            assert_eq!(module, "tests", "unexpected cfg(test) builtin module");
            assert!(
                !identity_source_list.contains(&embedded_path),
                "test-only builtin module must not affect the template fingerprint"
            );
            saw_test_module = true;
        } else {
            assert!(
                identity_source_list.contains(&embedded_path),
                "builtin seed module {module} is missing from the compiled template fingerprint"
            );
        }
        cfg_test = false;
    }
    assert!(
        saw_test_module,
        "expected an explicitly cfg(test) builtin module"
    );

    let builtins_blob_index = TEMPLATE_IDENTITY_SOURCE_BLOBS
        .iter()
        .position(|blob| *blob == builtins_source.as_bytes())
        .expect("builtins.rs must be embedded in the template fingerprint");
    let mut changed_builtins = builtins_source.as_bytes().to_vec();
    changed_builtins.extend_from_slice(b"\n// template fingerprint discriminator\n");
    let mut changed_sources = TEMPLATE_IDENTITY_SOURCE_BLOBS.to_vec();
    changed_sources[builtins_blob_index] = &changed_builtins;
    assert_ne!(
        template_cache_fingerprint(),
        template_cache_fingerprint_for_sources(&changed_sources),
        "changing compiled builtin seed source must change the template fingerprint"
    );

    let migrations_source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../memcore/src/db/migrations.rs"
    ));
    for module in migrations_source.lines().filter_map(|line| {
        line.trim()
            .strip_prefix("mod ")
            .and_then(|module| module.strip_suffix(';'))
    }) {
        let embedded_path = format!("/../memcore/src/db/migrations/{module}.rs");
        assert!(
            identity_source_list.contains(&embedded_path),
            "migration module {module} is missing from the compiled template fingerprint"
        );
    }
}

#[test]
fn template_copy_is_schema_only_for_seeded_state() {
    let dir = tempfile::tempdir().expect("create raw template copy directory");
    let copied = dir.path().join("template.sqlite");
    copy_template_db(&copied);

    let conn = rusqlite::Connection::open(&copied).expect("open raw copied template");
    let seeded_counts: (i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM hub_capabilities),
                (SELECT COUNT(*) FROM sandbox_policies)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("count seeded rows in raw copied template");
    assert_eq!(
        seeded_counts,
        (0, 0),
        "published template copies must not carry constructor-seeded hub capabilities or sandbox policies"
    );

    let profile_json: String = conn
        .query_row(
            "SELECT value_json FROM hard_state WHERE namespace = ?1 AND key = ?2",
            params![
                memcore::db::store_profile::STORE_IDENTITY_NAMESPACE,
                memcore::db::store_profile::STORE_PROFILE_KEY
            ],
            |row| row.get(0),
        )
        .expect("copied template retains its store profile");
    let profile: serde_json::Value =
        serde_json::from_str(&profile_json).expect("parse copied template store profile");
    assert_eq!(profile["value"], "tachi_full");
}

#[test]
fn template_project_copy_does_not_shadow_global_builtin_seed() {
    const BUILTIN_ID: &str = "skill:coding-architecture-decision";

    let (server, _project_db) = make_server_with_project_fixture("template-builtin-shadow");
    let global_builtin = server
        .with_global_store_read(|store| {
            store.hub_get(BUILTIN_ID).map_err(|error| error.to_string())
        })
        .expect("read representative builtin from global store");
    let project_builtin = server
        .with_project_store_read(|store| {
            store.hub_get(BUILTIN_ID).map_err(|error| error.to_string())
        })
        .expect("read representative builtin from project store");

    assert!(
        global_builtin.is_some(),
        "server initialization must seed the representative builtin into the global store"
    );
    assert!(
        project_builtin.is_none(),
        "project template copy must not retain a cached builtin that can shadow the refreshed global definition"
    );
}

#[test]
fn template_published_entry_requires_a_direct_regular_file() {
    let cache = tempfile::tempdir().expect("template entry-state tempdir");
    let missing = cache.path().join("missing.sqlite");
    assert_eq!(
        published_template_state(&missing).expect("inspect missing template entry"),
        PublishedTemplateState::Missing
    );

    let regular = cache.path().join("regular.sqlite");
    std::fs::write(&regular, b"template").expect("write regular template entry");
    assert_eq!(
        published_template_state(&regular).expect("inspect regular template entry"),
        PublishedTemplateState::RegularFile
    );

    let directory = cache.path().join("directory.sqlite");
    std::fs::create_dir(&directory).expect("create non-regular template entry");
    let error = published_template_state(&directory)
        .expect_err("directory template entry must fail loudly");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

    #[cfg(unix)]
    {
        let symlink = cache.path().join("symlink.sqlite");
        std::os::unix::fs::symlink(&regular, &symlink).expect("create template entry symlink");
        let error =
            published_template_state(&symlink).expect_err("template symlink must not be followed");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read(&regular).expect("symlink rejection preserves target"),
            b"template"
        );
    }
}

#[test]
fn template_cache_reaps_stale_recognized_entries_only() {
    let cache = tempfile::tempdir().expect("template cache tempdir");
    let current = cache
        .path()
        .join("memory-server-test-template-1111111111111111.sqlite");
    let prior = cache
        .path()
        .join("memory-server-test-template-2222222222222222.sqlite");
    let dead_pid = 2_000_000_001i32;
    assert!(
        !crate::daemon_lock::process_alive(dead_pid),
        "fixture assumes pid {dead_pid} is dead"
    );
    let abandoned = cache.path().join(format!(
        "memory-server-test-template-build-{dead_pid}-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.sqlite"
    ));
    let abandoned_wal = {
        let mut path = abandoned.clone().into_os_string();
        path.push("-wal");
        std::path::PathBuf::from(path)
    };
    let active = cache.path().join(format!(
        "memory-server-test-template-build-{}-aaaaaaaa-bbbb-cccc-dddd-ffffffffffff.sqlite",
        std::process::id()
    ));
    let unrecognized = cache
        .path()
        .join("memory-server-test-template-not-a-fingerprint.sqlite");

    for path in [
        &current,
        &prior,
        &abandoned,
        &abandoned_wal,
        &active,
        &unrecognized,
    ] {
        std::fs::write(path, b"fixture").expect("write template cache fixture");
    }
    let newest_mtime = [
        &current,
        &prior,
        &abandoned,
        &abandoned_wal,
        &active,
        &unrecognized,
    ]
    .into_iter()
    .map(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .expect("template cache fixture mtime")
    })
    .max()
    .expect("template cache fixture mtime set");
    let removed = reap_stale_template_cache_entries(
        cache.path(),
        &current,
        TEMPLATE_CACHE_MAX_AGE,
        newest_mtime + TEMPLATE_CACHE_MAX_AGE + std::time::Duration::from_secs(5),
    )
    .expect("reap stale template cache fixtures");

    assert_eq!(removed, 3, "prior template + abandoned build and WAL");
    assert!(current.exists(), "current template must always survive");
    assert!(!prior.exists(), "stale prior fingerprint must be reaped");
    assert!(!abandoned.exists(), "stale dead-owner build must be reaped");
    assert!(
        !abandoned_wal.exists(),
        "stale dead-owner build WAL must be reaped"
    );
    assert!(active.exists(), "a live owner's build must survive");
    assert!(
        unrecognized.exists(),
        "unrecognized cache entries are never owned by this reaper"
    );
}

#[test]
fn template_cache_preserves_young_recognized_entries() {
    let cache = tempfile::tempdir().expect("template cache tempdir");
    let current = cache
        .path()
        .join("memory-server-test-template-1111111111111111.sqlite");
    let young_prior = cache
        .path()
        .join("memory-server-test-template-2222222222222222.sqlite");
    let dead_pid = 2_000_000_001i32;
    assert!(
        !crate::daemon_lock::process_alive(dead_pid),
        "fixture assumes pid {dead_pid} is dead"
    );
    let young_abandoned = cache.path().join(format!(
        "memory-server-test-template-build-{dead_pid}-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.sqlite"
    ));
    for path in [&current, &young_prior, &young_abandoned] {
        std::fs::write(path, b"fixture").expect("write young template cache fixture");
    }

    let removed = reap_stale_template_cache_entries(
        cache.path(),
        &current,
        TEMPLATE_CACHE_MAX_AGE,
        std::time::SystemTime::now(),
    )
    .expect("scan young template cache fixtures");

    assert_eq!(removed, 0);
    assert!(current.exists());
    assert!(young_prior.exists(), "young prior template must survive");
    assert!(
        young_abandoned.exists(),
        "young dead-owner build must survive the age gate"
    );
}

const TEMPLATE_LIFETIME_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
const TEMPLATE_LIFETIME_POLL: std::time::Duration = std::time::Duration::from_millis(10);

struct TemplateHelperChild {
    child: Option<std::process::Child>,
    name: String,
}

impl TemplateHelperChild {
    fn new(child: std::process::Child, name: String) -> Self {
        Self {
            child: Some(child),
            name,
        }
    }

    fn child_mut(&mut self) -> &mut std::process::Child {
        self.child.as_mut().expect("template helper disarmed")
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child_mut().try_wait()
    }

    fn terminate_if_running(&mut self) -> std::io::Result<()> {
        if let Ok(Some(_)) = self.try_wait() {
            return Ok(());
        }
        match self.child_mut().kill() {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Ok(Some(_)) = self.try_wait() {
                    return Ok(());
                }
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    fn collect_terminal_output(&mut self) -> std::io::Result<std::process::Output> {
        use std::io::Read;

        let mut child = self.child.take().expect("template helper disarmed");
        let status = child.wait()?;
        let mut stdout = Vec::new();
        let stdout_error = child
            .stdout
            .take()
            .and_then(|mut pipe| pipe.read_to_end(&mut stdout).err());
        let mut stderr = Vec::new();
        let stderr_error = child
            .stderr
            .take()
            .and_then(|mut pipe| pipe.read_to_end(&mut stderr).err());
        if let Some(error) = stdout_error.or(stderr_error) {
            return Err(error);
        }
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }

    fn wait_and_collect(&mut self) -> std::io::Result<std::process::Output> {
        self.collect_terminal_output()
    }
}

impl Drop for TemplateHelperChild {
    fn drop(&mut self) {
        if self.child.is_none() {
            return;
        }
        if let Err(error) = self.terminate_if_running() {
            eprintln!(
                "terminate template helper {} during cleanup: {error}",
                self.name
            );
        }
        if let Err(error) = self.wait_and_collect() {
            eprintln!("reap template helper {} during cleanup: {error}", self.name);
        }
    }
}

fn spawn_template_helper(command: &mut std::process::Command, name: String) -> TemplateHelperChild {
    match command.spawn() {
        Ok(child) => TemplateHelperChild::new(child, name),
        Err(error) => panic!("spawn template helper {name}: {error}"),
    }
}

fn wait_for_template_lifetime_signal(path: &std::path::Path, signal: &str) {
    let deadline = std::time::Instant::now() + TEMPLATE_LIFETIME_WAIT;
    loop {
        match path.try_exists() {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => panic!(
                "inspect template lifetime {signal} {}: {error}",
                path.display()
            ),
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out after {TEMPLATE_LIFETIME_WAIT:?} waiting for template lifetime {signal} {}",
            path.display()
        );
        std::thread::sleep(remaining.min(TEMPLATE_LIFETIME_POLL));
    }
}

fn wait_for_template_lifetime_ready_child(
    child: &mut TemplateHelperChild,
    ready: &std::path::Path,
) {
    let deadline = std::time::Instant::now() + TEMPLATE_LIFETIME_WAIT;
    loop {
        match ready.try_exists() {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => panic!(
                "inspect template lifetime holder ready file {}: {error}",
                ready.display()
            ),
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child
                    .collect_terminal_output()
                    .expect("collect exited template lifetime holder output");
                panic!(
                    "template lifetime holder exited before ready: {}\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(None) => {}
            Err(error) => panic!("poll template lifetime holder: {error}"),
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            let kill_error = child.terminate_if_running().err();
            let output = child
                .wait_and_collect()
                .expect("collect timed-out template lifetime holder output");
            panic!(
                "timed out after {TEMPLATE_LIFETIME_WAIT:?} waiting for template lifetime holder ready; kill_error={kill_error:?}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(remaining.min(TEMPLATE_LIFETIME_POLL));
    }
}

fn wait_for_template_lifetime_child(
    child: &mut TemplateHelperChild,
    child_name: &str,
) -> std::process::Output {
    let deadline = std::time::Instant::now() + TEMPLATE_LIFETIME_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.collect_terminal_output().unwrap_or_else(|error| {
                    panic!("collect template lifetime {child_name}: {error}")
                });
            }
            Ok(None) => {}
            Err(error) => panic!("poll template lifetime {child_name}: {error}"),
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            let kill_error = child.terminate_if_running().err();
            let output = child.wait_and_collect().unwrap_or_else(|error| {
                panic!("collect timed-out template lifetime {child_name}: {error}")
            });
            panic!(
                "template lifetime {child_name} timed out after {TEMPLATE_LIFETIME_WAIT:?}; kill_error={kill_error:?}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(remaining.min(TEMPLATE_LIFETIME_POLL));
    }
}

#[test]
fn template_cache_active_user_blocks_prior_fingerprint_cleanup() {
    if let Some(mode) = std::env::var_os(TEMPLATE_LIFETIME_HELPER_ENV) {
        let mode = mode
            .into_string()
            .expect("template lifetime helper mode must be UTF-8");
        let cache_root = std::path::PathBuf::from(
            std::env::var_os(TEMPLATE_LIFETIME_ROOT_ENV)
                .expect("template lifetime helper cache root"),
        );
        let prior = cache_root.join("memory-server-test-template-1111111111111111.sqlite");
        let current = cache_root.join("memory-server-test-template-2222222222222222.sqlite");
        let injected_now_nanos = std::env::var(TEMPLATE_LIFETIME_NOW_NANOS_ENV)
            .expect("template lifetime helper injected now")
            .parse::<u64>()
            .expect("template lifetime helper injected now nanoseconds");
        let injected_now =
            std::time::UNIX_EPOCH + std::time::Duration::from_nanos(injected_now_nanos);

        match mode.as_str() {
            "hold-shared" => {
                let mut use_lock = TemplateCacheUseLock::acquire(&cache_root)
                    .expect("holder acquires template cache lock");
                assert!(
                    use_lock.cleanup_permitted(),
                    "holder must be the initial exclusive acquirer"
                );
                use_lock
                    .retain_shared()
                    .expect("holder retains shared template cache lock");
                let ready = cache_root.join("holder.ready");
                std::fs::write(&ready, b"shared-lock-held")
                    .expect("record template lifetime holder readiness");
                println!("template_lifetime mode=hold-shared state=ready shared=true");

                wait_for_template_lifetime_signal(
                    &cache_root.join("holder.release"),
                    "holder release",
                );
                let copied = cache_root.join("holder-later-copy.sqlite");
                std::fs::copy(&prior, &copied)
                    .expect("active holder must still copy its cached prior template");
                assert_eq!(
                    std::fs::read(&copied).expect("read active holder later copy"),
                    b"cached template"
                );
                println!("template_lifetime mode=hold-shared state=released prior_copy_ok=true");
            }
            "probe-active" => {
                let use_lock = TemplateCacheUseLock::acquire(&cache_root)
                    .expect("active probe acquires template cache lock");
                assert!(
                    !use_lock.cleanup_permitted(),
                    "active probe must not obtain exclusive cleanup while holder is alive"
                );
                let removed = use_lock
                    .reap_stale_entries(&cache_root, &current, TEMPLATE_CACHE_MAX_AGE, injected_now)
                    .expect("active probe checks template cache cleanup");
                let prior_exists = prior.exists();
                let current_exists = current.exists();
                assert_eq!(removed, 0);
                assert!(prior_exists, "active holder's prior template must survive");
                assert!(current_exists, "current template must survive active probe");
                let result = format!(
                    "cleanup_permitted=false removed={removed} prior_exists={prior_exists} current_exists={current_exists}"
                );
                std::fs::write(cache_root.join("probe-active.result"), &result)
                    .expect("record active template lifetime probe");
                println!("template_lifetime mode=probe-active {result}");
            }
            "probe-released" => {
                let use_lock = TemplateCacheUseLock::acquire(&cache_root)
                    .expect("released probe acquires template cache lock");
                assert!(
                    use_lock.cleanup_permitted(),
                    "released probe must obtain exclusive cleanup after holder exits"
                );
                let removed = use_lock
                    .reap_stale_entries(&cache_root, &current, TEMPLATE_CACHE_MAX_AGE, injected_now)
                    .expect("released probe reaps prior template");
                let prior_exists = prior.exists();
                let current_exists = current.exists();
                assert_eq!(
                    removed, 1,
                    "released probe must reap exactly the prior template"
                );
                assert!(
                    !prior_exists,
                    "released probe must remove the prior template"
                );
                assert!(
                    current_exists,
                    "released probe must preserve the current template"
                );
                let result = format!(
                    "cleanup_permitted=true removed={removed} prior_exists={prior_exists} current_exists={current_exists}"
                );
                std::fs::write(cache_root.join("probe-released.result"), &result)
                    .expect("record released template lifetime probe");
                println!("template_lifetime mode=probe-released {result}");
            }
            _ => panic!("unknown template lifetime helper mode: {mode}"),
        }
        return;
    }

    let cache = tempfile::tempdir().expect("template cache tempdir");
    let cached_by_process_a = cache
        .path()
        .join("memory-server-test-template-1111111111111111.sqlite");
    let current_for_process_b = cache
        .path()
        .join("memory-server-test-template-2222222222222222.sqlite");
    std::fs::write(&cached_by_process_a, b"cached template")
        .expect("write process A cached template");
    std::fs::write(&current_for_process_b, b"new template")
        .expect("write process B current template");
    let newest_mtime = [&cached_by_process_a, &current_for_process_b]
        .into_iter()
        .map(|path| {
            std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .expect("active-user template mtime")
        })
        .max()
        .expect("active-user template mtime set");
    let injected_now = newest_mtime + TEMPLATE_CACHE_MAX_AGE + std::time::Duration::from_secs(5);
    let injected_now_nanos: u64 = injected_now
        .duration_since(std::time::UNIX_EPOCH)
        .expect("active-user injected now after Unix epoch")
        .as_nanos()
        .try_into()
        .expect("active-user injected now fits nanoseconds in u64");
    let injected_now_nanos = injected_now_nanos.to_string();

    let executable = std::env::current_exe().expect("current test executable");
    let test_name = "tests::template_cache_active_user_blocks_prior_fingerprint_cleanup";
    let spawn_helper = |mode: &str| {
        spawn_template_helper(
            std::process::Command::new(&executable)
                .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
                .env(TEMPLATE_LIFETIME_HELPER_ENV, mode)
                .env(TEMPLATE_LIFETIME_ROOT_ENV, cache.path())
                .env(TEMPLATE_LIFETIME_NOW_NANOS_ENV, &injected_now_nanos)
                .env("CARGO_TERM_COLOR", "never")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
            format!("template lifetime helper {mode}"),
        )
    };

    let mut holder = spawn_helper("hold-shared");
    wait_for_template_lifetime_ready_child(&mut holder, &cache.path().join("holder.ready"));

    let mut active_probe_child = spawn_helper("probe-active");
    let active_probe =
        wait_for_template_lifetime_child(&mut active_probe_child, "active-cleanup probe");
    assert!(
        active_probe.status.success(),
        "active-cleanup probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&active_probe.stdout),
        String::from_utf8_lossy(&active_probe.stderr)
    );
    let active_result = std::fs::read_to_string(cache.path().join("probe-active.result"))
        .expect("read active template lifetime probe result");
    assert_eq!(
        active_result, "cleanup_permitted=false removed=0 prior_exists=true current_exists=true",
        "active-cleanup probe must discriminate exclusive cleanup denial"
    );
    assert!(
        cached_by_process_a.exists(),
        "active holder's prior template must remain before release"
    );

    std::fs::write(cache.path().join("holder.release"), b"release")
        .expect("release template lifetime holder");
    let holder = wait_for_template_lifetime_child(&mut holder, "holder");
    assert!(
        holder.status.success(),
        "template lifetime holder failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&holder.stdout),
        String::from_utf8_lossy(&holder.stderr)
    );

    let mut released_probe_child = spawn_helper("probe-released");
    let released_probe =
        wait_for_template_lifetime_child(&mut released_probe_child, "released-cleanup probe");
    assert!(
        released_probe.status.success(),
        "released-cleanup probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&released_probe.stdout),
        String::from_utf8_lossy(&released_probe.stderr)
    );
    let released_result = std::fs::read_to_string(cache.path().join("probe-released.result"))
        .expect("read released template lifetime probe result");
    assert_eq!(
        released_result, "cleanup_permitted=true removed=1 prior_exists=false current_exists=true",
        "released-cleanup probe must remove exactly the stale prior fingerprint"
    );

    println!(
        "{}\n{}\n{}",
        String::from_utf8_lossy(&holder.stdout).trim(),
        String::from_utf8_lossy(&active_probe.stdout).trim(),
        String::from_utf8_lossy(&released_probe.stdout).trim()
    );
}

#[test]
fn template_helper_guard_drop_reaps_holder_and_releases_lock() {
    let cache = tempfile::tempdir().expect("template helper guard tempdir");
    for (name, contents) in [
        (
            "memory-server-test-template-1111111111111111.sqlite",
            b"cached template".as_slice(),
        ),
        (
            "memory-server-test-template-2222222222222222.sqlite",
            b"new template".as_slice(),
        ),
    ] {
        std::fs::write(cache.path().join(name), contents)
            .expect("write template helper guard fixture");
    }
    let injected_now_nanos: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("template helper guard time after Unix epoch")
        .as_nanos()
        .try_into()
        .expect("template helper guard time fits nanoseconds in u64");
    let executable = std::env::current_exe().expect("current test executable");
    let helper_test = "tests::template_cache_active_user_blocks_prior_fingerprint_cleanup";
    let mut holder = spawn_template_helper(
        std::process::Command::new(&executable)
            .args(["--exact", helper_test, "--nocapture", "--test-threads=1"])
            .env(TEMPLATE_LIFETIME_HELPER_ENV, "hold-shared")
            .env(TEMPLATE_LIFETIME_ROOT_ENV, cache.path())
            .env(
                TEMPLATE_LIFETIME_NOW_NANOS_ENV,
                injected_now_nanos.to_string(),
            )
            .env("CARGO_TERM_COLOR", "never")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
        "template guard drop holder".to_string(),
    );
    let ready = cache.path().join("holder.ready");
    let release = cache.path().join("holder.release");
    wait_for_template_lifetime_ready_child(&mut holder, &ready);
    assert!(
        !release.exists(),
        "drop regression must not release holder cleanly"
    );

    drop(holder);

    assert!(
        !release.exists(),
        "holder must be terminated without release signal"
    );
    let fresh = TemplateCacheUseLock::acquire(cache.path())
        .expect("fresh acquirer after template helper guard drop");
    assert!(
        fresh.cleanup_permitted(),
        "reaped holder must release its shared cache lock"
    );
}

#[test]
fn template_db_path_is_shared_across_processes() {
    if let Some(mode) = std::env::var_os(TEMPLATE_GUARD_HELPER_ENV) {
        assert_eq!(
            mode, "resolve-template",
            "unknown template guard helper mode"
        );
        let output_path =
            std::env::var_os(TEMPLATE_GUARD_OUTPUT_ENV).expect("template guard helper output path");
        let template = template_db_path().clone();
        let copied = crate::utils::test_fixture_path(format!(
            "template-cross-process-{}.sqlite",
            std::process::id()
        ));
        copy_template_db(&copied);
        let _server = MemoryServer::new(copied, None).expect("initialize copied template");
        std::fs::write(&output_path, template.display().to_string())
            .expect("record resolved template path");
        println!("template_db_path={}", template.display());
        return;
    }

    let isolated_root = tempfile::tempdir().expect("create isolated template guard root");
    let isolated_tmp = isolated_root.path().join("tmp");
    let isolated_cache = isolated_root.path().join("cache");
    let build_receipt = isolated_root.path().join("template-builds.txt");
    std::fs::create_dir_all(&isolated_tmp).expect("create isolated template guard tmp");
    std::fs::create_dir_all(&isolated_cache).expect("create isolated template guard cache");

    let executable = std::env::current_exe().expect("current test executable");
    let test_name = "tests::template_db_path_is_shared_across_processes";
    let spawn_child = |index: usize| {
        let output_path = isolated_root
            .path()
            .join(format!("resolved-template-{index}.txt"));
        spawn_template_helper(
            std::process::Command::new(&executable)
                .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
                .env(TEMPLATE_GUARD_HELPER_ENV, "resolve-template")
                .env(TEMPLATE_GUARD_OUTPUT_ENV, &output_path)
                .env(TEMPLATE_BUILD_RECEIPT_ENV, &build_receipt)
                .env(TEMPLATE_CACHE_ROOT_ENV, &isolated_cache)
                .env("TMPDIR", &isolated_tmp)
                .env("TEMP", &isolated_tmp)
                .env("TMP", &isolated_tmp)
                .env("CARGO_TERM_COLOR", "never")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
            format!("template guard child {index}"),
        )
    };

    let mut first_child = spawn_child(0);
    let mut second_child = spawn_child(1);
    let first = wait_for_template_lifetime_child(&mut first_child, "template guard child 0");
    let second = wait_for_template_lifetime_child(&mut second_child, "template guard child 1");

    assert!(
        first.status.success(),
        "template guard child 0 failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "template guard child 1 failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );

    let first_path = std::fs::read_to_string(isolated_root.path().join("resolved-template-0.txt"))
        .expect("read template guard child 0 path");
    let second_path = std::fs::read_to_string(isolated_root.path().join("resolved-template-1.txt"))
        .expect("read template guard child 1 path");
    assert_eq!(
        first_path,
        second_path,
        "template cache must be shared across processes\nchild 0 stdout:\n{}\nchild 0 stderr:\n{}\nchild 1 stdout:\n{}\nchild 1 stderr:\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr),
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let receipts = std::fs::read_to_string(&build_receipt).unwrap_or_else(|error| {
        panic!(
            "read template build receipt {}: {error}",
            build_receipt.display()
        )
    });
    assert_eq!(
        receipts.lines().count(),
        1,
        "exactly one process must build the shared template; receipts:\n{receipts}\nchild 0 stdout:\n{}\nchild 0 stderr:\n{}\nchild 1 stdout:\n{}\nchild 1 stderr:\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr),
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
}

fn make_test_server(project_name: Option<&str>) -> (TestServer, Option<std::path::PathBuf>) {
    ensure_test_env();
    let fixture_root =
        crate::utils::test_fixture_path(format!("memory-server-test-{}", uuid::Uuid::new_v4()));
    let fixture_home = fixture_root.join("home");
    let db_path = fixture_root
        .join("global")
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(db_path.parent().expect("global test db parent"))
        .expect("create global test db parent");
    std::fs::create_dir_all(&fixture_home).expect("create fixture Tachi home");
    copy_template_db(&db_path);

    let project_db_path = project_name.map(|project_name| {
        let project_db_path = fixture_root
            .join("project")
            .join(".tachi")
            .join(memcore::MEMORY_DB_FILENAME);
        std::fs::create_dir_all(project_db_path.parent().expect("project test db parent"))
            .expect("create project test db parent");
        copy_template_db(&project_db_path);

        let mut manifest = crate::manifest::Manifest::empty();
        manifest.dbs.push(crate::manifest::DbEntry {
            path: project_db_path.display().to_string(),
            role: crate::manifest::DbRole::Project,
            owner: "test".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: format!("project:{project_name}"),
            notes: String::new(),
        });
        manifest
            .save(&fixture_home.join("manifest.json"))
            .expect("save fixture manifest");
        project_db_path
    });
    let server =
        MemoryServer::new_with_home_for_test(db_path, project_db_path.clone(), fixture_home)
            .expect("failed to create test server");
    (
        TestServer {
            server: Some(server),
            fixture_root,
        },
        project_db_path,
    )
}

pub(crate) fn make_server() -> TestServer {
    make_test_server(None).0
}

/// Plant `projects/wiki/tachi-memory.db` at an older `PRAGMA user_version`
/// so an ordinary Deny-authority open hits `#1119`
/// `SchemaMigrationOptInRequired`. Discriminates leftover-schema federation
/// (#1761): tray surfaces must degrade; `tachi migrate --apply` is the
/// authorized path. Does not weaken the stamp or the ordinary-open gate.
pub(crate) fn plant_leftover_shared_wiki(server: &MemoryServer) {
    let wiki_dir = server.tachi_home_dir().join("projects").join("wiki");
    std::fs::create_dir_all(&wiki_dir).expect("create leftover wiki dir");
    let wiki_db = wiki_dir.join(memcore::MEMORY_DB_FILENAME);
    {
        let _store = MemoryStore::open_with_label(
            wiki_db.to_str().expect("utf8 leftover wiki path"),
            "wiki",
        )
        .expect("seed leftover wiki at current schema");
    }
    let conn = rusqlite::Connection::open(&wiki_db).expect("reopen leftover wiki");
    conn.execute_batch(&format!(
        "PRAGMA user_version = {}",
        memcore::db::migrations::EXPECTED_SCHEMA_VERSION.saturating_sub(2)
    ))
    .expect("roll leftover wiki schema stamp back");
    drop(conn);
    let _ = std::fs::remove_file(format!("{}.migration-marker", wiki_db.display()));
}

pub(crate) fn register_logical_shared_wiki(server: &MemoryServer) {
    let db_path = server
        .tachi_home_dir()
        .join("projects")
        .join("wiki")
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(db_path.parent().expect("shared wiki parent"))
        .expect("create shared wiki parent");
    drop(
        MemoryStore::open(db_path.to_str().expect("utf8 shared wiki DB"))
            .expect("create shared wiki DB"),
    );

    let manifest_path = server.tachi_home_dir().join("manifest.json");
    let mut manifest = crate::manifest::Manifest::load_or_empty(&manifest_path);
    manifest.dbs.push(crate::manifest::DbEntry {
        path: db_path.display().to_string(),
        role: crate::manifest::DbRole::Project,
        owner: "test".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: Utc::now().to_rfc3339(),
        last_classification: "healthy".to_string(),
        scope_hint: "project:wiki".to_string(),
        notes: String::new(),
    });
    manifest.save(&manifest_path).expect("register shared wiki");
}

pub(crate) fn make_server_with_project_fixture(
    project_name: &str,
) -> (TestServer, std::path::PathBuf) {
    let (server, project_db_path) = make_test_server(Some(project_name));
    (
        server,
        project_db_path.expect("explicit project fixture must create a project database"),
    )
}

#[test]
fn make_server_keeps_named_project_resolution_inside_its_fixture_home() {
    ensure_test_env();
    let _lock = home_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let project_root = crate::utils::find_project_git_root().expect("test project root");
    let project_name = crate::path_utils::plan_c_dir_name_from_root(&project_root)
        .expect("derive current repository project name");
    let entry_id = "make-server-named-project-isolation";

    let ambient_home = tempfile::tempdir().expect("ambient test home");
    let _ambient_tachi_home =
        crate::test_support::EnvRestore::set_path("TACHI_HOME", ambient_home.path());
    let ambient_db = ambient_home.path().join("ambient-project.db");
    copy_template_db(&ambient_db);
    let mut ambient_entry = make_entry(entry_id);
    ambient_entry.text = "ambient manifest entry".to_string();
    MemoryStore::open(ambient_db.to_str().expect("utf8 ambient db"))
        .expect("open ambient project db")
        .upsert(&ambient_entry)
        .expect("seed ambient project db");
    let ambient_manifest = crate::manifest::Manifest {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        comment: String::new(),
        dbs: vec![crate::manifest::DbEntry {
            path: ambient_db.display().to_string(),
            role: crate::manifest::DbRole::Project,
            owner: "test".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: format!("project:{project_name}"),
            notes: String::new(),
        }],
    };
    ambient_manifest
        .save(&ambient_home.path().join("manifest.json"))
        .expect("save ambient manifest");

    let (server, fixture_project_db) = make_server_with_project_fixture(&project_name);
    let mut fixture_entry = make_entry(entry_id);
    fixture_entry.text = "fixture manifest entry".to_string();
    MemoryStore::open(fixture_project_db.to_str().expect("utf8 fixture db"))
        .expect("open fixture project db")
        .upsert(&fixture_entry)
        .expect("seed fixture project db");

    assert_eq!(
        std::env::var_os("TACHI_HOME").as_deref(),
        Some(ambient_home.path().as_os_str())
    );
    assert_ne!(server.tachi_home_dir(), ambient_home.path());
    let entry = server
        .with_named_project_store_read(&project_name, |store| {
            store.get(entry_id).map_err(|error| error.to_string())
        })
        .expect("named-project read through fixture manifest")
        .expect("fixture manifest entry exists");
    assert_eq!(entry.text, "fixture manifest entry");
}

#[test]
fn make_server_preserves_explicit_home_and_run_root() {
    ensure_test_env();
    let _lock = home_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let explicit_home = tempfile::tempdir().expect("explicit Tachi home");
    let explicit_run_root = explicit_home.path().join("caller-runs");
    let _home = crate::test_support::EnvRestore::set_path("TACHI_HOME", explicit_home.path());
    let _run_root = crate::test_support::EnvRestore::set_path("TACHI_RUN_ROOT", &explicit_run_root);

    let server = make_server();

    assert_eq!(
        std::env::var_os("TACHI_HOME").as_deref(),
        Some(explicit_home.path().as_os_str())
    );
    assert_eq!(
        std::env::var_os("TACHI_RUN_ROOT").as_deref(),
        Some(explicit_run_root.as_os_str())
    );
    assert_ne!(server.tachi_home_dir(), explicit_home.path());
    assert!(
        server.project_db_path_buf().is_none(),
        "make_server must retain its legacy global-only topology"
    );
}

pub(crate) fn make_server_with_temp_home() -> (MemoryServer, TempHomeGuard) {
    make_server_with_temp_home_and_migration_authority(MigrationAuthority::Deny)
}

pub(crate) fn make_server_with_temp_home_and_migration_authority(
    migration: MigrationAuthority,
) -> (MemoryServer, TempHomeGuard) {
    ensure_test_env();
    let temp_home = TempHomeGuard::new();
    let global_db = temp_home.temp_home.join(".tachi/global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global db parent"))
        .expect("create global db dir");
    copy_template_db(&global_db);
    let server = MemoryServer::new_with_migration_authority(global_db, None, migration)
        .expect("failed to create test server");
    (server, temp_home)
}

/// Fixture invariant: every row a test asked to seed is still a live row.
///
/// memcore's write path runs near-duplicate consolidation
/// (`memcore::db::memory_crud::merge_into_jaccard_candidate`; any FTS
/// candidate with token-Jaccard above 0.9). Every entry [`make_entry`] builds
/// carries the *same* body — the literal `"test memory"` — so seeding N such
/// entries merges rows 2..N into row 1 and stamps each of them
/// `superseded_by` at write time. The dead rows still exist and
/// `MemoryStore::get` still returns them, so a test can seed eleven rows,
/// operate on ten corpses, and pass anyway.
///
/// Fail here, at the seed, instead of downstream where the damage only
/// surfaces as a count assertion nobody can explain — or, worse, does not
/// surface at all. A fixture that wants several live rows must give each
/// entry its own body.
fn assert_seeded_rows_are_live(store: &MemoryStore, entries: &[MemoryEntry]) {
    let mut dead = Vec::new();
    for entry in entries {
        let (archived, superseded_by) = store
            .connection()
            .query_row(
                "SELECT archived, superseded_by FROM memories WHERE id = ?1",
                params![entry.id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .unwrap_or_else(|error| panic!("seeded fixture row {} must exist: {error}", entry.id));
        if let Some(winner) = superseded_by {
            dead.push(format!("{} (superseded_by {winner})", entry.id));
        } else if archived != 0 {
            dead.push(format!("{} (archived)", entry.id));
        }
    }
    assert!(
        dead.is_empty(),
        "fixture seeded {} rows but {} are already dead at write time: {}.\n\
         memcore folded them into a near-duplicate (token-Jaccard > 0.9, \
         memcore::db::memory_crud::merge_into_jaccard_candidate). Every row make_entry \
         produces shares the body \"test memory\", so only the first survives; the rest are \
         still readable via MemoryStore::get, which is how this hides. Give each seeded \
         entry a distinct `text` before asserting anything about these rows.",
        entries.len(),
        dead.len(),
        dead.join(", "),
    );
}

fn seed_wiki_project_entries(entries: Vec<MemoryEntry>) -> (MemoryServer, TempHomeGuard) {
    ensure_test_env();
    let temp_home = TempHomeGuard::new();
    let wiki_dir = temp_home.temp_home.join(".tachi/projects/wiki");
    std::fs::create_dir_all(&wiki_dir).expect("create wiki project dir");
    let wiki_db = wiki_dir.join("memory.db");
    {
        let mut store = MemoryStore::open(wiki_db.to_str().expect("utf8 wiki db"))
            .expect("open wiki project db");
        for entry in &entries {
            if entry.id == "wiki-operation-log"
                && entry.path == "/wiki/_log"
                && entry.topic.eq_ignore_ascii_case("wiki_log")
            {
                store
                    .upsert_wiki_operation_log(entry)
                    .expect("seed Wiki operation log");
            } else {
                store.upsert(entry).expect("seed wiki project entry");
            }
        }
        assert_seeded_rows_are_live(&store, &entries);
    }
    seed_pre_v23_wiki_reference_metadata(&wiki_db, &entries);
    let global_db = temp_home.temp_home.join(".tachi/global/memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global db parent"))
        .expect("create global db dir");
    copy_template_db(&global_db);
    let server = MemoryServer::new_with_migration_authority(
        global_db,
        None,
        MigrationAuthority::Allow {
            approved_by: "test:wiki-legacy-v22-fixture".to_string(),
        },
    )
    .expect("failed to create test server");
    server
        .with_named_project_store("wiki", |store| {
            let schema_version: i64 = store
                .connection()
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .map_err(|error| error.to_string())?;
            if schema_version != i64::from(memcore::db::migrations::EXPECTED_SCHEMA_VERSION) {
                return Err(format!(
                    "legacy wiki fixture must reopen at schema v{}; found v{schema_version}",
                    memcore::db::migrations::EXPECTED_SCHEMA_VERSION
                ));
            }
            let guard_count: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE type = 'trigger'
                       AND name IN (
                           'memories_reserved_refs_insert_guard',
                           'memories_reserved_refs_update_guard'
                       )",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if guard_count != 2 {
                return Err(format!(
                    "legacy wiki fixture must restore both v23 reference guards; found {guard_count}"
                ));
            }
            Ok(())
        })
        .expect("authorized named wiki open must restore v23 guards");
    (server, temp_home)
}

/// Seed only the pre-v23 state that an ordinary v23 upsert cannot express.
///
/// The base rows still travel through `MemoryStore::upsert`, preserving the
/// normal write boundary. The offline mutation first turns the disposable DB
/// into a genuine v22 snapshot, then restores only fixture-supplied reserved
/// reference metadata. The server above must migrate the snapshot back to
/// canonical v23 before any wiki operation can use it.
fn seed_pre_v23_wiki_reference_metadata(wiki_db: &std::path::Path, entries: &[MemoryEntry]) {
    let connection = rusqlite::Connection::open(wiki_db).expect("open offline wiki v22 fixture");
    connection
        .execute(
            "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
            params!["migrations", "v23_reserved_reference_guards"],
        )
        .expect("remove v23 guard migration sentinel from legacy fixture");
    connection
        .execute(
            "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
            params!["migrations", "v24_memories_scored_count"],
        )
        .expect("remove v24 scored-count migration sentinel from legacy fixture");
    connection
        .execute_batch(
            "DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
             DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;
             ALTER TABLE memories DROP COLUMN scored_count;
             PRAGMA user_version = 22;",
        )
        .expect("downgrade disposable wiki fixture to v22");
    let schema_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read legacy wiki fixture schema version");
    assert_eq!(
        schema_version, 22,
        "fixture must be pre-v23 before metadata seed"
    );

    for entry in entries.iter().filter(|entry| {
        entry.metadata.get("source_refs").is_some()
            || entry.metadata.get("evidence_refs_v1").is_some()
    }) {
        let updated = connection
            .execute(
                "UPDATE memories SET metadata = ?1 WHERE id = ?2",
                params![entry.metadata.to_string(), entry.id],
            )
            .expect("restore legacy wiki reserved metadata");
        assert_eq!(
            updated, 1,
            "legacy fixture row must exist before metadata restore"
        );
    }
}

pub(crate) fn make_entry(id: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/".to_string(),
        summary: "".to_string(),
        text: "test memory".to_string(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "".to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: "".to_string(),
        source: "test".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata: json!({}),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

pub(crate) fn create_named_project_db(home: &std::path::Path, name: &str) -> std::path::PathBuf {
    let db_path = home
        .join("projects")
        .join(name)
        .join(memcore::MEMORY_DB_FILENAME);
    std::fs::create_dir_all(db_path.parent().expect("named-project DB parent"))
        .expect("create named-project DB parent");
    drop(
        MemoryStore::open(db_path.to_str().expect("utf8 named-project DB"))
            .expect("create named-project DB"),
    );
    db_path
}

pub(crate) fn create_split_brain_alias(
    home: &std::path::Path,
    local_db: &std::path::Path,
) -> std::path::PathBuf {
    let project_root = crate::path_utils::plan_c_project_root_from_local_db(local_db)
        .expect("repo-local project DB");
    let project_name =
        crate::path_utils::plan_c_dir_name_from_root(&project_root).expect("project identity");
    create_named_project_db(home, &project_name)
}

fn make_test_tool(name: &str) -> rmcp::model::Tool {
    serde_json::from_value(json!({
        "name": name,
        "description": format!("tool {name}"),
        "inputSchema": {
            "type": "object",
            "additionalProperties": true,
        }
    }))
    .expect("failed to build test tool")
}

pub(crate) fn make_mcp_capability(id: &str, version: u32) -> HubCapability {
    let name = id.strip_prefix("mcp:").unwrap_or(id).to_string();
    HubCapability {
        id: id.to_string(),
        cap_type: "mcp".to_string(),
        name: name.clone(),
        version,
        description: format!("test capability {name}"),
        definition: json!({
            "transport": "stdio",
            "command": "/usr/bin/true",
            "args": [],
            "discovery_status": "ready",
        })
        .to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

async fn call_tool_via_server(
    mut server: TestServer,
    tool_name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    // Take ownership of the inner server for `serve_directly` (which consumes
    // it). The `server` wrapper, now holding `None`, lives to end of scope and
    // cleans up the temp db files on drop.
    let inner = server
        .server
        .take()
        .expect("TestServer inner server already taken");
    call_tool_on_server(inner, tool_name, arguments).await
}

/// Same as `call_tool_via_server`, but takes an owned `MemoryServer` directly
/// instead of a `TestServer` wrapper. `MemoryServer` is `Clone` over shared
/// `Arc`s (see `DbRuntime::global_store`), so callers that need to thread
/// state across several sequential calls against the SAME underlying store
/// (e.g. a write-then-read-back golden that proves a legacy alias and its
/// canonical verb persist identical state, not just identical echoed
/// responses) can `server.clone()` a `TestServer`'s inner server N times and
/// drive each call through this function while the original `TestServer`
/// still owns cleanup of the backing sqlite file.
pub(crate) async fn call_tool_on_server(
    inner: MemoryServer,
    tool_name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    let mut params = rmcp::model::CallToolRequestParams::new(tool_name.to_string());
    if let Some(arguments) = arguments.filter(|args| !args.is_empty()) {
        params = params.with_arguments(arguments);
    }

    let request =
        rmcp::model::ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(params));
    let (transport, mut receiver) =
        rmcp::transport::OneshotTransport::<rmcp::service::RoleServer>::new(
            rmcp::model::ClientJsonRpcMessage::request(request, rmcp::model::RequestId::Number(1)),
        );
    let service = rmcp::service::serve_directly(inner, transport, None);

    let message = tokio::time::timeout(std::time::Duration::from_secs(3), receiver.recv())
        .await
        .expect("tool call timed out")
        .expect("tool call should yield one response");

    let quit_reason = service.waiting().await.expect("wait for oneshot service");
    assert!(
        matches!(quit_reason, rmcp::service::QuitReason::Closed),
        "oneshot service should close cleanly after one tool call"
    );

    match message {
        rmcp::model::ServerJsonRpcMessage::Response(response) => match response.result {
            rmcp::model::ServerResult::CallToolResult(result) => Ok(result),
            other => panic!("expected CallToolResult, got {other:?}"),
        },
        rmcp::model::ServerJsonRpcMessage::Error(error) => Err(error.error),
        other => panic!("expected tool response or error, got {other:?}"),
    }
}

fn make_skill_capability(
    id: &str,
    name: &str,
    description: &str,
    visibility: &str,
) -> HubCapability {
    HubCapability {
        id: id.to_string(),
        cap_type: "skill".to_string(),
        name: name.to_string(),
        version: 1,
        description: description.to_string(),
        definition: json!({
            "prompt": format!("Run skill {name}"),
            "content": format!("# {name}\n\n{description}"),
            "policy": {
                "visibility": visibility,
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "input": {"type": "string"}
                }
            }
        })
        .to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "direct".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    }
}

mod chain_skills_tests;
mod claims_tests;
mod closure_scan_tests;
mod dispatch_tests;
mod docs_tests;
mod facade_tests;
mod fold_contract;
mod gh_comment_tests;
mod hub_tests;
mod kanban_tests;
mod memory_tests;
mod merge_tests;
mod orchestrator_tests;
mod profile_tests;
mod proxy_tests;
mod sandbox_fold;
mod sandbox_tests;
mod skill_tests;
mod tachi_handoff_tests;
mod vault_tests;
mod vc_tests;
mod wiki_tests;
mod workflow_tests;
