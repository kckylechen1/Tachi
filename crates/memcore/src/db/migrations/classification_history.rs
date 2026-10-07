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

// T1a Full predecessors: all historical Product and Portable objects.
// Before D3/v28 these reconstruct the unprofiled Full-shaped schema.
// v12: E11 base + complete original 1e996d70 co-release claims DDL before
// its unique-index BODY. v16/v17 co-released: v17's predecessor includes
// the original e26a315f v16 receipt ALTER as an intermediate prefix.
// The enum/generation/inline maintenance below is pinned at each source
// object, never today's initializer or today's migration installers.
pub(super) struct FullPrefix {
    pub(super) source: &'static str,
    pub(super) tables: &'static [&'static str],
    pub(super) portable: &'static [&'static str],
    early: &'static [&'static [&'static str]],
    late: &'static [&'static [&'static str]],
    rebuild: &'static str,
    columns: &'static [(&'static str, &'static str, &'static str)],
}
pub(super) fn full_prefix(index: u32) -> FullPrefix {
    match index {
        12 => FullPrefix {
            source: "56a702efd1e128d7bd62ebfaac8e20c961baf98a + 1e996d70 pre-index claims",
            tables: FULL_INVENTORY_0,
            portable: FULL_INVENTORY_1,
            early: &[FULL_EARLY_0, FULL_EARLY_1, FULL_EARLY_2, FULL_EARLY_3],
            late: &[FULL_LATE_0, FULL_LATE_1],
            columns: FULL_COLUMNS_0,
            rebuild: FULL_SQL_197,
        },
        14 => FullPrefix {
            source: "0f644bd3638acbb80b697ed4efdde5809a447ab2",
            tables: FULL_INVENTORY_2,
            portable: FULL_INVENTORY_3,
            early: &[
                FULL_EARLY_0,
                FULL_EARLY_1,
                FULL_EARLY_2,
                FULL_EARLY_3,
                FULL_EARLY_4,
                FULL_EARLY_5,
            ],
            late: &[FULL_LATE_0, FULL_LATE_1, FULL_LATE_2, FULL_LATE_3],
            columns: FULL_COLUMNS_0,
            rebuild: FULL_SQL_197,
        },
        15 => FullPrefix {
            source: "5d20e92d8ba047392b88f907d6b1419ac3de3103",
            tables: FULL_INVENTORY_4,
            portable: FULL_INVENTORY_3,
            early: &[
                FULL_EARLY_0,
                FULL_EARLY_1,
                FULL_EARLY_2,
                FULL_EARLY_3,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_7,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_1,
                FULL_LATE_2,
                FULL_LATE_3,
                FULL_LATE_4,
            ],
            columns: FULL_COLUMNS_0,
            rebuild: FULL_SQL_197,
        },
        16 => FullPrefix {
            source: "9ade01bb47144e71fbf4e16c9bff00209e1a7f6f",
            tables: FULL_INVENTORY_4,
            portable: FULL_INVENTORY_3,
            early: &[
                FULL_EARLY_0,
                FULL_EARLY_1,
                FULL_EARLY_2,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_7,
                FULL_EARLY_8,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_1,
                FULL_LATE_2,
                FULL_LATE_3,
                FULL_LATE_4,
            ],
            columns: FULL_COLUMNS_0,
            rebuild: FULL_SQL_197,
        },
        17 => FullPrefix {
            source: "9ade01bb47144e71fbf4e16c9bff00209e1a7f6f + e26a315f v16 intermediate",
            tables: FULL_INVENTORY_4,
            portable: FULL_INVENTORY_3,
            early: &[
                FULL_EARLY_0,
                FULL_EARLY_1,
                FULL_EARLY_2,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_7,
                FULL_EARLY_8,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_1,
                FULL_LATE_2,
                FULL_LATE_3,
                FULL_LATE_4,
            ],
            columns: FULL_COLUMNS_1,
            rebuild: FULL_SQL_197,
        },
        18 => FullPrefix {
            source: "d2ae1204cd1c1e714d1e74e00427e9e08893196b",
            tables: FULL_INVENTORY_4,
            portable: FULL_INVENTORY_3,
            early: &[
                FULL_EARLY_0,
                FULL_EARLY_1,
                FULL_EARLY_2,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_8,
                FULL_EARLY_9,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_1,
                FULL_LATE_2,
                FULL_LATE_3,
                FULL_LATE_4,
            ],
            columns: FULL_COLUMNS_0,
            rebuild: FULL_SQL_197,
        },
        20 => FullPrefix {
            source: "abc5771f23baae2db1818e13df3de1c4df504c03",
            tables: FULL_INVENTORY_5,
            portable: FULL_INVENTORY_3,
            early: &[
                FULL_EARLY_1,
                FULL_EARLY_2,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_8,
                FULL_EARLY_9,
                FULL_EARLY_10,
                FULL_EARLY_11,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_1,
                FULL_LATE_2,
                FULL_LATE_3,
                FULL_LATE_4,
                FULL_LATE_5,
            ],
            columns: FULL_COLUMNS_2,
            rebuild: FULL_SQL_198,
        },
        21 => FullPrefix {
            source: "dd1dc14a945866d09934e784d94a76a9eea3b9e2",
            tables: FULL_INVENTORY_6,
            portable: FULL_INVENTORY_3,
            early: &[
                FULL_EARLY_1,
                FULL_EARLY_2,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_8,
                FULL_EARLY_9,
                FULL_EARLY_10,
                FULL_EARLY_11,
                FULL_EARLY_12,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_1,
                FULL_LATE_2,
                FULL_LATE_3,
                FULL_LATE_4,
                FULL_LATE_5,
                FULL_LATE_6,
            ],
            columns: FULL_COLUMNS_2,
            rebuild: FULL_SQL_198,
        },
        31 => FullPrefix {
            source: "b80360d7cb9ae7b5f3f800a92c7895233358bd63",
            tables: FULL_INVENTORY_7,
            portable: FULL_INVENTORY_8,
            early: &[
                FULL_EARLY_1,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_9,
                FULL_EARLY_11,
                FULL_EARLY_12,
                FULL_EARLY_13,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_2,
                FULL_LATE_4,
                FULL_LATE_5,
                FULL_LATE_6,
                FULL_LATE_7,
            ],
            columns: FULL_COLUMNS_3,
            rebuild: P36_ENUM_REBUILD,
        },
        32 => FullPrefix {
            source: "46b5631ff80e188dc48718cde5788117a0a7db92",
            tables: FULL_INVENTORY_9,
            portable: FULL_INVENTORY_8,
            early: &[
                FULL_EARLY_1,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_9,
                FULL_EARLY_11,
                FULL_EARLY_12,
                FULL_EARLY_13,
                FULL_EARLY_14,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_2,
                FULL_LATE_4,
                FULL_LATE_5,
                FULL_LATE_6,
                FULL_LATE_7,
                FULL_LATE_8,
            ],
            columns: FULL_COLUMNS_3,
            rebuild: P36_ENUM_REBUILD,
        },
        37 => FullPrefix {
            source: "817a673f45c8bcdef14c5ff8bf89d84ffc05aeba",
            tables: FULL_INVENTORY_10,
            portable: FULL_INVENTORY_11,
            early: &[
                FULL_EARLY_1,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_9,
                FULL_EARLY_11,
                FULL_EARLY_12,
                FULL_EARLY_13,
                FULL_EARLY_15,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_2,
                FULL_LATE_4,
                FULL_LATE_5,
                FULL_LATE_6,
                FULL_LATE_7,
                FULL_LATE_9,
            ],
            columns: FULL_COLUMNS_3,
            rebuild: P36_ENUM_REBUILD,
        },
        38 => FullPrefix {
            source: "ee20bad9d01e5c8074b97c6fb2039f590d32c954",
            tables: FULL_INVENTORY_12,
            portable: FULL_INVENTORY_11,
            early: &[
                FULL_EARLY_1,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_9,
                FULL_EARLY_11,
                FULL_EARLY_12,
                FULL_EARLY_13,
                FULL_EARLY_15,
                FULL_EARLY_16,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_2,
                FULL_LATE_4,
                FULL_LATE_5,
                FULL_LATE_6,
                FULL_LATE_7,
                FULL_LATE_9,
                FULL_LATE_10,
                FULL_LATE_11,
            ],
            columns: FULL_COLUMNS_3,
            rebuild: P36_ENUM_REBUILD,
        },
        39 => FullPrefix {
            source: "ed992a36b97859fa16806706ac0e5c739de03e17",
            tables: FULL_INVENTORY_13,
            portable: FULL_INVENTORY_11,
            early: &[
                FULL_EARLY_1,
                FULL_EARLY_4,
                FULL_EARLY_6,
                FULL_EARLY_9,
                FULL_EARLY_11,
                FULL_EARLY_12,
                FULL_EARLY_13,
                FULL_EARLY_15,
                FULL_EARLY_17,
            ],
            late: &[
                FULL_LATE_0,
                FULL_LATE_2,
                FULL_LATE_4,
                FULL_LATE_5,
                FULL_LATE_6,
                FULL_LATE_7,
                FULL_LATE_9,
                FULL_LATE_10,
                FULL_LATE_12,
            ],
            columns: FULL_COLUMNS_3,
            rebuild: P36_ENUM_REBUILD,
        },
        _ => panic!("not a Product migration"),
    }
}
pub(super) fn install_full(conn: &Connection, prefix: &FullPrefix) {
    for sql in prefix.early.iter().flat_map(|group| group.iter()) {
        conn.execute_batch(sql).unwrap();
    }
    for (table, column, ddl) in prefix.columns {
        if !prefix.tables.contains(table) {
            continue;
        }
        if !table_has_column(conn, table, column).unwrap() {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}"))
                .unwrap();
        }
    }
    conn.execute_batch(prefix.rebuild).unwrap();
    for sql in prefix.late.iter().flat_map(|group| group.iter()) {
        conn.execute_batch(sql).unwrap();
    }
}
const FULL_INVENTORY_0: &[&str] = &[
    "access_history",
    "agent_known_state",
    "audit_log",
    "derived_items",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "hub_capabilities",
    "hub_version_routes",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memory_edges",
    "processed_events",
    "recall_cache",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_INVENTORY_1: &[&str] = &[
    "access_history",
    "derived_items",
    "hard_state",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memory_edges",
    "processed_events",
    "recall_cache",
    "tachi_events",
];
const FULL_INVENTORY_2: &[&str] = &[
    "access_history",
    "agent_known_state",
    "audit_log",
    "derived_items",
    "dispatch_outcomes",
    "edge_observations",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "hub_capabilities",
    "hub_version_routes",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memory_edges",
    "processed_events",
    "recall_cache",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_INVENTORY_3: &[&str] = &[
    "access_history",
    "derived_items",
    "edge_observations",
    "hard_state",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memory_edges",
    "processed_events",
    "recall_cache",
    "tachi_events",
];
const FULL_INVENTORY_4: &[&str] = &[
    "access_history",
    "agent_known_state",
    "audit_log",
    "derived_items",
    "dispatch_outcomes",
    "edge_observations",
    "exec_env_resource_bindings",
    "exec_env_resources",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "hub_capabilities",
    "hub_version_routes",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memory_edges",
    "processed_events",
    "recall_cache",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_INVENTORY_5: &[&str] = &[
    "access_history",
    "agent_known_state",
    "audit_log",
    "derived_items",
    "dispatch_adjudication_signatures",
    "dispatch_adjudications",
    "dispatch_outcomes",
    "edge_observations",
    "exec_env_resource_bindings",
    "exec_env_resources",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "hub_capabilities",
    "hub_version_routes",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memory_edges",
    "processed_events",
    "recall_cache",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_INVENTORY_6: &[&str] = &[
    "access_history",
    "agent_known_state",
    "audit_log",
    "derived_items",
    "dispatch_adjudication_signatures",
    "dispatch_adjudications",
    "dispatch_outcomes",
    "edge_observations",
    "exec_env_resource_bindings",
    "exec_env_resources",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "hub_capabilities",
    "hub_version_routes",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memory_edges",
    "mirror_eval_adjudications",
    "mirror_eval_observations",
    "mirror_eval_runs",
    "processed_events",
    "recall_cache",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_INVENTORY_7: &[&str] = &[
    "access_history",
    "account_custody",
    "agent_identities",
    "agent_known_state",
    "audit_log",
    "derived_items",
    "dispatch_adjudication_signatures",
    "dispatch_adjudications",
    "dispatch_outcomes",
    "edge_observations",
    "eval_rubric_scores",
    "exact_dedupe_apply_lineage",
    "exec_env_resource_bindings",
    "exec_env_resources",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "hub_capabilities",
    "hub_version_routes",
    "identity_admissions",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memories_symbolic_fts",
    "memories_symbolic_fts_config",
    "memories_symbolic_fts_content",
    "memories_symbolic_fts_data",
    "memories_symbolic_fts_docsize",
    "memories_symbolic_fts_idx",
    "memory_edges",
    "memory_outbox_destination_apply_receipts",
    "memory_outbox_events",
    "memory_search_generation",
    "mirror_eval_adjudications",
    "mirror_eval_observations",
    "mirror_eval_runs",
    "processed_events",
    "provider_account_aliases",
    "provider_account_events",
    "provider_accounts",
    "recall_cache",
    "recall_impression_groups",
    "recall_impressions",
    "rem_source_claims",
    "route_decisions",
    "route_recommendations",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_INVENTORY_8: &[&str] = &[
    "access_history",
    "derived_items",
    "edge_observations",
    "exact_dedupe_apply_lineage",
    "hard_state",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memories_symbolic_fts",
    "memories_symbolic_fts_config",
    "memories_symbolic_fts_content",
    "memories_symbolic_fts_data",
    "memories_symbolic_fts_docsize",
    "memories_symbolic_fts_idx",
    "memory_edges",
    "memory_outbox_destination_apply_receipts",
    "memory_outbox_events",
    "memory_search_generation",
    "processed_events",
    "recall_cache",
    "recall_impression_groups",
    "recall_impressions",
    "rem_source_claims",
    "tachi_events",
];
const FULL_INVENTORY_9: &[&str] = &[
    "a2a_delivery_receipts",
    "a2a_envelopes",
    "access_history",
    "account_custody",
    "agent_identities",
    "agent_known_state",
    "audit_log",
    "derived_items",
    "dispatch_adjudication_signatures",
    "dispatch_adjudications",
    "dispatch_outcomes",
    "edge_observations",
    "eval_rubric_scores",
    "exact_dedupe_apply_lineage",
    "exec_env_resource_bindings",
    "exec_env_resources",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "hub_capabilities",
    "hub_version_routes",
    "identity_admissions",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memories_symbolic_fts",
    "memories_symbolic_fts_config",
    "memories_symbolic_fts_content",
    "memories_symbolic_fts_data",
    "memories_symbolic_fts_docsize",
    "memories_symbolic_fts_idx",
    "memory_edges",
    "memory_outbox_destination_apply_receipts",
    "memory_outbox_events",
    "memory_search_generation",
    "mirror_eval_adjudications",
    "mirror_eval_observations",
    "mirror_eval_runs",
    "processed_events",
    "provider_account_aliases",
    "provider_account_events",
    "provider_accounts",
    "recall_cache",
    "recall_impression_groups",
    "recall_impressions",
    "rem_source_claims",
    "route_decisions",
    "route_recommendations",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_INVENTORY_10: &[&str] = &[
    "a2a_delivery_receipts",
    "a2a_envelopes",
    "access_history",
    "account_custody",
    "agent_identities",
    "agent_known_state",
    "audit_log",
    "delivery_events",
    "delivery_intents",
    "derived_items",
    "dispatch_adjudication_signatures",
    "dispatch_adjudications",
    "dispatch_outcomes",
    "edge_observations",
    "eval_rubric_scores",
    "exact_dedupe_apply_lineage",
    "exec_env_resource_bindings",
    "exec_env_resources",
    "exec_env_worktree_identities",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "harness_session_attachments",
    "harness_session_capability_advertisements",
    "harness_session_events",
    "harness_session_intervention_results",
    "harness_session_interventions",
    "harness_session_state",
    "hub_capabilities",
    "hub_version_routes",
    "identity_admissions",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memories_symbolic_fts",
    "memories_symbolic_fts_config",
    "memories_symbolic_fts_content",
    "memories_symbolic_fts_data",
    "memories_symbolic_fts_docsize",
    "memories_symbolic_fts_idx",
    "memory_edges",
    "memory_outbox_destination_apply_receipts",
    "memory_outbox_events",
    "memory_search_generation",
    "mirror_eval_adjudications",
    "mirror_eval_observations",
    "mirror_eval_runs",
    "model_alias_bindings",
    "model_alias_events",
    "model_aliases",
    "model_deployment_events",
    "model_deployment_health",
    "model_deployments",
    "pricing_snapshots",
    "processed_events",
    "provider_account_aliases",
    "provider_account_events",
    "provider_accounts",
    "recall_cache",
    "recall_impression_groups",
    "recall_impressions",
    "rem_source_claims",
    "route_decisions",
    "route_recommendations",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_INVENTORY_11: &[&str] = &[
    "access_history",
    "delivery_events",
    "delivery_intents",
    "derived_items",
    "edge_observations",
    "exact_dedupe_apply_lineage",
    "hard_state",
    "harness_session_attachments",
    "harness_session_capability_advertisements",
    "harness_session_events",
    "harness_session_intervention_results",
    "harness_session_interventions",
    "harness_session_state",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memories_symbolic_fts",
    "memories_symbolic_fts_config",
    "memories_symbolic_fts_content",
    "memories_symbolic_fts_data",
    "memories_symbolic_fts_docsize",
    "memories_symbolic_fts_idx",
    "memory_edges",
    "memory_outbox_destination_apply_receipts",
    "memory_outbox_events",
    "memory_search_generation",
    "processed_events",
    "recall_cache",
    "recall_impression_groups",
    "recall_impressions",
    "rem_source_claims",
    "tachi_events",
];
const FULL_INVENTORY_12: &[&str] = &[
    "a2a_delivery_receipts",
    "a2a_envelopes",
    "access_history",
    "account_custody",
    "agent_identities",
    "agent_known_state",
    "audit_log",
    "delivery_events",
    "delivery_intents",
    "derived_items",
    "dispatch_adjudication_signatures",
    "dispatch_adjudications",
    "dispatch_outcomes",
    "edge_observations",
    "eval_rubric_scores",
    "exact_dedupe_apply_lineage",
    "exec_env_resource_bindings",
    "exec_env_resources",
    "exec_env_worktree_identities",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "harness_session_attachments",
    "harness_session_capability_advertisements",
    "harness_session_events",
    "harness_session_intervention_results",
    "harness_session_interventions",
    "harness_session_state",
    "hub_capabilities",
    "hub_version_routes",
    "identity_admission_verification_receipts",
    "identity_admission_verification_revocations",
    "identity_admissions",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memories_symbolic_fts",
    "memories_symbolic_fts_config",
    "memories_symbolic_fts_content",
    "memories_symbolic_fts_data",
    "memories_symbolic_fts_docsize",
    "memories_symbolic_fts_idx",
    "memory_edges",
    "memory_outbox_destination_apply_receipts",
    "memory_outbox_events",
    "memory_search_generation",
    "mirror_eval_adjudications",
    "mirror_eval_observations",
    "mirror_eval_runs",
    "model_alias_bindings",
    "model_alias_events",
    "model_aliases",
    "model_deployment_events",
    "model_deployment_health",
    "model_deployments",
    "pricing_snapshots",
    "processed_events",
    "provider_account_aliases",
    "provider_account_events",
    "provider_accounts",
    "recall_cache",
    "recall_impression_groups",
    "recall_impressions",
    "rem_source_claims",
    "route_decisions",
    "route_recommendations",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_INVENTORY_13: &[&str] = &[
    "a2a_delivery_receipts",
    "a2a_envelopes",
    "access_history",
    "account_custody",
    "agent_identities",
    "agent_known_state",
    "audit_log",
    "current_truth_assertions",
    "current_truth_projection",
    "current_truth_refresh",
    "delivery_events",
    "delivery_intents",
    "derived_items",
    "dispatch_adjudication_signatures",
    "dispatch_adjudications",
    "dispatch_outcomes",
    "edge_observations",
    "eval_rubric_scores",
    "exact_dedupe_apply_lineage",
    "exec_env_resource_bindings",
    "exec_env_resources",
    "exec_env_worktree_identities",
    "exec_envs",
    "foundry_config",
    "foundry_jobs",
    "hard_state",
    "harness_session_attachments",
    "harness_session_capability_advertisements",
    "harness_session_events",
    "harness_session_intervention_results",
    "harness_session_interventions",
    "harness_session_state",
    "hub_capabilities",
    "hub_version_routes",
    "identity_admission_verification_receipts",
    "identity_admission_verification_revocations",
    "identity_admissions",
    "llm_usage",
    "memories",
    "memories_fts",
    "memories_fts_config",
    "memories_fts_content",
    "memories_fts_data",
    "memories_fts_docsize",
    "memories_fts_idx",
    "memories_symbolic_fts",
    "memories_symbolic_fts_config",
    "memories_symbolic_fts_content",
    "memories_symbolic_fts_data",
    "memories_symbolic_fts_docsize",
    "memories_symbolic_fts_idx",
    "memory_edges",
    "memory_outbox_destination_apply_receipts",
    "memory_outbox_events",
    "memory_search_generation",
    "mirror_eval_adjudications",
    "mirror_eval_observations",
    "mirror_eval_runs",
    "model_alias_bindings",
    "model_alias_events",
    "model_aliases",
    "model_deployment_events",
    "model_deployment_health",
    "model_deployments",
    "pricing_snapshots",
    "processed_events",
    "provider_account_aliases",
    "provider_account_events",
    "provider_accounts",
    "recall_cache",
    "recall_impression_groups",
    "recall_impressions",
    "rem_source_claims",
    "route_decisions",
    "route_recommendations",
    "sandbox_exec_audit",
    "sandbox_policies",
    "sandbox_rules",
    "session_claims",
    "tachi_events",
    "vault_audit",
    "vault_config",
    "vault_entries",
    "vault_key_health",
    "vault_key_rotations",
    "virtual_capability_bindings",
];
const FULL_COLUMNS_0: &[(&str, &str, &str)] = &[
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
    (
        "hub_capabilities",
        "review_status",
        "TEXT NOT NULL DEFAULT 'approved'",
    ),
    (
        "hub_capabilities",
        "health_status",
        "TEXT NOT NULL DEFAULT 'healthy'",
    ),
    ("hub_capabilities", "last_error", "TEXT"),
    ("hub_capabilities", "last_success_at", "TEXT"),
    ("hub_capabilities", "last_failure_at", "TEXT"),
    (
        "hub_capabilities",
        "fail_streak",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    ("hub_capabilities", "active_version", "TEXT"),
    (
        "hub_capabilities",
        "exposure_mode",
        "TEXT NOT NULL DEFAULT 'direct'",
    ),
    ("vault_entries", "allowed_agents", "TEXT"),
];
const FULL_COLUMNS_1: &[(&str, &str, &str)] = &[
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
    (
        "hub_capabilities",
        "review_status",
        "TEXT NOT NULL DEFAULT 'approved'",
    ),
    (
        "hub_capabilities",
        "health_status",
        "TEXT NOT NULL DEFAULT 'healthy'",
    ),
    ("hub_capabilities", "last_error", "TEXT"),
    ("hub_capabilities", "last_success_at", "TEXT"),
    ("hub_capabilities", "last_failure_at", "TEXT"),
    (
        "hub_capabilities",
        "fail_streak",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    ("hub_capabilities", "active_version", "TEXT"),
    (
        "hub_capabilities",
        "exposure_mode",
        "TEXT NOT NULL DEFAULT 'direct'",
    ),
    ("vault_entries", "allowed_agents", "TEXT"),
    ("dispatch_outcomes", "identity_receipt", "TEXT"),
];
const FULL_COLUMNS_2: &[(&str, &str, &str)] = &[
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
    (
        "hub_capabilities",
        "review_status",
        "TEXT NOT NULL DEFAULT 'approved'",
    ),
    (
        "hub_capabilities",
        "health_status",
        "TEXT NOT NULL DEFAULT 'healthy'",
    ),
    ("hub_capabilities", "last_error", "TEXT"),
    ("hub_capabilities", "last_success_at", "TEXT"),
    ("hub_capabilities", "last_failure_at", "TEXT"),
    (
        "hub_capabilities",
        "fail_streak",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    ("hub_capabilities", "active_version", "TEXT"),
    (
        "hub_capabilities",
        "exposure_mode",
        "TEXT NOT NULL DEFAULT 'direct'",
    ),
    ("vault_entries", "allowed_agents", "TEXT"),
];
const FULL_COLUMNS_3: &[(&str, &str, &str)] = &[
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
    (
        "hub_capabilities",
        "review_status",
        "TEXT NOT NULL DEFAULT 'approved'",
    ),
    (
        "hub_capabilities",
        "health_status",
        "TEXT NOT NULL DEFAULT 'healthy'",
    ),
    ("hub_capabilities", "last_error", "TEXT"),
    ("hub_capabilities", "last_success_at", "TEXT"),
    ("hub_capabilities", "last_failure_at", "TEXT"),
    (
        "hub_capabilities",
        "fail_streak",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    ("hub_capabilities", "active_version", "TEXT"),
    (
        "hub_capabilities",
        "exposure_mode",
        "TEXT NOT NULL DEFAULT 'direct'",
    ),
    ("vault_entries", "allowed_agents", "TEXT"),
    ("exec_envs", "agent_identity_id", "TEXT"),
    ("exec_envs", "claim_id", "TEXT"),
    ("session_claims", "mode", "TEXT"),
    ("memories", "scored_count", "INTEGER NOT NULL DEFAULT 0"),
];
const FULL_EARLY_0: &[&str] = &[SQL_0];
const FULL_EARLY_1: &[&str] = &[
    SQL_4,
    SQL_5,
    SQL_9,
    SQL_15,
    SQL_16,
    SQL_18,
    FULL_SQL_0,
    FULL_SQL_1,
    FULL_SQL_2,
    FULL_SQL_3,
    FULL_SQL_4,
    FULL_SQL_5,
    FULL_SQL_6,
    FULL_SQL_7,
    FULL_SQL_8,
    FULL_SQL_9,
    FULL_SQL_10,
    FULL_SQL_11,
    FULL_SQL_12,
    FULL_SQL_13,
    FULL_SQL_14,
    FULL_SQL_15,
];
const FULL_EARLY_2: &[&str] = &[SQL_10, SQL_23, FULL_SQL_16];
const FULL_EARLY_3: &[&str] = &[FULL_SQL_17];
const FULL_EARLY_4: &[&str] = &[SQL_36];
const FULL_EARLY_5: &[&str] = &[FULL_SQL_18];
const FULL_EARLY_6: &[&str] = &[FULL_SQL_19, FULL_SQL_20];
const FULL_EARLY_7: &[&str] = &[FULL_SQL_21];
const FULL_EARLY_8: &[&str] = &[FULL_SQL_22];
const FULL_EARLY_9: &[&str] = &[FULL_SQL_23];
const FULL_EARLY_10: &[&str] = &[SQL_39];
const FULL_EARLY_11: &[&str] = &[FULL_SQL_24, FULL_SQL_25];
const FULL_EARLY_12: &[&str] = &[FULL_SQL_26, FULL_SQL_27, FULL_SQL_28];
const FULL_EARLY_13: &[&str] = &[
    SQL_48,
    SQL_40,
    SQL_45,
    FULL_SQL_29,
    FULL_SQL_30,
    FULL_SQL_31,
    FULL_SQL_32,
    SQL_43,
    FULL_SQL_33,
    FULL_SQL_34,
    FULL_SQL_35,
    FULL_SQL_36,
    FULL_SQL_37,
    FULL_SQL_38,
    FULL_SQL_39,
    FULL_SQL_40,
    FULL_SQL_41,
    FULL_SQL_42,
    FULL_SQL_43,
    FULL_SQL_44,
    FULL_SQL_45,
    FULL_SQL_46,
];
const FULL_EARLY_14: &[&str] = &[FULL_SQL_47, FULL_SQL_48];
const FULL_EARLY_15: &[&str] = &[
    FULL_SQL_49,
    FULL_SQL_50,
    FULL_SQL_51,
    FULL_SQL_52,
    FULL_SQL_53,
    FULL_SQL_54,
    FULL_SQL_55,
    FULL_SQL_56,
    FULL_SQL_57,
    FULL_SQL_58,
    FULL_SQL_59,
    FULL_SQL_60,
    FULL_SQL_61,
    FULL_SQL_62,
    FULL_SQL_63,
    FULL_SQL_64,
    FULL_SQL_65,
    FULL_SQL_66,
];
const FULL_EARLY_16: &[&str] = &[FULL_SQL_67, FULL_SQL_68];
const FULL_EARLY_17: &[&str] = &[
    FULL_SQL_69,
    FULL_SQL_70,
    FULL_SQL_71,
    FULL_SQL_72,
    FULL_SQL_73,
];
const FULL_LATE_0: &[&str] = &[
    SQL_1,
    SQL_2,
    SQL_3,
    SQL_6,
    SQL_7,
    SQL_8,
    SQL_11,
    SQL_12,
    SQL_13,
    SQL_17,
    SQL_19,
    SQL_20,
    SQL_21,
    SQL_22,
    FULL_SQL_74,
    FULL_SQL_75,
    FULL_SQL_76,
    FULL_SQL_77,
    FULL_SQL_78,
    FULL_SQL_79,
    FULL_SQL_80,
    FULL_SQL_81,
    FULL_SQL_82,
    FULL_SQL_83,
    FULL_SQL_84,
    FULL_SQL_85,
    FULL_SQL_86,
    FULL_SQL_87,
    FULL_SQL_88,
    FULL_SQL_89,
    FULL_SQL_90,
    FULL_SQL_91,
    FULL_SQL_92,
    FULL_SQL_93,
    FULL_SQL_94,
    FULL_SQL_95,
    FULL_SQL_96,
    FULL_SQL_97,
    FULL_SQL_98,
    FULL_SQL_99,
    FULL_SQL_100,
    FULL_SQL_101,
    SQL_24,
    FULL_SQL_102,
    FULL_SQL_103,
    FULL_SQL_104,
    SQL_25,
    SQL_26,
    SQL_27,
    SQL_28,
    SQL_29,
    SQL_30,
    FULL_SQL_105,
    FULL_SQL_106,
    SQL_31,
    SQL_32,
    SQL_33,
    SQL_34,
    SQL_35,
    FULL_SQL_107,
    FULL_SQL_108,
    FULL_SQL_109,
    FULL_SQL_110,
];
const FULL_LATE_1: &[&str] = &[SQL_14];
const FULL_LATE_2: &[&str] = &[SQL_37, SQL_38, FULL_SQL_111, FULL_SQL_112, FULL_SQL_113];
const FULL_LATE_3: &[&str] = &[FULL_SQL_114];
const FULL_LATE_4: &[&str] = &[FULL_SQL_115, FULL_SQL_116, FULL_SQL_117, FULL_SQL_118];
const FULL_LATE_5: &[&str] = &[FULL_SQL_119, FULL_SQL_120, FULL_SQL_121];
const FULL_LATE_6: &[&str] = &[FULL_SQL_122, FULL_SQL_123];
const FULL_LATE_7: &[&str] = &[
    FULL_SQL_124,
    FULL_SQL_125,
    FULL_SQL_126,
    FULL_SQL_127,
    FULL_SQL_128,
    FULL_SQL_129,
    FULL_SQL_130,
    FULL_SQL_131,
    SQL_41,
    SQL_46,
    FULL_SQL_132,
    FULL_SQL_133,
    SQL_42,
    FULL_SQL_134,
    FULL_SQL_135,
    FULL_SQL_136,
    FULL_SQL_137,
    FULL_SQL_138,
    FULL_SQL_139,
    FULL_SQL_140,
    FULL_SQL_141,
    FULL_SQL_142,
    FULL_SQL_143,
    FULL_SQL_144,
    FULL_SQL_145,
    FULL_SQL_146,
    FULL_SQL_147,
    FULL_SQL_148,
    FULL_SQL_149,
    FULL_SQL_150,
    FULL_SQL_151,
    FULL_SQL_152,
    FULL_SQL_153,
    FULL_SQL_154,
    FULL_SQL_155,
    FULL_SQL_156,
];
const FULL_LATE_8: &[&str] = &[FULL_SQL_157, FULL_SQL_158, FULL_SQL_159];
const FULL_LATE_9: &[&str] = &[
    FULL_SQL_160,
    FULL_SQL_161,
    FULL_SQL_162,
    FULL_SQL_163,
    FULL_SQL_164,
    FULL_SQL_165,
    FULL_SQL_166,
    FULL_SQL_167,
    FULL_SQL_168,
    FULL_SQL_169,
    FULL_SQL_170,
    FULL_SQL_171,
    FULL_SQL_172,
    FULL_SQL_173,
    FULL_SQL_174,
    FULL_SQL_175,
    FULL_SQL_176,
    FULL_SQL_177,
    FULL_SQL_178,
    FULL_SQL_179,
    FULL_SQL_180,
];
const FULL_LATE_10: &[&str] = &[
    FULL_SQL_181,
    FULL_SQL_182,
    FULL_SQL_183,
    FULL_SQL_184,
    FULL_SQL_185,
    FULL_SQL_186,
    FULL_SQL_187,
    FULL_SQL_188,
    FULL_SQL_189,
    FULL_SQL_190,
];
const FULL_LATE_11: &[&str] = &[FULL_SQL_191, FULL_SQL_192];
const FULL_LATE_12: &[&str] = &[FULL_SQL_193, FULL_SQL_194, FULL_SQL_195, FULL_SQL_196];
const FULL_SQL_0: &str = r###"CREATE TABLE IF NOT EXISTS hub_capabilities (
            id          TEXT PRIMARY KEY,
            type        TEXT NOT NULL,
            name        TEXT NOT NULL,
            version     INTEGER NOT NULL DEFAULT 1,
            description TEXT NOT NULL DEFAULT '',
            definition  TEXT NOT NULL DEFAULT '',
            enabled     INTEGER NOT NULL DEFAULT 1,
            review_status TEXT NOT NULL DEFAULT 'approved',
            health_status TEXT NOT NULL DEFAULT 'healthy',
            last_error    TEXT,
            last_success_at TEXT,
            last_failure_at TEXT,
            fail_streak   INTEGER NOT NULL DEFAULT 0,
            active_version TEXT,
            exposure_mode TEXT NOT NULL DEFAULT 'direct',
            uses        INTEGER NOT NULL DEFAULT 0,
            successes   INTEGER NOT NULL DEFAULT 0,
            failures    INTEGER NOT NULL DEFAULT 0,
            avg_rating  REAL NOT NULL DEFAULT 0.0,
            last_used   TEXT,
            created_at  TEXT NOT NULL DEFAULT '',
            updated_at  TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_1: &str = r###"CREATE TABLE IF NOT EXISTS hub_version_routes (
            alias_id TEXT PRIMARY KEY,
            active_capability_id TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_2: &str = r###"CREATE TABLE IF NOT EXISTS virtual_capability_bindings (
            vc_id         TEXT NOT NULL,
            capability_id TEXT NOT NULL,
            priority      INTEGER NOT NULL DEFAULT 100,
            version_pin   INTEGER,
            enabled       INTEGER NOT NULL DEFAULT 1,
            metadata      TEXT NOT NULL DEFAULT '{}',
            created_at    TEXT NOT NULL DEFAULT '',
            updated_at    TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (vc_id, capability_id)
        );"###;
const FULL_SQL_3: &str = r###"CREATE TABLE IF NOT EXISTS audit_log (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp   TEXT NOT NULL,
            server_id   TEXT NOT NULL,
            tool_name   TEXT NOT NULL,
            args_hash   TEXT NOT NULL DEFAULT '',
            success     INTEGER NOT NULL DEFAULT 1,
            duration_ms INTEGER NOT NULL DEFAULT 0,
            error_kind  TEXT,
            created_at  TEXT NOT NULL DEFAULT (STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );"###;
const FULL_SQL_4: &str = r###"CREATE TABLE IF NOT EXISTS llm_usage (
            id                    INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp             TEXT NOT NULL,
            lane                  TEXT NOT NULL,
            model                 TEXT NOT NULL,
            provider_host         TEXT NOT NULL DEFAULT '',
            provider_logical_name TEXT NOT NULL DEFAULT '',
            provider_key_id       TEXT NOT NULL DEFAULT '',
            prompt_tokens         INTEGER,
            completion_tokens     INTEGER,
            total_tokens          INTEGER,
            max_tokens            INTEGER NOT NULL DEFAULT 0,
            request_chars         INTEGER NOT NULL DEFAULT 0,
            response_chars        INTEGER NOT NULL DEFAULT 0,
            duration_ms           INTEGER NOT NULL DEFAULT 0,
            success               INTEGER NOT NULL DEFAULT 1,
            created_at            TEXT NOT NULL DEFAULT (STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );"###;
const FULL_SQL_5: &str = r###"CREATE TABLE IF NOT EXISTS agent_known_state (
            agent_id   TEXT NOT NULL,
            memory_id  TEXT NOT NULL,
            revision   INTEGER NOT NULL DEFAULT 0,
            synced_at  TEXT NOT NULL,
            PRIMARY KEY (agent_id, memory_id)
        );"###;
const FULL_SQL_6: &str = r###"CREATE TABLE IF NOT EXISTS sandbox_rules (
            agent_role   TEXT NOT NULL,
            path_pattern TEXT NOT NULL,
            access_level TEXT NOT NULL DEFAULT 'read',
            created_at   TEXT NOT NULL,
            PRIMARY KEY (agent_role, path_pattern)
        );"###;
const FULL_SQL_7: &str = r###"CREATE TABLE IF NOT EXISTS sandbox_policies (
            capability_id    TEXT PRIMARY KEY,
            runtime_type     TEXT NOT NULL DEFAULT 'process',
            env_allowlist    TEXT NOT NULL DEFAULT '[]',
            fs_read_roots    TEXT NOT NULL DEFAULT '[]',
            fs_write_roots   TEXT NOT NULL DEFAULT '[]',
            cwd_roots        TEXT NOT NULL DEFAULT '[]',
            max_startup_ms   INTEGER NOT NULL DEFAULT 30000,
            max_tool_ms      INTEGER NOT NULL DEFAULT 30000,
            max_concurrency  INTEGER NOT NULL DEFAULT 1,
            enabled          INTEGER NOT NULL DEFAULT 1,
            created_at       TEXT NOT NULL DEFAULT '',
            updated_at       TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_8: &str = r###"CREATE TABLE IF NOT EXISTS sandbox_exec_audit (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp      TEXT NOT NULL,
            capability_id  TEXT NOT NULL,
            stage          TEXT NOT NULL DEFAULT 'preflight',
            decision       TEXT NOT NULL,
            reason         TEXT,
            duration_ms    INTEGER NOT NULL DEFAULT 0,
            tool_name      TEXT,
            error_kind     TEXT,
            metadata       TEXT NOT NULL DEFAULT '{}',
            created_at     TEXT NOT NULL DEFAULT (STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now'))
        );"###;
const FULL_SQL_9: &str = r###"CREATE TABLE IF NOT EXISTS vault_config (
            id              INTEGER PRIMARY KEY CHECK (id = 1),
            salt            TEXT NOT NULL,
            verifier        TEXT NOT NULL,
            kdf_algorithm   TEXT NOT NULL DEFAULT 'argon2id',
            kdf_params      TEXT NOT NULL DEFAULT '{"m":65536,"t":3,"p":4}',
            cipher          TEXT NOT NULL DEFAULT 'aes-256-gcm',
            created_at      TEXT NOT NULL DEFAULT '',
            updated_at      TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_10: &str = r###"CREATE TABLE IF NOT EXISTS vault_entries (
            name            TEXT PRIMARY KEY,
            encrypted_value TEXT NOT NULL,
            nonce           TEXT NOT NULL,
            secret_type     TEXT NOT NULL DEFAULT 'api_key',
            description     TEXT NOT NULL DEFAULT '',
            allowed_agents  TEXT,
            created_at      TEXT NOT NULL DEFAULT '',
            updated_at      TEXT NOT NULL DEFAULT '',
            accessed_at     TEXT NOT NULL DEFAULT '',
            access_count    INTEGER NOT NULL DEFAULT 0
        );"###;
const FULL_SQL_11: &str = r###"CREATE TABLE IF NOT EXISTS vault_audit (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp       TEXT NOT NULL,
            operation       TEXT NOT NULL,
            secret_name     TEXT,
            success         INTEGER NOT NULL DEFAULT 1,
            detail          TEXT
        );"###;
const FULL_SQL_12: &str = r###"CREATE TABLE IF NOT EXISTS vault_key_rotations (
            prefix              TEXT PRIMARY KEY,
            current_index       INTEGER NOT NULL DEFAULT 1,
            total_keys          INTEGER NOT NULL DEFAULT 0,
            rotation_strategy   TEXT NOT NULL DEFAULT 'round_robin',
            created_at          TEXT NOT NULL DEFAULT '',
            updated_at          TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_13: &str = r###"CREATE TABLE IF NOT EXISTS vault_key_health (
            logical_name    TEXT NOT NULL,
            key_id          TEXT NOT NULL,
            status          TEXT NOT NULL DEFAULT 'ok',
            cooldown_until  TEXT,
            last_success    TEXT,
            last_attempt    TEXT,
            last_error      TEXT,
            error_count     INTEGER NOT NULL DEFAULT 0,
            auth_failed     INTEGER NOT NULL DEFAULT 0,
            disabled        INTEGER NOT NULL DEFAULT 0,
            metadata        TEXT NOT NULL DEFAULT '{}',
            updated_at      TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (logical_name, key_id)
        );"###;
const FULL_SQL_14: &str = r###"CREATE TABLE IF NOT EXISTS foundry_jobs (
            id            TEXT PRIMARY KEY,
            kind          TEXT NOT NULL,
            lane          TEXT NOT NULL,
            status        TEXT NOT NULL DEFAULT 'queued',
            target_db     TEXT NOT NULL DEFAULT 'project',
            named_project TEXT,
            path_prefix   TEXT NOT NULL DEFAULT '/',
            memory_ids    TEXT NOT NULL DEFAULT '[]',
            target_agent_id TEXT,
            requested_by  TEXT,
            evidence_count INTEGER NOT NULL DEFAULT 0,
            goal_count    INTEGER NOT NULL DEFAULT 1,
            metadata      TEXT NOT NULL DEFAULT '{}',
            created_at    TEXT NOT NULL DEFAULT '',
            updated_at    TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_15: &str = r###"CREATE TABLE IF NOT EXISTS foundry_config (
            id                       INTEGER PRIMARY KEY CHECK (id = 1),
            enabled                  INTEGER NOT NULL DEFAULT 1,
            max_jobs_per_minute      INTEGER NOT NULL DEFAULT 10,
            distill_concurrency      INTEGER NOT NULL DEFAULT 1,
            enrichment_concurrency   INTEGER NOT NULL DEFAULT 1,
            llm_provider_override    TEXT,
            updated_at               TEXT NOT NULL DEFAULT '',
            updated_by               TEXT NOT NULL DEFAULT 'default'
        );"###;
const FULL_SQL_16: &str = r###"CREATE TABLE IF NOT EXISTS session_claims (
            claim_id             TEXT PRIMARY KEY,
            session_client       TEXT,
            issue_ref            TEXT,
            flow_id              TEXT,
            dispatch_id          TEXT,
            branch               TEXT NOT NULL DEFAULT '',
            declared_file_scope  TEXT,
            state                TEXT NOT NULL DEFAULT 'active',
            release_reason       TEXT,
            created_at           TEXT NOT NULL DEFAULT '',
            heartbeat_at         TEXT NOT NULL DEFAULT '',
            released_at          TEXT
        );"###;
const FULL_SQL_17: &str = r###"CREATE TABLE IF NOT EXISTS exec_envs (
            env_id         TEXT PRIMARY KEY,
            kind           TEXT NOT NULL DEFAULT 'worktree',
            path           TEXT NOT NULL,
            repo_root      TEXT NOT NULL DEFAULT '',
            branch         TEXT NOT NULL DEFAULT '',
            base_sha       TEXT NOT NULL DEFAULT '',
            dispatch_id    TEXT,
            state          TEXT NOT NULL DEFAULT 'active',
            reclaim_reason TEXT,
            schema_version INTEGER NOT NULL DEFAULT 1,
            created_at     TEXT NOT NULL DEFAULT '',
            reclaimed_at   TEXT
        );"###;
const FULL_SQL_18: &str = r###"CREATE TABLE IF NOT EXISTS dispatch_outcomes (
            outcome_id       TEXT PRIMARY KEY,
            dispatch_id      TEXT NOT NULL DEFAULT '',
            eval_memory_id   TEXT,
            model            TEXT,
            vendor           TEXT NOT NULL DEFAULT 'unknown',
            role             TEXT,
            seat             TEXT,
            task_type        TEXT,
            execution_outcome    TEXT NOT NULL,
            retry_count      INTEGER NOT NULL DEFAULT 0,
            error_class      TEXT,
            issue_ref        TEXT,
            pr_ref           TEXT,
            flow_id          TEXT,
            cost_tokens      INTEGER,
            cost_usd         REAL,
            verification_present INTEGER NOT NULL DEFAULT 0,
            diff_present         INTEGER NOT NULL DEFAULT 0,
            evidence_refs    TEXT NOT NULL DEFAULT '[]',
            idempotency_key  TEXT NOT NULL,
            created_at       TEXT NOT NULL DEFAULT '',
            updated_at       TEXT NOT NULL DEFAULT '',
            UNIQUE (idempotency_key)
        );"###;
const FULL_SQL_19: &str = r###"CREATE TABLE IF NOT EXISTS exec_env_resources (
            resource_id     TEXT PRIMARY KEY,

            kind            TEXT NOT NULL,

            path            TEXT NOT NULL,

            bytes           INTEGER,
            measured_at     TEXT,

            state           TEXT NOT NULL DEFAULT 'active',
            reclaim_reason  TEXT,
            reclaimed_at    TEXT,

            reclaimed_bytes INTEGER,
            created_at      TEXT NOT NULL DEFAULT '',
            updated_at      TEXT NOT NULL DEFAULT '',
            UNIQUE (path, kind)
        );"###;
const FULL_SQL_20: &str = r###"CREATE TABLE IF NOT EXISTS exec_env_resource_bindings (
            binding_id  TEXT PRIMARY KEY,
            env_id      TEXT NOT NULL,
            resource_id TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT '',
            released_at TEXT,
            UNIQUE (env_id, resource_id)
        );"###;
const FULL_SQL_21: &str = r###"CREATE TABLE IF NOT EXISTS dispatch_outcomes (
            outcome_id       TEXT PRIMARY KEY,
            dispatch_id      TEXT NOT NULL DEFAULT '',
            eval_memory_id   TEXT,
            model            TEXT,
            vendor           TEXT NOT NULL DEFAULT 'unknown',
            role             TEXT,
            seat             TEXT,
            task_type        TEXT,

            execution_outcome    TEXT NOT NULL,

            reported_outcome     TEXT,


            retry_count      INTEGER NOT NULL DEFAULT 0,
            error_class      TEXT,
            issue_ref        TEXT,
            pr_ref           TEXT,
            flow_id          TEXT,
            cost_tokens      INTEGER,
            cost_usd         REAL,
            verification_present INTEGER NOT NULL DEFAULT 0,
            diff_present         INTEGER NOT NULL DEFAULT 0,
            evidence_refs    TEXT NOT NULL DEFAULT '[]',
            idempotency_key  TEXT NOT NULL,
            created_at       TEXT NOT NULL DEFAULT '',
            updated_at       TEXT NOT NULL DEFAULT '',
            UNIQUE (idempotency_key)
        );"###;
