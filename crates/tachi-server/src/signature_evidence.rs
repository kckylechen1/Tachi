//! Vendor-keyed error-signature evidence store (#534 / #735).
//!
//! Storage split (design doc Part II is law): the taxonomy is code constants
//! (`tachi_dispatch::signatures`), the projection is computed at assembly time,
//! and *evidence* lives here as append-only, timestamped, `(vendor, role)`-
//! queryable rows. We reuse the existing `hard_state` state-kv store (the same
//! store the profile/card overlays use) rather than introducing a new table:
//! each signature/resolution occurrence is its own immutable row keyed by a
//! fresh UUID, so the namespace is genuinely append-only and needs no schema
//! migration. Rows are queried by listing the namespace and filtering on the
//! `(vendor, role)` fields carried inside each row's JSON.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use tachi_dispatch::{
    self_report_trust as compute_self_report_trust, signature_def, Severity, SignatureEvidenceRow,
    SignatureRowKind,
};

use crate::tool_params::{SignatureRecordParams, TachiCompleteParams};
use crate::MemoryServer;

/// Namespace for append-only signature evidence rows in `hard_state`.
pub(crate) const SIGNATURE_EVIDENCE_NS: &str = "dispatch_signature_evidence";
/// Namespace + key holding the seed-once marker, so startup seeding is
/// idempotent.
pub(crate) const SIGNATURE_SEED_NS: &str = "dispatch_signature_seed";
const SIGNATURE_SEED_KEY: &str = "taxonomy_evidence_v1";

/// Input for recording one signature/resolution occurrence at `complete` time.
pub(crate) struct SignatureRecord {
    pub vendor: String,
    pub role: String,
    pub signature: String,
    pub severity: Option<Severity>,
    pub evidence_ref: Option<String>,
    /// When true this is an append-only *resolution* row, marking the signature
    /// resolved as of `recorded_at`.
    pub resolved: bool,
    pub recorded_at: DateTime<Utc>,
    /// Frozen dispatch identity when this evidence came from a Tachi dispatch.
    /// Older/external evidence remains explicitly unattached.
    pub identity_receipt: Option<Value>,
}

fn record_to_value(record: &SignatureRecord) -> Value {
    let kind = if record.resolved {
        "resolution"
    } else {
        "signature"
    };
    let severity = record
        .severity
        .and_then(|s| serde_json::to_value(s).ok())
        .filter(|_| !record.resolved);
    json!({
        "kind": kind,
        "vendor": record.vendor,
        "role": record.role,
        "signature": record.signature,
        "severity": severity,
        "evidence_ref": record.evidence_ref,
        "identity_receipt": record.identity_receipt,
        "recorded_at": record.recorded_at.to_rfc3339(),
    })
}

/// Append one evidence row. Never overwrites: a fresh UUID key guarantees
/// append-only history.
pub(crate) fn record_signature(
    server: &MemoryServer,
    record: &SignatureRecord,
) -> Result<String, String> {
    let key = uuid::Uuid::new_v4().to_string();
    let value = serde_json::to_string(&record_to_value(record))
        .map_err(|e| format!("serialize signature evidence: {e}"))?;
    server.with_global_store(|store| {
        store
            .set_state(SIGNATURE_EVIDENCE_NS, &key, &value)
            .map(|_| ())
            .map_err(|e| format!("persist signature evidence: {e}"))
    })?;
    Ok(key)
}

