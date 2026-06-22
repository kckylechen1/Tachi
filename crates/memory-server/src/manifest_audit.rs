//! Project-DB relocation audit (DRY-RUN by default).
//!
//! Background: most `~/.tachi/projects/<name>/memory.db` files are *symlinks*
//! into a repo-local `<repo>/.tachi/memory.db`. A handful are **real files**
//! living centrally — real per-project data that was never linked back into its
//! owning repo (e.g. `quant`, `hyperion-*`, `Quant_Analyzer_2026_audit_*`). And
//! some are UUID-named smoke-test directories that are pure garbage.
//!
//! This module enumerates each centralized project DB, classifies it, and emits
//! a **relocation plan**. It performs NO filesystem mutation by itself — the CLI
//! layer ([`crate::bootstrap::manifest_cli`]) decides whether to print the plan
//! (default) or, behind an explicit `--apply` flag, act on it with a backup and
//! an ambiguity refusal.
//!
//! Design notes:
//!   * The classification core ([`classify_project_db`]) is a pure function over
//!     a small [`ProjectDbInput`] struct so it can be unit-tested with synthetic
//!     inputs (symlink vs real file vs uuid name vs owned-vs-home) without ever
//!     touching a real `~/.tachi`.
//!   * The filesystem enumeration ([`audit_projects_dir`]) is a thin shell that
//!     gathers facts (is it a symlink? where does it point? does the target
//!     exist? is there an owning git repo?) and hands each one to the pure core.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// How a centralized `~/.tachi/projects/<name>/memory.db` is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectDbClass {
    /// A symlink whose target exists. This is the normal, healthy case: the
    /// real DB lives in the repo and the central path is just an alias.
    SymlinkAlias,
    /// A symlink whose target is missing/broken. The central path points
    /// nowhere — candidate for GC of the dangling link (never the target).
    SymlinkBroken,
    /// A real file (not a symlink) for which we found an owning git repo. The
    /// data could be relocated into `<repo>/.tachi/memory.db` and re-linked.
    RealFileWithOwningRepo,
    /// A real file with no discoverable owning repo. Keep it as a home-resident
    /// named project; do NOT move it anywhere.
    RealFileHomeResident,
    /// The `<name>` directory looks like a throwaway UUID/smoke-test workspace.
    /// Candidate for GC of the whole directory.
    UuidSmokeTestGarbage,
}

impl ProjectDbClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectDbClass::SymlinkAlias => "symlink-alias",
            ProjectDbClass::SymlinkBroken => "symlink-broken",
            ProjectDbClass::RealFileWithOwningRepo => "real-file-with-owning-repo",
            ProjectDbClass::RealFileHomeResident => "real-file-home-resident",
            ProjectDbClass::UuidSmokeTestGarbage => "uuid-smoke-test-garbage",
        }
    }
}

/// The proposed action for a single classified entry. PLAN-ONLY: producing a
/// `RelocationItem` never moves or deletes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlannedAction {
    /// Nothing to do — healthy symlink alias.
    KeepAlias,
    /// Move the real file into its owning repo's `.tachi/` and (conceptually)
    /// replace the central path with a symlink. Gated behind `--apply`.
    RelocateToRepo,
    /// Keep as a home-resident named project (no move).
    KeepHomeResident,
    /// Garbage-collect: a broken symlink, or a UUID smoke-test directory.
    GarbageCollect,
}

impl PlannedAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlannedAction::KeepAlias => "keep-alias",
            PlannedAction::RelocateToRepo => "relocate-to-repo",
            PlannedAction::KeepHomeResident => "keep-home-resident",
            PlannedAction::GarbageCollect => "garbage-collect",
        }
    }
}

/// Pre-gathered facts about a single `~/.tachi/projects/<name>/memory.db`.
///
/// Keeping I/O out of the classifier means tests can synthesize any combination
/// (symlink target present/absent, owning repo found/not, uuid-shaped name).
#[derive(Debug, Clone)]
pub struct ProjectDbInput {
    /// The project directory name (the `<name>` segment).
    pub project_name: String,
    /// Absolute path to `~/.tachi/projects/<name>/memory.db`.
    pub db_path: PathBuf,
    /// `true` if `db_path` itself is a symlink.
    pub is_symlink: bool,
    /// If a symlink, the resolved target (whether or not it exists).
    pub symlink_target: Option<PathBuf>,
    /// If a symlink, whether the target path currently exists on disk.
    pub symlink_target_exists: bool,
    /// For a real (non-symlink) file: the discovered owning repo root, if any.
    /// Determined by the caller (manifest match / git-root scan).
    pub owning_repo: Option<PathBuf>,
}