const FULL_SQL_22: &str = r###"CREATE TABLE IF NOT EXISTS exec_envs (
            env_id         TEXT PRIMARY KEY,
            kind           TEXT NOT NULL DEFAULT 'worktree',
            path           TEXT NOT NULL,
            repo_root      TEXT NOT NULL DEFAULT '',
            branch         TEXT NOT NULL DEFAULT '',
            base_sha       TEXT NOT NULL DEFAULT '',
            dispatch_id    TEXT,
            env_class      TEXT NOT NULL DEFAULT 'edit-only',
            state          TEXT NOT NULL DEFAULT 'active',
            reclaim_reason TEXT,
            schema_version INTEGER NOT NULL DEFAULT 1,
            created_at     TEXT NOT NULL DEFAULT '',
            reclaimed_at   TEXT
        );"###;
const FULL_SQL_23: &str = r###"CREATE TABLE IF NOT EXISTS dispatch_outcomes (
            outcome_id       TEXT PRIMARY KEY,
            dispatch_id      TEXT NOT NULL DEFAULT '',
            eval_memory_id   TEXT,
            model            TEXT,
            vendor           TEXT NOT NULL DEFAULT 'unknown',
            role             TEXT,
            seat             TEXT,
            task_type        TEXT,

            execution_outcome    TEXT NOT NULL,

            reported_outcome     TEXT,


            retry_count      INTEGER NOT NULL DEFAULT 0,
            error_class      TEXT,
            issue_ref        TEXT,
            pr_ref           TEXT,
            flow_id          TEXT,
            cost_tokens      INTEGER,
            cost_usd         REAL,
            verification_present INTEGER NOT NULL DEFAULT 0,
             diff_present         INTEGER NOT NULL DEFAULT 0,
             evidence_refs    TEXT NOT NULL DEFAULT '[]',


             identity_receipt TEXT,






             identity_attribution_basis TEXT NOT NULL DEFAULT 'unknown',
             idempotency_key  TEXT NOT NULL,
            created_at       TEXT NOT NULL DEFAULT '',
            updated_at       TEXT NOT NULL DEFAULT '',
            UNIQUE (idempotency_key)
        );"###;
