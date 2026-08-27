//! Append-only assertion store + disposable reduction projection (#1696
//! storage/replay contract).
//!
//! * The **assertion table is the single append-only authority**. There is
//!   no update and no delete API for assertions — correction, retraction,
//!   reopen, and revert are new assertions, never rewrites.
//! * Ingestion is idempotent, keyed by immutable source identity/revision
//!   (`AssertionIngestionKey`): the same fact re-appended is a no-op; the
//!   same key with a *different* value is rejected — a source contradicted
//!   itself at a revision it declared immutable.
//! * The **projection table is disposable**: it can be dropped and rebuilt
//!   from the assertions without changing assertion authority. Full rebuild
//!   and incremental reduction both call the same pure
//!   [`super::reducer::reduce`] over the same deterministic ordering, so
//!   they are canonically equivalent.
//! * The **refresh table is operational metadata** (staleness posture and
//!   refresh debt), not a truth source: it never feeds the reducer.
//!
//! Schema lives in this crate (not memcore's DDL) because the typed
//! assertion vocabulary is defined here and memcore cannot depend on it;
//! `tachi-params` already carries the rusqlite edge for the taskintent
//! ingest adapter. The store owns a connection handed to it — the
//! `MemoryStore` integration seam is a later slice, mirroring how
//! `taskintent::memcore_ingest` takes a `Connection`.
//!
//! # Trust boundary and integration status (#1696 scope)
//!
//! **The store trusts its caller.** `authority_class`, `review_state`, and
//! `issuer` are data fields the caller supplies; this library performs
//! structural validation only. The semantic admission gate — who may append
//! which authority class, against which live identity — is the
//! server-integration admission surface and is deliberately OUT of this
//! leaf: #1696's verification list ships the store, fixtures, reducer
//! tests, and the #1693 consumer FIXTURE (discrimination 11), with no
//! production caller. The follow-up slices that consume this seam must
//! interpose that admission surface before any untrusted caller reaches
//! `append`. `tachi_events` remains the collect-only observation ledger;
//! bridging observations into assertions is a later slice, not a second
//! authority here.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::types::{
    AssertionIngestionKey, AssertionV1, AssertionValueV1, AuthorityClassV1, EvidenceHeadV1,
    GithubObjectRefV1, PredicateV1, ReviewStateV1, SourceRefV1, SubjectRefV1, VisibilityClassV1,
};

/// Typed store failure.
#[derive(Debug, thiserror::Error)]
pub enum CurrentTruthStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("assertion id must be non-empty")]
    EmptyAssertionId,
    #[error("subject repository must be `owner/name`")]
    MalformedSubject,
    #[error("source revision must be non-empty")]
    EmptySourceRevision,
    #[error("predicate `{0}` is projection-computed and may not be asserted by a source")]
    ProjectionPredicateNotAssertable(PredicateV1),
    #[error(
        "ingestion key already recorded with a different value at the same immutable revision: {0:?}"
    )]
    ContradictsExistingRevision(AssertionIngestionKey),
    #[error("stored assertion row is not decodable: {0}")]
    CorruptRow(String),
    #[error("observed_at/effective_at must be RFC 3339 — `{0}` does not parse")]
    MalformedObservedAt(String),
    #[error(
        "refresh record older than the recorded attempt for `{repo}`: {recorded_at} < {existing_attempt_at}"
    )]
    StaleRefreshRecord {
        repo: String,
        recorded_at: String,
        existing_attempt_at: String,
    },
}

/// The result of an append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendOutcome {
    /// A new assertion entered the store.
    Appended,
    /// The same immutable fact was already present — no write, no error.
    IdempotentDuplicate,
}

