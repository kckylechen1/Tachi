//! Per-DB Foundry runtime configuration.
//!
//! Each Tachi DB carries a single `foundry_config` row (PK=1) that governs how
//! the multi-DB Foundry scheduler treats it: whether to spawn a worker at all,
//! how many jobs/minute it may run, and per-lane concurrency caps. Missing rows
//! return `PerDbConfig::default()` so brand-new DBs join the scheduler with sane
//! defaults without an explicit upsert.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::MemoryError;

/// Runtime knobs the Foundry scheduler honors per DB. See module doc for
/// missing-row semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerDbConfig {
    pub enabled: bool,
    pub max_jobs_per_minute: u32,
    pub distill_concurrency: u32,
    pub enrichment_concurrency: u32,
    pub llm_provider_override: Option<String>,
    pub updated_at: String,
    pub updated_by: String,
}

impl Default for PerDbConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_jobs_per_minute: 10,
            distill_concurrency: 1,
            enrichment_concurrency: 1,
            llm_provider_override: None,
            updated_at: String::new(),
            updated_by: "default".to_string(),
        }
    }
}

/// Read the foundry_config row, or return defaults when absent.
pub fn get_foundry_config(conn: &Connection) -> Result<PerDbConfig, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT enabled, max_jobs_per_minute, distill_concurrency,
                enrichment_concurrency, llm_provider_override, updated_at, updated_by
         FROM foundry_config WHERE id = 1",
    )?;
    let row = stmt.query_row([], |row| {
        Ok(PerDbConfig {
            enabled: row.get::<_, i64>(0)? != 0,
            max_jobs_per_minute: row.get::<_, i64>(1)?.max(0) as u32,
            distill_concurrency: row.get::<_, i64>(2)?.max(0) as u32,
            enrichment_concurrency: row.get::<_, i64>(3)?.max(0) as u32,
            llm_provider_override: row.get(4)?,
            updated_at: row.get(5)?,
            updated_by: row.get(6)?,
        })
    });
    match row {
        Ok(cfg) => Ok(cfg),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(PerDbConfig::default()),
        Err(e) => Err(MemoryError::from(e)),
    }
}

/// Upsert the singleton foundry_config row.
pub fn set_foundry_config(
    conn: &Connection,
    cfg: &PerDbConfig,
    updated_by: &str,
) -> Result<(), MemoryError> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO foundry_config
            (id, enabled, max_jobs_per_minute, distill_concurrency,
             enrichment_concurrency, llm_provider_override, updated_at, updated_by)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            enabled = excluded.enabled,
            max_jobs_per_minute = excluded.max_jobs_per_minute,
            distill_concurrency = excluded.distill_concurrency,
            enrichment_concurrency = excluded.enrichment_concurrency,
            llm_provider_override = excluded.llm_provider_override,
            updated_at = excluded.updated_at,
            updated_by = excluded.updated_by",
        params![
            cfg.enabled as i64,
            cfg.max_jobs_per_minute as i64,
            cfg.distill_concurrency as i64,
            cfg.enrichment_concurrency as i64,
            cfg.llm_provider_override,
            now,
            updated_by,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_schema;

    fn open_test_db() -> Connection {
        let _ = libsimple::enable_auto_extension();
        crate::db::sqlite_vec::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        crate::db::sqlite_vec::try_load_sqlite_vec(&conn);
        conn
    }

    #[test]
    fn get_returns_defaults_when_no_row() {
        let conn = open_test_db();
        let cfg = get_foundry_config(&conn).unwrap();
        assert_eq!(cfg, PerDbConfig::default());
        assert!(cfg.enabled);
        assert_eq!(cfg.max_jobs_per_minute, 10);
    }

    #[test]
    fn set_then_get_round_trips() {
        let conn = open_test_db();
        let cfg = PerDbConfig {
            enabled: false,
            max_jobs_per_minute: 5,
            distill_concurrency: 2,
            enrichment_concurrency: 3,
            llm_provider_override: Some("ollama".to_string()),
            updated_at: String::new(),
            updated_by: String::new(),
        };
        set_foundry_config(&conn, &cfg, "test-user").unwrap();

        let read = get_foundry_config(&conn).unwrap();
        assert!(!read.enabled);
        assert_eq!(read.max_jobs_per_minute, 5);
        assert_eq!(read.distill_concurrency, 2);
        assert_eq!(read.enrichment_concurrency, 3);
        assert_eq!(read.llm_provider_override.as_deref(), Some("ollama"));
        assert_eq!(read.updated_by, "test-user");
        assert!(!read.updated_at.is_empty(), "updated_at should be set");
    }

    #[test]
    fn set_twice_updates_in_place() {
        let conn = open_test_db();
        set_foundry_config(&conn, &PerDbConfig::default(), "first").unwrap();
        let cfg = PerDbConfig {
            max_jobs_per_minute: 99,
            ..Default::default()
        };
        set_foundry_config(&conn, &cfg, "second").unwrap();

        let read = get_foundry_config(&conn).unwrap();
        assert_eq!(read.max_jobs_per_minute, 99);
        assert_eq!(read.updated_by, "second");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM foundry_config", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "PK CHECK constraint must enforce singleton row");
    }

    #[test]
    fn pk_check_rejects_non_one_id() {
        let conn = open_test_db();
        let res = conn.execute(
            "INSERT INTO foundry_config (id, enabled, max_jobs_per_minute,
                distill_concurrency, enrichment_concurrency, updated_at, updated_by)
             VALUES (2, 1, 10, 1, 1, '', 'x')",
            [],
        );
        assert!(res.is_err(), "id != 1 must be rejected by CHECK constraint");
    }
}