const FULL_SQL_24: &str = r###"CREATE TABLE IF NOT EXISTS dispatch_adjudications (
            adjudication_id TEXT PRIMARY KEY,
            outcome_id TEXT NOT NULL,
            event_key TEXT NOT NULL UNIQUE,
            verdict TEXT,
            not_required_reason TEXT,
            actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            evidence_ref TEXT NOT NULL CHECK (length(trim(evidence_ref)) > 0),
            created_at TEXT NOT NULL DEFAULT '',
            insertion_seq INTEGER NOT NULL,
            UNIQUE (outcome_id, insertion_seq),
            CHECK (
                (verdict IS NOT NULL AND length(trim(verdict)) > 0 AND not_required_reason IS NULL)
                OR
                (verdict IS NULL AND not_required_reason IS NOT NULL AND length(trim(not_required_reason)) > 0)
            )
        );"###;
const FULL_SQL_25: &str = r###"CREATE TABLE IF NOT EXISTS dispatch_adjudication_signatures (
            adjudication_id TEXT NOT NULL,
            signature_id TEXT NOT NULL,
            evidence_ref TEXT,
            resolved INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1)),
            PRIMARY KEY (adjudication_id, signature_id)
        );"###;
const FULL_SQL_26: &str = r###"CREATE TABLE IF NOT EXISTS mirror_eval_runs (
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
        );"###;
