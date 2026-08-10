//! `tachi vault reconcile` — provider-account plan/apply (tachi#1680 D4).
//!
//! Four stages, and the boundary between them is the point of the design:
//!
//! 1. **discover** — locked-safe and read-only, over exactly the admitted
//!    sources `intake` already scans. No new source vocabulary, no execution
//!    of anything discovered.
//! 2. **normalize + fingerprint** — needs the Vault unlocked, because deciding
//!    "these two names hold the same credential" means comparing the values,
//!    and the fingerprint key is derived from the master key.
//! 3. **plan** — a typed [`memcore::BoundAccountPlan`] plus the `pd1:` digest
//!    over it, written to an artifact the operator can read and approve.
//! 4. **apply** — hands that plan to the store's single write transaction,
//!    which re-reads every binding before it writes anything.
//!
//! # What this module deliberately does not do
//!
//! - **It never executes a discovered value.** Env files are parsed as data by
//!   `intake`'s own parser, and a value shaped like command substitution is
//!   classified and refused rather than being "cleaned up" — refusing is the
//!   fail-safe answer to something we cannot explain.
//! - **It never imports a secret.** Only credentials the Vault already holds
//!   become accounts. An env-file value with no Vault entry behind it is
//!   surfaced as an advisory, because bringing a secret under custody is an
//!   explicit operator action and not something a reconcile pass may do on the
//!   strength of having noticed a file.
//! - **It never reads a provider-owned session.** `intake` discovers
//!   `~/.codex/auth.json` as metadata; reconcile does not touch it. A session
//!   the provider itself owns is observed, never copied or claimed as custody.
//! - **It never merges two accounts.** Registry alias families (`EXTRACT_API_KEY`
//!   is a name for the SiliconFlow family) are a naming fact, not evidence that
//!   two *accounts* are one. Two names collapse into one account only when their
//!   values fingerprint identically; anything weaker is an advisory line, and
//!   fusing two persisted identities needs an operator confirmation the planner
//!   cannot supply.
//! - **It never touches `providers doctor`.** The doctor stays a pure report
//!   over the same read-only primitives.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use memcore::{
    AccountAction, AccountBinding, AccountClass, AliasSighting, AuthMode, BoundAccountPlan,
    CustodyBinding, CustodyKind, FingerprintKey, MemoryStore, PlanBindings, PlanSourceDigests,
    PlannedAccount, SourceBinding, VaultEntryBinding, VaultPoolBinding,
};
use serde::{Deserialize, Serialize};
use tachi_bootstrap::cli::{VaultAction, VaultReconcileAction};

use super::keys::{read_vault_config_for_key, read_verified_vault_key};
use super::{open_cli_store, open_cli_store_read_only};

/// Schema tag of the on-disk plan artifact. Checked as an exact literal at
/// apply: a plan from a build that meant something else by these fields is
/// refused rather than reinterpreted.
const PLAN_SCHEMA: &str = "tachi.provider-reconcile-plan.v1";

/// Descriptor scheme for a discovered env file. The reconcile pipeline owns
/// this grammar (memcore treats `source_id` as opaque), and apply resolves it
/// back to a path to re-hash the bytes.
const SOURCE_SCHEME_ENV_FILE: &str = "env_file:";

/// `provider_account_aliases.source_kind` values this pipeline mints.
const SOURCE_KIND_VAULT_ENTRY: &str = "vault_entry";
const SOURCE_KIND_CONFIG_ENV: &str = "config_env";

// ─── The artifact ───────────────────────────────────────────────────────────

/// What `plan` writes and `apply` reads.
///
/// It carries no rendered prose on purpose. A human-readable summary stored
/// beside the digest would be a lie surface — edit only the description and the
/// digest still verifies while the operator approves a paragraph the plan does
/// not implement. Both `plan` and `apply` render their human view from `bound`
/// instead, so what is displayed is always what is digested.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReconcilePlanArtifact {
    schema: String,
    generated_at: String,
    plan_digest: String,
    bound: BoundAccountPlan,
}

