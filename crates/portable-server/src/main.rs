//! Portable memory-kernel MCP server (tachi #924).
//!
//! A stripped server profile: the memory kernel (`portable-kernel` = memcore
//! with `admin` off) served over MCP, with the save/search/get/status
//! keep-set and nothing else. No dependency on `tachi-server`, so no operator
//! or product surface can be compiled in.
//!
//! Boot: `portable-server [--global-db <path>] [--project-db <path>] [--decay-policy <name>]
//! [--daemon] [--port <n>] [--allow-schema-migration]` (env fallbacks
//! `PORTABLE_MEMORY_DB` / `PORTABLE_DECAY_POLICY` / `PORTABLE_DAEMON` /
//! `PORTABLE_PORT` / `PORTABLE_ALLOW_SCHEMA_MIGRATION`). The last one is
//! #1119: this is a persistent-DB deploy entry point exactly like
//! `tachi-server serve`, so it carries the same typed schema-migration
//! opt-in — see `build_server`'s doc comment.
//!
//! Two transports over the same [`PortableServer`] tool surface (tachi
//! #938):
//! * default — MCP over stdin/stdout; point an MCP client at this binary as
//!   a stdio server.
//! * `--daemon` — MCP streamable-HTTP resident on `127.0.0.1:<port>`
//!   (default port `7919`; see `http.rs`), plus a `/health` probe.

mod config;
mod decay;
mod http;
mod malformed_json_middleware;
mod service;

use config::{Config, IN_MEMORY};
use portable_kernel::{DbOpenContext, MemoryStore, MigrationAuthority, OpenIntent, StoreProfile};
use service::PortableServer;

