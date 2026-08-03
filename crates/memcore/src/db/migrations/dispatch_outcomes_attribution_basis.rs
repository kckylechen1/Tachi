//! v17 (#1065 option D) — add and back-fill the frozen
//! `identity_attribution_basis` column on `dispatch_outcomes`.
//!
//! The basis names the evidence class of the flat identity columns
//! (model/vendor/role/seat) so readers can tell planned routing intent from
//! carrier-observed execution fact. Back-fill derives it from each row's
//! frozen receipt:
//!
//! - receipt STRUCTURALLY VALID (deserializes into [`ReceiptSkeleton`]) with
//!   `observed.acknowledgement: "unconfirmed"` → `planned_unconfirmed`
//! - `"acknowledged"` → `acknowledged_overlay`
//! - `"substituted"` / `"ignored"` → `observed`
//! - receipt present but structurally invalid OR unparseable JSON → `unknown`
//!   — this holds EVEN IF a legal acknowledgement token appears somewhere in
//!   the malformed JSON (codex round-2 finding, #1065 D BUG-1): the runtime
//!   (`tachi_dispatch::load_dispatch_identity_receipt_checked`) rejects any
//!   receipt that doesn't deserialize as `Corrupt`, attributing `unknown`. A
//!   loose `.pointer("/observed/acknowledgement")` probe on raw JSON would
//!   happily read a legal token out of a shape the runtime would reject,
//!   permanently back-filling a receipt the runtime itself calls
//!   unattributable into a spuriously attributable basis. Full-shape
//!   deserialization keeps this migration's verdict on a receipt identical to
//!   the runtime's verdict on the SAME receipt.
//! - no receipt, but the row carries real attribution → `fallback_unreceipted`
//!   (it was reconstructed from profile/agent at write time)
//! - no receipt and no attribution (`vendor='unknown'`, model NULL) → `unknown`

use rusqlite::Connection;
use serde::Deserialize;

use crate::db::StoreProfile;
use crate::error::MemoryError;

use super::dispatch_outcomes_reported::table_exists;
use super::legacy_columns::table_has_column;

/// v17-time-frozen mirror of `tachi_dispatch::DispatchIdentityRequest`. This
/// migration cannot depend on `tachi-dispatch` (memcore sits below it in the
/// dependency graph), so the shape is deliberately duplicated here rather
/// than imported — a divergence between this skeleton and the runtime type
/// is a real, backfill-only-affecting bug, but coupling memcore to
/// tachi-dispatch to avoid it would be the wrong trade (a migration's
/// contract is frozen at the schema version it shipped in; the LIVE type is
/// free to keep evolving in tachi-dispatch without rewriting frozen
/// migrations).
#[derive(Debug, Deserialize)]
struct RequestedSkeleton {
    #[allow(dead_code)]
    profile: Option<String>,
    #[allow(dead_code)]
    model: Option<String>,
    #[allow(dead_code)]
    agent: Option<String>,
    #[allow(dead_code)]
    harness: Option<String>,
}

/// v17-time-frozen mirror of `tachi_dispatch::DispatchIdentityEffective`
/// (13 fields — see that type's doc for what each one means; this skeleton
/// only needs to validate SHAPE, not interpret the values, so every field is
/// `#[allow(dead_code)]` after deserialization proves the receipt is
/// well-formed).
#[derive(Debug, Deserialize)]
struct EffectiveSkeleton {
    #[allow(dead_code)]
    profile: Option<String>,
    #[allow(dead_code)]
    model: Option<String>,
    #[allow(dead_code)]
    backend: String,
    #[allow(dead_code)]
    harness: String,
    #[allow(dead_code)]
    model_lineage_id: String,
    #[allow(dead_code)]
    concrete_model_release: String,
    #[allow(dead_code)]
    provider_model: String,
    #[allow(dead_code)]
    provider_model_version: String,
    #[allow(dead_code)]
    role: String,
    #[allow(dead_code)]
    seat: String,
    #[allow(dead_code)]
    transport: String,
    #[allow(dead_code)]
    adapter_version: String,
    #[allow(dead_code)]
    carrier_version: String,
}

/// v17-time-frozen mirror of `tachi_dispatch::DispatchIdentityObserved`.
/// `acknowledgement` stays a plain `String` here (not a closed enum) — the
/// vocabulary check happens explicitly in the back-fill match below so an
/// unrecognized token is a deliberate `unknown`, not a deserialize failure
/// that would (incorrectly) also reject an otherwise well-formed receipt.
#[derive(Debug, Deserialize)]
struct ObservedSkeleton {
    acknowledgement: String,
    #[allow(dead_code)]
    effective: EffectiveSkeleton,
    #[allow(dead_code)]
    mismatch: bool,
    #[allow(dead_code)]
    resolution_reason: String,
}