/// Re-reads a plan's env-file sources from inside apply's transaction.
struct EnvFileSources;

impl PlanSourceDigests for EnvFileSources {
    fn current_digest(&self, source_id: &str) -> Result<Option<String>, String> {
        let Some(path) = source_id.strip_prefix(SOURCE_SCHEME_ENV_FILE) else {
            return Err(format!(
                "source descriptor '{source_id}' is not one this build knows how to re-read"
            ));
        };
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(tachi_params::sha256_hex(&bytes))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }
}

// ─── CLI entry ──────────────────────────────────────────────────────────────

pub(super) fn run_reconcile_action(
    global_db_path: &PathBuf,
    action: VaultAction,
) -> Result<(), Box<dyn std::error::Error>> {
    let VaultAction::Reconcile { action } = action else {
        unreachable!("reconcile router received non-reconcile action");
    };
    let cwd = std::env::current_dir()?;
    let env_home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

    match action {
        VaultReconcileAction::Plan {
            json,
            out,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        } => {
            let config = read_vault_config_for_key(global_db_path)?;
            let key = read_verified_vault_key(
                &config,
                stdin_password,
                keychain,
                password_file.as_deref(),
                insecure_password_file,
            )?;
            let store = open_cli_store_read_only(global_db_path)?;
            let outcome = build_plan(&store, key.bytes(), &env_home, &cwd)?;
            let artifact = ReconcilePlanArtifact {
                schema: PLAN_SCHEMA.to_string(),
                generated_at: chrono::Utc::now().to_rfc3339(),
                plan_digest: memcore::plan_digest(&outcome.plan),
                bound: outcome.plan,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&artifact)?);
            } else {
                print!("{}", render_plan(&artifact, &outcome.advisories));
            }
            if let Some(path) = out {
                std::fs::write(&path, serde_json::to_string_pretty(&artifact)?)?;
                println!("wrote reconcile plan to {}", path.display());
            }
        }
        VaultReconcileAction::Apply { plan, json } => {
            let raw = std::fs::read_to_string(&plan)?;
            let artifact: ReconcilePlanArtifact = serde_json::from_str(&raw)?;
            if artifact.schema != PLAN_SCHEMA {
                return Err(format!(
                    "plan schema '{}' is not '{PLAN_SCHEMA}'; refusing to guess what it meant",
                    artifact.schema
                )
                .into());
            }
            // Re-render from the digested content before applying: what the
            // operator sees at apply time is the plan itself, never a stored
            // description of it.
            if !json {
                print!("{}", render_plan(&artifact, &[]));
            }
            let mut store = open_cli_store(global_db_path)?;
            let report = store.apply_provider_account_plan(
                &artifact.bound,
                &artifact.plan_digest,
                &EnvFileSources,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", render_apply(&report));
            }
        }
    }
    Ok(())
}

// ─── Stage 1-2: discover, normalize, fingerprint ────────────────────────────

/// One env-var name observed in one source file, with its normalized value.
struct EnvSighting {
    source_id: String,
    logical_name: String,
    value: String,
    /// Whether the raw value looks like something meant to be executed.
    executable: bool,
}

/// One credential the Vault actually holds, fingerprinted.
struct VaultCredential {
    /// The logical name: a `vault_entries.name` or a rotation-pool prefix.
    /// Never a pool member — member indices are custody layout (#1680 D5).
    logical_name: String,
    provider_kind: &'static str,
    account_class: AccountClass,
    custody_kind: CustodyKind,
    /// `fp1:` per member value, deduplicated and sorted.
    member_fingerprints: Vec<String>,
    /// Entry names bound by `updated_at`. Pool members are bound through
    /// `pool_prefix` instead, so no member name ever reaches the plan.
    bound_entry_names: Vec<String>,
    pool_prefix: Option<String>,
}

