use super::*;
use crate::utils::stable_hash;
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Component;

const EXPORT_MANIFEST_FORMAT: &str = "tachi_wiki_obsidian_manifest_v1";
const EXPORT_INDEX_PATH: &str = "_index.md";
const EXPORT_MANIFEST_PATH: &str = "_manifest.json";

#[derive(Debug, Clone)]
struct ExportCandidate {
    entry: MemoryEntry,
    store: StoreRef,
    source_key: String,
    wiki_path: String,
    relative_dir: PathBuf,
    file_stem: String,
}

#[derive(Debug, Clone)]
struct PlannedExportEntry {
    entry: MemoryEntry,
    store: StoreRef,
    source_key: String,
    wiki_path: String,
    output_path: PathBuf,
    content: String,
}

struct ExportPlan {
    entries: Vec<PlannedExportEntry>,
    index_content: String,
    manifest_content: String,
}

#[derive(Default)]
struct ExistingExport {
    entries: BTreeMap<PathBuf, String>,
    managed_paths: BTreeSet<PathBuf>,
}

pub(crate) trait ExportTestHook: Send + Sync {
    fn after_lock_acquired(&self) -> Result<(), String> {
        Ok(())
    }

    fn before_install(&self, _installed: usize, _path: &Path) -> Result<(), String> {
        Ok(())
    }
}

struct NoopExportHook;

impl ExportTestHook for NoopExportHook {}

fn store_ref_key(store: &StoreRef) -> String {
    serde_json::to_string(store).expect("StoreRef serialization is infallible")
}

fn source_key(store: &StoreRef, entry: &MemoryEntry) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        store_ref_key(store),
        entry.id,
        entry.path
    )
}

fn relative_wiki_dir(path: &str) -> PathBuf {
    path.trim_start_matches("/wiki")
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(sanitize_safe_path_name)
        .collect()
}

fn output_path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn reserved_output_path(path: &Path) -> bool {
    path == Path::new(EXPORT_INDEX_PATH) || path == Path::new(EXPORT_MANIFEST_PATH)
}

fn collision_file_name(candidate: &ExportCandidate, attempt: usize) -> String {
    let id = sanitize_safe_path_name(&candidate.entry.id);
    let digest = stable_hash(&candidate.source_key);
    if attempt == 0 {
        format!("{}--{}-{}.md", candidate.file_stem, id, digest)
    } else {
        format!("{}--{}-{}-{}.md", candidate.file_stem, id, digest, attempt)
    }
}

