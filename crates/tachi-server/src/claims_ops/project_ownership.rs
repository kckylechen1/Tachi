//! Project authority originates in one admitted local server context.
use crate::MemoryServer;
use memcore::db::session_claims;

pub(crate) const UNAVAILABLE: &str = "work ownership is unavailable";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalProject {
    pub(crate) identity: String,
    pub(crate) connection_id: String,
    pub(crate) project: String,
}

impl LocalProject {
    pub(crate) fn capture(server: &MemoryServer) -> Option<Self> {
        let runtime = server.agent_runtime_read();
        let connection = runtime.work_claim_connection.as_ref()?;
        if connection.admission != "self_asserted" {
            return None;
        }
        let project = runtime.session_project.as_ref().filter(|p| !p.is_empty())?;
        Some(Self {
            identity: connection.agent_identity_id.clone()?,
            connection_id: connection.connection_id.clone(),
            project: project.clone(),
        })
    }

    pub(crate) fn with_current<T>(
        &self,
        server: &MemoryServer,
        operation: impl FnOnce(&rusqlite::Connection) -> Result<T, memcore::MemoryError>,
    ) -> Result<T, String> {
        self.with_current_mapped(server, operation, false)
    }

    fn with_current_mapped<T>(
        &self,
        server: &MemoryServer,
        operation: impl FnOnce(&rusqlite::Connection) -> Result<T, memcore::MemoryError>,
        preserve_operation_error: bool,
    ) -> Result<T, String> {
        // Hold the same runtime snapshot through the synchronous transaction.
        // Identity/project replacement cannot race this authority check.
        let runtime = server.agent_runtime_read();
        if runtime.session_project.as_deref() != Some(&self.project)
            || !runtime
                .work_claim_connection
                .as_ref()
                .is_some_and(|connection| {
                    connection.admission == "self_asserted"
                        && connection.connection_id == self.connection_id
                        && connection.agent_identity_id.as_deref() == Some(&self.identity)
                })
        {
            return Err(UNAVAILABLE.into());
        }
        server.with_global_store(|store| {
            let mut admitted = false;
            let result = session_claims::with_local_project_admission(
                store.connection(),
                &self.identity,
                &self.connection_id,
                |conn| {
                    admitted = true;
                    operation(conn)
                },
            );
            // Keep Err inside the transaction so all operation writes roll back.
            result.map_err(|error| {
                if admitted && preserve_operation_error {
                    error.to_string()
                } else {
                    UNAVAILABLE.to_string()
                }
            })
        })
    }
}

pub(crate) fn with_claim_transition<T>(
    server: &MemoryServer,
    claim_id: &str,
    operation: impl FnOnce(&rusqlite::Connection) -> Result<T, memcore::MemoryError>,
) -> Result<T, String> {
    with_claim_transition_checked(server, claim_id, || Ok(()), operation)
}

pub(crate) fn with_claim_transition_checked<T>(
    server: &MemoryServer,
    claim_id: &str,
    validate: impl FnOnce() -> Result<(), memcore::MemoryError>,
    operation: impl FnOnce(&rusqlite::Connection) -> Result<T, memcore::MemoryError>,
) -> Result<T, String> {
    let missing = || memcore::MemoryError::NotFound(format!("claim {claim_id}"));
    if let Some(binding) = LocalProject::capture(server) {
        binding.with_current_mapped(
            server,
            |conn| {
                validate()?;
                if let Some(project) = session_claims::work_claim_project(conn, claim_id)? {
                    if project != binding.project {
                        return Err(missing());
                    }
                    // Contract (identity-workclaim-spine-v1): a DIFFERENT
                    // identity must first explicitly claim in this project to
                    // enter the holder/state/version checks — INCLUDING orphan
                    // handoff. Unqualified callers get the missing-target
                    // response (masked, never memcore's holder_mismatch);
                    // qualified callers proceed to memcore's honest rules.
                    let held: bool = conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM session_claims WHERE claim_id=?1 \
                         AND agent_identity_id=?2)",
                        rusqlite::params![claim_id, binding.identity],
                        |row| row.get(0),
                    )?;
                    if !held
                        && !session_claims::has_current_project_claim(
                            conn,
                            &binding.project,
                            &binding.identity,
                            &binding.connection_id,
                            &chrono::Utc::now().to_rfc3339(),
                        )?
                    {
                        return Err(missing());
                    }
                }
                operation(conn)
            },
            true,
        )
    } else {
        crate::verified_admission::with_current_admission_write(server, |conn| {
            validate()?;
            if session_claims::work_claim_project(conn, claim_id)?.is_some() {
                return Err(missing());
            }
            operation(conn)
        })
    }
}