/// A note for the operator that is not an action. Advisories never appear in
/// the artifact: they describe what the plan chose *not* to do, which is
/// exactly the part that must not be able to masquerade as approved content.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Advisory {
    code: String,
    subject: String,
    detail: String,
}

struct PlanOutcome {
    plan: BoundAccountPlan,
    advisories: Vec<Advisory>,
}

/// Whether a discovered value carries a shell command-substitution shape.
///
/// Nothing in this pipeline ever hands a discovered value to a shell, so this
/// is not what stops execution — the parser treating every value as data is.
/// It is the *visible* half of discrimination 3: a value that only makes sense
/// as something to run is not a credential, and turning it into an account
/// would be pretending we understood it.
///
/// Deliberately narrow. `${vault:A|B}` is Tachi's own indirection syntax and a
/// base64 key can contain almost anything, so only shapes with no reading as
/// literal credential material are flagged: `$(…)`, backticks, process
/// substitution, and a shebang.
fn looks_executable(value: &str) -> bool {
    value.contains("$(")
        || value.contains('`')
        || value.contains("<(")
        || value.contains(">(")
        || value.trim_start().starts_with("#!")
}

/// Trim the incidental whitespace an env file adds. This is the normalization
/// step `vault::fingerprint` deliberately refuses to do for its callers: it is
/// a policy about what counts as the same credential, and policy belongs here.
fn normalize_value(value: &str) -> &str {
    value.trim()
}

fn discover_env_sightings(env_home: &Path, cwd: &Path) -> (Vec<SourceBinding>, Vec<EnvSighting>) {
    let mut sources = Vec::new();
    let mut sightings = Vec::new();
    for path in super::intake::env_source_paths(env_home, cwd) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let source_id = format!("{SOURCE_SCHEME_ENV_FILE}{}", path.display());
        sources.push(SourceBinding {
            source_id: source_id.clone(),
            sha256: tachi_params::sha256_hex(&bytes),
        });
        for (_, logical_name, raw_value) in super::intake::parse_env_file(&path) {
            sightings.push(EnvSighting {
                source_id: source_id.clone(),
                logical_name,
                executable: looks_executable(&raw_value),
                value: normalize_value(&raw_value).to_string(),
            });
        }
    }
    (sources, sightings)
}