/// A single line of the relocation plan.
#[derive(Debug, Clone, Serialize)]
pub struct RelocationItem {
    pub project_name: String,
    pub db_path: String,
    pub class: ProjectDbClass,
    pub action: PlannedAction,
    /// For `RelocateToRepo`: the proposed destination
    /// `<owning_repo>/.tachi/memory.db`. None otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relocate_to: Option<String>,
    /// For symlink aliases: the resolved target (informational).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    /// Human-readable rationale.
    pub note: String,
}

/// The full plan.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RelocationPlan {
    pub items: Vec<RelocationItem>,
    /// Count of each class, for the summary line.
    pub n_symlink_alias: usize,
    pub n_symlink_broken: usize,
    pub n_relocatable: usize,
    pub n_home_resident: usize,
    pub n_garbage: usize,
}

impl RelocationPlan {
    fn tally(&mut self, item: &RelocationItem) {
        match item.class {
            ProjectDbClass::SymlinkAlias => self.n_symlink_alias += 1,
            ProjectDbClass::SymlinkBroken => self.n_symlink_broken += 1,
            ProjectDbClass::RealFileWithOwningRepo => self.n_relocatable += 1,
            ProjectDbClass::RealFileHomeResident => self.n_home_resident += 1,
            ProjectDbClass::UuidSmokeTestGarbage => self.n_garbage += 1,
        }
    }
}

/// Returns `true` if a project directory name looks like a throwaway
/// UUID-shaped or smoke-test workspace rather than a real named project.
///
/// Matches:
///   * canonical 8-4-4-4-12 hex UUID (with or without surrounding noise)
///   * a 32-char (or longer) run of hex with no separators
///   * names containing common smoke/scratch markers (`smoke`, `tmp-`, `scratch-`)
///     combined with a long hex tail
pub fn is_uuid_smoke_test_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();

    // Canonical UUID anywhere in the name.
    if contains_canonical_uuid(&lower) {
        return true;
    }

    // Explicit smoke/scratch markers paired with a hex tail.
    let has_marker = lower.contains("smoke")
        || lower.contains("scratch")
        || lower.starts_with("tmp-")
        || lower.starts_with("tmp_")
        || lower.contains("-recall-smoke");
    if has_marker && longest_hex_run(&lower) >= 8 {
        return true;
    }

    // A bare long hex blob (>=32 contiguous hex chars) — e.g. a md5/uuid-no-dash.
    if longest_hex_run(&lower) >= 32 {
        return true;
    }

    false
}

fn contains_canonical_uuid(s: &str) -> bool {
    // Look for the 8-4-4-4-12 hex pattern. Hand-rolled to avoid a regex dep.
    let bytes = s.as_bytes();
    let n = bytes.len();
    let is_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    // Need 36 chars for the dashed form.
    if n < 36 {
        return false;
    }
    let groups = [8usize, 4, 4, 4, 12];
    for start in 0..=(n - 36) {
        let mut idx = start;
        let mut ok = true;
        for (gi, &glen) in groups.iter().enumerate() {
            for _ in 0..glen {
                if idx >= n || !is_hex(bytes[idx]) {
                    ok = false;
                    break;
                }
                idx += 1;
            }
            if !ok {
                break;
            }
            if gi < groups.len() - 1 {
                if idx >= n || bytes[idx] != b'-' {
                    ok = false;
                    break;
                }
                idx += 1;
            }
        }
        if ok {
            return true;
        }
    }
    false
}

fn longest_hex_run(s: &str) -> usize {
    let mut best = 0usize;
    let mut cur = 0usize;
    for b in s.bytes() {
        let is_hex = b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        if is_hex {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 0;
        }
    }
    best
}

