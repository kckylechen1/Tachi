//! Core CRUD, search, and diagnostics methods on [`MemoryStore`].

use rusqlite::Connection;
use std::time::{Duration, Instant};

use crate::{
    db,
    error::MemoryError,
    search::{
        hybrid_search, hybrid_search_with_receipt, SearchOptions, SearchPhaseReceipt,
        SearchReceiptDatabaseScope, SearchReceiptOperation,
    },
    types::{MemoryEntry, SearchResult, StatsResult},
    MemoryStore,
};

impl MemoryStore {
    /// Fill `options.recall_config`/`decay_policy` from this store's
    /// host-injected [`crate::KernelPolicy`] when the caller left them
    /// unset. A caller-supplied per-call override always wins (tachi#1585
    /// D5 point 6) — this only supplies the store-level default that the
    /// `SearchOptions` seam falls back to, in place of the process-wide
    /// `RecallConfig::get()`/`DEFAULT_DECAY_POLICY` fallbacks those seams
    /// used before a store existed to ask.
    fn apply_kernel_policy_defaults(&self, options: &mut SearchOptions) {
        if options.recall_config.is_none() {
            options.recall_config = Some(self.policy.recall.clone());
        }
        if options.decay_policy.is_none() {
            options.decay_policy = Some(self.policy.decay.clone());
        }
    }

    /// Hybrid search: Text + FTS5 + optional vector channel.
    pub fn search(
        &self,
        query: &str,
        opts: Option<SearchOptions>,
    ) -> Result<Vec<SearchResult>, MemoryError> {
        let mut options = opts.unwrap_or_default();
        options.vec_available = self.vec_available;
        // tachi#1569: this is the only layer where both the store and the
        // options exist, so it is where store identity enters the query. A
        // caller-supplied value is overwritten on purpose — the store, not the
        // request, is the authority on which database this is.
        options.wiki_corpus_store = self.is_wiki_corpus_store();
        self.apply_kernel_policy_defaults(&mut options);
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        retry_search_locked(&self.db_label, || {
            hybrid_search(&self.conn, query, &options)
        })
    }

    /// Instrumented search for benchmark/evaluation fixtures. Unlike the
    /// bare-connection helper, this preserves the store's actual vector
    /// capability and adds the manifest DB label without disclosing a path.
    pub fn search_with_receipt(
        &self,
        query: &str,
        opts: Option<SearchOptions>,
    ) -> Result<(Vec<SearchResult>, SearchPhaseReceipt), MemoryError> {
        let mut options = opts.unwrap_or_default();
        options.vec_available = self.vec_available;
        // Same store-identity injection as `search` (tachi#1569); the
        // instrumented path must not measure a differently-gated query.
        options.wiki_corpus_store = self.is_wiki_corpus_store();
        self.apply_kernel_policy_defaults(&mut options);
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let (results, mut receipt) = retry_search_locked(&self.db_label, || {
            hybrid_search_with_receipt(&self.conn, query, &options)
        })?;
        receipt.operation = SearchReceiptOperation::MemoryStoreSearch;
        receipt.database_scope = SearchReceiptDatabaseScope::Label(self.db_label.clone());
        Ok((results, receipt))
    }

    /// Fetch a single entry by ID.
    pub fn get(&self, id: &str) -> Result<Option<MemoryEntry>, MemoryError> {
        self.get_with_options(id, false)
    }

    /// Fetch a single entry by ID with archive visibility control.
    ///
    /// tachi#1569 deliberately leaves this route ungated, unlike the list
    /// family above. An id-addressed read has no `path_prefix`, so it has no
    /// shape to opt back in with (see
    /// [`crate::namespace::is_internal_only_row`]'s doc), and the store's own
    /// machinery addresses its bookkeeping rows by id through this exact
    /// wrapper — `foundry_runtime_ops::wiki_evolver`'s REM ledger validation
    /// calls `wiki_store.get_with_options("wiki-rem:…", true)` and treats a
    /// `None` as a hard "ledger missing" error. Gating here would break that
    /// on the Wiki store. The user-facing `get` surface stays filtered by
    /// `tachi-server`'s `readable_entry`, whose predicate
    /// (`is_internal_only_row`) is the exact negation of the wiki clause.
    pub fn get_with_options(
        &self,
        id: &str,
        include_archived: bool,
    ) -> Result<Option<MemoryEntry>, MemoryError> {
        let ids = vec![id.to_string()];
        let mut map = db::fetch_by_ids(&self.conn, &ids, include_archived)?;
        Ok(map.remove(id))
    }

