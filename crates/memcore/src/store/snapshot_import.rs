//! Atomic snapshot import: move a stored corpus into a fresh destination
//! **including** the lifecycle columns `MemoryEntry` does not carry
//! (tachi#1607).
//!
//! Why this is a separate write path from `MemoryStore::upsert_batch`, and
//! not a flag on it: `upsert_batch` applies *writes*. It stamps
//! `created_at`/`updated_at` with wall clock, bumps `revision` on conflict,
//! and runs write-time near-duplicate consolidation that can stamp
//! `superseded_by` on rows the caller never asked to supersede. A migration
//! that replayed a corpus through it — even followed by public
//! `supersede_memory` calls to rebuild the edges — would preserve row count
//! and every visible `MemoryEntry` field while changing which rows satisfy
//! the canonical-active predicate `archived = 0 AND superseded_by IS NULL`.
//! That predicate is what default recall reads, so "row-count parity" would
//! ship a silently different memory. Snapshot semantics — verbatim lifecycle,
//! no consolidation, no revision bump, no `updated_at` rewrite — are the
//! entire point of this module.
//!
//! What is *not* relaxed: this path runs the same per-entry
//! `KernelPolicy`-driven write-path validation as ordinary `upsert`, takes
//! the same reserved-reference write authorization, and refuses reserved
//! identities through the very same function the ordinary upsert body calls
//! (`db::memory_crud::refuse_reserved_write_identity`: blank `id`,
//! `anchor:`, `wiki-rem:`, and the Wiki operation-log identity). The
//! read-only census of the real corpus on 2026-08-04 found zero blank,
//! `anchor:` or `wiki-rem:` ids, so refusing on encounter is correct — there
//! is nothing to exempt.
//!
//! Class law (tachi#1585): no closure and no `Connection`/`Transaction`
//! appears in any signature here, which is why the surface can be ungated for
//! the portable build.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    db::{self, SnapshotLifecycleRow, SnapshotVectorRow},
    error::MemoryError,
    types::MemoryEntry,
    MemoryStore,
};

/// One source row plus the lifecycle columns `MemoryEntry` cannot carry.
///
/// `created_at`, `updated_at` and `superseded_by` are written to the
/// destination **verbatim**. So are `archived`, `revision` and `valid_until`
/// from `entry`. Nothing here is defaulted, normalized, clamped, or
/// re-derived: a `revision` of 0, an empty `created_at`, or a `valid_until`
/// that is `Some("")` rather than `None` all land in the destination exactly
/// as the source had them, because a snapshot that "fixes" its source is no
/// longer a snapshot. Corruption is surfaced by comparing the receipt
/// checksums against the source, not by this path quietly rewriting it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableImportEntry {
    /// The row body. Its `vector` field carries the embedding; a row whose
    /// `vector` is `None` imports without a vector projection and is counted
    /// in [`PortableImportReceipt::vectors_absent`].
    pub entry: MemoryEntry,
    /// `memories.created_at`, verbatim.
    pub created_at: String,
    /// `memories.updated_at`, verbatim. Never replaced with write time.
    pub updated_at: String,
    /// `memories.superseded_by`, verbatim. A target that does not exist in
    /// the destination after the import is preserved as-is and reported in
    /// [`PortableImportReceipt::dangling_supersessions`] — never repaired
    /// (repair is tachi#1350's operation) and never implicitly rejected.
    pub superseded_by: Option<String>,
}

/// A preserved-but-unresolvable supersession edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DanglingSupersession {
    /// The imported row carrying the edge.
    pub id: String,
    /// The `superseded_by` value it carries, which matched no `memories.id`
    /// in the destination after the whole batch was written.
    pub superseded_by: String,
}

