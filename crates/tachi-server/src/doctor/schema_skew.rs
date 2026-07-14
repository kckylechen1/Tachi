//! #1119 — schema-version-skew tripwire.
//!
//! The incident this closes: a dev-built binary opened a live project DB and
//! silently upgraded its `PRAGMA user_version` to a schema newer than the
//! DEPLOYED daemon's `EXPECTED_SCHEMA_VERSION` supports. The deployed daemon
//! then refused every poll of that DB (correctly — see
//! `memcore::db::migrations::check_schema_version_gate`) and WARN-spammed for
//! hours before anyone noticed. The typed `DbOpenContext` gate
//! (`memcore::db::migrations::check_db_open_context_gate`, also #1119) stops it
//! from happening again going forward, but it never PROACTIVELY tells an
//! operator running `tachi doctor` "here is a DB that is already skewed" — this
//! module does.
//!
//! Scope: only findings whose `schema_kind == "tachi"` and that opened cleanly
//! (no `error`) are probed — i.e. `Healthy` / `VecExtensionMissing` /
//! `WalOrphan` findings (see `classify::classify_one`), never
//! `Corrupt` / `Placeholder` / `Backup` / `LegacySchema` (openclaw-era DBs
//! predate `PRAGMA user_version` entirely and read back `0`, which would
//! otherwise false-positive as "way behind"). Informational only, like every
//! other `doctor` warning in this module family: it never blocks a scan and
//! never touches the DB (read-only immutable open, same as `classify_one`).

use std::path::Path;

use memcore::db::migrations::{read_schema_version, EXPECTED_SCHEMA_VERSION};

use super::classify::make_immutable_uri;
use super::{DbClassification, DoctorFinding, DoctorWarning};

/// For every `DoctorFinding` that looks like a live tachi DB this binary might
/// actually poll, read its `PRAGMA user_version` (best-effort, read-only) and
/// compare against this binary's `EXPECTED_SCHEMA_VERSION`. Emits one
/// [`DoctorWarning`] per skewed DB, in either direction:
///
/// - DB AHEAD of this binary (`stored > EXPECTED_SCHEMA_VERSION`) — the exact
///   #1119 poisoning shape: some other (newer) binary already migrated it, and
///   THIS binary will refuse every open of it from here on.
/// - DB BEHIND this binary (`0 < stored < EXPECTED_SCHEMA_VERSION`) — not fatal
///   (the deploy daemon can still open/migrate it, `--allow-schema-migration`
///   permitting), but worth naming before a poller's first touch surprises an
///   operator with a migration or a #1119 refusal.
///
/// A `stored == 0` (openclaw-legacy or genuinely never-stamped) DB is never
/// flagged — the migration gate treats `0` as a build, not a migration, and
/// this tripwire mirrors that boundary rather than re-litigating it.
pub fn schema_version_skew_warnings(findings: &[DoctorFinding]) -> Vec<DoctorWarning> {
    let mut warnings = Vec::new();
    for finding in findings {
        if !is_probeable_tachi_db(finding) {
            continue;
        }
        let Some(stored) = probe_user_version(Path::new(&finding.path)) else {
            continue;
        };
        if stored == 0 || stored == EXPECTED_SCHEMA_VERSION {
            continue;
        }

        let warning = if stored > EXPECTED_SCHEMA_VERSION {
            DoctorWarning {
                code: "schema_version_ahead_of_binary".to_string(),
                path: finding.path.clone(),
                message: format!(
                    "{}: stamped schema {stored} is NEWER than this binary's supported \
                     {EXPECTED_SCHEMA_VERSION} — a newer-schema binary already migrated it \
                     in place (kckylechen1/Sigil#1119). This binary will refuse every \
                     open/poll of this DB from here on.",
                    finding.path
                ),
                remediation: "deploy a binary built at or after this DB's schema version to \
                    every channel that polls it (or restore the pre-migration backup if the \
                    migration was unintended); see \
                    docs/engineering/architecture/release-distribution.md"
                    .to_string(),
            }
        } else {
            DoctorWarning {
                code: "schema_version_behind_binary".to_string(),
                path: finding.path.clone(),
                message: format!(
                    "{}: stamped schema {stored} is behind this binary's \
                     {EXPECTED_SCHEMA_VERSION}; it stays readable but this binary will refuse \
                     to auto-migrate it (kckylechen1/Sigil#1119) unless the process that opens \
                     it was started with migration authority (the deploy ritual's \
                     `tachi serve --allow-schema-migration`).",
                    finding.path
                ),
                remediation: "harmless if this DB is not yet due for a write from this \
                    binary; if it IS, let the deploy ritual's opted-in daemon \
                    (`--allow-schema-migration`) touch it first"
                    .to_string(),
            }
        };
        warnings.push(warning);
    }
    warnings
}

