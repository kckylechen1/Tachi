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
//! | class | worktree | build target |
//! |---|---|---|
//! | `edit-only` (default) | yes | **none** |
//! | `build-ticketed` | yes | binds (refcounted) the executor seat's resident target |
//! | `build-private` | yes | a private target dir — approval + reservation required |
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
use tachi_clean::wt_clean::OutputFormat;
use tachi_clean::wt_open::{open_worktree, CargoTargetPolicy, OpenOptions, OpenReport};

use crate::server_state::MemoryServer;

/// Explicit approval for a `build-private` env: who signed off, and how much
/// disk was reserved for the private target dir. Provisioning refuses the class
/// without one (#894 S2c: "rare; explicit approval + disk reservation").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrivateTargetApproval {
    pub approved_by: String,
    pub reserved_bytes: i64,
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
    /// The executor seat's resident target dir — what a `BuildTicketed` env
    /// binds to (refcounted; it is shared by every ticketed lease on this
    /// machine). `None` falls back to
    /// `tachi_clean::wt_open::default_shared_cargo_target_dir()`.
    pub resident_target_dir: Option<PathBuf>,
    pub dry_run: bool,
}

impl ProvisionEnvOptions {
    /// The build target dir this class allocates, if any. `EditOnly` → `None`;
    /// that is the entire disk story of the default class.
    pub(crate) fn build_target_dir(&self, worktree_path: &str) -> Result<Option<PathBuf>, String> {
        match self.env_class {
            EnvClass::EditOnly => Ok(None),
            EnvClass::BuildTicketed => match &self.resident_target_dir {
                Some(dir) => Ok(Some(dir.clone())),
                None => tachi_clean::wt_open::default_shared_cargo_target_dir().map(Some),
            },
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
    /// seat builds, in the seat's own checkout, against the seat's target. The
    /// lease still *binds* the resident target (refcount, so it cannot be
    /// reclaimed out from under an outstanding ticket), but wiring the tree's
    /// cargo at that same shared dir is precisely the cross-tree poisoning we
    /// are removing.
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
    conn: &rusqlite::Connection,
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
/// ledger + S2c class policy).
///
/// The class invariant is enforced HERE, not just at the caller: an `EditOnly`
/// env with a build target is rejected outright rather than quietly bound. A
/// caller that computes the target dir wrong cannot talk this function into
/// allocating one — which is what makes the "edit-only allocates no build
/// target" contract hold at the seam instead of by convention up the stack.
pub(crate) fn register_env_resources(
    conn: &rusqlite::Connection,
    env_id: &str,
    class: EnvClass,
    worktree_path: &str,
    build_target: Option<&str>,
) -> Result<EnvResources, String> {
    if !class.allocates_build_target() && build_target.is_some() {
        return Err(format!(
            "env_class '{}' allocates no build target, but a build target dir ('{}') was \
             supplied — refusing to bind it (#894 S2c)",
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

    let worktree_resource_id = ensure_resource(conn, ResourceKind::Worktree, worktree_path)?;
    memcore::bind_resource(conn, env_id, &worktree_resource_id).map_err(|e| e.to_string())?;

    let build_target_resource_id = match build_target {
        None => None,
        Some(path) => {
            let id = ensure_resource(conn, ResourceKind::BuildTarget, path)?;
            // Many-to-many by design: the seat's resident target is bound by
            // every ticketed lease at once, and refcount>0 keeps it from being
            // reclaimed while any of them is still alive.
            memcore::bind_resource(conn, env_id, &id).map_err(|e| e.to_string())?;
            Some(id)
        }
    };

    Ok(EnvResources {
        worktree_resource_id,
        build_target_resource_id,
    })
}

/// The resource ids bound to a freshly provisioned lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvResources {
    pub worktree_resource_id: String,
    /// `None` for `edit-only` — the whole point of the default class.
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
    conn: &rusqlite::Connection,
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
/// mid-reclaim (`reclaiming`/`reclaim_failed`) or already reclaimed is not
/// something to quietly build into — those bytes are being (or have been) freed
/// under someone else's transaction.
pub(crate) fn ensure_resource_allow_quarantined(
    conn: &rusqlite::Connection,
    target_path: &str,
) -> Result<String, String> {
    let existing = memcore::find_resource_by_path(conn, target_path, ResourceKind::BuildTarget)
        .map_err(|e| e.to_string())?;
    match existing {
        Some(res) => match res.state {
            ResourceState::Active | ResourceState::Quarantined => Ok(res.resource_id),
            other => Err(format!(
                "build target '{target_path}' is '{}': a target that is being reclaimed (or \
                 already has been) must not be built into (#894 S2a/S2c)",
                other.as_str()
            )),
        },
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
            resident_target_dir: Some(PathBuf::from("/seat/target-resident")),
            dry_run: false,
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
        let store = store_with_lease("env-edit", EnvClass::EditOnly, "/wt/edit");
        let conn = store.connection();

        let opts = provision_opts(EnvClass::EditOnly, None);
        assert_eq!(
            opts.build_target_dir("/wt/edit").unwrap(),
            None,
            "edit-only must resolve no build target dir"
        );

        let bound = register_env_resources(conn, "env-edit", EnvClass::EditOnly, "/wt/edit", None)
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
        let store = store_with_lease("env-edit", EnvClass::EditOnly, "/wt/edit");
        let err = register_env_resources(
            store.connection(),
            "env-edit",
            EnvClass::EditOnly,
            "/wt/edit",
            Some("/seat/target-resident"),
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

    /// A ticketed env binds the seat's resident target — and because the binding
    /// table is many-to-many, N ticketed envs share ONE row with refcount N (so
    /// the target cannot be reclaimed while any of them is alive).
    #[test]
    fn build_ticketed_envs_share_one_refcounted_resident_target() {
        let store = store_with_lease("env-a", EnvClass::BuildTicketed, "/wt/a");
        let conn = store.connection();
        memcore::insert_exec_env(
            conn,
            &NewExecEnvLease {
                env_id: "env-b".to_string(),
                kind: "worktree".to_string(),
                path: "/wt/b".to_string(),
                repo_root: "/repo".to_string(),
                branch: "b".to_string(),
                base_sha: "abc1234".to_string(),
                dispatch_id: None,
                env_class: EnvClass::BuildTicketed,
                created_at: String::new(),
            },
        )
        .unwrap();

        let a = register_env_resources(
            conn,
            "env-a",
            EnvClass::BuildTicketed,
            "/wt/a",
            Some("/seat/target-resident"),
        )
        .unwrap();
        let b = register_env_resources(
            conn,
            "env-b",
            EnvClass::BuildTicketed,
            "/wt/b",
            Some("/seat/target-resident"),
        )
        .unwrap();

        let target_a = a.build_target_resource_id.expect("ticketed binds a target");
        let target_b = b.build_target_resource_id.expect("ticketed binds a target");
        assert_eq!(
            target_a, target_b,
            "one shared target dir must be ONE ledger row (two rows would split its refcount \
             and let a live target be deleted)"
        );
        assert_eq!(
            memcore::active_binding_count(conn, &target_a).unwrap(),
            2,
            "refcount = live bindings"
        );
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
        let store = store_with_lease("env-a", EnvClass::BuildTicketed, "/wt/a");
        let conn = store.connection();
        let target_id = ensure_resource(conn, ResourceKind::BuildTarget, "/seat/target-resident")
            .expect("register target");

        // An interrupted cargo fenced this dir off.
        let mut store = store;
        memcore::quarantine_resource(store.connection_mut(), &target_id, "interrupted").unwrap();

        let err = register_env_resources(
            store.connection(),
            "env-a",
            EnvClass::BuildTicketed,
            "/wt/a",
            Some("/seat/target-resident"),
        )
        .unwrap_err();
        assert!(
            err.contains("quarantined") || err.contains("not 'active'"),
            "a poisoned target must not be handed to a fresh env; got: {err}"
        );
    }
}
