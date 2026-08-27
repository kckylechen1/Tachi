//! Execution-environment provisioning, binding, and reclaim (#894 S1 + S2c).
//!
//! This module is the daemon-side owner of the `exec_envs` lease lifecycle. It
//! composes the git-worktree primitive (`tachi_clean::wt_open::open_worktree`,
//! the single source for the worktree mechanics) with a daemon-owned lease row,
//! so there is exactly one provisioning entrypoint. Dispatch binds a lease via
//! [`resolve_env_binding`] (the fail-safe managed-vs-unmanaged gate) and every
//! reclaim flows through the one reclaim function in `memcore::db::exec_env`.
//!
//! ## Env classes (#894 S2c)
//!
//! Provisioning takes an [`EnvClass`] and that class decides exactly one thing:
//! **which physical resources get allocated and bound to the lease.**
//!
//! | class | worktree | build target bound to the lease |
//! |---|---|---|
//! | `edit-only` (default) | yes | **none** |
//! | `build-ticketed` | yes | **none** — the seat owns the target it builds in |
//! | `build-private` | yes | a private target dir — approval + reservation required, both booked |
//!
//! ### Why `build-ticketed` binds no target (round-2 fix)
//!
//! It reads like it should: the lease causes builds, so book the target. But the
//! dir those builds run in is the *executor seat's*, and which one — the resident
//! target or the fork scratch target — is decided per ticket, at run time, by
//! `build_broker::target::plan_target` from the ticket's lineage. Provisioning
//! cannot know. Round-1 bound the lease to a dir resolved from
//! `default_shared_cargo_target_dir()`, and the only production call site passed
//! `resident_target_dir: None`, so every ticketed lease ended up refcounting a
//! path **the broker never touches**: the ledger said the lease held a target,
//! while the seat built somewhere else entirely.
//!
//! The broker books the target it actually uses (registers the row, records it
//! on the executor slot, stamps its generation, quarantines it on an interrupt).
//! That is the accounting — one writer, on the dir that really got written.
//!
//! `edit-only` is a **disk and routing** policy, not a sandbox. Nothing here
//! stops a worker with a shell and the same UID from running `cargo` in an
//! edit-only tree — a PATH/tool-table fence is bypassable by anyone who can
//! spawn a process, and the owner ratified (2026-07-13) that we will not
//! pretend otherwise. What the class buys:
//!
//! - **disk**: no target dir is wired to the tree, so it stays ~14MB;
//! - **routing**: builds are supposed to go to the build broker's
//!   machine-unique serialized executor seat ([`crate::build_broker`]) instead
//!   of N diverged worktrees driving one shared `CARGO_TARGET_DIR`, which is
//!   what manufactured phantom "symbol not found" compile errors on this
//!   machine twice in one night.
//!
//! If someone ignores the convention and runs cargo in an edit-only tree
//! anyway, they get a *local* `target/` (reclaimable by the sweep) — they do
//! NOT get to poison the seat's shared target from a diverged tree. That
//! containment, not enforcement, is the guarantee.

use std::path::{Path, PathBuf};

use memcore::{
    EnvClass, ExecEnvLease, ExecEnvSelector, ExecEnvState, NewExecEnvLease, NewExecEnvResource,
    ReclaimOutcome, ResourceKind, ResourceState,
};
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use tachi_clean::wt_clean::OutputFormat;
use tachi_clean::wt_open::{open_worktree, CargoTargetPolicy, OpenOptions, OpenReport};

use crate::server_state::MemoryServer;

/// `hard_state` namespace for booked private-target reservations; key = env_id.
///
/// **Deliberately excluded from the #1342 follow-up TTL/backfill pass.** Its
/// lifecycle is owned by the #894 exec-env-disk-governor design (`exec_env_reaper`,
/// see that module's doc header), not by a generic `hard_state` `expires_at`
/// sweep — a private-target reservation is live/reclaim-worthy exactly when
/// the disk-governor ledger (`exec_env_resources`/leases) says so, which is a
/// different, already-load-bearing state machine than "past a wall-clock
/// timestamp". Bolting a TTL onto this namespace too would give two
/// independent, potentially-disagreeing reapers authority over the same row.
pub(crate) const PRIVATE_RESERVATION_NS: &str = "exec_env_private_target";

/// Resolve a worktree to the one persisted physical-path spelling. Existing
/// trees are fully canonicalized. For a claim made before its leaf is created,
/// the nearest existing ancestor is canonicalized and the future suffix is
/// appended, so aliases such as macOS `/tmp` still cannot fork identity.
pub(crate) fn canonical_worktree_path(worktree_path: &str) -> Result<String, String> {
    let input = Path::new(worktree_path);
    let absolute = if input.is_absolute() {
        input.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("cannot resolve current directory: {error}"))?
            .join(input)
    };
    let canonical = match std::fs::canonicalize(&absolute) {
        Ok(path) => path,
        Err(full_error) => {
            let mut ancestor = absolute.as_path();
            let mut suffix = Vec::new();
            // Three-state path doctrine (same spirit as
            // `bootstrap/clean_cli.rs::lease_path_state`): only ConfirmedAbsent
            // continues walking; Unknown IO errors fail closed.
            loop {
                match path_presence(ancestor) {
                    PathPresence::Present => break,
                    PathPresence::ConfirmedAbsent => {
                        let leaf = ancestor.file_name().ok_or_else(|| {
                            format!(
                                "cannot canonicalize worktree path '{worktree_path}': {full_error}"
                            )
                        })?;
                        suffix.push(leaf.to_os_string());
                        ancestor = ancestor.parent().ok_or_else(|| {
                            format!(
                                "cannot canonicalize worktree path '{worktree_path}': {full_error}"
                            )
                        })?;
                    }
                    PathPresence::Unknown(err) => {
                        return Err(format!(
                            "cannot canonicalize worktree path '{worktree_path}': \
                             inconclusive stat on '{}': {err}",
                            ancestor.display()
                        ));
                    }
                }
            }
            let mut path = std::fs::canonicalize(ancestor).map_err(|error| {
                format!("cannot canonicalize ancestor for worktree path '{worktree_path}': {error}")
            })?;
            for leaf in suffix.into_iter().rev() {
                path.push(leaf);
            }
            path
        }
    };
    canonical
        .into_os_string()
        .into_string()
        .map_err(|_| format!("canonical worktree path for '{worktree_path}' is not valid UTF-8"))
}

/// Three-state path presence via `symlink_metadata` — never collapse unknown
/// IO errors into "absent" the way [`Path::exists`] does.
enum PathPresence {
    Present,
    ConfirmedAbsent,
    Unknown(std::io::Error),
}

fn path_presence(path: &Path) -> PathPresence {
    match std::fs::symlink_metadata(path) {
        Ok(_) => PathPresence::Present,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => PathPresence::ConfirmedAbsent,
        Err(err) => PathPresence::Unknown(err),
    }
}

#[cfg(test)]
mod canonical_worktree_path_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn canonical_worktree_path_walks_confirmed_absent_suffix() {
        let dir = tempdir().unwrap();
        let existing = dir.path();
        let future = existing.join("not-yet").join("leaf");
        let got = canonical_worktree_path(future.to_str().unwrap()).unwrap();
        let expected = format!(
            "{}/not-yet/leaf",
            existing.canonicalize().unwrap().display()
        );
        assert_eq!(got, expected);
    }
}

/// Explicit approval for a `build-private` env: who signed off, and how much
/// disk was reserved for the private target dir. Provisioning refuses the class
/// without one (#894 S2c: "rare; explicit approval + disk reservation").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PrivateTargetApproval {
    pub approved_by: String,
    pub reserved_bytes: i64,
}

/// A reservation as it stands **in the ledger** — i.e. the part that survives
/// the call (#894 S2c round-2).
///
/// Round-1 read `PrivateTargetApproval` once, in the validator, and threw it
/// away: nothing was ever written, so "reserved 40 GB" was a sentence in a CLI
/// flag and nothing else. Two things record it now, and they answer different
/// questions:
///
/// - the **resource row's `bytes`** is seeded with `reserved_bytes`, so every
///   consumer of the ledger's byte column counts a reserved target from the
///   moment it is approved — not from the first time somebody measures it. A
///   reservation that only shows up once you have already spent the disk is not
///   a reservation.
/// - this row keeps the **provenance**: who approved it, how much, for which
///   dir. `bytes` gets overwritten by the next real measurement; the approval
///   must not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PrivateTargetReservation {
    pub env_id: String,
    pub approved_by: String,
    pub reserved_bytes: i64,
    pub target_path: String,
    pub resource_id: String,
    pub approved_at: String,
}

