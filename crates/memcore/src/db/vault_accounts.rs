//! SQL for the provider-account tables (tachi#1680 D1/D5).
//!
//! Row types live in [`crate::vault::accounts`], mirroring the split the rest
//! of this table family already uses (`crate::vault` holds `VaultKeyHealth`,
//! `db::vault_db` holds its SQL).
//!
//! # Two shapes deliberately absent
//!
//! There is **no generic update and no delete** on this surface. Every write
//! here is a named state transition — observe an alias, retire an alias,
//! record a new fingerprint, move a custody pointer — because the audit value
//! of `provider_account_events` evaporates the moment a caller can rewrite an
//! account row without saying why. Deleting an account is likewise not an
//! accessor: retirement is a status transition, and the event history of a
//! credential that once existed is exactly the history an operator needs.
//!
//! # Transactions
//!
//! These accessors never open a transaction of their own, so they compose
//! inside the single write transaction #1680 D4's `apply` opens (an inner
//! `BEGIN` there would fail outright — SQLite has no nested transactions). The
//! one multi-statement accessor, [`insert_provider_account`], documents what a
//! caller outside a transaction risks.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::MemoryError;
use crate::vault::accounts::{
    AccountClass, AccountCustody, AuthMode, CustodyKind, CustodyResolution, NewProviderAccount,
    NewProviderAccountEvent, ProviderAccount, ProviderAccountAlias, ProviderAccountEvent,
    EVENT_KIND_ACCOUNT_CREATED,
};

use super::common::now_utc_iso;

const ACCOUNT_COLUMNS: &str = "account_id, provider_kind, auth_mode, auth_ref, \
     account_fingerprint, account_class, capabilities, credential_policy_ref, \
     refresh_authority, status, revision, source_refs, created_at, updated_at";

/// What [`record_provider_account_alias`] did, so a caller can decide whether
/// the observation is worth an event row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasObservation {
    /// The name had never been seen for this account.
    Created,
    /// The name was already active; only `last_seen` moved.
    Refreshed,
    /// The name had been retired and has been observed again. Distinct from
    /// `Refreshed` because a name coming back is a real event: it usually
    /// means a config was reverted or a rename was undone.
    Revived,
}

/// What [`record_account_fingerprint`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintUpdate {
    /// The fingerprint changed: the account advanced to `revision` and an
    /// event row was appended.
    Advanced { revision: i64, event_id: i64 },
    /// The stored fingerprint already equalled the new one, so nothing moved —
    /// no revision bump, no event. This is what makes an unchanged reconcile
    /// pass a genuine no-op rather than a stream of identical events.
    Unchanged { revision: i64 },
}

/// Insert a new account and its `account_created` event.
///
/// `revision` starts at 1 and `created_at`/`updated_at` are stamped here
/// rather than accepted from the caller: they are the store's, and a caller
/// that could choose its own revision could rewind the counter every later
/// slice binds its apply-time preconditions to.
///
/// Two statements, no internal transaction (see the module note). Inside D4's
/// apply transaction they are atomic; a caller writing outside a transaction
/// accepts that a crash between them leaves an account whose creation is not
/// in the event log.
pub fn insert_provider_account(
    conn: &Connection,
    new: &NewProviderAccount,
) -> Result<ProviderAccount, MemoryError> {
    if new.auth_mode.has_tachi_held_credential() && new.auth_ref.is_none() {
        return Err(MemoryError::InvalidArg(format!(
            "auth_mode '{}' holds a Tachi-side credential and therefore requires an auth_ref",
            new.auth_mode.as_str()
        )));
    }
    if !new.auth_mode.has_tachi_held_credential() && new.auth_ref.is_some() {
        return Err(MemoryError::InvalidArg(format!(
            "auth_mode '{}' has no Tachi-held credential, so an auth_ref would point at custody \
             that cannot exist",
            new.auth_mode.as_str()
        )));
    }

    let now = now_utc_iso();
    let capabilities = serde_json::to_string(&new.capabilities)?;
    let source_refs = serde_json::to_string(&new.source_refs)?;

    conn.execute(
        "INSERT INTO provider_accounts (
            account_id, provider_kind, auth_mode, auth_ref, account_fingerprint,
            account_class, capabilities, credential_policy_ref, refresh_authority,
            status, revision, source_refs, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?12, ?12)",
        params![
            new.account_id,
            new.provider_kind,
            new.auth_mode.as_str(),
            new.auth_ref,
            new.account_fingerprint,
            new.account_class.as_str(),
            capabilities,
            new.credential_policy_ref,
            new.refresh_authority,
            new.status,
            source_refs,
            now,
        ],
    )?;

    append_provider_account_event(
        conn,
        &NewProviderAccountEvent::new(new.account_id.as_str(), 1, EVENT_KIND_ACCOUNT_CREATED),
    )?;

    get_provider_account(conn, &new.account_id)?.ok_or_else(|| {
        MemoryError::Internal(format!(
            "account '{}' vanished immediately after insert",
            new.account_id
        ))
    })
}