fn parse_row(value: &Value) -> Option<(String, String, SignatureEvidenceRow)> {
    let vendor = value.get("vendor").and_then(Value::as_str)?.to_string();
    let role = value.get("role").and_then(Value::as_str)?.to_string();
    let signature = value.get("signature").and_then(Value::as_str)?.to_string();
    let kind = match value.get("kind").and_then(Value::as_str)? {
        "resolution" => SignatureRowKind::Resolution,
        _ => SignatureRowKind::Signature,
    };
    let severity = value
        .get("severity")
        .and_then(|s| serde_json::from_value::<Severity>(s.clone()).ok());
    let evidence_ref = value
        .get("evidence_ref")
        .and_then(Value::as_str)
        .map(str::to_string);
    // Epoch, never lexical: parse RFC3339 → seconds so decay/resolution compare
    // numerically (lane hazard, 2026-07-05).
    let recorded_at_epoch = value
        .get("recorded_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp())?;
    Some((
        vendor,
        role,
        SignatureEvidenceRow {
            kind,
            signature,
            severity,
            evidence_ref,
            recorded_at_epoch,
        },
    ))
}

/// Load every parseable evidence row. Rows that fail to parse are skipped (they
/// never fail the read — projection is best-effort by frozen decision 3).
fn load_all_rows(
    server: &MemoryServer,
) -> Result<Vec<(String, String, SignatureEvidenceRow)>, String> {
    let raw = server.with_global_store_read(|store| {
        store
            .list_state(SIGNATURE_EVIDENCE_NS)
            .map_err(|e| format!("list signature evidence: {e}"))
    })?;
    let mut out = Vec::with_capacity(raw.len());
    for row in raw {
        if let Ok(value) = serde_json::from_str::<Value>(&row.value_json) {
            if let Some(parsed) = parse_row(&value) {
                out.push(parsed);
            }
        }
    }
    Ok(out)
}

/// Rows for a single `(role, vendor)` lane.
pub(crate) fn rows_for_lane(
    server: &MemoryServer,
    role: &str,
    vendor: &str,
) -> Result<Vec<SignatureEvidenceRow>, String> {
    Ok(load_all_rows(server)?
        .into_iter()
        .filter(|(v, r, _)| v == vendor && r == role)
        .map(|(_, _, row)| row)
        .collect())
}

/// Rows for a vendor across all roles (self-report trust is vendor-scoped).
pub(crate) fn rows_for_vendor(
    server: &MemoryServer,
    vendor: &str,
) -> Result<Vec<SignatureEvidenceRow>, String> {
    Ok(load_all_rows(server)?
        .into_iter()
        .filter(|(v, _, _)| v == vendor)
        .map(|(_, _, row)| row)
        .collect())
}

/// Vendor-level self-report trust (`Some("low")` or `None`). Never persisted.
pub(crate) fn self_report_trust_for_vendor(
    server: &MemoryServer,
    vendor: &str,
) -> Result<Option<&'static str>, String> {
    if vendor == "unknown" {
        return Ok(None);
    }
    Ok(compute_self_report_trust(&rows_for_vendor(server, vendor)?))
}

fn parse_severity(raw: &str) -> Option<Severity> {
    serde_json::from_value(Value::String(raw.trim().to_ascii_lowercase())).ok()
}

/// Resolve the `(role_class, vendor)` lane for a signature recorded at
/// `complete`, honoring explicit overrides before deriving from the dispatch
/// profile/agent. `None` when the lane can't be resolved (vendor unknown or role
/// unrecognized) — the caller reports it as skipped rather than guessing.
fn resolve_complete_lane(
    params: &TachiCompleteParams,
    sig: &SignatureRecordParams,
) -> Option<(String, String)> {
    if sig
        .vendor
        .as_deref()
        .is_none_or(|vendor| vendor.trim().is_empty())
        && sig
            .role
            .as_deref()
            .is_none_or(|role| role.trim().is_empty())
    {
        if let Some(dispatch_id) = params.dispatch_id.as_deref() {
            if let Some(receipt) = crate::dispatch_ops::load_dispatch_identity_receipt(dispatch_id)
            {
                let role = tachi_dispatch::dispatch_role_class(&receipt.planned.role)?;
                let vendor = tachi_dispatch::normalize_vendor(
                    &receipt.planned.backend,
                    receipt.planned.model.as_deref(),
                );
                return (vendor != "unknown").then(|| (role.to_string(), vendor));
            }
        }
    }
    let profile_def = params
        .profile
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .and_then(crate::dispatch_profile::resolve_dispatch_profile);
    let vendor = sig
        .vendor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match profile_def {
            Some(p) => tachi_dispatch::normalize_vendor(p.backend, p.model),
            None => tachi_dispatch::normalize_vendor(&params.agent, None),
        });
    if vendor == "unknown" {
        return None;
    }
    let role = sig
        .role
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(tachi_dispatch::dispatch_role_class)
        .or_else(|| profile_def.and_then(|p| tachi_dispatch::dispatch_role_class(p.role)))
        .map(str::to_string)?;
    Some((role, vendor))
}