const ASSERTION_SCHEMA_SQL: &str = r#"
        -- CurrentTruth v1 append-only assertion authority (#1696).
        -- No UPDATE or DELETE path exists in the store API; corrections and
        -- retractions are new rows. `recorded_at` is operational metadata
        -- only and is never a reduction input (arrival order is not source
        -- revision).
        CREATE TABLE IF NOT EXISTS current_truth_assertions (
            assertion_id   TEXT PRIMARY KEY,
            subject_repo   TEXT NOT NULL,
            subject_kind   TEXT NOT NULL,
            subject_id     TEXT NOT NULL,
            predicate      TEXT NOT NULL,
            value_json     TEXT NOT NULL,
            issuer         TEXT NOT NULL,
            authority      TEXT NOT NULL,
            source_id      TEXT NOT NULL,
            source_revision TEXT NOT NULL,
            observed_at    TEXT NOT NULL,
            effective_at   TEXT NOT NULL,
            supersedes     TEXT,
            evidence_json  TEXT NOT NULL DEFAULT '[]',
            review_state   TEXT NOT NULL,
            visibility     TEXT NOT NULL,
            content_digest TEXT NOT NULL,
            recorded_at    TEXT NOT NULL DEFAULT '',
            UNIQUE (subject_repo, subject_kind, subject_id, predicate,
                    authority, issuer, source_id, source_revision)
        );
        CREATE INDEX IF NOT EXISTS idx_ct_assertions_subject
            ON current_truth_assertions(subject_repo, subject_kind, subject_id);
        CREATE INDEX IF NOT EXISTS idx_ct_assertions_predicate
            ON current_truth_assertions(predicate);
"#;

const PROJECTION_SCHEMA_SQL: &str = r#"
        -- Disposable, rebuildable reduction projection (#1696). Dropping
        -- every row here never changes assertion authority.
        CREATE TABLE IF NOT EXISTS current_truth_projection (
            repo        TEXT PRIMARY KEY,
            generation  TEXT NOT NULL,
            built_at    TEXT NOT NULL,
            view_json   TEXT NOT NULL
        );
"#;

const REFRESH_SCHEMA_SQL: &str = r#"
        -- Operational refresh posture metadata (#1696): staleness and
        -- refresh-debt bookkeeping. Not a truth source; never feeds the
        -- reducer.
        CREATE TABLE IF NOT EXISTS current_truth_refresh (
            repo                 TEXT PRIMARY KEY,
            fresh                INTEGER NOT NULL,
            last_fresh_revision  TEXT,
            last_fresh_at        TEXT,
            last_attempt_at      TEXT NOT NULL,
            unavailable_reason   TEXT
        );
"#;

/// SQLite-backed CurrentTruth store over a caller-owned connection.
pub struct CurrentTruthSqliteStore {
    conn: Connection,
}

impl CurrentTruthSqliteStore {
    /// Open (and migrate) a store at `path`.
    pub fn open(path: &str) -> Result<Self, CurrentTruthStoreError> {
        let conn = Connection::open(path)?;
        Self::with_connection(conn)
    }

    /// Open an in-memory store (tests, projections over ephemeral state).
    pub fn open_in_memory() -> Result<Self, CurrentTruthStoreError> {
        let conn = Connection::open_in_memory()?;
        Self::with_connection(conn)
    }

    /// Adopt a caller-owned connection and ensure the schema. Append-only
    /// semantics are enforced by this API surface — no mutation of existing
    /// assertion rows is possible through it.
    pub fn with_connection(conn: Connection) -> Result<Self, CurrentTruthStoreError> {
        conn.execute_batch(ASSERTION_SCHEMA_SQL)?;
        conn.execute_batch(PROJECTION_SCHEMA_SQL)?;
        conn.execute_batch(REFRESH_SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    /// Append one assertion. Idempotent on the immutable ingestion key;
    /// rejects a different value at the same key. Well-formed model-prose
    /// and candidate/rejected evidence IS stored (append-only history with
    /// provenance) — the reducer, not the store, keeps it from affecting
    /// current truth.
    pub fn append(&self, assertion: &AssertionV1) -> Result<AppendOutcome, CurrentTruthStoreError> {
        let transaction = self.conn.unchecked_transaction()?;
        let outcome = Self::append_in(&transaction, assertion)?;
        transaction.commit()?;
        Ok(outcome)
    }

    /// Digest over the full assertion content INCLUDING identity fields
    /// (subject, predicate, authority, issuer, source, revision): two
    /// assertions that differ in any field can never compare equal, so an
    /// id collision between distinct facts always surfaces as a typed
    /// rejection instead of a silent duplicate.
    fn content_digest(assertion: &AssertionV1) -> String {
        let value_digest = memcore::canonical_digest::canonical_json_digest_hex(
            &serde_json::to_value(&assertion.value).unwrap_or_default(),
        );
        let basis = serde_json::json!({
            "subject_repo": assertion.subject.repo,
            "subject_kind": kind_token(&assertion.subject.object),
            "subject_id": id_token(&assertion.subject.object),
            "predicate": assertion.predicate.as_str(),
            "authority": authority_token(assertion.authority_class),
            "issuer": assertion.issuer,
            "source": assertion.source_ref.source,
            "source_revision": assertion.source_ref.revision,
            "value_digest": value_digest,
            "observed_at": assertion.observed_at,
            "effective_at": assertion.effective_at,
            "supersedes": assertion.supersedes_assertion_id,
            "evidence_refs": assertion.evidence_refs,
            "review_state": review_token(assertion.review_state),
            "visibility": visibility_token(assertion.visibility),
        });
        memcore::canonical_digest::canonical_json_digest_hex(&basis)
    }

    /// Append many in ONE transaction: either the whole batch lands or none
    /// of it does — a mid-batch failure can never leave an append prefix
    /// visible.
    pub fn append_all(&self, assertions: &[AssertionV1]) -> Result<usize, CurrentTruthStoreError> {
        let transaction = self.conn.unchecked_transaction()?;
        let mut appended = 0;
        for assertion in assertions {
            let outcome = Self::append_in(&transaction, assertion)?;
            if outcome == AppendOutcome::Appended {
                appended += 1;
            }
        }
        transaction.commit()?;
        Ok(appended)
    }

    fn append_in(
        tx: &rusqlite::Transaction<'_>,
        assertion: &AssertionV1,
    ) -> Result<AppendOutcome, CurrentTruthStoreError> {
        Self::validate(assertion)?;
        let value_json =
            serde_json::to_string(&assertion.value).unwrap_or_else(|_| "null".to_string());
        let content_digest = Self::content_digest(assertion);
        let existing: Option<String> = tx
            .query_row(
                "SELECT content_digest FROM current_truth_assertions WHERE assertion_id = ?1",
                params![assertion.assertion_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing_digest) = existing {
            if existing_digest == content_digest {
                return Ok(AppendOutcome::IdempotentDuplicate);
            }
            return Err(CurrentTruthStoreError::ContradictsExistingRevision(
                assertion.ingestion_key(),
            ));
        }
        let evidence_json =
            serde_json::to_string(&assertion.evidence_refs).unwrap_or_else(|_| "[]".to_string());
        let insert = tx.execute(
            "INSERT INTO current_truth_assertions (
                assertion_id, subject_repo, subject_kind, subject_id, predicate,
                value_json, issuer, authority, source_id, source_revision,
                observed_at, effective_at, supersedes, evidence_json,
                review_state, visibility, content_digest, recorded_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, '')",
            params![
                assertion.assertion_id,
                assertion.subject.repo,
                kind_token(&assertion.subject.object),
                id_token(&assertion.subject.object),
                assertion.predicate.as_str(),
                value_json,
                assertion.issuer,
                authority_token(assertion.authority_class),
                assertion.source_ref.source,
                assertion.source_ref.revision,
                assertion.observed_at,
                assertion.effective_at,
                assertion.supersedes_assertion_id,
                evidence_json,
                review_token(assertion.review_state),
                visibility_token(assertion.visibility),
                content_digest,
            ],
        );
        match insert {
            Ok(_) => Ok(AppendOutcome::Appended),
            Err(rusqlite::Error::SqliteFailure(failure, _))
                if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(CurrentTruthStoreError::ContradictsExistingRevision(
                    assertion.ingestion_key(),
                ))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Read every assertion in deterministic (identity-keyed) order — the
    /// same order regardless of arrival. This is the exact input ordering
    /// for both incremental reduction and full rebuild.
    pub fn assertions(&self) -> Result<Vec<AssertionV1>, CurrentTruthStoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT assertion_id, subject_repo, subject_kind, subject_id, predicate,
                    value_json, issuer, authority, source_id, source_revision,
                    observed_at, effective_at, supersedes, evidence_json,
                    review_state, visibility
             FROM current_truth_assertions
             ORDER BY subject_repo, subject_kind, subject_id, predicate,
                      authority, issuer, source_id, source_revision, assertion_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, String>(14)?,
                row.get::<_, String>(15)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (
                assertion_id,
                subject_repo,
                subject_kind,
                subject_id,
                predicate,
                value_json,
                issuer,
                authority,
                source_id,
                source_revision,
                observed_at,
                effective_at,
                supersedes,
                evidence_json,
                review_state,
                visibility,
            ) = row?;
            out.push(decode_assertion_row(
                assertion_id,
                subject_repo,
                subject_kind,
                subject_id,
                predicate,
                value_json,
                issuer,
                authority,
                source_id,
                source_revision,
                observed_at,
                effective_at,
                supersedes,
                evidence_json,
                review_state,
                visibility,
            )?);
        }
        Ok(out)
    }

    /// Assertions for one repository, same deterministic ordering.
    pub fn assertions_for_repo(
        &self,
        repo: &str,
    ) -> Result<Vec<AssertionV1>, CurrentTruthStoreError> {
        Ok(self
            .assertions()?
            .into_iter()
            .filter(|assertion| assertion.subject.repo == repo)
            .collect())
    }

    /// Assertion count for one repository (test/health use).
    pub fn assertion_count(&self, repo: &str) -> Result<usize, CurrentTruthStoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM current_truth_assertions WHERE subject_repo = ?1",
            params![repo],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Write (replace) the disposable projection for one repository.
    /// `generation` binds the projection to the exact assertion set it was
    /// built from; `built_at` is caller-supplied (this module reads no
    /// clock).
    pub fn write_projection(
        &self,
        repo: &str,
        generation: &str,
        built_at: &str,
        view_json: &str,
    ) -> Result<(), CurrentTruthStoreError> {
        self.conn.execute(
            "INSERT INTO current_truth_projection (repo, generation, built_at, view_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repo) DO UPDATE SET
                generation = excluded.generation,
                built_at = excluded.built_at,
                view_json = excluded.view_json",
            params![repo, generation, built_at, view_json],
        )?;
        Ok(())
    }

    /// Read the stored projection for one repository, if present.
    pub fn read_projection(
        &self,
        repo: &str,
    ) -> Result<Option<StoredProjectionV1>, CurrentTruthStoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT generation, built_at, view_json
                 FROM current_truth_projection WHERE repo = ?1",
                params![repo],
                |row| {
                    Ok(StoredProjectionV1 {
                        generation: row.get(0)?,
                        built_at: row.get(1)?,
                        view_json: row.get(2)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Drop the disposable projection for one repository. Never touches the
    /// assertion authority.
    pub fn drop_projection(&self, repo: &str) -> Result<(), CurrentTruthStoreError> {
        self.conn.execute(
            "DELETE FROM current_truth_projection WHERE repo = ?1",
            params![repo],
        )?;
        Ok(())
    }

    /// Record one refresh attempt's operational posture. `recorded_at` is
    /// caller-supplied. Monotonic by attempt instant: an attempt older than
    /// the recorded one is refused — a late, out-of-order refresh record can
    /// never erase a newer posture (a newer outage included).
    pub fn record_refresh(
        &self,
        repo: &str,
        fresh: bool,
        last_fresh_revision: Option<&str>,
        last_fresh_at: Option<&str>,
        recorded_at: &str,
        unavailable_reason: Option<&str>,
    ) -> Result<(), CurrentTruthStoreError> {
        if chrono::DateTime::parse_from_rfc3339(recorded_at).is_err() {
            return Err(CurrentTruthStoreError::MalformedObservedAt(
                recorded_at.to_string(),
            ));
        }
        // One transaction: the staleness check and the upsert commit
        // atomically, so a concurrent writer on the same file cannot
        // interleave an older record over a newer one.
        let transaction = self.conn.unchecked_transaction()?;
        let existing: Option<RefreshPostureRowV1> = transaction
            .query_row(
                "SELECT fresh, last_fresh_revision, last_fresh_at, last_attempt_at,
                        unavailable_reason
                 FROM current_truth_refresh WHERE repo = ?1",
                params![repo],
                |row| {
                    Ok(RefreshPostureRowV1 {
                        fresh: row.get::<_, i64>(0)? != 0,
                        last_fresh_revision: row.get(1)?,
                        last_fresh_at: row.get(2)?,
                        last_attempt_at: row.get(3)?,
                        unavailable_reason: row.get(4)?,
                    })
                },
            )
            .optional()?;
        if let Some(existing) = existing {
            if super::types::ordering_instant(recorded_at)
                < super::types::ordering_instant(&existing.last_attempt_at)
            {
                return Err(CurrentTruthStoreError::StaleRefreshRecord {
                    repo: repo.to_string(),
                    recorded_at: recorded_at.to_string(),
                    existing_attempt_at: existing.last_attempt_at,
                });
            }
        }
        transaction.execute(
            "INSERT INTO current_truth_refresh (
                repo, fresh, last_fresh_revision, last_fresh_at,
                last_attempt_at, unavailable_reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(repo) DO UPDATE SET
                fresh = excluded.fresh,
                last_fresh_revision = COALESCE(excluded.last_fresh_revision,
                                               current_truth_refresh.last_fresh_revision),
                last_fresh_at = COALESCE(excluded.last_fresh_at,
                                         current_truth_refresh.last_fresh_at),
                last_attempt_at = excluded.last_attempt_at,
                unavailable_reason = excluded.unavailable_reason",
            params![
                repo,
                fresh as i64,
                last_fresh_revision,
                last_fresh_at,
                recorded_at,
                unavailable_reason
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Read the recorded refresh posture for one repository, if any.
    pub fn refresh_posture_row(
        &self,
        repo: &str,
    ) -> Result<Option<RefreshPostureRowV1>, CurrentTruthStoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT fresh, last_fresh_revision, last_fresh_at, last_attempt_at,
                        unavailable_reason
                 FROM current_truth_refresh WHERE repo = ?1",
                params![repo],
                |row| {
                    Ok(RefreshPostureRowV1 {
                        fresh: row.get::<_, i64>(0)? != 0,
                        last_fresh_revision: row.get(1)?,
                        last_fresh_at: row.get(2)?,
                        last_attempt_at: row.get(3)?,
                        unavailable_reason: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Refresh-debt count over all repositories (content-free health).
    pub fn refresh_debt_repos(&self) -> Result<usize, CurrentTruthStoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM current_truth_refresh WHERE fresh = 0",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    fn validate(assertion: &AssertionV1) -> Result<(), CurrentTruthStoreError> {
        if assertion.assertion_id.is_empty() {
            return Err(CurrentTruthStoreError::EmptyAssertionId);
        }
        if assertion.subject.repo.split('/').count() != 2
            || assertion.subject.repo.starts_with('/')
            || assertion.subject.repo.ends_with('/')
        {
            return Err(CurrentTruthStoreError::MalformedSubject);
        }
        if assertion.source_ref.revision.is_empty() {
            return Err(CurrentTruthStoreError::EmptySourceRevision);
        }
        if !assertion.predicate.is_source_predicate() {
            return Err(CurrentTruthStoreError::ProjectionPredicateNotAssertable(
                assertion.predicate,
            ));
        }
        // `observed_at` is ordering authority: it must parse as RFC 3339 so
        // the reduction's instant ordering is real, not lexical accident.
        if chrono::DateTime::parse_from_rfc3339(&assertion.observed_at).is_err() {
            return Err(CurrentTruthStoreError::MalformedObservedAt(
                assertion.observed_at.clone(),
            ));
        }
        if !assertion.effective_at.is_empty()
            && chrono::DateTime::parse_from_rfc3339(&assertion.effective_at).is_err()
        {
            return Err(CurrentTruthStoreError::MalformedObservedAt(
                assertion.effective_at.clone(),
            ));
        }
        Ok(())
    }
}

/// A stored (disposable) projection row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredProjectionV1 {
    pub generation: String,
    pub built_at: String,
    pub view_json: String,
}

/// A recorded refresh posture row (operational metadata).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshPostureRowV1 {
    pub fresh: bool,
    pub last_fresh_revision: Option<String>,
    pub last_fresh_at: Option<String>,
    pub last_attempt_at: String,
    pub unavailable_reason: Option<String>,
}

/// The canonical generation digest binding a projection to its exact
/// assertion set: a digest over the sorted assertion ids of the input set.
/// Two projections over the same set share a generation; any append changes
/// it. Pure — usable by both incremental and full-rebuild paths.
pub fn generation_digest(assertions: &[AssertionV1]) -> String {
    let mut pairs: Vec<(&str, String)> = assertions
        .iter()
        .map(|assertion| {
            (
                assertion.assertion_id.as_str(),
                serde_json::to_string(assertion).unwrap_or_default(),
            )
        })
        .collect();
    pairs.sort_unstable();
    let basis = serde_json::json!({
        "assertions": pairs
            .into_iter()
            .map(|(id, content)| serde_json::json!({"id": id, "content": content}))
            .collect::<Vec<_>>(),
    });
    memcore::canonical_digest::canonical_json_digest_hex(&basis)
}

fn kind_token(object: &GithubObjectRefV1) -> &'static str {
    match object {
        GithubObjectRefV1::Issue(_) => "issue",
        GithubObjectRefV1::PullRequest(_) => "pull_request",
        GithubObjectRefV1::Commit(_) => "commit",
    }
}

fn id_token(object: &GithubObjectRefV1) -> String {
    match object {
        GithubObjectRefV1::Issue(number) | GithubObjectRefV1::PullRequest(number) => {
            number.to_string()
        }
        GithubObjectRefV1::Commit(sha) => sha.clone(),
    }
}

fn authority_token(authority: AuthorityClassV1) -> &'static str {
    match authority {
        AuthorityClassV1::GitHubTypedObject => "github_typed_object",
        AuthorityClassV1::OwnerDecision => "owner_decision",
        AuthorityClassV1::ReviewedDisposition => "reviewed_disposition",
        AuthorityClassV1::ModelProse => "model_prose",
    }
}

fn review_token(review: ReviewStateV1) -> &'static str {
    match review {
        ReviewStateV1::Observed => "observed",
        ReviewStateV1::Candidate => "candidate",
        ReviewStateV1::Reviewed => "reviewed",
        ReviewStateV1::Rejected => "rejected",
    }
}

fn visibility_token(visibility: VisibilityClassV1) -> &'static str {
    match visibility {
        VisibilityClassV1::Public => "public",
        VisibilityClassV1::Private => "private",
    }
}

fn parse_authority(token: &str) -> AuthorityClassV1 {
    match token {
        "owner_decision" => AuthorityClassV1::OwnerDecision,
        "reviewed_disposition" => AuthorityClassV1::ReviewedDisposition,
        "model_prose" => AuthorityClassV1::ModelProse,
        _ => AuthorityClassV1::GitHubTypedObject,
    }
}

fn parse_review(token: &str) -> ReviewStateV1 {
    match token {
        "candidate" => ReviewStateV1::Candidate,
        "reviewed" => ReviewStateV1::Reviewed,
        "rejected" => ReviewStateV1::Rejected,
        _ => ReviewStateV1::Observed,
    }
}

fn parse_visibility(token: &str) -> VisibilityClassV1 {
    match token {
        "private" => VisibilityClassV1::Private,
        _ => VisibilityClassV1::Public,
    }
}

fn parse_predicate(token: &str) -> Option<PredicateV1> {
    [
        PredicateV1::IssueOpen,
        PredicateV1::IssueClosed,
        PredicateV1::ImplementationPrLinked,
        PredicateV1::PrOpen,
        PredicateV1::PrMerged,
        PredicateV1::PrClosedUnmerged,
        PredicateV1::MergeReverted,
        PredicateV1::IssueReopened,
        PredicateV1::ImplementationPresent,
        PredicateV1::OwnerAcceptancePresent,
        PredicateV1::HandoffCurrent,
        PredicateV1::HandoffStale,
        PredicateV1::OpenAction,
    ]
    .into_iter()
    .find(|predicate| predicate.as_str() == token)
}

/// Decode one stored row. Unknown closed-vocabulary tokens are **errors**,
/// never silent guesses — a row this store wrote always round-trips, so an
/// undecodable row means the table was written by something else and must
/// surface, not be interpreted (#1297: explicit unknown over fluent
/// narrative).
#[allow(clippy::too_many_arguments)]
fn decode_assertion_row(
    assertion_id: String,
    subject_repo: String,
    subject_kind: String,
    subject_id: String,
    predicate: String,
    value_json: String,
    issuer: String,
    authority: String,
    source_id: String,
    source_revision: String,
    observed_at: String,
    effective_at: String,
    supersedes: Option<String>,
    evidence_json: String,
    review_state: String,
    visibility: String,
) -> Result<AssertionV1, CurrentTruthStoreError> {
    let number = subject_id.parse::<u64>().ok();
    let object = match (subject_kind.as_str(), number) {
        ("issue", Some(number)) => GithubObjectRefV1::Issue(number),
        ("pull_request", Some(number)) => GithubObjectRefV1::PullRequest(number),
        ("commit", None) => GithubObjectRefV1::Commit(subject_id),
        _ => {
            return Err(CurrentTruthStoreError::CorruptRow(format!(
                "subject {subject_kind}/{subject_id}"
            )));
        }
    };
    let Some(predicate) = parse_predicate(&predicate) else {
        return Err(CurrentTruthStoreError::CorruptRow(format!(
            "predicate `{predicate}`"
        )));
    };
    let value = serde_json::from_str::<AssertionValueV1>(&value_json)
        .map_err(|error| CurrentTruthStoreError::CorruptRow(format!("value: {error}")))?;
    let evidence_refs = serde_json::from_str(&evidence_json)
        .map_err(|error| CurrentTruthStoreError::CorruptRow(format!("evidence: {error}")))?;
    if ![
        "github_typed_object",
        "owner_decision",
        "reviewed_disposition",
        "model_prose",
    ]
    .contains(&authority.as_str())
    {
        return Err(CurrentTruthStoreError::CorruptRow(format!(
            "authority `{authority}`"
        )));
    }
    if !["observed", "candidate", "reviewed", "rejected"].contains(&review_state.as_str()) {
        return Err(CurrentTruthStoreError::CorruptRow(format!(
            "review_state `{review_state}`"
        )));
    }
    if !["public", "private"].contains(&visibility.as_str()) {
        return Err(CurrentTruthStoreError::CorruptRow(format!(
            "visibility `{visibility}`"
        )));
    }
    Ok(AssertionV1 {
        assertion_id,
        subject: SubjectRefV1 {
            repo: subject_repo,
            object,
        },
        predicate,
        value,
        issuer,
        authority_class: parse_authority(&authority),
        source_ref: SourceRefV1 {
            source: source_id,
            revision: source_revision,
        },
        observed_at,
        effective_at,
        supersedes_assertion_id: supersedes,
        evidence_refs,
        review_state: parse_review(&review_state),
        visibility: parse_visibility(&visibility),
    })
}

/// Evidence-head helper for store readers that need heads without raw
/// assertions (#1693 consumer boundary).
pub fn heads_of(assertions: &[AssertionV1]) -> Vec<EvidenceHeadV1> {
    assertions.iter().map(EvidenceHeadV1::of).collect()
}