/// Deterministic accounting for one [`MemoryStore::import_snapshot_batch`]
/// call, computed from the **destination's post-write state inside the import
/// transaction** — never from the caller's input structs. A receipt derived
/// from the inputs would stay green even if the writer dropped
/// `superseded_by`; this one cannot.
///
/// # Reconciling without raw destination SQL
///
/// Both checksums are lowercase hex SHA-256 over a compact UTF-8 JSON array
/// (`serde_json` compact form: no whitespace, `"` string quoting with
/// `serde_json`'s standard escaping). The exact canonicalization:
///
/// **`lifecycle_checksum`** — array of one object per imported row, array
/// elements sorted ascending by `id` compared as raw UTF-8 bytes
/// (Rust `str` ordering, i.e. SQLite `BINARY` collation), each object with
/// exactly these seven keys in this order (alphabetical):
///
/// ```text
/// {"archived":<bool>,"created_at":<string>,"id":<string>,
///  "revision":<integer>,"superseded_by":<string|null>,
///  "updated_at":<string>,"valid_until":<string|null>}
/// ```
///
/// `archived` is the JSON boolean `memories.archived != 0`; `revision` is the
/// integer as stored; `superseded_by` and `valid_until` are JSON `null` when
/// the column is SQL NULL and a JSON string otherwise (including the empty
/// string, which is a distinct value from NULL and is preserved as such).
///
/// **`vector_checksum`** — array of one object per imported row that has a
/// vector projection, sorted by `id` the same way, each object exactly:
///
/// ```text
/// {"id":<string>,"vector_sha256":<string>}
/// ```
///
/// where `vector_sha256` is the lowercase hex SHA-256 of the raw
/// `memories_vec.embedding` blob — little-endian IEEE-754 `f32`, four bytes
/// per element, no header — which is the same byte string a source database
/// stores. Rows without a vector do not appear.
///
/// For an empty import both fields are the SHA-256 of the two-byte document
/// `[]`, not an empty string.
///
/// A consumer computing the source side can either run the same SQL over its
/// own database and hash per the rules above, or, if it links this crate,
/// call [`PortableImportReceipt::expected_lifecycle_checksum`] /
/// [`PortableImportReceipt::expected_vector_checksum`] over the entries it is
/// about to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortableImportReceipt {
    /// Rows written to `memories`. Equals the input length on success.
    pub rows_imported: usize,
    /// Imported rows that landed a `memories_vec` projection.
    pub vectors_imported: usize,
    /// Imported rows that carried no vector and therefore have no projection.
    pub vectors_absent: usize,
    /// Preserved edges whose target is absent from the destination, sorted by
    /// `id`. Empty when every edge resolves.
    pub dangling_supersessions: Vec<DanglingSupersession>,
    /// See the type-level doc for the exact canonicalization.
    pub lifecycle_checksum: String,
    /// See the type-level doc for the exact canonicalization.
    pub vector_checksum: String,
}

impl PortableImportReceipt {
    fn empty() -> Result<Self, MemoryError> {
        Ok(Self {
            rows_imported: 0,
            vectors_imported: 0,
            vectors_absent: 0,
            dangling_supersessions: Vec::new(),
            lifecycle_checksum: canonical_sha256(&Vec::<SnapshotLifecycleRow>::new())?,
            vector_checksum: canonical_sha256(&Vec::<SnapshotVectorRow>::new())?,
        })
    }

    /// The `lifecycle_checksum` a successful import of `entries` must return,
    /// computed from the source-side values alone.
    ///
    /// This is the source half of the reconciliation: it never touches a
    /// database, so comparing it against a receipt compares two independent
    /// paths — one through SQLite storage and back, one not.
    pub fn expected_lifecycle_checksum(
        entries: &[PortableImportEntry],
    ) -> Result<String, MemoryError> {
        let mut rows: Vec<SnapshotLifecycleRow> = entries
            .iter()
            .map(|import| SnapshotLifecycleRow {
                archived: import.entry.archived,
                created_at: import.created_at.clone(),
                id: import.entry.id.clone(),
                revision: import.entry.revision,
                superseded_by: import.superseded_by.clone(),
                updated_at: import.updated_at.clone(),
                valid_until: import.entry.valid_until.clone(),
            })
            .collect();
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        canonical_sha256(&rows)
    }