    pub fn list_user_facing_wiki_entries(
        &self,
        path_prefix: &str,
        limit: usize,
        include_superseded: bool,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_user_facing_wiki_entries(&self.conn, path_prefix, limit, include_superseded)
    }

    /// Compatibility access for diagnostics and typed helpers that operate on
    /// non-memory tables. `rusqlite::Connection` is inherently write-capable
    /// even through `&Connection`, so a connection authorizer
    /// (`db::open::install_reserved_reference_authorizer`) sits on every store
    /// connection. Auxiliary-table DML and non-protected `memories` content
    /// columns remain available through this seam; fixed typed store
    /// operations arm a private scoped token for the rest.
    ///
    /// # What is denied, stated without euphemism
    ///
    /// * **Every schema mutation.** CREATE/DROP of table, index, view,
    ///   trigger and virtual table — including all TEMP variants — plus ALTER
    ///   TABLE, ANALYZE and REINDEX. Not "protected DDL": *all* DDL. The
    ///   authorizer's allowlist is two byte-exact internal shapes (the
    ///   canonical `memories_reserved_refs_*`/search-generation triggers under
    ///   an armed schema-migration token, and the `ingest_stable_owner_fence`
    ///   temp pair under an armed owner-fence token). No name, table or
    ///   temp-ness a caller can choose lands inside it.
    /// * Raw `memories` INSERTs and UPDATEs of authority-bearing columns.
    /// * `ATTACH`/`DETACH` and `PRAGMA writable_schema`.
    ///
    /// Denied statements fail at prepare time with SQLite's generic
    /// `not authorized`, which names neither the rule nor the alternative —
    /// hence this comment.
    ///
    /// # Injecting a mid-transaction store-write failure in a test
    ///
    /// Do **not** reach for `CREATE TEMP TRIGGER` through this seam. It is
    /// denied, so the fixture dies on `not authorized` before it ever reaches
    /// the code under test, and the test asserts nothing (tachi#1443 — four
    /// such fixtures in two PRs in one day).
    ///
    /// The sanctioned route is a **second, unguarded connection to the same
    /// database file**, which carries no authorizer. Install the failing
    /// trigger there, then drive the code under test through the store:
    ///
    /// * from `tachi-server`:
    ///   `crate::test_support::with_unrestricted_fixture_connection(path, op)`
    ///   (see `tests/skill_tests/builtin_ingest/ingest_source.rs`);
    /// * from `memcore`: `rusqlite::Connection::open(&path)` against the
    ///   store's own file (see `store/vault.rs`'s
    ///   `vault_replace_api_key_pool_rolls_back_when_rotation_write_fails`).
    ///
    /// Two constraints come with it, both fail-closed if ignored:
    ///
    /// 1. The store must be **file-backed**. An `open_in_memory()` store has
    ///    no path for a second connection to open, so a fixture that needs
    ///    injected failure must use a `tempfile`-backed store instead.
    /// 2. A temp trigger dies with the connection that created it, so the
    ///    injected trigger must be **persistent** — and a persistent
    ///    non-canonical trigger left behind makes the database refuse to open
    ///    (`db::open::validate_persistent_trigger_inventory`). Drop it before
    ///    anything reopens the file, or keep the file disposable.
    ///
    /// When no injected failure is needed at all, prefer a deterministic
    /// no-DDL failure (a missing row, a violated constraint): see
    /// `store/vault.rs`'s
    /// `vault_touch_entries_atomic_rolls_back_every_touch_when_one_name_is_missing`.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Compatibility access for typed helpers that need a transaction on
    /// non-memory tables (`Connection::transaction` requires `&mut`), notably
    /// exec-env/resource/claim and recall-proposal operations. It carries the
    /// same connection-level restrictions as [`Self::connection`] — including
    /// the blanket DDL denial and the fault-injection guidance documented
    /// there, which is what a test wanting to break one of these transactions
    /// needs to read first.
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Run a TRUNCATE WAL checkpoint to reclaim the `-wal` file.
    ///
    /// Default PASSIVE auto-checkpoints merge WAL frames into the DB but never
    /// shrink the `-wal` file, so a write burst (or long-lived readers blocking
    /// truncation) lets it balloon — observed as a 25 MB orphaned WAL on a busy
    /// agent DB. A periodic TRUNCATE checkpoint reclaims it when readers are
    /// quiet. Best-effort: returns Ok even if SQLite reports a busy checkpoint.
    pub fn checkpoint_wal_truncate(&self) -> Result<(), MemoryError> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(MemoryError::from)
    }

    /// Run `PRAGMA optimize` on the write connection.
    ///
    /// SQLite's query planner relies on `sqlite_stat1` (populated by
    /// `ANALYZE`) to choose between candidate indexes; on a long-lived,
    /// heavily-written DB that table goes stale as row-count/value
    /// distributions shift, and the planner can mis-pick an index that made
    /// sense at `ANALYZE` time but no longer reflects the data (observed:
    /// the planner favoring `idx_memories_archived` over a more selective
    /// index once `memories` grew past its original shape). `PRAGMA
    /// optimize` runs SQLite's own heuristic — a lightweight `ANALYZE` only
    /// on tables it judges likely to have stale statistics — so it's safe
    /// and cheap to call often; the project's own docs recommend it "run
    /// occasionally, or once before closing the database." Best-effort: a
    /// failure here should never fail whatever else the caller was doing.
    pub fn run_optimize(&self) -> Result<(), MemoryError> {
        let _authorization = db::authorize_planner_maintenance(&self.reserved_reference_write)?;
        self.conn
            .execute_batch("PRAGMA optimize;")
            .map_err(MemoryError::from)
    }

    /// Fetch multiple newest entries up to a limit (used for dedup).
    pub fn get_all(&self, limit: usize) -> Result<Vec<MemoryEntry>, MemoryError> {
        self.get_all_with_options(limit, false)
    }

    /// Fetch newest entries with archive visibility control.
    ///
    /// tachi#1569: like every list route on this store, the Wiki corpus's
    /// internal rows are excluded in SQL when this handle *is* the Wiki
    /// corpus — before the `limit` is applied, so bookkeeping rows can no
    /// longer starve the caller's budget and then be dropped downstream.
    pub fn get_all_with_options(
        &self,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::get_all(
            &self.conn,
            limit,
            include_archived,
            self.is_wiki_corpus_store(),
        )
    }

    /// Id of an active non-REM row with the EXACT `path` and `text`, via a
    /// direct SQL predicate (no recency-window cutoff to hide behind — see
    /// `db::find_exact_path_text_id`'s doc for the #1041 F6 bug this fixes).
    pub fn find_exact_path_text_id(
        &self,
        path: &str,
        text: &str,
    ) -> Result<Option<String>, MemoryError> {
        db::find_exact_path_text_id(&self.conn, path, text)
    }

    /// List entries under a path (exact + descendants) with SQL pushdown.
    pub fn list_by_path(
        &self,
        path_prefix: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_by_path(
            &self.conn,
            path_prefix,
            limit,
            include_archived,
            self.is_wiki_corpus_store(),
        )
    }

    /// List active, unsuperseded entries under a path (exact + descendants).
    pub fn list_by_path_active_unsuperseded(
        &self,
        path_prefix: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_by_path_active_unsuperseded(
            &self.conn,
            path_prefix,
            limit,
            self.is_wiki_corpus_store(),
        )
    }

    /// List entries under a path (exact + descendants), newest-first by
    /// `timestamp`. Use this instead of `list_by_path` when the caller wants
    /// a recency-first view and applies `limit` as a hard cutoff — see
    /// `list_by_path_recent`'s doc comment for why `list_by_path`'s
    /// `path ASC` primary sort can silently drop the newest rows.
    pub fn list_by_path_recent(
        &self,
        path_prefix: &str,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_by_path_recent(
            &self.conn,
            path_prefix,
            limit,
            include_archived,
            self.is_wiki_corpus_store(),
        )
    }

    /// Delete a memory entry by ID. Returns true if found and deleted.
    pub fn delete(&mut self, id: &str) -> Result<bool, MemoryError> {
        let db_label = self.db_label.clone();
        let profile = self.profile;
        db::retry_memory_locked("delete", &db_label, || {
            db::delete(&mut self.conn, id, self.vec_available, profile)
        })
    }

    pub fn list_wiki_duplicate_candidates(
        &self,
        path: &str,
        topic: &str,
        parent_path: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        db::list_wiki_duplicate_candidates(&self.conn, path, topic, parent_path, Some(limit))
    }

    /// Run PRAGMA quick_check to detect database corruption early.
    /// Returns Ok(true) if healthy, Ok(false) if corrupt.
    pub fn quick_check(&self) -> Result<bool, MemoryError> {
        let result: String = self
            .conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        Ok(result == "ok")
    }

    /// Get aggregate statistics about the memory store.
    pub fn stats(&self, include_archived: bool) -> Result<StatsResult, MemoryError> {
        db::stats(&self.conn, include_archived)
    }
}