/// Record the leader-supplied signatures carried on a `complete` call. Returns a
/// pipeline-status value; recording failures are reported, never fatal.
pub(crate) fn record_complete_signatures(
    server: &MemoryServer,
    params: &TachiCompleteParams,
) -> Value {
    if params.signatures.is_empty() {
        return json!("skipped (no signatures)");
    }
    let now = Utc::now();
    let identity_receipt = params
        .dispatch_id
        .as_deref()
        .and_then(crate::dispatch_ops::load_dispatch_identity_receipt)
        .map(|receipt| {
            serde_json::to_value(receipt).expect("dispatch identity receipt serializes")
        });
    let mut recorded = Vec::new();
    let mut skipped = Vec::new();
    for sig in &params.signatures {
        let Some((role, vendor)) = resolve_complete_lane(params, sig) else {
            skipped.push(json!({
                "signature": sig.signature,
                "reason": "unresolved (role, vendor) lane; pass role/vendor explicitly",
            }));
            continue;
        };
        let severity = sig
            .severity
            .as_deref()
            .and_then(parse_severity)
            .or_else(|| signature_def(&sig.signature).map(|d| d.severity))
            .filter(|_| !sig.resolved);
        let record = SignatureRecord {
            vendor: vendor.clone(),
            role: role.clone(),
            signature: sig.signature.clone(),
            severity,
            evidence_ref: sig.evidence_ref.clone(),
            resolved: sig.resolved,
            recorded_at: now,
            identity_receipt: identity_receipt.clone(),
        };
        match record_signature(server, &record) {
            Ok(key) => recorded.push(json!({
                "signature": sig.signature,
                "role": role,
                "vendor": vendor,
                "resolved": sig.resolved,
                "key": key,
            })),
            Err(err) => skipped.push(json!({
                "signature": sig.signature,
                "reason": err,
            })),
        }
    }
    json!({ "recorded": recorded, "skipped": skipped })
}

/// Seed the 2026-07-05 campaign taxonomy evidence, once. Idempotent via a
/// seed-once marker so repeated serve starts do not duplicate rows. Callable
/// directly in tests to populate an in-memory server.
pub(crate) fn seed_signature_taxonomy_evidence(server: &MemoryServer) -> Result<bool, String> {
    let already = server.with_global_store(|store| {
        store
            .insert_state_if_absent(SIGNATURE_SEED_NS, SIGNATURE_SEED_KEY, "{\"seeded\":true}")
            .map_err(|e| format!("claim signature seed marker: {e}"))
    })?;
    if !already {
        return Ok(false);
    }

    let now = Utc::now();
    let mut records: Vec<SignatureRecord> = Vec::new();

    // glm-as-implementer: the 2026-07-05 vault-security failures + the falsified
    // clippy-clean report + repeated over-close. All fresh (active).
    for evidence_ref in ["#541", "#583", "#600", "#607"] {
        records.push(SignatureRecord {
            vendor: "glm".to_string(),
            role: "implementer".to_string(),
            signature: "fake_security_fix".to_string(),
            severity: signature_def("fake_security_fix").map(|d| d.severity),
            evidence_ref: Some(evidence_ref.to_string()),
            resolved: false,
            recorded_at: now,
            identity_receipt: None,
        });
    }
    records.push(SignatureRecord {
        vendor: "glm".to_string(),
        role: "implementer".to_string(),
        signature: "falsified_ci_report".to_string(),
        severity: signature_def("falsified_ci_report").map(|d| d.severity),
        evidence_ref: Some("#610".to_string()),
        resolved: false,
        recorded_at: now,
        identity_receipt: None,
    });
    for _ in 0..3 {
        records.push(SignatureRecord {
            vendor: "glm".to_string(),
            role: "implementer".to_string(),
            signature: "self_close_overreach".to_string(),
            severity: signature_def("self_close_overreach").map(|d| d.severity),
            evidence_ref: Some("2026-07-05-campaign".to_string()),
            resolved: false,
            recorded_at: now,
            identity_receipt: None,
        });
    }

    // codex-as-implementer: parking + breadcrumb urges were vaccinated via the
    // constitution — recorded then resolved (proves the resolution path). Plus
    // one fresh, unresolved assertion_weakening from #733 BUG-1.
    let older = now - chrono::Duration::days(10);
    let resolved_at = now - chrono::Duration::days(9);
    for signature in ["parking_after_contract", "breadcrumb_violation"] {
        records.push(SignatureRecord {
            vendor: "codex".to_string(),
            role: "implementer".to_string(),
            signature: signature.to_string(),
            severity: signature_def(signature).map(|d| d.severity),
            evidence_ref: Some("2026-07-05-campaign".to_string()),
            resolved: false,
            recorded_at: older,
            identity_receipt: None,
        });
        records.push(SignatureRecord {
            vendor: "codex".to_string(),
            role: "implementer".to_string(),
            signature: signature.to_string(),
            severity: None,
            evidence_ref: Some("constitution-remap".to_string()),
            resolved: true,
            recorded_at: resolved_at,
            identity_receipt: None,
        });
    }
    records.push(SignatureRecord {
        vendor: "codex".to_string(),
        role: "implementer".to_string(),
        signature: "assertion_weakening".to_string(),
        severity: signature_def("assertion_weakening").map(|d| d.severity),
        evidence_ref: Some("PR #733 BUG-1".to_string()),
        resolved: false,
        recorded_at: now,
        identity_receipt: None,
    });

    for record in &records {
        record_signature(server, record)?;
    }
    Ok(true)
}
