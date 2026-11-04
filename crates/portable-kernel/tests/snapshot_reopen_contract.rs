use portable_kernel::{DbOpenContext, MemoryEntry, MemoryStore, PortableImportEntry, StoreProfile};
use rusqlite::{types::Value, Connection};
use std::path::Path;

const ROLE: &str = "snapshot-reopen-contract";

fn context(profile: StoreProfile, context: DbOpenContext) -> DbOpenContext {
    context.with_exact_profile(profile).with_exact_role(ROLE)
}

fn snapshot(id: &str) -> PortableImportEntry {
    PortableImportEntry {
        entry: MemoryEntry {
            id: id.into(),
            path: format!("/snapshot/{id}"),
            summary: "synthetic historical snapshot".into(),
            text: format!("synthetic reopen fixture {id}"),
            importance: 0.7,
            timestamp: "2026-08-01T10:00:00+00:00".into(),
            valid_from: String::new(),
            valid_until: Some(String::new()),
            category: "fact".into(),
            topic: String::new(),
            keywords: vec!["snapshot".into()],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "external:snapshot-contract".into(),
            scope: "general".into(),
            archived: true,
            access_count: 11,
            scored_count: 19,
            last_access: Some("2026-08-02T10:00:00Z".into()),
            last_use_at: Some("2026-08-03T10:00:00Z".into()),
            revision: 0,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({"synthetic": true}),
            recall_count: 5,
            query_diversity: 3,
            tier: "raw".into(),
        },
        created_at: String::new(),
        updated_at: String::new(),
        superseded_by: Some("snapshot-active".into()),
    }
}

fn fixture(path: &Path, profile: StoreProfile) -> Vec<Vec<Value>> {
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(portable_kernel::IS_PORTABLE_BUILD);
        assert!(!portable_kernel::ADMIN_SURFACE_ENABLED);
    }
    let mut store = MemoryStore::open_with_context(
        path.to_str().unwrap(),
        &context(profile, DbOpenContext::create_fresh()),
    )
    .unwrap();
    let mut active = snapshot("snapshot-active");
    active.entry.archived = false;
    active.entry.revision = 7;
    active.entry.valid_from = "2026-08-01T10:00:00Z".into();
    active.entry.valid_until = None;
    active.created_at = "2026-07-01T10:00:00Z".into();
    active.updated_at = "2026-07-02T10:00:00Z".into();
    active.superseded_by = None;
    store
        .import_snapshot_batch(&[active, snapshot("snapshot-edge")])
        .unwrap();
    drop(store);
    let connection = Connection::open(path).unwrap();
    let historical_values: bool = connection
        .query_row(
            "SELECT created_at='' AND updated_at='' AND revision=0
                 AND valid_from='' AND valid_until='' AND archived=1
                 AND superseded_by='snapshot-active'
             FROM memories WHERE id='snapshot-edge'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        historical_values,
        "the public import must preserve the input"
    );
    raw_rows(&connection)
}

fn raw_rows(connection: &Connection) -> Vec<Vec<Value>> {
    let mut statement = connection
        .prepare("SELECT * FROM memories ORDER BY id")
        .unwrap();
    let width = statement.column_count();
    statement
        .query_map([], |row| (0..width).map(|index| row.get(index)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn projection_ids(connection: &Connection, table: &str) -> Vec<String> {
    connection
        .prepare(&format!("SELECT id FROM {table} ORDER BY id"))
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn current_reopens_preserve_snapshot_values_and_still_repair_fts() {
    for profile in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.db");
        let expected = fixture(&path, profile);
        for authority in [
            DbOpenContext::open_existing_deny(),
            DbOpenContext::open_existing_allow("synthetic-owner"),
        ] {
            for _ in 0..2 {
                let connection = Connection::open(&path).unwrap();
                connection
                    .execute_batch(
                        "DELETE FROM memories_fts WHERE id='snapshot-edge';
                         DELETE FROM memories_symbolic_fts WHERE id='snapshot-edge';",
                    )
                    .unwrap();
                assert_eq!(
                    projection_ids(&connection, "memories_fts"),
                    ["snapshot-active"]
                );
                assert_eq!(
                    projection_ids(&connection, "memories_symbolic_fts"),
                    ["snapshot-active"]
                );
                drop(connection);
                drop(
                    MemoryStore::open_with_context(
                        path.to_str().unwrap(),
                        &context(profile, authority.clone()),
                    )
                    .unwrap(),
                );
                let connection = Connection::open(&path).unwrap();
                assert_eq!(raw_rows(&connection), expected, "{profile:?}");
                for table in ["memories_fts", "memories_symbolic_fts"] {
                    assert_eq!(
                        projection_ids(&connection, table),
                        ["snapshot-active", "snapshot-edge"],
                        "current reopen must retain {table} drift repair"
                    );
                }
            }
        }
    }
}

#[test]
fn public_initializer_preserves_current_snapshot_values() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("snapshot.db");
    let expected = fixture(&path, StoreProfile::TachiFull);
    for _ in 0..2 {
        let connection = Connection::open(&path).unwrap();
        portable_kernel::db::init_schema(&connection).unwrap();
        assert_eq!(raw_rows(&connection), expected);
    }
}
