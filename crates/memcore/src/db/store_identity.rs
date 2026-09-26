//! Write-once store identity stamps (#1579) and the profile-admission
//! resolution that rides with them (#1585 D2).
//!
//! ## The hole this closes
//!
//! Before this, a database's manifest role — "am I the Wiki corpus?" — was
//! *inferred from the name of the directory it happened to sit in*
//! (`ProjectDbState::open`'s `db_path.parent().file_name()`). Every identity
//! decision downstream (`is_wiki_corpus_store`, `validate_path_for_db`) rested
//! on that guess. It fails in all four directions:
//!
//! * **Forgeable by relocation** — move any DB into a directory called `wiki`
//!   and it acquires wiki authority; move the real wiki DB out and it loses it.
//! * **Order-dependent** — whichever caller filled the attachment cache first
//!   decided the label for every later reader.
//! * **Silent** — a wrong label over-filters reads (rows vanish from a
//!   caller's view) with no error anywhere.
//! * **Two authorities** — the read side and the write side each derived it.
//!
//! The fix is to put identity *inside the file*: two write-once rows in the
//! existing `hard_state` table, namespace [`STORE_IDENTITY_NAMESPACE`], keys
//! [`STORE_ROLE_KEY`] and [`STORE_PROFILE_KEY`]. The stamp travels with the
//! bytes, so relocation cannot forge it; `db_label` becomes stamp-derived and
//! the caller's `db_label` argument becomes a *claim* that is verified, not a
//! conferral that is trusted.
//!
//! Write-once is enforced at two levels: the stamps are written with
//! [`crate::db::insert_state_if_absent`] (never `set_state`), and
//! [`crate::db::set_state`] / [`crate::db::delete_state`] refuse this
//! namespace outright, so no ordinary state write can reach it.
//!
//! ## Fail direction (frozen, #1585 D1)
//!
//! Identity unavailable ⇒ **NOT the Wiki corpus**. Reads go unfiltered
//! (nothing silently disappears) and `/wiki` writes are refused loudly by the
//! existing `PathRoutingError::WikiPathInNonWikiDb`. An absent stamp never
//! upgrades a store's authority.
//!
//! The stamps carry no `expires_at`, so `reap_expired_state` — which is
//! generic over every namespace — is a no-op on them.

use std::path::Path;
use std::sync::Once;

use rusqlite::Connection;

use crate::error::MemoryError;
use crate::path_router::UNKNOWN_DB_LABEL;

use super::common::now_utc_iso;
use super::store_profile::{
    parse_stored_profile, ProfileRequirement, StoreProfile, STORE_IDENTITY_NAMESPACE,
    STORE_PROFILE_KEY, STORE_ROLE_KEY,
};

/// Kernel-reserved write-once partition stamp for destination-side outbox
/// apply (#1718).  It lives beside the role/profile stamps so generic state
/// writers cannot overwrite the destination partition after the first
/// successful apply.
pub(crate) const OUTBOX_DESTINATION_PARTITION_KEY: &str = "outbox_destination_partition";

/// A store's resolved identity: what it says it is, not what a caller claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreIdentity {
    /// The manifest role this handle will carry as `MemoryStore::db_label`.
    /// [`UNKNOWN_DB_LABEL`] when neither a stamp nor a caller claim exists.
    pub db_label: String,
    /// The **effective** schema profile: the stored one whenever the store has
    /// one. See [`super::store_profile`] for why this is never the required one.
    pub profile: StoreProfile,
}

/// Does this connection have a `hard_state` table yet? A brand-new file does
/// not, and asking for a stamp there is a legitimate "absent", not an error.
fn hard_state_exists(conn: &Connection) -> Result<bool, MemoryError> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type = 'table' AND name = 'hard_state')",
        [],
        |row| row.get(0),
    )?;
    Ok(present)
}

/// Read one identity stamp's `value` field, tolerating a DB too young to have
/// `hard_state`.
pub(crate) fn read_stamp(conn: &Connection, key: &str) -> Result<Option<String>, MemoryError> {
    if !hard_state_exists(conn)? {
        return Ok(None);
    }
    let Some((value_json, _version)) =
        super::state::get_state(conn, STORE_IDENTITY_NAMESPACE, key)?
    else {
        return Ok(None);
    };
    let parsed: serde_json::Value = serde_json::from_str(&value_json)?;
    let value = parsed
        .get("value")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "store identity stamp {STORE_IDENTITY_NAMESPACE}/{key} is malformed \
                 (no string `value` field): {value_json}"
            ))
        })?;
    Ok(Some(value.to_string()))
}

