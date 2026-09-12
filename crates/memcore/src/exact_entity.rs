//! Bounded exact-entity reads over one already-admitted store (#1882).
//! No entity normalization, database routing, role grants or ranking live here.

use std::sync::atomic::Ordering;

use rusqlite::{limits::Limit, params, Connection, Transaction};

use crate::{db, namespace, MemoryEntry, MemoryError, MemoryStore};

/// Host-supplied work ceilings, including the fixed callback ownership probe.
/// VM instructions are charged in approximate 256-instruction quanta; this is
/// not a filesystem/lock-wait or wall-clock deadline.
#[derive(Debug, Clone, Copy)]
pub struct ExactEntityReadBudget {
    pub max_results: usize,
    pub max_vm_steps: u64,
    pub max_request_bytes: usize,
    /// Maximum lightweight candidate rows stepped. Reaching this ceiling is
    /// conservatively exhausted without stepping an extra row to prove EOF.
    pub max_admission_rows: usize,
    pub max_row_bytes: usize,
    pub max_admission_bytes: usize,
    pub max_hydrated_bytes: usize,
}

/// A trusted host constructs this request after store and role admission.
/// `sandbox_rules` must be resolved from that host's canonical authority (the
/// GLOBAL store for Tachi), not copied from model input or the candidate store.
/// It is request-lifetime evidence, not a new policy store. Cross-database rule
/// and candidate snapshots have the same non-atomic limitation as the adapter.
/// No role means the host has selected the existing unscoped read semantics.
/// Store/project/private identity belongs to the admitted `MemoryStore`; this
/// request cannot supply a path to another database or broaden that handle.
/// This is a host API, not an untrusted tool-argument deserialization surface.
pub struct ExactEntityReadRequest<'a> {
    /// Expected canonical role/profile, checked against this admitted handle.
    pub expected_store: &'a db::StoreIdentity,
    /// Optional stronger file identity expectation; never reopens the path.
    pub expected_physical_db_identity: Option<&'a str>,
    /// None explicitly requires a public handle, not any partition.
    pub expected_private_partition: Option<&'a crate::private_partition::AdmittedPartition>,
    pub exact_aliases: &'a [String],
    pub path_prefix: Option<&'a str>,
    pub domain: Option<&'a str>,
    pub surface: Option<namespace::Surface>,
    pub agent_role: Option<&'a str>,
    pub sandbox_rules: &'a [(String, String)],
    pub as_of: Option<&'a str>,
    pub include_archived: bool,
    pub budget: ExactEntityReadBudget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactEntityReadStatus {
    Complete,
    ResultLimit,
    VmBudget,
    AdmissionRows,
    RowBytes,
    AdmissionBytes,
    HydratedBytes,
    CallbackUnavailable,
    NestedRead,
}

#[derive(Debug)]
pub struct ExactEntityReadResult {
    pub entries: Vec<MemoryEntry>,
    pub status: ExactEntityReadStatus,
    /// Approximate instruction quanta, including the ownership probe.
    pub charged_vm_steps: u64,
    pub admission_rows: usize,
    pub admission_bytes: usize,
    pub hydrated_bytes: usize,
}

struct QueryGuard<'a> {
    conn: &'a Connection,
    state: &'a db::ReservedReferenceWriteFlag,
    previous_length: i32,
}

impl Drop for QueryGuard<'_> {
    fn drop(&mut self) {
        self.state.exact_active.store(false, Ordering::SeqCst);
        self.state.exact_remaining.store(0, Ordering::SeqCst);
        self.state.exact_exhausted.store(false, Ordering::SeqCst);
        // Restoring a previously successful limit on the same live connection
        // is valid. No callback setter is called: a foreign hook stays foreign.
        let _ = self
            .conn
            .set_limit(Limit::SQLITE_LIMIT_LENGTH, self.previous_length);
    }
}