fn build_export_plan(entries: Vec<StoredWikiEntry>) -> Result<ExportPlan, String> {
    let mut candidates = Vec::new();
    let mut source_keys = BTreeSet::new();
    for stored in entries {
        let entry = stored.entry;
        if entry.path == "/wiki/_log" || !is_user_facing_wiki_entry(&entry) {
            continue;
        }
        let store = stored.store;
        let source_key = source_key(&store, &entry);
        if !source_keys.insert(source_key.clone()) {
            return Err(format!(
                "Refusing Wiki export: duplicate logical source entry '{}'; the export plan must contain one output per source entry",
                source_key
            ));
        }
        candidates.push(ExportCandidate {
            wiki_path: entry.path.clone(),
            relative_dir: relative_wiki_dir(&entry.path),
            file_stem: obsidian_file_stem(&entry),
            source_key,
            store,
            entry,
        });
    }
    candidates.sort_by(|a, b| a.source_key.cmp(&b.source_key));

    let mut collision_groups: BTreeMap<(PathBuf, String), Vec<usize>> = BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        collision_groups
            .entry((candidate.relative_dir.clone(), candidate.file_stem.clone()))
            .or_default()
            .push(index);
    }

    let mut used_output_paths = BTreeSet::new();
    let mut planned_entries = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        let group_size = collision_groups
            .get(&(candidate.relative_dir.clone(), candidate.file_stem.clone()))
            .map(Vec::len)
            .unwrap_or(1);
        let plain_path = candidate
            .relative_dir
            .join(format!("{}.md", candidate.file_stem));
        let needs_suffix = group_size > 1 || reserved_output_path(&plain_path);
        let mut attempt = 0;
        let output_path = loop {
            let file_name = if needs_suffix {
                collision_file_name(candidate, attempt)
            } else {
                format!("{}.md", candidate.file_stem)
            };
            let output_path = candidate.relative_dir.join(file_name);
            if used_output_paths.insert(output_path.clone()) {
                break output_path;
            }
            attempt += 1;
            if attempt == usize::MAX {
                return Err(format!(
                    "Refusing Wiki export: unable to deterministically disambiguate output for source '{}'",
                    candidate.source_key
                ));
            }
        };
        debug_assert_eq!(index, planned_entries.len());
        planned_entries.push(PlannedExportEntry {
            entry: candidate.entry.clone(),
            store: candidate.store.clone(),
            source_key: candidate.source_key.clone(),
            wiki_path: candidate.wiki_path.clone(),
            output_path,
            content: markdown_for_obsidian(&candidate.entry),
        });
    }

    let mut index: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
    for planned in &planned_entries {
        let file_name = planned
            .output_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                format!(
                    "Refusing Wiki export: output path '{}' has no valid filename",
                    planned.output_path.display()
                )
            })?;
        index.entry(planned.wiki_path.clone()).or_default().push((
            file_name.to_string(),
            output_path_string(&planned.output_path),
            planned.entry.summary.clone(),
        ));
    }

    let mut index_content = String::from("# Wiki Index\n\n");
    for (path, mut files) in index {
        files.sort_by(|a, b| a.1.cmp(&b.1));
        index_content.push_str(&format!("## {path}\n"));
        for (_file, output_path, summary) in files {
            let link = output_path.trim_end_matches(".md");
            if summary.is_empty() {
                index_content.push_str(&format!("- [[{link}]]\n"));
            } else {
                index_content.push_str(&format!("- [[{link}]] - {summary}\n"));
            }
        }
        index_content.push('\n');
    }

    let manifest_entries = planned_entries
        .iter()
        .map(|planned| {
            json!({
                "entry_id": planned.entry.id.clone(),
                "store_ref": planned.store.clone(),
                "wiki_path": planned.wiki_path.clone(),
                "output_path": output_path_string(&planned.output_path),
            })
        })
        .collect::<Vec<_>>();
    let manifest = json!({
        "format": EXPORT_MANIFEST_FORMAT,
        "count": planned_entries.len(),
        "entries": manifest_entries,
    });
    let manifest_content = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("serialize Wiki export manifest: {e}"))?
        + "\n";

    Ok(ExportPlan {
        entries: planned_entries,
        index_content,
        manifest_content,
    })
}

fn parse_manifest_output_path(value: &serde_json::Value) -> Result<PathBuf, String> {
    let raw = value
        .as_str()
        .ok_or_else(|| "Wiki export manifest output_path must be a string".to_string())?;
    let path = PathBuf::from(raw);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "Wiki export manifest output_path '{}' is not a safe relative path",
            raw
        ));
    }
    Ok(path)
}

fn path_metadata(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspect export path '{}': {error}", path.display())),
    }
}

fn require_regular_managed_file(path: &Path, label: &str) -> Result<(), String> {
    let metadata = path_metadata(path)?.ok_or_else(|| {
        format!(
            "Refusing Wiki export: valid prior manifest claims {label} '{}' but it is missing",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "Refusing Wiki export: prior-manifest-owned {label} '{}' is not a regular non-symlink file",
            path.display()
        ));
    }
    Ok(())
}

fn validate_output_components(output: &Path, relative: &Path) -> Result<(), String> {
    let mut cursor = output.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(segment) = component else {
            return Err(format!(
                "Refusing Wiki export: output path '{}' is not a safe relative path",
                relative.display()
            ));
        };
        cursor.push(segment);
        let Some(metadata) = path_metadata(&cursor)? else {
            break;
        };
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "Refusing Wiki export: output component '{}' is a symlink",
                cursor.display()
            ));
        }
        if index + 1 < components.len() && !metadata.is_dir() {
            return Err(format!(
                "Refusing Wiki export: output parent component '{}' is not a directory",
                cursor.display()
            ));
        }
    }
    Ok(())
}