/// Write one identity stamp if — and only if — the key is currently absent.
/// Returns whether this call is the one that wrote it.
///
/// `conferred_by` is the provenance an operator greps for after the fact: the
/// resolver that had the authority to say so (e.g. `"manifest:named-project"`,
/// `"runtime:global"`, `"open:create-fresh"`).
pub(crate) fn write_stamp_if_absent(
    conn: &Connection,
    key: &str,
    value: &str,
    conferred_by: &str,
) -> Result<bool, MemoryError> {
    let payload = serde_json::json!({
        "value": value,
        "conferred_by": conferred_by,
        "stamped_at": now_utc_iso(),
    })
    .to_string();
    super::state::insert_state_if_absent(conn, STORE_IDENTITY_NAMESPACE, key, &payload)
}

/// Resolve the **effective** schema profile for a store being opened, and say
/// whether it still needs stamping (#1585 D2's admission table).
///
/// | stored  | fresh file | required                  | outcome                                 |
/// |---------|-----------|---------------------------|-----------------------------------------|
/// | present | —         | admitted                  | effective = stored (never `required`)    |
/// | present | —         | `AtLeast`, not satisfied  | `StoreProfileMismatch`                   |
/// | present | —         | `Exact`, not equal        | `StoreProfileNotExact`                   |
/// | absent  | yes       | any                       | effective = required profile; stamp it   |
/// | absent  | no        | `TachiFull` (either kind) | adopt `TachiFull` + stamp (the live DBs) |
/// | absent  | no        | `PortableKernel` (either) | `StoreProfileUnstamped`                  |
///
/// "Admitted" is [`ProfileRequirement::admits`]: the satisfies lattice for
/// `AtLeast`, equality for `Exact` (W1-2).
///
/// `fresh` is the caller's already-computed "this file carries no schema
/// version stamp" fact (`PRAGMA user_version == 0`), which is this codebase's
/// established fresh-vs-existing discriminator — see
/// [`super::migrations::check_db_open_context_gate`]'s decision table. It is
/// deliberately NOT a table-count sniff.
///
/// The adopt row is what lets the nine production databases upgrade in place:
/// they are complete `TachiFull` stores that simply predate the stamp. Adopting
/// writes the profile row and nothing else — no schema-version bump, no
/// sentinel change.
pub(crate) fn resolve_profile(
    stored: Option<StoreProfile>,
    fresh: bool,
    required: impl Into<ProfileRequirement>,
    db_path: &Path,
) -> Result<StoreProfile, MemoryError> {
    let required = required.into();
    match stored {
        Some(stored) if required.admits(stored) => Ok(stored),
        Some(stored) => {
            let required_token = required.profile().as_str().to_string();
            let stored = stored.as_str().to_string();
            let db_path = db_path.display().to_string();
            Err(match required {
                ProfileRequirement::AtLeast(_) => MemoryError::StoreProfileMismatch {
                    required: required_token,
                    stored,
                    db_path,
                },
                ProfileRequirement::Exact(_) => MemoryError::StoreProfileNotExact {
                    required: required_token,
                    stored,
                    db_path,
                },
            })
        }
        None if fresh => Ok(required.profile()),
        None if required.profile().includes_product() => Ok(StoreProfile::TachiFull),
        None => Err(MemoryError::StoreProfileUnstamped {
            db_path: db_path.display().to_string(),
        }),
    }
}