/// Fingerprint every admitted credential the Vault holds.
fn fingerprint_vault_credentials(
    store: &MemoryStore,
    master_key: &[u8; 32],
    fp_key: &FingerprintKey,
    advisories: &mut Vec<Advisory>,
) -> Result<Vec<VaultCredential>, Box<dyn std::error::Error>> {
    let admitted = crate::status_ops::status_health::admitted_env_secret_names();
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("vault_list_entries: {e}"))?;
    let rotations = store
        .vault_list_rotations()
        .map_err(|e| format!("vault_list_rotations: {e}"))?;

    let pool_prefixes: Vec<String> = rotations
        .iter()
        .map(|rotation| rotation.prefix.clone())
        .filter(|prefix| admitted.contains(prefix))
        .collect();

    let mut credentials: Vec<VaultCredential> = Vec::new();
    let mut consumed: HashSet<String> = HashSet::new();

    for prefix in &pool_prefixes {
        // Members plus a standalone entry under the prefix itself: both are the
        // same logical credential, and both must be fingerprinted or a rotation
        // would look like a value change it is not.
        let mut members: Vec<(usize, &memcore::VaultEntry)> = Vec::new();
        let mut bound_entry_names = Vec::new();
        for entry in &entries {
            if let Some(index) = memcore::api_key_pool_member_index(&entry.name, prefix) {
                members.push((index, entry));
                consumed.insert(entry.name.clone());
            } else if &entry.name == prefix {
                members.push((0, entry));
                bound_entry_names.push(entry.name.clone());
                consumed.insert(entry.name.clone());
            }
        }
        if members.is_empty() {
            advisories.push(Advisory {
                code: "empty_rotation_pool".to_string(),
                subject: prefix.clone(),
                detail: "a rotation row exists with no member entries; nothing to fingerprint"
                    .to_string(),
            });
            continue;
        }
        members.sort_by_key(|(index, _)| *index);

        let Some(provider_kind) =
            crate::status_ops::status_health::provider_kind_for_env_name(prefix)
        else {
            continue;
        };
        let account_class = crate::status_ops::status_health::account_class_for_env_name(prefix)
            .unwrap_or(AccountClass::ModelApi);
        let member_fingerprints = fingerprint_entries(
            master_key,
            fp_key,
            provider_kind,
            members.into_iter().map(|(_, entry)| entry),
            advisories,
        );
        if member_fingerprints.is_empty() {
            continue;
        }
        credentials.push(VaultCredential {
            logical_name: prefix.clone(),
            provider_kind,
            account_class,
            custody_kind: CustodyKind::VaultRotationPool,
            member_fingerprints,
            bound_entry_names,
            pool_prefix: Some(prefix.clone()),
        });
    }

    for entry in &entries {
        if consumed.contains(&entry.name) || !admitted.contains(&entry.name) {
            continue;
        }
        let Some(provider_kind) =
            crate::status_ops::status_health::provider_kind_for_env_name(&entry.name)
        else {
            continue;
        };
        let account_class =
            crate::status_ops::status_health::account_class_for_env_name(&entry.name)
                .unwrap_or(AccountClass::ModelApi);
        let member_fingerprints = fingerprint_entries(
            master_key,
            fp_key,
            provider_kind,
            std::iter::once(entry),
            advisories,
        );
        if member_fingerprints.is_empty() {
            continue;
        }
        credentials.push(VaultCredential {
            logical_name: entry.name.clone(),
            provider_kind,
            account_class,
            custody_kind: CustodyKind::VaultEntry,
            member_fingerprints,
            bound_entry_names: vec![entry.name.clone()],
            pool_prefix: None,
        });
    }

    credentials.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
    Ok(credentials)
}

/// Decrypt, normalize and fingerprint a set of Vault entries. Plaintext is
/// zeroed the moment its fingerprint exists.
fn fingerprint_entries<'a>(
    master_key: &[u8; 32],
    fp_key: &FingerprintKey,
    provider_kind: &str,
    entries: impl Iterator<Item = &'a memcore::VaultEntry>,
    advisories: &mut Vec<Advisory>,
) -> Vec<String> {
    let mut fingerprints = Vec::new();
    for entry in entries {
        if entry.secret_type != memcore::SECRET_TYPE_API_KEY {
            advisories.push(Advisory {
                code: "unsupported_secret_type".to_string(),
                subject: entry.name.clone(),
                detail: format!(
                    "secret_type '{}' is not an API-key pool credential; no account is planned",
                    entry.secret_type
                ),
            });
            continue;
        }
        let Ok(plaintext) =
            crate::vault_crypto::decrypt(master_key, &entry.encrypted_value, &entry.nonce)
        else {
            advisories.push(Advisory {
                code: "undecryptable_entry".to_string(),
                subject: entry.name.clone(),
                detail: "value did not decrypt under the unlocked master key; no fingerprint is \
                         computed and no account is planned"
                    .to_string(),
            });
            continue;
        };
        let mut value = match String::from_utf8(plaintext) {
            Ok(value) => value,
            Err(_) => {
                advisories.push(Advisory {
                    code: "non_utf8_secret".to_string(),
                    subject: entry.name.clone(),
                    detail: "value is not UTF-8; no fingerprint is computed".to_string(),
                });
                continue;
            }
        };
        let fingerprint = fp_key.key_fingerprint(provider_kind, normalize_value(&value));
        crate::vault_crypto::zero_string(&mut value);
        fingerprints.push(fingerprint);
    }
    fingerprints.sort();
    fingerprints.dedup();
    fingerprints
}

// ─── Stage 3: plan ──────────────────────────────────────────────────────────