const FULL_SQL_27: &str = r###"CREATE TABLE IF NOT EXISTS mirror_eval_observations (
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
        );"###;
const FULL_SQL_28: &str = r###"CREATE TABLE IF NOT EXISTS mirror_eval_adjudications (
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
        );"###;
const FULL_SQL_29: &str = r###"CREATE TABLE IF NOT EXISTS provider_accounts (
            account_id            TEXT PRIMARY KEY,
            provider_kind         TEXT NOT NULL,
            auth_mode             TEXT NOT NULL CHECK (auth_mode IN (
                                      'api_key_pool', 'brokered_oauth', 'cloud_iam',
                                      'provider_owned_session', 'local_no_auth', 'unsupported'
                                  )),
            auth_ref              TEXT UNIQUE,
            account_fingerprint   TEXT NOT NULL,
            account_class         TEXT NOT NULL DEFAULT 'model_api',
            capabilities          TEXT NOT NULL DEFAULT '[]',
            credential_policy_ref TEXT,
            refresh_authority     TEXT NOT NULL DEFAULT 'none',
            status                TEXT NOT NULL DEFAULT 'active',
            revision              INTEGER NOT NULL DEFAULT 1,
            source_refs           TEXT NOT NULL DEFAULT '[]',
            created_at            TEXT NOT NULL,
            updated_at            TEXT NOT NULL
        );"###;