fn parse_existing_manifest(output: &Path) -> Result<ExistingExport, String> {
    if let Some(metadata) = path_metadata(output)? {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "Refusing Wiki export: output '{}' is not a regular directory",
                output.display()
            ));
        }
    }

    let index_path = output.join(EXPORT_INDEX_PATH);
    let manifest_path = output.join(EXPORT_MANIFEST_PATH);
    let index_exists = path_metadata(&index_path)?.is_some();
    let Some(manifest_metadata) = path_metadata(&manifest_path)? else {
        if index_exists {
            return Err(format!(
                "Refusing Wiki export: reserved managed artifact '{}' exists without a valid prior Tachi manifest ownership marker",
                index_path.display()
            ));
        }
        return Ok(ExistingExport::default());
    };
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        return Err(format!(
            "Refusing Wiki export: reserved managed artifact '{}' is not a regular Tachi manifest",
            manifest_path.display()
        ));
    }

    let content = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read existing Wiki export manifest: {e}"))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("parse existing Wiki export manifest: {e}"))?;
    let format = value
        .get("format")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "existing Wiki export manifest has no format marker".to_string())?;
    if format != EXPORT_MANIFEST_FORMAT {
        return Err(format!(
            "existing Wiki export manifest format '{format}' is not '{EXPORT_MANIFEST_FORMAT}'"
        ));
    }
    let entries = value
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "existing Wiki export manifest has no entries array".to_string())?;
    let count = value
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "existing Wiki export manifest has no numeric count".to_string())?;
    if count != entries.len() as u64 {
        return Err(format!(
            "existing Wiki export manifest count {} does not match {} entries",
            count,
            entries.len()
        ));
    }

    require_regular_managed_file(&index_path, "reserved index")?;
    let mut existing = ExistingExport::default();
    existing
        .managed_paths
        .insert(PathBuf::from(EXPORT_INDEX_PATH));
    existing
        .managed_paths
        .insert(PathBuf::from(EXPORT_MANIFEST_PATH));
    let mut source_keys = BTreeSet::new();
    for entry in entries {
        let output_path =
            parse_manifest_output_path(entry.get("output_path").ok_or_else(|| {
                "existing Wiki export manifest entry has no output_path".to_string()
            })?)?;
        if reserved_output_path(&output_path) {
            return Err(format!(
                "existing Wiki export manifest entry illegally claims reserved managed artifact '{}'",
                output_path.display()
            ));
        }
        let entry_id = entry
            .get("entry_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "existing Wiki export manifest entry has no entry_id".to_string())?;
        let store_ref = entry
            .get("store_ref")
            .ok_or_else(|| "existing Wiki export manifest entry has no store_ref".to_string())?;
        let wiki_path = entry
            .get("wiki_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "existing Wiki export manifest entry has no wiki_path".to_string())?;
        let store_ref: StoreRef = serde_json::from_value(store_ref.clone())
            .map_err(|e| format!("existing Wiki export manifest has invalid store_ref: {e}"))?;
        let store_ref = store_ref_key(&store_ref);
        let source_key = format!("{store_ref}\u{1f}{entry_id}\u{1f}{wiki_path}");
        if !source_keys.insert(source_key.clone()) {
            return Err(format!(
                "existing Wiki export manifest repeats logical source entry '{source_key}'"
            ));
        }
        if existing
            .entries
            .insert(output_path.clone(), source_key)
            .is_some()
        {
            return Err(format!(
                "existing Wiki export manifest maps multiple entries to output '{}'",
                output_path.display()
            ));
        }
        validate_output_components(output, &output_path)?;
        require_regular_managed_file(&output.join(&output_path), "entry file")?;
        existing.managed_paths.insert(output_path);
    }
    Ok(existing)
}

fn validate_output_targets(output: &Path, plan: &ExportPlan) -> Result<ExistingExport, String> {
    let existing = parse_existing_manifest(output)?;
    for planned in &plan.entries {
        validate_output_components(output, &planned.output_path)?;
        let target = output.join(&planned.output_path);
        if path_metadata(&target)?.is_none() {
            continue;
        }
        match existing.entries.get(&planned.output_path) {
            Some(previous) if previous == &planned.source_key => {}
            Some(previous) => {
                return Err(format!(
                    "Refusing Wiki export: output '{}' belongs to a different source entry ('{}' instead of '{}')",
                    target.display(),
                    previous,
                    planned.source_key
                ));
            }
            None => {
                return Err(format!(
                    "Refusing Wiki export: output '{}' already exists without a matching manifest entry; refusing to overwrite another source",
                    target.display()
                ));
            }
        }
    }
    Ok(existing)
}

fn canonical_output_lock_key(output: &Path) -> Result<String, String> {
    let absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("resolve export current directory: {e}"))?
            .join(output)
    };
    if absolute.exists() {
        return absolute
            .canonicalize()
            .map(|path| path.display().to_string())
            .map_err(|e| format!("canonicalize export output '{}': {e}", absolute.display()));
    }
    let mut suffix = Vec::new();
    let mut ancestor = absolute.as_path();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            format!(
                "cannot resolve existing ancestor for export output '{}'",
                absolute.display()
            )
        })?;
        suffix.push(name.to_os_string());
        ancestor = ancestor.parent().ok_or_else(|| {
            format!(
                "cannot resolve parent for export output '{}'",
                absolute.display()
            )
        })?;
    }
    let mut normalized = ancestor
        .canonicalize()
        .map_err(|e| format!("canonicalize export ancestor '{}': {e}", ancestor.display()))?;
    for component in suffix.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized.display().to_string())
}