fn build_plan(
    store: &MemoryStore,
    master_key: &[u8; 32],
    env_home: &Path,
    cwd: &Path,
) -> Result<PlanOutcome, Box<dyn std::error::Error>> {
    let mut advisories = Vec::new();
    let fp_key = FingerprintKey::derive_from_master_key(master_key);

    let (sources, sightings) = discover_env_sightings(env_home, cwd);
    let credentials = fingerprint_vault_credentials(store, master_key, &fp_key, &mut advisories)?;

    // Group interchangeable credentials: one account per (family, member set).
    // Grouping is by fingerprint evidence and never by name — two names of one
    // registry family holding *different* values stay two accounts.
    let mut groups: BTreeMap<(String, String), Vec<VaultCredential>> = BTreeMap::new();
    for credential in credentials {
        let account_fingerprint =
            fp_key.account_fingerprint_from_members(credential.member_fingerprints.iter());
        groups
            .entry((credential.provider_kind.to_string(), account_fingerprint))
            .or_default()
            .push(credential);
    }

    let conn = store.connection();
    let existing_accounts = memcore::list_provider_accounts(conn)?;
    let mut custody_target_index: HashMap<String, String> = HashMap::new();
    for account in &existing_accounts {
        if let Some(custody) = memcore::get_account_custody(conn, &account.account_id)? {
            custody_target_index.insert(custody.custody_target.clone(), account.account_id.clone());
        }
    }

    let mut bindings = PlanBindings {
        sources,
        ..PlanBindings::default()
    };
    let mut actions: Vec<AccountAction> = Vec::new();
    let mut bound_entries: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut bound_pools: BTreeMap<String, String> = BTreeMap::new();
    let mut bound_accounts: BTreeMap<String, AccountBinding> = BTreeMap::new();
    let mut claimed_sightings: HashSet<usize> = HashSet::new();

    let entry_updated_at: HashMap<String, String> = store
        .vault_list_entry_timestamps()
        .map_err(|e| format!("vault_list_entry_timestamps: {e}"))?
        .into_iter()
        .collect();

    for ((provider_kind, account_fingerprint), group) in groups {
        // Custody points at one object. Prefer the registry's canonical key so
        // the choice is the registry's rather than an accident of ordering.
        let custody_credential = pick_custody_credential(&group);
        let mut alias_names: BTreeSet<String> =
            group.iter().map(|c| c.logical_name.clone()).collect();
        let mut source_refs: BTreeSet<String> = BTreeSet::new();

        for credential in &group {
            for name in &credential.bound_entry_names {
                bound_entries.insert(name.clone(), entry_updated_at.get(name).cloned());
            }
            if let Some(prefix) = &credential.pool_prefix {
                bound_pools.insert(
                    prefix.clone(),
                    memcore::vault_pool_members_digest(conn, prefix)?,
                );
            }
            source_refs.insert(format!("vault:{}", credential.logical_name));
        }

        // An env-file name joins this account only when its value fingerprints
        // to one of the account's members under the same provider family.
        let member_set: HashSet<&String> = group
            .iter()
            .flat_map(|credential| credential.member_fingerprints.iter())
            .collect();
        for (index, sighting) in sightings.iter().enumerate() {
            if sighting.executable {
                continue;
            }
            if crate::status_ops::status_health::provider_kind_for_env_name(&sighting.logical_name)
                != Some(provider_kind.as_str())
            {
                continue;
            }
            let fingerprint = fp_key.key_fingerprint(&provider_kind, &sighting.value);
            if !member_set.contains(&fingerprint) {
                continue;
            }
            if memcore::names_rotation_pool_member(&sighting.logical_name) {
                advisories.push(Advisory {
                    code: "member_shaped_alias_skipped".to_string(),
                    subject: sighting.logical_name.clone(),
                    detail: "names a rotation-pool member; pool layout stays in custody"
                        .to_string(),
                });
                continue;
            }
            alias_names.insert(sighting.logical_name.clone());
            source_refs.insert(sighting.source_id.clone());
            claimed_sightings.insert(index);
        }

        let existing = resolve_existing_account(
            conn,
            &account_fingerprint,
            &custody_credential.logical_name,
            &custody_target_index,
            &mut advisories,
        )?;

        match existing {
            Some(account) => {
                bound_accounts.insert(
                    account.account_id.clone(),
                    AccountBinding {
                        account_id: account.account_id.clone(),
                        revision: account.revision,
                        auth_ref: account.auth_ref.clone(),
                        credential_policy_ref: account.credential_policy_ref.clone(),
                    },
                );
                if let Some(auth_ref) = &account.auth_ref {
                    if let Some(custody) = memcore::get_account_custody_by_auth_ref(conn, auth_ref)?
                    {
                        bindings.custody.push(CustodyBinding {
                            auth_ref: auth_ref.clone(),
                            revision: custody.revision,
                        });
                    }
                }
                if account.account_fingerprint != account_fingerprint {
                    actions.push(AccountAction::RecordFingerprint {
                        account_id: account.account_id.clone(),
                        account_fingerprint: account_fingerprint.clone(),
                        event_kind: memcore::vault::accounts::EVENT_KIND_FINGERPRINT_OBSERVED
                            .to_string(),
                        evidence: member_evidence(&group),
                    });
                }
                let known: HashSet<String> =
                    memcore::list_provider_account_aliases(conn, &account.account_id)?
                        .into_iter()
                        .filter(|alias| !alias.retired)
                        .map(|alias| alias.alias_name)
                        .collect();
                for alias_name in &alias_names {
                    if known.contains(alias_name) {
                        continue;
                    }
                    actions.push(AccountAction::ObserveAlias {
                        account_id: account.account_id.clone(),
                        alias_name: alias_name.clone(),
                        source_kind: alias_source_kind(alias_name, &group),
                    });
                }
            }
            None => {
                let account_id = memcore::mint_account_id();
                actions.push(AccountAction::CreateAccount {
                    account: Box::new(PlannedAccount {
                        account_id,
                        provider_kind: provider_kind.clone(),
                        auth_mode: AuthMode::ApiKeyPool,
                        account_class: custody_credential.account_class,
                        account_fingerprint: account_fingerprint.clone(),
                        auth_ref: memcore::mint_auth_ref(),
                        custody_kind: custody_credential.custody_kind,
                        custody_logical_name: custody_credential.logical_name.clone(),
                        capabilities: Vec::new(),
                        credential_policy_ref: None,
                        source_refs: source_refs.iter().cloned().collect(),
                        aliases: alias_names
                            .iter()
                            .map(|alias_name| AliasSighting {
                                alias_name: alias_name.clone(),
                                source_kind: alias_source_kind(alias_name, &group),
                            })
                            .collect(),
                        evidence: member_evidence(&group),
                    }),
                });
            }
        }
    }

    for (index, sighting) in sightings.iter().enumerate() {
        if claimed_sightings.contains(&index) {
            continue;
        }
        advise_unclaimed(sighting, &mut advisories);
    }

    bindings.vault_entries = bound_entries
        .into_iter()
        .map(|(entry_name, updated_at)| VaultEntryBinding {
            entry_name,
            updated_at,
        })
        .collect();
    bindings.vault_pools = bound_pools
        .into_iter()
        .map(|(prefix, members_digest)| VaultPoolBinding {
            prefix,
            members_digest,
        })
        .collect();
    bindings.accounts = bound_accounts.into_values().collect();
    bindings.custody.sort_by(|a, b| a.auth_ref.cmp(&b.auth_ref));
    bindings.custody.dedup_by(|a, b| a.auth_ref == b.auth_ref);
    advisories.sort_by(|a, b| (&a.code, &a.subject).cmp(&(&b.code, &b.subject)));

    Ok(PlanOutcome {
        plan: BoundAccountPlan { bindings, actions },
        advisories,
    })
}

