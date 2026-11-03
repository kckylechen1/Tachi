//! Strict revisions use the existing transaction/receipt boundary, with an
//! append-only lineage ledger instead of mutating the predecessor row.

use super::*;
use super::receipts::persist_supersession_receipt;
use crate::typed_records::{invalid, RECORD_KINDS_NAMESPACE};
use crate::{
    canonical_json, canonical_json_digest_hex, RecordKindId, RecordRegistry, RecordWriter,
    TypedRecord, TypedRecordInput, TypedRecordTarget, TypedRecordWrite,
};

const ROUTE: &str = "typed_record_revision_v1";

fn read_record(conn: &Connection, id: &str) -> Result<Option<TypedRecord>, MemoryError> {
    let row = conn.query_row(
        "SELECT r.id,r.kind,r.kind_version,r.keys_json,r.payload_json,r.links_json,
                r.revision,r.content_sha256,r.writer,r.created_at,s.target_id
         FROM typed_records r LEFT JOIN typed_record_supersessions s ON s.source_id=r.id
         WHERE r.id=?1",
        [id],
        |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, u32>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?, row.get::<_, String>(7)?, row.get::<_, String>(8)?,
            row.get::<_, String>(9)?, row.get::<_, Option<String>>(10)?,
        )),
    ).optional()?;
    row.map(|row| {
        let record = TypedRecord {
            id: row.0, kind: RecordKindId { name: row.1, version: row.2 },
            keys: serde_json::from_str(&row.3)?, payload: serde_json::from_str(&row.4)?,
            links: serde_json::from_str(&row.5)?, revision: row.6, content_sha256: row.7,
            writer: row.8, created_at: row.9, superseded_by: row.10,
        };
        let input = TypedRecordInput {
            id: record.id.clone(), kind: record.kind.clone(), keys: record.keys.clone(),
            payload: record.payload.clone(), links: record.links.clone(),
        };
        if canonical_json_digest_hex(&serde_json::to_value(input)?) != record.content_sha256 {
            return Err(invalid("typed record content digest does not match its immutable identity"));
        }
        Ok(record)
    }).transpose()
}

fn freeze_registry(conn: &Connection, registry: &RecordRegistry) -> Result<(), MemoryError> {
    for definition in registry.definitions() {
        let key = definition.id.stamp_key();
        let value = serde_json::json!({
            "contract": "typed_record_kind.v1", "append_only": true,
            "draft": "2020-12", "definition": definition,
        });
        let value = canonical_json(&value).to_string();
        let existing = db::get_state(conn, RECORD_KINDS_NAMESPACE, &key)?;
        if let Some((stored, version)) = existing {
            if stored != value || version != 1 {
                return Err(invalid(format!("record kind definition conflict for {key}")));
            }
        } else {
            let now = db::now_utc_iso();
            conn.execute(
                "INSERT INTO hard_state(namespace,key,value_json,version,created_at,updated_at)
                 VALUES(?1,?2,?3,1,?4,?4)",
                params![RECORD_KINDS_NAMESPACE, key, value, now],
            )?;
        }
    }
    Ok(())
}