const FULL_SQL_30: &str = r###"CREATE TABLE IF NOT EXISTS provider_account_aliases (
            account_id  TEXT NOT NULL,
            alias_name  TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            first_seen  TEXT NOT NULL,
            last_seen   TEXT NOT NULL,
            retired     INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (account_id, alias_name)
        );"###;
const FULL_SQL_31: &str = r###"CREATE TABLE IF NOT EXISTS provider_account_events (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id  TEXT NOT NULL,
            revision    INTEGER NOT NULL,
            event_kind  TEXT NOT NULL,
            plan_digest TEXT,
            evidence    TEXT NOT NULL DEFAULT '{}',
            created_at  TEXT NOT NULL
        );"###;
const FULL_SQL_32: &str = r###"CREATE TABLE IF NOT EXISTS account_custody (
            auth_ref       TEXT PRIMARY KEY,
            account_id     TEXT NOT NULL UNIQUE,
            custody_kind   TEXT NOT NULL CHECK (custody_kind IN ('vault_rotation_pool', 'vault_entry')),
            custody_target TEXT NOT NULL,
            revision       INTEGER NOT NULL,
            updated_at     TEXT NOT NULL
        );"###;
const FULL_SQL_33: &str = r###"CREATE TABLE IF NOT EXISTS exec_envs (
            env_id         TEXT PRIMARY KEY,
            kind           TEXT NOT NULL DEFAULT 'worktree',
            path           TEXT NOT NULL,
            repo_root      TEXT NOT NULL DEFAULT '',
            branch         TEXT NOT NULL DEFAULT '',
            base_sha       TEXT NOT NULL DEFAULT '',
            dispatch_id    TEXT,
            agent_identity_id TEXT,
            claim_id       TEXT,
            env_class      TEXT NOT NULL DEFAULT 'edit-only',
            state          TEXT NOT NULL DEFAULT 'active',
            reclaim_reason TEXT,
            schema_version INTEGER NOT NULL DEFAULT 1,
            created_at     TEXT NOT NULL DEFAULT '',
            reclaimed_at   TEXT
        );"###;
const FULL_SQL_34: &str = r###"CREATE TABLE IF NOT EXISTS route_recommendations (
            recommendation_id      TEXT PRIMARY KEY,
            task_type               TEXT,
            risk                    TEXT NOT NULL DEFAULT 'unknown',
            candidates              TEXT NOT NULL DEFAULT '[]',
            recommended_profile     TEXT,
            policy_source_revision  TEXT,
            rows_considered         INTEGER NOT NULL DEFAULT 0,
            occurred_at             TEXT NOT NULL DEFAULT '',
            recorded_at             TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_35: &str = r###"CREATE TABLE IF NOT EXISTS route_decisions (
            route_decision_id  TEXT PRIMARY KEY,
            dispatch_id         TEXT NOT NULL,
            recommendation_id   TEXT,
            selected_profile    TEXT,
            selected_model      TEXT,
            assignment_mode     TEXT NOT NULL CHECK (assignment_mode IN ('advised', 'unadvised', 'user_forced', 'experiment')),
            override_flag       INTEGER NOT NULL DEFAULT 0 CHECK (override_flag IN (0, 1)),
            contract_hash       TEXT,
            env_id              TEXT,
            host_profile        TEXT,
            work_claim_id       TEXT,
            occurred_at         TEXT NOT NULL DEFAULT '',
            recorded_at         TEXT NOT NULL DEFAULT '',
            UNIQUE (dispatch_id)
        );"###;
const FULL_SQL_36: &str = r###"CREATE TABLE IF NOT EXISTS eval_rubric_scores (
            rubric_score_id          TEXT PRIMARY KEY,
            adjudication_id          TEXT NOT NULL,
            subject_kind             TEXT NOT NULL CHECK (subject_kind IN ('dispatch', 'mirror')),
            rubric_hash              TEXT NOT NULL,
            contract_correctness     TEXT NOT NULL CHECK (contract_correctness IN ('pass', 'concern', 'fail', 'not_assessed')),
            evidence_quality         TEXT NOT NULL CHECK (evidence_quality IN ('pass', 'concern', 'fail', 'not_assessed')),
            safety                   TEXT NOT NULL CHECK (safety IN ('pass', 'concern', 'fail', 'not_assessed')),
            scope_discipline         TEXT NOT NULL CHECK (scope_discipline IN ('pass', 'concern', 'fail', 'not_assessed')),
            intervention_burden      TEXT NOT NULL CHECK (intervention_burden IN ('pass', 'concern', 'fail', 'not_assessed')),
            completion_integrity     TEXT NOT NULL CHECK (completion_integrity IN ('pass', 'concern', 'fail', 'not_assessed')),
            adjudication_confidence  TEXT NOT NULL CHECK (adjudication_confidence IN ('low', 'medium', 'high')),
            adjudicator_actor        TEXT NOT NULL CHECK (length(trim(adjudicator_actor)) > 0),
            adjudicator_vendor       TEXT NOT NULL DEFAULT 'unknown',
            independence_basis       TEXT NOT NULL CHECK (independence_basis IN ('structural_cross_vendor', 'declared_only', 'self', 'identity_bound')),
            occurred_at              TEXT NOT NULL DEFAULT '',
            recorded_at              TEXT NOT NULL DEFAULT '',
            UNIQUE (subject_kind, adjudication_id)
        );"###;
const FULL_SQL_37: &str = r###"CREATE TABLE IF NOT EXISTS session_claims (
            claim_id             TEXT PRIMARY KEY,
            session_client       TEXT,
            issue_ref            TEXT,
            flow_id              TEXT,
            dispatch_id          TEXT,
            branch               TEXT NOT NULL DEFAULT '',
            worktree_path        TEXT,
            declared_file_scope  TEXT,
            agent_identity_id    TEXT,
            role                 TEXT,
            mode                 TEXT,
            expected_head        TEXT,
            lease_expires_at     TEXT,
            transition_version   INTEGER NOT NULL DEFAULT 0,
            exec_env_id          TEXT,
            orphaned_at          TEXT,
            state                TEXT NOT NULL DEFAULT 'active',
            release_reason       TEXT,
            created_at           TEXT NOT NULL DEFAULT '',
            heartbeat_at         TEXT NOT NULL DEFAULT '',
            released_at          TEXT
        );"###;
const FULL_SQL_38: &str = r###"CREATE TABLE IF NOT EXISTS agent_identities (
            agent_identity_id TEXT PRIMARY KEY,
            display_name      TEXT,
            seat              TEXT,
            capability_json   TEXT,
            created_at        TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_39: &str = r###"CREATE TABLE IF NOT EXISTS identity_admissions (
            admission_id      TEXT PRIMARY KEY,
            agent_identity_id TEXT,
            connection_id     TEXT NOT NULL,
            state             TEXT NOT NULL CHECK (state IN ('self_asserted', 'verified', 'rejected', 'unavailable')),
            rejection_evidence TEXT,
            created_at        TEXT NOT NULL DEFAULT '',
            UNIQUE(agent_identity_id, connection_id)
        );"###;
const FULL_SQL_40: &str = r###"CREATE TABLE IF NOT EXISTS recall_impression_groups (
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
        );"###;
const FULL_SQL_41: &str = r###"CREATE TABLE IF NOT EXISTS recall_impressions (
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
        );"###;
const FULL_SQL_42: &str = r###"CREATE TABLE IF NOT EXISTS rem_source_claims (
            source_key      TEXT PRIMARY KEY NOT NULL,
            source_identity TEXT NOT NULL,
            draft_id        TEXT NOT NULL,
            claimed_at      TEXT NOT NULL
        );"###;
const FULL_SQL_43: &str = r###"CREATE TABLE IF NOT EXISTS exact_dedupe_apply_lineage (
            loser_id          TEXT PRIMARY KEY NOT NULL,
            apply_id          TEXT NOT NULL,
            plan_digest       TEXT NOT NULL,
            winner_id         TEXT NOT NULL,
            before_revision   INTEGER NOT NULL,
            archived_revision INTEGER NOT NULL,
            loser_valid_until_before TEXT,
            applied_at        TEXT NOT NULL
        );"###;
const FULL_SQL_44: &str = r###"CREATE TABLE IF NOT EXISTS memory_outbox_events (
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
        );"###;
const FULL_SQL_45: &str = r###"CREATE TABLE IF NOT EXISTS memory_outbox_destination_apply_receipts (
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
        );"###;
const FULL_SQL_46: &str = r###"CREATE TABLE IF NOT EXISTS memory_search_generation (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        generation INTEGER NOT NULL CHECK (generation >= 0)
    );"###;
const FULL_SQL_47: &str = r###"CREATE TABLE IF NOT EXISTS a2a_envelopes (
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
        );"###;
const FULL_SQL_48: &str = r###"CREATE TABLE IF NOT EXISTS a2a_delivery_receipts (
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
        );"###;
const FULL_SQL_49: &str = r###"CREATE TABLE IF NOT EXISTS model_deployments (
            deployment_id        TEXT PRIMARY KEY,
            provider_account_id  TEXT NOT NULL,
            endpoint_ref         TEXT,
            protocol_kind        TEXT NOT NULL,
            provider_model_id    TEXT NOT NULL,
            effective_version    TEXT,
            capabilities         TEXT NOT NULL DEFAULT '{}',
            context_window       INTEGER,
            max_output           INTEGER,
            attachment_bounds    TEXT NOT NULL DEFAULT '{}',
            region               TEXT,
            data_policy          TEXT,
            pricing_snapshot_ref TEXT,
            catalog_source       TEXT NOT NULL,
            fetched_at           TEXT NOT NULL,
            effective_at         TEXT NOT NULL,
            expires_at           TEXT,
            status               TEXT NOT NULL DEFAULT 'active',
            revision             INTEGER NOT NULL DEFAULT 1,
            source_refs          TEXT NOT NULL DEFAULT '[]',
            created_at           TEXT NOT NULL,
            updated_at           TEXT NOT NULL
        );"###;
const FULL_SQL_50: &str = r###"CREATE TABLE IF NOT EXISTS model_deployment_events (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            deployment_id TEXT NOT NULL,
            revision      INTEGER NOT NULL,
            event_kind    TEXT NOT NULL,
            plan_digest   TEXT,
            evidence      TEXT NOT NULL DEFAULT '{}',
            created_at    TEXT NOT NULL
        );"###;
