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
            session_claims::with_local_project_admission(
                store.connection(),
                &self.identity,
                &self.connection_id,
                operation,
            )
            .map_err(|_| UNAVAILABLE.to_string())
        })
    }
}

pub(crate) fn with_claim_transition<T>(
    server: &MemoryServer,
    claim_id: &str,
    operation: impl FnOnce(&rusqlite::Connection) -> Result<T, memcore::MemoryError>,
) -> Result<T, String> {
    if let Some(binding) = LocalProject::capture(server) {
        binding.with_current(server, |conn| {
            if session_claims::work_claim_project(conn, claim_id)?
                .is_some_and(|project| project != binding.project)
            {
                return Err(memcore::MemoryError::InvalidArg(UNAVAILABLE.into()));
            }
            operation(conn)
        })
    } else {
        crate::verified_admission::with_current_admission_write(server, |conn| {
            if session_claims::work_claim_project(conn, claim_id)?.is_some() {
                return Err(memcore::MemoryError::InvalidArg(UNAVAILABLE.into()));
            }
            operation(conn)
        })
    }
}