/// Search is the user-visible read boundary where a short-lived startup or
/// migration lock must not leak into otherwise healthy Memory/Wiki sections.
/// SQLite's busy handler covers `BUSY`, but `LOCKED` can return immediately;
/// this bounded policy covers both without changing unrelated read methods.
const SEARCH_LOCK_RETRY_BUDGET: Duration = Duration::from_secs(3);
const SEARCH_LOCK_RETRY_INITIAL_BACKOFF: Duration = Duration::from_millis(25);
const SEARCH_LOCK_RETRY_MAX_BACKOFF: Duration = Duration::from_millis(400);

fn retry_search_locked<T>(
    db_label: &str,
    mut operation: impl FnMut() -> Result<T, MemoryError>,
) -> Result<T, MemoryError> {
    let started = Instant::now();
    let mut backoff = SEARCH_LOCK_RETRY_INITIAL_BACKOFF;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(MemoryError::Sqlite(error))
                if db::sqlite_error_is_locked(&error)
                    && started.elapsed().saturating_add(backoff) < SEARCH_LOCK_RETRY_BUDGET =>
            {
                tracing::debug!(
                    op = "search",
                    db_label,
                    backoff_ms = backoff.as_millis() as u64,
                    "memcore search lock retry: database busy/locked, backing off"
                );
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(SEARCH_LOCK_RETRY_MAX_BACKOFF);
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_retries_a_real_sqlite_lock_and_preserves_exhaustion_errors() {
        let path = crate::test_fixtures::test_fixture_path(format!(
            "memcore-search-lock-retry-{}.db",
            uuid::Uuid::new_v4()
        ));
        let path_string = path.to_string_lossy().into_owned();
        let store = MemoryStore::open(&path_string).expect("open search store");
        store
            .conn
            .busy_timeout(Duration::ZERO)
            .expect("disable SQLite's opaque busy wait for discrimination");
        store
            .conn
            .pragma_update(None, "journal_mode", "DELETE")
            .expect("use rollback journal so an exclusive lock blocks readers");

        let locker = Connection::open(&path).expect("open independent lock owner");
        locker
            .busy_timeout(Duration::ZERO)
            .expect("disable locker busy wait");
        locker
            .execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
            .expect("hold a real exclusive SQLite lock");

        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            locker.execute_batch("ROLLBACK;").expect("release lock");
        });
        store
            .search(
                "first search after health",
                Some(SearchOptions {
                    record_access: false,
                    ..Default::default()
                }),
            )
            .expect("search should recover when the real lock clears within budget");
        release.join().expect("lock owner thread");

        let persistent_locker = Connection::open(&path).expect("open persistent lock owner");
        persistent_locker
            .busy_timeout(Duration::ZERO)
            .expect("disable locker busy wait");
        persistent_locker
            .execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
            .expect("hold persistent real lock");
        let error = store
            .search(
                "honest exhaustion",
                Some(SearchOptions {
                    record_access: false,
                    ..Default::default()
                }),
            )
            .expect_err("persistent lock must remain an honest failure");
        assert!(
            matches!(error, MemoryError::Sqlite(ref sqlite) if db::sqlite_error_is_locked(sqlite)),
            "retry exhaustion must preserve the typed SQLite lock error: {error}"
        );
        persistent_locker
            .execute_batch("ROLLBACK;")
            .expect("release persistent lock");
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn search_with_receipt_preserves_store_vector_capability_and_db_label() {
        let store = MemoryStore::open_in_memory().expect("open test store");
        let (_, receipt) = store
            .search_with_receipt(
                "no matching content",
                Some(SearchOptions {
                    vec_available: !store.vec_available,
                    record_access: false,
                    ..Default::default()
                }),
            )
            .expect("receipt search");

        assert_eq!(receipt.operation, SearchReceiptOperation::MemoryStoreSearch);
        assert_eq!(
            receipt.database_scope,
            SearchReceiptDatabaseScope::Label("unknown".to_string())
        );
        assert_eq!(
            receipt
                .candidates
                .expect("candidate phase ran")
                .vec_available,
            store.vec_available,
            "store capability, not caller input, controls the vector channel"
        );
    }

    fn store_identity_entry(id: &str, path: &str, text: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: path.to_string(),
            summary: text.to_string(),
            text: text.to_string(),
            importance: 0.7,
            timestamp: "2026-07-31T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "manual".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    /// tachi#1569 fixture: a file-backed store opened with an explicit
    /// manifest label, seeded with one ordinary row and one recall-cache row.
    /// Both ids carry the same needle so one query reaches both, and both
    /// paths are write-legal in *any* labelled store, so the wiki and
    /// non-wiki fixtures differ only by the store's identity — which is the
    /// whole point of the comparison.
    fn store_with_internal_rows(label: &str) -> (MemoryStore, std::path::PathBuf) {
        let path = crate::test_fixtures::test_fixture_path(format!(
            "memcore-store-identity-{}-{}.db",
            label,
            uuid::Uuid::new_v4()
        ));
        let path_string = path.to_string_lossy().into_owned();
        let mut store =
            MemoryStore::open_with_label(&path_string, label).expect("open labelled store");

        store
            .upsert(&store_identity_entry(
                "store-identity-ordinary",
                "/notes/store-identity",
                "StoreIdentityNeedle ordinary user-facing content",
            ))
            .expect("seed ordinary row");
        store
            .upsert(&store_identity_entry(
                "store-identity-cache",
                "/recall-cache/store-identity",
                "StoreIdentityNeedle rendered recall rows",
            ))
            .expect("seed recall-cache row");

        (store, path)
    }

    fn search_ids(store: &MemoryStore, path_prefix: Option<&str>) -> Vec<String> {
        let mut ids = store
            .search(
                "StoreIdentityNeedle",
                Some(SearchOptions {
                    top_k: 20,
                    candidates_per_channel: 20,
                    record_access: false,
                    path_prefix: path_prefix.map(str::to_owned),
                    ..Default::default()
                }),
            )
            .expect("search")
            .into_iter()
            .map(|result| result.entry.id)
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    /// (a) An unscoped query against the Wiki store must not return the
    /// store's internal rows — and the reason must be the store's identity,
    /// not the request's shape, which is what the identical query against an
    /// identically-seeded non-wiki store proves is *not* doing the work here.
    #[test]
    fn unscoped_search_of_the_wiki_store_excludes_internal_rows() {
        let (wiki_store, wiki_path) = store_with_internal_rows("wiki");
        assert!(wiki_store.is_wiki_corpus_store());
        assert_eq!(
            search_ids(&wiki_store, None),
            vec!["store-identity-ordinary".to_string()],
            "an unscoped read of the wiki store must not surface its recall-cache rows"
        );
        drop(wiki_store);
        let _ = std::fs::remove_file(wiki_path);
    }

    /// (b) The frozen tachi#1569 decision: an explicit `/recall-cache`
    /// `path_prefix` still reaches those rows through the store-keyed gate.
    /// Without this the SQL clause would drop them before the Rust classifier
    /// (`is_namespace_search_noise`'s opt-in) could hand them back.
    #[test]
    fn recall_cache_opt_in_survives_the_store_keyed_gate() {
        let (wiki_store, wiki_path) = store_with_internal_rows("wiki");
        assert_eq!(
            search_ids(&wiki_store, Some("/recall-cache")),
            vec!["store-identity-cache".to_string()],
            "an explicit /recall-cache prefix is an opt-in, not a leak"
        );
        drop(wiki_store);
        let _ = std::fs::remove_file(wiki_path);
    }

    /// (c) A store that is not the Wiki corpus is unaffected: same rows, same
    /// query, no gate. (The rows still reach the caller here because the Rust
    /// noise classifier is a ranking-time filter for search *results*, and
    /// this store is not the wiki corpus, so nothing in SQL excludes them.)
    #[test]
    fn a_non_wiki_store_is_not_gated_by_store_identity() {
        let (project_store, project_path) = store_with_internal_rows("some-project");
        assert!(!project_store.is_wiki_corpus_store());
        assert!(
            project_store
                .list_by_path("/recall-cache", 10, false)
                .expect("list")
                .iter()
                .any(|entry| entry.id == "store-identity-cache"),
            "a non-wiki store must keep returning every row it did before"
        );
        drop(project_store);
        let _ = std::fs::remove_file(project_path);
    }

    /// The list family is the surface tachi#1561 had to patch server-side
    /// because it never reached the wiki clause at all. Store identity now
    /// excludes internal rows *before* the `LIMIT` is applied.
    #[test]
    fn list_routes_of_the_wiki_store_exclude_internal_rows_before_the_limit() {
        let (wiki_store, wiki_path) = store_with_internal_rows("wiki");
        let listed = wiki_store.list_by_path("/", 10, false).expect("list");
        assert!(
            listed
                .iter()
                .any(|entry| entry.id == "store-identity-ordinary"),
            "ordinary rows must still list"
        );
        assert!(
            !listed
                .iter()
                .any(|entry| entry.id == "store-identity-cache"),
            "the wiki store's internal rows must not consume a list budget"
        );

        // …and the same opt-in applies here.
        let opted_in = wiki_store
            .list_by_path("/recall-cache", 10, false)
            .expect("list opted in");
        assert!(
            opted_in
                .iter()
                .any(|entry| entry.id == "store-identity-cache"),
            "an explicit /recall-cache list prefix opts back in, exactly as search does"
        );

        let recent = wiki_store
            .list_by_path_recent("/", 10, false)
            .expect("list recent");
        assert!(!recent
            .iter()
            .any(|entry| entry.id == "store-identity-cache"));
        let all = wiki_store.get_all(10).expect("get_all");
        assert!(!all.iter().any(|entry| entry.id == "store-identity-cache"));

        // Id-addressed reads stay ungated on purpose: the store's own
        // machinery fetches its bookkeeping rows this way (see
        // `get_with_options`' doc).
        assert!(
            wiki_store
                .get("store-identity-cache")
                .expect("get by id")
                .is_some(),
            "an id-addressed read must still resolve internal rows"
        );
        drop(wiki_store);
        let _ = std::fs::remove_file(wiki_path);
    }
}