const FULL_SQL_51: &str = r###"CREATE TABLE IF NOT EXISTS model_aliases (
            alias_name            TEXT PRIMARY KEY,
            required_capabilities TEXT NOT NULL DEFAULT '{}',
            constraints           TEXT NOT NULL DEFAULT '{}',
            status                TEXT NOT NULL DEFAULT 'active',
            revision              INTEGER NOT NULL DEFAULT 1,
            policy_digest         TEXT,
            source_refs           TEXT NOT NULL DEFAULT '[]',
            created_at            TEXT NOT NULL,
            updated_at            TEXT NOT NULL
        );"###;
const FULL_SQL_52: &str = r###"CREATE TABLE IF NOT EXISTS model_alias_bindings (
            alias_name    TEXT NOT NULL,
            deployment_id TEXT NOT NULL,
            priority      INTEGER NOT NULL DEFAULT 0,
            retired       INTEGER NOT NULL DEFAULT 0,
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL,
            PRIMARY KEY (alias_name, deployment_id)
        );"###;
const FULL_SQL_53: &str = r###"CREATE TABLE IF NOT EXISTS model_alias_events (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            alias_name  TEXT NOT NULL,
            revision    INTEGER NOT NULL,
            event_kind  TEXT NOT NULL,
            plan_digest TEXT,
            evidence    TEXT NOT NULL DEFAULT '{}',
            created_at  TEXT NOT NULL
        );"###;
const FULL_SQL_54: &str = r###"CREATE TABLE IF NOT EXISTS pricing_snapshots (
            snapshot_id    TEXT PRIMARY KEY,
            provider_kind  TEXT NOT NULL,
            pricing_data   TEXT NOT NULL DEFAULT '{}',
            catalog_source TEXT,
            fetched_at     TEXT NOT NULL,
            created_at     TEXT NOT NULL
        );"###;
const FULL_SQL_55: &str = r###"CREATE TABLE IF NOT EXISTS model_deployment_health (
            deployment_id   TEXT PRIMARY KEY,
            state           TEXT NOT NULL DEFAULT 'ok',
            cooldown_until  TEXT,
            last_success_at TEXT,
            last_attempt_at TEXT,
            last_error      TEXT,
            error_count     INTEGER NOT NULL DEFAULT 0,
            evidence_kind   TEXT,
            observed_at     TEXT NOT NULL DEFAULT '',
            metadata        TEXT NOT NULL DEFAULT '{}',
            updated_at      TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_56: &str = r###"CREATE TABLE a2a_envelopes (
            envelope_id                    TEXT PRIMARY KEY NOT NULL,
            kind                           TEXT NOT NULL CHECK (kind = 'turn_response/v1'),
            issuer_agent_identity_id       TEXT NOT NULL,
            issuer_admission_id            TEXT NOT NULL,
            recipient_agent_identity_id    TEXT NOT NULL,
            recipient_admission_id         TEXT NOT NULL,
            subject_ref                    TEXT NOT NULL,
            body                           TEXT,
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
            CHECK (body IS NULL OR length(CAST(body AS BLOB)) BETWEEN 1 AND 4096),
            CHECK (expires_at > created_at),
            FOREIGN KEY (issuer_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
            FOREIGN KEY (issuer_admission_id) REFERENCES identity_admissions(admission_id),
            FOREIGN KEY (recipient_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
            FOREIGN KEY (recipient_admission_id) REFERENCES identity_admissions(admission_id)
        );"###;
const FULL_SQL_57: &str = r###"CREATE TABLE a2a_delivery_receipts (
            receipt_id                     TEXT PRIMARY KEY NOT NULL,
            envelope_id                    TEXT NOT NULL,
            envelope_version               INTEGER NOT NULL CHECK (envelope_version > 0),
            state                          TEXT NOT NULL CHECK (state IN ('received','accepted','consumed','expired')),
            actor_agent_identity_id        TEXT NOT NULL,
            actor_admission_id             TEXT NOT NULL,
            identity_assurance             TEXT NOT NULL CHECK (identity_assurance = 'self_asserted'),
            trust_domain                   TEXT NOT NULL CHECK (trust_domain = 'same_host'),
            trust_basis                    TEXT NOT NULL CHECK (trust_basis IN ('current_local_connection','historical_local_admission')),
            occurred_at                    TEXT NOT NULL,
            UNIQUE (envelope_id, envelope_version),
            UNIQUE (envelope_id, state),
            FOREIGN KEY (envelope_id) REFERENCES a2a_envelopes(envelope_id),
            FOREIGN KEY (actor_agent_identity_id) REFERENCES agent_identities(agent_identity_id),
            FOREIGN KEY (actor_admission_id) REFERENCES identity_admissions(admission_id)
        );"###;
const FULL_SQL_58: &str = r###"CREATE TABLE IF NOT EXISTS harness_session_attachments (
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
        );"###;
const FULL_SQL_59: &str = r###"CREATE TABLE IF NOT EXISTS harness_session_events (
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
        );"###;
const FULL_SQL_60: &str = r###"CREATE TABLE IF NOT EXISTS harness_session_state (
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
        );"###;
const FULL_SQL_61: &str = r###"CREATE TABLE IF NOT EXISTS harness_session_interventions (
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
        );"###;
const FULL_SQL_62: &str = r###"CREATE TABLE IF NOT EXISTS harness_session_intervention_results (
            result_row_id INTEGER PRIMARY KEY AUTOINCREMENT,
            attachment_id TEXT NOT NULL REFERENCES harness_session_attachments(attachment_id),
            request_id TEXT NOT NULL,
            disposition TEXT NOT NULL CHECK (disposition IN ('accepted', 'refused', 'unsupported', 'failed')),
            authority_confirmation_ref TEXT CHECK (authority_confirmation_ref IS NULL OR (length(authority_confirmation_ref) <= 128 AND length(trim(authority_confirmation_ref)) > 0 AND instr(CAST(authority_confirmation_ref AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            detail TEXT CHECK (detail IS NULL OR (length(detail) > 0 AND length(detail) <= 2000 AND instr(CAST(detail AS BLOB), CAST(x'00' AS BLOB)) = 0)),
            recorded_at TEXT NOT NULL,
            source_host_identity TEXT NOT NULL CHECK (length(trim(source_host_identity)) > 0),
            UNIQUE (attachment_id, request_id)
        );"###;
const FULL_SQL_63: &str = r###"CREATE TABLE IF NOT EXISTS harness_session_capability_advertisements (
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
const FULL_SQL_64: &str = r###"CREATE TABLE IF NOT EXISTS delivery_intents (
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
        );"###;
const FULL_SQL_65: &str = r###"CREATE TABLE IF NOT EXISTS delivery_events (
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
        );"###;
const FULL_SQL_66: &str = r###"CREATE TABLE IF NOT EXISTS exec_env_worktree_identities (
            env_id      TEXT PRIMARY KEY,
            device      INTEGER NOT NULL,
            inode       INTEGER NOT NULL,
            captured_at TEXT NOT NULL DEFAULT ''
        );"###;
const FULL_SQL_67: &str = r###"CREATE TABLE identity_admission_verification_receipts (
    receipt_id TEXT PRIMARY KEY,
    admission_id TEXT NOT NULL UNIQUE REFERENCES identity_admissions(admission_id),
    agent_identity_id TEXT NOT NULL REFERENCES agent_identities(agent_identity_id),
    connection_id TEXT NOT NULL,
    issuer_id TEXT NOT NULL,
    verification_method TEXT NOT NULL,
    verification_version TEXT NOT NULL,
    trust_domain TEXT NOT NULL,
    verification_scope TEXT NOT NULL,
    evidence_digest TEXT NOT NULL CHECK (
        length(evidence_digest) = 64
        AND evidence_digest NOT GLOB '*[^0-9a-f]*'
    ),
    evidence_ref TEXT NOT NULL CHECK (
        length(evidence_ref) BETWEEN 1 AND 256
        AND evidence_ref NOT GLOB '*[^a-zA-Z0-9:._/-]*'
    ),
    evidence_issued_at TEXT NOT NULL,
    evidence_expires_at TEXT NOT NULL,
    nonce TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL CHECK (
        length(request_digest) = 64
        AND request_digest NOT GLOB '*[^0-9a-f]*'
    ),
    current_state TEXT NOT NULL CHECK (current_state = 'verified'),
    verified_at TEXT NOT NULL,
    UNIQUE (issuer_id, idempotency_key),
    UNIQUE (issuer_id, trust_domain, nonce),
    FOREIGN KEY (admission_id, agent_identity_id, connection_id)
        REFERENCES identity_admissions(admission_id, agent_identity_id, connection_id)
);"###;
const FULL_SQL_68: &str = r###"CREATE TABLE identity_admission_verification_revocations (
    revocation_id TEXT PRIMARY KEY,
    admission_id TEXT NOT NULL UNIQUE
        REFERENCES identity_admission_verification_receipts(admission_id),
    issuer_id TEXT NOT NULL,
    evidence_digest TEXT NOT NULL CHECK (
        length(evidence_digest) = 64
        AND evidence_digest NOT GLOB '*[^0-9a-f]*'
    ),
    evidence_ref TEXT NOT NULL CHECK (
        length(evidence_ref) BETWEEN 1 AND 256
        AND evidence_ref NOT GLOB '*[^a-zA-Z0-9:._/-]*'
    ),
    nonce TEXT NOT NULL,
    revoked_at TEXT NOT NULL,
    UNIQUE (issuer_id, nonce)
);"###;
const FULL_SQL_69: &str = r###"CREATE TABLE IF NOT EXISTS current_truth_assertions (
            assertion_id TEXT PRIMARY KEY,
            subject_repo TEXT NOT NULL,
            subject_kind TEXT NOT NULL,
            subject_id TEXT NOT NULL,
            predicate TEXT NOT NULL,
            value_json TEXT NOT NULL,
            issuer TEXT NOT NULL,
            authority TEXT NOT NULL,
            source_id TEXT NOT NULL,
            source_revision TEXT NOT NULL,
            observed_at TEXT NOT NULL,
            effective_at TEXT NOT NULL,
            supersedes TEXT,
            evidence_json TEXT NOT NULL DEFAULT '[]',
            review_state TEXT NOT NULL,
            visibility TEXT NOT NULL,
            content_digest TEXT NOT NULL,
            recorded_at TEXT NOT NULL DEFAULT '',
            UNIQUE (subject_repo, subject_kind, subject_id, predicate,
                    authority, issuer, source_id, source_revision)
        );"###;
const FULL_SQL_70: &str = r###"CREATE TABLE IF NOT EXISTS current_truth_projection (
            repo TEXT PRIMARY KEY,
            generation TEXT NOT NULL,
            built_at TEXT NOT NULL,
            view_json TEXT NOT NULL
        );"###;
const FULL_SQL_71: &str = r###"CREATE TABLE IF NOT EXISTS current_truth_refresh (
            repo TEXT NOT NULL,
            subject_token TEXT NOT NULL,
            fresh INTEGER NOT NULL CHECK (fresh IN (0, 1)),
            last_fresh_revision TEXT,
            last_fresh_at TEXT,
            last_attempt_at TEXT NOT NULL,
            unavailable_reason TEXT,
            repository_visibility TEXT CHECK (repository_visibility IN ('public', 'private')),
            repository_visibility_at TEXT,
            PRIMARY KEY (repo, subject_token)
        );"###;
const FULL_SQL_72: &str = r###"CREATE TABLE identity_admission_verification_receipts (
    receipt_id TEXT PRIMARY KEY,
    admission_id TEXT NOT NULL UNIQUE REFERENCES identity_admissions(admission_id),
    agent_identity_id TEXT NOT NULL REFERENCES agent_identities(agent_identity_id),
    connection_id TEXT NOT NULL,
    issuer_id TEXT NOT NULL,
    verification_method TEXT NOT NULL,
    verification_version TEXT NOT NULL,
    trust_domain TEXT NOT NULL,
    verification_scope TEXT NOT NULL,
    evidence_digest TEXT NOT NULL CHECK (
        length(evidence_digest) = 64
        AND evidence_digest NOT GLOB '*[^0-9a-f]*'
    ),
    evidence_ref TEXT NOT NULL CHECK (
        length(evidence_ref) BETWEEN 1 AND 256
        AND evidence_ref NOT GLOB '*[^a-zA-Z0-9:._/-]*'
    ),
    evidence_issued_at TEXT NOT NULL,
    evidence_expires_at TEXT NOT NULL,
    nonce TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL CHECK (
        length(request_digest) = 64
        AND request_digest NOT GLOB '*[^0-9a-f]*'
    ),
    current_state TEXT NOT NULL CHECK (current_state = 'verified'),
    verified_at TEXT NOT NULL,
    UNIQUE (issuer_id, idempotency_key),
    UNIQUE (issuer_id, trust_domain, nonce),
    FOREIGN KEY (admission_id, agent_identity_id, connection_id)
        REFERENCES identity_admissions(admission_id, agent_identity_id, connection_id)
) WITHOUT ROWID;"###;
const FULL_SQL_73: &str = r###"CREATE TABLE identity_admission_verification_revocations (
    revocation_id TEXT PRIMARY KEY,
    admission_id TEXT NOT NULL UNIQUE
        REFERENCES identity_admission_verification_receipts(admission_id),
    issuer_id TEXT NOT NULL,
    evidence_digest TEXT NOT NULL CHECK (
        length(evidence_digest) = 64
        AND evidence_digest NOT GLOB '*[^0-9a-f]*'
    ),
    evidence_ref TEXT NOT NULL CHECK (
        length(evidence_ref) BETWEEN 1 AND 256
        AND evidence_ref NOT GLOB '*[^a-zA-Z0-9:._/-]*'
    ),
    nonce TEXT NOT NULL,
    revoked_at TEXT NOT NULL,
    UNIQUE (issuer_id, nonce)
) WITHOUT ROWID;"###;
const FULL_SQL_74: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_hub_cap_type ON hub_capabilities(type);"###;
const FULL_SQL_75: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_hub_cap_name ON hub_capabilities(name);"###;
const FULL_SQL_76: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_hub_cap_enabled ON hub_capabilities(enabled);"###;
const FULL_SQL_77: &str = r###"CREATE INDEX IF NOT EXISTS idx_hub_route_target ON hub_version_routes(active_capability_id);"###;
const FULL_SQL_78: &str = r###"CREATE INDEX IF NOT EXISTS idx_vc_binding_capability
            ON virtual_capability_bindings(capability_id);"###;
