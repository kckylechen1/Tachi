//! Read-only source resolution for the #1073 pilot.
//!
//! This module deliberately opens only the two user-authorised databases and
//! only with SQLite's read-only flag.  A row must match its route, stable id,
//! revision, and full UTF-8 content SHA-256 before it can be passed to a
//! producer.  No resolver method formats or persists source text.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};

use super::pilot::{PilotRowV1, PilotSourceRouteV1};

pub const DEFAULT_ANTIGRAVITY_SOURCE_DB: &str = "/Users/kckylechen/.gemini/antigravity/memory.db";
pub const DEFAULT_HAPI_PROJECT_DB: &str = "/Users/kckylechen/.tachi/projects/hapi/memory.db";

/// Complete source text exists only in process memory between a successful
/// read-only verification and the producer call.  It is intentionally absent
/// from all manifest and report types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPilotSourceV1 {
    pub source_route: PilotSourceRouteV1,
    pub source_id: String,
    pub source_revision: i64,
    pub full_text: String,
}

impl ResolvedPilotSourceV1 {
    pub fn content_sha256(&self) -> String {
        format!("{:x}", Sha256::digest(self.full_text.as_bytes()))
    }
}

#[derive(Debug)]
pub enum SourceResolveError {
    ReadOnlyOpen {
        source_route: PilotSourceRouteV1,
        message: String,
    },
    Query {
        source_route: PilotSourceRouteV1,
        message: String,
    },
    MissingExactRevision {
        source_route: PilotSourceRouteV1,
        source_id: String,
        source_revision: i64,
    },
    BindingMismatch {
        source_route: PilotSourceRouteV1,
        source_id: String,
        source_revision: i64,
    },
    DigestMismatch {
        source_route: PilotSourceRouteV1,
        source_id: String,
        source_revision: i64,
        expected: String,
        actual: String,
    },
}

impl std::fmt::Display for SourceResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnlyOpen {
                source_route,
                message,
            } => write!(
                f,
                "could not open {} source database read-only: {message}",
                source_route.as_str()
            ),
            Self::Query {
                source_route,
                message,
            } => write!(
                f,
                "could not read {} source database: {message}",
                source_route.as_str()
            ),
            Self::MissingExactRevision {
                source_route,
                source_id,
                source_revision,
            } => write!(
                f,
                "{} source {}@{} is absent or not active",
                source_route.as_str(),
                source_id,
                source_revision
            ),
            Self::BindingMismatch {
                source_route,
                source_id,
                source_revision,
            } => write!(
                f,
                "resolved {} source {}@{} does not match the manifest binding",
                source_route.as_str(),
                source_id,
                source_revision
            ),
            Self::DigestMismatch {
                source_route,
                source_id,
                source_revision,
                expected,
                actual,
            } => write!(
                f,
                "{} source {}@{} digest mismatch: expected {expected}, read {actual}",
                source_route.as_str(),
                source_id,
                source_revision
            ),
        }
    }
}

impl std::error::Error for SourceResolveError {}

/// Resolver seam used by the runner.  Implementations must resolve the exact
/// manifest row, verify its digest, and return no public/reportable text.
pub trait PilotSourceResolverV1 {
    fn resolve_verified(
        &self,
        binding: &PilotRowV1,
    ) -> Result<ResolvedPilotSourceV1, SourceResolveError>;
}

/// Recheck a resolver result at the consuming boundary. Implementations of
/// the resolver trait are injectable, so neither durable persistence nor
/// runtime spend trusts the implementation's claim without comparing the
/// complete route/id/revision/digest binding again.
pub fn verify_resolved_source_v1(
    binding: &PilotRowV1,
    resolved: &ResolvedPilotSourceV1,
) -> Result<(), SourceResolveError> {
    if resolved.source_route != binding.source_route
        || resolved.source_id != binding.source_id
        || resolved.source_revision != binding.source_revision
    {
        return Err(SourceResolveError::BindingMismatch {
            source_route: binding.source_route,
            source_id: binding.source_id.clone(),
            source_revision: binding.source_revision,
        });
    }
    let actual = resolved.content_sha256();
    if actual != binding.content_sha256 {
        return Err(SourceResolveError::DigestMismatch {
            source_route: binding.source_route,
            source_id: binding.source_id.clone(),
            source_revision: binding.source_revision,
            expected: binding.content_sha256.clone(),
            actual,
        });
    }
    Ok(())
}

/// The only production source resolver for phase 1.  Environment overrides
/// are opt-in and limited to the two named variables; it never scans HOME or
/// discovers databases dynamically.
#[derive(Debug, Clone)]
pub struct SqlitePilotSourceResolverV1 {
    antigravity_db: PathBuf,
    hapi_db: PathBuf,
}

