//! Frozen historical-source fixtures for D7 T1b. No current initializer is used.
//! These are source reconstructions, not receipts of deployed database lineage.
//! Before D3/v28 there was no Portable profile: these prefixes project the
//! historical kernel table components. Prototype Wiki tables/indexes that
//! preceded the v28 schema boundary are excluded until v28; compatibility
//! with those prototypes is not proved here. v25/v26 shipped together, so
//! the v25 component is pinned from fb02c415's frozen V25 literal.
//! P36 adds original 2b3951a4 initializer enum/index maintenance, plus the
//! repaired v36 delivery shape from 37a7981d^ (before v37 introduction).
//! The original 2b3951a4 delivery shape fails today's validator because its
//! global claim/ack indexes were added during v36; that shape is not proved. Generation
//! maintenance is a separately labeled fixed 2126bacf bridge (policy §8 A6).
use super::*;
pub(super) fn install(conn: &Connection, migration: u32) {
    let sql: &[&str] = match migration {
        // predecessor v12: 3fec84ceba336fd35d6a46d9eac1e686eeee582e crates/memcore/src/db/schema/ddl.rs
        13 => &[
            SQL_0, SQL_1, SQL_2, SQL_3, SQL_4, SQL_5, SQL_6, SQL_7, SQL_8, SQL_9, SQL_10, SQL_11,
            SQL_12, SQL_13, SQL_14, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19, SQL_20, SQL_21, SQL_22,
            SQL_23, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30, SQL_31, SQL_32, SQL_33,
            SQL_34, SQL_35,
        ],
        // predecessor v18: 6661d576dfa2a46e6c211fbcbe9e42cd2d7cad40 crates/memcore/src/db/schema/ddl.rs
        19 => &[
            SQL_0, SQL_1, SQL_2, SQL_3, SQL_4, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37, SQL_38,
            SQL_9, SQL_10, SQL_11, SQL_12, SQL_13, SQL_14, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_23, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35,
        ],
        // predecessor v21: 70d982664e53a78ece6e0e19fa670b7a827cf66d crates/memcore/src/db/schema/ddl.rs
        22 => &[
            SQL_39, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_10, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_23, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_42,
        ],
        // predecessor v22: e9a74946566054c0f5459ae12ce0a70580f0f99b crates/memcore/src/db/schema/ddl.rs
        23 => &[
            SQL_39, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_10, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_42,
        ],
        // predecessor v23: 48a6cfd32e545fbda88885feca0f9ad8f66b5925 crates/memcore/src/db/schema/ddl.rs
        24 => &[
            SQL_44, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47,
        ],
        // predecessor v24: 55574baf246b32039a8425d66235bce2d39651f0 crates/memcore/src/db/schema/ddl.rs
        25 => &[
            SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47,
        ],
        // predecessor v25: 70a18102c13935c2290aa772b6751a14a2170241 crates/memcore/src/db/schema/ddl.rs
        26 => &[
            V25, SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36,
            SQL_37, SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18,
            SQL_19, SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29,
            SQL_30, SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47,
        ],
        // predecessor v26: fb02c415b115a8abfb760d6dd94777fa50a256c8 crates/memcore/src/db/schema/ddl.rs
        27 => &[
            SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47, SQL_49,
        ],
        // predecessor v27: 221f3de6a30341ec82cf38311862d1e1161175eb crates/memcore/src/db/schema/ddl.rs
        28 => &[
            SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47, SQL_49,
        ],
        // predecessor v28: 8394d762dadf969af25907cc8fbb2a706f32b9a7 crates/memcore/src/db/schema/ddl.rs
        29 => &[
            SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47, SQL_49, SQL_50,
        ],
        // predecessor v29: fff33777be127cbf5f589a25515086e4215ad059 crates/memcore/src/db/schema/ddl.rs
        30 => &[
            SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47, SQL_49, SQL_50,
            SQL_51,
        ],
        // predecessor v32: ebb95a47817d796b427bd826a3a78a479a8e8c58 crates/memcore/src/db/schema/ddl.rs
        33 => &[
            SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47, SQL_49, SQL_50,
            SQL_51, SQL_52,
        ],
        // predecessor v33: 6f2a548708d294c56986b91869d73f39c4572fa7 crates/memcore/src/db/schema/ddl.rs
        34 => &[
            SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47, SQL_49, SQL_53,
            SQL_50, SQL_51, SQL_52,
        ],
        // predecessor v34: a86350a5020c1a9c14cc448627ae4a92185da78e crates/memcore/src/db/schema/ddl.rs
        35 => &[
            SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47, SQL_49, SQL_53,
            SQL_54, SQL_50, SQL_51, SQL_52,
        ],
        // predecessor v35: 76bba43f86631404406d9115cb0d0d2f460baf24 crates/memcore/src/db/schema/ddl.rs
        36 => &[
            SQL_48, SQL_1, SQL_2, SQL_3, SQL_4, SQL_40, SQL_5, SQL_6, SQL_7, SQL_8, SQL_36, SQL_37,
            SQL_38, SQL_9, SQL_45, SQL_11, SQL_12, SQL_13, SQL_15, SQL_16, SQL_17, SQL_18, SQL_19,
            SQL_20, SQL_21, SQL_22, SQL_43, SQL_24, SQL_25, SQL_26, SQL_27, SQL_28, SQL_29, SQL_30,
            SQL_31, SQL_32, SQL_33, SQL_34, SQL_35, SQL_41, SQL_46, SQL_42, SQL_47, SQL_49, SQL_53,
            SQL_55, SQL_50, SQL_51, SQL_52,
        ],
        // predecessor v36: 2b3951a43e852341dcc455ae9a62490145673d66 crates/memcore/src/db/schema/ddl.rs
        37 => &[
            SQL_48,
            SQL_1,
            SQL_2,
            SQL_3,
            SQL_4,
            SQL_40,
            SQL_5,
            SQL_6,
            SQL_7,
            SQL_8,
            SQL_36,
            SQL_37,
            SQL_38,
            SQL_9,
            SQL_45,
            SQL_11,
            SQL_12,
            SQL_13,
            SQL_15,
            SQL_16,
            SQL_17,
            SQL_18,
            SQL_19,
            SQL_20,
            SQL_21,
            SQL_22,
            SQL_43,
            SQL_24,
            SQL_25,
            SQL_26,
            SQL_27,
            SQL_28,
            SQL_29,
            SQL_30,
            SQL_31,
            SQL_32,
            SQL_33,
            SQL_34,
            SQL_35,
            SQL_41,
            SQL_46,
            SQL_42,
            SQL_47,
            SQL_49,
            SQL_53,
            SQL_55,
            P36_DELIVERY_REPAIRED,
            SQL_50,
            SQL_51,
            SQL_52,
        ],
        _ => panic!("no historical prefix for v{migration}"),
    };
    for statement in sql {
        if !statement.trim_start().starts_with("CREATE INDEX")
            && !statement.trim_start().starts_with("CREATE UNIQUE INDEX")
        {
            conn.execute_batch(statement).unwrap();
        }
    }
    let columns: &[(&str, &str, &str)] = match migration {
        13 | 19 => &[
            ("memories", "archived", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "created_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "updated_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "revision", "INTEGER NOT NULL DEFAULT 1"),
            ("memories", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "valid_until", "TEXT"),
            ("memories", "retention_policy", "TEXT"),
            ("memories", "domain", "TEXT"),
            ("memories", "superseded_by", "TEXT"),
            ("memories", "recall_count", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "query_diversity", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "tier", "TEXT NOT NULL DEFAULT 'raw'"),
            ("access_history", "query_hash", "TEXT"),
            ("memory_edges", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memory_edges", "valid_to", "TEXT"),
            ("derived_items", "summary", "TEXT NOT NULL DEFAULT ''"),
            ("derived_items", "importance", "REAL NOT NULL DEFAULT 0.5"),
            ("derived_items", "scope", "TEXT NOT NULL DEFAULT 'general'"),
            ("derived_items", "created_at", "TEXT NOT NULL DEFAULT ''"),
        ],
        22 => &[
            ("memories", "archived", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "created_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "updated_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "revision", "INTEGER NOT NULL DEFAULT 1"),
            ("memories", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "valid_until", "TEXT"),
            ("memories", "retention_policy", "TEXT"),
            ("memories", "domain", "TEXT"),
            ("memories", "superseded_by", "TEXT"),
            ("memories", "idless_identity", "TEXT"),
            ("memories", "recall_count", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "query_diversity", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "tier", "TEXT NOT NULL DEFAULT 'raw'"),
            ("access_history", "query_hash", "TEXT"),
            ("memory_edges", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memory_edges", "valid_to", "TEXT"),
            ("derived_items", "summary", "TEXT NOT NULL DEFAULT ''"),
            ("derived_items", "importance", "REAL NOT NULL DEFAULT 0.5"),
            ("derived_items", "scope", "TEXT NOT NULL DEFAULT 'general'"),
            ("derived_items", "created_at", "TEXT NOT NULL DEFAULT ''"),
        ],
        23 => &[
            (
                "recall_cache",
                "generation_fingerprint",
                "TEXT NOT NULL DEFAULT ''",
            ),
            ("memories", "archived", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "created_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "updated_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "revision", "INTEGER NOT NULL DEFAULT 1"),
            ("memories", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "valid_until", "TEXT"),
            ("memories", "retention_policy", "TEXT"),
            ("memories", "domain", "TEXT"),
            ("memories", "superseded_by", "TEXT"),
            ("memories", "idless_identity", "TEXT"),
            ("memories", "recall_count", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "query_diversity", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "tier", "TEXT NOT NULL DEFAULT 'raw'"),
            ("access_history", "query_hash", "TEXT"),
            ("memory_edges", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memory_edges", "valid_to", "TEXT"),
            ("derived_items", "summary", "TEXT NOT NULL DEFAULT ''"),
            ("derived_items", "importance", "REAL NOT NULL DEFAULT 0.5"),
            ("derived_items", "scope", "TEXT NOT NULL DEFAULT 'general'"),
            ("derived_items", "created_at", "TEXT NOT NULL DEFAULT ''"),
        ],
        24 => &[
            (
                "recall_cache",
                "generation_fingerprint",
                "TEXT NOT NULL DEFAULT ''",
            ),
            ("memories", "archived", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "created_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "updated_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "revision", "INTEGER NOT NULL DEFAULT 1"),
            ("memories", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "valid_until", "TEXT"),
            ("memories", "retention_policy", "TEXT"),
            ("memories", "domain", "TEXT"),
            ("memories", "superseded_by", "TEXT"),
            ("memories", "idless_identity", "TEXT"),
            ("memories", "recall_count", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "query_diversity", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "tier", "TEXT NOT NULL DEFAULT 'raw'"),
            ("memories", "last_use_at", "TEXT"),
            ("access_history", "query_hash", "TEXT"),
            (
                "access_history",
                "event_kind",
                "TEXT NOT NULL DEFAULT 'display'",
            ),
            ("memory_edges", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memory_edges", "valid_to", "TEXT"),
            ("derived_items", "summary", "TEXT NOT NULL DEFAULT ''"),
            ("derived_items", "importance", "REAL NOT NULL DEFAULT 0.5"),
            ("derived_items", "scope", "TEXT NOT NULL DEFAULT 'general'"),
            ("derived_items", "created_at", "TEXT NOT NULL DEFAULT ''"),
        ],
        25 | 26 | 27 | 28 | 29 | 30 | 33 | 34 | 35 | 36 | 37 => &[
            (
                "recall_cache",
                "generation_fingerprint",
                "TEXT NOT NULL DEFAULT ''",
            ),
            ("memories", "archived", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "created_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "updated_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "revision", "INTEGER NOT NULL DEFAULT 1"),
            ("memories", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "valid_until", "TEXT"),
            ("memories", "retention_policy", "TEXT"),
            ("memories", "domain", "TEXT"),
            ("memories", "superseded_by", "TEXT"),
            ("memories", "idless_identity", "TEXT"),
            ("memories", "recall_count", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "query_diversity", "INTEGER NOT NULL DEFAULT 0"),
            ("memories", "tier", "TEXT NOT NULL DEFAULT 'raw'"),
            ("memories", "last_use_at", "TEXT"),
            ("access_history", "query_hash", "TEXT"),
            (
                "access_history",
                "event_kind",
                "TEXT NOT NULL DEFAULT 'display'",
            ),
            ("memory_edges", "valid_from", "TEXT NOT NULL DEFAULT ''"),
            ("memory_edges", "valid_to", "TEXT"),
            ("derived_items", "summary", "TEXT NOT NULL DEFAULT ''"),
            ("derived_items", "importance", "REAL NOT NULL DEFAULT 0.5"),
            ("derived_items", "scope", "TEXT NOT NULL DEFAULT 'general'"),
            ("derived_items", "created_at", "TEXT NOT NULL DEFAULT ''"),
            ("memories", "scored_count", "INTEGER NOT NULL DEFAULT 0"),
        ],
        _ => unreachable!(),
    };
    for (table, column, ddl) in columns {
        if !table_has_column(conn, table, column).unwrap() {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}"))
                .unwrap();
        }
    }
    for statement in sql {
        if statement.trim_start().starts_with("CREATE INDEX")
            || statement.trim_start().starts_with("CREATE UNIQUE INDEX")
        {
            conn.execute_batch(statement).unwrap();
        }
    }
    if migration == 35 {
        crate::db::migrations::harness_session_events::install_shipped_v34_receipt_tables_for_test(
            conn,
        );
    }
    if migration == 37 {
        conn.execute_batch(P36_ENUM_REBUILD).unwrap();
        conn.execute_batch(P36_GUARDS).unwrap();
    }
    if migration >= 22 {
        conn.execute_batch(FROZEN_SEARCH_MAINTENANCE).unwrap();
    }
    if migration >= 28 {
        for (table, column) in [
            ("recall_impression_groups", "typo_fallback_activated"),
            ("recall_impression_groups", "typo_fallback_prefilter_count"),
            ("recall_impression_groups", "typo_fallback_compared_count"),
            (
                "recall_impression_groups",
                "typo_fallback_token_comparison_count",
            ),
            ("recall_impression_groups", "typo_fallback_edit_cell_count"),
            ("recall_impression_groups", "typo_fallback_candidate_count"),
            ("recall_impressions", "typo_fallback_candidate"),
        ] {
            if !table_has_column(conn, table, column).unwrap() {
                conn.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0"
                ))
                .unwrap();
            }
        }
    }
}
const SQL_0: &str = r###"CREATE TABLE IF NOT EXISTS memories (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            valid_from   TEXT NOT NULL DEFAULT '',
            valid_until  TEXT,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
                access_count    INTEGER NOT NULL DEFAULT 0,
                last_access     TEXT,
                revision        INTEGER NOT NULL DEFAULT 1,
                metadata        TEXT NOT NULL DEFAULT '{}',
                superseded_by   TEXT,
                recall_count    INTEGER NOT NULL DEFAULT 0,
                query_diversity INTEGER NOT NULL DEFAULT 0,
                tier            TEXT NOT NULL DEFAULT 'raw'
            );"###;