struct ExportLock {
    _file: fs::File,
}

fn acquire_export_lock(server: &MemoryServer, output: &Path) -> Result<ExportLock, String> {
    let key = canonical_output_lock_key(output)?;
    let lock_path = server
        .tachi_home_dir()
        .join("runtime/wiki-export-locks")
        .join(format!("{}.lock", stable_hash(&key)));
    let parent = lock_path
        .parent()
        .ok_or_else(|| format!("export lock '{}' has no parent", lock_path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "create export lock directory '{}': {error}",
            parent.display()
        )
    })?;
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options.open(&lock_path).map_err(|error| {
        format!(
            "Refusing Wiki export: open cooperative per-output export lock '{}': {error}",
            lock_path.display()
        )
    })?;
    let path_metadata = fs::symlink_metadata(&lock_path)
        .map_err(|error| format!("inspect export lock '{}': {error}", lock_path.display()))?;
    let file_metadata = file
        .metadata()
        .map_err(|error| format!("stat export lock '{}': {error}", lock_path.display()))?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_file()
        || path_metadata.dev() != file_metadata.dev()
        || path_metadata.ino() != file_metadata.ino()
    {
        return Err(format!(
            "Refusing Wiki export: cooperative per-output export lock '{}' changed physical identity",
            lock_path.display()
        ));
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(format!(
            "Refusing Wiki export: cooperative per-output export lock '{}' is already held or unavailable: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    // Keep the stable lock inode on disk. Unlinking it on Drop would permit a
    // waiter to lock the old inode while a third exporter creates and locks a
    // new inode at the same path.
    Ok(ExportLock { _file: file })
}

struct ExportStage {
    root: PathBuf,
    new_root: PathBuf,
    backup_root: PathBuf,
}

impl ExportStage {
    fn create(output: &Path) -> Result<Self, String> {
        let parent = output.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|e| format!("create export staging parent '{}': {e}", parent.display()))?;
        for _ in 0..32 {
            let root = parent.join(format!(
                ".tachi-wiki-export-stage-{}",
                uuid::Uuid::new_v4().as_simple()
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    let new_root = root.join("new");
                    let backup_root = root.join("backup");
                    fs::create_dir(&new_root)
                        .map_err(|e| format!("create export new staging directory: {e}"))?;
                    fs::create_dir(&backup_root)
                        .map_err(|e| format!("create export backup staging directory: {e}"))?;
                    return Ok(Self {
                        root,
                        new_root,
                        backup_root,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("create unique export staging directory: {error}"));
                }
            }
        }
        Err("create unique export staging directory: exhausted attempts".to_string())
    }

    fn write_new(&self, relative: &Path, content: &str) -> Result<(), String> {
        let path = self.new_root.join(relative);
        let parent = path
            .parent()
            .ok_or_else(|| format!("staged output '{}' has no parent", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|e| format!("create staged output parent '{}': {e}", parent.display()))?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("create-new staged export file '{}': {e}", path.display()))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("write staged export file '{}': {e}", path.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync staged export file '{}': {e}", path.display()))?;
        Ok(())
    }
}

impl Drop for ExportStage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn stage_export(output: &Path, plan: &ExportPlan) -> Result<ExportStage, String> {
    let stage = ExportStage::create(output)?;
    for planned in &plan.entries {
        stage.write_new(&planned.output_path, &planned.content)?;
    }
    stage.write_new(Path::new(EXPORT_INDEX_PATH), &plan.index_content)?;
    stage.write_new(Path::new(EXPORT_MANIFEST_PATH), &plan.manifest_content)?;
    Ok(stage)
}

fn ensure_output_parent(
    output: &Path,
    relative: &Path,
    created_dirs: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let parent = relative
        .parent()
        .ok_or_else(|| format!("export output '{}' has no parent", relative.display()))?;
    let mut cursor = output.to_path_buf();
    if path_metadata(&cursor)?.is_none() {
        fs::create_dir(&cursor)
            .map_err(|e| format!("create export output '{}': {e}", cursor.display()))?;
        created_dirs.push(cursor.clone());
    }
    for component in parent.components() {
        let Component::Normal(segment) = component else {
            return Err(format!("unsafe export parent '{}'", parent.display()));
        };
        cursor.push(segment);
        match path_metadata(&cursor)? {
            Some(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(format!(
                    "Refusing Wiki export: output parent '{}' is not a regular directory",
                    cursor.display()
                ));
            }
            Some(_) => {}
            None => {
                fs::create_dir(&cursor).map_err(|e| {
                    format!("create export output parent '{}': {e}", cursor.display())
                })?;
                created_dirs.push(cursor.clone());
            }
        }
    }
    validate_output_components(output, relative)
}

fn rollback_export(
    output: &Path,
    stage: &ExportStage,
    installed: &[PathBuf],
    moved_old: &[PathBuf],
    created_dirs: &[PathBuf],
) -> Result<(), String> {
    let mut errors = Vec::new();
    for relative in installed.iter().rev() {
        let path = output.join(relative);
        if let Err(error) = fs::remove_file(&path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!("remove new '{}': {error}", path.display()));
            }
        }
    }
    for relative in moved_old.iter().rev() {
        let backup = stage.backup_root.join(relative);
        let destination = output.join(relative);
        if let Some(parent) = destination.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                errors.push(format!(
                    "recreate rollback parent '{}': {error}",
                    parent.display()
                ));
                continue;
            }
        }
        if let Err(error) = fs::rename(&backup, &destination) {
            errors.push(format!(
                "restore prior managed file '{}' -> '{}': {error}",
                backup.display(),
                destination.display()
            ));
        }
    }
    for directory in created_dirs.iter().rev() {
        let _ = fs::remove_dir(directory);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn commit_export(
    output: &Path,
    plan: &ExportPlan,
    existing: &ExistingExport,
    stage: &ExportStage,
    hook: &dyn ExportTestHook,
) -> Result<(), String> {
    let mut moved_old = Vec::new();
    let mut installed = Vec::new();
    let mut created_dirs = Vec::new();
    let commit_result: Result<(), String> = (|| {
        for relative in &existing.managed_paths {
            let source = output.join(relative);
            let backup = stage.backup_root.join(relative);
            let parent = backup
                .parent()
                .ok_or_else(|| format!("backup path '{}' has no parent", backup.display()))?;
            fs::create_dir_all(parent)
                .map_err(|e| format!("create export backup parent '{}': {e}", parent.display()))?;
            fs::rename(&source, &backup).map_err(|e| {
                format!(
                    "preserve prior managed file '{}' -> '{}': {e}",
                    source.display(),
                    backup.display()
                )
            })?;
            moved_old.push(relative.clone());
        }

        let mut install_paths = plan
            .entries
            .iter()
            .map(|planned| planned.output_path.clone())
            .collect::<Vec<_>>();
        install_paths.push(PathBuf::from(EXPORT_INDEX_PATH));
        install_paths.push(PathBuf::from(EXPORT_MANIFEST_PATH));
        for relative in install_paths {
            hook.before_install(installed.len(), &relative)?;
            ensure_output_parent(output, &relative, &mut created_dirs)?;
            let staged = stage.new_root.join(&relative);
            let destination = output.join(&relative);
            fs::rename(&staged, &destination).map_err(|e| {
                format!(
                    "install staged managed file '{}' -> '{}': {e}",
                    staged.display(),
                    destination.display()
                )
            })?;
            installed.push(relative);
        }
        Ok(())
    })();

    if let Err(error) = commit_result {
        return match rollback_export(output, stage, &installed, &moved_old, &created_dirs) {
            Ok(()) => Err(format!(
                "Wiki export transaction failed and prior managed state was restored: {error}"
            )),
            Err(rollback) => Err(format!(
                "Wiki export transaction failed: {error}; ROLLBACK INCOMPLETE: {rollback}"
            )),
        };
    }
    Ok(())
}

fn obsidian_file_stem(entry: &MemoryEntry) -> String {
    let topic = entry.topic.trim();
    if !topic.is_empty() {
        return sanitize_safe_path_name(topic);
    }
    let summary = entry.summary.trim();
    if !summary.is_empty() {
        return sanitize_safe_path_name(summary);
    }
    sanitize_safe_path_name(&entry.id)
}

fn yaml_string_list(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".to_string();
    }
    let items = values
        .iter()
        .map(|value| format!("\"{}\"", value.replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

fn obsidian_link_entities(text: &str, entities: &[String]) -> String {
    let mut sorted = entities
        .iter()
        .map(|entity| entity.trim())
        .filter(|entity| !entity.is_empty())
        .collect::<Vec<_>>();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    sorted.dedup();

    let mut out = String::with_capacity(text.len());
    let mut idx = 0usize;
    while idx < text.len() {
        if text[idx..].starts_with("[[") {
            if let Some(end) = text[idx + 2..].find("]]") {
                let end_idx = idx + 2 + end + 2;
                out.push_str(&text[idx..end_idx]);
                idx = end_idx;
                continue;
            }
        }

        let mut matched: Option<&str> = None;
        for entity in &sorted {
            if text[idx..].starts_with(*entity) && is_entity_boundary(text, idx, idx + entity.len())
            {
                matched = Some(entity);
                break;
            }
        }
        if let Some(entity) = matched {
            out.push_str("[[");
            out.push_str(entity);
            out.push_str("]]");
            idx += entity.len();
        } else if let Some(ch) = text[idx..].chars().next() {
            out.push(ch);
            idx += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

fn is_entity_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = if start == 0 {
        None
    } else {
        text[..start].chars().next_back()
    };
    let after = if end >= text.len() {
        None
    } else {
        text[end..].chars().next()
    };
    !before.is_some_and(is_entity_word_char) && !after.is_some_and(is_entity_word_char)
}

fn is_entity_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '-'
}

fn reference_line(ref_str: &str) -> String {
    if ref_str.starts_with("http://")
        || ref_str.starts_with("https://")
        || ref_str.starts_with("file://")
    {
        format!("- [{ref_str}]({ref_str})\n")
    } else {
        format!("- `{ref_str}`\n")
    }
}

fn append_references_section(body: &mut String, metadata: &serde_json::Value) {
    let typed_refs: Vec<&serde_json::Value> = metadata
        .get("evidence_refs_v1")
        .and_then(|v| v.as_array())
        .map(|refs| {
            refs.iter()
                .filter(|value| {
                    value
                        .get("ref")
                        .and_then(|v| v.as_str())
                        .is_some_and(|reference| !reference.trim().is_empty())
                })
                .collect()
        })
        .unwrap_or_default();
    if !typed_refs.is_empty() {
        body.push_str("\n\n## Evidence Refs (typed)\n\n");
        for typed_ref in typed_refs {
            let Some(ref_str) = typed_ref.get("ref").and_then(|v| v.as_str()) else {
                continue;
            };
            let kind = typed_ref
                .get("target_kind")
                .and_then(|v| v.as_str())
                .map(|kind| format!(" ({kind})"))
                .unwrap_or_default();
            let mut line = reference_line(ref_str);
            line.truncate(line.trim_end_matches('\n').len());
            body.push_str(&line);
            body.push_str(&kind);
            body.push('\n');
        }
        return;
    }
    let legacy_refs: Vec<&str> = metadata
        .get("source_refs")
        .and_then(|v| v.as_array())
        .map(|refs| {
            refs.iter()
                .filter_map(|value| value.as_str())
                .filter(|reference| !reference.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    if !legacy_refs.is_empty() {
        body.push_str("\n\n## References\n\n");
        for ref_str in legacy_refs {
            body.push_str(&reference_line(ref_str));
        }
    }
}

fn markdown_for_obsidian(entry: &MemoryEntry) -> String {
    let mut body = String::new();
    body.push_str("---\n");
    body.push_str(&format!("id: \"{}\"\n", entry.id.replace('"', "\\\"")));
    body.push_str(&format!("importance: {}\n", entry.importance));
    body.push_str(&format!(
        "keywords: {}\n",
        yaml_string_list(&entry.keywords)
    ));
    body.push_str(&format!(
        "entities: {}\n",
        yaml_string_list(&entry.entities)
    ));
    body.push_str(&format!("tags: {}\n", yaml_string_list(&entry.keywords)));
    body.push_str(&format!(
        "timestamp: \"{}\"\n",
        entry.timestamp.replace('"', "\\\"")
    ));
    body.push_str(&format!(
        "category: \"{}\"\n",
        entry.category.replace('"', "\\\"")
    ));
    body.push_str("---\n\n");
    body.push_str(&obsidian_link_entities(&entry.text, &entry.entities));
    if !entry.entities.is_empty() {
        body.push_str("\n\n## See Also\n");
        for entity in &entry.entities {
            body.push_str(&format!("- [[{}]]\n", entity));
        }
    }
    append_references_section(&mut body, &entry.metadata);
    body
}

pub(crate) fn export_wiki_obsidian(
    server: &MemoryServer,
    project: &str,
    output: &Path,
) -> Result<Value, String> {
    export_wiki_obsidian_with_hook(server, project, output, &NoopExportHook)
}

pub(crate) fn export_wiki_obsidian_with_hook(
    server: &MemoryServer,
    project: &str,
    output: &Path,
    hook: &dyn ExportTestHook,
) -> Result<Value, String> {
    let _lock = acquire_export_lock(server, output)?;
    hook.after_lock_acquired()?;
    let plan = WikiReadPlan::from_project(Some(project))?;
    let entries = list_wiki_entries_for_plan(server, &plan, "/wiki", 100_000)?;

    // The complete plan, rendered entry content, index, and manifest are all
    // built before the first output-directory mutation. This is the boundary
    // that prevents a late collision or manifest error from leaving a partial
    // export behind.
    let export_plan = build_export_plan(entries)?;
    let existing = validate_output_targets(output, &export_plan)?;
    let stage = stage_export(output, &export_plan)?;
    commit_export(output, &export_plan, &existing, &stage, hook)?;

    let index_path = output.join(EXPORT_INDEX_PATH);
    let manifest_path = output.join(EXPORT_MANIFEST_PATH);

    append_wiki_log(
        server,
        "export",
        &format!(
            "obsidian | {} entry(s) -> {}",
            export_plan.entries.len(),
            output.display()
        ),
    );

    Ok(json!({
        "status": "completed",
        "format": "obsidian",
        "output": output,
        "count": export_plan.entries.len(),
        "written_files": export_plan.entries.len(),
        "index": index_path,
        "manifest": manifest_path,
    }))
}