pub fn get_provider_account(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<ProviderAccount>, MemoryError> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM provider_accounts WHERE account_id = ?1");
    let row = conn
        .prepare(&sql)?
        .query_row(params![account_id], account_from_row)
        .optional()?;
    row.transpose_parse()
}

/// The plan-time merge lookup: every account currently carrying this account
/// fingerprint. Returns a list rather than an `Option` on purpose — two
/// accounts sharing a fingerprint is a state the plan must be able to *see* and
/// report, not one the accessor gets to hide by returning the first row.
pub fn find_provider_accounts_by_fingerprint(
    conn: &Connection,
    account_fingerprint: &str,
) -> Result<Vec<ProviderAccount>, MemoryError> {
    let sql = format!(
        "SELECT {ACCOUNT_COLUMNS} FROM provider_accounts \
         WHERE account_fingerprint = ?1 ORDER BY account_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![account_fingerprint], account_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(|row| row.parse()).collect()
}

pub fn find_provider_account_by_auth_ref(
    conn: &Connection,
    auth_ref: &str,
) -> Result<Option<ProviderAccount>, MemoryError> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM provider_accounts WHERE auth_ref = ?1");
    let row = conn
        .prepare(&sql)?
        .query_row(params![auth_ref], account_from_row)
        .optional()?;
    row.transpose_parse()
}

pub fn list_provider_accounts(conn: &Connection) -> Result<Vec<ProviderAccount>, MemoryError> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM provider_accounts ORDER BY account_id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], account_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(|row| row.parse()).collect()
}

/// Observe an env-var name for an account: insert it, or move `last_seen` (and
/// un-retire it) if it is already known. Never deletes, never rewrites
/// `first_seen` — the first time a name was seen is a fact about history.
pub fn record_provider_account_alias(
    conn: &Connection,
    account_id: &str,
    alias_name: &str,
    source_kind: &str,
) -> Result<AliasObservation, MemoryError> {
    let now = now_utc_iso();
    let existing: Option<i64> = conn
        .query_row(
            "SELECT retired FROM provider_account_aliases WHERE account_id = ?1 AND alias_name = ?2",
            params![account_id, alias_name],
            |row| row.get(0),
        )
        .optional()?;

    match existing {
        None => {
            conn.execute(
                "INSERT INTO provider_account_aliases
                    (account_id, alias_name, source_kind, first_seen, last_seen, retired)
                 VALUES (?1, ?2, ?3, ?4, ?4, 0)",
                params![account_id, alias_name, source_kind, now],
            )?;
            Ok(AliasObservation::Created)
        }
        Some(retired) => {
            conn.execute(
                "UPDATE provider_account_aliases
                    SET last_seen = ?3, source_kind = ?4, retired = 0
                  WHERE account_id = ?1 AND alias_name = ?2",
                params![account_id, alias_name, now, source_kind],
            )?;
            if retired != 0 {
                Ok(AliasObservation::Revived)
            } else {
                Ok(AliasObservation::Refreshed)
            }
        }
    }
}