const SQL_1: &str = r###"CREATE INDEX IF NOT EXISTS idx_memories_path        ON memories(path);"###;
const SQL_2: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_memories_importance  ON memories(importance DESC);"###;
const SQL_3: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_memories_timestamp   ON memories(timestamp DESC);"###;
const SQL_4: &str = r###"CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            id UNINDEXED,
            path,
            summary,
            text,
            keywords,
            entities,
            tokenize = 'simple'
        );"###;
const SQL_5: &str = r###"CREATE TABLE IF NOT EXISTS memory_edges (
            source_id  TEXT NOT NULL,
            target_id  TEXT NOT NULL,
            relation   TEXT NOT NULL,
            weight     REAL NOT NULL DEFAULT 1.0,
            metadata   TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (source_id, target_id, relation)
        );"###;
const SQL_6: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_edges_source ON memory_edges(source_id);"###;
const SQL_7: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_edges_target ON memory_edges(target_id);"###;
const SQL_8: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_edges_relation ON memory_edges(relation);"###;
const SQL_9: &str = r###"CREATE TABLE IF NOT EXISTS hard_state (
            namespace        TEXT NOT NULL,
            key              TEXT NOT NULL,
            value_json       TEXT NOT NULL DEFAULT '{}',
            version          INTEGER NOT NULL DEFAULT 1,
            created_at       TEXT NOT NULL DEFAULT '',
            updated_at       TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (namespace, key)
        );"###;
const SQL_10: &str = r###"CREATE TABLE IF NOT EXISTS access_history (
            memory_id  TEXT NOT NULL,
            accessed_at TEXT NOT NULL,
            query_hash  TEXT NOT NULL DEFAULT ''
        );"###;
const SQL_11: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_access_hist_mem ON access_history(memory_id);"###;
const SQL_12: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_access_hist_time ON access_history(accessed_at DESC);"###;
const SQL_13: &str = r###"CREATE INDEX IF NOT EXISTS idx_access_hist_mem_time ON access_history(memory_id, accessed_at DESC);"###;
const SQL_14: &str = r###"CREATE INDEX IF NOT EXISTS idx_access_hist_hash ON access_history(memory_id, query_hash) WHERE query_hash != '';"###;
const SQL_15: &str = r###"CREATE TABLE IF NOT EXISTS derived_items (
            id         TEXT PRIMARY KEY,
            text       TEXT NOT NULL DEFAULT '',
            path       TEXT NOT NULL DEFAULT '/',
            summary    TEXT NOT NULL DEFAULT '',
            importance REAL NOT NULL DEFAULT 0.5,
            source     TEXT NOT NULL DEFAULT '',
            scope      TEXT NOT NULL DEFAULT 'general',
            metadata   TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT ''
        );"###;
const SQL_16: &str = r###"CREATE TABLE IF NOT EXISTS processed_events (
            event_hash TEXT NOT NULL,
            event_id   TEXT NOT NULL DEFAULT '',
            worker     TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (event_hash, worker)
        );"###;
const SQL_17: &str = r###"CREATE INDEX IF NOT EXISTS idx_processed_events_created_at ON processed_events(created_at DESC);"###;
const SQL_18: &str = r###"CREATE TABLE IF NOT EXISTS tachi_events (
            id               TEXT PRIMARY KEY,
            source_repo      TEXT NOT NULL DEFAULT '',
            adapter          TEXT NOT NULL DEFAULT '',
            project          TEXT NOT NULL DEFAULT '',
            domain           TEXT NOT NULL DEFAULT '',
            session_id       TEXT NOT NULL DEFAULT '',
            actor            TEXT NOT NULL DEFAULT '',
            event_type       TEXT NOT NULL,
            authority        TEXT NOT NULL DEFAULT 'collect_only',
            effects          TEXT NOT NULL DEFAULT '[]',
            projection_hints TEXT NOT NULL DEFAULT '[]',
            payload_json     TEXT NOT NULL DEFAULT '{}',
            provenance_json  TEXT NOT NULL DEFAULT '{}',
            created_at       TEXT NOT NULL
        );"###;