/// Read the booked reservation for a lease, if it has one.
///
/// **Deliberately unread in production, and this is the honest note about it.**
/// The write side is live (`provision_env` books the row above); nothing reads
/// it back yet, so the linter is correct to call this dead. It is kept, rather
/// than deleted, because the consumer is already named and already needed: the
/// orphan build-artifact reaper (#894 S2b) currently decides whether a target
/// dir is live by reading the process table, which cannot see a holder that
/// declared itself anywhere but `argv`. The ledger is the surface where a
/// holder *can* declare itself, and this row is a `BuildPrivate` lease doing
/// exactly that — an approved, sized, attributed claim on a directory. Wiring
/// the reaper to consult it is the ledger-based holder discovery that the
/// reaper's destructive path is blocked on; deleting this reader now would only
/// mean writing it again there.
#[allow(dead_code)]
pub(crate) fn private_target_reservation(
    conn: &rusqlite::Connection,
    env_id: &str,
) -> Result<Option<PrivateTargetReservation>, String> {
    let row = memcore::db::get_state(conn, PRIVATE_RESERVATION_NS, env_id)
        .map_err(|e| format!("read private target reservation for {env_id}: {e}"))?;
    match row {
        None => Ok(None),
        Some((json, _version)) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("decode private target reservation for {env_id}: {e}")),
    }
}

/// Options for provisioning a managed execution environment. Mirrors
/// `tachi_clean::wt_open::OpenOptions` (the git-worktree primitive) minus the
/// output-format concern, which the entrypoint fixes to suppressed JSON, plus
/// the S2c class policy.
#[derive(Debug, Clone)]
pub(crate) struct ProvisionEnvOptions {
    pub repo_root: PathBuf,
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub task: Option<String>,
    pub role: Option<String>,
    pub dispatch_id: Option<String>,
    pub name: Option<String>,
    /// Provisioning class (#894 S2c). Defaults to `EditOnly` at every call site
    /// that does not care.
    pub env_class: EnvClass,
    /// Required iff `env_class == BuildPrivate`.
    pub private_target_approval: Option<PrivateTargetApproval>,
    /// Explicit private target dir for `BuildPrivate`. Defaults to
    /// `<worktree>/target` (cargo's own default) when omitted.
    pub private_target_dir: Option<PathBuf>,
    pub dry_run: bool,
}

impl ProvisionEnvOptions {
    /// The build target dir this lease allocates **for itself**, if any.
    ///
    /// `BuildPrivate` is the only class that resolves one. `EditOnly` gets none
    /// (the entire disk story of the default class) and `BuildTicketed` gets
    /// none either: its builds run in the executor seat's target, which the seat
    /// picks per ticket and books itself. Handing a ticketed lease a target dir
    /// here — as round-1 did, defaulting to the machine-shared
    /// `CARGO_TARGET_DIR` — books a path the broker never builds in.
    pub(crate) fn build_target_dir(&self, worktree_path: &str) -> Result<Option<PathBuf>, String> {
        match self.env_class {
            EnvClass::EditOnly | EnvClass::BuildTicketed => Ok(None),
            EnvClass::BuildPrivate => Ok(Some(match &self.private_target_dir {
                Some(dir) => dir.clone(),
                None => PathBuf::from(worktree_path).join("target"),
            })),
        }
    }

    /// The cargo target-dir policy written into the worktree's
    /// `.cargo/config.toml`.
    ///
    /// Note that `BuildTicketed` writes **no** config either: a ticketed env
    /// does not build in its own tree — it submits a ticket and the executor
    /// seat builds, in the seat's own checkout, against the seat's target.
    /// Wiring the tree's cargo at a shared dir is precisely the cross-tree
    /// poisoning we are removing.
    fn cargo_target_policy(&self, worktree_path: &str) -> Result<CargoTargetPolicy, String> {
        match self.env_class {
            EnvClass::EditOnly | EnvClass::BuildTicketed => Ok(CargoTargetPolicy::Unallocated),
            EnvClass::BuildPrivate => Ok(CargoTargetPolicy::Private(
                self.build_target_dir(worktree_path)?
                    .expect("BuildPrivate always resolves a target dir"),
            )),
        }
    }
}

/// Outcome of provisioning: the underlying worktree open report plus the lease
/// id when a lease row was recorded. `env_id` is `None` when the worktree open
/// failed / was a dry-run, or when the (non-fatal) lease insert failed — in the
/// latter case a warning is appended to `report.warnings` and the worktree
/// stands untracked (no lease). Such an untracked worktree directory is
/// reclaimed by the age-based `clean sweep` (`tachi_clean::sweep`), not by the
/// stale-lease backstop (which only reclaims leases, and here none was
/// recorded). Resource registration failure is returned as an error after the
/// lease and any partial bindings are rolled back; callers never receive an
/// active lease that the protected cleanup lifecycle cannot own.
#[derive(Debug, Clone)]
pub(crate) struct ProvisionedEnv {
    pub env_id: Option<String>,
    pub report: OpenReport,
}

/// Resolved dispatch working-directory binding after the env gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnvResolution {
    /// cwd resolved from a managed lease. Stamped `env: managed`.
    Managed { cwd: String, env_id: String },
    /// Explicit opt-in bare cwd. Stamped `env: unmanaged`.
    Unmanaged { cwd: String },
    /// Neither env_id nor cwd — the pre-existing daemon-default project root.
    /// Stamped `env: default`.
    Default,
}

impl EnvResolution {
    /// Ledger stamp for this resolution.
    pub(crate) fn stamp(&self) -> &'static str {
        match self {
            EnvResolution::Managed { .. } => "managed",
            EnvResolution::Unmanaged { .. } => "unmanaged",
            EnvResolution::Default => "default",
        }
    }

    /// Resolved working directory to hand the dispatch, if any (`None` = keep
    /// the daemon-default project root).
    pub(crate) fn cwd(&self) -> Option<&str> {
        match self {
            EnvResolution::Managed { cwd, .. } | EnvResolution::Unmanaged { cwd } => Some(cwd),
            EnvResolution::Default => None,
        }
    }

    /// Lease id bound to this dispatch, if any.
    pub(crate) fn env_id(&self) -> Option<&str> {
        match self {
            EnvResolution::Managed { env_id, .. } => Some(env_id),
            _ => None,
        }
    }
}

/// The fail-safe env-binding gate (#894 S1, pure/testable).
///
/// Precedence and rules:
/// 1. `env_id` set → cwd is resolved from the lease (managed). A bare `cwd`
///    supplied alongside `env_id` is a contradiction and is rejected. The lease
///    must exist and be `active`; otherwise fail-closed with a receipt.
/// 2. No `env_id`, `cwd` set → a *bare* cwd. Accepted only when `unmanaged_cwd`
///    is true; stamped `unmanaged`. Otherwise rejected — the fail-safe default
///    is a managed env, and widening is explicit + stamped.
/// 3. Neither → the pre-existing daemon-default project root (`default`).
///
/// `lease` is the pre-fetched row for `env_id` (or `None` when unset / missing).
pub(crate) fn resolve_env_binding(
    env_id: Option<&str>,
    cwd: Option<&str>,
    unmanaged_cwd: bool,
    lease: Option<&ExecEnvLease>,
) -> Result<EnvResolution, String> {
    let env_id = env_id.map(str::trim).filter(|s| !s.is_empty());
    let cwd = cwd.map(str::trim).filter(|s| !s.is_empty());

    if let Some(id) = env_id {
        if cwd.is_some() {
            return Err(format!(
                "env_id '{id}' and an explicit cwd are mutually exclusive: env_id resolves the \
                 working directory from the daemon-owned lease (#894 S1). Drop the cwd."
            ));
        }
        let lease = lease.ok_or_else(|| {
            format!(
                "env_id '{id}' has no exec_envs lease (unknown or never provisioned); refusing to \
                 dispatch into an unresolvable environment (fail-closed, #894 S1)"
            )
        })?;
        if lease.state != ExecEnvState::Active {
            return Err(format!(
                "env_id '{id}' lease is {} (not active); cannot bind a dispatch to a reclaimed \
                 environment (fail-closed, #894 S1)",
                lease.state.as_str()
            ));
        }
        return Ok(EnvResolution::Managed {
            cwd: lease.path.clone(),
            env_id: id.to_string(),
        });
    }

    if let Some(cwd) = cwd {
        if !unmanaged_cwd {
            return Err(
                "a bare cwd is only accepted with explicit unmanaged_cwd:true or an env_id; the \
                 fail-safe default is a managed env, and an unmanaged cwd must be opted into and is \
                 ledger-stamped `env: unmanaged` (#894 S1)"
                    .to_string(),
            );
        }
        return Ok(EnvResolution::Unmanaged {
            cwd: cwd.to_string(),
        });
    }

    Ok(EnvResolution::Default)
}