/// Mark an alias retired. The row stays: "this account was once reachable
/// under that name" outlives the name.
pub fn retire_provider_account_alias(
    conn: &Connection,
    account_id: &str,
    alias_name: &str,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let changed = conn.execute(
        "UPDATE provider_account_aliases
            SET retired = 1, last_seen = ?3
          WHERE account_id = ?1 AND alias_name = ?2 AND retired = 0",
        params![account_id, alias_name, now],
    )?;
    Ok(changed > 0)
}

pub fn list_provider_account_aliases(
    conn: &Connection,
    account_id: &str,
) -> Result<Vec<ProviderAccountAlias>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT account_id, alias_name, source_kind, first_seen, last_seen, retired
           FROM provider_account_aliases WHERE account_id = ?1 ORDER BY alias_name",
    )?;
    let rows = stmt.query_map(params![account_id], |row| {
        let retired: i64 = row.get(5)?;
        Ok(ProviderAccountAlias {
            account_id: row.get(0)?,
            alias_name: row.get(1)?,
            source_kind: row.get(2)?,
            first_seen: row.get(3)?,
            last_seen: row.get(4)?,
            retired: retired != 0,
        })
    })?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

/// Append one audit event. The only write this table has.
pub fn append_provider_account_event(
    conn: &Connection,
    event: &NewProviderAccountEvent,
) -> Result<i64, MemoryError> {
    conn.execute(
        "INSERT INTO provider_account_events
            (account_id, revision, event_kind, plan_digest, evidence, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.account_id,
            event.revision,
            event.event_kind,
            event.plan_digest,
            event.evidence,
            now_utc_iso(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_provider_account_events(
    conn: &Connection,
    account_id: &str,
) -> Result<Vec<ProviderAccountEvent>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id, account_id, revision, event_kind, plan_digest, evidence, created_at
           FROM provider_account_events WHERE account_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![account_id], |row| {
        Ok(ProviderAccountEvent {
            id: row.get(0)?,
            account_id: row.get(1)?,
            revision: row.get(2)?,
            event_kind: row.get(3)?,
            plan_digest: row.get(4)?,
            evidence: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

/// Record a new account fingerprint: bump the revision and append an event, in
/// that order, or do nothing at all if the fingerprint is unchanged.
///
/// This is the whole rotation story (#1680 D2). Members rotating, a member
/// added or dropped, or the Vault master key being rekeyed all land here with
/// a different `event_kind`; none of them touches `account_id`, so identity
/// survives every one of them and the event row says which happened.
pub fn record_account_fingerprint(
    conn: &Connection,
    account_id: &str,
    account_fingerprint: &str,
    event_kind: &str,
    plan_digest: Option<&str>,
    evidence: &str,
) -> Result<FingerprintUpdate, MemoryError> {
    let current: Option<(String, i64)> = conn
        .query_row(
            "SELECT account_fingerprint, revision FROM provider_accounts WHERE account_id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (current_fingerprint, current_revision) = current.ok_or_else(|| {
        MemoryError::NotFound(format!(
            "provider account '{account_id}' does not exist, so it has no fingerprint to record"
        ))
    })?;

    if current_fingerprint == account_fingerprint {
        return Ok(FingerprintUpdate::Unchanged {
            revision: current_revision,
        });
    }

    let next_revision = current_revision + 1;
    conn.execute(
        "UPDATE provider_accounts
            SET account_fingerprint = ?2, revision = ?3, updated_at = ?4
          WHERE account_id = ?1",
        params![
            account_id,
            account_fingerprint,
            next_revision,
            now_utc_iso()
        ],
    )?;

    let mut event =
        NewProviderAccountEvent::new(account_id, next_revision, event_kind).with_evidence(evidence);
    if let Some(digest) = plan_digest {
        event = event.with_plan_digest(digest);
    }
    let event_id = append_provider_account_event(conn, &event)?;

    Ok(FingerprintUpdate::Advanced {
        revision: next_revision,
        event_id,
    })
}

/// Bind an `auth_ref` to the Vault object that actually holds the secret.
///
/// The custody row is the only place this mapping exists, and it is reachable
/// only through [`resolve_auth_ref`] — which is why no account-shaped
/// serialization can leak Vault layout by accident.
pub fn insert_account_custody(
    conn: &Connection,
    auth_ref: &str,
    account_id: &str,
    custody_kind: CustodyKind,
    custody_target: &str,
) -> Result<AccountCustody, MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "INSERT INTO account_custody
            (auth_ref, account_id, custody_kind, custody_target, revision, updated_at)
         VALUES (?1, ?2, ?3, ?4, 1, ?5)",
        params![
            auth_ref,
            account_id,
            custody_kind.as_str(),
            custody_target,
            now
        ],
    )?;
    get_account_custody_by_auth_ref(conn, auth_ref)?.ok_or_else(|| {
        MemoryError::Internal("custody row vanished immediately after insert".to_string())
    })
}

/// **The custody resolver** (#1680 D5): opaque `auth_ref` in, Vault location
/// out.
///
/// It lives here, in memcore, rather than in the server's `vault_ops`, because
/// resolution must be callable from wherever a credential is about to be used
/// without dragging in product code: `tachi-llm` does not and must not depend
/// on `tachi-server`, so a resolver on the server side would be unreachable
/// from the materialization path. The server resolves through this function and
/// passes the *result* into the existing durable-source closure seam, leaving
/// `tachi-llm`'s signatures untouched.
pub fn resolve_auth_ref(
    conn: &Connection,
    auth_ref: &str,
) -> Result<Option<CustodyResolution>, MemoryError> {
    let row: Option<(String, String, String, i64)> = conn
        .query_row(
            "SELECT account_id, custody_kind, custody_target, revision
               FROM account_custody WHERE auth_ref = ?1",
            params![auth_ref],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;

    row.map(
        |(account_id, kind, custody_target, revision)| -> Result<CustodyResolution, MemoryError> {
            Ok(CustodyResolution {
                account_id,
                custody_kind: parse_custody_kind(&kind)?,
                custody_target,
                revision,
            })
        },
    )
    .transpose()
}

pub fn get_account_custody(
    conn: &Connection,
    account_id: &str,
) -> Result<Option<AccountCustody>, MemoryError> {
    custody_row(
        conn,
        "SELECT auth_ref, account_id, custody_kind, custody_target, revision, updated_at
           FROM account_custody WHERE account_id = ?1",
        account_id,
    )
}

pub fn get_account_custody_by_auth_ref(
    conn: &Connection,
    auth_ref: &str,
) -> Result<Option<AccountCustody>, MemoryError> {
    custody_row(
        conn,
        "SELECT auth_ref, account_id, custody_kind, custody_target, revision, updated_at
           FROM account_custody WHERE auth_ref = ?1",
        auth_ref,
    )
}

/// Repoint custody at a different Vault object, keeping `auth_ref` — and
/// therefore every upper-layer reference to the account — unchanged. A
/// no-change call is a no-op, so restructuring twice with the same result does
/// not inflate the revision.
pub fn update_custody_target(
    conn: &Connection,
    auth_ref: &str,
    custody_kind: CustodyKind,
    custody_target: &str,
) -> Result<Option<AccountCustody>, MemoryError> {
    let Some(existing) = get_account_custody_by_auth_ref(conn, auth_ref)? else {
        return Ok(None);
    };
    if existing.custody_kind == custody_kind && existing.custody_target == custody_target {
        return Ok(Some(existing));
    }
    conn.execute(
        "UPDATE account_custody
            SET custody_kind = ?2, custody_target = ?3, revision = ?4, updated_at = ?5
          WHERE auth_ref = ?1",
        params![
            auth_ref,
            custody_kind.as_str(),
            custody_target,
            existing.revision + 1,
            now_utc_iso()
        ],
    )?;
    get_account_custody_by_auth_ref(conn, auth_ref)
}

fn custody_row(
    conn: &Connection,
    sql: &str,
    key: &str,
) -> Result<Option<AccountCustody>, MemoryError> {
    let row: Option<(String, String, String, String, i64, String)> = conn
        .query_row(sql, params![key], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .optional()?;

    row.map(
        |(auth_ref, account_id, kind, custody_target, revision, updated_at)| -> Result<
            AccountCustody,
            MemoryError,
        > {
            Ok(AccountCustody {
                auth_ref,
                account_id,
                custody_kind: parse_custody_kind(&kind)?,
                custody_target,
                revision,
                updated_at,
            })
        },
    )
    .transpose()
}

fn parse_custody_kind(value: &str) -> Result<CustodyKind, MemoryError> {
    CustodyKind::parse(value).ok_or_else(|| {
        MemoryError::Internal(format!(
            "account_custody.custody_kind '{value}' is outside the closed set this build knows"
        ))
    })
}

/// The raw column tuple of a `provider_accounts` row, before the enum and JSON
/// columns are parsed. Kept as a separate step so a stored value outside a
/// closed set surfaces as a typed error naming the column, not as a
/// `FromSqlConversionFailure` from inside a row callback.
struct RawAccountRow {
    account_id: String,
    provider_kind: String,
    auth_mode: String,
    auth_ref: Option<String>,
    account_fingerprint: String,
    account_class: String,
    capabilities: String,
    credential_policy_ref: Option<String>,
    refresh_authority: String,
    status: String,
    revision: i64,
    source_refs: String,
    created_at: String,
    updated_at: String,
}

impl RawAccountRow {
    fn parse(self) -> Result<ProviderAccount, MemoryError> {
        let auth_mode = AuthMode::parse(&self.auth_mode).ok_or_else(|| {
            MemoryError::Internal(format!(
                "provider_accounts.auth_mode '{}' is outside the closed set this build knows",
                self.auth_mode
            ))
        })?;
        let account_class = AccountClass::parse(&self.account_class).ok_or_else(|| {
            MemoryError::Internal(format!(
                "provider_accounts.account_class '{}' is outside the closed set this build knows",
                self.account_class
            ))
        })?;
        Ok(ProviderAccount {
            account_id: self.account_id,
            provider_kind: self.provider_kind,
            auth_mode,
            auth_ref: self.auth_ref,
            account_fingerprint: self.account_fingerprint,
            account_class,
            capabilities: serde_json::from_str(&self.capabilities)?,
            credential_policy_ref: self.credential_policy_ref,
            refresh_authority: self.refresh_authority,
            status: self.status,
            revision: self.revision,
            source_refs: serde_json::from_str(&self.source_refs)?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// `Option<RawAccountRow>` → `Option<ProviderAccount>` without losing the
/// parse error.
trait TransposeParse {
    fn transpose_parse(self) -> Result<Option<ProviderAccount>, MemoryError>;
}

impl TransposeParse for Option<RawAccountRow> {
    fn transpose_parse(self) -> Result<Option<ProviderAccount>, MemoryError> {
        self.map(|row| row.parse()).transpose()
    }
}

fn account_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawAccountRow> {
    Ok(RawAccountRow {
        account_id: row.get(0)?,
        provider_kind: row.get(1)?,
        auth_mode: row.get(2)?,
        auth_ref: row.get(3)?,
        account_fingerprint: row.get(4)?,
        account_class: row.get(5)?,
        capabilities: row.get(6)?,
        credential_policy_ref: row.get(7)?,
        refresh_authority: row.get(8)?,
        status: row.get(9)?,
        revision: row.get(10)?,
        source_refs: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}
