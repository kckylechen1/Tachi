//! Atomic publication of a provisioned execution environment.
//!
//! Filesystem provisioning happens before this writer is called. Everything
//! that makes the resulting worktree visible to dispatch or cleanup is then
//! committed in one SQLite transaction: lease, resources, bindings, optional
//! private-target reservation, and the final active state. A process crash can
//! therefore expose either no lease or the complete published lease, never a
//! permanently fenced intermediate row.

use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::collections::HashSet;
use uuid::Uuid;

use super::super::*;

const PRIVATE_TARGET_RESERVATION_NAMESPACE: &str = "exec_env_private_target";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEnvProvisioningResource {
    pub kind: db::exec_env_resources::ResourceKind,
    pub path: String,
    pub bytes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedExecEnvResource {
    pub resource_id: String,
    pub kind: db::exec_env_resources::ResourceKind,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedExecEnv {
    pub worktree: PublishedExecEnvResource,
    pub build_target: Option<PublishedExecEnvResource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEnvPrivateTargetReservation {
    pub approved_by: String,
    pub reserved_bytes: i64,
    pub target_path: String,
}

#[derive(Serialize)]
struct PersistedPrivateTargetReservation<'a> {
    env_id: &'a str,
    approved_by: &'a str,
    reserved_bytes: i64,
    target_path: &'a str,
    resource_id: &'a str,
    approved_at: &'a str,
}

impl MemoryStore {
    /// Publish one already-created filesystem environment as a complete ledger
    /// unit. Dropping or crashing before `commit` leaves no visible lease,
    /// binding, resource revival, measurement, or reservation.
    pub fn publish_exec_env_atomically(
        &mut self,
        lease: &db::exec_env::NewExecEnvLease,
        resources: &[ExecEnvProvisioningResource],
        private_target_reservation: Option<&ExecEnvPrivateTargetReservation>,
    ) -> Result<PublishedExecEnv, MemoryError> {
        if resources.is_empty() {
            return Err(MemoryError::InvalidArg(format!(
                "exec env '{}' cannot be published without a physical resource",
                lease.env_id
            )));
        }
        let worktree_count = resources
            .iter()
            .filter(|resource| {
                resource.kind == db::exec_env_resources::ResourceKind::Worktree
                    && resource.path == lease.path
            })
            .count();
        if worktree_count != 1 {
            return Err(MemoryError::InvalidArg(format!(
                "exec env '{}' requires exactly one worktree resource at '{}', found {worktree_count}",
                lease.env_id, lease.path
            )));
        }
        let mut unique_resources = HashSet::with_capacity(resources.len());
        if resources.iter().any(|resource| {
            !unique_resources.insert((resource.kind.as_str(), resource.path.as_str()))
        }) {
            return Err(MemoryError::InvalidArg(format!(
                "exec env '{}' contains duplicate provisioning resources",
                lease.env_id
            )));
        }
        if lease.env_class.requires_approval() != private_target_reservation.is_some() {
            return Err(MemoryError::InvalidArg(format!(
                "exec env '{}' class '{}' has inconsistent private-target reservation evidence",
                lease.env_id,
                lease.env_class.as_str()
            )));
        }
        match private_target_reservation {
            None => {
                if resources.len() != 1 {
                    return Err(MemoryError::InvalidArg(format!(
                        "exec env '{}' class '{}' may publish only its worktree resource",
                        lease.env_id,
                        lease.env_class.as_str()
                    )));
                }
            }
            Some(reservation) => {
                let matching_targets: Vec<_> = resources
                    .iter()
                    .filter(|resource| {
                        resource.kind == db::exec_env_resources::ResourceKind::BuildTarget
                            && resource.path == reservation.target_path
                            && resource.bytes == Some(reservation.reserved_bytes)
                    })
                    .collect();
                if resources.len() != 2 || matching_targets.len() != 1 {
                    return Err(MemoryError::InvalidArg(format!(
                        "exec env '{}' private reservation must match exactly one build target and one worktree",
                        lease.env_id
                    )));
                }
            }
        }

        let _authorization =
            db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = db::normalize_utc_iso_or_now(&lease.created_at);
        tx.execute(
            "INSERT INTO exec_envs
             (env_id, kind, path, repo_root, branch, base_sha, dispatch_id, agent_identity_id,
              claim_id, env_class, state, reclaim_reason, schema_version, created_at, reclaimed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, 'provisioning', NULL, 1, ?9, NULL)",
            params![
                lease.env_id,
                lease.kind,
                lease.path,
                lease.repo_root,
                lease.branch,
                lease.base_sha,
                lease.dispatch_id,
                lease.env_class.as_str(),
                now,
            ],
        )?;

        let mut published = Vec::with_capacity(resources.len());
        for resource in resources {
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT resource_id, state FROM exec_env_resources
                     WHERE path = ?1 AND kind = ?2",
                    params![resource.path, resource.kind.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if resource.kind == db::exec_env_resources::ResourceKind::Worktree {
                if let Some((resource_id, _)) = existing.as_ref() {
                    let live_bindings: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM exec_env_resource_bindings
                         WHERE resource_id = ?1 AND released_at IS NULL",
                        [resource_id],
                        |row| row.get(0),
                    )?;
                    if live_bindings != 0 {
                        return Err(MemoryError::Duplicate(format!(
                            "worktree resource '{}' at '{}' already has {live_bindings} live binding(s); a physical workspace may belong to only one live exec env",
                            resource_id, resource.path
                        )));
                    }
                }
            }
            let resource_id = match existing {
                None => {
                    let resource_id = Uuid::new_v4().to_string();
                    let measured_at = resource.bytes.map(|_| now.clone());
                    tx.execute(
                        "INSERT INTO exec_env_resources
                         (resource_id, kind, path, bytes, measured_at, state, reclaim_reason,
                          reclaimed_at, reclaimed_bytes, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, 'active', NULL, NULL, NULL, ?6, ?6)",
                        params![
                            resource_id,
                            resource.kind.as_str(),
                            resource.path,
                            resource.bytes,
                            measured_at,
                            now,
                        ],
                    )?;
                    resource_id
                }
                Some((resource_id, state)) if state == "active" => {
                    if let Some(bytes) = resource.bytes {
                        let changed = tx.execute(
                            "UPDATE exec_env_resources SET bytes = ?2, measured_at = ?3,
                                 updated_at = ?3 WHERE resource_id = ?1 AND state = 'active'",
                            params![resource_id, bytes, now],
                        )?;
                        if changed != 1 {
                            return Err(MemoryError::InvalidArg(format!(
                                "resource '{resource_id}' changed during exec env publication"
                            )));
                        }
                    }
                    resource_id
                }
                Some((previous_resource_id, state)) if state == "reclaimed" => {
                    let resource_id = Uuid::new_v4().to_string();
                    let measured_at = resource.bytes.map(|_| now.clone());
                    let changed = tx.execute(
                        "UPDATE exec_env_resources SET resource_id = ?1, bytes = ?2,
                             measured_at = ?3, state = 'active', reclaim_reason = NULL,
                             reclaimed_at = NULL, reclaimed_bytes = NULL, created_at = ?4,
                             updated_at = ?4 WHERE resource_id = ?5 AND state = 'reclaimed'",
                        params![
                            resource_id,
                            resource.bytes,
                            measured_at,
                            now,
                            previous_resource_id,
                        ],
                    )?;
                    if changed != 1 {
                        return Err(MemoryError::InvalidArg(format!(
                            "reclaimed resource '{previous_resource_id}' changed during exec env publication"
                        )));
                    }
                    resource_id
                }
                Some((resource_id, state)) => {
                    return Err(MemoryError::Duplicate(format!(
                        "exec_env_resource at ('{}', {}) already exists as '{}' (resource_id '{}'); only active reuse or reclaimed revival is allowed",
                        resource.path,
                        resource.kind.as_str(),
                        state,
                        resource_id
                    )))
                }
            };

            tx.execute(
                "INSERT INTO exec_env_resource_bindings
                 (binding_id, env_id, resource_id, created_at, released_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![Uuid::new_v4().to_string(), lease.env_id, resource_id, now],
            )?;
            published.push(PublishedExecEnvResource {
                resource_id,
                kind: resource.kind,
                path: resource.path.clone(),
            });
        }

        if let Some(reservation) = private_target_reservation {
            let target = published
                .iter()
                .find(|resource| {
                    resource.kind == db::exec_env_resources::ResourceKind::BuildTarget
                        && resource.path == reservation.target_path
                })
                .ok_or_else(|| {
                    MemoryError::InvalidArg(format!(
                        "exec env '{}' private reservation has no matching build target at '{}'",
                        lease.env_id, reservation.target_path
                    ))
                })?;
            if reservation.approved_by.trim().is_empty() || reservation.reserved_bytes <= 0 {
                return Err(MemoryError::InvalidArg(format!(
                    "exec env '{}' private reservation is missing approver or positive bytes",
                    lease.env_id
                )));
            }
            let approved_at = db::normalize_utc_iso_or_now("");
            let value_json = serde_json::to_string(&PersistedPrivateTargetReservation {
                env_id: &lease.env_id,
                approved_by: &reservation.approved_by,
                reserved_bytes: reservation.reserved_bytes,
                target_path: &reservation.target_path,
                resource_id: &target.resource_id,
                approved_at: &approved_at,
            })
            .map_err(|error| MemoryError::InvalidArg(error.to_string()))?;
            db::set_state(
                &tx,
                PRIVATE_TARGET_RESERVATION_NAMESPACE,
                &lease.env_id,
                &value_json,
            )?;
        }
        let (binding_count, inactive_count, worktree_binding_count): (i64, i64, i64) = tx
            .query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN r.state = 'active' THEN 0 ELSE 1 END), 0),
                        COALESCE(SUM(CASE WHEN r.kind = 'worktree' AND r.path = ?2 THEN 1 ELSE 0 END), 0)
                 FROM exec_env_resource_bindings b
                 JOIN exec_env_resources r ON r.resource_id = b.resource_id
                 WHERE b.env_id = ?1 AND b.released_at IS NULL",
                params![lease.env_id, lease.path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        if binding_count != resources.len() as i64
            || inactive_count != 0
            || worktree_binding_count != 1
        {
            return Err(MemoryError::InvalidArg(format!(
                "exec env '{}' has incomplete publication evidence: {binding_count} bindings, {inactive_count} inactive resources, {worktree_binding_count} canonical worktrees",
                lease.env_id
            )));
        }
        let changed = tx.execute(
            "UPDATE exec_envs SET state = 'active' WHERE env_id = ?1 AND state = 'provisioning'",
            params![lease.env_id],
        )?;
        if changed != 1 {
            return Err(MemoryError::InvalidArg(format!(
                "exec env '{}' lost complete provisioning evidence before publication",
                lease.env_id
            )));
        }
        tx.commit()?;
        let worktree = published
            .iter()
            .find(|resource| resource.kind == db::exec_env_resources::ResourceKind::Worktree)
            .cloned()
            .expect("validated worktree resource is present");
        let build_target = published
            .into_iter()
            .find(|resource| resource.kind == db::exec_env_resources::ResourceKind::BuildTarget);
        Ok(PublishedExecEnv {
            worktree,
            build_target,
        })
    }
}