// Disarm only instrumentation before rollback: an exhausted hook must not
// interrupt cleanup of the snapshot this read created. The exhaustion witness
// remains intact until the outer QueryGuard classifies the outcome.
struct ReadSnapshot<'a> {
    transaction: Option<Transaction<'a>>,
    state: &'a db::ReservedReferenceWriteFlag,
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        self.state.exact_active.store(false, Ordering::SeqCst);
        drop(self.transaction.take());
    }
}

fn invalid(message: &str) -> MemoryError {
    MemoryError::InvalidArg(format!("exact entity read: {message}"))
}

fn charge(used: &mut usize, amount: usize, maximum: usize) -> bool {
    match used.checked_add(amount) {
        Some(next) if next <= maximum => {
            *used = next;
            true
        }
        _ => false,
    }
}

fn validate(request: &ExactEntityReadRequest<'_>) -> Result<(), MemoryError> {
    let b = request.budget;
    if [
        b.max_results,
        b.max_request_bytes,
        b.max_admission_rows,
        b.max_row_bytes,
        b.max_admission_bytes,
        b.max_hydrated_bytes,
    ]
    .contains(&0)
        || b.max_vm_steps == 0
        || b.max_vm_steps > u64::MAX - db::EXACT_PROGRESS_INTERVAL
        || b.max_row_bytes > i32::MAX as usize
        || b.max_results.checked_add(1).is_none()
    {
        return Err(invalid("zero or overflowing budget"));
    }
    if request.exact_aliases.is_empty()
        || request.exact_aliases.len() > b.max_request_bytes
        || request.sandbox_rules.len() > b.max_request_bytes
    {
        return Err(invalid("empty identities or excessive request cardinality"));
    }
    let mut bytes = 0;
    if request.expected_store.db_label.trim().is_empty()
        || !charge(
            &mut bytes,
            request.expected_store.db_label.len(),
            b.max_request_bytes,
        )
    {
        return Err(invalid(
            "missing store identity or request byte budget exceeded",
        ));
    }
    for identity in [
        request.expected_physical_db_identity,
        request
            .expected_private_partition
            .map(|partition| partition.partition_id.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        if identity.is_empty() || !charge(&mut bytes, identity.len(), b.max_request_bytes) {
            return Err(invalid("empty identity or request byte budget exceeded"));
        }
    }
    for alias in request.exact_aliases {
        if alias.trim().is_empty() || !charge(&mut bytes, alias.len(), b.max_request_bytes) {
            return Err(invalid("empty identity or request byte budget exceeded"));
        }
    }
    for value in [
        request.path_prefix,
        request.domain,
        request.agent_role,
        request.as_of,
    ]
    .into_iter()
    .flatten()
    {
        if value.trim().is_empty() || !charge(&mut bytes, value.len(), b.max_request_bytes) {
            return Err(invalid("empty selector or request byte budget exceeded"));
        }
    }
    if request
        .path_prefix
        .is_some_and(|path| !path.starts_with('/'))
    {
        return Err(invalid("path prefix must be absolute"));
    }
    if request.agent_role.is_none() && !request.sandbox_rules.is_empty() {
        return Err(invalid("sandbox rules require a governed role"));
    }
    for (pattern, level) in request.sandbox_rules {
        if !charge(&mut bytes, pattern.len(), b.max_request_bytes)
            || !charge(&mut bytes, level.len(), b.max_request_bytes)
        {
            return Err(invalid("governed rule byte budget exceeded"));
        }
    }
    Ok(())
}

impl MemoryStore {
    /// Read exact structural entity aliases without opening another database,
    /// granting role/private access, recording use, or changing search ranking.
    /// Only complete admission is ordered/capped; interrupted work returns no
    /// rows. A result limit is explicit and never represented as completeness.
    pub fn read_exact_entities(
        &self,
        request: ExactEntityReadRequest<'_>,
    ) -> Result<ExactEntityReadResult, MemoryError> {
        validate(&request)?;
        if request.expected_store.db_label != self.db_label()
            || request.expected_store.profile != self.store_profile()
            || request
                .expected_physical_db_identity
                .is_some_and(|identity| Some(identity) != self.opened_physical_db_identity())
            || request.expected_private_partition != self.admitted_partition.as_ref()
        {
            return Err(invalid("admitted store identity mismatch"));
        }
        let mut result = ExactEntityReadResult {
            entries: Vec::new(),
            status: ExactEntityReadStatus::Complete,
            charged_vm_steps: 0,
            admission_rows: 0,
            admission_bytes: 0,
            hydrated_bytes: 0,
        };
        let state = &self.reserved_reference_write;
        if state
            .exact_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            result.status = ExactEntityReadStatus::NestedRead;
            return Ok(result);
        }
        state
            .exact_remaining
            .store(request.budget.max_vm_steps, Ordering::SeqCst);
        state.exact_exhausted.store(false, Ordering::SeqCst);
        let previous_length = match self.conn.limit(Limit::SQLITE_LIMIT_LENGTH) {
            Ok(limit) => limit,
            Err(error) => {
                state.exact_active.store(false, Ordering::SeqCst);
                state.exact_remaining.store(0, Ordering::SeqCst);
                return Err(error.into());
            }
        };
        let guard = QueryGuard {
            conn: &self.conn,
            state,
            previous_length,
        };
        let witness = state.exact_witness.load(Ordering::SeqCst);
        let operation = (|| -> Result<(), MemoryError> {
            // Fixed, database-independent work. The interval is approximate;
            // this probe and its charge are pinned by callback discriminators.
            self.conn.query_row(
                "WITH RECURSIVE probe(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM probe WHERE n<128) SELECT max(n) FROM probe",
                [], |row| row.get::<_, i64>(0))?;
            if state.exact_witness.load(Ordering::SeqCst) == witness {
                result.status = ExactEntityReadStatus::CallbackUnavailable;
                return Ok(());
            }
            // Never raise an inherited connection ceiling. CASE projections
            // below avoid fetching an oversized field before its byte check.
            self.conn.set_limit(
                Limit::SQLITE_LIMIT_LENGTH,
                previous_length.min(request.budget.max_row_bytes as i32),
            )?;
            let at = match request.as_of {
                Some(at) => db::normalize_sqlite_as_of(&self.conn, at)?,
                None => db::now_utc_iso(),
            };
            // A caller-owned transaction is retained; otherwise this guard pins
            // one snapshot for admission and hydration and rolls back on exit.
            let _snapshot = ReadSnapshot {
                transaction: if self.conn.is_autocommit() {
                    Some(self.conn.unchecked_transaction()?)
                } else {
                    None
                },
                state,
            };
            self.exact_entity_rows(&request, &at, &mut result)
        })();
        result.charged_vm_steps = state
            .exact_witness
            .load(Ordering::SeqCst)
            .wrapping_sub(witness)
            .saturating_mul(db::EXACT_PROGRESS_INTERVAL);
        if state.exact_exhausted.load(Ordering::SeqCst) {
            result.entries.clear();
            result.status = ExactEntityReadStatus::VmBudget;
        } else if let Err(error) = operation {
            // A length error is local to the scoped byte ceiling. All other
            // SQLite errors, including an unrelated interrupt, remain errors.
            if matches!(&error, MemoryError::Sqlite(e) if e.sqlite_error_code() == Some(rusqlite::ErrorCode::TooBig))
            {
                result.entries.clear();
                result.status = ExactEntityReadStatus::RowBytes;
            } else {
                return Err(error);
            }
        }
        drop(guard);
        Ok(result)
    }

    fn exact_entity_rows(
        &self,
        request: &ExactEntityReadRequest<'_>,
        at: &str,
        result: &mut ExactEntityReadResult,
    ) -> Result<(), MemoryError> {
        let b = request.budget;
        // No candidate LIMIT. Domain/archive filtering is SQL-side; path/role
        // admission follows on bounded id/path before any entity/metadata parse.
        let mut statement = self.conn.prepare(
            "SELECT rowid, octet_length(id)+octet_length(path),
             CASE WHEN octet_length(id)+octet_length(path)<=?1 THEN id END,
             CASE WHEN octet_length(id)+octet_length(path)<=?1 THEN path END
             FROM memories WHERE (?2 IS NULL OR domain=?2) AND (?3 OR archived=0)",
        )?;
        let mut rows = statement.query(params![
            b.max_row_bytes as i64,
            request.domain,
            request.include_archived
        ])?;
        let mut admitted = Vec::new();
        loop {
            if result.admission_rows == b.max_admission_rows {
                result.status = ExactEntityReadStatus::AdmissionRows;
                return Ok(());
            }
            let Some(row) = rows.next()? else {
                break;
            };
            result.admission_rows += 1;
            let bytes: usize = row.get(1)?;
            if bytes > b.max_row_bytes {
                result.status = ExactEntityReadStatus::RowBytes;
                return Ok(());
            }
            if !charge(&mut result.admission_bytes, bytes, b.max_admission_bytes) {
                result.status = ExactEntityReadStatus::AdmissionBytes;
                return Ok(());
            }
            let id: String = row.get(2)?;
            let path: String = row.get(3)?;
            if let Some(prefix) = request.path_prefix {
                let prefix = prefix.trim_end_matches('/');
                if !prefix.is_empty()
                    && path != prefix
                    && !path
                        .strip_prefix(prefix)
                        .is_some_and(|tail| tail.starts_with('/'))
                {
                    continue;
                }
            }
            if let Some(role) = request.agent_role {
                if !db::evaluate_sandbox_access(request.sandbox_rules, role, &path, "read").0 {
                    continue;
                }
            }
            let rowid: i64 = row.get(0)?;
            if self
                .exact_admission_projection(rowid, request, at, result)?
                .is_some()
            {
                admitted.push((id, rowid));
            }
            if result.status != ExactEntityReadStatus::Complete {
                return Ok(());
            }
        }
        // Ordering is evidence-neutral, not product ranking. No denied or
        // lexical-only row consumes a result slot.
        admitted.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        if admitted.len() > b.max_results {
            result.status = ExactEntityReadStatus::ResultLimit;
            admitted.truncate(b.max_results);
        }
        for (_, rowid) in admitted {
            // Account all text/JSON columns before the existing full renderer.
            let lengths = "COALESCE(octet_length(id),0)+COALESCE(octet_length(path),0)+COALESCE(octet_length(summary),0)+COALESCE(octet_length(text),0)+COALESCE(octet_length(timestamp),0)+COALESCE(octet_length(valid_from),0)+COALESCE(octet_length(valid_until),0)+COALESCE(octet_length(category),0)+COALESCE(octet_length(topic),0)+COALESCE(octet_length(keywords),0)+COALESCE(octet_length(entities),0)+COALESCE(octet_length(source),0)+COALESCE(octet_length(scope),0)+COALESCE(octet_length(metadata),0)+COALESCE(octet_length(retention_policy),0)+COALESCE(octet_length(domain),0)+COALESCE(octet_length(last_access),0)+COALESCE(octet_length(last_use_at),0)+COALESCE(octet_length(tier),0)+COALESCE(octet_length(superseded_by),0)";
            let bytes: usize = self.conn.query_row(
                &format!("SELECT {lengths} FROM memories WHERE rowid=?1"),
                [rowid],
                |r| r.get(0),
            )?;
            if bytes > b.max_row_bytes {
                result.entries.clear();
                result.status = ExactEntityReadStatus::RowBytes;
                return Ok(());
            }
            if !charge(&mut result.hydrated_bytes, bytes, b.max_hydrated_bytes) {
                result.entries.clear();
                result.status = ExactEntityReadStatus::HydratedBytes;
                return Ok(());
            }
            let entry = self.conn.query_row(
                &format!(
                    "SELECT {} FROM memories WHERE rowid=?1",
                    db::MEMORY_SELECT_COLUMNS
                ),
                [rowid],
                db::row_to_entry,
            )?;
            result.entries.push(entry);
        }
        Ok(())
    }

    fn exact_admission_projection(
        &self,
        rowid: i64,
        request: &ExactEntityReadRequest<'_>,
        at: &str,
        result: &mut ExactEntityReadResult,
    ) -> Result<Option<()>, MemoryError> {
        let columns = [
            "entities",
            "metadata",
            "timestamp",
            "valid_from",
            "valid_until",
            "superseded_by",
            "category",
            "source",
            "topic",
            "domain",
            "id",
            "path",
        ];
        let lengths = columns
            .iter()
            .map(|c| format!("COALESCE(octet_length({c}),0)"))
            .collect::<Vec<_>>()
            .join("+");
        let projections = columns
            .iter()
            .map(|c| format!("CASE WHEN ({lengths})<=?2 THEN {c} END"))
            .collect::<Vec<_>>()
            .join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {lengths},{projections} FROM memories WHERE rowid=?1"
        ))?;
        let mut rows = stmt.query(params![rowid, request.budget.max_row_bytes as i64])?;
        let row = rows
            .next()?
            .ok_or_else(|| invalid("candidate disappeared within read snapshot"))?;
        let bytes: usize = row.get(0)?;
        if bytes > request.budget.max_row_bytes {
            result.status = ExactEntityReadStatus::RowBytes;
            return Ok(None);
        }
        if !charge(
            &mut result.admission_bytes,
            bytes,
            request.budget.max_admission_bytes,
        ) {
            result.status = ExactEntityReadStatus::AdmissionBytes;
            return Ok(None);
        }
        let entities: String = row.get(1)?;
        let entities: Vec<String> =
            serde_json::from_str(&entities).map_err(|_| invalid("malformed entity metadata"))?;
        if !entities.iter().any(|e| request.exact_aliases.contains(e)) {
            return Ok(None);
        }
        let metadata: String = row.get(2)?;
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata).map_err(|_| invalid("malformed namespace metadata"))?;
        if !metadata.is_object() {
            return Err(invalid("namespace metadata must be an object"));
        }
        let timestamp: String = row.get(3)?;
        let from: String = row.get(4)?;
        let until: Option<String> = row.get(5)?;
        let superseded: Option<String> = row.get(6)?;
        let from = db::normalize_utc_iso(if from.trim().is_empty() {
            &timestamp
        } else {
            &from
        })?;
        let until = until.as_deref().map(db::normalize_utc_iso).transpose()?;
        if until.as_ref().is_some_and(|end| end <= &from) {
            return Err(invalid("invalid half-open validity window"));
        }
        if superseded.is_some() && until.is_none() {
            return Err(invalid("superseded row lacks historical validity boundary"));
        }
        if from.as_str() > at
            || until.as_deref().is_some_and(|end| at >= end)
            || (request.as_of.is_none() && superseded.is_some())
        {
            return Ok(None);
        }
        // Only bounded classification fields, never content, reach canonical
        // namespace functions. This ephemeral projection creates no new facts.
        let id: String = row.get(11)?;
        let path: String = row.get(12)?;
        let category: String = row.get(7)?;
        let source: String = row.get(8)?;
        let topic: String = row.get(9)?;
        let domain: Option<String> = row.get(10)?;
        let projection = namespace::NamespaceProjection {
            id: &id,
            path: &path,
            category: &category,
            source: &source,
            topic: &topic,
            domain: domain.as_deref(),
            metadata: &metadata,
        };
        if namespace::is_namespace_search_noise_projection(&projection, request.path_prefix)
            || namespace::is_non_default_retrievable_wiki_row_projection(&projection)
            || request
                .surface
                .is_some_and(|surface| namespace::surface_of_projection(&projection) != surface)
            || (self.is_wiki_corpus_store()
                && !namespace::is_user_facing_wiki_entry_allowing_recall_cache_projection(
                    &projection,
                ))
        {
            return Ok(None);
        }
        Ok(Some(()))
    }
}

#[cfg(test)]
mod tests;