/// Which of a group's interchangeable names custody points at.
fn pick_custody_credential(group: &[VaultCredential]) -> &VaultCredential {
    group
        .iter()
        .find(|credential| {
            crate::status_ops::status_health::canonical_key_for_env_name(&credential.logical_name)
                == Some(credential.logical_name.as_str())
        })
        .unwrap_or_else(|| {
            group
                .iter()
                .min_by(|a, b| a.logical_name.cmp(&b.logical_name))
                .expect("a group is never empty")
        })
}

/// The persisted account this credential group belongs to, if any.
///
/// Fingerprint evidence first — that is identity. Custody target second, which
/// is the rotation case: the same Vault object, new key material, so the
/// account is the same account and its fingerprint is what moves. Two accounts
/// carrying one fingerprint is reported and never guessed at.
fn resolve_existing_account(
    conn: &rusqlite::Connection,
    account_fingerprint: &str,
    custody_logical_name: &str,
    custody_target_index: &HashMap<String, String>,
    advisories: &mut Vec<Advisory>,
) -> Result<Option<memcore::ProviderAccount>, Box<dyn std::error::Error>> {
    let by_fingerprint = memcore::find_provider_accounts_by_fingerprint(conn, account_fingerprint)?;
    match by_fingerprint.len() {
        1 => return Ok(Some(by_fingerprint.into_iter().next().expect("len == 1"))),
        0 => {}
        _ => {
            advisories.push(Advisory {
                code: "ambiguous_account_fingerprint".to_string(),
                subject: account_fingerprint.to_string(),
                detail: "more than one account carries this fingerprint; reconcile plans nothing \
                         for it rather than picking one"
                    .to_string(),
            });
            return Ok(None);
        }
    }

    let Some(account_id) = custody_target_index.get(custody_logical_name) else {
        return Ok(None);
    };
    Ok(memcore::get_provider_account(conn, account_id)?)
}