const SQL_19: &str = r###"CREATE INDEX IF NOT EXISTS idx_tachi_events_created_at ON tachi_events(created_at DESC);"###;
const SQL_20: &str = r###"CREATE INDEX IF NOT EXISTS idx_tachi_events_project_domain ON tachi_events(project, domain, created_at DESC);"###;
const SQL_21: &str = r###"CREATE INDEX IF NOT EXISTS idx_tachi_events_type ON tachi_events(event_type, created_at DESC);"###;
const SQL_22: &str = r###"CREATE INDEX IF NOT EXISTS idx_tachi_events_session ON tachi_events(session_id, created_at DESC);"###;
const SQL_23: &str = r###"CREATE TABLE IF NOT EXISTS recall_cache (
            cache_id      TEXT PRIMARY KEY,
            query         TEXT NOT NULL DEFAULT '',
            rows_json     TEXT NOT NULL DEFAULT '[]',
            result_count  INTEGER NOT NULL DEFAULT 0,
            reranked      INTEGER NOT NULL DEFAULT 0,
            hit_count     INTEGER NOT NULL DEFAULT 0,
            created_at    TEXT NOT NULL DEFAULT '',
            updated_at    TEXT NOT NULL DEFAULT '',
            last_hit_at   TEXT
        );"###;
const SQL_24: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_recall_cache_updated ON recall_cache(updated_at);"###;
const SQL_25: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_memories_archived    ON memories(archived);"###;
const SQL_26: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_memories_last_access ON memories(last_access DESC);"###;
const SQL_27: &str = r###"CREATE INDEX IF NOT EXISTS idx_memories_valid_time  ON memories(valid_from, valid_until);"###;
const SQL_28: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_derived_source       ON derived_items(source);"###;
const SQL_29: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_derived_path         ON derived_items(path);"###;
const SQL_30: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_derived_created_at   ON derived_items(created_at DESC);"###;
const SQL_31: &str = r###"CREATE INDEX IF NOT EXISTS idx_memories_retention_policy ON memories(retention_policy);"###;
const SQL_32: &str = r###"CREATE INDEX IF NOT EXISTS idx_memories_domain ON memories(domain);"###;
const SQL_33: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_memories_superseded ON memories(superseded_by);"###;
const SQL_34: &str = r###"CREATE INDEX IF NOT EXISTS idx_memories_tier ON memories(tier);"###;
const SQL_35: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_memories_recall ON memories(recall_count DESC);"###;
const SQL_36: &str = r###"CREATE TABLE IF NOT EXISTS edge_observations (
            observation_id     TEXT PRIMARY KEY,
            source_id          TEXT NOT NULL,
            target_id          TEXT NOT NULL,
            relation           TEXT NOT NULL,
            capture_event_kind TEXT NOT NULL DEFAULT 'unknown',
            capture_event_id   TEXT NOT NULL DEFAULT '',
            actor              TEXT NOT NULL DEFAULT 'unknown',
            reason_code        TEXT NOT NULL DEFAULT '',
            observed_at        TEXT NOT NULL,
            evidence_hash      TEXT,
            invalidated_at     TEXT
        );"###;
const SQL_37: &str = r###"CREATE INDEX IF NOT EXISTS idx_edge_obs_edge
            ON edge_observations(source_id, target_id, relation);"###;
const SQL_38: &str = r###"CREATE INDEX IF NOT EXISTS idx_edge_obs_observed_at
            ON edge_observations(observed_at);"###;
const SQL_39: &str = r###"CREATE TABLE IF NOT EXISTS memories (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            valid_from   TEXT NOT NULL DEFAULT '',
            valid_until  TEXT,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
                access_count    INTEGER NOT NULL DEFAULT 0,
                last_access     TEXT,
                revision        INTEGER NOT NULL DEFAULT 1,
                metadata        TEXT NOT NULL DEFAULT '{}',
                superseded_by   TEXT,
                idless_identity TEXT,
                recall_count    INTEGER NOT NULL DEFAULT 0,
                query_diversity INTEGER NOT NULL DEFAULT 0,
                tier            TEXT NOT NULL DEFAULT 'raw'
            );"###;
const SQL_40: &str = r###"CREATE VIRTUAL TABLE IF NOT EXISTS memories_symbolic_fts USING fts5(
            id,
            path,
            summary,
            text,
            keywords,
            entities,
            topic,
            tokenize = 'trigram case_sensitive 0'
        );"###;
const SQL_41: &str = r###"CREATE INDEX IF NOT EXISTS idx_access_hist_hash
            ON access_history(memory_id, query_hash) WHERE query_hash != '';"###;
const SQL_42: &str = r###"CREATE INDEX IF NOT EXISTS idx_hard_state_ns_updated
            ON hard_state(namespace, updated_at DESC);"###;
const SQL_43: &str = r###"CREATE TABLE IF NOT EXISTS recall_cache (
            cache_id      TEXT PRIMARY KEY,
            generation_fingerprint TEXT NOT NULL DEFAULT '',
            query         TEXT NOT NULL DEFAULT '',
            rows_json     TEXT NOT NULL DEFAULT '[]',
            result_count  INTEGER NOT NULL DEFAULT 0,
            reranked      INTEGER NOT NULL DEFAULT 0,
            hit_count     INTEGER NOT NULL DEFAULT 0,
            created_at    TEXT NOT NULL DEFAULT '',
            updated_at    TEXT NOT NULL DEFAULT '',
            last_hit_at   TEXT
        );"###;
const SQL_44: &str = r###"CREATE TABLE IF NOT EXISTS memories (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            valid_from   TEXT NOT NULL DEFAULT '',
            valid_until  TEXT,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
                
                
                
                
                
                
                
                
                
                access_count    INTEGER NOT NULL DEFAULT 0,
                last_access     TEXT,
                
                
                
                last_use_at     TEXT,
                revision        INTEGER NOT NULL DEFAULT 1,
                metadata        TEXT NOT NULL DEFAULT '{}',
                superseded_by   TEXT,
                idless_identity TEXT,
                
                
                
                
                
                
                
                recall_count    INTEGER NOT NULL DEFAULT 0,
                query_diversity INTEGER NOT NULL DEFAULT 0,
                tier            TEXT NOT NULL DEFAULT 'raw'
            );"###;
const SQL_45: &str = r###"CREATE TABLE IF NOT EXISTS access_history (
            memory_id  TEXT NOT NULL,
            accessed_at TEXT NOT NULL,
            query_hash  TEXT NOT NULL DEFAULT '',
            event_kind  TEXT NOT NULL DEFAULT 'display'
        );"###;
const SQL_46: &str = r###"CREATE INDEX IF NOT EXISTS idx_access_hist_mem_kind_time
            ON access_history(memory_id, event_kind, accessed_at DESC);"###;
const SQL_47: &str = r###"
        DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
        DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;

        CREATE TRIGGER memories_reserved_refs_insert_guard
        BEFORE INSERT ON memories
        WHEN (
             json_type(NEW.metadata, '$.evidence_refs_v1') IS NOT NULL
             OR json_type(NEW.metadata, '$.source_refs') IS NOT NULL
         )
         AND NOT EXISTS (
             SELECT 1
             FROM memories AS current
             WHERE current.id = NEW.id
               AND json_type(current.metadata, '$.evidence_refs_v1')
                   IS json_type(NEW.metadata, '$.evidence_refs_v1')
               AND json_quote(json_extract(current.metadata, '$.evidence_refs_v1'))
                   IS json_quote(json_extract(NEW.metadata, '$.evidence_refs_v1'))
               AND json_type(current.metadata, '$.source_refs')
                   IS json_type(NEW.metadata, '$.source_refs')
               AND json_quote(json_extract(current.metadata, '$.source_refs'))
                   IS json_quote(json_extract(NEW.metadata, '$.source_refs'))
         )
         AND tachi_reserved_reference_write_enabled() = 0
        BEGIN
            SELECT RAISE(ABORT, 'reserved memory reference metadata requires typed mutation');
        END;

        CREATE TRIGGER memories_reserved_refs_update_guard
        BEFORE UPDATE OF metadata ON memories
        WHEN (
             json_type(NEW.metadata, '$.evidence_refs_v1')
                 IS NOT json_type(OLD.metadata, '$.evidence_refs_v1')
             OR json_quote(json_extract(NEW.metadata, '$.evidence_refs_v1'))
                 IS NOT json_quote(json_extract(OLD.metadata, '$.evidence_refs_v1'))
             OR json_type(NEW.metadata, '$.source_refs')
                 IS NOT json_type(OLD.metadata, '$.source_refs')
             OR json_quote(json_extract(NEW.metadata, '$.source_refs'))
                 IS NOT json_quote(json_extract(OLD.metadata, '$.source_refs'))
         )
         AND tachi_reserved_reference_write_enabled() = 0
        BEGIN
            SELECT RAISE(ABORT, 'reserved memory reference metadata requires typed mutation');
        END;
