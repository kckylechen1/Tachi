//! Report of the git-repository identity each project library answers to
//! (#2252). The identity name is derived from the repository root
//! (`path_utils::plan_c_root_derived_names`), like the path generations, so it
//! needs no migration.
//!
//! The repository short name (`Tachi` for `kckylechen1/Tachi`) is different:
//! it is only an alias once recorded on the library's manifest entry (#2238).
//! The report plans those records; `--apply` writes them under the manifest
//! registration lock. An alias that any other library could also claim is
//! never written.

use std::error::Error;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::manifest::Manifest;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct GitIdentityRow {
    pub library: String,
    pub repository: String,
    pub identity_name: String,
    pub db: PathBuf,
}

pub(super) fn git_identity_rows(tachi_home: &Path) -> Vec<GitIdentityRow> {
    let mut rows: Vec<GitIdentityRow> = std::fs::read_dir(tachi_home.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let db = ["tachi-memory.db", "memory.db"]
                .iter()
                .map(|leaf| dir.join(leaf))
                .find(|path| path.exists())
                .and_then(|path| std::fs::canonicalize(path).ok())?;
            let root =
                crate::path_utils::plan_c_project_root_from_local_db_in_home(&db, tachi_home)?;
            let identity = crate::path_utils::git_identity::git_library_identity(&root)?;
            Some(GitIdentityRow {
                library: entry.file_name().to_string_lossy().into_owned(),
                repository: identity.display().to_string(),
                identity_name: identity.dir_name(),
                db,
            })
        })
        .collect();
    rows.sort_by(|a, b| a.library.cmp(&b.library));
    rows
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum AliasStatus {
    WouldRecord,
    Recorded,
    AlreadyRecorded,
    Skipped,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct AliasDecision {
    pub db: PathBuf,
    pub repository: String,
    pub alias: String,
    pub status: AliasStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Manifest entries (indices) that address this DB.
    #[serde(skip)]
    entries: Vec<usize>,
}

/// One name some library already answers to, case-folded.
struct NameClaim {
    name: String,
    db: PathBuf,
    owner: String,
}

/// Plan the repository-name alias for every repo-local library. A
/// candidate is skipped when it is not a valid project identity, or when
/// another DB claims the same name case-insensitively: a scope or derived
/// name, a recorded alias, a library directory, or another repository's
/// candidate in this same plan.
pub(super) fn plan_aliases(manifest: &Manifest, tachi_home: &Path) -> Vec<AliasDecision> {
    let canonical = |path: &str| std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
    let root_of = |path: &str| {
        crate::path_utils::plan_c_project_root_from_local_db_in_home(Path::new(path), tachi_home)
    };

    let mut decisions: Vec<AliasDecision> = Vec::new();
    let mut claims: Vec<NameClaim> = Vec::new();
    for (index, entry) in manifest.dbs.iter().enumerate() {
        let db = canonical(&entry.path);
        let root = root_of(&entry.path);
        let owner = format!("manifest entry {}", entry.path);
        let mut names: Vec<String> = entry.aliases.clone();
        names.extend(
            entry
                .scope_hint
                .strip_prefix("project:")
                .map(str::to_string),
        );
        if let Some(root) = &root {
            names.extend(crate::path_utils::plan_c_root_derived_names(root));
        }
        claims.extend(names.into_iter().map(|name| NameClaim {
            name: name.to_ascii_lowercase(),
            db: db.clone(),
            owner: owner.clone(),
        }));

        let Some(identity) = root
            .as_deref()
            .and_then(crate::path_utils::git_identity::git_library_identity)
        else {
            continue;
        };
        if let Some(existing) = decisions.iter_mut().find(|decision| decision.db == db) {
            existing.entries.push(index);
            continue;
        }
        decisions.push(AliasDecision {
            db,
            repository: identity.display().to_string(),
            alias: identity.repo_name().to_string(),
            status: AliasStatus::WouldRecord,
            reason: None,
            entries: vec![index],
        });
    }
    for dir in std::fs::read_dir(tachi_home.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
    {
        // Every DB leaf in the directory claims the name: a second, distinct
        // `memory.db` beside a linked `tachi-memory.db` must still block it.
        for leaf in [
            memcore::MEMORY_DB_FILENAME,
            memcore::LEGACY_MEMORY_DB_FILENAME,
        ]
        .iter()
        .map(|leaf| dir.path().join(leaf))
        .filter(|path| std::fs::symlink_metadata(path).is_ok())
        {
            claims.push(NameClaim {
                name: dir.file_name().to_string_lossy().to_ascii_lowercase(),
                db: canonical(&leaf.to_string_lossy()),
                owner: format!("library {}", leaf.display()),
            });
        }
    }
    // An alias lives on a manifest entry, so a library without one is
    // reported rather than omitted. Its candidate still counts below.
    for row in git_identity_rows(tachi_home) {
        if decisions.iter().any(|decision| decision.db == row.db) {
            continue;
        }
        decisions.push(AliasDecision {
            alias: row
                .repository
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string(),
            db: row.db,
            repository: row.repository,
            status: AliasStatus::Skipped,
            reason: Some("no manifest entry; run `tachi manifest refresh` first".to_string()),
            entries: Vec::new(),
        });
    }
    for name in ["global", "wiki", "unknown"] {
        claims.push(NameClaim {
            name: name.to_string(),
            db: PathBuf::new(),
            owner: format!("reserved name '{name}'"),
        });
    }
    claims.extend(decisions.iter().map(|decision| NameClaim {
        name: decision.alias.to_ascii_lowercase(),
        db: decision.db.clone(),
        owner: format!(
            "repository {} ({})",
            decision.repository,
            decision.db.display()
        ),
    }));

    for decision in &mut decisions {
        if decision.entries.is_empty() {
            continue;
        }
        let recorded = decision.entries.iter().all(|&index| {
            manifest.dbs[index]
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(&decision.alias))
        });
        let folded = decision.alias.to_ascii_lowercase();
        let conflicts: Vec<&str> = claims
            .iter()
            .filter(|claim| claim.name == folded && claim.db != decision.db)
            .map(|claim| claim.owner.as_str())
            .collect();
        if crate::MemoryServer::validate_named_project(&decision.alias).is_err() {
            decision.status = AliasStatus::Skipped;
            decision.reason = Some(format!(
                "repository name '{}' is not a valid project identity",
                decision.alias
            ));
        } else if !conflicts.is_empty() {
            decision.status = AliasStatus::Skipped;
            decision.reason = Some(format!(
                "ambiguous with {}{}",
                conflicts.join(", "),
                if recorded {
                    "; the recorded alias is left unchanged"
                } else {
                    ""
                }
            ));
        } else if recorded {
            decision.status = AliasStatus::AlreadyRecorded;
        }
    }
    decisions.sort_by(|a, b| a.db.cmp(&b.db));
    decisions
}

/// An absent manifest plans as empty; it has no entry to record on, so it is
/// never created here.
fn load_manifest(manifest_path: &Path) -> Result<Manifest, String> {
    match Manifest::load(manifest_path) {
        Ok(manifest) => Ok(manifest),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::empty()),
        Err(error) => Err(format!(
            "read manifest {}: {error}",
            manifest_path.display()
        )),
    }
}