fn alias_source_kind(alias_name: &str, group: &[VaultCredential]) -> String {
    if group
        .iter()
        .any(|credential| credential.logical_name == alias_name)
    {
        SOURCE_KIND_VAULT_ENTRY.to_string()
    } else {
        SOURCE_KIND_CONFIG_ENV.to_string()
    }
}

/// Public-safe evidence for an account event: how many distinct key
/// fingerprints back it, never the fingerprints of anything else.
fn member_evidence(group: &[VaultCredential]) -> String {
    let members: BTreeSet<&String> = group
        .iter()
        .flat_map(|credential| credential.member_fingerprints.iter())
        .collect();
    serde_json::json!({
        "member_count": members.len(),
        "member_fingerprints": members.iter().collect::<Vec<_>>(),
    })
    .to_string()
}

fn advise_unclaimed(sighting: &EnvSighting, advisories: &mut Vec<Advisory>) {
    if sighting.executable {
        advisories.push(Advisory {
            code: "refused_executable_candidate".to_string(),
            subject: sighting.logical_name.clone(),
            detail: "value carries a command-substitution shape; treated as data and refused, \
                     never executed"
                .to_string(),
        });
        return;
    }
    if crate::status_ops::status_health::provider_kind_for_env_name(&sighting.logical_name)
        .is_none()
    {
        return;
    }
    advisories.push(Advisory {
        code: "not_under_vault_custody".to_string(),
        subject: sighting.logical_name.clone(),
        detail: "recognized provider name whose value the Vault does not hold; reconcile never \
                 imports a secret, so no account is planned"
            .to_string(),
    });
}

// ─── Rendering ──────────────────────────────────────────────────────────────

