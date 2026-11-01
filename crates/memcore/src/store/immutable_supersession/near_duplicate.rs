//! Id-less near-duplicate writes through the canonical supersession claim.

use super::*;

impl ImmutableSupersessionTransaction<'_> {
    /// Persist an id-less save and, only for its explicit near-duplicate policy,
    /// route the semantic loser through the canonical supersession claim. The
    /// ordinary upsert remains non-semantic; this method is the only transaction
    /// seam that opts into the merge policy.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_idless_with_near_duplicate_merge(
        &mut self,
        entry: &MemoryEntry,
        idless_identity: &str,
        metadata_patch: &Map<String, Value>,
        metadata_removals: &[&str],
        mutations: &[db::ValidatedReferenceMutation],
    ) -> Result<(db::IdlessUpsertResult, Value), MemoryError> {
        let (result, metadata) = self
            .upsert_with_validated_reference_mutations_and_metadata_removals(
                entry,
                Some(idless_identity),
                metadata_patch,
                metadata_removals,
                mutations,
                db::NearDuplicatePolicy::NonSemantic,
            )?;
        if !matches!(result, db::IdlessUpsertResult::Saved) {
            return Ok((result, metadata));
        }

        let Some(candidate_id) = db::find_jaccard_candidate_within_tx(&self.tx, entry)? else {
            return Ok((result, metadata));
        };
        let source = self
            .get_memory(&entry.id)?
            .ok_or_else(|| MemoryError::NotFound(entry.id.clone()))?;
        // Near-duplicate matching is heuristic, so it cannot create authority
        // to retire a lifecycle-protected row. The id-less save itself has
        // already landed atomically above; keep both rows active when the new
        // row is protected instead of turning an optional dedup decision into
        // a failed save (for example, append-only precedent candidates).
        if crate::store::memory_lifecycle::lifecycle_protection_reason(&source).is_some() {
            return Ok((result, metadata));
        }
        let target = self
            .get_memory(&candidate_id)?
            .ok_or_else(|| MemoryError::NotFound(candidate_id.clone()))?;
        let expected = SupersessionExpectedState::active_unsuperseded(&source, Some(&target));
        let mutation_timestamp = db::now_utc_iso();
        if db::jaccard_supersession_valid_until(&source, &mutation_timestamp)?.is_none() {
            return Ok((result, metadata));
        }
        let claim = self.claim_checked_immutable_supersession_at(
            &source.id,
            &target.id,
            &expected,
            SUPERSESSION_ROUTE_IDLESS_NEAR_DUPLICATE,
            SUPERSESSION_POLICY_IDLESS_NEAR_DUPLICATE,
            true,
            &mutation_timestamp,
        )?;
        if claim.result == SupersessionCommitResult::Applied {
            self.add_canonical_supersession_edge(
                &MemoryEdge {
                    source_id: target.id.clone(),
                    target_id: source.id.clone(),
                    relation: "supersedes".to_string(),
                    weight: 1.0,
                    metadata: serde_json::json!({
                        "source": "idless_near_duplicate",
                        "route": SUPERSESSION_ROUTE_IDLESS_NEAR_DUPLICATE,
                        "policy_version": SUPERSESSION_POLICY_IDLESS_NEAR_DUPLICATE,
                    }),
                    created_at: mutation_timestamp.clone(),
                    valid_from: mutation_timestamp.clone(),
                    valid_to: None,
                },
                &db::EdgeProvenance {
                    capture_event_kind: SUPERSESSION_ROUTE_IDLESS_NEAR_DUPLICATE.to_string(),
                    actor: "memcore".to_string(),
                    reason_code: "near_duplicate_supersession".to_string(),
                    authority: Some(db::EdgeAuthority::StructuralBookkeeping),
                    ..Default::default()
                },
            )?;
            let _authorization =
                db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
            db::merge_jaccard_candidate_within_tx(
                &self.tx,
                entry,
                entry.importance,
                &db::now_utc_iso(),
                &candidate_id,
            )?;
            let changed = self.tx.execute(
                "UPDATE memories SET idless_identity = NULL
                 WHERE id = ?1 AND superseded_by = ?2",
                params![&source.id, &candidate_id],
            )?;
            if changed != 1 {
                return Err(MemoryError::InvalidArg(format!(
                    "id-less supersession did not release loser identity: {}",
                    source.id
                )));
            }
        }
        Ok((result, metadata))
    }
}
