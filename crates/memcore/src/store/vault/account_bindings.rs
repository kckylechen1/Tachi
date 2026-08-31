//! Account identity and alias events inside the caller's Vault transaction.

use super::VaultTransaction;
use crate::db;
use crate::error::MemoryError;
use crate::vault::accounts::{
    mint_account_id, mint_auth_ref, AccountClass, AuthMode, CustodyKind, NewProviderAccount,
    NewProviderAccountEvent, ProviderAccount, ACCOUNT_STATUS_ACTIVE, EVENT_KIND_ALIAS_OBSERVED,
    EVENT_KIND_ALIAS_RETIRED, EVENT_KIND_FINGERPRINT_OBSERVED, EVENT_KIND_SLOT_REBIND,
};

impl VaultTransaction<'_> {
    /// Whether replacing this physical entry must also update a durable
    /// account identity. This check shares the caller's write transaction.
    pub fn vault_account_entry_is_tracked(&self, entry_name: &str) -> Result<bool, MemoryError> {
        for account in db::list_provider_accounts(self.connection())? {
            if db::get_account_custody(self.connection(), &account.account_id)?
                .is_some_and(|custody| custody.custody_target == entry_name)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Observe one already-validated ModelApi entry. Existing custody is the
    /// rotation-stable identity. A fingerprint match with different custody
    /// requires explicit reconciliation, never an implicit move or a second
    /// account. Normal entry writes use `create_if_missing = false`; a first
    /// slot binding admits the account and its custody together.
    pub fn vault_observe_model_account_entry(
        &self,
        entry_name: &str,
        provider_kind: &str,
        account_fingerprint: &str,
        create_if_missing: bool,
    ) -> Result<Option<ProviderAccount>, MemoryError> {
        let conn = self.connection();
        let mut owners = Vec::new();
        for account in db::list_provider_accounts(conn)? {
            if let Some(custody) = db::get_account_custody(conn, &account.account_id)? {
                if custody.custody_target == entry_name {
                    if custody.custody_kind != CustodyKind::VaultEntry {
                        return Err(MemoryError::InvalidArg(
                            "account custody is a rotation pool, not this single Vault entry"
                                .into(),
                        ));
                    }
                    owners.push(account);
                }
            }
        }
        if owners.is_empty() && !create_if_missing {
            return Ok(None);
        }
        if owners.is_empty()
            && !db::find_provider_accounts_by_fingerprint(conn, account_fingerprint)?.is_empty()
        {
            return Err(MemoryError::InvalidArg(
                "matching credential already has different account custody; bind its canonical Vault entry or reconcile the duplicate before binding"
                    .into(),
            ));
        }
        if owners.len() > 1 {
            return Err(MemoryError::InvalidArg(
                "ambiguous provider account identity; reconcile the accounts before binding".into(),
            ));
        }
        let account = match owners.pop() {
            Some(account) => {
                if account.provider_kind != provider_kind
                    || account.account_class != AccountClass::ModelApi
                    || account.auth_mode != AuthMode::ApiKeyPool
                    || account.status != ACCOUNT_STATUS_ACTIVE
                {
                    return Err(MemoryError::InvalidArg(
                        "provider account identity is not an active matching ModelApi account"
                            .into(),
                    ));
                }
                db::record_account_fingerprint(
                    conn,
                    &account.account_id,
                    Some(account.revision),
                    account_fingerprint,
                    EVENT_KIND_FINGERPRINT_OBSERVED,
                    None,
                    &serde_json::json!({
                        "old_fingerprint": account.account_fingerprint,
                        "new_fingerprint": account_fingerprint,
                    })
                    .to_string(),
                )?;
                db::get_provider_account(conn, &account.account_id)?.ok_or_else(|| {
                    MemoryError::Internal("observed provider account disappeared".into())
                })?
            }
            None => {
                let mut new = NewProviderAccount::api_key_pool(
                    mint_account_id(),
                    provider_kind,
                    mint_auth_ref(),
                    account_fingerprint,
                    AccountClass::ModelApi,
                );
                new.source_refs.push(format!("vault:{entry_name}"));
                let account = db::insert_provider_account(conn, &new)?;
                db::insert_account_custody(
                    conn,
                    account
                        .auth_ref
                        .as_deref()
                        .expect("API-key accounts have custody"),
                    &account.account_id,
                    CustodyKind::VaultEntry,
                    entry_name,
                )?;
                account
            }
        };
        let observed = db::record_provider_account_alias(
            conn,
            &account.account_id,
            entry_name,
            "vault_entry",
        )?;
        if observed != db::vault_accounts::AliasObservation::Refreshed {
            db::append_provider_account_event(
                conn,
                &NewProviderAccountEvent::new(
                    &account.account_id,
                    account.revision,
                    EVENT_KIND_ALIAS_OBSERVED,
                )
                .with_evidence(
                    serde_json::json!({"alias_name": entry_name, "source_kind": "vault_entry"})
                        .to_string(),
                ),
            )?;
        }
        Ok(Some(account))
    }

    /// Plain secret deletion cannot implicitly retire an active account.
    /// Removing a lane slot only retires that slot alias, with its event in
    /// the same transaction as the caller's encrypted-row deletion.
    pub fn vault_prepare_account_entry_removal(&self, name: &str) -> Result<(), MemoryError> {
        let conn = self.connection();
        let accounts = db::list_provider_accounts(conn)?;
        for account in accounts
            .iter()
            .filter(|account| account.status == ACCOUNT_STATUS_ACTIVE)
        {
            let owns_custody = db::get_account_custody(conn, &account.account_id)?
                .is_some_and(|custody| custody.custody_target == name);
            let owns_alias = db::list_provider_account_aliases(conn, &account.account_id)?
                .iter()
                .any(|alias| {
                    !alias.retired && alias.alias_name == name && alias.source_kind != "lane_slot"
                });
            if owns_custody || owns_alias {
                return Err(MemoryError::InvalidArg(format!(
                    "Vault entry '{name}' backs active provider-account custody or aliases; reconcile or explicitly retire that account before deletion"
                )));
            }
        }
        for account in accounts {
            let is_slot_alias = db::list_provider_account_aliases(conn, &account.account_id)?
                .iter()
                .any(|alias| {
                    !alias.retired && alias.alias_name == name && alias.source_kind == "lane_slot"
                });
            if is_slot_alias && db::retire_provider_account_alias(conn, &account.account_id, name)?
            {
                db::append_provider_account_event(
                    conn,
                    &NewProviderAccountEvent::new(
                        &account.account_id,
                        account.revision,
                        EVENT_KIND_ALIAS_RETIRED,
                    )
                    .with_evidence(
                        serde_json::json!({
                            "alias_name": name,
                            "source_kind": "lane_slot",
                            "reason": "slot_removed",
                        })
                        .to_string(),
                    ),
                )?;
            }
        }
        Ok(())
    }

    /// Move a slot alias without deleting history. Event persistence is part of
    /// the same transaction as the encrypted pointer, for CLI and MCP callers.
    pub fn vault_record_slot_account_alias(
        &self,
        account: &ProviderAccount,
        slot: &str,
        old_fingerprint: Option<&str>,
        new_fingerprint: &str,
        rebound: bool,
    ) -> Result<(), MemoryError> {
        let conn = self.connection();
        let evidence = serde_json::json!({
            "slot": slot,
            "old_fingerprint": old_fingerprint,
            "new_fingerprint": new_fingerprint,
            "account_id": account.account_id,
        })
        .to_string();
        for old in db::list_provider_accounts(conn)? {
            if old.account_id != account.account_id
                && db::retire_provider_account_alias(conn, &old.account_id, slot)?
            {
                db::append_provider_account_event(
                    conn,
                    &NewProviderAccountEvent::new(
                        &old.account_id,
                        old.revision,
                        EVENT_KIND_ALIAS_RETIRED,
                    )
                    .with_evidence(&evidence),
                )?;
            }
        }
        let observed =
            db::record_provider_account_alias(conn, &account.account_id, slot, "lane_slot")?;
        if rebound || observed != db::vault_accounts::AliasObservation::Refreshed {
            let kind = if rebound {
                EVENT_KIND_SLOT_REBIND
            } else {
                EVENT_KIND_ALIAS_OBSERVED
            };
            db::append_provider_account_event(
                conn,
                &NewProviderAccountEvent::new(&account.account_id, account.revision, kind)
                    .with_evidence(evidence),
            )?;
        }
        Ok(())
    }
}