const FULL_SQL_79: &str = r###"CREATE INDEX IF NOT EXISTS idx_vc_binding_priority
            ON virtual_capability_bindings(vc_id, priority ASC, capability_id ASC);"###;
const FULL_SQL_80: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp DESC);"###;
const FULL_SQL_81: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_audit_server ON audit_log(server_id);"###;
const FULL_SQL_82: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_audit_created_at ON audit_log(created_at DESC);"###;
const FULL_SQL_83: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_llm_usage_timestamp ON llm_usage(timestamp DESC);"###;
const FULL_SQL_84: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_llm_usage_lane ON llm_usage(lane, timestamp DESC);"###;
const FULL_SQL_85: &str = r###"CREATE INDEX IF NOT EXISTS idx_llm_usage_provider_key ON llm_usage(provider_logical_name, provider_key_id, timestamp DESC);"###;
const FULL_SQL_86: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_agent_known_agent ON agent_known_state(agent_id);"###;
const FULL_SQL_87: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_agent_known_memory ON agent_known_state(memory_id);"###;
const FULL_SQL_88: &str = r###"CREATE INDEX IF NOT EXISTS idx_agent_known_synced_at ON agent_known_state(synced_at DESC);"###;
const FULL_SQL_89: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_sandbox_role ON sandbox_rules(agent_role);"###;
const FULL_SQL_90: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_sandbox_policy_enabled ON sandbox_policies(enabled);"###;
const FULL_SQL_91: &str = r###"CREATE INDEX IF NOT EXISTS idx_sandbox_exec_timestamp ON sandbox_exec_audit(timestamp DESC);"###;
const FULL_SQL_92: &str = r###"CREATE INDEX IF NOT EXISTS idx_sandbox_exec_capability ON sandbox_exec_audit(capability_id);"###;
const FULL_SQL_93: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_vault_entries_type ON vault_entries(secret_type);"###;
const FULL_SQL_94: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_vault_audit_timestamp ON vault_audit(timestamp DESC);"###;
const FULL_SQL_95: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_vault_audit_operation ON vault_audit(operation);"###;
const FULL_SQL_96: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_vault_audit_secret_name ON vault_audit(secret_name);"###;
const FULL_SQL_97: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_vault_key_health_status ON vault_key_health(status);"###;
const FULL_SQL_98: &str = r###"CREATE INDEX IF NOT EXISTS idx_vault_key_health_logical ON vault_key_health(logical_name);"###;
const FULL_SQL_99: &str = r###"CREATE INDEX IF NOT EXISTS idx_vault_key_health_cooldown ON vault_key_health(logical_name, cooldown_until);"###;
const FULL_SQL_100: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_foundry_jobs_status ON foundry_jobs(status);"###;
const FULL_SQL_101: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_foundry_jobs_kind ON foundry_jobs(kind);"###;
const FULL_SQL_102: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_exec_envs_state ON exec_envs(state);"###;
const FULL_SQL_103: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_exec_envs_path ON exec_envs(path);"###;
const FULL_SQL_104: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_exec_envs_dispatch ON exec_envs(dispatch_id);"###;
const FULL_SQL_105: &str = r###"CREATE INDEX IF NOT EXISTS idx_hub_cap_review_status ON hub_capabilities(review_status);"###;
const FULL_SQL_106: &str = r###"CREATE INDEX IF NOT EXISTS idx_hub_cap_health_status ON hub_capabilities(health_status);"###;
const FULL_SQL_107: &str = r###"CREATE INDEX IF NOT EXISTS idx_memories_path_active_ts ON memories(path, timestamp DESC) WHERE archived = 0 AND superseded_by IS NULL;"###;
const FULL_SQL_108: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_session_claims_state ON session_claims(state);"###;
const FULL_SQL_109: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_session_claims_issue ON session_claims(issue_ref);"###;
const FULL_SQL_110: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_session_claims_flow ON session_claims(flow_id);"###;
const FULL_SQL_111: &str = r###"CREATE INDEX IF NOT EXISTS idx_dispatch_outcomes_vendor_ts
            ON dispatch_outcomes(vendor, created_at);"###;
const FULL_SQL_112: &str = r###"CREATE INDEX IF NOT EXISTS idx_dispatch_outcomes_issue_ref
            ON dispatch_outcomes(issue_ref);"###;
const FULL_SQL_113: &str = r###"CREATE INDEX IF NOT EXISTS idx_dispatch_outcomes_dispatch_id
            ON dispatch_outcomes(dispatch_id);"###;
const FULL_SQL_114: &str = r###"CREATE UNIQUE INDEX IF NOT EXISTS idx_session_claims_identity_active
            ON session_claims(COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id, ''))
            WHERE state = 'active';"###;
const FULL_SQL_115: &str = r###"CREATE INDEX IF NOT EXISTS idx_exec_env_resources_state
            ON exec_env_resources(state);"###;
const FULL_SQL_116: &str = r###"CREATE INDEX IF NOT EXISTS idx_exec_env_resources_kind
            ON exec_env_resources(kind);"###;
const FULL_SQL_117: &str = r###"CREATE INDEX IF NOT EXISTS idx_exec_env_resource_bindings_env
            ON exec_env_resource_bindings(env_id);"###;
const FULL_SQL_118: &str = r###"CREATE INDEX IF NOT EXISTS idx_exec_env_resource_bindings_resource
            ON exec_env_resource_bindings(resource_id);"###;
const FULL_SQL_119: &str = r###"CREATE INDEX IF NOT EXISTS idx_dispatch_adjudications_outcome
            ON dispatch_adjudications(outcome_id, created_at);"###;
const FULL_SQL_120: &str = r###"CREATE INDEX IF NOT EXISTS idx_dispatch_adjudication_signatures_signature
            ON dispatch_adjudication_signatures(signature_id);"###;
const FULL_SQL_121: &str = r###"CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_idless_identity_active
           ON memories(idless_identity)
           WHERE idless_identity IS NOT NULL AND archived = 0 AND superseded_by IS NULL;"###;
const FULL_SQL_122: &str = r###"CREATE INDEX IF NOT EXISTS idx_mirror_eval_runs_native_child
            ON mirror_eval_runs(native_child_id, created_at);"###;
const FULL_SQL_123: &str = r###"CREATE INDEX IF NOT EXISTS idx_mirror_eval_adjudications_run
            ON mirror_eval_adjudications(eval_run_id, created_at);"###;
const FULL_SQL_124: &str = r###"CREATE INDEX IF NOT EXISTS idx_provider_accounts_kind ON provider_accounts(provider_kind);"###;
const FULL_SQL_125: &str = r###"CREATE INDEX IF NOT EXISTS idx_provider_accounts_fingerprint ON provider_accounts(account_fingerprint);"###;
const FULL_SQL_126: &str = r###"CREATE INDEX IF NOT EXISTS idx_provider_account_aliases_name ON provider_account_aliases(alias_name);"###;
const FULL_SQL_127: &str = r###"CREATE INDEX IF NOT EXISTS idx_provider_account_events_account ON provider_account_events(account_id, id);"###;
const FULL_SQL_128: &str = r###"CREATE INDEX IF NOT EXISTS idx_route_recommendations_recorded_at
            ON route_recommendations(recorded_at);"###;
const FULL_SQL_129: &str = r###"CREATE INDEX IF NOT EXISTS idx_route_decisions_recommendation
            ON route_decisions(recommendation_id);"###;
const FULL_SQL_130: &str = r###"CREATE INDEX IF NOT EXISTS idx_eval_rubric_scores_adjudication
            ON eval_rubric_scores(subject_kind, adjudication_id);"###;
const FULL_SQL_131: &str = r###"CREATE INDEX IF NOT EXISTS idx_identity_admissions_connection ON identity_admissions(connection_id);"###;
const FULL_SQL_132: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_exec_envs_claim ON exec_envs(claim_id);"###;
const FULL_SQL_133: &str = r###"CREATE UNIQUE INDEX IF NOT EXISTS idx_session_claims_identity_active
            ON session_claims(COALESCE(session_client, ''), COALESCE(issue_ref, ''), COALESCE(flow_id, ''))
            WHERE state = 'active' AND mode IS NULL;"###;
const FULL_SQL_134: &str = r###"DROP TRIGGER IF EXISTS memories_reserved_refs_insert_guard;"###;
const FULL_SQL_135: &str = r###"DROP TRIGGER IF EXISTS memories_reserved_refs_update_guard;"###;
const FULL_SQL_136: &str = r###"CREATE TRIGGER memories_reserved_refs_insert_guard
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
        END;"###;
const FULL_SQL_137: &str = r###"CREATE TRIGGER memories_reserved_refs_update_guard
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
        END;"###;
const FULL_SQL_138: &str = r###"CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_created
            ON recall_impression_groups(created_at DESC, group_id);"###;
const FULL_SQL_139: &str = r###"CREATE INDEX IF NOT EXISTS idx_recall_impression_groups_fingerprint
            ON recall_impression_groups(query_fingerprint, created_at DESC)
            WHERE query_fingerprint IS NOT NULL;"###;
const FULL_SQL_140: &str = r###"CREATE INDEX IF NOT EXISTS idx_recall_impressions_memory
            ON recall_impressions(memory_id, group_id);"###;
const FULL_SQL_141: &str = r###"CREATE INDEX IF NOT EXISTS idx_recall_impressions_group_final_rank
            ON recall_impressions(group_id, final_rank, memory_id);"###;
const FULL_SQL_142: &str = r###"CREATE INDEX IF NOT EXISTS idx_rem_source_claims_draft
            ON rem_source_claims(draft_id);"###;
const FULL_SQL_143: &str = r###"CREATE INDEX IF NOT EXISTS idx_memory_outbox_events_state_created
            ON memory_outbox_events(state, created_at);"###;
const FULL_SQL_144: &str = r###"CREATE INDEX IF NOT EXISTS idx_memory_outbox_events_state_changed
            ON memory_outbox_events(state, state_changed_at);"###;
const FULL_SQL_145: &str = r###"CREATE INDEX IF NOT EXISTS idx_memory_outbox_events_object
            ON memory_outbox_events(object_id, created_at);"###;
const FULL_SQL_146: &str = r###"CREATE INDEX IF NOT EXISTS idx_memory_outbox_destination_apply_object
            ON memory_outbox_destination_apply_receipts(object_id, event_id);"###;
const FULL_SQL_147: &str =
    r###"INSERT OR IGNORE INTO memory_search_generation (id, generation) VALUES (1, 0);"###;
