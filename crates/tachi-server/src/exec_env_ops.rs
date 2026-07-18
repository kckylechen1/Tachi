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

use std::path::PathBuf;

use memcore::{
    EnvClass, ExecEnvLease, ExecEnvSelector, ExecEnvState, NewExecEnvLease, NewExecEnvResource,
    ReclaimOutcome, ResourceKind, ResourceState,
};
use serde::{Deserialize, Serialize};
use tachi_clean::wt_clean::OutputFormat;
use tachi_clean::wt_open::{open_worktree, CargoTargetPolicy, OpenOptions, OpenReport};

use crate::server_state::MemoryServer;

/// `hard_state` namespace for booked private-target reservations; key = env_id.
pub(crate) const PRIVATE_RESERVATION_NS: &str = "exec_env_private_target";

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
/// recorded).
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
/// Provisioning failures (or dry-run) return the report with `env_id: None` and
/// no lease. A lease-insert failure after a successful open is non-fatal: the
/// worktree stands, a warning is attached, and the orphaned worktree directory
/// is left to the age-based `clean sweep` (there is no lease for the stale-lease
/// backstop to reclaim in this case).
pub(crate) fn provision_managed_env(
    conn: &mut rusqlite::Connection,
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

    match memcore::insert_exec_env(conn, &lease).map_err(|e| e.to_string()) {
        Ok(()) => {
            let build_target = opts.build_target_dir(&report.path)?;
            let build_target = build_target.as_ref().map(|p| p.display().to_string());
            if let Err(err) = register_env_resources(
                conn,
                &env_id,
                opts.env_class,
                &report.path,
                build_target.as_deref(),
                opts.private_target_approval.as_ref(),
            ) {
                // Non-fatal, but loudly non-silent: the lease exists and the
                // worktree exists; what's missing is the bytes ledger row, which
                // means the reclaim path cannot free this tree by itself.
                report.warnings.push(format!(
                    "worktree + lease recorded but the resource ledger write failed: {err}; \
                     this env's bytes are not tracked and will need the age-based `clean sweep`"
                ));
            }
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
    conn: &mut rusqlite::Connection,
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

    let worktree_resource_id = ensure_resource(conn, ResourceKind::Worktree, worktree_path)?;
    memcore::bind_resource(conn, env_id, &worktree_resource_id).map_err(|e| e.to_string())?;

    let build_target_resource_id = match build_target {
        None => None,
        Some(path) => {
            let id = ensure_resource(conn, ResourceKind::BuildTarget, path)?;
            // Many-to-many by design: the binding table is a refcount, so a
            // resource cannot be reclaimed while any live lease holds it.
            memcore::bind_resource(conn, env_id, &id).map_err(|e| e.to_string())?;
            if let Some(approval) = approval {
                book_private_reservation(conn, env_id, &id, path, approval)?;
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
    conn: &mut rusqlite::Connection,
    env_id: &str,
    resource_id: &str,
    target_path: &str,
    approval: &PrivateTargetApproval,
) -> Result<(), String> {
    memcore::record_resource_measurement(conn, resource_id, approval.reserved_bytes, "")
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
    memcore::db::set_state(conn, PRIVATE_RESERVATION_NS, env_id, &value)
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
/// Fail-closed on a row that is not `active`: a reclaimed or quarantined
/// resource must not silently back a new env. A quarantined build target in
/// particular is the "interrupted cargo poisoned this dir" state — the broker
/// clears it via `release_quarantine`, and until it does, nothing may bind it.
pub(crate) fn ensure_resource(
    conn: &mut rusqlite::Connection,
    kind: ResourceKind,
    path: &str,
) -> Result<String, String> {
    if let Some(existing) =
        memcore::find_resource_by_path(conn, path, kind).map_err(|e| e.to_string())?
    {
        if existing.state != ResourceState::Active {
            return Err(format!(
                "resource '{path}' ({}) is '{}', not 'active': it cannot back a new env until it \
                 is cleared (quarantined targets go through the broker's release path; a \
                 reclaimed row means those bytes are gone) (#894 S2a/S2c)",
                kind.as_str(),
                existing.state.as_str()
            ));
        }
        return Ok(existing.resource_id);
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

/// Resolve (registering if new) the ledger row for a build target dir the
/// **broker** is about to touch — the one caller that is allowed to see a
/// `quarantined` row, because clearing that quarantine is its job
/// (`build_broker::clear_target_for_reuse` -> `memcore::release_quarantine`).
///
/// Still fail-closed on the states nobody can safely proceed from: a target
/// mid-reclaim (`reclaiming`/`reclaim_failed`) is not something to quietly
/// build into — those bytes are being (or have been) freed under someone
/// else's transaction.
///
/// ## `reclaimed` ⇒ re-registrable (was an open S2a round-2 dependency; now
/// resolved)
///
/// A seat target holds no lease binding (that is the round-2 fix: only the
/// broker books it), so a *stale, unheld* seat target is legitimately
/// reclaimable by the orphan reaper. When that happens the row goes
/// `reclaimed` — and `memcore::insert_resource`'s revive semantics (#894 S2a)
/// now cover exactly this case: registering over a `reclaimed` `(path, kind)`
/// resurrects that row in place under a fresh `resource_id`, `state` back to
/// `active`. So a seat whose target got swept is not wedged: its next ticket
/// calls this function, sees `Reclaimed`, and re-registers the same path as a
/// virgin dir — same as the `None` (never-seen) arm below, just through the
/// revive path instead of a plain insert. Reclaiming an idle target dir costs
/// a cold rebuild, never a wedged seat.
pub(crate) fn ensure_resource_allow_quarantined(
    conn: &mut rusqlite::Connection,
    target_path: &str,
) -> Result<String, String> {
    let existing = memcore::find_resource_by_path(conn, target_path, ResourceKind::BuildTarget)
        .map_err(|e| e.to_string())?;
    match existing {
        Some(res)
            if matches!(
                res.state,
                ResourceState::Active | ResourceState::Quarantined
            ) =>
        {
            Ok(res.resource_id)
        }
        Some(res) if res.state == ResourceState::Reclaimed => {
            // Revived, not a plain insert: `insert_resource` resurrects the
            // `(path, kind)` row under a fresh id rather than erroring, so the
            // seat's next ticket gets a virgin-looking target instead of
            // wedging on the swept row.
            let resource_id = uuid::Uuid::new_v4().to_string();
            memcore::insert_resource(
                conn,
                &NewExecEnvResource {
                    resource_id: resource_id.clone(),
                    kind: ResourceKind::BuildTarget,
                    path: target_path.to_string(),
                    bytes: None,
                    created_at: String::new(),
                },
            )
            .map_err(|e| e.to_string())?;
            Ok(resource_id)
        }
        Some(res) => Err(format!(
            "build target '{target_path}' is '{}': a target mid-reclaim must not be built into \
             until that resolves (#894 S2a/S2c)",
            res.state.as_str()
        )),
        None => {
            let resource_id = uuid::Uuid::new_v4().to_string();
            memcore::insert_resource(
                conn,
                &NewExecEnvResource {
                    resource_id: resource_id.clone(),
                    kind: ResourceKind::BuildTarget,
                    path: target_path.to_string(),
                    bytes: None,
                    created_at: String::new(),
                },
            )
            .map_err(|e| e.to_string())?;
            Ok(resource_id)
        }
    }
}

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
        resolve_env_binding(env_id, cwd, unmanaged_cwd, lease.as_ref())
    }

    /// THE single reclaim path for a lease (#894 S1). Flips `active` ->
    /// `reclaimed` transactionally and idempotently.
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
        let conn = store.connection_mut();

        let opts = provision_opts(EnvClass::EditOnly, None);
        assert_eq!(
            opts.build_target_dir("/wt/edit").unwrap(),
            None,
            "edit-only must resolve no build target dir"
        );

        let bound =
            register_env_resources(conn, "env-edit", EnvClass::EditOnly, "/wt/edit", None, None)
                .expect("register");

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
            store.connection_mut(),
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
        let conn = store.connection_mut();

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
        let bound =
            register_env_resources(conn, "env-t", EnvClass::BuildTicketed, "/wt/t", None, None)
                .expect("register");
        assert!(
            bound.build_target_resource_id.is_none(),
            "a build-ticketed lease must not hold a build_target resource"
        );
        assert!(
            memcore::list_resources(conn, None, Some(ResourceKind::BuildTarget))
                .unwrap()
                .is_empty(),
            "provisioning a ticketed env must not create a build_target row at all — the broker \
             registers the target it actually builds in"
        );

        // And the seam refuses one even if a caller hands it the seat's target.
        let err = register_env_resources(
            conn,
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
        let conn = store.connection_mut();

        let bound = register_env_resources(
            conn,
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
            store.connection_mut(),
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
            store.connection_mut(),
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

    /// A seat's build target holds no lease binding (round-2 fix: only the
    /// broker books it), so an idle seat target is legitimately reclaimable by
    /// the orphan reaper — the row can go `reclaimed` out from under a seat
    /// that still thinks it owns that path. `insert_resource`'s revive
    /// semantics (#894 S2a) are what keep the seat from wedging on that: the
    /// next ticket's `ensure_resource_allow_quarantined` call on the same path
    /// must come back `Ok` with a fresh, `active` resource_id — not an error
    /// that leaves the seat stuck on the swept row (the open dependency this
    /// module used to carry against S2a round-2).
    #[test]
    fn a_reclaimed_seat_target_is_revived_not_wedged() {
        let mut store = memcore::MemoryStore::open_in_memory().expect("in-memory store");
        let conn = store.connection_mut();

        let first_id =
            ensure_resource_allow_quarantined(conn, "/seat/target").expect("first registration");

        // The orphan reaper sweeps the idle target: nobody held a binding on
        // it, so the reclaim goes through clean.
        let outcome =
            memcore::reclaim_resource(conn, &first_id, Some("orphan sweep"), |_res| Ok(0))
                .expect("reclaim");
        assert!(matches!(
            outcome,
            memcore::ResourceReclaimOutcome::Reclaimed { .. }
        ));
        assert_eq!(
            memcore::get_resource(conn, &first_id)
                .unwrap()
                .unwrap()
                .state,
            ResourceState::Reclaimed
        );

        // The seat's next ticket asks for the same path again — it must NOT
        // error or wedge; it must come back as a fresh, active resource, and
        // the build proceeds instead of stalling.
        let second_id = ensure_resource_allow_quarantined(conn, "/seat/target")
            .expect("a swept seat target must revive, not wedge the seat (#894 S2a/S2c)");
        assert_ne!(
            second_id, first_id,
            "revive mints a fresh resource_id (S2a's RegisterOutcome::Revived contract)"
        );
        let revived = memcore::get_resource(conn, &second_id).unwrap().unwrap();
        assert_eq!(revived.state, ResourceState::Active);
        assert_eq!(revived.path, "/seat/target");
    }
}