/// PURE classification core. No I/O. Given pre-gathered facts, decide the class
/// and the planned action.
///
/// Precedence:
///   1. UUID/smoke-test name → garbage (even if it is a real file or symlink;
///      a throwaway workspace's data is not worth relocating).
///   2. Symlink → alias (target exists) or broken (target missing).
///   3. Real file + owning repo found → relocatable.
///   4. Real file, no owning repo → home-resident (keep, never move).
pub fn classify_project_db(input: &ProjectDbInput) -> RelocationItem {
    let db_path = input.db_path.to_string_lossy().to_string();

    // (1) UUID / smoke-test garbage takes precedence.
    if is_uuid_smoke_test_name(&input.project_name) {
        return RelocationItem {
            project_name: input.project_name.clone(),
            db_path,
            class: ProjectDbClass::UuidSmokeTestGarbage,
            action: PlannedAction::GarbageCollect,
            relocate_to: None,
            symlink_target: input
                .symlink_target
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            note: "project name looks like a UUID/smoke-test workspace — GC candidate".to_string(),
        };
    }

    // (2) Symlinks.
    if input.is_symlink {
        let target = input
            .symlink_target
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        if input.symlink_target_exists {
            return RelocationItem {
                project_name: input.project_name.clone(),
                db_path,
                class: ProjectDbClass::SymlinkAlias,
                action: PlannedAction::KeepAlias,
                relocate_to: None,
                symlink_target: target,
                note: "healthy symlink alias into repo-local DB — keep".to_string(),
            };
        }
        return RelocationItem {
            project_name: input.project_name.clone(),
            db_path,
            class: ProjectDbClass::SymlinkBroken,
            action: PlannedAction::GarbageCollect,
            relocate_to: None,
            symlink_target: target,
            note: "dangling symlink (target missing) — GC the link only".to_string(),
        };
    }

    // (3) Real file with a discoverable owning repo → relocatable.
    if let Some(repo) = &input.owning_repo {
        let dest = repo.join(".tachi").join("memory.db");
        return RelocationItem {
            project_name: input.project_name.clone(),
            db_path,
            class: ProjectDbClass::RealFileWithOwningRepo,
            action: PlannedAction::RelocateToRepo,
            relocate_to: Some(dest.to_string_lossy().to_string()),
            symlink_target: None,
            note: format!(
                "real per-project DB; owning repo {} — could relocate into <repo>/.tachi/",
                repo.display()
            ),
        };
    }

    // (4) Real file, no owning repo → keep as home-resident named project.
    RelocationItem {
        project_name: input.project_name.clone(),
        db_path,
        class: ProjectDbClass::RealFileHomeResident,
        action: PlannedAction::KeepHomeResident,
        relocate_to: None,
        symlink_target: None,
        note: "real DB with no owning repo — keep as home-resident named project".to_string(),
    }
}

/// Resolve the owning repo for a real, home-resident project DB.
///
/// Strategy (no mutation):
///   1. If a manifest entry's `scope_hint` is `project:<name>` AND its recorded
///      `path` lives under a repo `.tachi/` (not under `~/.tachi/projects/`),
///      use that repo root.
///   2. Else, if `<git_roots>` contains a repo whose basename equals `<name>`
///      (case-insensitive), use it.
///
/// Returns the repo root (the directory that should contain `.tachi/memory.db`).
pub fn resolve_owning_repo(
    project_name: &str,
    manifest_project_paths: &[(String, String)], // (scope_hint, path)
    candidate_git_roots: &[PathBuf],
) -> Option<PathBuf> {
    let scope = format!("project:{project_name}");
    // (1) Manifest-recorded repo-local path for this project name.
    for (hint, path) in manifest_project_paths {
        if hint != &scope {
            continue;
        }
        let norm = path.replace('\\', "/");
        // Must be a repo-local `.tachi/memory.db`, NOT the central
        // `~/.tachi/projects/<name>/memory.db` we are auditing.
        if norm.contains("/.tachi/projects/") {
            continue;
        }
        if let Some(idx) = norm.rfind("/.tachi/") {
            let repo = &norm[..idx];
            if !repo.is_empty() {
                return Some(PathBuf::from(repo));
            }
        }
    }
    // (2) Git-root basename match.
    let want = project_name.to_ascii_lowercase();
    for root in candidate_git_roots {
        if let Some(base) = root.file_name().and_then(|s| s.to_str()) {
            if base.to_ascii_lowercase() == want {
                return Some(root.clone());
            }
        }
    }
    None
}