const FULL_SQL_148: &str = r###"CREATE TRIGGER IF NOT EXISTS memory_search_generation_after_insert
    AFTER INSERT ON memories
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;"###;
const FULL_SQL_149: &str = r###"CREATE TRIGGER IF NOT EXISTS memory_search_generation_after_update
    AFTER UPDATE ON memories
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;"###;
const FULL_SQL_150: &str = r###"CREATE TRIGGER IF NOT EXISTS memory_search_generation_after_delete
    AFTER DELETE ON memories
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;"###;
const FULL_SQL_151: &str = r###"CREATE TRIGGER IF NOT EXISTS memory_edge_search_generation_after_insert
    AFTER INSERT ON memory_edges
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;"###;
const FULL_SQL_152: &str = r###"CREATE TRIGGER IF NOT EXISTS memory_edge_search_generation_after_update
    AFTER UPDATE ON memory_edges
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;"###;
const FULL_SQL_153: &str = r###"CREATE TRIGGER IF NOT EXISTS memory_edge_search_generation_after_delete
    AFTER DELETE ON memory_edges
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;"###;
const FULL_SQL_154: &str = r###"CREATE TRIGGER IF NOT EXISTS memory_access_search_generation_after_insert
    AFTER INSERT ON access_history
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;"###;
const FULL_SQL_155: &str = r###"CREATE TRIGGER IF NOT EXISTS memory_access_search_generation_after_update
    AFTER UPDATE ON access_history
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;"###;
const FULL_SQL_156: &str = r###"CREATE TRIGGER IF NOT EXISTS memory_access_search_generation_after_delete
    AFTER DELETE ON access_history
    BEGIN
        SELECT CASE
            WHEN (SELECT COUNT(*) FROM memory_search_generation WHERE id = 1) != 1
                THEN RAISE(ABORT, 'memory search generation row missing')
            WHEN (SELECT generation FROM memory_search_generation WHERE id = 1) >= 9223372036854775807
                THEN RAISE(ABORT, 'memory search generation exhausted')
        END;
        UPDATE memory_search_generation SET generation = generation + 1 WHERE id = 1;
    END;"###;
const FULL_SQL_157: &str = r###"CREATE INDEX IF NOT EXISTS idx_a2a_envelopes_recipient_state
            ON a2a_envelopes(recipient_agent_identity_id, current_state, expires_at, created_at, envelope_id);"###;
const FULL_SQL_158: &str = r###"CREATE INDEX IF NOT EXISTS idx_a2a_envelopes_issuer_created
            ON a2a_envelopes(issuer_agent_identity_id, created_at DESC, envelope_id DESC);"###;
const FULL_SQL_159: &str = r###"CREATE INDEX IF NOT EXISTS idx_a2a_receipts_envelope_version
            ON a2a_delivery_receipts(envelope_id, envelope_version);"###;
const FULL_SQL_160: &str = r###"CREATE INDEX IF NOT EXISTS idx_model_deployments_account ON model_deployments(provider_account_id);"###;
const FULL_SQL_161: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_model_deployments_status ON model_deployments(status);"###;
const FULL_SQL_162: &str = r###"CREATE INDEX IF NOT EXISTS idx_model_deployment_events_deployment ON model_deployment_events(deployment_id, id);"###;
const FULL_SQL_163: &str =
    r###"CREATE INDEX IF NOT EXISTS idx_model_aliases_status ON model_aliases(status);"###;
const FULL_SQL_164: &str = r###"CREATE INDEX IF NOT EXISTS idx_model_alias_bindings_deployment ON model_alias_bindings(deployment_id);"###;
const FULL_SQL_165: &str = r###"CREATE INDEX IF NOT EXISTS idx_model_alias_events_alias ON model_alias_events(alias_name, id);"###;
const FULL_SQL_166: &str = r###"CREATE INDEX IF NOT EXISTS idx_pricing_snapshots_provider_kind ON pricing_snapshots(provider_kind);"###;
const FULL_SQL_167: &str = r###"CREATE INDEX IF NOT EXISTS idx_model_deployment_health_state ON model_deployment_health(state);"###;
const FULL_SQL_168: &str = r###"CREATE INDEX IF NOT EXISTS idx_model_deployment_health_cooldown ON model_deployment_health(cooldown_until);"###;
const FULL_SQL_169: &str = r###"CREATE INDEX idx_a2a_envelopes_recipient_state
            ON a2a_envelopes(recipient_agent_identity_id, current_state, expires_at, created_at, envelope_id);"###;
const FULL_SQL_170: &str = r###"CREATE INDEX idx_a2a_envelopes_issuer_created
            ON a2a_envelopes(issuer_agent_identity_id, created_at DESC, envelope_id DESC);"###;
const FULL_SQL_171: &str = r###"CREATE INDEX idx_a2a_receipts_envelope_version
            ON a2a_delivery_receipts(envelope_id, envelope_version);"###;
const FULL_SQL_172: &str = r###"CREATE INDEX IF NOT EXISTS idx_harness_session_attachments_claim
            ON harness_session_attachments(work_claim_id, expected_transition_version);"###;
const FULL_SQL_173: &str = r###"CREATE INDEX IF NOT EXISTS idx_harness_session_attachments_receipt
            ON harness_session_attachments(admission_receipt_ref, created_at);"###;
const FULL_SQL_174: &str = r###"CREATE INDEX IF NOT EXISTS idx_harness_session_events_revision
            ON harness_session_events(attachment_id, source_revision);"###;
const FULL_SQL_175: &str = r###"CREATE INDEX IF NOT EXISTS idx_harness_session_interventions_attachment
            ON harness_session_interventions(attachment_id, requested_at);"###;
const FULL_SQL_176: &str = r###"CREATE INDEX IF NOT EXISTS idx_delivery_intents_state_requester
            ON delivery_intents(delivery_state, requester_agent_identity_id);"###;
const FULL_SQL_177: &str = r###"CREATE INDEX IF NOT EXISTS idx_delivery_intents_execution
            ON delivery_intents(execution_source, execution_ref);"###;
const FULL_SQL_178: &str = r###"CREATE INDEX IF NOT EXISTS idx_delivery_events_delivery
            ON delivery_events(delivery_id, event_row_id);"###;
const FULL_SQL_179: &str = r###"CREATE UNIQUE INDEX IF NOT EXISTS idx_delivery_events_claim_key_global
            ON delivery_events(event_id) WHERE kind = 'claimed';"###;
const FULL_SQL_180: &str = r###"CREATE UNIQUE INDEX IF NOT EXISTS idx_delivery_events_ack_key_global
            ON delivery_events(event_id) WHERE kind = 'delivered';"###;
const FULL_SQL_181: &str = r###"CREATE UNIQUE INDEX idx_identity_admissions_verified_binding
ON identity_admissions(admission_id, agent_identity_id, connection_id);"###;
const FULL_SQL_182: &str = r###"CREATE INDEX idx_identity_verification_receipts_identity
ON identity_admission_verification_receipts(agent_identity_id, trust_domain, verification_scope);"###;
const FULL_SQL_183: &str = r###"CREATE INDEX idx_identity_verification_revocations_admission
ON identity_admission_verification_revocations(admission_id, revoked_at);"###;
const FULL_SQL_184: &str = r###"CREATE TRIGGER identity_verification_receipts_no_replace
BEFORE INSERT ON identity_admission_verification_receipts
WHEN EXISTS (
    SELECT 1 FROM identity_admission_verification_receipts
    WHERE receipt_id = NEW.receipt_id
       OR admission_id = NEW.admission_id
       OR (issuer_id = NEW.issuer_id AND idempotency_key = NEW.idempotency_key)
       OR (issuer_id = NEW.issuer_id AND trust_domain = NEW.trust_domain AND nonce = NEW.nonce)
)
BEGIN
    SELECT RAISE(ABORT, 'verified admission receipts are append-only');
END;"###;
const FULL_SQL_185: &str = r###"CREATE TRIGGER identity_verification_receipts_no_update
BEFORE UPDATE ON identity_admission_verification_receipts
BEGIN
    SELECT RAISE(ABORT, 'verified admission receipts are append-only');
END;"###;
const FULL_SQL_186: &str = r###"CREATE TRIGGER identity_verification_receipts_no_delete
BEFORE DELETE ON identity_admission_verification_receipts
BEGIN
    SELECT RAISE(ABORT, 'verified admission receipts are append-only');
END;"###;
const FULL_SQL_187: &str = r###"CREATE TRIGGER identity_verified_admissions_no_delete
BEFORE DELETE ON identity_admissions
WHEN OLD.state = 'verified'
BEGIN
    SELECT RAISE(ABORT, 'verified identity admissions are append-only');
END;"###;
const FULL_SQL_188: &str = r###"CREATE TRIGGER identity_verification_revocations_no_replace
BEFORE INSERT ON identity_admission_verification_revocations
WHEN EXISTS (
    SELECT 1 FROM identity_admission_verification_revocations
    WHERE revocation_id = NEW.revocation_id
       OR admission_id = NEW.admission_id
       OR (issuer_id = NEW.issuer_id AND nonce = NEW.nonce)
)
BEGIN
    SELECT RAISE(ABORT, 'verified admission revocations are append-only');
END;"###;
const FULL_SQL_189: &str = r###"CREATE TRIGGER identity_verification_revocations_no_update
BEFORE UPDATE ON identity_admission_verification_revocations
BEGIN
    SELECT RAISE(ABORT, 'verified admission revocations are append-only');
END;"###;
const FULL_SQL_190: &str = r###"CREATE TRIGGER identity_verification_revocations_no_delete
BEFORE DELETE ON identity_admission_verification_revocations
BEGIN
    SELECT RAISE(ABORT, 'verified admission revocations are append-only');
END;"###;
const FULL_SQL_191: &str = r###"CREATE TRIGGER identity_verified_admissions_no_replace
BEFORE INSERT ON identity_admissions
WHEN EXISTS (
    SELECT 1 FROM identity_admissions
    WHERE admission_id = NEW.admission_id AND state = 'verified'
)
BEGIN
    SELECT RAISE(ABORT, 'verified identity admissions are append-only');
END;"###;
const FULL_SQL_192: &str = r###"CREATE TRIGGER identity_verified_admissions_no_update
BEFORE UPDATE ON identity_admissions
WHEN OLD.state = 'verified'
BEGIN
    SELECT RAISE(ABORT, 'verified identity admissions are append-only');
END;"###;
const FULL_SQL_193: &str = r###"CREATE INDEX IF NOT EXISTS idx_ct_assertions_subject
            ON current_truth_assertions(subject_repo, subject_kind, subject_id);"###;
const FULL_SQL_194: &str = r###"CREATE INDEX IF NOT EXISTS idx_ct_assertions_predicate
            ON current_truth_assertions(predicate);"###;
const FULL_SQL_195: &str = r###"CREATE TRIGGER identity_verified_admissions_no_replace
BEFORE INSERT ON identity_admissions
WHEN EXISTS (
    SELECT 1 FROM identity_admissions
    WHERE state = 'verified'
      AND (
          rowid = NEW.rowid
          OR admission_id = NEW.admission_id
          OR (
              NEW.agent_identity_id IS NOT NULL
              AND agent_identity_id = NEW.agent_identity_id
              AND connection_id = NEW.connection_id
          )
      )
)
BEGIN
    SELECT RAISE(ABORT, 'verified identity admissions are append-only');
END;"###;
const FULL_SQL_196: &str = r###"CREATE TRIGGER identity_verified_admissions_no_update
BEFORE UPDATE ON identity_admissions
WHEN OLD.state = 'verified'
  OR NEW.state = 'verified'
  OR EXISTS (
      SELECT 1 FROM identity_admissions
      WHERE state = 'verified'
        AND rowid != OLD.rowid
        AND (
            rowid = NEW.rowid
            OR admission_id = NEW.admission_id
            OR (
                NEW.agent_identity_id IS NOT NULL
                AND agent_identity_id = NEW.agent_identity_id
                AND connection_id = NEW.connection_id
            )
        )
  )
BEGIN
    SELECT RAISE(ABORT, 'verified identity admissions are append-only');
END;"###;
const FULL_SQL_197: &str = r###"CREATE TABLE memories_new (
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
            last_access  TEXT,
            revision     INTEGER NOT NULL DEFAULT 1,
            metadata     TEXT NOT NULL DEFAULT '{}',
             retention_policy TEXT,
             domain       TEXT,
             superseded_by TEXT,
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
             created_at, updated_at, access_count, last_access, revision,
             metadata, retention_policy, domain, superseded_by,
             recall_count, query_diversity, tier)
        SELECT
             id, path, summary, text, importance, timestamp,
             COALESCE(NULLIF(valid_from, ''), timestamp), NULLIF(valid_until, ''),
             category, topic, keywords, entities, source, scope, archived,
             created_at, updated_at, access_count, last_access, revision,
             metadata, retention_policy, domain, superseded_by,
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
        CREATE INDEX IF NOT EXISTS idx_memories_path_active_ts ON memories(path, timestamp DESC) WHERE archived = 0 AND superseded_by IS NULL;"###;
const FULL_SQL_198: &str = r###"CREATE TABLE memories_new (
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
            last_access  TEXT,
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
             created_at, updated_at, access_count, last_access, revision,
             metadata, retention_policy, domain, superseded_by, idless_identity,
             recall_count, query_diversity, tier)
        SELECT
             id, path, summary, text, importance, timestamp,
             COALESCE(NULLIF(valid_from, ''), timestamp), NULLIF(valid_until, ''),
             category, topic, keywords, entities, source, scope, archived,
             created_at, updated_at, access_count, last_access, revision,
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
            WHERE idless_identity IS NOT NULL AND archived = 0 AND superseded_by IS NULL;"###;