"###;
const SQL_48: &str = r###"CREATE TABLE IF NOT EXISTS memories (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            valid_from   TEXT NOT NULL DEFAULT '',
            valid_until  TEXT,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
                
                
                
                
                
                
                
                
                
                access_count    INTEGER NOT NULL DEFAULT 0,
                scored_count    INTEGER NOT NULL DEFAULT 0,
                last_access     TEXT,
                
                
                
                last_use_at     TEXT,
                revision        INTEGER NOT NULL DEFAULT 1,
                metadata        TEXT NOT NULL DEFAULT '{}',
                superseded_by   TEXT,
                idless_identity TEXT,
                
                
                
                
                
                
                
                recall_count    INTEGER NOT NULL DEFAULT 0,
                query_diversity INTEGER NOT NULL DEFAULT 0,
                tier            TEXT NOT NULL DEFAULT 'raw'
            );"###;
const SQL_49: &str = r###"
        CREATE TABLE IF NOT EXISTS recall_impression_groups (
            group_id         TEXT PRIMARY KEY,
            created_at       TEXT NOT NULL,
            legacy_query_bucket TEXT,
            query_fingerprint TEXT,
            fusion_policy_version TEXT,
            pre_boost_adjustment_version TEXT,
            tie_break_policy_version TEXT,
            candidate_policy_version TEXT,
            schema_identity TEXT,
            weights_profile  TEXT NOT NULL,
            semantic_weight  REAL NOT NULL,
            fts_weight       REAL NOT NULL,
            symbolic_weight  REAL NOT NULL,
            decay_weight     REAL NOT NULL,
            use_rrf          INTEGER NOT NULL CHECK (use_rrf IN (0, 1)),
            rrf_k            REAL NOT NULL,
            top_k            INTEGER NOT NULL,
            candidate_count  INTEGER NOT NULL,
            displayed_count  INTEGER NOT NULL,
            scored_returned_count INTEGER NOT NULL,
            replay_count     INTEGER NOT NULL DEFAULT 0,
            CHECK (
                (query_fingerprint IS NULL
                 AND fusion_policy_version IS NULL
                 AND pre_boost_adjustment_version IS NULL
                 AND tie_break_policy_version IS NULL
                 AND candidate_policy_version IS NULL
                 AND schema_identity IS NULL)
                OR
                (query_fingerprint IS NOT NULL
                 AND length(query_fingerprint) = 64
                 AND query_fingerprint NOT GLOB '*[^0-9a-f]*'
                 AND fusion_policy_version IS NOT NULL
                 AND pre_boost_adjustment_version IS NOT NULL
                 AND tie_break_policy_version IS NOT NULL
                 AND candidate_policy_version IS NOT NULL
                 AND schema_identity IS NOT NULL)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_created
            ON recall_impression_groups(created_at DESC, group_id);
        CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_fingerprint
            ON recall_impression_groups(query_fingerprint, created_at DESC)
            WHERE query_fingerprint IS NOT NULL;

        CREATE TABLE IF NOT EXISTS recall_impressions (
            group_id              TEXT NOT NULL,
            memory_id             TEXT NOT NULL,
            vector_score          REAL NOT NULL,
            fts_score             REAL NOT NULL,
            symbolic_score        REAL NOT NULL,
            decay_score           REAL NOT NULL,
            vec_rank              INTEGER,
            fts_rank              INTEGER,
            sym_rank              INTEGER,
            merge_adjustment      TEXT NOT NULL CHECK (merge_adjustment IN ('none', 'exact_id', 'superseded_scale', 'exact_id_superseded_scale')),
            pre_boost_score       REAL NOT NULL,
            pre_boost_rank        INTEGER NOT NULL,
            tie_break_epoch_millis INTEGER NOT NULL,
            final_score           REAL NOT NULL,
            final_rank            INTEGER NOT NULL,
            scored                INTEGER NOT NULL CHECK (scored IN (0, 1)),
            scored_returned       INTEGER NOT NULL CHECK (scored_returned IN (0, 1)),
            access_count_at_recall INTEGER NOT NULL,
            PRIMARY KEY (group_id, memory_id),
            FOREIGN KEY (group_id) REFERENCES recall_impression_groups(group_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_recall_impressions_memory
            ON recall_impressions(memory_id, group_id);
        CREATE INDEX IF NOT EXISTS idx_recall_impressions_group_final_rank
            ON recall_impressions(group_id, final_rank, memory_id);
"###;
const SQL_50: &str = r###"
        -- Shared REM coordination ledger. The Wiki store is the one database
        -- every repo-local evolver can see, so source claims live here rather
        -- than in one caller's global/project source store. Claims are
        -- insert-once and retained after completion as replay evidence.
        CREATE TABLE IF NOT EXISTS rem_source_claims (
            source_key      TEXT PRIMARY KEY NOT NULL,
            source_identity TEXT NOT NULL,
            draft_id        TEXT NOT NULL,
            claimed_at      TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_rem_source_claims_draft
            ON rem_source_claims(draft_id);

        -- Transactional binding between an exact-dedupe apply receipt and
        -- every archived loser. Keeping this outside caller metadata preserves
        -- arbitrary valid JSON while preventing a legacy receipt downgrade
        -- from claiming a v2-applied row.
        CREATE TABLE IF NOT EXISTS exact_dedupe_apply_lineage (
            loser_id          TEXT PRIMARY KEY NOT NULL,
            apply_id          TEXT NOT NULL,
            plan_digest       TEXT NOT NULL,
            winner_id         TEXT NOT NULL,
            before_revision   INTEGER NOT NULL,
            archived_revision INTEGER NOT NULL,
            loser_valid_until_before TEXT,
            applied_at        TEXT NOT NULL
        );
"###;
const SQL_51: &str = r###"
        CREATE TABLE IF NOT EXISTS memory_outbox_events (
            event_id         TEXT PRIMARY KEY NOT NULL,
            object_id        TEXT NOT NULL,
            object_class     TEXT NOT NULL,
            authority_class  TEXT NOT NULL,
            source_store     TEXT NOT NULL,
            source_partition TEXT NOT NULL,
            source_revision  INTEGER NOT NULL,
            payload_digest   TEXT NOT NULL,
            state            TEXT NOT NULL CHECK (state IN ('pending', 'in_flight', 'acknowledged', 'rejected', 'conflicted', 'quarantined')),
            last_error_class TEXT,
            created_at       TEXT NOT NULL,
            state_changed_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memory_outbox_events_state_created
            ON memory_outbox_events(state, created_at);
        CREATE INDEX IF NOT EXISTS idx_memory_outbox_events_state_changed
            ON memory_outbox_events(state, state_changed_at);
        CREATE INDEX IF NOT EXISTS idx_memory_outbox_events_object
            ON memory_outbox_events(object_id, created_at);
"###;
const SQL_52: &str = r###"
        CREATE TABLE IF NOT EXISTS memory_outbox_destination_apply_receipts (
            event_id                         TEXT PRIMARY KEY NOT NULL,
            object_id                        TEXT NOT NULL,
            source_store                     TEXT NOT NULL,
            source_partition                 TEXT NOT NULL,
            source_revision                  INTEGER NOT NULL,
            source_payload_digest            TEXT NOT NULL,
            destination_store                TEXT NOT NULL,
            destination_partition            TEXT NOT NULL,
            destination_object_revision      INTEGER NOT NULL,
            destination_payload_digest       TEXT NOT NULL,
            application                      TEXT NOT NULL CHECK (application = 'applied')
        );
        CREATE INDEX IF NOT EXISTS idx_memory_outbox_destination_apply_object
            ON memory_outbox_destination_apply_receipts(object_id, event_id);
"###;
const SQL_53: &str = r###"CREATE TABLE IF NOT EXISTS harness_session_attachments (
            attachment_id TEXT PRIMARY KEY NOT NULL,
            host_identity TEXT NOT NULL CHECK (length(trim(host_identity)) > 0),
            identity_attribution_basis TEXT NOT NULL CHECK (identity_attribution_basis IN ('trusted_local_host_declared', 'verified')),
            protocol_version TEXT NOT NULL CHECK (length(trim(protocol_version)) > 0),
            adapter_connection_identity TEXT NOT NULL CHECK (length(trim(adapter_connection_identity)) > 0),
            remote_session_id TEXT NOT NULL CHECK (length(trim(remote_session_id)) > 0),
            work_claim_id TEXT NOT NULL CHECK (length(trim(work_claim_id)) > 0),
            expected_transition_version INTEGER NOT NULL CHECK (expected_transition_version >= 0),
            agent_identity_id TEXT NOT NULL CHECK (length(trim(agent_identity_id)) > 0),
            contract_digest TEXT NOT NULL CHECK (length(trim(contract_digest)) > 0),
            capabilities_json TEXT NOT NULL CHECK (json_valid(capabilities_json)),
            tool_profile TEXT NOT NULL CHECK (length(trim(tool_profile)) > 0),
            capability_class TEXT NOT NULL CHECK (length(trim(capability_class)) > 0),
            policy_digest TEXT NOT NULL CHECK (length(trim(policy_digest)) > 0),
            descriptor_digest TEXT NOT NULL CHECK (length(trim(descriptor_digest)) > 0),
            idempotency_key TEXT NOT NULL CHECK (length(trim(idempotency_key)) > 0),
            admission_receipt_ref TEXT NOT NULL CHECK (length(trim(admission_receipt_ref)) > 0),
            state TEXT NOT NULL CHECK (state IN ('attached', 'reconnect_failed', 'unknown')),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE (idempotency_key),
            UNIQUE (host_identity, protocol_version, adapter_connection_identity, remote_session_id)
        );
        CREATE INDEX IF NOT EXISTS idx_harness_session_attachments_claim
            ON harness_session_attachments(work_claim_id, expected_transition_version);
        CREATE INDEX IF NOT EXISTS idx_harness_session_attachments_receipt
            ON harness_session_attachments(admission_receipt_ref, created_at);"###;
const SQL_54: &str = r###"CREATE TABLE IF NOT EXISTS harness_session_events (
            event_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            event_id TEXT NOT NULL CHECK (length(event_id) <= 128 AND length(trim(event_id)) > 0 AND instr(CAST(event_id AS BLOB), CAST(x'00' AS BLOB)) = 0),
            kind TEXT NOT NULL CHECK (kind IN ('accepted', 'started', 'progress', 'input_required', 'terminal', 'cleanup')),
            outcome TEXT CHECK (outcome IS NULL OR outcome IN ('completed', 'failed', 'cancelled')),
            source_revision INTEGER NOT NULL CHECK (source_revision >= 0),
            authority_confirmation_ref TEXT CHECK (authority_confirmation_ref IS NULL OR (length(authority_confirmation_ref) <= 128 AND length(trim(authority_confirmation_ref)) > 0 AND instr(CAST(authority_confirmation_ref AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            summary TEXT CHECK (summary IS NULL OR (length(summary) > 0 AND length(summary) <= 2000 AND instr(CAST(summary AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            payload_digest TEXT CHECK (payload_digest IS NULL OR (length(payload_digest) <= 128 AND length(trim(payload_digest)) > 0 AND payload_digest NOT GLOB '*[^A-Za-z0-9+=/_:-]*' AND length(CAST(payload_digest AS BLOB)) = length(payload_digest))),
            occurred_at TEXT NOT NULL CHECK (length(occurred_at) <= 64 AND instr(CAST(occurred_at AS BLOB), CAST(x'00' AS BLOB)) = 0),
            ingested_at TEXT NOT NULL,
            source_host_identity TEXT NOT NULL CHECK (length(trim(source_host_identity)) > 0),
            UNIQUE (attachment_id, event_id),
            CHECK (kind != 'terminal' OR outcome IS NOT NULL),
            CHECK (outcome IS NULL OR kind = 'terminal'),
            CHECK (kind = 'terminal' OR outcome IS NULL),
            CHECK (outcome != 'cancelled' OR (authority_confirmation_ref IS NOT NULL AND length(trim(authority_confirmation_ref)) > 0))
        );
        CREATE INDEX IF NOT EXISTS idx_harness_session_events_revision
            ON harness_session_events(attachment_id, source_revision);

        CREATE TABLE IF NOT EXISTS harness_session_state (
            attachment_id TEXT PRIMARY KEY NOT NULL REFERENCES harness_session_attachments(attachment_id),
            canonical_state TEXT NOT NULL CHECK (canonical_state IN
                ('accepted', 'started', 'progressing', 'input_required', 'completed', 'failed',
                 'cancelled', 'inconsistent_reconciling', 'unknown_orphaned')),
            canonical_revision INTEGER NOT NULL CHECK (canonical_revision >= 0),
            terminal_digest TEXT CHECK (terminal_digest IS NULL OR length(trim(terminal_digest)) > 0),
            conflicting_terminal_digest TEXT CHECK (conflicting_terminal_digest IS NULL OR length(trim(conflicting_terminal_digest)) > 0),
            cleanup_recorded INTEGER NOT NULL DEFAULT 0 CHECK (cleanup_recorded IN (0, 1)),
            last_event_id TEXT CHECK (last_event_id IS NULL OR length(trim(last_event_id)) > 0),
            pre_disconnect_rank INTEGER NOT NULL DEFAULT -1 CHECK (pre_disconnect_rank >= -1),
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS harness_session_interventions (
            intervention_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            request_id TEXT NOT NULL CHECK (length(request_id) <= 128 AND length(trim(request_id)) > 0 AND instr(CAST(request_id AS BLOB), CAST(x'00' AS BLOB)) = 0),
            kind TEXT NOT NULL CHECK (kind IN ('request_status', 'prompt_or_correct', 'request_pause', 'request_cancel', 'request_resume')),
            reason TEXT NOT NULL CHECK (length(reason) > 0 AND length(reason) <= 1000 AND instr(CAST(reason AS BLOB), CAST(x'00' AS BLOB)) = 0),
            expected_session_revision INTEGER NOT NULL CHECK (expected_session_revision >= 0),
            capability_source TEXT NOT NULL CHECK (capability_source IN ('declared', 'advertised')),
            requested_by TEXT NOT NULL CHECK (length(trim(requested_by)) > 0),
            requested_at TEXT NOT NULL,
            UNIQUE (attachment_id, request_id)
        );
        CREATE INDEX IF NOT EXISTS idx_harness_session_interventions_attachment
            ON harness_session_interventions(attachment_id, requested_at);

        CREATE TABLE IF NOT EXISTS harness_session_intervention_results (
            result_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            request_id TEXT NOT NULL,
            disposition TEXT NOT NULL CHECK (disposition IN ('accepted', 'refused', 'unsupported', 'failed')),
            authority_confirmation_ref TEXT CHECK (authority_confirmation_ref IS NULL OR (length(authority_confirmation_ref) <= 128 AND length(trim(authority_confirmation_ref)) > 0 AND instr(CAST(authority_confirmation_ref AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            detail TEXT CHECK (detail IS NULL OR (length(detail) > 0 AND length(detail) <= 2000 AND instr(CAST(detail AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            recorded_at TEXT NOT NULL,
            source_host_identity TEXT NOT NULL CHECK (length(trim(source_host_identity)) > 0),
            UNIQUE (attachment_id, request_id)
        );

        CREATE TABLE IF NOT EXISTS harness_session_capability_advertisements (
            advertisement_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            advertisement_seq INTEGER NOT NULL CHECK (advertisement_seq >= 1),
            capabilities_json TEXT NOT NULL CHECK (
                json_valid(capabilities_json)
                AND length(CAST(capabilities_json AS BLOB)) <= 256
                AND json_remove(capabilities_json, '$.observe', '$.wait', '$.prompt', '$.cancel', '$.resume', '$.load', '$.events', '$.artifacts') = '{}'
                AND json_type(capabilities_json, '$.observe') IN ('true', 'false')
                AND json_type(capabilities_json, '$.wait') IN ('true', 'false')
                AND json_type(capabilities_json, '$.prompt') IN ('true', 'false')
                AND json_type(capabilities_json, '$.cancel') IN ('true', 'false')
                AND json_type(capabilities_json, '$.resume') IN ('true', 'false')
                AND json_type(capabilities_json, '$.load') IN ('true', 'false')
                AND json_type(capabilities_json, '$.events') IN ('true', 'false')
                AND json_type(capabilities_json, '$.artifacts') IN ('true', 'false')
            ),
            source_host_identity TEXT NOT NULL CHECK (length(trim(source_host_identity)) > 0),
            advertised_at TEXT NOT NULL,
            UNIQUE (attachment_id, advertisement_seq)
        );"###;
const SQL_55: &str = r###"CREATE TABLE IF NOT EXISTS harness_session_events (
            event_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            event_id TEXT NOT NULL CHECK (length(event_id) <= 128 AND length(trim(event_id)) > 0 AND instr(CAST(event_id AS BLOB), CAST(x'00' AS BLOB)) = 0),
            kind TEXT NOT NULL CHECK (kind IN ('accepted', 'started', 'progress', 'input_required', 'terminal', 'cleanup')),
            outcome TEXT CHECK (outcome IS NULL OR outcome IN ('completed', 'failed', 'cancelled')),
            source_revision INTEGER NOT NULL CHECK (source_revision >= 0),
            authority_confirmation_ref TEXT CHECK (authority_confirmation_ref IS NULL OR (length(authority_confirmation_ref) <= 128 AND length(trim(authority_confirmation_ref)) > 0 AND instr(CAST(authority_confirmation_ref AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            summary TEXT CHECK (summary IS NULL OR (length(summary) > 0 AND length(summary) <= 2000 AND instr(CAST(summary AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            payload_digest TEXT CHECK (payload_digest IS NULL OR (length(payload_digest) <= 128 AND length(trim(payload_digest)) > 0 AND payload_digest NOT GLOB '*[^A-Za-z0-9+=/_:-]*' AND length(CAST(payload_digest AS BLOB)) = length(payload_digest))),
            occurred_at TEXT NOT NULL CHECK (length(occurred_at) <= 64 AND instr(CAST(occurred_at AS BLOB), CAST(x'00' AS BLOB)) = 0),
            ingested_at TEXT NOT NULL,
            source_host_identity TEXT NOT NULL CHECK (length(trim(source_host_identity)) > 0),
            UNIQUE (attachment_id, event_id),
            CHECK (kind != 'terminal' OR outcome IS NOT NULL),
            CHECK (outcome IS NULL OR kind = 'terminal'),
            CHECK (kind = 'terminal' OR outcome IS NULL),
            CHECK (outcome != 'cancelled' OR (authority_confirmation_ref IS NOT NULL AND length(trim(authority_confirmation_ref)) > 0))
        );
        CREATE INDEX IF NOT EXISTS idx_harness_session_events_revision
            ON harness_session_events(attachment_id, source_revision);

        CREATE TABLE IF NOT EXISTS harness_session_state (
            attachment_id TEXT PRIMARY KEY NOT NULL REFERENCES harness_session_attachments(attachment_id),
            canonical_state TEXT NOT NULL CHECK (canonical_state IN
                ('accepted', 'started', 'progressing', 'input_required', 'completed', 'failed',
                 'cancelled', 'inconsistent_reconciling', 'unknown_orphaned')),
            canonical_revision INTEGER NOT NULL CHECK (canonical_revision >= 0),
            terminal_digest TEXT CHECK (terminal_digest IS NULL OR length(trim(terminal_digest)) > 0),
            conflicting_terminal_digest TEXT CHECK (conflicting_terminal_digest IS NULL OR length(trim(conflicting_terminal_digest)) > 0),
            cleanup_recorded INTEGER NOT NULL DEFAULT 0 CHECK (cleanup_recorded IN (0, 1)),
            last_event_id TEXT CHECK (last_event_id IS NULL OR length(trim(last_event_id)) > 0),
            pre_disconnect_rank INTEGER NOT NULL DEFAULT -1 CHECK (pre_disconnect_rank >= -1),
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS harness_session_interventions (
            intervention_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            request_id TEXT NOT NULL CHECK (length(request_id) <= 128 AND length(trim(request_id)) > 0 AND instr(CAST(request_id AS BLOB), CAST(x'00' AS BLOB)) = 0),
            kind TEXT NOT NULL CHECK (kind IN ('request_status', 'prompt_or_correct', 'request_pause', 'request_cancel', 'request_resume')),
            reason TEXT NOT NULL CHECK (length(reason) > 0 AND length(reason) <= 1000 AND instr(CAST(reason AS BLOB), CAST(x'00' AS BLOB)) = 0),
            expected_session_revision INTEGER NOT NULL CHECK (expected_session_revision >= 0),
            capability_source TEXT NOT NULL CHECK (capability_source IN ('declared', 'advertised', 'legacy_unknown')),
            requested_by TEXT NOT NULL CHECK (length(trim(requested_by)) > 0),
            requested_at TEXT NOT NULL,
            UNIQUE (attachment_id, request_id)
        );
        CREATE INDEX IF NOT EXISTS idx_harness_session_interventions_attachment
            ON harness_session_interventions(attachment_id, requested_at);

        CREATE TABLE IF NOT EXISTS harness_session_intervention_results (
            result_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            request_id TEXT NOT NULL,
            disposition TEXT NOT NULL CHECK (disposition IN ('accepted', 'refused', 'unsupported', 'failed')),
            authority_confirmation_ref TEXT CHECK (authority_confirmation_ref IS NULL OR (length(authority_confirmation_ref) <= 128 AND length(trim(authority_confirmation_ref)) > 0 AND instr(CAST(authority_confirmation_ref AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            detail TEXT CHECK (detail IS NULL OR (length(detail) > 0 AND length(detail) <= 2000 AND instr(CAST(detail AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            recorded_at TEXT NOT NULL,
            source_host_identity TEXT NOT NULL CHECK (length(trim(source_host_identity)) > 0),
            UNIQUE (attachment_id, request_id)
        );

        CREATE TABLE IF NOT EXISTS harness_session_capability_advertisements (
            advertisement_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            advertisement_seq INTEGER NOT NULL CHECK (advertisement_seq >= 1),
            capabilities_json TEXT NOT NULL CHECK (
                json_valid(capabilities_json)
                AND length(CAST(capabilities_json AS BLOB)) <= 256
                AND json_remove(capabilities_json, '$.observe', '$.wait', '$.prompt', '$.cancel', '$.resume', '$.load', '$.events', '$.artifacts') = '{}'
                AND COALESCE(json_type(capabilities_json, '$.observe'), '') IN ('true', 'false')
                AND COALESCE(json_type(capabilities_json, '$.wait'), '') IN ('true', 'false')
                AND COALESCE(json_type(capabilities_json, '$.prompt'), '') IN ('true', 'false')
                AND COALESCE(json_type(capabilities_json, '$.cancel'), '') IN ('true', 'false')
                AND COALESCE(json_type(capabilities_json, '$.resume'), '') IN ('true', 'false')
                AND COALESCE(json_type(capabilities_json, '$.load'), '') IN ('true', 'false')
                AND COALESCE(json_type(capabilities_json, '$.events'), '') IN ('true', 'false')
                AND COALESCE(json_type(capabilities_json, '$.artifacts'), '') IN ('true', 'false')
            ),
            source_host_identity TEXT NOT NULL CHECK (length(trim(source_host_identity)) > 0),
            advertised_at TEXT NOT NULL,
            UNIQUE (attachment_id, advertisement_seq)
        );"###;

const V25: &str = r###"
        CREATE TABLE IF NOT EXISTS recall_impression_groups (
            group_id         TEXT PRIMARY KEY,
            created_at       TEXT NOT NULL,
            query_hash       TEXT NOT NULL,
            weights_profile  TEXT NOT NULL,
            semantic_weight  REAL NOT NULL,
            fts_weight       REAL NOT NULL,
            symbolic_weight  REAL NOT NULL,
            decay_weight     REAL NOT NULL,
            use_rrf          INTEGER NOT NULL CHECK (use_rrf IN (0, 1)),
            rrf_k            REAL NOT NULL,
            top_k            INTEGER NOT NULL,
            candidate_count  INTEGER NOT NULL,
            displayed_count  INTEGER NOT NULL,
            scored_returned_count INTEGER NOT NULL,
            replay_count     INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_created
            ON recall_impression_groups(created_at DESC, group_id);
        CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_query_hash
            ON recall_impression_groups(query_hash, created_at DESC);

        CREATE TABLE IF NOT EXISTS recall_impressions (
            group_id              TEXT NOT NULL,
            memory_id             TEXT NOT NULL,
            vector_score          REAL NOT NULL,
            fts_score             REAL NOT NULL,
            symbolic_score        REAL NOT NULL,
            decay_score           REAL NOT NULL,
            vec_rank              INTEGER,
            fts_rank              INTEGER,
            sym_rank              INTEGER,
            merge_adjustment      TEXT NOT NULL CHECK (merge_adjustment IN ('none', 'exact_id', 'superseded_scale', 'exact_id_superseded_scale')),
            pre_boost_score       REAL NOT NULL,
            pre_boost_rank        INTEGER NOT NULL,
            tie_break_epoch_millis INTEGER NOT NULL,
            final_score           REAL NOT NULL,
            final_rank            INTEGER NOT NULL,
            scored                INTEGER NOT NULL CHECK (scored IN (0, 1)),
            scored_returned       INTEGER NOT NULL CHECK (scored_returned IN (0, 1)),
            access_count_at_recall INTEGER NOT NULL,
            PRIMARY KEY (group_id, memory_id),
            FOREIGN KEY (group_id) REFERENCES recall_impression_groups(group_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_recall_impressions_memory
            ON recall_impressions(memory_id, group_id);
        CREATE INDEX IF NOT EXISTS idx_recall_impressions_group_final_rank
            ON recall_impressions(group_id, final_rank, memory_id);
"###;

// Frozen unversioned maintenance, 2126bacf db/search_generation.rs.
const FROZEN_SEARCH_MAINTENANCE: &str = r###"
    CREATE TABLE IF NOT EXISTS memory_search_generation (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        generation INTEGER NOT NULL CHECK (generation >= 0)
    );
    INSERT OR IGNORE INTO memory_search_generation (id, generation) VALUES (1, 0);

    CREATE TRIGGER IF NOT EXISTS memory_search_generation_after_insert
    AFTER INSERT ON memories
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_search_generation_after_update
    AFTER UPDATE ON memories
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_search_generation_after_delete
    AFTER DELETE ON memories
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_edge_search_generation_after_insert
    AFTER INSERT ON memory_edges
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_edge_search_generation_after_update
    AFTER UPDATE ON memory_edges
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_edge_search_generation_after_delete
    AFTER DELETE ON memory_edges
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_access_search_generation_after_insert
    AFTER INSERT ON access_history
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_access_search_generation_after_update
    AFTER UPDATE ON access_history
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;

    CREATE TRIGGER IF NOT EXISTS memory_access_search_generation_after_delete
    AFTER DELETE ON access_history
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;
"###;

pub(super) const A2A_V31: &str = r###"
        CREATE TABLE IF NOT EXISTS a2a_envelopes (
            envelope_id                    TEXT PRIMARY KEY NOT NULL,
            kind                           TEXT NOT NULL CHECK (kind = 'turn_response/v1'),
            issuer_agent_identity_id       TEXT NOT NULL,
            issuer_admission_id             TEXT NOT NULL,
            recipient_agent_identity_id    TEXT NOT NULL,
            recipient_admission_id          TEXT NOT NULL,
            subject_ref                    TEXT NOT NULL,
            body                           TEXT NOT NULL,
            body_digest                    TEXT NOT NULL CHECK (length(body_digest) = 64 AND body_digest = lower(body_digest) AND body_digest NOT GLOB '*[^0-9a-f]*'),
            issuer_identity_assurance      TEXT NOT NULL CHECK (issuer_identity_assurance = 'self_asserted'),
            recipient_identity_assurance   TEXT NOT NULL CHECK (recipient_identity_assurance = 'self_asserted'),
            issuer_trust_domain            TEXT NOT NULL CHECK (issuer_trust_domain = 'same_host'),
            recipient_trust_domain         TEXT NOT NULL CHECK (recipient_trust_domain = 'same_host'),
            issuer_trust_basis             TEXT NOT NULL CHECK (issuer_trust_basis = 'current_local_connection'),
            recipient_trust_basis          TEXT NOT NULL CHECK (recipient_trust_basis = 'historical_local_admission'),
            idempotency_key                TEXT NOT NULL,
            created_at                     TEXT NOT NULL,
            expires_at                     TEXT NOT NULL,
            current_state                  TEXT NOT NULL CHECK (current_state IN ('received','accepted','consumed','expired')),
            state_version                  INTEGER NOT NULL CHECK (state_version > 0),
            UNIQUE (issuer_agent_identity_id, idempotency_key),
            CHECK (length(trim(subject_ref)) > 0),
            CHECK (length(body) > 0),
            CHECK (length(CAST(body AS BLOB)) <= 4096),
            CHECK (expires_at > created_at),
            FOREIGN KEY (issuer_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
            FOREIGN KEY (issuer_admission_id) REFERENCES identity_admissions(admission_id),
            FOREIGN KEY (recipient_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
            FOREIGN KEY (recipient_admission_id) REFERENCES identity_admissions(admission_id)
        );
        CREATE INDEX IF NOT EXISTS idx_a2a_envelopes_recipient_state
            ON a2a_envelopes(recipient_agent_identity_id, current_state, expires_at, created_at, envelope_id);
        CREATE INDEX IF NOT EXISTS idx_a2a_envelopes_issuer_created
            ON a2a_envelopes(issuer_agent_identity_id, created_at DESC, envelope_id DESC);

        CREATE TABLE IF NOT EXISTS a2a_delivery_receipts (
            receipt_id                     TEXT PRIMARY KEY NOT NULL,
            envelope_id                    TEXT NOT NULL,
            envelope_version               INTEGER NOT NULL CHECK (envelope_version > 0),
            state                          TEXT NOT NULL CHECK (state IN ('received','accepted','consumed','expired')),
            actor_agent_identity_id        TEXT NOT NULL,
            actor_admission_id              TEXT NOT NULL,
            identity_assurance             TEXT NOT NULL CHECK (identity_assurance = 'self_asserted'),
            trust_domain                   TEXT NOT NULL CHECK (trust_domain = 'same_host'),
            trust_basis                    TEXT NOT NULL CHECK (trust_basis IN ('current_local_connection','historical_local_admission')),
            occurred_at                    TEXT NOT NULL,
            UNIQUE (envelope_id, envelope_version),
            UNIQUE (envelope_id, state),
            FOREIGN KEY (envelope_id) REFERENCES a2a_envelopes(envelope_id),
            FOREIGN KEY (actor_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
            FOREIGN KEY (actor_admission_id) REFERENCES identity_admissions(admission_id)
        );
        CREATE INDEX IF NOT EXISTS idx_a2a_receipts_envelope_version
            ON a2a_delivery_receipts(envelope_id, envelope_version);
"###;

// Original v20 source 71a4f9f3 migrations.rs.
pub(super) const MIRROR_V20: &str = r###"CREATE TABLE IF NOT EXISTS mirror_eval_runs (
            eval_run_id         TEXT PRIMARY KEY,
            register_key        TEXT NOT NULL UNIQUE,
            frozen_contract_ref TEXT NOT NULL CHECK (length(trim(frozen_contract_ref)) > 0),
            execution_origin    TEXT NOT NULL CHECK (length(trim(execution_origin)) > 0),
            lifecycle_owner     TEXT NOT NULL CHECK (length(trim(lifecycle_owner)) > 0),
            harness             TEXT,
            native_child_id     TEXT,
            requested_profile   TEXT,
            requested_model     TEXT,
            requested_agent     TEXT,
            created_at          TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_mirror_eval_runs_native_child
            ON mirror_eval_runs(native_child_id, created_at);
        CREATE TABLE IF NOT EXISTS mirror_eval_observations (
            observation_id   TEXT PRIMARY KEY,
            eval_run_id      TEXT NOT NULL UNIQUE,
            terminal_outcome TEXT NOT NULL CHECK (length(trim(terminal_outcome)) > 0),
            duration_ms      INTEGER,
            cost_tokens      INTEGER,
            cost_usd         REAL,
            result_ref       TEXT,
            artifacts        TEXT NOT NULL DEFAULT '[]',
            effective_model    TEXT,
            effective_backend  TEXT,
            effective_harness  TEXT,
            created_at       TEXT NOT NULL DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS mirror_eval_adjudications (
            adjudication_id       TEXT PRIMARY KEY,
            eval_run_id           TEXT NOT NULL,
            event_key             TEXT NOT NULL UNIQUE,
            actor                 TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            verifier_model        TEXT,
            usefulness            TEXT NOT NULL CHECK (length(trim(usefulness)) > 0),
            failure_mode          TEXT,
            first_review_findings TEXT NOT NULL DEFAULT '[]',
            plan_delta            TEXT,
            next_prompt_delta     TEXT,
            evidence_usable       INTEGER NOT NULL CHECK (evidence_usable IN (0, 1)),
            used_in_final_claim   INTEGER NOT NULL DEFAULT 0 CHECK (used_in_final_claim IN (0, 1)),
            human_override        INTEGER NOT NULL DEFAULT 0 CHECK (human_override IN (0, 1)),
            evidence_ref          TEXT NOT NULL CHECK (length(trim(evidence_ref)) > 0),
            created_at            TEXT NOT NULL DEFAULT '',
            insertion_seq         INTEGER NOT NULL,
            UNIQUE (eval_run_id, insertion_seq)
        );
        CREATE INDEX IF NOT EXISTS idx_mirror_eval_adjudications_run
            ON mirror_eval_adjudications(eval_run_id, created_at);"###;

// 2b3951a4 historical initializer enum rebuild, literal projection expressions.
const P36_ENUM_REBUILD: &str = r###"
        CREATE TABLE memories_new (
            id           TEXT PRIMARY KEY,
            path         TEXT NOT NULL DEFAULT '/',
            summary      TEXT NOT NULL DEFAULT '',
            text         TEXT NOT NULL DEFAULT '',
            importance   REAL NOT NULL DEFAULT 0.7,
            timestamp    TEXT NOT NULL,
            valid_from   TEXT NOT NULL DEFAULT '',
            valid_until  TEXT,
            category     TEXT NOT NULL DEFAULT 'fact',
            topic        TEXT NOT NULL DEFAULT '',
            keywords     TEXT NOT NULL DEFAULT '[]',
            entities     TEXT NOT NULL DEFAULT '[]',
            source       TEXT NOT NULL DEFAULT 'manual',
            scope        TEXT NOT NULL DEFAULT 'general',
            archived     INTEGER NOT NULL DEFAULT 0,
            created_at   TEXT NOT NULL DEFAULT '',
            updated_at   TEXT NOT NULL DEFAULT '',
            access_count INTEGER NOT NULL DEFAULT 0,
            scored_count INTEGER NOT NULL DEFAULT 0,
            last_access  TEXT,
            last_use_at  TEXT,
            revision     INTEGER NOT NULL DEFAULT 1,
             metadata     TEXT NOT NULL DEFAULT '{}',
             retention_policy TEXT,
             domain       TEXT,
             superseded_by TEXT,
             idless_identity TEXT,
             recall_count    INTEGER NOT NULL DEFAULT 0,
             query_diversity INTEGER NOT NULL DEFAULT 0,
             tier            TEXT NOT NULL DEFAULT 'raw',
             CHECK (category IN ('fact','decision','experience','preference','entity','other','kanban','handoff','ghost','wiki','guide','eval','sticky')),
            CHECK (scope IN ('user','project','general')),
            CHECK (retention_policy IS NULL OR retention_policy IN ('ephemeral','durable','permanent','pinned')),
            CHECK (
                source IN ('manual','extraction','migration','auto','foundry_distill','foundry_recall_rerank_cache','handoff','kanban','wiki','ghost','ingest_event')
                OR source LIKE 'external:%'
            )
        );

        INSERT INTO memories_new
            (id, path, summary, text, importance, timestamp, valid_from, valid_until,
             category, topic, keywords, entities, source, scope, archived,
              created_at, updated_at, access_count, scored_count, last_access, last_use_at, revision,
             metadata, retention_policy, domain, superseded_by, idless_identity,
             recall_count, query_diversity, tier)
        SELECT
             id, path, summary, text, importance, timestamp,
             COALESCE(NULLIF(valid_from, ''), timestamp), NULLIF(valid_until, ''),
             category, topic, keywords, entities, source, scope, archived,
              created_at, updated_at, access_count, COALESCE(scored_count, 0), last_access, last_use_at, revision,
             metadata, retention_policy, domain, superseded_by, idless_identity,
             COALESCE(recall_count, 0), COALESCE(query_diversity, 0), COALESCE(tier, 'raw')
        FROM memories;

        DROP TABLE memories;
        ALTER TABLE memories_new RENAME TO memories;

        CREATE INDEX IF NOT EXISTS idx_memories_path        ON memories(path);
        CREATE INDEX IF NOT EXISTS idx_memories_importance  ON memories(importance DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_timestamp   ON memories(timestamp DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_archived    ON memories(archived);
        CREATE INDEX IF NOT EXISTS idx_memories_last_access ON memories(last_access DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_valid_time  ON memories(valid_from, valid_until);
        CREATE INDEX IF NOT EXISTS idx_memories_retention_policy ON memories(retention_policy);
        CREATE INDEX IF NOT EXISTS idx_memories_domain ON memories(domain);
        CREATE INDEX IF NOT EXISTS idx_memories_superseded ON memories(superseded_by);
        CREATE INDEX IF NOT EXISTS idx_memories_tier ON memories(tier);
        CREATE INDEX IF NOT EXISTS idx_memories_recall ON memories(recall_count DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_path_active_ts ON memories(path, timestamp DESC) WHERE archived = 0 AND superseded_by IS NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_idless_identity_active
            ON memories(idless_identity)
            WHERE idless_identity IS NOT NULL AND archived = 0 AND superseded_by IS NULL;

        "###;

const P36_GUARDS: &str = r###"
        DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;
        DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;

        CREATE TRIGGER memories_reserved_refs_insert_guard
        BEFORE INSERT ON memories
        WHEN (
             json_type(NEW.metadata, '$.evidence_refs_v1') IS NOT NULL
             OR json_type(NEW.metadata, '$.source_refs') IS NOT NULL
         )
         AND NOT EXISTS (
             SELECT 1
             FROM memories AS current
             WHERE current.id = NEW.id
               AND json_type(current.metadata, '$.evidence_refs_v1')
                   IS json_type(NEW.metadata, '$.evidence_refs_v1')
               AND json_quote(json_extract(current.metadata, '$.evidence_refs_v1'))
                   IS json_quote(json_extract(NEW.metadata, '$.evidence_refs_v1'))
               AND json_type(current.metadata, '$.source_refs')
                   IS json_type(NEW.metadata, '$.source_refs')
               AND json_quote(json_extract(current.metadata, '$.source_refs'))
                   IS json_quote(json_extract(NEW.metadata, '$.source_refs'))
         )
         AND tachi_reserved_reference_write_enabled() = 0
        BEGIN
            SELECT RAISE(ABORT, 'reserved memory reference metadata requires typed mutation');
        END;

        CREATE TRIGGER memories_reserved_refs_update_guard
        BEFORE UPDATE OF metadata ON memories
        WHEN (
             json_type(NEW.metadata, '$.evidence_refs_v1')
                 IS NOT json_type(OLD.metadata, '$.evidence_refs_v1')
             OR json_quote(json_extract(NEW.metadata, '$.evidence_refs_v1'))
                 IS NOT json_quote(json_extract(OLD.metadata, '$.evidence_refs_v1'))
             OR json_type(NEW.metadata, '$.source_refs')
                 IS NOT json_type(OLD.metadata, '$.source_refs')
             OR json_quote(json_extract(NEW.metadata, '$.source_refs'))
                 IS NOT json_quote(json_extract(OLD.metadata, '$.source_refs'))
         )
         AND tachi_reserved_reference_write_enabled() = 0
        BEGIN
            SELECT RAISE(ABORT, 'reserved memory reference metadata requires typed mutation');
        END;
"###;

// v36 repaired historical source 37a7981d^, before v37 introduction.
const P36_DELIVERY_REPAIRED: &str = r###"CREATE TABLE IF NOT EXISTS delivery_intents (
            delivery_id TEXT PRIMARY KEY,
            idempotency_key TEXT NOT NULL UNIQUE CHECK (length(idempotency_key) <= 128 AND length(trim(idempotency_key)) > 0 AND instr(CAST(idempotency_key AS BLOB), CAST(x'00' AS BLOB)) = 0),
            execution_source TEXT NOT NULL CHECK (execution_source IN ('managed_dispatch', 'attached_session')),
            execution_ref TEXT NOT NULL CHECK (length(execution_ref) <= 128 AND length(trim(execution_ref)) > 0 AND instr(CAST(execution_ref AS BLOB), CAST(x'00' AS BLOB)) = 0),
            terminal_receipt_revision INTEGER NOT NULL CHECK (terminal_receipt_revision >= 0),
            work_claim_id TEXT CHECK (work_claim_id IS NULL OR (length(work_claim_id) <= 128 AND length(trim(work_claim_id)) > 0 AND instr(CAST(work_claim_id AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            result_ref TEXT NOT NULL CHECK (length(result_ref) <= 512 AND length(trim(result_ref)) > 0 AND instr(CAST(result_ref AS BLOB), CAST(x'00' AS BLOB)) = 0),
            result_revision INTEGER NOT NULL CHECK (result_revision >= 1),
            payload_digest TEXT NOT NULL CHECK (length(payload_digest) <= 128 AND length(trim(payload_digest)) > 0 AND payload_digest NOT GLOB '*[^A-Za-z0-9+=/_:-]*' AND length(CAST(payload_digest AS BLOB)) = length(payload_digest)),
            visibility_class TEXT NOT NULL CHECK (visibility_class IN ('public', 'private')),
            delivery_policy TEXT NOT NULL CHECK (delivery_policy IN ('return_to_current_call', 'resume_requester_operation', 'announce_requester_session', 'silent_artifact_only')),
            protocol_capability TEXT NOT NULL CHECK (length(protocol_capability) <= 128 AND length(trim(protocol_capability)) > 0 AND instr(CAST(protocol_capability AS BLOB), CAST(x'00' AS BLOB)) = 0),
            requester_agent_identity_id TEXT CHECK (requester_agent_identity_id IS NULL OR (length(requester_agent_identity_id) <= 128 AND length(trim(requester_agent_identity_id)) > 0 AND instr(CAST(requester_agent_identity_id AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            requester_host_identity TEXT CHECK (requester_host_identity IS NULL OR (length(requester_host_identity) <= 128 AND length(trim(requester_host_identity)) > 0 AND instr(CAST(requester_host_identity AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            requester_session_ref TEXT CHECK (requester_session_ref IS NULL OR (length(requester_session_ref) <= 256 AND length(trim(requester_session_ref)) > 0 AND instr(CAST(requester_session_ref AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            delivery_state TEXT NOT NULL CHECK (delivery_state IN ('not_ready', 'ready', 'requester_queued', 'delivered', 'blocked', 'retrying', 'dismissed')),
            blocker_class TEXT CHECK (blocker_class IS NULL OR (length(blocker_class) <= 128 AND length(trim(blocker_class)) > 0 AND instr(CAST(blocker_class AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            active_claim_key TEXT CHECK (active_claim_key IS NULL OR (length(active_claim_key) <= 128 AND length(trim(active_claim_key)) > 0 AND instr(CAST(active_claim_key AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            claimed_by TEXT CHECK (claimed_by IS NULL OR (length(claimed_by) <= 128 AND length(trim(claimed_by)) > 0 AND instr(CAST(claimed_by AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            claim_expires_at TEXT CHECK (claim_expires_at IS NULL OR (length(claim_expires_at) <= 64 AND instr(CAST(claim_expires_at AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
            next_retry_at TEXT CHECK (next_retry_at IS NULL OR (length(next_retry_at) <= 64 AND instr(CAST(next_retry_at AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            expires_at TEXT CHECK (expires_at IS NULL OR (length(expires_at) <= 64 AND instr(CAST(expires_at AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            created_at TEXT NOT NULL,
            ready_at TEXT,
            delivered_at TEXT,
            dismissed_at TEXT,
            revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
            updated_at TEXT NOT NULL,
            CHECK (delivery_state != 'delivered' OR delivered_at IS NOT NULL),
            CHECK (delivery_state != 'dismissed' OR dismissed_at IS NOT NULL),
            CHECK (delivery_state = 'requester_queued' OR (active_claim_key IS NULL AND claimed_by IS NULL AND claim_expires_at IS NULL)),
            CHECK (delivery_state != 'requester_queued' OR (active_claim_key IS NOT NULL AND claimed_by IS NOT NULL AND claim_expires_at IS NOT NULL)),
            CHECK (delivery_state IN ('blocked', 'retrying') OR blocker_class IS NULL)
        );
        CREATE INDEX IF NOT EXISTS idx_delivery_intents_state_requester
            ON delivery_intents(delivery_state, requester_agent_identity_id);
        CREATE INDEX IF NOT EXISTS idx_delivery_intents_execution
            ON delivery_intents(execution_source, execution_ref);

        CREATE TABLE IF NOT EXISTS delivery_events (
            event_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            delivery_id TEXT NOT NULL REFERENCES delivery_intents(delivery_id),
            event_id TEXT NOT NULL CHECK (length(event_id) <= 256 AND length(trim(event_id)) > 0 AND instr(CAST(event_id AS BLOB), CAST(x'00' AS BLOB)) = 0),
            kind TEXT NOT NULL CHECK (kind IN ('intent_created', 'result_ready', 'result_superseded', 'claimed', 'delivered', 'blocked', 'retry_scheduled', 'dismissed', 'claim_expired', 'transition_debt')),
            expected_revision INTEGER CHECK (expected_revision IS NULL OR expected_revision >= 1),
            detail TEXT CHECK (detail IS NULL OR (length(detail) > 0 AND length(detail) <= 1000 AND instr(CAST(detail AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            payload_digest TEXT CHECK (payload_digest IS NULL OR (length(payload_digest) <= 128 AND length(trim(payload_digest)) > 0 AND payload_digest NOT GLOB '*[^A-Za-z0-9+=/_:-]*' AND length(CAST(payload_digest AS BLOB)) = length(payload_digest))),
            actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            occurred_at TEXT NOT NULL CHECK (length(occurred_at) <= 64 AND instr(CAST(occurred_at AS BLOB), CAST(x'00' AS BLOB)) = 0),
            recorded_at TEXT NOT NULL,
            UNIQUE (delivery_id, event_id)
        );
        CREATE INDEX IF NOT EXISTS idx_delivery_events_delivery
            ON delivery_events(delivery_id, event_row_id);

        CREATE UNIQUE INDEX IF NOT EXISTS idx_delivery_events_claim_key_global
            ON delivery_events(event_id) WHERE kind = 'claimed';
        CREATE UNIQUE INDEX IF NOT EXISTS idx_delivery_events_ack_key_global
            ON delivery_events(event_id) WHERE kind = 'delivered';"###;
