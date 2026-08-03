use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

const INSTRUCTION_MANIFEST_SCHEMA: &str = "tachi.instruction_surfaces.v1";
const CLEAN_STATUS: &str = "clean";
const MISSING_STATUS: &str = "missing";
const NON_CLEAN_STATUS: &str = "non_clean";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Audience {
    Public,
    CarrierPrivate,
}

impl Audience {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::CarrierPrivate => "carrier-private",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Tier {
    CompressedAdapter,
    ExpandedManual,
}

impl Tier {
    fn as_str(&self) -> &'static str {
        match self {
            Self::CompressedAdapter => "compressed-adapter",
            Self::ExpandedManual => "expanded-manual",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
enum Carrier {
    Codex,
    Claude,
    Gemini,
    Antigravity,
    Cursor,
}

impl Carrier {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Cursor => "cursor",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum OwnershipMode {
    SourceOwned,
    CarrierOwned,
}

impl OwnershipMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::SourceOwned => "source-owned",
            Self::CarrierOwned => "carrier-owned",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct DensityBudget {
    pub(super) name: String,
    pub(super) bytes: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstructionTargetDeclaration {
    carrier: Carrier,
    path: String,
    ownership_mode: OwnershipMode,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstructionSurfaceDeclaration {
    id: String,
    source: String,
    audience: Audience,
    tier: Tier,
    density_budget: DensityBudget,
    remediation_owner: String,
    targets: Vec<InstructionTargetDeclaration>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstructionManifestDocument {
    schema_version: String,
    root: String,
    surfaces: Vec<InstructionSurfaceDeclaration>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct InstructionManifestStatus {
    pub(super) schema_version: String,
    pub(super) manifest_path: String,
    pub(super) root: String,
    pub(super) manifest_hash: String,
    pub(super) status: String,
    pub(super) sources: Vec<InstructionSourceStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct InstructionSourceStatus {
    pub(super) id: String,
    pub(super) source: String,
    pub(super) resolved_path: String,
    pub(super) audience: String,
    pub(super) tier: String,
    pub(super) density_budget: DensityBudget,
    pub(super) remediation_owner: String,
    pub(super) exists: bool,
    pub(super) status: String,
    pub(super) hash: Option<String>,
    pub(super) bytes: Option<u64>,
    pub(super) targets: Vec<InstructionTargetStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(super) struct InstructionTargetStatus {
    pub(super) carrier: String,
    pub(super) path: String,
    pub(super) resolved_path: String,
    pub(super) ownership_mode: String,
    pub(super) exists: bool,
    pub(super) status: String,
    pub(super) hash: Option<String>,
    pub(super) bytes: Option<u64>,
}

#[derive(Debug, Clone)]
struct DeclaredFileStatus {
    resolved_path: PathBuf,
    exists: bool,
    status: &'static str,
    hash: Option<String>,
    bytes: Option<u64>,
}

/// Parse and read exactly the files declared by an instruction-surface manifest.
///
/// This is deliberately a status-only seam: it never crawls from `root`, checks
/// density policy, renders a target, or writes a receipt or source file.
pub(super) fn scan_instruction_manifest(
    manifest_path: &Path,
) -> Result<InstructionManifestStatus, String> {
    let canonical_manifest_path = manifest_path.canonicalize().map_err(|error| {
        format!(
            "read instruction manifest '{}': {error}",
            manifest_path.display()
        )
    })?;
    let manifest_metadata = fs::metadata(&canonical_manifest_path).map_err(|error| {
        format!(
            "inspect instruction manifest '{}': {error}",
            canonical_manifest_path.display()
        )
    })?;
    if !manifest_metadata.is_file() {
        let kind = if manifest_metadata.is_dir() {
            "directory"
        } else {
            "non-regular file"
        };
        return Err(format!(
            "read instruction manifest '{}': resolved path is a {kind}",
            canonical_manifest_path.display()
        ));
    }
    let manifest_text = fs::read_to_string(&canonical_manifest_path).map_err(|error| {
        format!(
            "read instruction manifest '{}': {error}",
            canonical_manifest_path.display()
        )
    })?;
    let mut manifest: InstructionManifestDocument =
        serde_json::from_str(&manifest_text).map_err(|error| {
            format!(
                "parse instruction manifest '{}': {error}",
                canonical_manifest_path.display()
            )
        })?;
    validate_manifest(&manifest)?;

    let manifest_parent = canonical_manifest_path.parent().ok_or_else(|| {
        format!(
            "instruction manifest '{}' has no parent directory",
            canonical_manifest_path.display()
        )
    })?;
    let root_candidate = manifest_parent.join(&manifest.root);
    let root = root_candidate.canonicalize().map_err(|error| {
        format!(
            "resolve instruction manifest root '{}' relative to '{}': {error}",
            manifest.root,
            canonical_manifest_path.display()
        )
    })?;
    if !root.is_dir() {
        return Err(format!(
            "instruction manifest root '{}' is not a directory",
            root.display()
        ));
    }

    manifest
        .surfaces
        .sort_by(|left, right| left.id.cmp(&right.id));
    for surface in &mut manifest.surfaces {
        surface.targets.sort_by(|left, right| {
            left.carrier
                .as_str()
                .cmp(right.carrier.as_str())
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| {
                    left.ownership_mode
                        .as_str()
                        .cmp(right.ownership_mode.as_str())
                })
        });
    }
    let canonical_manifest = serde_json::to_string(&manifest)
        .map_err(|error| format!("serialize instruction manifest for hashing: {error}"))?;

    let mut sources = Vec::with_capacity(manifest.surfaces.len());
    let mut target_owners = BTreeMap::new();
    for surface in manifest.surfaces {
        let source_status =
            inspect_declared_file(&root, &surface.source, &format!("source '{}'", surface.id))?;
        let mut targets = Vec::with_capacity(surface.targets.len());
        for target in surface.targets {
            let target_status = inspect_declared_file(
                &root,
                &target.path,
                &format!(
                    "target '{}' for source '{}'",
                    target.carrier.as_str(),
                    surface.id
                ),
            )?;
            let resolved_target = target_status.resolved_path.clone();
            if let Some((previous_surface, previous_declared_path)) = target_owners.insert(
                resolved_target.clone(),
                (surface.id.clone(), target.path.clone()),
            ) {
                return Err(format!(
                    "duplicate resolved target '{}' declared by surfaces '{}' (path '{}') and '{}' (path '{}')",
                    resolved_target.display(),
                    previous_surface,
                    previous_declared_path,
                    surface.id,
                    target.path
                ));
            }
            targets.push(InstructionTargetStatus {
                carrier: target.carrier.as_str().to_string(),
                path: target.path,
                resolved_path: target_status.resolved_path.display().to_string(),
                ownership_mode: target.ownership_mode.as_str().to_string(),
                exists: target_status.exists,
                status: target_status.status.to_string(),
                hash: target_status.hash,
                bytes: target_status.bytes,
            });
        }

        sources.push(InstructionSourceStatus {
            id: surface.id,
            source: surface.source,
            resolved_path: source_status.resolved_path.display().to_string(),
            audience: surface.audience.as_str().to_string(),
            tier: surface.tier.as_str().to_string(),
            density_budget: surface.density_budget,
            remediation_owner: surface.remediation_owner,
            exists: source_status.exists,
            status: source_status.status.to_string(),
            hash: source_status.hash,
            bytes: source_status.bytes,
            targets,
        });
    }

    let all_clean = sources.iter().all(|source| {
        source.status == CLEAN_STATUS
            && source
                .targets
                .iter()
                .all(|target| target.status == CLEAN_STATUS)
    });

    Ok(InstructionManifestStatus {
        schema_version: INSTRUCTION_MANIFEST_SCHEMA.to_string(),
        manifest_path: canonical_manifest_path.display().to_string(),
        root: root.display().to_string(),
        manifest_hash: crate::utils::stable_hash(&canonical_manifest),
        status: if all_clean {
            CLEAN_STATUS.to_string()
        } else {
            NON_CLEAN_STATUS.to_string()
        },
        sources,
    })
}

fn validate_manifest(manifest: &InstructionManifestDocument) -> Result<(), String> {
    if manifest.schema_version != INSTRUCTION_MANIFEST_SCHEMA {
        return Err(format!(
            "unsupported instruction manifest schema '{}'; expected '{}'",
            manifest.schema_version, INSTRUCTION_MANIFEST_SCHEMA
        ));
    }
    if manifest.root.trim().is_empty() {
        return Err("instruction manifest root must not be empty".to_string());
    }
    if Path::new(&manifest.root).is_absolute() {
        return Err("instruction manifest root must be relative to the manifest".to_string());
    }

    let mut ids = BTreeSet::new();
    for surface in &manifest.surfaces {
        if surface.id.trim().is_empty() {
            return Err("instruction surface id must not be empty".to_string());
        }
        if !ids.insert(surface.id.clone()) {
            return Err(format!("duplicate surface id '{}'", surface.id));
        }
        if surface.source.trim().is_empty() {
            return Err(format!(
                "source path for surface '{}' must not be empty",
                surface.id
            ));
        }
        if surface.density_budget.name.trim().is_empty() {
            return Err(format!(
                "density budget name for surface '{}' must not be empty",
                surface.id
            ));
        }
        if surface.remediation_owner.trim().is_empty() {
            return Err(format!(
                "remediation owner for surface '{}' must not be empty",
                surface.id
            ));
        }

        let mut targets = BTreeSet::new();
        for target in &surface.targets {
            let key = format!(
                "{}\u{1f}{}\u{1f}{}",
                target.carrier.as_str(),
                target.path,
                target.ownership_mode.as_str()
            );
            if !targets.insert(key) {
                return Err(format!(
                    "duplicate target '{}' for surface '{}'",
                    target.path, surface.id
                ));
            }
            if target.path.trim().is_empty() {
                return Err(format!(
                    "target path for surface '{}' must not be empty",
                    surface.id
                ));
            }
        }
    }
    Ok(())
}

fn inspect_declared_file(
    root: &Path,
    declared_path: &str,
    label: &str,
) -> Result<DeclaredFileStatus, String> {
    let lexical_path = resolve_declared_path(root, declared_path, label)?;
    ensure_existing_symlinks_within_root(root, &lexical_path, label)?;
    match fs::symlink_metadata(&lexical_path) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let resolved_path = resolve_missing_declared_path(root, &lexical_path, label)?;
            return Ok(DeclaredFileStatus {
                resolved_path,
                exists: false,
                status: MISSING_STATUS,
                hash: None,
                bytes: None,
            });
        }
        Err(error) => {
            return Err(format!(
                "inspect declared {label} '{}': {error}",
                lexical_path.display()
            ));
        }
    }

    let canonical_path = lexical_path.canonicalize().map_err(|error| {
        format!(
            "resolve declared {label} '{}': {error}",
            lexical_path.display()
        )
    })?;
    if !canonical_path.starts_with(root) {
        return Err(format!(
            "declared {label} '{}' resolves outside declared root '{}'",
            declared_path,
            root.display()
        ));
    }

    let metadata = fs::metadata(&canonical_path).map_err(|error| {
        format!(
            "inspect declared {label} '{}': {error}",
            canonical_path.display()
        )
    })?;
    if !metadata.is_file() {
        let kind = if metadata.is_dir() {
            "directory"
        } else {
            "non-regular file"
        };
        return Err(format!(
            "read declared {label} '{}': resolved path is a {kind}",
            canonical_path.display()
        ));
    }

    let content = fs::read(&canonical_path).map_err(|error| {
        format!(
            "read declared {label} '{}': {error}",
            canonical_path.display()
        )
    })?;
    let text = std::str::from_utf8(&content).map_err(|error| {
        format!(
            "read declared {label} '{}': invalid UTF-8 ({error})",
            canonical_path.display()
        )
    })?;

    Ok(DeclaredFileStatus {
        resolved_path: canonical_path,
        exists: true,
        status: CLEAN_STATUS,
        hash: Some(crate::utils::stable_hash(text)),
        bytes: Some(content.len() as u64),
    })
}

fn resolve_missing_declared_path(
    root: &Path,
    lexical_path: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let mut existing_ancestor = lexical_path.to_path_buf();
    let mut missing_suffix = Vec::new();

    let canonical_ancestor = loop {
        match fs::symlink_metadata(&existing_ancestor) {
            Ok(_) => {
                let canonical = existing_ancestor.canonicalize().map_err(|error| {
                    format!(
                        "resolve existing ancestor for missing declared {label} '{}': {error}",
                        existing_ancestor.display()
                    )
                })?;
                break canonical;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let component = existing_ancestor.file_name().ok_or_else(|| {
                    format!(
                        "resolve missing declared {label} '{}': no existing ancestor",
                        lexical_path.display()
                    )
                })?;
                missing_suffix.push(component.to_os_string());
                if !existing_ancestor.pop() {
                    return Err(format!(
                        "resolve missing declared {label} '{}': no existing ancestor",
                        lexical_path.display()
                    ));
                }
            }
            Err(error) => {
                return Err(format!(
                    "inspect existing ancestor for missing declared {label} '{}': {error}",
                    existing_ancestor.display()
                ));
            }
        }
    };

    if !canonical_ancestor.starts_with(root) {
        return Err(format!(
            "declared {label} path '{}' resolves outside declared root '{}'",
            lexical_path.display(),
            root.display()
        ));
    }

    let mut resolved = canonical_ancestor;
    for component in missing_suffix.iter().rev() {
        resolved.push(component);
    }
    if !resolved.starts_with(root) {
        return Err(format!(
            "declared {label} path '{}' resolves outside declared root '{}'",
            lexical_path.display(),
            root.display()
        ));
    }
    Ok(resolved)
}

fn ensure_existing_symlinks_within_root(
    root: &Path,
    lexical_path: &Path,
    label: &str,
) -> Result<(), String> {
    let relative = lexical_path.strip_prefix(root).map_err(|_| {
        format!(
            "declared {label} path '{}' is outside declared root '{}'",
            lexical_path.display(),
            root.display()
        )
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let canonical = current.canonicalize().map_err(|error| {
                    format!(
                        "resolve symlink in declared {label} path '{}': {error}",
                        current.display()
                    )
                })?;
                if !canonical.starts_with(root) {
                    return Err(format!(
                        "declared {label} path '{}' resolves outside declared root '{}'",
                        lexical_path.display(),
                        root.display()
                    ));
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "inspect symlink in declared {label} path '{}': {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}

fn resolve_declared_path(root: &Path, declared_path: &str, label: &str) -> Result<PathBuf, String> {
    let path = Path::new(declared_path);
    if declared_path.trim().is_empty() {
        return Err(format!("declared {label} path must not be empty"));
    }
    if path.is_absolute() {
        return Err(format!(
            "declared {label} path '{}' is absolute; only paths inside the declared root are allowed",
            declared_path
        ));
    }

    let mut resolved = root.to_path_buf();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => resolved.push(part),
            Component::ParentDir => {
                resolved.pop();
                if !resolved.starts_with(root) {
                    return Err(format!(
                        "declared {label} path '{}' escapes outside declared root '{}'",
                        declared_path,
                        root.display()
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "declared {label} path '{}' is absolute; only paths inside the declared root are allowed",
                    declared_path
                ));
            }
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::scan_instruction_manifest;
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let root = crate::utils::test_fixture_path(format!(
            "instruction-manifest-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".agents")).expect("fixture root");
        (root.clone(), root.join(".agents/instruction-surfaces.json"))
    }

    fn base_manifest() -> Value {
        json!({
            "schema_version": "tachi.instruction_surfaces.v1",
            "root": "..",
            "surfaces": [
                {
                    "id": "z-private",
                    "source": "CLAUDE.md",
                    "audience": "carrier-private",
                    "tier": "expanded-manual",
                    "density_budget": {"name": "provisional-expanded", "bytes": 4096},
                    "remediation_owner": "carrier-manual-owner",
                    "targets": [
                        {"carrier": "claude", "path": "targets/claude/CLAUDE.md", "ownership_mode": "carrier-owned"},
                        {"carrier": "codex", "path": "targets/codex/private.md", "ownership_mode": "source-owned"}
                    ]
                },
                {
                    "id": "a-public",
                    "source": "AGENTS.md",
                    "audience": "public",
                    "tier": "compressed-adapter",
                    "density_budget": {"name": "provisional-compressed", "bytes": 2048},
                    "remediation_owner": "repository-owner",
                    "targets": [
                        {"carrier": "cursor", "path": "targets/cursor/AGENTS.md", "ownership_mode": "source-owned"}
                    ]
                }
            ]
        })
    }

    fn write_manifest(path: &Path, value: &Value) {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(value).expect("manifest JSON"),
        )
        .expect("manifest file");
    }

    #[test]
    fn valid_scan_is_sorted_and_repeatable_without_timestamp_identity() {
        let (root, manifest_path) = fixture("stable");
        std::fs::write(root.join("AGENTS.md"), "public\n").expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private\n").expect("source");
        std::fs::create_dir_all(root.join("targets/claude")).expect("target dir");
        std::fs::create_dir_all(root.join("targets/codex")).expect("target dir");
        std::fs::create_dir_all(root.join("targets/cursor")).expect("target dir");
        std::fs::write(root.join("targets/claude/CLAUDE.md"), "private\n").expect("target");
        std::fs::write(root.join("targets/codex/private.md"), "private target\n").expect("target");
        std::fs::write(root.join("targets/cursor/AGENTS.md"), "public target\n").expect("target");
        write_manifest(&manifest_path, &base_manifest());

        let first = scan_instruction_manifest(&manifest_path).expect("valid manifest");
        let second = scan_instruction_manifest(&manifest_path).expect("repeat scan");
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );

        let value = serde_json::to_value(&first).unwrap();
        assert_eq!(value["status"], "clean");
        assert_eq!(value["sources"][0]["id"], "a-public");
        assert_eq!(value["sources"][1]["id"], "z-private");
        assert_eq!(value["sources"][1]["targets"][0]["carrier"], "claude");
        assert_eq!(value["sources"][1]["targets"][1]["carrier"], "codex");
        assert!(value["manifest_hash"].as_str().is_some());
        assert!(value["sources"][0]["hash"].as_str().is_some());
        assert!(value["sources"][0]["bytes"].as_u64().is_some());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_declared_paths_are_explicit_non_clean_statuses() {
        let (root, manifest_path) = fixture("missing");
        write_manifest(&manifest_path, &base_manifest());
        let before = std::fs::read(&manifest_path).expect("manifest before scan");

        let report =
            scan_instruction_manifest(&manifest_path).expect("missing files are reportable");
        let after = std::fs::read(&manifest_path).expect("manifest after scan");
        assert_eq!(before, after);

        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["status"], "non_clean");
        assert_eq!(value["sources"][0]["status"], "missing");
        assert_eq!(value["sources"][0]["exists"], false);
        assert_eq!(value["sources"][1]["targets"][0]["status"], "missing");
        assert_eq!(value["sources"][1]["targets"][0]["exists"], false);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_duplicate_unknown_and_escape_declarations_fail_loudly() {
        let (root, manifest_path) = fixture("invalid");
        let mut manifest = base_manifest();

        manifest["surfaces"][0]["id"] = json!("a-public");
        write_manifest(&manifest_path, &manifest);
        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        assert!(error.contains("duplicate surface id"), "{error}");

        for field in ["schema", "schema_id"] {
            manifest = base_manifest();
            manifest[field] = json!("tachi.instruction_surfaces.v1");
            write_manifest(&manifest_path, &manifest);
            let error = scan_instruction_manifest(&manifest_path).unwrap_err();
            assert!(
                error.contains("unknown field") && error.contains(field),
                "{error}"
            );
        }

        manifest = base_manifest();
        manifest["surfaces"][0]["audience"] = json!("unknown");
        write_manifest(&manifest_path, &manifest);
        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        assert!(error.contains("unknown variant `unknown`"), "{error}");

        manifest = base_manifest();
        manifest["surfaces"][0]["targets"][0]["ownership_mode"] = json!("unknown");
        write_manifest(&manifest_path, &manifest);
        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        assert!(
            error.contains("expected `source-owned` or `carrier-owned`"),
            "{error}"
        );

        manifest = base_manifest();
        manifest["surfaces"][0]["source"] = json!("/etc/passwd");
        write_manifest(&manifest_path, &manifest);
        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        assert!(error.contains("absolute"), "{error}");

        manifest = base_manifest();
        manifest["surfaces"][0]["source"] = json!("../outside.md");
        write_manifest(&manifest_path, &manifest);
        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        assert!(error.contains("outside declared root"), "{error}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn duplicate_targets_are_rejected_after_global_path_resolution() {
        let (root, manifest_path) = fixture("duplicate-target");
        std::fs::write(root.join("AGENTS.md"), "public\n").expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private\n").expect("source");
        std::fs::create_dir_all(root.join("targets/a")).expect("alias target dir");
        std::fs::create_dir_all(root.join("targets/codex")).expect("target dir");
        let target = root.join("targets/codex/AGENTS.md");
        std::fs::write(&target, "shared target\n").expect("target");

        let mut manifest = base_manifest();
        manifest["surfaces"][0]["targets"][0]["path"] = json!("targets/codex/AGENTS.md");
        manifest["surfaces"][1]["targets"][0]["path"] = json!("targets/a/../codex/AGENTS.md");
        write_manifest(&manifest_path, &manifest);

        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        let resolved_target = target.canonicalize().expect("resolved target");
        assert!(error.contains("duplicate resolved target"), "{error}");
        assert!(
            error.contains("a-public") && error.contains("z-private"),
            "{error}"
        );
        assert!(
            error.contains(resolved_target.to_string_lossy().as_ref()),
            "{error}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_alias_targets_are_rejected_when_they_resolve_inside_root() {
        let (root, manifest_path) = fixture("duplicate-symlink-target");
        std::fs::write(root.join("AGENTS.md"), "public\n").expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private\n").expect("source");
        std::fs::create_dir_all(root.join("targets/a")).expect("alias target dir");
        std::fs::create_dir_all(root.join("targets/codex")).expect("target dir");
        let target = root.join("targets/codex/AGENTS.md");
        std::fs::write(&target, "shared target\n").expect("target");
        std::os::unix::fs::symlink("../codex/AGENTS.md", root.join("targets/a/AGENTS.md"))
            .expect("target alias");

        let mut manifest = base_manifest();
        manifest["surfaces"][0]["targets"][0]["path"] = json!("targets/codex/AGENTS.md");
        manifest["surfaces"][1]["targets"][0]["path"] = json!("targets/a/AGENTS.md");
        write_manifest(&manifest_path, &manifest);

        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        let resolved_target = target.canonicalize().expect("resolved target");
        assert!(error.contains("duplicate resolved target"), "{error}");
        assert!(
            error.contains("a-public") && error.contains("z-private"),
            "{error}"
        );
        assert!(
            error.contains(resolved_target.to_string_lossy().as_ref()),
            "{error}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn missing_targets_are_rejected_after_resolving_existing_symlink_ancestors() {
        let (root, manifest_path) = fixture("duplicate-missing-symlink-target");
        std::fs::write(root.join("AGENTS.md"), "public\n").expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private\n").expect("source");
        std::fs::create_dir(root.join("real")).expect("real target dir");
        std::os::unix::fs::symlink("real", root.join("alias")).expect("target alias");

        let mut manifest = base_manifest();
        manifest["surfaces"][0]["targets"] = json!([
            {
                "carrier": "claude",
                "path": "real/missing.md",
                "ownership_mode": "carrier-owned"
            }
        ]);
        manifest["surfaces"][1]["targets"] = json!([
            {
                "carrier": "cursor",
                "path": "alias/missing.md",
                "ownership_mode": "source-owned"
            }
        ]);
        write_manifest(&manifest_path, &manifest);

        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        let resolved_future = root
            .canonicalize()
            .expect("canonical root")
            .join("real/missing.md");
        assert!(error.contains("duplicate resolved target"), "{error}");
        assert!(
            error.contains("a-public") && error.contains("z-private"),
            "{error}"
        );
        assert!(
            error.contains(resolved_future.to_string_lossy().as_ref()),
            "{error}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn missing_target_through_in_root_symlink_reports_canonical_future_path() {
        let (root, manifest_path) = fixture("missing-symlink-target");
        std::fs::write(root.join("AGENTS.md"), "public\n").expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private\n").expect("source");
        std::fs::create_dir(root.join("real")).expect("real target dir");
        std::os::unix::fs::symlink("real", root.join("alias")).expect("target alias");

        let mut manifest = base_manifest();
        manifest["surfaces"][0]["targets"] = json!([]);
        manifest["surfaces"][1]["targets"][0]["path"] = json!("alias/missing.md");
        write_manifest(&manifest_path, &manifest);

        let report = scan_instruction_manifest(&manifest_path).expect("missing target report");
        let resolved_future = root
            .canonicalize()
            .expect("canonical root")
            .join("real/missing.md");
        let value = serde_json::to_value(&report).expect("report JSON");
        assert_eq!(value["status"], "non_clean");
        assert_eq!(value["sources"][0]["targets"][0]["exists"], false);
        assert_eq!(value["sources"][0]["targets"][0]["status"], "missing");
        assert_eq!(
            value["sources"][0]["targets"][0]["resolved_path"],
            resolved_future.to_string_lossy().as_ref()
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn declared_fifo_is_rejected_without_blocking_or_opening() {
        const FIFO_CHILD_MANIFEST_ENV: &str = "TACHI_INSTRUCTION_MANIFEST_FIFO_CHILD";

        let (root, manifest_path) = fixture("fifo");
        std::fs::write(root.join("AGENTS.md"), "public\n").expect("source");
        std::fs::write(root.join("CLAUDE.md"), "private\n").expect("source");
        std::fs::create_dir_all(root.join("targets")).expect("target dir");
        let fifo_path = root.join("targets/fifo");
        let fifo_name = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
            fifo_path.as_os_str(),
        ))
        .expect("FIFO path");
        let result = unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) };
        assert_eq!(
            result,
            0,
            "create FIFO: {}",
            std::io::Error::last_os_error()
        );

        let mut manifest = base_manifest();
        manifest["surfaces"][0]["targets"] = json!([]);
        manifest["surfaces"][1]["targets"] = json!([
            {
                "carrier": "cursor",
                "path": "targets/fifo",
                "ownership_mode": "source-owned"
            }
        ]);
        write_manifest(&manifest_path, &manifest);

        let mut child = std::process::Command::new(
            std::env::current_exe().expect("instruction manifest test executable"),
        )
        .args([
            "--exact",
            "bootstrap::instruction_manifest::tests::scan_fifo_fixture_in_child",
            "--nocapture",
        ])
        .env(FIFO_CHILD_MANIFEST_ENV, &manifest_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn FIFO scan child");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll FIFO scan child") {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(root);
                panic!("FIFO scan blocked instead of rejecting the non-regular file");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let output = child.wait_with_output().expect("collect FIFO scan child");
        assert!(
            status.success(),
            "FIFO child failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn scan_fifo_fixture_in_child() {
        let Some(manifest_path) = std::env::var_os("TACHI_INSTRUCTION_MANIFEST_FIFO_CHILD") else {
            return;
        };
        let error = scan_instruction_manifest(Path::new(&manifest_path)).unwrap_err();
        assert!(error.contains("non-regular file"), "{error}");
    }

    #[cfg(unix)]
    fn assert_manifest_fifo_rejected_without_blocking(root: &Path, manifest_path: &Path) {
        const FIFO_MANIFEST_CHILD_ENV: &str = "TACHI_INSTRUCTION_MANIFEST_INPUT_FIFO_CHILD";

        let mut child = std::process::Command::new(
            std::env::current_exe().expect("instruction manifest test executable"),
        )
        .args([
            "--exact",
            "bootstrap::instruction_manifest::tests::scan_fifo_manifest_fixture_in_child",
            "--nocapture",
        ])
        .env(FIFO_MANIFEST_CHILD_ENV, manifest_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn FIFO manifest scan child");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll FIFO manifest scan child") {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(root);
                panic!("manifest FIFO scan blocked instead of rejecting the non-regular file");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let output = child
            .wait_with_output()
            .expect("collect FIFO manifest scan child");
        assert!(
            status.success(),
            "FIFO manifest child failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_fifo_manifest_is_rejected_without_blocking_or_reading() {
        let (root, _manifest_path) = fixture("fifo-manifest");
        let fifo_path = root.join(".agents/instruction-surfaces.fifo");
        let fifo_name = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
            fifo_path.as_os_str(),
        ))
        .expect("FIFO manifest path");
        let result = unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) };
        assert_eq!(
            result,
            0,
            "create manifest FIFO: {}",
            std::io::Error::last_os_error()
        );

        assert_manifest_fifo_rejected_without_blocking(&root, &fifo_path);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_fifo_manifest_is_rejected_without_blocking_or_reading() {
        let (root, _manifest_path) = fixture("symlink-fifo-manifest");
        let fifo_path = root.join(".agents/instruction-surfaces.fifo");
        let symlink_path = root.join(".agents/instruction-surfaces-link.json");
        let fifo_name = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
            fifo_path.as_os_str(),
        ))
        .expect("FIFO manifest path");
        let result = unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) };
        assert_eq!(
            result,
            0,
            "create manifest FIFO: {}",
            std::io::Error::last_os_error()
        );
        std::os::unix::fs::symlink("instruction-surfaces.fifo", &symlink_path)
            .expect("manifest FIFO symlink");

        assert_manifest_fifo_rejected_without_blocking(&root, &symlink_path);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn scan_fifo_manifest_fixture_in_child() {
        let Some(manifest_path) = std::env::var_os("TACHI_INSTRUCTION_MANIFEST_INPUT_FIFO_CHILD")
        else {
            return;
        };
        let error = scan_instruction_manifest(Path::new(&manifest_path)).unwrap_err();
        assert!(
            error.contains("read instruction manifest") && error.contains("non-regular file"),
            "{error}"
        );
    }

    #[test]
    fn declared_directory_and_invalid_utf8_are_loud_read_errors() {
        let (root, manifest_path) = fixture("invalid-declared-file");
        let manifest = base_manifest();

        std::fs::create_dir(root.join("AGENTS.md")).expect("declared directory");
        write_manifest(&manifest_path, &manifest);
        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        assert!(
            error.contains("read declared source") && error.contains("directory"),
            "{error}"
        );

        std::fs::remove_dir(root.join("AGENTS.md")).expect("declared directory cleanup");
        std::fs::write(root.join("AGENTS.md"), [0xff, 0xfe]).expect("invalid UTF-8 file");
        write_manifest(&manifest_path, &manifest);
        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        assert!(
            error.contains("read declared source") && error.contains("invalid UTF-8"),
            "{error}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_refused_before_reading_the_declared_file() {
        let (root, manifest_path) = fixture("symlink");
        let outside = root.parent().expect("fixture parent").join(format!(
            "instruction-manifest-outside-{}",
            std::process::id()
        ));
        std::fs::write(&outside, "outside\n").expect("outside file");
        std::os::unix::fs::symlink(&outside, root.join("escape.md")).expect("escape symlink");

        let mut manifest = base_manifest();
        manifest["surfaces"][0]["source"] = json!("escape.md");
        write_manifest(&manifest_path, &manifest);
        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        assert!(error.contains("outside declared root"), "{error}");

        let outside_dir = root.parent().expect("fixture parent").join(format!(
            "instruction-manifest-outside-dir-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&outside_dir).expect("outside directory");
        std::os::unix::fs::symlink(&outside_dir, root.join("escape-dir"))
            .expect("directory escape symlink");
        manifest["surfaces"][0]["source"] = json!("escape-dir/missing.md");
        write_manifest(&manifest_path, &manifest);
        let error = scan_instruction_manifest(&manifest_path).unwrap_err();
        assert!(error.contains("outside declared root"), "{error}");

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&outside_dir);
        let _ = std::fs::remove_dir_all(root);
    }
}
