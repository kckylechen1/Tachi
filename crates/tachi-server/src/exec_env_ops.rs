//! Execution-environment provisioning, binding, and reclaim (#894 S1).
//!
//! This module is the daemon-side owner of the `exec_envs` lease lifecycle. It
//! composes the git-worktree primitive (`tachi_clean::wt_open::open_worktree`,
//! the single source for the worktree mechanics) with a daemon-owned lease row,
//! so there is exactly one provisioning entrypoint. Dispatch binds a lease via
//! [`resolve_env_binding`] (the fail-safe managed-vs-unmanaged gate) and every
//! reclaim flows through the one reclaim function in `memcore::db::exec_env`.

use std::path::PathBuf;

use memcore::{ExecEnvLease, ExecEnvSelector, ExecEnvState, NewExecEnvLease, ReclaimOutcome};
use tachi_clean::wt_clean::OutputFormat;
use tachi_clean::wt_open::{open_worktree, OpenOptions, OpenReport};

use crate::server_state::MemoryServer;

/// Options for provisioning a managed execution environment. Mirrors
/// `tachi_clean::wt_open::OpenOptions` (the git-worktree primitive) minus the
/// output-format concern, which the entrypoint fixes to suppressed JSON.
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
    pub dry_run: bool,
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

/// Single provisioning entrypoint (#894 S1): open a managed worktree via the
/// `tachi_clean` primitive, then record a daemon-owned lease on `conn`. This is
/// the one place that composes the worktree mechanics with a lease, so every
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
    let mut report = open_worktree(build_open_options(opts))?;
    if !report.opened || !report.errors.is_empty() {
        return Ok(ProvisionedEnv {
            env_id: None,
            report,
        });
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
        created_at: String::new(),
    };

    match memcore::insert_exec_env(conn, &lease).map_err(|e| e.to_string()) {
        Ok(()) => Ok(ProvisionedEnv {
            env_id: Some(env_id),
            report,
        }),
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
}