impl ImmutableSupersessionTransaction<'_> {
    fn append_record(
        &mut self,
        registry: &RecordRegistry,
        writer: &RecordWriter,
        input: &TypedRecordInput,
        predecessor: Option<&str>,
        role: &str,
    ) -> Result<TypedRecordWrite, MemoryError> {
        if db::store_identity::store_role_stamp(&self.tx)?.as_deref() != Some(role) {
            return Err(invalid("typed store role changed after handle admission"));
        }
        writer.require(role, &input.kind)?;
        registry.validate_input(input)?;
        let digest = canonical_json_digest_hex(&serde_json::to_value(input)?);
        let existing = read_record(&self.tx, &input.id)?;
        let source = predecessor.map(|id| {
            if id == input.id { return Err(invalid("a record cannot supersede itself")); }
            let source = read_record(&self.tx, id)?.ok_or_else(|| invalid("record predecessor is missing"))?;
            writer.require(role, &source.kind)?;
            if source.kind.name != input.kind.name {
                return Err(invalid("a record cannot supersede a different kind"));
            }
            Ok(source)
        }).transpose()?;
        if let Some(existing) = existing {
            let actual_predecessor: Option<String> = self.tx.query_row(
                "SELECT source_id FROM typed_record_supersessions WHERE target_id=?1",
                [&input.id], |row| row.get(0),
            ).optional()?;
            if existing.content_sha256 != digest || existing.writer != writer.actor
                || actual_predecessor.as_deref() != predecessor
            {
                return Err(invalid("record idempotency conflict: id already names different content, writer or lineage"));
            }
            let receipt = match source {
                Some(source) => {
                    let id = SupersessionReceipt::id_for(ROUTE, ROUTE, &source.id, &input.id);
                    let receipt = load_supersession_receipt(&self.tx, &id)?
                        .ok_or_else(|| invalid("typed lineage has no durable supersession receipt"))?;
                    if receipt.route != ROUTE || receipt.source_id != source.id || receipt.target_id != input.id {
                        return Err(invalid("typed supersession receipt identity conflict"));
                    }
                    Some(receipt)
                }
                None => None,
            };
            // Reopening with changed policy must also reject an otherwise exact replay.
            let _authorization = db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
            freeze_registry(&self.tx, registry)?;
            return Ok(TypedRecordWrite { record: existing, prior_idempotent: true, receipt });
        }
        if source.as_ref().is_some_and(|record| record.superseded_by.is_some()) {
            return Err(invalid("record predecessor already has a successor"));
        }
        for link in &input.links {
            if let TypedRecordTarget::Local { record_id, kind, keys, content_sha256 } = &link.target {
                let target = read_record(&self.tx, record_id)?
                    .ok_or_else(|| invalid("local typed reference target is missing"))?;
                if target.kind != *kind || canonical_json(&target.keys) != canonical_json(keys)
                    || target.content_sha256 != *content_sha256
                {
                    return Err(invalid("local typed reference identity or digest mismatch"));
                }
            }
        }
        let revision = match &source {
            Some(source) => source.revision.checked_add(1).ok_or_else(|| invalid("typed revision overflow"))?,
            None => 1,
        };
        let created_at = db::now_utc_iso();
        let _authorization = db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        freeze_registry(&self.tx, registry)?;
        self.tx.execute(
            "INSERT INTO typed_records(id,kind,kind_version,keys_json,payload_json,links_json,
                                       revision,content_sha256,writer,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![input.id, input.kind.name, input.kind.version,
                canonical_json(&input.keys).to_string(), canonical_json(&input.payload).to_string(),
                canonical_json(&serde_json::to_value(&input.links)?).to_string(),
                revision, digest, writer.actor, created_at],
        )?;
        let receipt = if let Some(source) = source {
            let receipt_id = SupersessionReceipt::id_for(ROUTE, ROUTE, &source.id, &input.id);
            self.tx.execute(
                "INSERT INTO typed_record_supersessions(source_id,target_id,receipt_id,created_at)
                 VALUES(?1,?2,?3,?4)",
                params![source.id, input.id, receipt_id, created_at],
            )?;
            let mut receipt = SupersessionReceipt {
                receipt_id, source_id: source.id.clone(), target_id: input.id.clone(),
                source_revision_before: source.revision, source_revision_after: source.revision,
                target_revision_before: None, target_revision_after: Some(revision),
                source_archived_before: false, source_archived_after: false,
                source_superseded_by_before: None, source_superseded_by_after: Some(input.id.clone()),
                source_valid_until_before: None, source_valid_until_after: None,
                source_path_before: format!("typed-record/{}", source.kind.stamp_key()),
                source_text_digest_before: canonical_json_digest_hex(&source.payload),
                target_path_before: None, target_text_digest_before: None,
                partition_id: self.partition_id.clone(), route: ROUTE.into(), policy_version: ROUTE.into(),
                dependent_write_disposition: "typed_revision_and_lineage_appended".into(),
                commit_result: SupersessionCommitResult::Applied, durable: false,
            };
            persist_supersession_receipt(&self.tx, &mut receipt)?;
            Some(receipt)
        } else { None };
        let record = read_record(&self.tx, &input.id)?.ok_or_else(|| invalid("inserted record disappeared"))?;
        Ok(TypedRecordWrite { record, prior_idempotent: false, receipt })
    }
}

impl MemoryStore {
    /// Freeze each host kind/version's entire schema contract. A later host
    /// may add versions, but cannot reinterpret a previously admitted version.
    pub fn register_record_kinds(&mut self) -> Result<(), MemoryError> {
        let registry = self.policy.record_registry.clone().ok_or_else(|| invalid("host record registry is absent"))?;
        self.with_immutable_supersession_transaction(|operation| {
            let _authorization = db::authorize_reserved_reference_write(&operation.reserved_reference_write)?;
            freeze_registry(&operation.tx, &registry)
        })
    }

    /// Append one immutable revision, optionally replacing exactly one current
    /// predecessor. Revision, lineage and receipt commit or roll back together.
    pub fn append_typed_record(
        &mut self,
        writer: &RecordWriter,
        input: &TypedRecordInput,
        predecessor: Option<&str>,
    ) -> Result<TypedRecordWrite, MemoryError> {
        let registry = self.policy.record_registry.clone().ok_or_else(|| invalid("host record registry is absent"))?;
        let role = db::store_identity::store_role_stamp(&self.conn)?
            .ok_or_else(|| invalid("typed records require a persisted store role"))?;
        if role != self.db_label { return Err(invalid("typed store role and admitted handle disagree")); }
        self.with_immutable_supersession_transaction(|operation| {
            operation.append_record(&registry, writer, input, predecessor, &role)
        })
    }

    /// Read the immutable revision and its current lineage without a write.
    pub fn get_typed_record(&self, id: &str) -> Result<Option<TypedRecord>, MemoryError> {
        read_record(&self.conn, id)
    }
}