/// Build a [`RelocationPlan`] from a set of pre-classified inputs. Pure.
pub fn build_plan(inputs: &[ProjectDbInput]) -> RelocationPlan {
    let mut plan = RelocationPlan::default();
    for inp in inputs {
        let item = classify_project_db(inp);
        plan.tally(&item);
        plan.items.push(item);
    }
    plan
}

/// Enumerate `~/.tachi/projects/*/memory.db`, gather filesystem facts, resolve
/// owning repos from the manifest + candidate git roots, and return the inputs.
///
/// This is the I/O shell; it performs only reads (`symlink_metadata`,
/// `read_link`, `exists`). It NEVER moves or deletes anything.
pub fn gather_project_inputs(
    projects_dir: &Path,
    manifest_project_paths: &[(String, String)],
    candidate_git_roots: &[PathBuf],
) -> std::io::Result<Vec<ProjectDbInput>> {
    let mut out = Vec::new();
    if !projects_dir.exists() {
        return Ok(out);
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(projects_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for proj_dir in entries {
        if !proj_dir.is_dir() {
            continue;
        }
        let project_name = match proj_dir.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let db_path = proj_dir.join("memory.db");
        // `symlink_metadata` does NOT follow the final symlink, so we can tell a
        // link apart from a real file.
        let meta = match std::fs::symlink_metadata(&db_path) {
            Ok(m) => m,
            Err(_) => continue, // no memory.db in this project dir
        };
        let is_symlink = meta.file_type().is_symlink();
        let (symlink_target, symlink_target_exists) = if is_symlink {
            match std::fs::read_link(&db_path) {
                Ok(t) => {
                    // Resolve relative links against the project dir.
                    let resolved = if t.is_absolute() {
                        t.clone()
                    } else {
                        proj_dir.join(&t)
                    };
                    let exists = resolved.exists();
                    (Some(resolved), exists)
                }
                Err(_) => (None, false),
            }
        } else {
            (None, false)
        };
        // Only resolve an owning repo for real files (the only relocatable case).
        let owning_repo = if !is_symlink {
            resolve_owning_repo(&project_name, manifest_project_paths, candidate_git_roots)
        } else {
            None
        };
        out.push(ProjectDbInput {
            project_name,
            db_path,
            is_symlink,
            symlink_target,
            symlink_target_exists,
            owning_repo,
        });
    }
    Ok(out)
}

/// Render the plan as human-readable text.
pub fn render_plan(plan: &RelocationPlan, projects_dir: &Path) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "project-DB relocation plan (DRY-RUN)  dir={}  entries={}",
        projects_dir.display(),
        plan.items.len()
    );
    let _ = writeln!(
        out,
        "  symlink-alias={}  symlink-broken={}  relocatable={}  home-resident={}  garbage={}",
        plan.n_symlink_alias,
        plan.n_symlink_broken,
        plan.n_relocatable,
        plan.n_home_resident,
        plan.n_garbage,
    );
    for item in &plan.items {
        let _ = writeln!(
            out,
            "  [{}] {}  action={}",
            item.class.as_str(),
            item.project_name,
            item.action.as_str(),
        );
        let _ = writeln!(out, "      path: {}", item.db_path);
        if let Some(t) = &item.symlink_target {
            let _ = writeln!(out, "      -> target: {t}");
        }
        if let Some(dest) = &item.relocate_to {
            let _ = writeln!(out, "      => relocate to: {dest}");
        }
        let _ = writeln!(out, "      note: {}", item.note);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str) -> ProjectDbInput {
        ProjectDbInput {
            project_name: name.to_string(),
            db_path: PathBuf::from(format!("/u/.tachi/projects/{name}/memory.db")),
            is_symlink: false,
            symlink_target: None,
            symlink_target_exists: false,
            owning_repo: None,
        }
    }

    #[test]
    fn symlink_with_live_target_is_alias() {
        let mut i = input("hyperion");
        i.is_symlink = true;
        i.symlink_target = Some(PathBuf::from("/repos/hyperion/.tachi/memory.db"));
        i.symlink_target_exists = true;
        let item = classify_project_db(&i);
        assert_eq!(item.class, ProjectDbClass::SymlinkAlias);
        assert_eq!(item.action, PlannedAction::KeepAlias);
        assert!(item.relocate_to.is_none());
    }

    #[test]
    fn symlink_with_missing_target_is_broken_gc() {
        let mut i = input("stale-proj");
        i.is_symlink = true;
        i.symlink_target = Some(PathBuf::from("/gone/.tachi/memory.db"));
        i.symlink_target_exists = false;
        let item = classify_project_db(&i);
        assert_eq!(item.class, ProjectDbClass::SymlinkBroken);
        assert_eq!(item.action, PlannedAction::GarbageCollect);
    }

    #[test]
    fn real_file_with_owning_repo_is_relocatable() {
        let mut i = input("quant");
        i.owning_repo = Some(PathBuf::from("/Users/me/repos/quant"));
        let item = classify_project_db(&i);
        assert_eq!(item.class, ProjectDbClass::RealFileWithOwningRepo);
        assert_eq!(item.action, PlannedAction::RelocateToRepo);
        assert_eq!(
            item.relocate_to.as_deref(),
            Some("/Users/me/repos/quant/.tachi/memory.db")
        );
    }

    #[test]
    fn real_file_without_repo_is_home_resident() {
        let i = input("Quant_Analyzer_2026_audit_notes");
        let item = classify_project_db(&i);
        assert_eq!(item.class, ProjectDbClass::RealFileHomeResident);
        assert_eq!(item.action, PlannedAction::KeepHomeResident);
        assert!(item.relocate_to.is_none());
    }

    #[test]
    fn uuid_name_is_garbage_even_as_real_file_with_repo() {
        // A UUID-named dir that *happens* to have an owning repo and real data
        // must still be classified garbage (precedence rule 1).
        let mut i = input("8f14e45f-ceea-467a-9c8a-1b2c3d4e5f60");
        i.owning_repo = Some(PathBuf::from("/repos/whatever"));
        let item = classify_project_db(&i);
        assert_eq!(item.class, ProjectDbClass::UuidSmokeTestGarbage);
        assert_eq!(item.action, PlannedAction::GarbageCollect);
    }

    #[test]
    fn uuid_detection_matches_expected_shapes() {
        // Canonical dashed UUID.
        assert!(is_uuid_smoke_test_name(
            "8f14e45f-ceea-467a-9c8a-1b2c3d4e5f60"
        ));
        // UUID embedded with a prefix.
        assert!(is_uuid_smoke_test_name(
            "run-8f14e45f-ceea-467a-9c8a-1b2c3d4e5f60"
        ));
        // 32-char hex blob (no dashes).
        assert!(is_uuid_smoke_test_name("9b74c9897bac770ffc029102a200c5de"));
        // smoke marker + hex tail.
        assert!(is_uuid_smoke_test_name("tachi-recall-smoke.ab12cd34"));
        // Real named projects must NOT match.
        assert!(!is_uuid_smoke_test_name("quant"));
        assert!(!is_uuid_smoke_test_name("hyperion-research"));
        assert!(!is_uuid_smoke_test_name("Quant_Analyzer_2026_audit_main"));
        // "facade" contains hex chars a,c,e but only 6-long → not a blob.
        assert!(!is_uuid_smoke_test_name("facade"));
        // A short hex-ish word like "decade" (6) must not trip the 32-run rule.
        assert!(!is_uuid_smoke_test_name("decade"));
    }

    #[test]
    fn resolve_owning_repo_prefers_manifest_repo_local_path() {
        let manifest = vec![
            // Central path for this project — must be IGNORED as an owner.
            (
                "project:quant".to_string(),
                "/u/.tachi/projects/quant/memory.db".to_string(),
            ),
            // Repo-local path — this is the real owner.
            (
                "project:quant".to_string(),
                "/Users/me/repos/quant/.tachi/memory.db".to_string(),
            ),
        ];
        let repo = resolve_owning_repo("quant", &manifest, &[]);
        assert_eq!(repo, Some(PathBuf::from("/Users/me/repos/quant")));
    }

    #[test]
    fn resolve_owning_repo_falls_back_to_git_root_basename() {
        let roots = vec![
            PathBuf::from("/Users/me/repos/other"),
            PathBuf::from("/Users/me/repos/Hyperion"),
        ];
        // Case-insensitive basename match.
        let repo = resolve_owning_repo("hyperion", &[], &roots);
        assert_eq!(repo, Some(PathBuf::from("/Users/me/repos/Hyperion")));
        // No match → None (becomes home-resident).
        assert_eq!(resolve_owning_repo("nope", &[], &roots), None);
    }

    #[test]
    fn resolve_owning_repo_ignores_central_only_manifest_entry() {
        // If the ONLY manifest path for the project is the central one, there is
        // no repo-local owner → None.
        let manifest = vec![(
            "project:lonely".to_string(),
            "/u/.tachi/projects/lonely/memory.db".to_string(),
        )];
        assert_eq!(resolve_owning_repo("lonely", &manifest, &[]), None);
    }

    #[test]
    fn build_plan_tallies_each_class() {
        let mut alias = input("alias-proj");
        alias.is_symlink = true;
        alias.symlink_target = Some(PathBuf::from("/r/.tachi/memory.db"));
        alias.symlink_target_exists = true;

        let mut broken = input("broken-proj");
        broken.is_symlink = true;
        broken.symlink_target_exists = false;

        let mut reloc = input("reloc-proj");
        reloc.owning_repo = Some(PathBuf::from("/repos/reloc-proj"));

        let home = input("home-proj");

        let garbage = input("11111111-2222-3333-4444-555555555555");

        let plan = build_plan(&[alias, broken, reloc, home, garbage]);
        assert_eq!(plan.items.len(), 5);
        assert_eq!(plan.n_symlink_alias, 1);
        assert_eq!(plan.n_symlink_broken, 1);
        assert_eq!(plan.n_relocatable, 1);
        assert_eq!(plan.n_home_resident, 1);
        assert_eq!(plan.n_garbage, 1);
    }

    #[cfg(unix)]
    #[test]
    fn gather_project_inputs_reads_symlinks_and_real_files() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");

        // (a) symlink-alias: real target exists.
        let repo = dir.path().join("repo-a/.tachi");
        std::fs::create_dir_all(&repo).unwrap();
        let real_target = repo.join("memory.db");
        std::fs::write(&real_target, b"db").unwrap();
        let alias_dir = projects.join("alias-proj");
        std::fs::create_dir_all(&alias_dir).unwrap();
        symlink(&real_target, alias_dir.join("memory.db")).unwrap();

        // (b) broken symlink.
        let broken_dir = projects.join("broken-proj");
        std::fs::create_dir_all(&broken_dir).unwrap();
        symlink(
            dir.path().join("does-not-exist.db"),
            broken_dir.join("memory.db"),
        )
        .unwrap();

        // (c) real home-resident file.
        let home_dir = projects.join("home-proj");
        std::fs::create_dir_all(&home_dir).unwrap();
        std::fs::write(home_dir.join("memory.db"), b"db").unwrap();

        // (d) uuid garbage dir with a real file.
        let uuid_dir = projects.join("8f14e45f-ceea-467a-9c8a-1b2c3d4e5f60");
        std::fs::create_dir_all(&uuid_dir).unwrap();
        std::fs::write(uuid_dir.join("memory.db"), b"db").unwrap();

        let inputs = gather_project_inputs(&projects, &[], &[]).unwrap();
        let plan = build_plan(&inputs);

        // 4 entries gathered.
        assert_eq!(plan.items.len(), 4);
        assert_eq!(plan.n_symlink_alias, 1);
        assert_eq!(plan.n_symlink_broken, 1);
        assert_eq!(plan.n_home_resident, 1);
        assert_eq!(plan.n_garbage, 1);
        // home-proj must NOT have been relocated (no owning repo passed).
        let home = plan
            .items
            .iter()
            .find(|i| i.project_name == "home-proj")
            .unwrap();
        assert_eq!(home.action, PlannedAction::KeepHomeResident);
    }

    #[test]
    fn gather_returns_empty_when_dir_missing() {
        let got = gather_project_inputs(Path::new("/no/such/dir/xyz"), &[], &[]).unwrap();
        assert!(got.is_empty());
    }
}
