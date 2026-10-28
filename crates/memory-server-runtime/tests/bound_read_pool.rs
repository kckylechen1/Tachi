//! Behavioral discriminators for bound-path read reuse. No timing thresholds.

use memcore::{KernelPolicy, MemoryStore, MigrationAuthority};
use memory_server_runtime::{DbRuntime, ProjectDbState, ReadStorePool, StoreLabel};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

struct Fixture {
    runtime: Option<DbRuntime>,
    root: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new(role: Option<&str>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "tachi-bound-read-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        // create_dir, not create_dir_all: never adopt somebody else's directory.
        std::fs::create_dir(&root).expect("owned fixture directory");
        let global = root.join("global.db");
        let project = root.join("project").join("tachi-memory.db");
        std::fs::create_dir(project.parent().unwrap()).expect("project directory");
        let policy = KernelPolicy::default();
        let global_store =
            MemoryStore::open_with_label(global.to_str().expect("global path"), "global")
                .expect("seed global store");
        let global_vec_available = global_store.vec_available;
        let global_read_pool = ReadStorePool::open_read_only(
            global.to_str().expect("global path"),
            1,
            "global",
            &policy,
        )
        .expect("global read pool");
        let project_seed = match role {
            Some(role) => MemoryStore::open_with_label(project.to_str().expect("path"), role),
            None => MemoryStore::open(project.to_str().expect("path")),
        }
        .expect("seed project");
        drop(project_seed);
        let state = ProjectDbState::open(
            project.clone(),
            1,
            &MigrationAuthority::Deny,
            StoreLabel::inferred(&project),
            &policy,
        )
        .expect("bound state");
        let runtime = DbRuntime {
            global_store: Arc::new(Mutex::new(global_store)),
            global_read_pool,
            global_rw_gate: Arc::new(RwLock::new(())),
            global_contention_recorder: Arc::new(OnceLock::new()),
            global_db_path: Arc::new(global),
            global_vec_available,
            project_db: Arc::new(RwLock::new(Some(state))),
            attached_project_dbs: Arc::new(RwLock::new(HashMap::new())),
            project_attach_init_gate: Arc::new(Mutex::new(())),
            schema_migration: MigrationAuthority::Deny,
            kernel_policy: policy,
        };
        Self {
            runtime: Some(runtime),
            root,
            project,
        }
    }

    fn runtime(&self) -> &DbRuntime {
        self.runtime.as_ref().expect("fixture runtime")
    }

    fn mark_pool(&self) {
        self.runtime()
            .with_project_store_read(|store| {
                // cache_size is connection-local. A newly opened handle will
                // have memcore's default instead of this scratch-only marker.
                store
                    .connection()
                    .pragma_update(None, "cache_size", -321_i64)
                    .map_err(|error| error.to_string())
            })
            .expect("mark bound pool connection");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.runtime.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn cache_size(store: &mut MemoryStore) -> Result<i64, String> {
    store
        .connection()
        .query_row("PRAGMA cache_size", [], |row| row.get(0))
        .map_err(|error| error.to_string())
}

#[test]
fn bound_named_read_uses_the_existing_pool_connection() {
    let fixture = Fixture::new(Some("named"));
    fixture.mark_pool();
    let value = fixture
        .runtime()
        .with_path_store_read_with_label(&fixture.project, "named", cache_size)
        .expect("named read");
    assert_eq!(value, -321, "a second direct read connection was opened");
    assert!(fixture
        .runtime()
        .attached_project_dbs
        .read()
        .unwrap()
        .is_empty());
}

#[test]
fn bound_path_is_not_misclassified_as_an_unattached_read_session() {
    let fixture = Fixture::new(Some("named"));
    let session = fixture
        .runtime()
        .open_unattached_path_store_read_session_with_label(&fixture.project, "named")
        .expect("resolve read route");
    assert!(
        session.is_none(),
        "the existing bound pool must own this read route"
    );
}

#[test]
fn bound_read_does_not_hold_the_unrelated_global_gate() {
    let fixture = Fixture::new(Some("named"));
    let runtime = fixture.runtime();
    runtime
        .with_path_store_read_with_label(&fixture.project, "named", |_| {
            assert!(
                runtime.global_rw_gate.try_write().is_ok(),
                "a bound project read acquired the unrelated global gate"
            );
            Ok(())
        })
        .expect("bound project read");
}

#[test]
fn bound_read_still_refuses_a_conflicting_declared_role() {
    let fixture = Fixture::new(Some("named"));
    let mut called = false;
    let result = fixture.runtime().with_path_store_read_with_label(
        &fixture.project,
        "another-project",
        |_| {
            called = true;
            Ok(())
        },
    );
    assert!(!called, "a conflicting role reached the read closure");
    assert!(result.unwrap_err().contains("store role conflict"));
}

#[test]
fn legacy_project_role_keeps_bare_name_compatibility_and_pool_reuse() {
    let fixture = Fixture::new(Some("project:named"));
    fixture.mark_pool();
    let value = fixture
        .runtime()
        .with_path_store_read_with_label(&fixture.project, "named", cache_size)
        .expect("legacy-compatible read");
    assert_eq!(value, -321);
}

#[test]
fn unstamped_bound_pool_does_not_erase_a_declared_read_identity() {
    let fixture = Fixture::new(None);
    fixture.mark_pool();
    fixture
        .runtime()
        .with_path_store_read_with_label(&fixture.project, "wiki", |store| {
            assert!(store.is_wiki_corpus_store());
            assert_eq!(store.db_label(), "wiki");
            assert_ne!(
                cache_size(store)?,
                -321,
                "unknown pool must not impersonate wiki"
            );
            Ok(())
        })
        .expect("preserve request-owned declared identity");
    fixture
        .runtime()
        .with_project_store_read(|store| {
            assert_eq!(store.db_label(), memcore::path_router::UNKNOWN_DB_LABEL);
            Ok(())
        })
        .expect("bound identity remains unchanged");
}

#[test]
fn missing_read_path_does_not_create_or_attach_a_store() {
    let fixture = Fixture::new(Some("named"));
    let missing = fixture.root.join("missing").join("tachi-memory.db");
    let result = fixture.runtime().with_path_store_read(&missing, |_| Ok(()));
    assert!(result.is_err());
    assert!(!missing.parent().unwrap().exists());
    assert!(fixture
        .runtime()
        .attached_project_dbs
        .read()
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[test]
fn replaced_bound_path_is_refused_before_serving_cached_data() {
    let fixture = Fixture::new(Some("named"));
    let old_directory = fixture.root.join("retired-project");
    // Move the whole SQLite file set together; never mix the old WAL with a new DB.
    std::fs::rename(fixture.project.parent().unwrap(), &old_directory).unwrap();
    std::fs::create_dir(fixture.project.parent().unwrap()).unwrap();
    drop(
        MemoryStore::open_with_label(fixture.project.to_str().unwrap(), "named")
            .expect("replacement scratch store"),
    );
    let mut called = false;
    let result =
        fixture
            .runtime()
            .with_path_store_read_with_label(&fixture.project, "named", |_| {
                called = true;
                Ok(())
            });
    assert!(!called, "a stale bound identity reached the callback");
    assert!(result.unwrap_err().contains("physical identity"));
}