    /// The `vector_checksum` a successful import of `entries` must return,
    /// computed from the source-side embeddings alone.
    pub fn expected_vector_checksum(
        entries: &[PortableImportEntry],
    ) -> Result<String, MemoryError> {
        let mut rows: Vec<SnapshotVectorRow> = entries
            .iter()
            .filter_map(|import| {
                import
                    .entry
                    .vector
                    .as_ref()
                    .map(|vector| SnapshotVectorRow {
                        id: import.entry.id.clone(),
                        vector_sha256: hex_sha256(&db::serialize_f32(vector)),
                    })
            })
            .collect();
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        canonical_sha256(&rows)
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_sha256<T: Serialize>(value: &T) -> Result<String, MemoryError> {
    Ok(hex_sha256(&serde_json::to_vec(value)?))
}

impl MemoryStore {
    /// Import a stored snapshot — rows, FTS/symbolic projections, existing
    /// vectors, and the full lifecycle state — in one transaction, and return
    /// a deterministic receipt the caller can reconcile against its source
    /// without any raw SQL on this destination.
    ///
    /// # What is preserved
    ///
    /// `archived`, `revision`, `valid_until`, `created_at`, `updated_at` and
    /// `superseded_by` are written exactly as supplied. Write-time
    /// near-duplicate consolidation is **not** run: two byte-identical bodies
    /// that the source says are both canonical stay both canonical here.
    /// A `superseded_by` pointing at an id absent from the destination is
    /// imported as-is and listed in
    /// [`PortableImportReceipt::dangling_supersessions`]; repairing it is
    /// tachi#1350's operation, not this one.
    ///
    /// # What is refused
    ///
    /// * any entry failing this store's `KernelPolicy`-driven path validation
    ///   — checked for every entry *before* a transaction is opened;
    /// * a blank `id`, an `id` in the reserved `anchor:` or `wiki-rem:`
    ///   namespaces, or the Wiki operation-log identity — through the same
    ///   `refuse_reserved_write_identity` body the ordinary upsert seam runs
    ///   (tachi#1602), so the refusals and their error text are identical;
    /// * an `id` that **already exists** in the destination. Snapshot import
    ///   targets a fresh destination; rewriting an existing row in place
    ///   would be an unaudited raw-write escape, so this is a typed
    ///   [`MemoryError::Duplicate`] and the whole batch rolls back. Importing
    ///   into a non-empty store is therefore supported only when the imported
    ///   ids are disjoint from the ids already present;
    /// * an entry carrying a vector when this store has no vector projection
    ///   table — importing it would silently drop the embedding.
    ///
    /// # Atomicity
    ///
    /// One `BEGIN IMMEDIATE` for the whole batch. Any row, projection,
    /// lifecycle, readback or receipt failure — at any position in the batch
    /// — rolls back every earlier row and projection: zero rows, zero FTS
    /// entries, zero vectors. An empty slice is a successful no-op that opens
    /// no transaction and returns a receipt with zero counts and the
    /// empty-array checksums.
    ///
    /// # Trust boundary
    ///
    /// Unlike ordinary upsert, `metadata` is stored verbatim, including the
    /// reserved reference keys (`evidence_refs_v1`, `source_refs`, `rem`,
    /// `wiki_log`) that `merge_ordinary_reserved_metadata` strips from
    /// caller-supplied writes — stripping them would delete evidence from
    /// every imported row, which for a migration is data loss. That makes
    /// this a **database-owner migration primitive**: it must not be wired to
    /// an endpoint that accepts untrusted caller-authored metadata. Its
    /// intended caller holds the source database and the destination file.
    ///
    /// `store_identity` and `hard_state` are untouched by construction: this
    /// method writes `memories` and its projections only, so the destination
    /// keeps the `StoreProfile` and injected `KernelPolicy` it was opened
    /// with.
    pub fn import_snapshot_batch(
        &mut self,
        entries: &[PortableImportEntry],
    ) -> Result<PortableImportReceipt, MemoryError> {
        if entries.is_empty() {
            return PortableImportReceipt::empty();
        }

        // Pre-walk: every entry's policy-driven path check runs before any
        // write, so an invalid entry anywhere in the batch means no
        // transaction is ever opened — the same ordering `upsert_batch` uses.
        for import in entries {
            self.validate_write_path(&import.entry)?;
        }

        let vec_available = self.vec_available;
        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        for import in entries {
            db::import_snapshot_row_within_tx(&tx, import, vec_available)?;
        }

        // Receipt is built from the destination's own post-write state, still
        // inside the transaction, so a failure here rolls the import back too.
        let mut lifecycle_rows = Vec::with_capacity(entries.len());
        let mut vector_rows = Vec::new();
        let mut dangling = Vec::new();
        for import in entries {
            let id = import.entry.id.as_str();
            lifecycle_rows.push(db::read_snapshot_lifecycle_row_within_tx(&tx, id)?);
            if let Some(blob) = db::read_snapshot_vector_blob_within_tx(&tx, id, vec_available)? {
                vector_rows.push(SnapshotVectorRow {
                    id: id.to_string(),
                    vector_sha256: hex_sha256(&blob),
                });
            }
            if let Some(target) = &import.superseded_by {
                if !db::memory_row_exists_within_tx(&tx, target)? {
                    dangling.push(DanglingSupersession {
                        id: id.to_string(),
                        superseded_by: target.clone(),
                    });
                }
            }
        }
        lifecycle_rows.sort_by(|left, right| left.id.cmp(&right.id));
        vector_rows.sort_by(|left, right| left.id.cmp(&right.id));
        dangling.sort_by(|left, right| left.id.cmp(&right.id));

        let receipt = PortableImportReceipt {
            rows_imported: entries.len(),
            vectors_imported: vector_rows.len(),
            vectors_absent: entries.len() - vector_rows.len(),
            dangling_supersessions: dangling,
            lifecycle_checksum: canonical_sha256(&lifecycle_rows)?,
            vector_checksum: canonical_sha256(&vector_rows)?,
        };

        tx.commit()?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_entry(id: &str, text: &str) -> PortableImportEntry {
        PortableImportEntry {
            entry: MemoryEntry {
                id: id.to_string(),
                path: "/snapshot/import".to_string(),
                summary: String::new(),
                text: text.to_string(),
                importance: 0.5,
                timestamp: "2026-01-02T03:04:05.000Z".to_string(),
                valid_from: "2026-01-02T03:04:05.000Z".to_string(),
                valid_until: None,
                category: "fact".to_string(),
                topic: "snapshot".to_string(),
                keywords: Vec::new(),
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                source: "migration".to_string(),
                scope: "project".to_string(),
                archived: false,
                access_count: 0,
                scored_count: 0,
                last_access: None,
                last_use_at: None,
                revision: 1,
                metadata: serde_json::json!({}),
                vector: None,
                retention_policy: None,
                domain: None,
                recall_count: 0,
                query_diversity: 0,
                tier: "raw".to_string(),
            },
            created_at: "2025-11-01T00:00:00.000Z".to_string(),
            updated_at: "2025-12-01T00:00:00.000Z".to_string(),
            superseded_by: None,
        }
    }

    /// The six row shapes tachi#1607's acceptance names, read back column by
    /// column. This is the test that discriminates snapshot import from
    /// `upsert_batch`: run this batch through `upsert_batch` and
    /// `created_at`/`updated_at` become write time, `superseded_by` is lost,
    /// and the archived/superseded rows silently become canonical-active.
    #[test]
    fn snapshot_import_preserves_all_six_row_shapes_with_exact_readback_parity() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");

        // 1: active, unsuperseded. 2: archived + superseded (the ordinary
        // shape). 3: active *and* superseded — 3,563 rows of the real corpus.
        // 4: superseded with a null valid_until — 3,874 of them. 5: vector
        // present. 6: vector missing.
        let mut active = snapshot_entry("snap-active", "snapshot row one active");
        active.created_at = "2025-01-01T00:00:00.000Z".to_string();
        active.updated_at = "2025-01-02T00:00:00.000Z".to_string();

        let mut archived = snapshot_entry("snap-archived", "snapshot row two archived");
        archived.entry.archived = true;
        archived.entry.revision = 7;
        archived.entry.valid_until = Some("2025-06-01T00:00:00.000Z".to_string());
        archived.superseded_by = Some("snap-active".to_string());

        let mut active_superseded =
            snapshot_entry("snap-active-superseded", "snapshot row three live edge");
        active_superseded.entry.archived = false;
        active_superseded.entry.revision = 3;
        active_superseded.entry.valid_until = Some("2025-07-01T00:00:00.000Z".to_string());
        active_superseded.superseded_by = Some("snap-active".to_string());

        let mut null_valid_until =
            snapshot_entry("snap-null-valid-until", "snapshot row four open interval");
        null_valid_until.entry.valid_until = None;
        null_valid_until.superseded_by = Some("snap-active".to_string());

        let mut with_vector = snapshot_entry("snap-vector", "snapshot row five with embedding");
        with_vector.entry.vector = Some(vec![0.125_f32; 1024]);

        let without_vector = snapshot_entry("snap-no-vector", "snapshot row six without embedding");

        let batch = vec![
            active,
            archived,
            active_superseded,
            null_valid_until,
            with_vector,
            without_vector,
        ];
        let receipt = store
            .import_snapshot_batch(&batch)
            .expect("snapshot import must succeed");

        assert_eq!(receipt.rows_imported, 6);
        assert_eq!(receipt.vectors_imported, 1);
        assert_eq!(receipt.vectors_absent, 5);
        assert!(
            receipt.dangling_supersessions.is_empty(),
            "every edge resolves inside this batch: {:?}",
            receipt.dangling_supersessions
        );
        assert_eq!(
            receipt.lifecycle_checksum,
            PortableImportReceipt::expected_lifecycle_checksum(&batch).expect("expected lifecycle"),
            "destination lifecycle state must hash to the source-side value"
        );
        assert_eq!(
            receipt.vector_checksum,
            PortableImportReceipt::expected_vector_checksum(&batch).expect("expected vector"),
            "destination vector projection must hash to the source-side value"
        );

        for import in &batch {
            let id = import.entry.id.as_str();
            let (archived, revision, valid_until, created_at, updated_at, superseded_by): (
                i64,
                i64,
                Option<String>,
                String,
                String,
                Option<String>,
            ) = store
                .connection()
                .query_row(
                    "SELECT archived, revision, valid_until, created_at, updated_at, superseded_by
                     FROM memories WHERE id = ?1",
                    [id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .unwrap_or_else(|error| panic!("row {id} must exist: {error}"));
            assert_eq!(archived != 0, import.entry.archived, "archived for {id}");
            assert_eq!(revision, import.entry.revision, "revision for {id}");
            assert_eq!(
                valid_until, import.entry.valid_until,
                "valid_until for {id}"
            );
            assert_eq!(created_at, import.created_at, "created_at for {id}");
            assert_eq!(updated_at, import.updated_at, "updated_at for {id}");
            assert_eq!(
                superseded_by, import.superseded_by,
                "superseded_by for {id}"
            );
        }

        // The active-superseded row is exactly the shape the issue says a
        // naive migration flips: it must NOT satisfy the canonical-active
        // predicate after import.
        let canonical_active: Vec<String> = store
            .connection()
            .prepare(
                "SELECT id FROM memories WHERE archived = 0 AND superseded_by IS NULL ORDER BY id",
            )
            .expect("prepare")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("collect");
        assert_eq!(
            canonical_active,
            vec![
                "snap-active".to_string(),
                "snap-no-vector".to_string(),
                "snap-vector".to_string()
            ],
            "canonical-active set must match the source snapshot exactly"
        );

        // Vector projection landed for exactly the vector-bearing row.
        let vec_ids: Vec<String> = store
            .connection()
            .prepare("SELECT id FROM memories_vec ORDER BY id")
            .expect("prepare")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("collect");
        assert_eq!(vec_ids, vec!["snap-vector".to_string()]);

        // FTS projection landed for every row.
        let fts_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM memories_fts", [], |row| row.get(0))
            .expect("fts count");
        assert_eq!(fts_count, 6, "every imported row needs an FTS projection");
    }

    /// tachi#1572's failure mode, inverted: `upsert_batch` folds a
    /// byte-identical second body into the first and stamps it
    /// `superseded_by`. Snapshot import must not, because the source says
    /// both rows are canonical.
    #[test]
    fn byte_identical_bodies_both_stay_canonical_because_near_dup_policy_is_not_invoked() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let shared = "identical body that ordinary write-time consolidation would merge";
        let batch = vec![
            snapshot_entry("snap-dup-1", shared),
            snapshot_entry("snap-dup-2", shared),
        ];

        let receipt = store
            .import_snapshot_batch(&batch)
            .expect("identical bodies must import");
        assert_eq!(receipt.rows_imported, 2);
        assert!(receipt.dangling_supersessions.is_empty());

        for id in ["snap-dup-1", "snap-dup-2"] {
            let superseded_by: Option<String> = store
                .connection()
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap_or_else(|error| panic!("row {id} must exist: {error}"));
            assert!(
                superseded_by.is_none(),
                "snapshot import invented a supersession edge for {id}: {superseded_by:?}"
            );
        }
    }

    #[test]
    fn dangling_supersession_target_is_preserved_and_reported_not_repaired() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let mut dangling = snapshot_entry("snap-dangling", "snapshot row with an absent target");
        dangling.superseded_by = Some("snap-absent-target".to_string());
        let batch = vec![dangling];

        let receipt = store
            .import_snapshot_batch(&batch)
            .expect("a dangling edge must not fail the import");
        assert_eq!(
            receipt.dangling_supersessions,
            vec![DanglingSupersession {
                id: "snap-dangling".to_string(),
                superseded_by: "snap-absent-target".to_string(),
            }]
        );

        let stored: Option<String> = store
            .connection()
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = 'snap-dangling'",
                [],
                |row| row.get(0),
            )
            .expect("row must exist");
        assert_eq!(
            stored.as_deref(),
            Some("snap-absent-target"),
            "the dangling value must be preserved verbatim, not nulled or repaired"
        );
        assert_eq!(
            receipt.lifecycle_checksum,
            PortableImportReceipt::expected_lifecycle_checksum(&batch).expect("expected lifecycle")
        );
    }

    #[test]
    fn a_later_row_failure_leaves_zero_rows_and_zero_projections() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let mut good = snapshot_entry("snap-rollback-ok", "snapshot rollback good row");
        good.entry.vector = Some(vec![0.5_f32; 1024]);
        // `wiki-rem:` is refused inside the shared reserved-identity guard,
        // i.e. after the pre-walk and inside the transaction, so this proves
        // the transaction rolls back rather than only pre-validation failing.
        let bad = snapshot_entry("wiki-rem:snap-rollback-bad", "snapshot rollback bad row");

        let error = store
            .import_snapshot_batch(&[good, bad])
            .expect_err("a later invalid row must fail the whole import");
        assert!(
            matches!(error, MemoryError::InvalidArg(_)),
            "unexpected error variant: {error:?}"
        );

        let rows: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .expect("count memories");
        assert_eq!(rows, 0, "rolled-back import must leave zero rows");
        let fts: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM memories_fts", [], |row| row.get(0))
            .expect("count fts");
        assert_eq!(fts, 0, "rolled-back import must leave zero FTS projections");
        let vectors: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM memories_vec", [], |row| row.get(0))
            .expect("count vectors");
        assert_eq!(
            vectors, 0,
            "rolled-back import must leave zero vector projections"
        );
    }