/// v17-time-frozen mirror of `tachi_dispatch::DispatchIdentityReceipt`.
#[derive(Debug, Deserialize)]
struct ReceiptSkeleton {
    #[allow(dead_code)]
    contract_id: String,
    #[allow(dead_code)]
    requested: RequestedSkeleton,
    #[allow(dead_code)]
    planned: EffectiveSkeleton,
    observed: ObservedSkeleton,
    #[allow(dead_code)]
    resolution_reason: String,
    #[serde(default)]
    #[allow(dead_code)]
    cross_lineage_authorized: bool,
}

pub(super) fn migrate_v17_dispatch_outcomes_attribution_basis(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    // #1585 D3: product-scoped migration. A PortableKernel store never
    // created the table(s) this touches, so the work is vacuously done.
    // Returning Ok here (rather than skipping the call) is deliberate:
    // `apply_versioned_migration` still marks the sentinel, so a portable
    // database is a COMPLETE stamped-28 database by every existing gate's
    // definition (`validate_current_schema_integrity`,
    // `MIGRATION_SENTINEL_KEYS`) — the sentinel set is profile-invariant.
    if !profile.includes_product() {
        return Ok(0);
    }
    if !table_exists(conn, "dispatch_outcomes")? {
        return Ok(0);
    }
    if !table_has_column(conn, "dispatch_outcomes", "identity_attribution_basis")? {
        conn.execute(
            "ALTER TABLE dispatch_outcomes ADD COLUMN \
             identity_attribution_basis TEXT NOT NULL DEFAULT 'unknown'",
            [],
        )?;
    }

    // Back-fill rows still on the column default. Receipt parsing happens in
    // Rust against the full `ReceiptSkeleton` shape (not a loose JSON
    // pointer probe) so this migration's verdict on a receipt agrees with
    // the runtime's verdict on the SAME receipt — see module docs.
    let mut stmt = conn.prepare(
        "SELECT outcome_id, identity_receipt, vendor, model FROM dispatch_outcomes \
         WHERE identity_attribution_basis = 'unknown'",
    )?;
    let rows: Vec<(String, Option<String>, String, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    let mut backfilled = 0usize;
    for (outcome_id, receipt_json, vendor, model) in rows {
        let basis = match receipt_json.as_deref() {
            Some(raw) => match serde_json::from_str::<ReceiptSkeleton>(raw) {
                Ok(skeleton) => match skeleton.observed.acknowledgement.as_str() {
                    "unconfirmed" => "planned_unconfirmed",
                    "acknowledged" => "acknowledged_overlay",
                    "substituted" | "ignored" => "observed",
                    _ => "unknown",
                },
                // Structurally invalid or outright unparseable — a receipt
                // the runtime itself would classify `Corrupt` must land the
                // same `unknown` here, never a value read out of the shape
                // the runtime rejected.
                Err(_) => "unknown",
            },
            None if vendor != "unknown" || model.is_some() => "fallback_unreceipted",
            None => "unknown",
        };
        if basis != "unknown" {
            conn.execute(
                "UPDATE dispatch_outcomes SET identity_attribution_basis = ?2 \
                 WHERE outcome_id = ?1",
                rusqlite::params![outcome_id, basis],
            )?;
            backfilled += 1;
        }
    }
    Ok(backfilled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE dispatch_outcomes (
                 outcome_id TEXT PRIMARY KEY,
                 dispatch_id TEXT NOT NULL,
                 model TEXT,
                 vendor TEXT NOT NULL DEFAULT 'unknown',
                 execution_outcome TEXT NOT NULL,
                 identity_receipt TEXT,
                 idempotency_key TEXT NOT NULL
             );",
        )
        .unwrap();
    }

    fn insert(
        conn: &Connection,
        id: &str,
        vendor: &str,
        model: Option<&str>,
        receipt: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO dispatch_outcomes
                 (outcome_id, dispatch_id, model, vendor, execution_outcome,
                  identity_receipt, idempotency_key)
             VALUES (?1, ?1, ?2, ?3, 'completed', ?4, ?1)",
            rusqlite::params![id, model, vendor, receipt],
        )
        .unwrap();
    }

    fn basis(conn: &Connection, id: &str) -> String {
        conn.query_row(
            "SELECT identity_attribution_basis FROM dispatch_outcomes WHERE outcome_id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// A full, structurally-legal receipt skeleton (every field the runtime
    /// type requires) carrying the given `acknowledgement` token. Fixtures
    /// use this so the "legal token" tests actually exercise a receipt the
    /// runtime itself would accept as `Present`, not a fragment.
    fn full_receipt_json(acknowledgement: &str) -> String {
        serde_json::json!({
            "contract_id": "dispatch_identity_receipt/v1",
            "requested": {
                "profile": null,
                "model": null,
                "agent": null,
                "harness": null,
            },
            "planned": {
                "profile": null,
                "model": "zhipuai-coding-plan/glm-5.2",
                "backend": "glm",
                "harness": "acp",
                "model_lineage_id": "zhipuai-coding-plan/glm",
                "concrete_model_release": "glm-5.2",
                "provider_model": "glm-5.2",
                "provider_model_version": "unknown",
                "role": "implementer",
                "seat": "unknown",
                "transport": "acp",
                "adapter_version": "unknown",
                "carrier_version": "unknown",
            },
            "observed": {
                "acknowledgement": acknowledgement,
                "effective": {
                    "profile": null,
                    "model": null,
                    "backend": "unknown",
                    "harness": "unknown",
                    "model_lineage_id": "unknown",
                    "concrete_model_release": "unknown",
                    "provider_model": "unknown",
                    "provider_model_version": "unknown",
                    "role": "unknown",
                    "seat": "unknown",
                    "transport": "unknown",
                    "adapter_version": "unknown",
                    "carrier_version": "unknown",
                },
                "mismatch": false,
                "resolution_reason": "carrier acknowledgement unavailable",
            },
            "resolution_reason": "resolved from profile",
            "cross_lineage_authorized": false,
        })
        .to_string()
    }

    #[test]
    fn v17_backfills_basis_from_frozen_receipts_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        legacy_table(&conn);
        insert(
            &conn,
            "o-unconfirmed",
            "glm",
            Some("m"),
            Some(&full_receipt_json("unconfirmed")),
        );
        insert(
            &conn,
            "o-substituted",
            "glm",
            Some("m"),
            Some(&full_receipt_json("substituted")),
        );
        insert(
            &conn,
            "o-acked",
            "glm",
            Some("m"),
            Some(&full_receipt_json("acknowledged")),
        );
        insert(&conn, "o-corrupt", "glm", Some("m"), Some("not json"));
        // Structurally invalid (only the `observed.acknowledgement` field
        // present, everything else the real receipt type requires is
        // missing) but the token it DOES carry is legal (#1065 D BUG-1
        // discriminator). This must land `unknown`, exactly like a
        // full-JSON-parse-failure — a legal token inside a malformed shape
        // is not evidence, it's the exact spoofing surface the runtime's
        // `Corrupt` classification exists to close.
        insert(
            &conn,
            "o-partial-legal-token",
            "glm",
            Some("m"),
            Some(r#"{"observed":{"acknowledgement":"unconfirmed"}}"#),
        );
        insert(&conn, "o-legacy", "codex", None, None);
        insert(&conn, "o-void", "unknown", None, None);

        let first = migrate_v17_dispatch_outcomes_attribution_basis(&conn, StoreProfile::TachiFull)
            .unwrap();
        assert_eq!(first, 4, "four rows earn a non-unknown basis");
        assert_eq!(basis(&conn, "o-unconfirmed"), "planned_unconfirmed");
        assert_eq!(basis(&conn, "o-substituted"), "observed");
        assert_eq!(basis(&conn, "o-acked"), "acknowledged_overlay");
        assert_eq!(basis(&conn, "o-corrupt"), "unknown");
        assert_eq!(
            basis(&conn, "o-partial-legal-token"),
            "unknown",
            "a structurally-invalid receipt must stay unknown even when it \
             happens to carry a legal acknowledgement token"
        );
        assert_eq!(basis(&conn, "o-legacy"), "fallback_unreceipted");
        assert_eq!(basis(&conn, "o-void"), "unknown");

        // Re-run: column exists, only still-unknown rows are revisited, and
        // they legitimately stay unknown — no churn.
        let second =
            migrate_v17_dispatch_outcomes_attribution_basis(&conn, StoreProfile::TachiFull)
                .unwrap();
        assert_eq!(second, 0);
    }
}
