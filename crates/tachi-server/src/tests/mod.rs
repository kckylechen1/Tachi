use crate::server_state::MemoryServer;
use crate::tool_params::*;
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

/// Path to a schema-initialized SQLite file, built once per test binary
/// process, that every `make_server*` call below copies from instead of
/// paying `MemoryServer::new`'s full DDL + `ensure_column` + data-migration
/// chain on a brand-new empty file. `run_data_migrations` is already
/// exercised twice-in-a-row by memcore's own migration tests (see
/// `crates/memcore/src/db/migrations.rs`), so re-running the same
/// startup path against an already-migrated file is a normal, supported
/// state (identical to a real restart against an existing `~/.tachi` DB) —
/// not a special case invented for this fixture (issue #682 template-DB
/// fixture, G2).
/// The template lives at a STABLE path keyed by the test binary's identity
/// (path + size + mtime), NOT behind a process-local `OnceLock` alone:
/// nextest runs each test in its own process, so a per-process cache would
/// rebuild the template for every single test and make the suite slower,
/// not faster. Keying on the binary identity means a recompile (which is
/// the only way the schema/migration chain can change) automatically gets
/// a fresh template, while all test processes of one build share one file.
fn template_db_path() -> &'static std::path::PathBuf {
    static TEMPLATE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    TEMPLATE.get_or_init(|| {
        ensure_test_env();
        let fingerprint = {
            use std::hash::{Hash, Hasher};
            let exe = std::env::current_exe().expect("test binary path");
            let meta = std::fs::metadata(&exe).expect("test binary metadata");
            let mut hasher = std::hash::DefaultHasher::new();
            exe.hash(&mut hasher);
            meta.len().hash(&mut hasher);
            meta.modified()
                .expect("test binary mtime")
                .hash(&mut hasher);
            hasher.finish()
        };
        let path = crate::utils::test_fixture_path(format!(
            "memory-server-test-template-{fingerprint:016x}.sqlite"
        ));
        if path.exists() {
            return path;
        }
        // Build at a unique scratch path first, then atomically rename into
        // place so concurrent test processes never observe a half-written
        // template. If several processes race, each builds an equivalent
        // file and the renames just overwrite one another; `fs::copy`
        // readers hold their own fd so an overwrite mid-copy is still safe.
        let build = crate::utils::test_fixture_path(format!(
            "memory-server-test-template-build-{}.sqlite",
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
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = build.clone().into_os_string();
            sidecar.push(suffix);
            let _ = std::fs::remove_file(std::path::PathBuf::from(sidecar));
        }
        std::fs::rename(&build, &path).expect("publish template test db fixture");
        path
    })
}

/// Copy the schema-initialized template into `dest` so the caller's
/// subsequent `MemoryServer::new` finds an already-migrated file. The
/// template is checkpointed with `wal_checkpoint(TRUNCATE)` before publish,
/// so the base file alone is a complete snapshot (no sidecars to copy).
fn copy_template_db(dest: &std::path::Path) {
    std::fs::copy(template_db_path(), dest).expect("seed test db from template fixture");
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

fn shell_params(action: &str) -> TachiShellParams {
    TachiShellParams {
        action: action.to_string(),
        format: None,
        flow_id: None,
        task: None,
        title: None,
        agent: None,
        profile: None,
        cwd: None,
        tool_profile: None,
        mcp_access: None,
        allowed_mcp_servers: Vec::new(),
        async_dispatch: false,
        dispatch_reason: None,
        project: None,
        limit: None,
        notes: None,
        validation: Vec::new(),
        allowed_scope: Vec::new(),
        slices: Vec::new(),
    }
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
            store.upsert(entry).expect("seed wiki project entry");
        }
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
        .execute_batch(
            "DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
             DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;
             PRAGMA user_version = 22;",
        )
        .expect("downgrade disposable wiki fixture to v22 guards");
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

mod bootstrap_tests;
mod chain_skills_tests;
mod claims_tests;
mod closure_scan_tests;
mod credential_tests;
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
mod portable_mirror_tests;
mod profile_tests;
mod proxy_tests;
mod sandbox_fold;
mod sandbox_tests;
mod shell_tests;
mod skill_tests;
mod tachi_handoff_tests;
mod vault_tests;
mod vc_tests;
mod wiki_tests;
mod workflow_tests;