    #[test]
    fn empty_input_is_a_successful_no_op_with_an_empty_receipt() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let receipt = store
            .import_snapshot_batch(&[])
            .expect("empty import is Ok");
        assert_eq!(receipt.rows_imported, 0);
        assert_eq!(receipt.vectors_imported, 0);
        assert_eq!(receipt.vectors_absent, 0);
        assert!(receipt.dangling_supersessions.is_empty());
        assert_eq!(
            receipt.lifecycle_checksum,
            PortableImportReceipt::expected_lifecycle_checksum(&[]).expect("expected lifecycle")
        );
        assert_eq!(
            receipt.vector_checksum,
            PortableImportReceipt::expected_vector_checksum(&[]).expect("expected vector")
        );
        let rows: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .expect("count memories");
        assert_eq!(rows, 0);
    }

    #[test]
    fn an_id_already_present_in_the_destination_is_refused_and_rolls_the_batch_back() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        store
            .import_snapshot_batch(&[snapshot_entry("snap-existing", "first import")])
            .expect("first import");

        let error = store
            .import_snapshot_batch(&[
                snapshot_entry("snap-fresh", "second import fresh row"),
                snapshot_entry("snap-existing", "second import colliding row"),
            ])
            .expect_err("an existing id must be refused");
        assert!(
            matches!(error, MemoryError::Duplicate(_)),
            "unexpected error variant: {error:?}"
        );

        assert!(
            store.get("snap-fresh").expect("get").is_none(),
            "the earlier valid row of a refused batch must not persist"
        );
        let text: String = store
            .connection()
            .query_row(
                "SELECT text FROM memories WHERE id = 'snap-existing'",
                [],
                |row| row.get(0),
            )
            .expect("original row must survive");
        assert_eq!(
            text, "first import",
            "the pre-existing row must not be overwritten"
        );
    }

    #[test]
    fn a_blank_id_is_refused_by_the_shared_reserved_identity_guard() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let error = store
            .import_snapshot_batch(&[snapshot_entry("   ", "blank id row")])
            .expect_err("a blank id must be refused");
        assert!(
            error.to_string().contains("entry.id must be provided"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn an_anchor_namespace_id_is_refused_by_the_shared_reserved_identity_guard() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let error = store
            .import_snapshot_batch(&[snapshot_entry("anchor:project:x", "anchor row")])
            .expect_err("an anchor: id must be refused");
        assert!(
            error.to_string().contains("reserved 'anchor:' namespace"),
            "unexpected refusal: {error}"
        );
        let rows: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .expect("count memories");
        assert_eq!(rows, 0);
    }

    /// The receipt must be computed from the destination, not the input: a
    /// batch whose stored lifecycle differs from the supplied one has to show
    /// a checksum mismatch. Simulated here by hashing a deliberately wrong
    /// source-side expectation.
    #[test]
    fn lifecycle_checksum_changes_when_any_lifecycle_field_changes() {
        let mut store = MemoryStore::open_in_memory().expect("open_in_memory");
        let mut row = snapshot_entry("snap-checksum", "snapshot checksum row");
        row.superseded_by = Some("snap-checksum-target".to_string());
        let batch = vec![row.clone()];
        let receipt = store.import_snapshot_batch(&batch).expect("import");

        let mut altered = row;
        altered.superseded_by = None;
        assert_ne!(
            receipt.lifecycle_checksum,
            PortableImportReceipt::expected_lifecycle_checksum(&[altered])
                .expect("expected lifecycle"),
            "dropping superseded_by must change the lifecycle checksum"
        );
    }
}