fn is_probeable_tachi_db(finding: &DoctorFinding) -> bool {
    finding.error.is_none()
        && finding.schema_kind == "tachi"
        && matches!(
            finding.classification,
            DbClassification::Healthy
                | DbClassification::VecExtensionMissing
                | DbClassification::WalOrphan
        )
}

/// Best-effort `PRAGMA user_version` read via a read-only immutable open —
/// mirrors `classify_one`'s own open path. `None` on any open/read failure
/// (informational tripwire; never surfaces its own probe failures as a
/// warning, consistent with the rest of this module family).
fn probe_user_version(path: &Path) -> Option<u32> {
    let uri = make_immutable_uri(path);
    let conn = memcore::db::open_immutable_readonly(&uri).ok()?;
    read_schema_version(&conn).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::JobBreakdown;

    fn finding(
        path: &str,
        classification: DbClassification,
        schema_kind: &str,
        error: Option<&str>,
    ) -> DoctorFinding {
        DoctorFinding {
            path: path.to_string(),
            classification,
            file_size: 4096,
            has_wal: false,
            mem_count: Some(0),
            vec_rowid_count: None,
            none_domain_count: None,
            cross_domain_suspect_count: None,
            cross_domain_suspect_sample: Vec::new(),
            jobs: JobBreakdown::default(),
            schema_kind: schema_kind.to_string(),
            error: error.map(|e| e.to_string()),
            scope_hint: "global".to_string(),
        }
    }

    fn make_tachi_db_at_version(version: u32) -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let _ = libsimple::enable_auto_extension();
        memcore::db::register_sqlite_vec();
        let conn = rusqlite::Connection::open(tmp.path()).expect("open");
        let _ = memcore::db::try_load_sqlite_vec(&conn);
        memcore::db::init_schema(&conn).expect("init_schema");
        conn.execute_batch(&format!("PRAGMA user_version = {version}"))
            .expect("stamp version");
        tmp
    }

    #[test]
    fn flags_db_ahead_of_this_binary() {
        let tmp = make_tachi_db_at_version(EXPECTED_SCHEMA_VERSION + 1);
        let findings = vec![finding(
            tmp.path().to_str().unwrap(),
            DbClassification::Healthy,
            "tachi",
            None,
        )];

        let warnings = schema_version_skew_warnings(&findings);
        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert_eq!(warnings[0].code, "schema_version_ahead_of_binary");
        assert!(warnings[0].message.contains("NEWER"));
    }

    #[test]
    fn flags_db_behind_this_binary() {
        let tmp = make_tachi_db_at_version(EXPECTED_SCHEMA_VERSION - 1);
        let findings = vec![finding(
            tmp.path().to_str().unwrap(),
            DbClassification::Healthy,
            "tachi",
            None,
        )];

        let warnings = schema_version_skew_warnings(&findings);
        assert_eq!(warnings.len(), 1, "got: {warnings:?}");
        assert_eq!(warnings[0].code, "schema_version_behind_binary");
        assert!(warnings[0].message.contains("--allow-schema-migration"));
    }

    #[test]
    fn does_not_flag_db_at_current_version() {
        let tmp = make_tachi_db_at_version(EXPECTED_SCHEMA_VERSION);
        let findings = vec![finding(
            tmp.path().to_str().unwrap(),
            DbClassification::Healthy,
            "tachi",
            None,
        )];

        assert!(schema_version_skew_warnings(&findings).is_empty());
    }

    #[test]
    fn does_not_flag_fresh_zero_stamped_db() {
        let tmp = make_tachi_db_at_version(0);
        let findings = vec![finding(
            tmp.path().to_str().unwrap(),
            DbClassification::Healthy,
            "tachi",
            None,
        )];

        assert!(schema_version_skew_warnings(&findings).is_empty());
    }

    #[test]
    fn skips_non_tachi_and_errored_findings() {
        let tmp = make_tachi_db_at_version(EXPECTED_SCHEMA_VERSION + 1);
        let findings = vec![
            finding(
                tmp.path().to_str().unwrap(),
                DbClassification::LegacySchema,
                "openclaw_legacy",
                None,
            ),
            finding(
                "/nonexistent/errored.db",
                DbClassification::Corrupt,
                "unknown",
                Some("boom"),
            ),
            finding(
                "/nonexistent/placeholder.db",
                DbClassification::Placeholder,
                "empty",
                None,
            ),
            finding(
                "/nonexistent/backup.db",
                DbClassification::Backup,
                "unknown",
                None,
            ),
        ];

        assert!(
            schema_version_skew_warnings(&findings).is_empty(),
            "legacy/corrupt/placeholder/backup findings must never be probed"
        );
    }
}
