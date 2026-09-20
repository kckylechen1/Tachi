use super::*;
use rusqlite::{params, Connection};

struct Fixture {
    root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ct-projection-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn open(&self) -> CurrentTruthSqliteStore {
        CurrentTruthSqliteStore::open(self.path().to_str().unwrap()).unwrap()
    }

    fn path(&self) -> std::path::PathBuf {
        self.root.join("truth.sqlite")
    }

    fn open_fixture_connection(&self) -> Connection {
        Connection::open(self.path()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn seed_legacy_rows(conn: &Connection, reverse: bool) {
    let mut rows = [
        (
            "KCKYLECHEN1/TACHI",
            "old",
            "2026-08-26T00:00:00Z",
            "old-view",
        ),
        (REPO, "new", "2026-08-27T00:00:00Z", "new-view"),
    ];
    if reverse {
        rows.reverse();
    }
    for row in rows {
        conn.execute(
            "INSERT INTO current_truth_projection VALUES (?1, ?2, ?3, ?4)",
            row,
        )
        .unwrap();
    }
}

fn assertion_rows(conn: &Connection) -> Vec<Vec<rusqlite::types::Value>> {
    let mut stmt = conn
        .prepare("SELECT * FROM current_truth_assertions ORDER BY assertion_id")
        .unwrap();
    stmt.query_map([], |row| {
        (0..row.as_ref().column_count())
            .map(|index| row.get(index))
            .collect()
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

#[test]
fn mixed_case_projection_rewrite_is_canonical_and_preserves_assertions() {
    for reverse in [false, true] {
        let fixture = Fixture::new();
        let store = fixture.open();
        store.append_all(&mint_assertions(&state_v1())).unwrap();
        let conn = fixture.open_fixture_connection();
        seed_legacy_rows(&conn, reverse);
        conn.execute(
            "INSERT INTO current_truth_projection VALUES ('other/repo', 'other', 'other', 'other')",
            [],
        )
        .unwrap();
        let before = assertion_rows(&conn);
        store
            .write_projection(
                "KcKyleChen1/Tachi",
                "rebuilt",
                "2026-09-21T00:00:00Z",
                "rebuilt-view",
            )
            .unwrap();
        let rows: Vec<(String, String, String)> = conn.prepare(
            "SELECT repo, generation, view_json FROM current_truth_projection WHERE repo = ?1 COLLATE NOCASE"
        ).unwrap().query_map([REPO], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(
            rows,
            vec![(
                REPO.to_string(),
                "rebuilt".to_string(),
                "rebuilt-view".to_string()
            )]
        );
        assert_eq!(
            store
                .read_projection("KCKYLECHEN1/TACHI")
                .unwrap()
                .unwrap()
                .generation,
            "rebuilt"
        );
        assert_eq!(
            store
                .read_projection("other/repo")
                .unwrap()
                .unwrap()
                .generation,
            "other"
        );
        assert_eq!(
            assertion_rows(&conn),
            before,
            "the complete immutable assertion rows stay byte-identical"
        );
    }
}

#[test]
fn mixed_case_legacy_projection_is_cache_miss_and_consumer_rebuilds_from_authority() {
    for reverse in [false, true] {
        let fixture = Fixture::new();
        let store = fixture.open();
        store.append_all(&mint_assertions(&state_v1())).unwrap();
        store
            .record_refresh(
                REPO,
                true,
                Some("r1"),
                Some("2026-08-26T10:00:00Z"),
                "2026-08-26T10:00:00Z",
                None,
            )
            .unwrap();
        let expected = consumer::read_view(
            &store,
            REPO,
            CallerAuthorizationV1 {
                sees_private: false,
            },
        )
        .unwrap();
        let conn = fixture.open_fixture_connection();
        seed_legacy_rows(&conn, reverse);
        for repo in [REPO, "KCKYLECHEN1/TACHI", "KcKyleChen1/Tachi"] {
            assert!(
                store.read_projection(repo).unwrap().is_none(),
                "divergent legacy rows must not choose a truth by insertion order or timestamp"
            );
        }
        assert_eq!(
            consumer::read_view(
                &store,
                REPO,
                CallerAuthorizationV1 {
                    sees_private: false
                }
            )
            .unwrap(),
            expected
        );
        let assertions = store.assertions_for_repo(REPO).unwrap();
        let generation = generation_digest(&assertions);
        let view = serde_json::to_string(&reduce(&assertions).all()).unwrap();
        store
            .write_projection(REPO, &generation, "2026-09-21T00:00:00Z", &view)
            .unwrap();
        assert_eq!(
            store.read_projection(REPO).unwrap().unwrap().view_json,
            view
        );
    }
}

#[test]
fn mixed_case_single_legacy_projection_remains_readable() {
    let fixture = Fixture::new();
    let store = fixture.open();
    fixture.open_fixture_connection().execute("INSERT INTO current_truth_projection VALUES ('KCKYLECHEN1/TACHI', 'legacy', 'at', 'view')", []).unwrap();
    assert_eq!(
        store.read_projection(REPO).unwrap().unwrap().generation,
        "legacy"
    );
}

#[test]
fn mixed_case_projection_insert_failure_rolls_back_entire_rewrite() {
    let fixture = Fixture::new();
    let store = fixture.open();
    store.append_all(&mint_assertions(&state_v1())).unwrap();
    let conn = fixture.open_fixture_connection();
    seed_legacy_rows(&conn, false);
    let before = assertion_rows(&conn);
    // A separate direct connection installs the fault before the real API call.
    // Never remove or weaken a store authorizer to inject this failure.
    conn.execute_batch(
        "CREATE TRIGGER fail_projection_insert BEFORE INSERT ON current_truth_projection
        WHEN NEW.generation = 'rejected'
        BEGIN SELECT RAISE(ABORT, 'projection insertion refused'); END;",
    )
    .unwrap();
    let error = store
        .write_projection(REPO, "rejected", "later", "bad")
        .unwrap_err();
    assert!(error.to_string().contains("projection insertion refused"));
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT repo, generation FROM current_truth_projection ORDER BY repo")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("KCKYLECHEN1/TACHI".to_string(), "old".to_string()),
            (REPO.to_string(), "new".to_string())
        ]
    );
    assert_eq!(assertion_rows(&conn), before);
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM current_truth_projection WHERE generation = ?1",
            params!["rejected"],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