/// Generate a unique lease id. A uuid v4 is used precisely because it carries
/// no timing dependence: an earlier timestamp-nanos XOR pid scheme collided
/// when two calls landed inside the same clock tick of the same process (see
/// `generated_env_ids_are_unique_and_prefixed`, #1026). A genuine uuid
/// collision would still surface as a DB insert error rather than a silent
/// overwrite — that backstop is unchanged.
fn generate_env_id() -> String {
    format!("env-{}", uuid::Uuid::new_v4().simple())
}

/// The approval gate for a provisioning request (#894 S2c, pure/testable).
///
/// `build-private` is the only class that can hand a lease its own multi-GB
/// target dir, so it is the only class that needs a human on the hook. Refusing
/// it without an approval is fail-closed by construction: a caller cannot get a
/// private target by simply omitting a field.
pub(crate) fn validate_provision_request(opts: &ProvisionEnvOptions) -> Result<(), String> {
    match (opts.env_class, &opts.private_target_approval) {
        (EnvClass::BuildPrivate, None) => Err(
            "env_class 'build-private' requires an explicit approval (approved_by + \
             reserved_bytes): a private cargo target dir is a multi-GB disk allocation and is \
             the rare exception, not something a caller gets by default. Use 'build-ticketed' \
             (submit a build ticket to the serialized executor seat) or 'edit-only' (#894 S2c)."
                .to_string(),
        ),
        (EnvClass::BuildPrivate, Some(approval)) => {
            if approval.approved_by.trim().is_empty() {
                return Err(
                    "env_class 'build-private' approval is missing `approved_by`: an unattributed \
                     approval is not an approval (#894 S2c)"
                        .to_string(),
                );
            }
            if approval.reserved_bytes <= 0 {
                return Err(format!(
                    "env_class 'build-private' needs a positive disk reservation, got \
                     {} bytes (#894 S2c)",
                    approval.reserved_bytes
                ));
            }
            Ok(())
        }
        // An approval supplied for a class that allocates no private target is a
        // confused caller, not a free pass — surface it rather than ignoring it.
        (class, Some(_)) => Err(format!(
            "env_class '{}' does not allocate a private target dir, so a private-target approval \
             is meaningless here; drop it or use 'build-private' (#894 S2c)",
            class.as_str()
        )),
        (_, None) => Ok(()),
    }
}

/// Single provisioning entrypoint (#894 S1, class-aware since S2c): open a
/// managed worktree via the `tachi_clean` primitive, record a daemon-owned
/// lease, then register + bind the physical resources the class allocates. This
/// is the one place that composes the worktree mechanics with a lease, so every
/// consumer (the `wt-open` CLI today; a daemon/MCP caller later) wraps exactly
/// one source of truth for provisioning instead of a divergent copy.
///
/// Open failures (or dry-run) return the report with `env_id: None` and no
/// lease. A lease-insert failure after a successful open is non-fatal: the
/// worktree stands untracked for the age-based sweep. Once a lease insert
/// succeeds, resource registration is part of the publication boundary: a
/// failure rolls the lease and partial bindings back and returns an error.
pub(crate) fn provision_managed_env(
    store: &mut memcore::MemoryStore,
    opts: &ProvisionEnvOptions,
) -> Result<ProvisionedEnv, String> {
    // Fail-closed BEFORE any filesystem work: a refused class must not leave a
    // half-provisioned worktree behind.
    validate_provision_request(opts)?;

    let mut report = open_worktree(build_open_options(opts))?;
    if !report.opened || !report.errors.is_empty() {
        return Ok(ProvisionedEnv {
            env_id: None,
            report,
        });
    }
    report.path = match canonical_worktree_path(&report.path) {
        Ok(path) => path,
        Err(error) => {
            report.warnings.push(format!(
                "worktree provisioned but its path could not be canonicalized: {error}; no ExecEnv lease was recorded"
            ));
            return Ok(ProvisionedEnv {
                env_id: None,
                report,
            });
        }
    };
    // The worktree path is only known after the open, so the private-target
    // default (`<worktree>/target`) is resolved here and the config written now.
    if opts.env_class == EnvClass::BuildPrivate {
        match tachi_clean::wt_open::provision_cargo_target_config(
            std::path::Path::new(&report.path),
            &opts.cargo_target_policy(&report.path)?,
        ) {
            Ok(tachi_clean::wt_open::CargoTargetProvision::Written(dir)) => {
                report.cargo_target_dir = Some(dir.display().to_string());
            }
            Ok(_) => {}
            Err(err) => report.warnings.push(format!(
                "private cargo target-dir provisioning failed: {err}"
            )),
        }
    } else if opts.env_class == EnvClass::EditOnly
        && std::path::Path::new(&report.path)
            .join("Cargo.toml")
            .exists()
    {
        // Say the quiet part out loud at the exact moment it matters: this tree
        // has no target dir wired to it, on purpose.
        report.warnings.push(
            "env_class 'edit-only': no cargo target dir is wired to this worktree. Run builds \
             through the build broker (a build ticket into the serialized executor seat), not \
             in-tree — an in-tree `cargo` here will grow a local target/ that the sweep has to \
             reclaim. This is a convention, not a fence: nothing stops you (#894 S2c)."
                .to_string(),
        );
    }

    let env_id = generate_env_id();
    let lease = NewExecEnvLease {
        env_id: env_id.clone(),
        kind: "worktree".to_string(),
        path: report.path.clone(),
        repo_root: report.repo_root.clone(),
        branch: report.branch.clone(),
        base_sha: report.base_sha.clone(),
        dispatch_id: opts.dispatch_id.clone(),
        env_class: opts.env_class,
        created_at: String::new(),
    };

    match memcore::insert_exec_env(store.connection_mut(), &lease).map_err(|e| e.to_string()) {
        Ok(()) => {
            let build_target = opts.build_target_dir(&report.path)?;
            let build_target = build_target.as_ref().map(|p| p.display().to_string());
            register_env_resources_or_rollback(
                store,
                &env_id,
                opts.env_class,
                &report.path,
                build_target.as_deref(),
                opts.private_target_approval.as_ref(),
            )?;
            Ok(ProvisionedEnv {
                env_id: Some(env_id),
                report,
            })
        }
        Err(err) => {
            report.warnings.push(format!(
                "worktree provisioned but exec_env lease record failed: {err}; it is now an \
                 untracked worktree with no managed lease — reclaim the orphaned directory via \
                 the age-based `clean sweep`"
            ));
            Ok(ProvisionedEnv {
                env_id: None,
                report,
            })
        }
    }
}

fn register_env_resources_or_rollback(
    store: &mut memcore::MemoryStore,
    env_id: &str,
    class: EnvClass,
    worktree_path: &str,
    build_target: Option<&str>,
    approval: Option<&PrivateTargetApproval>,
) -> Result<EnvResources, String> {
    match register_env_resources(store, env_id, class, worktree_path, build_target, approval) {
        Ok(resources) => Ok(resources),
        Err(error) => {
            rollback_failed_resource_registration(store, env_id).map_err(|rollback_error| {
                format!(
                    "resource ledger registration failed for provisioned env {env_id}: {error}; \
                     lease rollback also failed, leaving the environment fail-closed: \
                     {rollback_error}"
                )
            })?;
            Err(format!(
                "resource ledger registration failed for provisioned env {env_id}: {error}; \
                 the lease and any partial bindings were rolled back"
            ))
        }
    }
}

fn rollback_failed_resource_registration(
    store: &mut memcore::MemoryStore,
    env_id: &str,
) -> Result<(), String> {
    let tx = store
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE exec_env_resource_bindings SET released_at = ?2 \
         WHERE env_id = ?1 AND released_at IS NULL",
        rusqlite::params![env_id, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|error| error.to_string())?;
    let deleted = tx
        .execute(
            "DELETE FROM exec_envs WHERE env_id = ?1 AND state = 'active'",
            rusqlite::params![env_id],
        )
        .map_err(|error| error.to_string())?;
    if deleted != 1 {
        return Err(format!(
            "expected one active lease for {env_id}, deleted {deleted}"
        ));
    }
    tx.commit().map_err(|error| error.to_string())
}