impl SqlitePilotSourceResolverV1 {
    pub fn from_env() -> Self {
        Self {
            antigravity_db: std::env::var_os("ANTIGRAVITY_SOURCE_DB")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_ANTIGRAVITY_SOURCE_DB)),
            hapi_db: std::env::var_os("HAPI_PROJECT_DB")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_HAPI_PROJECT_DB)),
        }
    }

    pub fn with_paths(antigravity_db: PathBuf, hapi_db: PathBuf) -> Self {
        Self {
            antigravity_db,
            hapi_db,
        }
    }

    fn path_for(&self, source_route: PilotSourceRouteV1) -> &Path {
        match source_route {
            PilotSourceRouteV1::Antigravity => &self.antigravity_db,
            PilotSourceRouteV1::Hapi => &self.hapi_db,
        }
    }
}

impl PilotSourceResolverV1 for SqlitePilotSourceResolverV1 {
    fn resolve_verified(
        &self,
        binding: &PilotRowV1,
    ) -> Result<ResolvedPilotSourceV1, SourceResolveError> {
        let route = binding.source_route;
        let conn = Connection::open_with_flags(
            self.path_for(route),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|err| SourceResolveError::ReadOnlyOpen {
            source_route: route,
            message: err.to_string(),
        })?;

        // Both authorised stores use the Tachi memories schema.  Selecting by
        // id AND revision avoids accepting a newer row that happened to reuse
        // the stable id.  `archived = 0` prevents resurrecting withdrawn data.
        let resolved = conn
            .query_row(
                "SELECT id, revision, text FROM memories \
                 WHERE id = ?1 AND revision = ?2 AND archived = 0 LIMIT 1",
                (&binding.source_id, binding.source_revision),
                |row| {
                    Ok(ResolvedPilotSourceV1 {
                        source_route: route,
                        source_id: row.get(0)?,
                        source_revision: row.get(1)?,
                        full_text: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|err| SourceResolveError::Query {
                source_route: route,
                message: err.to_string(),
            })?
            .ok_or_else(|| SourceResolveError::MissingExactRevision {
                source_route: route,
                source_id: binding.source_id.clone(),
                source_revision: binding.source_revision,
            })?;

        verify_resolved_source_v1(binding, &resolved)?;
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pilot::{PilotRowKindV1, PilotSourceRouteV1, PilotStratumV1};
    use tachi_params::LessonCandidateKindV1;

    fn binding(text: &str) -> PilotRowV1 {
        PilotRowV1 {
            source_route: PilotSourceRouteV1::Antigravity,
            source_id: "source-1".to_string(),
            source_revision: 7,
            content_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
            capture_timestamp: "2026-07-24T00:00:00Z".to_string(),
            kind: PilotRowKindV1::Narrative,
            stratum: PilotStratumV1::CorrectionAlignment,
            selection_reason: "public-safe synthetic reason".to_string(),
            reference_decision: "public-safe synthetic decision".to_string(),
            target_kind: LessonCandidateKindV1::Precedent,
        }
    }

    fn test_db(text: &str) -> (PathBuf, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("sigil-1073-source-{}.db", uuid::Uuid::new_v4()));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id TEXT, revision INTEGER, text TEXT, archived INTEGER);",
        )
        .unwrap_or_else(|_| panic!("test database setup failed"));
        // A parameterized statement keeps the test fixture isolated from the
        // production query and avoids interpolating source text into SQL.
        conn.execute(
            "INSERT INTO memories (id, revision, text, archived) VALUES (?1, ?2, ?3, 0)",
            ("source-1", 7, text),
        )
        .unwrap();
        (path.clone(), path)
    }

    #[test]
    fn resolver_requires_the_exact_full_content_digest() {
        let text = "synthetic source body";
        let (antigravity, hapi) = test_db(text);
        let resolver = SqlitePilotSourceResolverV1::with_paths(antigravity.clone(), hapi);
        let resolved = resolver
            .resolve_verified(&binding(text))
            .expect("exact binding resolves");
        assert_eq!(resolved.content_sha256(), binding(text).content_sha256);

        let mut tampered = binding(text);
        tampered.content_sha256 = "0".repeat(64);
        let error = resolver
            .resolve_verified(&tampered)
            .expect_err("digest mismatch is spend-blocking");
        let _ = std::fs::remove_file(antigravity);
        assert!(matches!(error, SourceResolveError::DigestMismatch { .. }));
    }
}