/// Record every unambiguous planned alias. The plan is recomputed from the
/// manifest read under the registration lock, so it cannot act on a stale
/// view; an unchanged manifest is not rewritten.
pub(super) fn apply_aliases(tachi_home: &Path) -> Result<Vec<AliasDecision>, String> {
    // The registration file lock is a no-op off Unix, so a concurrent daemon
    // registration could be lost; refuse rather than race.
    if !cfg!(unix) {
        return Err(
            "recording repository aliases requires the Unix manifest file lock; not supported on this platform"
                .to_string(),
        );
    }
    crate::project_db_ops::with_manifest_registration_lock_in_home(tachi_home, |manifest_path| {
        let mut manifest = load_manifest(manifest_path)?;
        let mut decisions = plan_aliases(&manifest, tachi_home);
        let mut changed = false;
        for decision in &mut decisions {
            if decision.status != AliasStatus::WouldRecord {
                continue;
            }
            for &index in &decision.entries {
                let aliases = &mut manifest.dbs[index].aliases;
                if !aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(&decision.alias))
                {
                    aliases.push(decision.alias.clone());
                    changed = true;
                }
            }
            decision.status = AliasStatus::Recorded;
        }
        if changed {
            manifest.generated_at = chrono::Utc::now().to_rfc3339();
            manifest
                .save(manifest_path)
                .map_err(|error| format!("save manifest {}: {error}", manifest_path.display()))?;
        }
        Ok(decisions)
    })
}