/// Register the physical resources a lease owns and bind them to it (#894 S2a
/// ledger + S2c class policy), booking a `build-private` reservation on the way.
///
/// The class invariant is enforced HERE, not just at the caller: an `EditOnly`
/// (or `BuildTicketed`) env with a build target is rejected outright rather than
/// quietly bound. A caller that computes the target dir wrong cannot talk this
/// function into allocating one — which is what makes "only `build-private` owns
/// a build target" hold at the seam instead of by convention up the stack.
///
/// `approval` is required exactly when the class allocates a private target, and
/// it is **written down** (#894 S2c round-2): the target row's `bytes` is seeded
/// with the reservation and a provenance row records who approved it. A
/// reservation nobody records is not a reservation.
pub(crate) fn register_env_resources(
    store: &mut memcore::MemoryStore,
    env_id: &str,
    class: EnvClass,
    worktree_path: &str,
    build_target: Option<&str>,
    approval: Option<&PrivateTargetApproval>,
) -> Result<EnvResources, String> {
    if !class.allocates_build_target() && build_target.is_some() {
        return Err(format!(
            "env_class '{}' allocates no build target of its own, but a build target dir ('{}') \
             was supplied — refusing to bind it. A ticketed env's builds run in the executor \
             seat's target, which the seat books itself (#894 S2c)",
            class.as_str(),
            build_target.unwrap_or_default()
        ));
    }
    if class.allocates_build_target() && build_target.is_none() {
        return Err(format!(
            "env_class '{}' requires a build target dir, none was resolved (#894 S2c)",
            class.as_str()
        ));
    }
    if class.requires_approval() && approval.is_none() {
        return Err(format!(
            "env_class '{}' requires an approval (approved_by + reserved_bytes) and none reached \
             the resource ledger: the reservation must be BOOKED, not just checked (#894 S2c)",
            class.as_str()
        ));
    }

    let worktree_resource_id = ensure_resource(
        store.connection_mut(),
        ResourceKind::Worktree,
        worktree_path,
    )?;
    memcore::bind_resource(store.connection_mut(), env_id, &worktree_resource_id)
        .map_err(|e| e.to_string())?;

    let build_target_resource_id = match build_target {
        None => None,
        Some(path) => {
            let id = ensure_resource(store.connection_mut(), ResourceKind::BuildTarget, path)?;
            // Many-to-many by design: the binding table is a refcount, so a
            // resource cannot be reclaimed while any live lease holds it.
            memcore::bind_resource(store.connection_mut(), env_id, &id)
                .map_err(|e| e.to_string())?;
            if let Some(approval) = approval {
                book_private_reservation(store, env_id, &id, path, approval)?;
            }
            Some(id)
        }
    };

    Ok(EnvResources {
        worktree_resource_id,
        build_target_resource_id,
    })
}

/// Write the reservation down, in the two places that need it (#894 S2c
/// round-2): the resource row's `bytes` (so disk accounting sees the reserved
/// target immediately, before a single artifact is built) and a provenance row
/// (so `bytes` being overwritten by the next real measurement does not erase who
/// approved what).
fn book_private_reservation(
    store: &mut memcore::MemoryStore,
    env_id: &str,
    resource_id: &str,
    target_path: &str,
    approval: &PrivateTargetApproval,
) -> Result<(), String> {
    memcore::record_resource_measurement(
        store.connection_mut(),
        resource_id,
        approval.reserved_bytes,
        "",
    )
    .map_err(|e| format!("book reserved bytes for {target_path}: {e}"))?;

    let reservation = PrivateTargetReservation {
        env_id: env_id.to_string(),
        approved_by: approval.approved_by.clone(),
        reserved_bytes: approval.reserved_bytes,
        target_path: target_path.to_string(),
        resource_id: resource_id.to_string(),
        approved_at: chrono::Utc::now().to_rfc3339(),
    };
    let value =
        serde_json::to_string(&reservation).map_err(|e| format!("serialize reservation: {e}"))?;
    store
        .set_state(PRIVATE_RESERVATION_NS, env_id, &value)
        .map_err(|e| format!("record private target reservation for {env_id}: {e}"))?;
    Ok(())
}

/// The resource ids bound to a freshly provisioned lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvResources {
    pub worktree_resource_id: String,
    /// `None` for every class except `build-private` — an edit-only env owns no
    /// target, and a ticketed env's target belongs to the executor seat.
    pub build_target_resource_id: Option<String>,
}

