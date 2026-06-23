use super::*;

#[test]
fn r9_domain_backfill_repairs_missing_and_path_like_values() {
    let dir = TempDir::new().unwrap();
    let (path, conn) = fresh_db(&dir, "domains.db");
    insert_memory(&conn, "m1", "/wiki/agent/tachi", "wiki", "{}", None, None);
    insert_memory(&conn, "m2", "/scratch/repro", "scratch", "{}", None, None);
    insert_memory(
        &conn,
        "m3",
        "/trading/equity/positions",
        "trade",
        "{}",
        None,
        None,
    );
    conn.execute(
        "UPDATE memories SET domain = '/scratch/sigil' WHERE id = 'm2'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE memories SET domain = 'Hyperion' WHERE id = 'm3'",
        [],
    )
    .unwrap();
    drop(conn);

    let mut ctx = open_ctx(&path, "test");
    let dry = DomainRepair.dry_run(&mut ctx).unwrap();
    assert!(
        dry.findings.iter().any(|f| f.kind == "domain_repaired"),
        "expected domain repair finding, got {dry:?}"
    );

    let app = DomainRepair.apply(&mut ctx).unwrap();
    assert_eq!(app.applied, 3);

    let conn = Connection::open(&path).unwrap();
    let domains: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, domain FROM memories ORDER BY id")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        domains,
        vec![
            ("m1".to_string(), "wiki".to_string()),
            ("m2".to_string(), "scratch".to_string()),
            ("m3".to_string(), "hyperion".to_string()),
        ]
    );
}