fn build_server(config: Config) -> Result<PortableServer, String> {
    // #1119: persistent-path opens carry an explicit migration authority
    // instead of the fail-closed default — this daemon/stdio boot is the same
    // shape of deploy entry point as `tachi-server serve`, which has its own
    // `--allow-schema-migration` → typed `MigrationAuthority` translation.
    // Without this, upgrading portable-server and pointing it at a legitimate
    // older-schema persistent DB would refuse to boot with no opt-in route.
    let migration = if config.allow_schema_migration {
        MigrationAuthority::Allow {
            approved_by: "portable-server --allow-schema-migration".to_string(),
        }
    } else {
        MigrationAuthority::Deny
    };

    fn open_store(path: &str, migration: &MigrationAuthority) -> Result<MemoryStore, String> {
        if path == IN_MEMORY {
            // Always a fresh store — never gated, not affected by migration
            // authority.
            MemoryStore::open_in_memory().map_err(|e| format!("open in-memory store: {e}"))
        } else {
            let ctx = DbOpenContext {
                intent: OpenIntent::OpenExisting,
                migration: migration.clone(),
                // #1585 D2: this binary IS the portable kernel — save/search/
                // get/status and nothing else — so it requires only the
                // portable profile. It therefore also refuses an *unstamped*
                // pre-#1585 database rather than adopting one: adopting would
                // mean a portable binary silently claiming authority over a
                // full Tachi store. See `StoreProfileUnstamped`'s remediation
                // text for the operator route.
                required_profile: StoreProfile::PortableKernel,
            };
            MemoryStore::open_with_context(path, &ctx)
                .map_err(|e| format!("open store at {path}: {e}"))
        }
    }

    let store = open_store(&config.db_path, &migration)?;
    let project_stores = config
        .project_db_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let name = if index == 0 {
                "project".to_string()
            } else {
                format!("project-{index}")
            };
            open_store(path, &migration).map(|store| (name, path.clone(), store))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PortableServer::new(
        store,
        project_stores,
        config.decay_policy,
        config.decay_policy_name,
        config.db_path,
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_args_and_env().map_err(|e| {
        eprintln!("portable-server: {e}");
        e
    })?;

    let daemon = config.daemon;
    let port = config.port;

    if daemon {
        eprintln!(
            "[portable-server] db={} decay_policy={} — serving memory kernel (save/search/get/status) over MCP streamable-HTTP on 127.0.0.1:{port}",
            config.db_path, config.decay_policy_name
        );
        let server = build_server(config)?;
        // Fail loud (#936 lesson): any exit from `serve_http` other than a
        // commanded SIGINT/SIGTERM shutdown is a bug, not something to idle
        // through — log it and exit non-zero instead of lingering deaf.
        if let Err(e) = http::serve_http(server, port).await {
            eprintln!("[portable-server] ERROR: daemon exited: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    eprintln!(
        "[portable-server] db={} decay_policy={} — serving memory kernel (save/search/get/status) over MCP stdio",
        config.db_path, config.decay_policy_name
    );

    let server = build_server(config)?;

    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let running = rmcp::service::serve_server(server, transport).await?;

    tokio::select! {
        reason = running.waiting() => {
            eprintln!("[portable-server] stopped: {reason:?}");
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("[portable-server] SIGINT — shutting down");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::StatusParams;
    use rmcp::handler::server::wrapper::Parameters;

    #[tokio::test]
    async fn build_server_attaches_configured_project_store() {
        let config = Config {
            db_path: IN_MEMORY.to_string(),
            project_db_paths: vec![IN_MEMORY.to_string()],
            decay_policy_name: "default".to_string(),
            decay_policy: None,
            daemon: false,
            port: 7919,
            allow_schema_migration: false,
        };
        let server = build_server(config).expect("build server");
        let status = server
            .status(Parameters(StatusParams {}))
            .await
            .expect("status");
        let status: serde_json::Value = serde_json::from_str(&status).expect("status json");

        assert_eq!(status["databases"]["global"]["path"], IN_MEMORY);
        assert_eq!(status["databases"]["project"]["path"], IN_MEMORY);
    }

    // --- #1119: --allow-schema-migration wiring for persistent-path opens --

    /// Fabricate a persistent (on-disk) DB file stamped at an older schema
    /// version — a raw `PRAGMA user_version` write, since `MemoryStore`
    /// exposes no public setter (by design: the stamp only ever advances
    /// through a real migration run). No tables are created; `init_schema`'s
    /// idempotent `CREATE TABLE IF NOT EXISTS` DDL builds them when the
    /// authorized migration actually runs, exactly as it would for a real
    /// legacy file.
    fn fabricate_stamped_older_db() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("portable-server-1119-{}.db", uuid::Uuid::new_v4()));
        let conn = rusqlite::Connection::open(&path).expect("create raw sqlite file");
        let older = portable_kernel::db::migrations::EXPECTED_SCHEMA_VERSION - 1;
        conn.execute_batch(&format!("PRAGMA user_version = {older}"))
            .expect("stamp older version");
        drop(conn);
        path
    }

    /// The same stamped-older file, plus the one thing #1585 requires before a
    /// portable binary may touch an existing database: a
    /// `store_identity/profile` stamp reading `portable_kernel`.
    ///
    /// The stamp lives in `hard_state`, which a raw pre-schema file does not
    /// have yet, so the fixture creates that one table verbatim from the
    /// kernel's own portable DDL chunk and inserts the row in the same shape
    /// `db::store_identity::write_stamp_if_absent` writes (a JSON object whose
    /// `value` field carries the token). Everything else — every other table,
    /// every migration — is still built by the authorized migration run under
    /// test, exactly as for a real legacy portable file.
    fn fabricate_stamped_older_portable_db() -> std::path::PathBuf {
        use portable_kernel::db::store_profile::{STORE_IDENTITY_NAMESPACE, STORE_PROFILE_KEY};

        let path = fabricate_stamped_older_db();
        let conn = rusqlite::Connection::open(&path).expect("reopen raw sqlite file");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS hard_state (
                 namespace        TEXT NOT NULL,
                 key              TEXT NOT NULL,
                 value_json       TEXT NOT NULL DEFAULT '{}',
                 version          INTEGER NOT NULL DEFAULT 1,
                 created_at       TEXT NOT NULL DEFAULT '',
                 updated_at       TEXT NOT NULL DEFAULT '',
                 PRIMARY KEY (namespace, key)
             );",
        )
        .expect("create hard_state to hold the profile stamp");
        let stamped_at = "2026-01-01T00:00:00Z";
        let value_json = serde_json::json!({
            "value": StoreProfile::PortableKernel.as_str(),
            "conferred_by": "test:portable-server-1585-fixture",
            "stamped_at": stamped_at,
        })
        .to_string();
        conn.execute(
            "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, ?4, ?4)",
            rusqlite::params![
                STORE_IDENTITY_NAMESPACE,
                STORE_PROFILE_KEY,
                value_json,
                stamped_at
            ],
        )
        .expect("stamp the portable_kernel profile");
        drop(conn);
        path
    }

    fn config_for_persistent_db(db_path: &std::path::Path, allow: bool) -> Config {
        Config {
            db_path: db_path.to_string_lossy().to_string(),
            project_db_paths: Vec::new(),
            decay_policy_name: "default".to_string(),
            decay_policy: None,
            daemon: false,
            port: 7919,
            allow_schema_migration: allow,
        }
    }

    /// Discriminating test 2: `--allow-schema-migration` (config field `true`)
    /// migrates a stamped-older persistent DB forward instead of refusing.
    ///
    /// The fixture carries a `portable_kernel` profile stamp because #1585's
    /// adoption asymmetry now decides admission *before* this binary is
    /// allowed to migrate anything: an UNSTAMPED existing DB is refused
    /// outright (pinned by
    /// `allow_schema_migration_true_still_refuses_an_unstamped_persistent_db`
    /// below), so it can no longer serve as the input that isolates the
    /// migration-authority variable. Holding the profile fixed at
    /// `portable_kernel` is what keeps this pair discriminating: it and its
    /// `false` twin below differ in exactly one thing, the flag.
    #[test]
    fn allow_schema_migration_true_migrates_stamped_older_persistent_db() {
        let path = fabricate_stamped_older_portable_db();
        let config = config_for_persistent_db(&path, true);

        let result = build_server(config);
        assert!(
            result.is_ok(),
            "Allow must migrate a stamped-older persistent DB, got: {:?}",
            result.err()
        );

        // Prove it actually migrated (not just "didn't error"): the on-disk
        // stamp now reads the current schema version.
        let conn = rusqlite::Connection::open(&path).expect("reopen db");
        let stored: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("read user_version");
        assert_eq!(
            stored as u32,
            portable_kernel::db::migrations::EXPECTED_SCHEMA_VERSION,
            "authorized migration must advance the stamp to the current version"
        );

        // #1585 D2's load-bearing rule: the migration walked the STORED
        // profile, so the store is still portable afterwards. A migration that
        // silently promoted it to `tachi_full` would be the reverse of the
        // damaging failure mode the profile stamp exists to prevent.
        let profile: String = conn
            .query_row(
                "SELECT value_json FROM hard_state WHERE namespace = ?1 AND key = ?2",
                rusqlite::params![
                    portable_kernel::db::store_profile::STORE_IDENTITY_NAMESPACE,
                    portable_kernel::db::store_profile::STORE_PROFILE_KEY
                ],
                |row| row.get(0),
            )
            .expect("profile stamp survives the migration");
        assert!(
            profile.contains(StoreProfile::PortableKernel.as_str()),
            "migrating must not re-shape the store's profile; stamp reads: {profile}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// #1585 D2's adoption asymmetry, from the portable side: migration
    /// authority is NOT profile authority. Even holding `Allow`, this binary
    /// refuses an existing database that carries no profile stamp — such a
    /// file predates #1585 and is therefore a full Tachi store, and adopting
    /// it would let a portable build claim authority over product data and
    /// then walk its migrations as if the product tables were absent.
    ///
    /// The refusal is typed (`StoreProfileUnstamped`) and arrives *before* any
    /// DDL runs, so the fixture's schema stamp must be untouched afterwards.
    #[test]
    fn allow_schema_migration_true_still_refuses_an_unstamped_persistent_db() {
        let path = fabricate_stamped_older_db();
        let older = portable_kernel::db::migrations::EXPECTED_SCHEMA_VERSION - 1;
        let config = config_for_persistent_db(&path, true);

        // Not `expect_err`: `PortableServer` is deliberately not `Debug` (same
        // reason as `allow_schema_migration_false_refuses_stamped_older_persistent_db`).
        let err = match build_server(config) {
            Ok(_) => panic!(
                "a portable-profile opener must refuse an unstamped existing DB even with \
                 --allow-schema-migration (tachi#1585 D2 adoption asymmetry)"
            ),
            Err(err) => err,
        };
        assert!(
            err.contains("store profile unstamped"),
            "expected the typed StoreProfileUnstamped refusal, got: {err}"
        );
        assert!(
            err.contains("portable-kernel build"),
            "the refusal must carry its operator remediation route, got: {err}"
        );

        let conn = rusqlite::Connection::open(&path).expect("reopen db");
        let stored: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("read user_version");
        assert_eq!(
            stored as u32, older,
            "a refused open must leave the schema stamp exactly as it found it"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Discriminating test 3: without the flag (default `Deny`, fail-closed),
    /// the same stamped-older persistent DB is refused with the typed
    /// `SchemaMigrationOptInRequired` error, not silently migrated.
    #[test]
    fn allow_schema_migration_false_refuses_stamped_older_persistent_db() {
        let path = fabricate_stamped_older_portable_db();
        let config = config_for_persistent_db(&path, false);

        // Not `expect_err`: that would require `PortableServer: Debug`, and the
        // server owns a store we deliberately don't want formatted into panics.
        let err = match build_server(config) {
            Ok(_) => panic!(
                "Deny (default) must refuse a stamped-older persistent DB instead of migrating it"
            ),
            Err(err) => err,
        };
        assert!(
            err.contains("refusing to migrate db schema"),
            "expected the typed SchemaMigrationOptInRequired refusal, got: {err}"
        );

        let _ = std::fs::remove_file(&path);
    }
}