#[derive(Serialize)]
struct GitIdentityReport {
    libraries: Vec<GitIdentityRow>,
    aliases: Vec<AliasDecision>,
}

pub(in crate::bootstrap) fn run_git_identity(
    json: bool,
    apply: bool,
    tachi_home: &Path,
) -> Result<(), Box<dyn Error>> {
    let rows = git_identity_rows(tachi_home);
    let aliases = if apply {
        apply_aliases(tachi_home)?
    } else {
        plan_aliases(
            &load_manifest(&tachi_home.join("manifest.json"))?,
            tachi_home,
        )
    };
    if json {
        let report = GitIdentityReport {
            libraries: rows,
            aliases,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Git identity names (derived from each repository, nothing to migrate)\n");
    for row in &rows {
        println!(
            "{:<46} {:<34} also answers to {}",
            row.library, row.repository, row.identity_name
        );
    }
    if apply {
        println!("\nRepository aliases (applied)\n");
    } else {
        println!("\nRepository aliases (plan only; --apply records them)\n");
    }
    for decision in &aliases {
        let status = match decision.status {
            AliasStatus::WouldRecord => "would record",
            AliasStatus::Recorded => "recorded",
            AliasStatus::AlreadyRecorded => "already recorded",
            AliasStatus::Skipped => "skipped",
        };
        println!(
            "{:<24} {:<34} {:<16} {}",
            decision.alias,
            decision.repository,
            status,
            decision.db.display()
        );
        if let Some(reason) = &decision.reason {
            println!("    {reason}");
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn rows_name_each_repo_local_library_by_its_repository() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repos/Sigil");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(".tachi")).unwrap();
        std::fs::write(
            repo.join(".git/config"),
            "[remote \"origin\"]\n\turl = https://github.com/kckylechen1/Tachi.git\n",
        )
        .unwrap();
        std::fs::write(repo.join(".tachi/tachi-memory.db"), b"").unwrap();
        let alias = home.join("projects/Sigil-82fc");
        std::fs::create_dir_all(&alias).unwrap();
        std::os::unix::fs::symlink(
            repo.join(".tachi/tachi-memory.db"),
            alias.join("tachi-memory.db"),
        )
        .unwrap();
        std::fs::create_dir_all(home.join("projects/hapi")).unwrap();
        std::fs::write(home.join("projects/hapi/memory.db"), b"").unwrap();

        let rows = git_identity_rows(&home);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].library, "Sigil-82fc");
        assert_eq!(rows[0].repository, "kckylechen1/Tachi");
        assert_eq!(rows[0].identity_name, "github.com_kckylechen1_tachi");
    }

    fn repo_db(temp: &Path, dir: &str, origin: &str) -> PathBuf {
        let repo = temp.join("repos").join(dir);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(".tachi")).unwrap();
        std::fs::write(
            repo.join(".git/config"),
            format!("[remote \"origin\"]\n\turl = {origin}\n"),
        )
        .unwrap();
        let db = repo.join(".tachi/tachi-memory.db");
        std::fs::write(&db, b"").unwrap();
        std::fs::canonicalize(db).unwrap()
    }

    fn project_entry(db: &Path) -> crate::manifest::DbEntry {
        crate::manifest::DbEntry {
            path: db.display().to_string(),
            role: crate::manifest::DbRole::Project,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: false,
            allow_write: true,
            last_doctor_at: "1970-01-01T00:00:00Z".to_string(),
            last_classification: "healthy".to_string(),
            scope_hint: String::new(),
            notes: String::new(),
            aliases: Vec::new(),
        }
    }

    /// Two repositories sharing a short name both stay unaliased, whatever
    /// the manifest order; a unique name is recorded once and a re-run
    /// leaves the manifest byte-identical.
    #[test]
    fn apply_records_unique_names_skips_shared_ones_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let home = temp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let widget = repo_db(&temp, "widget", "https://github.com/acme/Widget.git");
        let fork = repo_db(&temp, "fork", "git@github.com:other/widget.git");
        let gadget = repo_db(
            &temp,
            "gadget-checkout",
            "https://github.com/acme/Gadget.git",
        );
        let mut manifest = Manifest::empty();
        manifest.dbs = vec![
            project_entry(&widget),
            project_entry(&fork),
            project_entry(&gadget),
        ];
        let manifest_path = home.join("manifest.json");
        manifest.save(&manifest_path).unwrap();

        let decisions = apply_aliases(&home).unwrap();
        let by_db = |db: &Path| {
            decisions
                .iter()
                .find(|decision| decision.db == db)
                .unwrap_or_else(|| panic!("{decisions:#?}"))
        };
        for (db, other) in [(&widget, "other/widget"), (&fork, "acme/Widget")] {
            let decision = by_db(db);
            assert_eq!(decision.status, AliasStatus::Skipped, "{decision:#?}");
            assert!(
                decision.reason.as_deref().unwrap().contains(other),
                "{decision:#?}"
            );
        }
        assert_eq!(by_db(&gadget).status, AliasStatus::Recorded);

        let written = Manifest::load(&manifest_path).unwrap();
        let aliases: Vec<&[String]> = written
            .dbs
            .iter()
            .map(|entry| entry.aliases.as_slice())
            .collect();
        assert_eq!(aliases, [&[][..], &[][..], &["Gadget".to_string()][..]]);

        let before = std::fs::read(&manifest_path).unwrap();
        let rerun = apply_aliases(&home).unwrap();
        assert_eq!(
            rerun
                .iter()
                .find(|decision| decision.db == gadget)
                .map(|decision| decision.status),
            Some(AliasStatus::AlreadyRecorded)
        );
        assert_eq!(std::fs::read(&manifest_path).unwrap(), before);
    }

    /// Review R1 (#2275): a library directory whose linked leaf is this
    /// repository but whose second leaf is a distinct DB still owns the name.
    #[test]
    fn apply_skips_alias_claimed_by_any_leaf_of_a_library_directory() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let home = temp.join("home");
        let widget = repo_db(
            &temp,
            "widget-checkout",
            "https://github.com/acme/Widget.git",
        );
        let library = home.join("projects/Widget");
        std::fs::create_dir_all(&library).unwrap();
        std::os::unix::fs::symlink(&widget, library.join(memcore::MEMORY_DB_FILENAME)).unwrap();
        std::fs::write(library.join(memcore::LEGACY_MEMORY_DB_FILENAME), b"other").unwrap();
        let mut manifest = Manifest::empty();
        manifest.dbs = vec![project_entry(&widget)];
        manifest.save(&home.join("manifest.json")).unwrap();

        let decisions = apply_aliases(&home).unwrap();
        let decision = decisions.iter().find(|d| d.db == widget).unwrap();
        assert_eq!(decision.status, AliasStatus::Skipped, "{decision:#?}");
        let written = Manifest::load(&home.join("manifest.json")).unwrap();
        assert!(written.dbs[0].aliases.is_empty());
    }
}