/// Resolve a store's manifest role from its stamp and the caller's claim
/// (#1579's resolution table).
///
/// | stamp   | caller claim        | result                                    |
/// |---------|---------------------|-------------------------------------------|
/// | present | none (`unknown`)    | the stamp                                 |
/// | present | equal               | the stamp                                 |
/// | present | same project, legacy `project:`-prefixed stamp spelling | the stamp |
/// | present | different           | `StoreRoleConflict` — refuse the open     |
/// | absent  | none (`unknown`)    | `unknown`; nothing stamped                |
/// | absent  | declared            | the claim (stamped by the write path)     |
///
/// There is deliberately no first-wins arm: a disagreement between a stamped
/// role and a declared one is a routing bug or a moved file, and answering it
/// by silently preferring either side is how the read path and the write path
/// came to disagree in the first place.
///
/// The one compatibility arm (third row) exists because live project DBs were
/// stamped `project:<name>` by the foundry scheduler's write-open, which used
/// the manifest `scope_hint` display text as the store label; every
/// named-project door claims the bare `<name>` for the SAME manifest-resolved
/// project. The prefix is spelling, not identity: the stamp still answers in
/// its own spelling, and a claim naming any other project — including a
/// different Plan-C hash suffix of the same repo nickname — still refuses.
/// The aliasing runs stamp-side only: a `project:`-shaped *claim* against a
/// bare or special-purpose stamp (`wiki`, `global`) is still a conflict.
pub(crate) fn resolve_role(
    stored: Option<&str>,
    claimed: &str,
    db_path: &Path,
) -> Result<String, MemoryError> {
    let claim_declared = claimed != UNKNOWN_DB_LABEL;
    match (stored, claim_declared) {
        (Some(stored), false) => Ok(stored.to_string()),
        (Some(stored), true)
            if stored == claimed || legacy_project_scope_stamp(stored) == Some(claimed) =>
        {
            Ok(stored.to_string())
        }
        (Some(stored), true) => Err(MemoryError::StoreRoleConflict {
            claimed: claimed.to_string(),
            stored: stored.to_string(),
            db_path: db_path.display().to_string(),
        }),
        (None, true) => Ok(claimed.to_string()),
        (None, false) => Ok(UNKNOWN_DB_LABEL.to_string()),
    }
}

/// The project identity a legacy `project:`-prefixed role stamp names, when
/// the stamp is that spelling. Project identities are canonical ASCII
/// aliases (`[A-Za-z0-9._-]`, never `:`), so the split is unambiguous.
fn legacy_project_scope_stamp(stored: &str) -> Option<&str> {
    stored.strip_prefix("project:")
}

/// The single shared role-vs-claim decision for doors that gate an
/// ALREADY-OPEN store's identity instead of re-opening it.
///
/// `memory-server-runtime`'s bound-project alias gate used to mirror
/// [`resolve_role`]'s table with a strict `==`, so a legacy
/// `project:`-prefixed stamp rejected its bare named-project claim at the
/// alias gate before memcore was ever asked. This wrapper IS that table,
/// behind a seam shaped for an already-resolved handle: `resolved_label` is
/// the bound/attached store's `db_label` — [`UNKNOWN_DB_LABEL`] reads as "no
/// stamp", exactly how the open path treats an absent stamp — and the claim
/// is the caller's declared role. Ok(()) means the claim agrees with the
/// store's identity (legacy spelling included); the typed
/// [`MemoryError::StoreRoleConflict`] means it names a different store.
///
/// Every door that must decide "does this claim match this open store?" goes
/// through here so the alias decision cannot drift from the open decision
/// again.
pub fn validate_declared_role_against_resolved_label(
    resolved_label: &str,
    claim: &str,
    db_path: &Path,
) -> Result<(), MemoryError> {
    let stored = (resolved_label != UNKNOWN_DB_LABEL).then_some(resolved_label);
    resolve_role(stored, claim, db_path).map(|_| ())
}

/// One WARN per process for a read-only open that declared a role against a
/// store carrying no role stamp.
///
/// This is the #1569 behavior kept intact: a read-only handle cannot stamp, so
/// it must keep honoring the declared role or the Wiki read gate would go dark
/// on a store that has simply never been opened for write since the upgrade.
/// It is worth exactly one line of operator noise, not one per open — read
/// pools open many handles against the same file.
pub(crate) fn warn_read_only_declared_unstamped_once(db_path: &str, claimed: &str) {
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            db_path = %db_path,
            claimed_role = %claimed,
            "read-only open declared store role {claimed:?} for a database carrying no \
             store_identity role stamp; honoring the claim for this handle (tachi#1569 \
             behavior). Open this database read-write once with the same resolved role to \
             stamp it permanently (tachi#1579)."
        );
    });
}