/// Resolve the ledger row for `(path, kind)`, registering it if this is the
/// first time anyone claimed it. `(path, kind)` is UNIQUE in S2a, so a shared
/// target dir resolves to ONE row that many leases bind (splitting it into two
/// rows would split its refcount and let a live target be deleted).
///
/// A reclaimed path is a prior physical incarnation and is revived through
/// memcore's one canonical re-registration writer. Every other non-active
/// state fails closed. A quarantined build target in particular is the
/// "interrupted cargo poisoned this dir" state — the broker clears it via
/// `release_quarantine`, and until it does, nothing may bind it.
pub(crate) fn ensure_resource(
    conn: &mut rusqlite::Connection,
    kind: ResourceKind,
    path: &str,
) -> Result<String, String> {
    if let Some(existing) =
        memcore::find_resource_by_path(conn, path, kind).map_err(|e| e.to_string())?
    {
        if existing.state == ResourceState::Active {
            return Ok(existing.resource_id);
        }
        if existing.state != ResourceState::Reclaimed {
            return Err(format!(
                "resource '{path}' ({}) is '{}', not 'active': it cannot back a new env until it \
                 is cleared (quarantined targets go through the broker's release path; a \
                 reclaimed row means those bytes are gone) (#894 S2a/S2c)",
                kind.as_str(),
                existing.state.as_str()
            ));
        }
    }
    let resource_id = uuid::Uuid::new_v4().to_string();
    memcore::insert_resource(
        conn,
        &NewExecEnvResource {
            resource_id: resource_id.clone(),
            kind,
            path: path.to_string(),
            bytes: None,
            created_at: String::new(),
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(resource_id)
}

/// Fence all active resources bound to a lease in the resource ledger (#894 S2a/S2c/S2e, #1322).
///
/// Looks up all active bindings for `env_id` in `exec_env_resource_bindings` and transitions
/// every resource row to `quarantined` in one SQLite transaction.
pub(crate) fn quarantine_lease_resources(
    conn: &mut rusqlite::Connection,
    env_id: &str,
    reason: &str,
) -> Result<Vec<String>, String> {
    if env_id.trim().is_empty() {
        return Err("cannot quarantine resources for an empty exec env id".to_string());
    }
    let lease_state: Option<String> = conn
        .query_row(
            "SELECT state FROM exec_envs WHERE env_id = ?1",
            rusqlite::params![env_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("read exec env {env_id} before quarantine: {error}"))?;
    let Some(lease_state) = lease_state else {
        return Err(format!(
            "exec env {env_id} does not exist; refusing false quarantine"
        ));
    };
    if lease_state != "active" && lease_state != "dispatching" {
        return Err(format!(
            "exec env {env_id} is {lease_state}; refusing quarantine without a live lease"
        ));
    }
    let sql = "SELECT resource_id FROM exec_env_resource_bindings WHERE env_id = ?1 AND released_at IS NULL ORDER BY resource_id";
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let resource_ids: Vec<String> = stmt
        .query_map(rusqlite::params![env_id], |row| row.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("read active resources for exec env {env_id}: {e}"))?;
    drop(stmt);

    if resource_ids.is_empty() {
        return Err(format!(
            "exec env {env_id} has no live resource bindings; refusing false quarantine"
        ));
    }

    memcore::quarantine_resources_atomically(conn, &resource_ids, reason)
        .map_err(|err| format!("atomically quarantine resources for exec env {env_id}: {err}"))
}

/// Parent-owned exclusive admission for a Required postflight dispatch.
///
/// The transition is persisted before preimage capture. Concurrent callers can
/// therefore never both observe an active lease and spawn. A crash leaves the
/// lease in `dispatching`, which is intentionally unusable until reconciled.
pub(crate) struct ExecEnvDispatchLeaseGuard {
    server: MemoryServer,
    env_id: String,
    armed: bool,
}

impl ExecEnvDispatchLeaseGuard {
    pub(crate) fn acquire(server: &MemoryServer, env_id: &str) -> Result<Self, String> {
        let env_id = env_id.trim();
        if env_id.is_empty() {
            return Err("Required postflight dispatch needs a managed exec env id".to_string());
        }
        server.with_global_store(|store| {
            let conn = store.connection_mut();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| error.to_string())?;
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM exec_envs WHERE env_id = ?1",
                    rusqlite::params![env_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            let Some(state) = state else {
                return Err(format!("exec env {env_id} does not exist"));
            };
            if state != "active" {
                return Err(format!(
                    "exec env {env_id} is {state}; another dispatch or terminal action owns it"
                ));
            }
            let live_bindings: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM exec_env_resource_bindings WHERE env_id = ?1 AND released_at IS NULL",
                    rusqlite::params![env_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if live_bindings == 0 {
                return Err(format!(
                    "exec env {env_id} has no live resource bindings; refusing orphan admission"
                ));
            }
            let quarantined: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM exec_env_resource_bindings b JOIN exec_env_resources r ON r.resource_id = b.resource_id WHERE b.env_id = ?1 AND b.released_at IS NULL AND r.state = 'quarantined'",
                    rusqlite::params![env_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if quarantined != 0 {
                return Err(format!(
                    "exec env {env_id} has quarantined resources; refusing dispatch"
                ));
            }
            let changed = tx
                .execute(
                    "UPDATE exec_envs SET state = 'dispatching' WHERE env_id = ?1 AND state = 'active'",
                    rusqlite::params![env_id],
                )
                .map_err(|error| error.to_string())?;
            if changed != 1 {
                return Err(format!(
                    "exec env {env_id} changed during dispatch admission"
                ));
            }
            tx.commit().map_err(|error| error.to_string())
        })?;
        Ok(Self {
            server: server.clone(),
            env_id: env_id.to_string(),
            armed: true,
        })
    }

    fn release_state(&mut self) -> Result<(), String> {
        self.server.with_global_store(|store| {
            let changed = store
                .connection_mut()
                .execute(
                    "UPDATE exec_envs SET state = 'active' WHERE env_id = ?1 AND state = 'dispatching'",
                    rusqlite::params![self.env_id],
                )
                .map_err(|error| error.to_string())?;
            if changed != 1 {
                return Err(format!(
                    "exec env {} lost its dispatch admission state",
                    self.env_id
                ));
            }
            Ok(())
        })?;
        self.armed = false;
        Ok(())
    }

    pub(crate) fn release_clean(&mut self) -> Result<(), String> {
        self.release_state()
    }

    pub(crate) fn release_after_fence(&mut self) -> Result<(), String> {
        self.release_state()
    }

    pub(crate) fn release_without_spawn(&mut self) -> Result<(), String> {
        self.release_state()
    }

    fn fence_abandoned(&mut self) {
        if !self.armed {
            return;
        }
        let fenced = self.server.with_global_store(|store| {
            quarantine_lease_resources(
                store.connection_mut(),
                &self.env_id,
                "dispatch aborted before postflight ownership completed",
            )
        });
        if fenced.is_ok() {
            let _ = self.release_state();
        }
    }
}

impl Drop for ExecEnvDispatchLeaseGuard {
    fn drop(&mut self) {
        self.fence_abandoned();
    }
}

// Compatibility shim for old exec_env_ops::ensure_resource_allow_quarantined
// path (#1702 carve 4). Body lives in tachi-build-broker.
#[allow(unused_imports)]
pub(crate) use tachi_build_broker::ensure_resource_allow_quarantined;

fn build_open_options(opts: &ProvisionEnvOptions) -> OpenOptions {
    OpenOptions {
        repo_root: opts.repo_root.clone(),
        path: opts.path.clone(),
        branch: opts.branch.clone(),
        base: opts.base.clone(),
        task: opts.task.clone(),
        role: opts.role.clone(),
        dispatch_id: opts.dispatch_id.clone(),
        name: opts.name.clone(),
        // Always `Unallocated` at open time. No class wires the *tree's* cargo
        // at the machine-shared target dir any more — that coupling, across N
        // diverged worktrees, is the poisoning we are removing. A
        // `build-private` env does get a `.cargo/config.toml`, but its default
        // target (`<worktree>/target`) is only knowable once the open has
        // produced a path, so `provision_managed_env` writes it right after.
        cargo_target: CargoTargetPolicy::Unallocated,
        dry_run: opts.dry_run,
        // Emit suppressed — the returned report is the single surface.
        output: OutputFormat::Json,
    }
}

impl MemoryServer {
    /// Resolve the dispatch cwd binding through the fail-safe env gate. Fetches
    /// the lease when an `env_id` is supplied.
    pub(crate) fn resolve_dispatch_env_binding(
        &self,
        env_id: Option<&str>,
        cwd: Option<&str>,
        unmanaged_cwd: bool,
    ) -> Result<EnvResolution, String> {
        let trimmed_env_id = env_id.map(str::trim).filter(|s| !s.is_empty());
        let lease = match trimmed_env_id {
            Some(id) => self.with_global_store_read(|store| {
                memcore::get_exec_env(store.connection(), id).map_err(|e| e.to_string())
            })?,
            None => None,
        };
        if let Some(id) = trimmed_env_id {
            let quarantined_resource = self.with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT r.resource_id \
                         FROM exec_env_resource_bindings b \
                         JOIN exec_env_resources r ON r.resource_id = b.resource_id \
                         WHERE b.env_id = ?1 AND b.released_at IS NULL \
                           AND r.state = 'quarantined' \
                         ORDER BY r.resource_id LIMIT 1",
                        rusqlite::params![id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|error| error.to_string())
            })?;
            if let Some(resource_id) = quarantined_resource {
                return Err(format!(
                    "env_id '{id}' is bound to quarantined resource '{resource_id}'; refusing \
                     re-dispatch until the canonical resource release path clears the fence \
                     (fail-closed, #1322)"
                ));
            }
        }
        resolve_env_binding(env_id, cwd, unmanaged_cwd, lease.as_ref())
    }

    /// Ordinary reclaim path for a lease (#894 S1). Flips `active` ->
    /// `reclaimed` transactionally and idempotently. The external cleaner uses
    /// memcore's removal-claim protocol so it owns the lease before deleting.
    ///
    /// Wired producers today: only the `safe_merge` completion path
    /// ([`gh_ops::router`], via [`reclaim_exec_env_for_worktree`]). Routing the
    /// remaining terminal transitions — dispatch `cancel` and generic
    /// terminal-state — through here is a #894 S2 policy call (a terminal task
    /// may still be sitting on unmerged work, so reclaim there is not
    /// unconditional) and is deliberately NOT wired in S1. Until S2 lands, the
    /// `clean sweep` stale-lease backstop (see
    /// `bootstrap::clean_cli::sweep_stale_exec_env_leases`) is what keeps a
    /// crash / kill -9 / non-safe_merge exit from leaking an `active` lease
    /// forever: it reclaims leases whose worktree is already gone from disk.
    pub(crate) fn reclaim_exec_env(
        &self,
        selector: &ExecEnvSelector,
        reason: Option<&str>,
    ) -> Result<ReclaimOutcome, String> {
        self.with_global_store(|store| {
            memcore::reclaim_exec_env(store.connection_mut(), selector, reason)
                .map_err(|e| e.to_string())
        })
    }

    /// Reclaim the active lease for a worktree path (the selector safe_merge /
    /// local merge have on hand). Best-effort: `NotFound` is not an error for
    /// the caller — a worktree opened outside a lease has nothing to flip.
    pub(crate) fn reclaim_exec_env_for_worktree(
        &self,
        worktree_path: &str,
        reason: Option<&str>,
    ) -> Result<ReclaimOutcome, String> {
        self.reclaim_exec_env(&ExecEnvSelector::Path(worktree_path.to_string()), reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_dispatchable_env(server: &MemoryServer, env_id: &str, resource_id: &str) {
        server
            .with_global_store(|store| {
                memcore::insert_exec_env(
                    store.connection_mut(),
                    &NewExecEnvLease {
                        env_id: env_id.to_string(),
                        kind: "worktree".to_string(),
                        path: format!("/wt/{env_id}"),
                        repo_root: "/repo".to_string(),
                        branch: env_id.to_string(),
                        base_sha: "abc1234".to_string(),
                        dispatch_id: None,
                        env_class: EnvClass::EditOnly,
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                memcore::insert_resource(
                    store.connection_mut(),
                    &NewExecEnvResource {
                        resource_id: resource_id.to_string(),
                        kind: ResourceKind::Worktree,
                        path: format!("/wt/{env_id}"),
                        bytes: None,
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                memcore::bind_resource(store.connection_mut(), env_id, resource_id)
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .expect("seed dispatchable env");
    }

    fn lease(state: ExecEnvState, path: &str) -> ExecEnvLease {
        ExecEnvLease {
            env_id: "env-x".to_string(),
            kind: "worktree".to_string(),
            path: path.to_string(),
            repo_root: "/repo".to_string(),
            branch: "b".to_string(),
            base_sha: "sha".to_string(),
            dispatch_id: None,
            agent_identity_id: None,
            claim_id: None,
            env_class: EnvClass::EditOnly,
            state,
            reclaim_reason: None,
            schema_version: 1,
            created_at: "2026-07-10T00:00:00Z".to_string(),
            reclaimed_at: None,
        }
    }

    #[test]
    fn env_id_resolves_cwd_from_active_lease() {
        let l = lease(ExecEnvState::Active, "/wt/managed");
        let out = resolve_env_binding(Some("env-x"), None, false, Some(&l)).unwrap();
        assert_eq!(
            out,
            EnvResolution::Managed {
                cwd: "/wt/managed".to_string(),
                env_id: "env-x".to_string()
            }
        );
        assert_eq!(out.stamp(), "managed");
        assert_eq!(out.cwd(), Some("/wt/managed"));
        assert_eq!(out.env_id(), Some("env-x"));
    }

    #[test]
    fn env_id_with_explicit_cwd_is_rejected() {
        let l = lease(ExecEnvState::Active, "/wt/managed");
        let err = resolve_env_binding(Some("env-x"), Some("/other"), false, Some(&l)).unwrap_err();
        assert!(err.contains("mutually exclusive"), "got: {err}");
    }

    #[test]
    fn env_id_missing_lease_fails_closed() {
        let err = resolve_env_binding(Some("env-x"), None, false, None).unwrap_err();
        assert!(err.contains("no exec_envs lease"), "got: {err}");
    }

    #[test]
    fn env_id_reclaimed_lease_fails_closed() {
        let l = lease(ExecEnvState::Reclaimed, "/wt/managed");
        let err = resolve_env_binding(Some("env-x"), None, false, Some(&l)).unwrap_err();
        assert!(err.contains("not active"), "got: {err}");
    }

    #[test]
    fn env_id_dispatching_lease_fails_closed() {
        let l = lease(ExecEnvState::Dispatching, "/wt/managed");
        let err = resolve_env_binding(Some("env-x"), None, false, Some(&l)).unwrap_err();
        assert!(err.contains("not active"), "got: {err}");
    }

    #[test]
    fn postflight_dispatch_admission_is_exclusive_and_clean_release_reopens() {
        let temp = tempfile::tempdir().expect("temp server");
        let server = MemoryServer::new(temp.path().join("global.sqlite"), None).expect("server");
        seed_dispatchable_env(&server, "env-exclusive", "res-exclusive");

        let mut first =
            ExecEnvDispatchLeaseGuard::acquire(&server, "env-exclusive").expect("first admission");
        let conflict = match ExecEnvDispatchLeaseGuard::acquire(&server, "env-exclusive") {
            Ok(_) => panic!("a second dispatch must not share one lease"),
            Err(error) => error,
        };
        assert!(conflict.contains("dispatching"), "{conflict}");

        first.release_clean().expect("clean release");
        let mut reopened = ExecEnvDispatchLeaseGuard::acquire(&server, "env-exclusive")
            .expect("lease reopens only after clean finalization");
        reopened
            .release_without_spawn()
            .expect("unspawned admission release");
    }

    #[test]
    fn abandoned_postflight_dispatch_fences_real_resource_before_reopening_lease() {
        let temp = tempfile::tempdir().expect("temp server");
        let server = MemoryServer::new(temp.path().join("global.sqlite"), None).expect("server");
        seed_dispatchable_env(&server, "env-abort", "res-abort");

        let guard =
            ExecEnvDispatchLeaseGuard::acquire(&server, "env-abort").expect("dispatch admission");
        drop(guard);

        let (lease_state, resource_state): (String, String) = server
            .with_global_store_read(|store| {
                let lease_state = store
                    .connection()
                    .query_row(
                        "SELECT state FROM exec_envs WHERE env_id = 'env-abort'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                let resource_state = store
                    .connection()
                    .query_row(
                        "SELECT state FROM exec_env_resources WHERE resource_id = 'res-abort'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                Ok((lease_state, resource_state))
            })
            .expect("read fenced state");
        assert_eq!(lease_state, "active");
        assert_eq!(resource_state, "quarantined");
        let error = server
            .resolve_dispatch_env_binding(Some("env-abort"), None, false)
            .expect_err("fenced resource must block redispatch");
        assert!(
            error.contains("quarantined resource 'res-abort'"),
            "{error}"
        );
    }

    #[test]
    fn abandoned_dispatch_with_a_failed_fence_stays_exclusively_fail_closed() {
        let temp = tempfile::tempdir().expect("temp server");
        let db_path = temp.path().join("global.sqlite");
        let server = MemoryServer::new(db_path.clone(), None).expect("server");
        seed_dispatchable_env(&server, "env-fence-fail", "res-fence-fail");
        let guard = ExecEnvDispatchLeaseGuard::acquire(&server, "env-fence-fail")
            .expect("dispatch admission");

        let fault_connection = rusqlite::Connection::open(db_path).expect("fault connection");
        fault_connection
            .execute_batch(
                "CREATE TRIGGER fail_abandoned_dispatch_fence
                 BEFORE UPDATE OF state ON exec_env_resources
                 WHEN NEW.state = 'quarantined' AND OLD.resource_id = 'res-fence-fail'
                 BEGIN SELECT RAISE(ABORT, 'injected abandoned fence failure'); END;",
            )
            .expect("install fence failure trigger");
        drop(guard);

        let lease = server
            .with_global_store_read(|store| {
                memcore::get_exec_env(store.connection(), "env-fence-fail")
                    .map_err(|error| error.to_string())
            })
            .expect("read lease")
            .expect("lease exists");
        assert_eq!(lease.state, ExecEnvState::Dispatching);
        let error = server
            .resolve_dispatch_env_binding(Some("env-fence-fail"), None, false)
            .expect_err("failed fence must not reopen the lease");
        assert!(error.contains("not active"), "{error}");
    }

    #[test]
    fn quarantine_refuses_an_orphan_lease_without_live_resource_bindings() {
        let temp = tempfile::tempdir().expect("temp server");
        let server = MemoryServer::new(temp.path().join("global.sqlite"), None).expect("server");
        server
            .with_global_store(|store| {
                memcore::insert_exec_env(
                    store.connection_mut(),
                    &NewExecEnvLease {
                        env_id: "env-orphan".to_string(),
                        kind: "worktree".to_string(),
                        path: "/wt/orphan".to_string(),
                        repo_root: "/repo".to_string(),
                        branch: "orphan".to_string(),
                        base_sha: "abc1234".to_string(),
                        dispatch_id: None,
                        env_class: EnvClass::EditOnly,
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())
            })
            .expect("seed orphan env");

        let error = server
            .with_global_store(|store| {
                quarantine_lease_resources(
                    store.connection_mut(),
                    "env-orphan",
                    "postflight rejected",
                )
            })
            .expect_err("an empty binding set must not report quarantine");
        assert!(error.contains("no live resource bindings"), "{error}");
    }

    #[test]
    fn managed_dispatch_rejects_a_lease_with_a_quarantined_bound_resource() {
        let temp = tempfile::tempdir().expect("temp server");
        let server = MemoryServer::new(temp.path().join("global.sqlite"), None).expect("server");
        server
            .with_global_store(|store| {
                memcore::insert_exec_env(
                    store.connection_mut(),
                    &NewExecEnvLease {
                        env_id: "env-fenced".to_string(),
                        kind: "worktree".to_string(),
                        path: "/wt/fenced".to_string(),
                        repo_root: "/repo".to_string(),
                        branch: "fenced".to_string(),
                        base_sha: "abc1234".to_string(),
                        dispatch_id: None,
                        env_class: EnvClass::EditOnly,
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                memcore::insert_resource(
                    store.connection_mut(),
                    &NewExecEnvResource {
                        resource_id: "res-fenced".to_string(),
                        kind: ResourceKind::Worktree,
                        path: "/wt/fenced".to_string(),
                        bytes: None,
                        created_at: String::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
                memcore::bind_resource(store.connection_mut(), "env-fenced", "res-fenced")
                    .map_err(|error| error.to_string())?;
                memcore::quarantine_resource(
                    store.connection_mut(),
                    "res-fenced",
                    "postflight indeterminate",
                )
                .map_err(|error| error.to_string())?;
                Ok(())
            })
            .expect("seed fenced managed env");

        let error = server
            .resolve_dispatch_env_binding(Some("env-fenced"), None, false)
            .expect_err("a quarantined lease resource must block production admission");
        assert!(
            error.contains("quarantined resource 'res-fenced'"),
            "{error}"
        );
    }

    #[test]
    fn bare_cwd_without_optin_is_rejected() {
        // Fail-safe default: a bare cwd must be explicitly opted into.
        let err = resolve_env_binding(None, Some("/some/dir"), false, None).unwrap_err();
        assert!(err.contains("unmanaged_cwd"), "got: {err}");
    }

    #[test]
    fn bare_cwd_with_optin_is_unmanaged() {
        let out = resolve_env_binding(None, Some("/some/dir"), true, None).unwrap();
        assert_eq!(
            out,
            EnvResolution::Unmanaged {
                cwd: "/some/dir".to_string()
            }
        );
        assert_eq!(out.stamp(), "unmanaged");
        assert_eq!(out.env_id(), None);
    }

    #[test]
    fn no_cwd_no_env_id_is_default() {
        let out = resolve_env_binding(None, None, false, None).unwrap();
        assert_eq!(out, EnvResolution::Default);
        assert_eq!(out.stamp(), "default");
        assert_eq!(out.cwd(), None);
    }

    #[test]
    fn unmanaged_optin_without_cwd_is_still_default() {
        // unmanaged_cwd:true with no cwd is meaningless, not an error.
        let out = resolve_env_binding(None, None, true, None).unwrap();
        assert_eq!(out, EnvResolution::Default);
    }

    #[test]
    fn whitespace_env_id_and_cwd_are_treated_as_unset() {
        let out = resolve_env_binding(Some("  "), Some("   "), false, None).unwrap();
        assert_eq!(out, EnvResolution::Default);
    }

    #[test]
    fn generated_env_ids_are_unique_and_prefixed() {
        let a = generate_env_id();
        let b = generate_env_id();
        assert!(a.starts_with("env-"));
        assert_ne!(a, b);
    }

    // ─── env classes (#894 S2c) ─────────────────────────────────────────────

    fn provision_opts(
        env_class: EnvClass,
        approval: Option<PrivateTargetApproval>,
    ) -> ProvisionEnvOptions {
        ProvisionEnvOptions {
            repo_root: PathBuf::from("/repo"),
            path: None,
            branch: None,
            base: None,
            task: None,
            role: None,
            dispatch_id: None,
            name: None,
            env_class,
            private_target_approval: approval,
            private_target_dir: None,
            dry_run: false,
        }
    }

    fn approval(bytes: i64) -> PrivateTargetApproval {
        PrivateTargetApproval {
            approved_by: "owner".to_string(),
            reserved_bytes: bytes,
        }
    }

    fn store_with_lease(env_id: &str, class: EnvClass, path: &str) -> memcore::MemoryStore {
        let store = memcore::MemoryStore::open_in_memory().expect("in-memory store");
        memcore::insert_exec_env(
            store.connection(),
            &NewExecEnvLease {
                env_id: env_id.to_string(),
                kind: "worktree".to_string(),
                path: path.to_string(),
                repo_root: "/repo".to_string(),
                branch: "tachi/894/w".to_string(),
                base_sha: "abc1234".to_string(),
                dispatch_id: None,
                env_class: class,
                created_at: String::new(),
            },
        )
        .expect("seed lease");
        store
    }

    /// ① The default class allocates NO build target — the whole disk story of
    /// `edit-only`. If provisioning ever starts handing edit-only envs a target
    /// dir (e.g. by "helpfully" defaulting to the shared one, which is what the
    /// pre-S2c code did for every worktree), this reds.
    #[test]
    fn edit_only_env_binds_no_build_target_resource() {
        let mut store = store_with_lease("env-edit", EnvClass::EditOnly, "/wt/edit");

        let opts = provision_opts(EnvClass::EditOnly, None);
        assert_eq!(
            opts.build_target_dir("/wt/edit").unwrap(),
            None,
            "edit-only must resolve no build target dir"
        );

        let bound = register_env_resources(
            &mut store,
            "env-edit",
            EnvClass::EditOnly,
            "/wt/edit",
            None,
            None,
        )
        .expect("register");
        let conn = store.connection_mut();

        assert!(
            bound.build_target_resource_id.is_none(),
            "edit-only env must have no build_target resource"
        );
        assert!(
            memcore::list_resources(conn, None, Some(ResourceKind::BuildTarget))
                .unwrap()
                .is_empty(),
            "provisioning an edit-only env must not create a build_target resource row at all"
        );
        // The worktree itself IS tracked — the bytes ledger still owns the tree.
        assert_eq!(
            memcore::active_binding_count(conn, &bound.worktree_resource_id).unwrap(),
            1
        );
    }

    /// The class contract is enforced at the seam, not just at the caller: a
    /// caller that computes a target dir for an edit-only env cannot talk this
    /// function into binding it.
    #[test]
    fn register_env_resources_refuses_a_build_target_for_edit_only() {
        let mut store = store_with_lease("env-edit", EnvClass::EditOnly, "/wt/edit");
        let err = register_env_resources(
            &mut store,
            "env-edit",
            EnvClass::EditOnly,
            "/wt/edit",
            Some("/seat/target-resident"),
            None,
        )
        .unwrap_err();
        assert!(err.contains("allocates no build target"), "got: {err}");
        assert!(
            memcore::list_resources(store.connection(), None, Some(ResourceKind::BuildTarget))
                .unwrap()
                .is_empty(),
            "the refused call must not have registered the target anyway"
        );
    }

    /// ② (round-2) A `build-ticketed` env holds **no build target of its own**.
    ///
    /// Round-1 bound it to a "resident target" that, at the only production call
    /// site (`resident_target_dir: None`), resolved to
    /// `default_shared_cargo_target_dir()` — a path the broker never builds in.
    /// The lease's ledger row and the seat's actual target were two different
    /// dirs. This test fails if a ticketed lease ever books a build target again:
    /// the dir a ticket lands on (resident vs fork scratch) is the seat's
    /// run-time decision, and the seat books it.
    #[test]
    fn build_ticketed_env_holds_no_build_target_of_its_own() {
        let mut store = store_with_lease("env-t", EnvClass::BuildTicketed, "/wt/t");

        // Provisioning resolves NO target dir for the class...
        let opts = provision_opts(EnvClass::BuildTicketed, None);
        assert_eq!(
            opts.build_target_dir("/wt/t").unwrap(),
            None,
            "a ticketed env must not resolve a build target dir at provisioning time — which \
             target its tickets land on is decided per ticket by the broker"
        );
        assert!(!EnvClass::BuildTicketed.allocates_build_target());

        // ...and the lease binds none.
        let bound = register_env_resources(
            &mut store,
            "env-t",
            EnvClass::BuildTicketed,
            "/wt/t",
            None,
            None,
        )
        .expect("register");
        assert!(
            bound.build_target_resource_id.is_none(),
            "a build-ticketed lease must not hold a build_target resource"
        );
        assert!(
            memcore::list_resources(store.connection(), None, Some(ResourceKind::BuildTarget))
                .unwrap()
                .is_empty(),
            "provisioning a ticketed env must not create a build_target row at all — the broker \
             registers the target it actually builds in"
        );

        // And the seam refuses one even if a caller hands it the seat's target.
        let err = register_env_resources(
            &mut store,
            "env-t",
            EnvClass::BuildTicketed,
            "/wt/t",
            Some("/seat/target-resident"),
            None,
        )
        .unwrap_err();
        assert!(err.contains("allocates no build target"), "got: {err}");
    }

    /// ⑤ `build-private` is the only class that can hand a lease its own
    /// multi-GB target dir, so it is the only one that needs a human on the
    /// hook. No approval → refused, before any filesystem work happens.
    #[test]
    fn build_private_without_approval_is_refused() {
        let err =
            validate_provision_request(&provision_opts(EnvClass::BuildPrivate, None)).unwrap_err();
        assert!(err.contains("requires an explicit approval"), "got: {err}");

        // An unattributed approval is not an approval.
        let err = validate_provision_request(&provision_opts(
            EnvClass::BuildPrivate,
            Some(PrivateTargetApproval {
                approved_by: "   ".to_string(),
                reserved_bytes: 1_000,
            }),
        ))
        .unwrap_err();
        assert!(err.contains("approved_by"), "got: {err}");

        // Neither is an approval that reserves no disk.
        let err = validate_provision_request(&provision_opts(
            EnvClass::BuildPrivate,
            Some(PrivateTargetApproval {
                approved_by: "owner".to_string(),
                reserved_bytes: 0,
            }),
        ))
        .unwrap_err();
        assert!(err.contains("disk reservation"), "got: {err}");

        // A complete approval passes.
        validate_provision_request(&provision_opts(
            EnvClass::BuildPrivate,
            Some(PrivateTargetApproval {
                approved_by: "owner".to_string(),
                reserved_bytes: 40_000_000_000,
            }),
        ))
        .expect("an approved build-private request is allowed");
    }

    #[test]
    fn the_default_classes_need_no_approval_and_reject_a_stray_one() {
        validate_provision_request(&provision_opts(EnvClass::EditOnly, None)).unwrap();
        validate_provision_request(&provision_opts(EnvClass::BuildTicketed, None)).unwrap();

        // An approval on a class that allocates no private target is a confused
        // caller — surfaced, not silently ignored.
        let err = validate_provision_request(&provision_opts(
            EnvClass::BuildTicketed,
            Some(PrivateTargetApproval {
                approved_by: "owner".to_string(),
                reserved_bytes: 1,
            }),
        ))
        .unwrap_err();
        assert!(
            err.contains("does not allocate a private target"),
            "got: {err}"
        );
    }

    /// ⑥ (round-2) An approved `build-private` reservation is **in the ledger**,
    /// not just in a validator's local variable.
    ///
    /// Round-1 read `reserved_bytes` once in `validate_provision_request` and
    /// dropped it on the floor — nothing was persisted, so "reserved 40 GB"
    /// could not be answered by any query, and the disk accounting saw the
    /// private target as 0 bytes until somebody happened to measure it. Delete
    /// either write in `book_private_reservation` and this reds.
    #[test]
    fn an_approved_private_target_reservation_is_booked_in_the_ledger() {
        const RESERVED: i64 = 40_000_000_000;
        let mut store = store_with_lease("env-p", EnvClass::BuildPrivate, "/wt/p");

        let bound = register_env_resources(
            &mut store,
            "env-p",
            EnvClass::BuildPrivate,
            "/wt/p",
            Some("/wt/p/target"),
            Some(&approval(RESERVED)),
        )
        .expect("register");

        let target_id = bound
            .build_target_resource_id
            .expect("build-private binds its private target");
        let conn = store.connection_mut();

        // 1. The bytes are on the resource row, so disk accounting counts the
        //    reservation from the moment it is approved.
        let row = memcore::get_resource(conn, &target_id).unwrap().unwrap();
        assert_eq!(
            row.bytes,
            Some(RESERVED),
            "the reserved bytes must be booked on the target's ledger row — a reservation the \
             ledger cannot see is not a reservation (#894 S2c round-2)"
        );
        assert_eq!(row.path, "/wt/p/target");

        // 2. The provenance survives a later real measurement overwriting `bytes`.
        memcore::record_resource_measurement(conn, &target_id, 12_345, "").unwrap();
        let reservation = private_target_reservation(conn, "env-p")
            .unwrap()
            .expect("the approval is queryable");
        assert_eq!(reservation.approved_by, "owner");
        assert_eq!(reservation.reserved_bytes, RESERVED);
        assert_eq!(reservation.target_path, "/wt/p/target");
        assert_eq!(reservation.resource_id, target_id);
        assert!(!reservation.approved_at.is_empty());
    }

    /// The seam refuses to allocate a private target with no approval in hand,
    /// even if the caller already talked its way past `validate_provision_request`
    /// — the booking and the check are the same gate.
    #[test]
    fn register_env_resources_refuses_build_private_without_an_approval_to_book() {
        let mut store = store_with_lease("env-p", EnvClass::BuildPrivate, "/wt/p");
        let err = register_env_resources(
            &mut store,
            "env-p",
            EnvClass::BuildPrivate,
            "/wt/p",
            Some("/wt/p/target"),
            None,
        )
        .unwrap_err();
        assert!(err.contains("must be BOOKED"), "got: {err}");
        assert!(private_target_reservation(store.connection(), "env-p")
            .unwrap()
            .is_none());
    }

    #[test]
    fn build_private_defaults_its_target_dir_under_the_worktree() {
        let opts = provision_opts(
            EnvClass::BuildPrivate,
            Some(PrivateTargetApproval {
                approved_by: "owner".to_string(),
                reserved_bytes: 1_000,
            }),
        );
        assert_eq!(
            opts.build_target_dir("/wt/private").unwrap(),
            Some(PathBuf::from("/wt/private/target"))
        );
        // ...and that is what gets written into .cargo/config.toml.
        assert_eq!(
            opts.cargo_target_policy("/wt/private").unwrap(),
            CargoTargetPolicy::Private(PathBuf::from("/wt/private/target"))
        );
    }

    #[test]
    fn reclaimed_worktree_resource_revives_for_same_path_reprovision() {
        let mut store = store_with_lease("env-old", EnvClass::EditOnly, "/wt/churn");
        let old = register_env_resources(
            &mut store,
            "env-old",
            EnvClass::EditOnly,
            "/wt/churn",
            None,
            None,
        )
        .expect("register first incarnation");
        assert_eq!(
            memcore::claim_exec_env_removal(store.connection_mut(), "/wt/churn").unwrap(),
            Some("env-old".to_string())
        );
        memcore::complete_exec_env_removal(
            store.connection_mut(),
            "env-old",
            Some("test removal"),
            123,
        )
        .unwrap();

        memcore::insert_exec_env(
            store.connection(),
            &NewExecEnvLease {
                env_id: "env-new".to_string(),
                kind: "worktree".to_string(),
                path: "/wt/churn".to_string(),
                repo_root: "/repo".to_string(),
                branch: "tachi/894/new".to_string(),
                base_sha: "def5678".to_string(),
                dispatch_id: None,
                env_class: EnvClass::EditOnly,
                created_at: String::new(),
            },
        )
        .unwrap();
        let new = register_env_resources_or_rollback(
            &mut store,
            "env-new",
            EnvClass::EditOnly,
            "/wt/churn",
            None,
            None,
        )
        .expect("revive reclaimed physical path");

        assert_ne!(new.worktree_resource_id, old.worktree_resource_id);
        assert!(
            memcore::get_resource(store.connection(), &old.worktree_resource_id)
                .unwrap()
                .is_none()
        );
        let revived = memcore::get_resource(store.connection(), &new.worktree_resource_id)
            .unwrap()
            .unwrap();
        assert_eq!(revived.state, ResourceState::Active);
        assert_eq!(revived.reclaimed_bytes, None);
        assert_eq!(
            memcore::active_binding_count(store.connection(), &new.worktree_resource_id).unwrap(),
            1
        );
    }

    #[test]
    fn resource_registration_failure_exposes_no_active_lease_or_partial_binding() {
        let mut store = store_with_lease("env-p", EnvClass::BuildPrivate, "/wt/p");
        let target_id = ensure_resource(
            store.connection_mut(),
            ResourceKind::BuildTarget,
            "/wt/p/target",
        )
        .unwrap();
        memcore::quarantine_resource(store.connection_mut(), &target_id, "poisoned").unwrap();

        let error = register_env_resources_or_rollback(
            &mut store,
            "env-p",
            EnvClass::BuildPrivate,
            "/wt/p",
            Some("/wt/p/target"),
            Some(&approval(1_000)),
        )
        .expect_err("quarantined target must fail the whole ledger publication");
        assert!(error.contains("rolled back"), "{error}");
        assert!(memcore::get_exec_env(store.connection(), "env-p")
            .unwrap()
            .is_none());
        let worktree =
            memcore::find_resource_by_path(store.connection(), "/wt/p", ResourceKind::Worktree)
                .unwrap()
                .expect("partial resource row remains available for orphan reconciliation");
        assert_eq!(worktree.state, ResourceState::Active);
        assert_eq!(
            memcore::active_binding_count(store.connection(), &worktree.resource_id).unwrap(),
            0
        );
        assert_eq!(
            memcore::active_binding_count(store.connection(), &target_id).unwrap(),
            0
        );
    }

    #[test]
    fn edit_only_and_ticketed_trees_are_wired_to_no_cargo_target() {
        // The poison vector: pre-S2c EVERY worktree got .cargo/config.toml
        // pointing at the machine-shared target. Neither of these classes may.
        for class in [EnvClass::EditOnly, EnvClass::BuildTicketed] {
            let opts = provision_opts(class, None);
            assert_eq!(
                opts.cargo_target_policy("/wt/x").unwrap(),
                CargoTargetPolicy::Unallocated,
                "{} must not wire the tree's cargo at any shared target dir",
                class.as_str()
            );
        }
    }

    #[test]
    fn a_quarantined_target_cannot_back_a_new_env() {
        let mut store = store_with_lease("env-p", EnvClass::BuildPrivate, "/wt/p");
        let target_id = ensure_resource(
            store.connection_mut(),
            ResourceKind::BuildTarget,
            "/wt/p/target",
        )
        .expect("register");

        // An interrupted cargo fenced this dir off.
        memcore::quarantine_resource(store.connection_mut(), &target_id, "interrupted").unwrap();

        let err = register_env_resources(
            &mut store,
            "env-p",
            EnvClass::BuildPrivate,
            "/wt/p",
            Some("/wt/p/target"),
            Some(&approval(1_000)),
        )
        .unwrap_err();
        assert!(
            err.contains("quarantined") || err.contains("not 'active'"),
            "a poisoned target must not be handed to a fresh env; got: {err}"
        );
        // …and the refused call booked nothing.
        assert!(
            private_target_reservation(store.connection(), "env-p")
                .unwrap()
                .is_none(),
            "a refused provision must not leave a reservation behind"
        );
    }
}