fn render_plan(artifact: &ReconcilePlanArtifact, advisories: &[Advisory]) -> String {
    let mut out = String::new();
    out.push_str(&format!("PLAN\t{}\n", artifact.plan_digest));
    if artifact.bound.actions.is_empty() {
        out.push_str("(no provider-account actions; the recorded accounts already match)\n");
    }
    for action in &artifact.bound.actions {
        out.push_str(&render_action(action));
    }
    for binding in &artifact.bound.bindings.accounts {
        out.push_str(&format!(
            "BOUND\taccount\t{}\trevision={}\n",
            binding.account_id, binding.revision
        ));
    }
    for binding in &artifact.bound.bindings.vault_entries {
        out.push_str(&format!(
            "BOUND\tvault_entry\t{}\t{}\n",
            binding.entry_name,
            binding.updated_at.as_deref().unwrap_or("(absent)")
        ));
    }
    for binding in &artifact.bound.bindings.vault_pools {
        out.push_str(&format!("BOUND\tvault_pool\t{}\n", binding.prefix));
    }
    for binding in &artifact.bound.bindings.sources {
        out.push_str(&format!("BOUND\tsource\t{}\n", binding.source_id));
    }
    for advisory in advisories {
        out.push_str(&format!(
            "ADVISORY\t{}\t{}\t{}\n",
            advisory.code, advisory.subject, advisory.detail
        ));
    }
    out
}

fn render_action(action: &AccountAction) -> String {
    match action {
        AccountAction::CreateAccount { account } => format!(
            "ACTION\tcreate_account\t{}\t{}\t{}\taliases=[{}]\n",
            account.account_id,
            account.provider_kind,
            account.account_class.as_str(),
            account
                .aliases
                .iter()
                .map(|alias| alias.alias_name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
        AccountAction::ObserveAlias {
            account_id,
            alias_name,
            source_kind,
        } => format!("ACTION\tobserve_alias\t{account_id}\t{alias_name}\t{source_kind}\n"),
        AccountAction::RetireAlias {
            account_id,
            alias_name,
        } => format!("ACTION\tretire_alias\t{account_id}\t{alias_name}\n"),
        AccountAction::RecordFingerprint {
            account_id,
            account_fingerprint,
            ..
        } => format!("ACTION\trecord_fingerprint\t{account_id}\t{account_fingerprint}\n"),
        AccountAction::RepointCustody { auth_ref, .. } => {
            format!("ACTION\trepoint_custody\t{auth_ref}\n")
        }
        AccountAction::MergeAccounts {
            from_account_id,
            into_account_id,
            confirmation,
        } => format!(
            "ACTION\tmerge_accounts\t{from_account_id}\t{into_account_id}\tconfirmed={}\n",
            confirmation.is_some()
        ),
    }
}

fn render_apply(report: &memcore::AccountApplyReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "APPLIED\t{}\tchanged={}\n",
        report.plan_digest, report.changed
    ));
    for account_id in &report.accounts_created {
        out.push_str(&format!("CREATED\taccount\t{account_id}\n"));
    }
    for alias in &report.aliases_created {
        out.push_str(&format!(
            "CREATED\talias\t{}\t{}\n",
            alias.account_id, alias.alias_name
        ));
    }
    for alias in &report.aliases_revived {
        out.push_str(&format!(
            "REVIVED\talias\t{}\t{}\n",
            alias.account_id, alias.alias_name
        ));
    }
    for alias in &report.aliases_retired {
        out.push_str(&format!(
            "RETIRED\talias\t{}\t{}\n",
            alias.account_id, alias.alias_name
        ));
    }
    for change in &report.fingerprints_advanced {
        out.push_str(&format!(
            "ADVANCED\tfingerprint\t{}\trevision={}\n",
            change.account_id, change.revision
        ));
    }
    for change in &report.accounts_retired {
        out.push_str(&format!(
            "RETIRED\taccount\t{}\trevision={}\n",
            change.account_id, change.revision
        ));
    }
    if !report.noop_event_ids.is_empty() {
        out.push_str(&format!(
            "NOOP\treplay recorded on {} account(s)\n",
            report.noop_event_ids.len()
        ));
    }
    out
}

#[cfg(test)]
mod tests;