/// Decode a role/profile pair straight off an open connection. Used by the
/// read-only open path, which never writes and therefore never stamps.
pub(crate) fn read_identity(
    conn: &Connection,
    db_path: &Path,
) -> Result<(Option<String>, Option<StoreProfile>), MemoryError> {
    let role = read_stamp(conn, STORE_ROLE_KEY)?;
    let profile = match read_stamp(conn, STORE_PROFILE_KEY)? {
        Some(token) => Some(parse_stored_profile(&token, db_path)?),
        None => None,
    };
    Ok((role, profile))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> &'static Path {
        Path::new("/tmp/store.db")
    }

    #[test]
    fn stored_profile_wins_over_required_when_it_satisfies() {
        // The load-bearing rule: a portable opener on a full store gets
        // TachiFull back, NOT the PortableKernel it asked for.
        assert_eq!(
            resolve_profile(
                Some(StoreProfile::TachiFull),
                false,
                StoreProfile::PortableKernel,
                p()
            )
            .expect("full store satisfies a portable requirement"),
            StoreProfile::TachiFull
        );
    }

    #[test]
    fn portable_store_refuses_a_full_requirement() {
        let err = resolve_profile(
            Some(StoreProfile::PortableKernel),
            false,
            StoreProfile::TachiFull,
            p(),
        )
        .expect_err("portable store cannot satisfy a full requirement");
        assert!(
            matches!(err, MemoryError::StoreProfileMismatch { .. }),
            "{err}"
        );
    }

    #[test]
    fn fresh_file_takes_the_required_profile() {
        for required in [StoreProfile::PortableKernel, StoreProfile::TachiFull] {
            assert_eq!(
                resolve_profile(None, true, required, p()).expect("fresh build"),
                required
            );
        }
    }

    #[test]
    fn unstamped_existing_store_adopts_full_but_refuses_portable() {
        assert_eq!(
            resolve_profile(None, false, StoreProfile::TachiFull, p()).expect("adopt"),
            StoreProfile::TachiFull
        );
        let err = resolve_profile(None, false, StoreProfile::PortableKernel, p())
            .expect_err("portable must not adopt an unstamped store");
        assert!(
            matches!(err, MemoryError::StoreProfileUnstamped { .. }),
            "{err}"
        );
    }

    #[test]
    fn exact_requirement_resolution_table() {
        use ProfileRequirement::Exact;
        use StoreProfile::{PortableKernel as P, TachiFull as F};
        // Admitted: stored equals required, effective = stored.
        assert_eq!(resolve_profile(Some(P), false, Exact(P), p()).unwrap(), P);
        assert_eq!(resolve_profile(Some(F), false, Exact(F), p()).unwrap(), F);
        // The superset the lattice would admit is refused, typed.
        let err = resolve_profile(Some(F), false, Exact(P), p())
            .expect_err("exact portable must refuse a full store");
        assert!(
            matches!(err, MemoryError::StoreProfileNotExact { .. }),
            "{err}"
        );
        let err = resolve_profile(Some(P), false, Exact(F), p())
            .expect_err("exact full must refuse a portable store");
        assert!(
            matches!(err, MemoryError::StoreProfileNotExact { .. }),
            "{err}"
        );
        // Fresh builds take the required profile; unstamped existing stores
        // follow the same adopt/refuse rows as `AtLeast`.
        assert_eq!(resolve_profile(None, true, Exact(P), p()).unwrap(), P);
        assert_eq!(resolve_profile(None, true, Exact(F), p()).unwrap(), F);
        assert_eq!(resolve_profile(None, false, Exact(F), p()).unwrap(), F);
        let err = resolve_profile(None, false, Exact(P), p())
            .expect_err("exact portable must not adopt an unstamped store");
        assert!(
            matches!(err, MemoryError::StoreProfileUnstamped { .. }),
            "{err}"
        );
    }

    #[test]
    fn role_resolution_prefers_the_stamp_and_refuses_conflicts() {
        assert_eq!(
            resolve_role(Some("wiki"), UNKNOWN_DB_LABEL, p()).expect("no claim"),
            "wiki"
        );
        assert_eq!(
            resolve_role(Some("wiki"), "wiki", p()).expect("matching claim"),
            "wiki"
        );
        let err = resolve_role(Some("wiki"), "global", p())
            .expect_err("conflicting claim must not be resolved by preference");
        assert!(
            matches!(err, MemoryError::StoreRoleConflict { .. }),
            "{err}"
        );
        assert_eq!(
            resolve_role(None, "wiki", p()).expect("declared claim on unstamped store"),
            "wiki"
        );
        assert_eq!(
            resolve_role(None, UNKNOWN_DB_LABEL, p()).expect("no stamp, no claim"),
            UNKNOWN_DB_LABEL
        );
    }

    #[test]
    fn legacy_project_scope_stamp_admits_the_bare_name_claim() {
        // The foundry scheduler stamped live project DBs with the manifest
        // scope_hint text (`project:<name>`); every named-project door claims
        // the bare `<name>`. Same manifest-resolved project ⇒ the stamp
        // answers, in its own spelling.
        assert_eq!(
            resolve_role(Some("project:tachi"), "tachi", p()).expect("same project, read as bare"),
            "project:tachi"
        );
        // The stored spelling stays authoritative even under the compatible
        // claim: the resolution never rewrites the stamp.
        assert_eq!(
            resolve_role(Some("project:Split_Brain_Repo"), "Split_Brain_Repo", p())
                .expect("same project, read as bare"),
            "project:Split_Brain_Repo"
        );
    }

    #[test]
    fn legacy_project_scope_stamp_still_refuses_different_projects() {
        // A different bare name is a different project — the prefix is not a
        // wildcard.
        let err = resolve_role(Some("project:tachi"), "antigravity", p())
            .expect_err("different project name must conflict");
        assert!(
            matches!(err, MemoryError::StoreRoleConflict { .. }),
            "{err}"
        );
        // The live 2026-09 different-hash shape: two Plan-C identities of the
        // same repo nickname are NOT the same project.
        let err = resolve_role(
            Some("project:yaya-14890056"),
            "yaya-20994e76035f4528deda42ce",
            p(),
        )
        .expect_err("different Plan-C hash identities must conflict");
        assert!(
            matches!(err, MemoryError::StoreRoleConflict { .. }),
            "{err}"
        );
        let err = resolve_role(
            Some("Quant_Analyzer_2026-64b4e4e2e5b3e54938151007"),
            "Quant_Analyzer_2026-a5c4bf5d",
            p(),
        )
        .expect_err("bare different-hash stamps must keep conflicting");
        assert!(
            matches!(err, MemoryError::StoreRoleConflict { .. }),
            "{err}"
        );
        // Frozen roles are unaffected: a `project:`-shaped claim is never
        // aliased onto a bare or special-purpose stamp, and wiki/global keep
        // refusing every other name.
        let err = resolve_role(Some("wiki"), "project:wiki", p())
            .expect_err("a project-scoped claim on the wiki corpus must conflict");
        assert!(
            matches!(err, MemoryError::StoreRoleConflict { .. }),
            "{err}"
        );
        let err = resolve_role(Some("global"), "project:global", p())
            .expect_err("a project-scoped claim on the global store must conflict");
        assert!(
            matches!(err, MemoryError::StoreRoleConflict { .. }),
            "{err}"
        );
    }

    #[test]
    fn resolved_label_gate_shares_the_open_path_table() {
        // The alias-gate seam: an `unknown` resolved label is "no stamp" and
        // accepts any declared claim (the open path would stamp it).
        validate_declared_role_against_resolved_label(UNKNOWN_DB_LABEL, "anything", p())
            .expect("unstamped bound store accepts a declared claim");
        // The legacy spelling admits the bare claim...
        validate_declared_role_against_resolved_label("project:tachi", "tachi", p())
            .expect("legacy scope stamp admits its bare project claim");
        // ...and nothing else: a different project (including a different
        // Plan-C hash) and a project-scoped claim on a frozen role refuse.
        let err = validate_declared_role_against_resolved_label(
            "project:yaya-14890056",
            "yaya-20994e76035f4528deda42ce",
            p(),
        )
        .expect_err("different Plan-C hash identities must conflict at the gate");
        assert!(
            matches!(err, MemoryError::StoreRoleConflict { .. }),
            "{err}"
        );
        let err = validate_declared_role_against_resolved_label("wiki", "project:wiki", p())
            .expect_err("the wiki corpus must refuse a project-scoped claim");
        assert!(
            matches!(err, MemoryError::StoreRoleConflict { .. }),
            "{err}"
        );
    }
}
