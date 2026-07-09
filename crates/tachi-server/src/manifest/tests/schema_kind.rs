use super::*;

#[test]
fn classify_db_schema_distinguishes_kinds() {
    let dir = tempdir().unwrap();

    // Tachi-shaped DB.
    let tachi_path = dir.path().join("tachi.db");
    let conn = rusqlite::Connection::open(&tachi_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE memories (
                id TEXT PRIMARY KEY, path TEXT NOT NULL DEFAULT '/',
                summary TEXT, text TEXT, importance REAL, timestamp TEXT NOT NULL,
                category TEXT, topic TEXT
            );",
    )
    .unwrap();
    drop(conn);
    assert_eq!(classify_db_schema(&tachi_path), SchemaKind::Tachi);

    // OpenClaw chunks-shaped DB.
    let chunks_path = dir.path().join("chunks.db");
    let conn = rusqlite::Connection::open(&chunks_path).unwrap();
    conn.execute_batch("CREATE TABLE chunks (id TEXT PRIMARY KEY);")
        .unwrap();
    drop(conn);
    assert_eq!(classify_db_schema(&chunks_path), SchemaKind::OpenclawChunks);

    // Empty SQLite file → Unknown.
    let empty_path = dir.path().join("empty.db");
    let conn = rusqlite::Connection::open(&empty_path).unwrap();
    drop(conn);
    assert_eq!(classify_db_schema(&empty_path), SchemaKind::Unknown);

    // Missing file → Unknown.
    let missing = dir.path().join("nope.db");
    assert_eq!(classify_db_schema(&missing), SchemaKind::Unknown);
}

#[test]
fn schema_kind_serde_roundtrip() {
    let json = serde_json::to_string(&SchemaKind::Tachi).unwrap();
    assert_eq!(json, "\"tachi\"");
    // Wire form: openclaw_legacy (preserves backward-compat with the
    // existing manifest schema_kind string).
    let json = serde_json::to_string(&SchemaKind::OpenclawChunks).unwrap();
    assert_eq!(json, "\"openclaw_legacy\"");
    let back: SchemaKind = serde_json::from_str("\"openclaw_legacy\"").unwrap();
    assert_eq!(back, SchemaKind::OpenclawChunks);
}
