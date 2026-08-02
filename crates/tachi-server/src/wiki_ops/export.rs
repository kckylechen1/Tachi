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
/// Directory-name prefix for per-run staging roots. Recovery scans the output's
/// parent for this prefix, so it must stay stable across releases.
const EXPORT_STAGE_PREFIX: &str = ".tachi-wiki-export-stage-";
/// Crash-recovery claim written inside every staging root. Its presence, a
/// matching canonical output key, and a matching output `(dev, ino)` together
/// are the only evidence that authorizes touching an output directory that
/// would otherwise fail the reserved-artifact ownership check.
const EXPORT_STAGE_CLAIM_PATH: &str = "_stage.json";
const EXPORT_STAGE_CLAIM_TMP_PATH: &str = "_stage.json.tmp";
const EXPORT_STAGE_CLAIM_FORMAT: &str = "tachi_wiki_export_stage_v1";
/// Extension carried by every generated wiki file.
const EXPORT_FILE_EXTENSION: &str = ".md";
/// Upper bound on a complete generated file name (stem + disambiguators + ext).
/// Every common filesystem caps a single name component at NAME_MAX, which is
/// 255 bytes on APFS and ext4; a longer component fails the write outright with
/// ENAMETOOLONG.
const EXPORT_FILE_NAME_MAX_BYTES: usize = 255;
/// Upper bound on a generated file stem: the longest stem whose plain
/// `<stem>.md` name still fits `EXPORT_FILE_NAME_MAX_BYTES`.
///
/// LLM-authored topics are unbounded, so the stem needs *a* bound. It is
/// deliberately the exact limit rather than a round number: truncating a stem
/// changes the file name, and changing the file name of a note that already
/// exists in the vault silently orphans every wikilink pointing at it. Only
/// stems the unbounded predecessor could not write at all (253 bytes and up,
/// where `<stem>.md` exceeds 255) are rewritten here; everything it could
/// write keeps its exact bytes.
const EXPORT_FILE_STEM_MAX_BYTES: usize = EXPORT_FILE_NAME_MAX_BYTES - EXPORT_FILE_EXTENSION.len();
const EXPORT_NAME_DIGEST_BYTES: usize = 8;

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

    /// Fires once the staging tree is fully written and its claim is durable,
    /// but before the commit claim is armed and before the output directory is
    /// touched.
    fn after_stage_ready(&self) -> Result<(), String> {
        Ok(())
    }

    /// Fires before each prior-managed file is moved aside into the backup
    /// tree. `moved` is the number of files already moved.
    fn before_backup(&self, _moved: usize, _path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn before_install(&self, _installed: usize, _path: &Path) -> Result<(), String> {
        Ok(())
    }

    /// When true, an `Err` from any hook point above is treated as a simulated
    /// SIGKILL rather than a recoverable error: no rollback runs, and the
    /// staging directory (with whatever claim it currently carries) is left on
    /// disk exactly as a killed process would leave it. This is the only way to
    /// exercise the crash windows that `Drop`-based cleanup cannot cover.
    fn simulates_process_death(&self) -> bool {
        false
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

/// Truncate `value` to at most `max_bytes`, replacing the dropped tail with a
/// short digest of the *whole* original so distinct inputs that share a prefix
/// still map to distinct outputs. `suffix_extra` is appended after the digest
/// (used to keep the `.md` extension on file names).
fn bounded_name(value: &str, max_bytes: usize, suffix_extra: &str) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let digest = stable_hash(value);
    let suffix = format!("-{}{}", &digest[..EXPORT_NAME_DIGEST_BYTES], suffix_extra);
    // The bounds are compile-time larger than any suffix we build here, but keep
    // the arithmetic saturating so a future constant change cannot panic.
    let mut keep = max_bytes.saturating_sub(suffix.len());
    // `sanitize_safe_path_name` emits ASCII only, so this loop is a no-op today;
    // it keeps the truncation panic-free if a non-ASCII producer is ever added.
    while keep > 0 && !value.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}{suffix}", &value[..keep])
}

fn collision_file_name(candidate: &ExportCandidate, attempt: usize) -> String {
    let id = sanitize_safe_path_name(&candidate.entry.id);
    let digest = stable_hash(&candidate.source_key);
    let name = if attempt == 0 {
        format!(
            "{}--{}-{}{EXPORT_FILE_EXTENSION}",
            candidate.file_stem, id, digest
        )
    } else {
        format!(
            "{}--{}-{}-{}{EXPORT_FILE_EXTENSION}",
            candidate.file_stem, id, digest, attempt
        )
    };
    // A stem that fits `<stem>.md` can still overflow once the disambiguators
    // are appended, and an unbounded entry id can overflow it on its own, so the
    // collision form is bounded by the whole name. Every attempt stays distinct:
    // a different `attempt` yields a different digest, so the disambiguation
    // loop still terminates.
    bounded_name(&name, EXPORT_FILE_NAME_MAX_BYTES, EXPORT_FILE_EXTENSION)
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
            .join(format!("{}{EXPORT_FILE_EXTENSION}", candidate.file_stem));
        let needs_suffix = group_size > 1 || reserved_output_path(&plain_path);
        let mut attempt = 0;
        let output_path = loop {
            let file_name = if needs_suffix {
                collision_file_name(candidate, attempt)
            } else {
                format!("{}{EXPORT_FILE_EXTENSION}", candidate.file_stem)
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

fn acquire_export_lock(server: &MemoryServer, key: &str) -> Result<ExportLock, String> {
    let lock_path = server
        .tachi_home_dir()
        .join("runtime/wiki-export-locks")
        .join(format!("{}.lock", stable_hash(key)));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagePhase {
    /// The staging tree is still being built. Nothing in the output directory
    /// has been touched, so a leftover staging root in this phase is pure
    /// residue and is simply deleted.
    Staging,
    /// The publish sequence has started (or is about to). The output directory
    /// may be torn, and the claim carries everything needed to roll the publish
    /// forward to completion.
    Committing,
    /// A recoverable publish error was hit and the run decided to restore the
    /// prior state. Recovery completes that restore instead of rolling forward,
    /// so a crash inside the rollback window is not a torn output either.
    RollingBack,
}

impl StagePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Committing => "committing",
            Self::RollingBack => "rolling_back",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "staging" => Some(Self::Staging),
            "committing" => Some(Self::Committing),
            "rolling_back" => Some(Self::RollingBack),
            _ => None,
        }
    }
}

/// Physical identity of the output directory a staging root belongs to.
///
/// The canonical path alone is not an ownership proof. A path is a name, and
/// the directory behind it can be deleted and replaced between the crash and
/// the recovery; a claim that only names the path would then let a surviving
/// `committing` phase publish into a stranger's directory, and it would do so
/// *before* `validate_output_targets` — the check that exists to refuse exactly
/// that directory. So the claim also pins the `(dev, ino)` the interrupted run
/// actually wrote into, and recovery re-stats the path and declines unless both
/// still match. (Inode numbers can be recycled after a delete, so this narrows
/// the window rather than closing it absolutely; declining is the safe side,
/// since a declined claim just falls through to the ownership refusal.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputIdentity {
    dev: u64,
    ino: u64,
}

impl OutputIdentity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
}

/// On-disk recovery journal for one staging root.
struct StageClaim {
    output_key: String,
    /// The output directory's physical identity when the claim was written.
    /// `None` for a `staging` claim, which never touches the output and so has
    /// nothing to pin; every output-mutating phase records it.
    output_identity: Option<OutputIdentity>,
    phase: StagePhase,
    /// Every relative path the interrupted run intended to publish, in publish
    /// order (entries, then the reserved index, then the reserved manifest).
    install_paths: Vec<PathBuf>,
    /// Every relative path the interrupted run owned before it started, i.e.
    /// the prior manifest's managed set.
    backup_paths: Vec<PathBuf>,
}

/// fsync a directory handle so renames that landed in it survive a power cut.
///
/// Measured on this repo's macOS/APFS target, `F_FULLFSYNC` on a directory
/// descriptor (what `File::sync_all` issues there) returns 0, so a failure is a
/// real filesystem error rather than an unsupported-operation stub.
fn sync_dir_durable(path: &Path) -> Result<(), String> {
    let dir = fs::File::open(path)
        .map_err(|e| format!("open directory for fsync '{}': {e}", path.display()))?;
    dir.sync_all()
        .map_err(|e| format!("fsync directory '{}': {e}", path.display()))
}

/// Best-effort directory fsync, for the sites where a sync failure must not
/// fail an otherwise-good export: crash atomicity there rests on `rename`
/// ordering, which a killed process cannot tear, so the sync only tightens the
/// power-loss tail, and `finish_interrupted_commit` reports rather than hides
/// what that tail can leave behind. The stage claim is the one exception — see
/// [`write_stage_claim`].
fn sync_dir(path: &Path) {
    let _ = sync_dir_durable(path);
}

/// fsync every directory in `path`'s subtree (directories only; the files
/// themselves are fsynced as they are written).
fn sync_dir_tree(path: &Path) -> Result<(), String> {
    for entry in fs::read_dir(path)
        .map_err(|e| format!("scan staging directory '{}': {e}", path.display()))?
    {
        let entry =
            entry.map_err(|e| format!("read staging entry under '{}': {e}", path.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|e| format!("stat staging entry '{}': {e}", entry.path().display()))?;
        if file_type.is_dir() {
            sync_dir_tree(&entry.path())?;
        }
    }
    sync_dir(path);
    Ok(())
}

fn write_stage_claim(
    root: &Path,
    output_key: &str,
    output_identity: Option<OutputIdentity>,
    phase: StagePhase,
    install_paths: &[PathBuf],
    backup_paths: &[PathBuf],
) -> Result<(), String> {
    let claim = json!({
        "format": EXPORT_STAGE_CLAIM_FORMAT,
        "output": output_key,
        "output_dev": output_identity.map(|identity| identity.dev),
        "output_ino": output_identity.map(|identity| identity.ino),
        "phase": phase.as_str(),
        "install_paths": install_paths.iter().map(|path| output_path_string(path.as_path())).collect::<Vec<_>>(),
        "backup_paths": backup_paths.iter().map(|path| output_path_string(path.as_path())).collect::<Vec<_>>(),
    });
    let content = serde_json::to_string_pretty(&claim)
        .map_err(|e| format!("serialize export stage claim: {e}"))?
        + "\n";
    let tmp_path = root.join(EXPORT_STAGE_CLAIM_TMP_PATH);
    let claim_path = root.join(EXPORT_STAGE_CLAIM_PATH);
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| format!("create export stage claim '{}': {e}", tmp_path.display()))?;
        file.write_all(content.as_bytes())
            .map_err(|e| format!("write export stage claim '{}': {e}", tmp_path.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync export stage claim '{}': {e}", tmp_path.display()))?;
    }
    // Rename + directory fsync is the commit point for the claim itself: a
    // reader either sees the whole previous claim or the whole new one.
    fs::rename(&tmp_path, &claim_path).map_err(|e| {
        format!(
            "publish export stage claim '{}' -> '{}': {e}",
            tmp_path.display(),
            claim_path.display()
        )
    })?;
    // Fatal, unlike every other directory sync in this module: the whole
    // recovery protocol is keyed on the claim being readable after a crash, so
    // a claim whose directory entry never reached the disk is an export that
    // silently cannot be repaired. Refusing here keeps the failure loud.
    sync_dir_durable(root)
}

fn claim_paths(value: &serde_json::Value, key: &str) -> Option<Vec<PathBuf>> {
    let array = value.get(key)?.as_array()?;
    let mut paths = Vec::with_capacity(array.len());
    for item in array {
        // Reuse the manifest path validator: a claim is untrusted on-disk input
        // and must never be able to escape the output directory.
        paths.push(parse_manifest_output_path(item).ok()?);
    }
    Some(paths)
}

/// Read a staging root's claim. Anything unreadable, malformed, foreign, or
/// unsafe yields `None`, which means "not provably ours — leave it alone".
fn read_stage_claim(root: &Path) -> Option<StageClaim> {
    let claim_path = root.join(EXPORT_STAGE_CLAIM_PATH);
    let metadata = fs::symlink_metadata(&claim_path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let content = fs::read_to_string(&claim_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    if value.get("format").and_then(serde_json::Value::as_str)? != EXPORT_STAGE_CLAIM_FORMAT {
        return None;
    }
    let output_identity = match (value.get("output_dev"), value.get("output_ino")) {
        (None | Some(serde_json::Value::Null), None | Some(serde_json::Value::Null)) => None,
        // Half a pinned identity is a malformed claim, not a permissive one.
        (Some(dev), Some(ino)) => Some(OutputIdentity {
            dev: dev.as_u64()?,
            ino: ino.as_u64()?,
        }),
        _ => return None,
    };
    Some(StageClaim {
        output_key: value
            .get("output")
            .and_then(serde_json::Value::as_str)?
            .to_string(),
        output_identity,
        phase: StagePhase::parse(value.get("phase").and_then(serde_json::Value::as_str)?)?,
        install_paths: claim_paths(&value, "install_paths")?,
        backup_paths: claim_paths(&value, "backup_paths")?,
    })
}

struct ExportStage {
    root: PathBuf,
    new_root: PathBuf,
    backup_root: PathBuf,
    output_key: String,
    /// When false, `Drop` leaves the staging root on disk. Cleared either after
    /// a successful publish (already cleaned explicitly) or when a test hook
    /// simulates process death.
    armed: bool,
}

impl ExportStage {
    fn create(output: &Path, output_key: &str) -> Result<Self, String> {
        let parent = output.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|e| format!("create export staging parent '{}': {e}", parent.display()))?;
        for _ in 0..32 {
            let root = parent.join(format!(
                "{EXPORT_STAGE_PREFIX}{}",
                uuid::Uuid::new_v4().as_simple()
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    let new_root = root.join("new");
                    let backup_root = root.join("backup");
                    // Claim the staging root for this output before a single
                    // byte is staged, so even a crash mid-staging leaves an
                    // attributable (and therefore reclaimable) directory. Until
                    // that claim lands the root is unattributable, so a failure
                    // here has to clean up after itself rather than leave
                    // residue no later run is allowed to touch.
                    let prepared = (|| -> Result<(), String> {
                        fs::create_dir(&new_root)
                            .map_err(|e| format!("create export new staging directory: {e}"))?;
                        fs::create_dir(&backup_root)
                            .map_err(|e| format!("create export backup staging directory: {e}"))?;
                        // No output identity yet: this phase never touches the
                        // output, and the directory may not even exist until
                        // `commit_export` materializes it and pins it into the
                        // `committing` claim.
                        write_stage_claim(&root, output_key, None, StagePhase::Staging, &[], &[])
                    })();
                    if let Err(error) = prepared {
                        let _ = fs::remove_dir_all(&root);
                        return Err(error);
                    }
                    return Ok(Self {
                        root,
                        new_root,
                        backup_root,
                        output_key: output_key.to_string(),
                        armed: true,
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

    /// Make the staged tree durable and flip the claim to `committing`. After
    /// this returns, any interruption is repaired by rolling the publish
    /// forward from `new/`; before it returns, any interruption is repaired by
    /// deleting the staging root.
    fn arm_commit(
        &self,
        output_identity: OutputIdentity,
        install_paths: &[PathBuf],
        backup_paths: &[PathBuf],
    ) -> Result<(), String> {
        sync_dir_tree(&self.new_root)?;
        write_stage_claim(
            &self.root,
            &self.output_key,
            Some(output_identity),
            StagePhase::Committing,
            install_paths,
            backup_paths,
        )
    }

    /// Record the intent to restore the prior state before the restore starts,
    /// so an interruption mid-restore is completed by the next run rather than
    /// stranding a half-restored output.
    fn arm_rollback(
        &self,
        output_identity: OutputIdentity,
        install_paths: &[PathBuf],
        backup_paths: &[PathBuf],
    ) -> Result<(), String> {
        write_stage_claim(
            &self.root,
            &self.output_key,
            Some(output_identity),
            StagePhase::RollingBack,
            install_paths,
            backup_paths,
        )
    }

    /// Leave the staging root on disk, exactly as a killed process would.
    fn leak(&mut self) {
        self.armed = false;
    }

    /// Retire a fully published staging root. The claim is unlinked *before*
    /// the tree, because a claim-less directory is inert residue while a
    /// surviving `committing` claim would let a later run replay a publish that
    /// already happened.
    fn finish(&mut self) {
        self.armed = false;
        let _ = fs::remove_file(self.root.join(EXPORT_STAGE_CLAIM_PATH));
        sync_dir(&self.root);
        let _ = fs::remove_dir_all(&self.root);
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
        if self.armed {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn stage_export(output: &Path, plan: &ExportPlan, output_key: &str) -> Result<ExportStage, String> {
    let stage = ExportStage::create(output, output_key)?;
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

/// Recovery variant of [`ensure_output_parent`].
///
/// Recovery only ever rolls forward or completes a restore; neither is undone
/// in-process, so there is no rollback to hand a created-directory list to and
/// the list is deliberately discarded.
fn ensure_recovered_output_parent(output: &Path, relative: &Path) -> Result<(), String> {
    ensure_output_parent(output, relative, &mut Vec::new())
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

/// Roll an interrupted publish forward to completion.
///
/// The staged tree is written in full *before* the claim flips to
/// `committing`, so at any interruption point every not-yet-published file is
/// still sitting in `new/`. Publishing is therefore idempotent: a path missing
/// from `new/` has already been installed, and a path still present is renamed
/// into place (over a prior version that the interrupted run had not yet moved
/// aside, if the interruption happened during the backup phase).
///
/// Rolling forward — rather than back — is what makes this restartable without
/// a second journal: it needs no record of *which* individual steps completed.
fn finish_interrupted_commit(output: &Path, root: &Path, claim: &StageClaim) -> Result<(), String> {
    let new_root = root.join("new");
    for relative in &claim.install_paths {
        let staged = new_root.join(relative);
        let Some(staged_metadata) = path_metadata(&staged)? else {
            // A `rename` is atomic, so a file a killed process left unpublished
            // is still in `new/`: absent from both sides means the staged copy
            // was lost by something below this layer (an unflushed directory
            // entry across a power cut). Say so instead of publishing a
            // manifest that references a file which no longer exists.
            if path_metadata(&output.join(relative))?.is_none() {
                return Err(format!(
                    "Wiki export recovery cannot complete: '{}' is missing from both the staging tree and the output",
                    relative.display()
                ));
            }
            // Already published by the interrupted run.
            continue;
        };
        if staged_metadata.file_type().is_symlink() || !staged_metadata.is_file() {
            return Err(format!(
                "Refusing Wiki export recovery: staged file '{}' is not a regular non-symlink file",
                staged.display()
            ));
        }
        let destination = output.join(relative);
        if let Some(metadata) = path_metadata(&destination)? {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "Refusing Wiki export recovery: output '{}' is not a regular non-symlink file",
                    destination.display()
                ));
            }
        }
        ensure_recovered_output_parent(output, relative)?;
        fs::rename(&staged, &destination).map_err(|e| {
            format!(
                "recover staged managed file '{}' -> '{}': {e}",
                staged.display(),
                destination.display()
            )
        })?;
    }

    // Prior-manifest-owned files the interrupted run had decided to drop. Ones
    // it already moved into `backup/` die with the staging root; ones it had
    // not reached yet are still in the output and are dropped here, so the
    // recovered output matches the manifest that was just published.
    let install_set = claim.install_paths.iter().collect::<BTreeSet<_>>();
    for relative in &claim.backup_paths {
        if install_set.contains(relative) {
            continue;
        }
        let stale = output.join(relative);
        let Some(metadata) = path_metadata(&stale)? else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        fs::remove_file(&stale)
            .map_err(|e| format!("drop stale managed file '{}': {e}", stale.display()))?;
    }
    Ok(())
}

/// Complete an interrupted restore-to-prior-state.
///
/// Mirror image of [`finish_interrupted_commit`], and idempotent for the same
/// reason: a prior version still sitting in `backup/` has not been restored
/// yet, and one that is gone from `backup/` already has been.
fn finish_interrupted_rollback(
    output: &Path,
    root: &Path,
    claim: &StageClaim,
) -> Result<(), String> {
    let backup_root = root.join("backup");
    for relative in &claim.backup_paths {
        let backup = backup_root.join(relative);
        let Some(backup_metadata) = path_metadata(&backup)? else {
            continue;
        };
        if backup_metadata.file_type().is_symlink() || !backup_metadata.is_file() {
            return Err(format!(
                "Refusing Wiki export recovery: backup file '{}' is not a regular non-symlink file",
                backup.display()
            ));
        }
        let destination = output.join(relative);
        if let Some(metadata) = path_metadata(&destination)? {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "Refusing Wiki export recovery: output '{}' is not a regular non-symlink file",
                    destination.display()
                ));
            }
        }
        ensure_recovered_output_parent(output, relative)?;
        fs::rename(&backup, &destination).map_err(|e| {
            format!(
                "restore prior managed file '{}' -> '{}': {e}",
                backup.display(),
                destination.display()
            )
        })?;
    }

    // Files the abandoned publish would have added but the prior state never
    // had. Anything the prior state did have was just restored above.
    let backup_set = claim.backup_paths.iter().collect::<BTreeSet<_>>();
    for relative in &claim.install_paths {
        if backup_set.contains(relative) {
            continue;
        }
        let added = output.join(relative);
        let Some(metadata) = path_metadata(&added)? else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        fs::remove_file(&added)
            .map_err(|e| format!("remove abandoned managed file '{}': {e}", added.display()))?;
    }
    Ok(())
}

/// Reclaim staging roots left by a killed export of *this* output.
///
/// Runs under the per-output lock, before any ownership validation, so a torn
/// publish is repaired before `parse_existing_manifest` can reject it. Only
/// directories carrying our own claim for our own output key — and, for the
/// phases that write into the output, for the very same output inode — are
/// touched. Anything else (a foreign directory, another output's staging root,
/// a claim-less residue that a concurrent run may still be creating, a claim
/// whose output was replaced since the crash) is left exactly as found, so the
/// reserved-artifact refusal still protects output directories this tool did
/// not produce.
fn recover_interrupted_exports(output: &Path, output_key: &str) -> Result<(), String> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let Some(parent_metadata) = path_metadata(parent)? else {
        return Ok(());
    };
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Ok(());
    }
    let mut roots = Vec::new();
    for entry in fs::read_dir(parent)
        .map_err(|e| format!("scan export staging parent '{}': {e}", parent.display()))?
    {
        let entry = entry.map_err(|e| {
            format!(
                "read export staging parent entry under '{}': {e}",
                parent.display()
            )
        })?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(EXPORT_STAGE_PREFIX))
        {
            roots.push(entry.path());
        }
    }
    roots.sort();
    for root in roots {
        recover_stage_root(output, output_key, &root)?;
    }
    Ok(())
}

/// Re-stat the output and report whether it is still the directory `claim` was
/// written against. A claim with no pinned identity can never authorize a
/// mutation, so it answers `false`.
fn claim_matches_current_output(output: &Path, claim: &StageClaim) -> Result<bool, String> {
    let Some(identity) = claim.output_identity else {
        return Ok(false);
    };
    match output_directory_identity(output)? {
        OutputDirectory::Directory(current) => Ok(current == identity),
        OutputDirectory::NotADirectory | OutputDirectory::Missing => Ok(false),
    }
}

fn recover_stage_root(output: &Path, output_key: &str, root: &Path) -> Result<(), String> {
    let Some(metadata) = path_metadata(root)? else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }
    // A staging root we did not create is not evidence about our output.
    // SAFETY: `geteuid()` reads the caller's effective uid; it takes no
    // pointers, cannot fail, and has no side effects.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Ok(());
    }
    let Some(claim) = read_stage_claim(root) else {
        return Ok(());
    };
    if claim.output_key != output_key {
        return Ok(());
    }
    // Every phase that mutates the output must first prove that the directory
    // sitting at our canonical path *is* the directory the interrupted run was
    // writing into. If the output was deleted and something else took the path,
    // the claim describes a directory that no longer exists: leave both the
    // stranger's directory and the claim untouched and let the ownership check
    // downstream refuse the output, exactly as it would with no claim at all.
    if claim.phase != StagePhase::Staging && !claim_matches_current_output(output, &claim)? {
        return Ok(());
    }
    match claim.phase {
        // Nothing in the output was touched yet; the staging root is residue.
        StagePhase::Staging => {}
        StagePhase::Committing => finish_interrupted_commit(output, root, &claim)?,
        StagePhase::RollingBack => finish_interrupted_rollback(output, root, &claim)?,
    }
    fs::remove_dir_all(root).map_err(|e| {
        format!(
            "remove recovered export staging directory '{}': {e}",
            root.display()
        )
    })
}

enum OutputDirectory {
    Directory(OutputIdentity),
    NotADirectory,
    Missing,
}

/// Stat the output directory, following symlinks.
///
/// Symlinks are followed deliberately: the identity worth pinning is the
/// directory the publish actually writes into, and an output root that is a
/// symlink to a real directory has always been accepted here. Repointing that
/// symlink elsewhere changes the identity, which is exactly what recovery needs
/// to notice.
fn output_directory_identity(output: &Path) -> Result<OutputDirectory, String> {
    match fs::metadata(output) {
        Ok(metadata) if metadata.is_dir() => {
            Ok(OutputDirectory::Directory(OutputIdentity::of(&metadata)))
        }
        Ok(_) => Ok(OutputDirectory::NotADirectory),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OutputDirectory::Missing),
        Err(error) => Err(format!(
            "inspect export output '{}': {error}",
            output.display()
        )),
    }
}

/// Materialize the output directory and read back the physical identity this
/// publish is about to write into.
///
/// The directory is created here rather than lazily inside the install loop
/// because the commit claim must be armed *before* the first output mutation
/// and must carry the identity it is claiming: there is nothing to pin until
/// the directory exists. The cost is that the output root is no longer part of
/// the commit's created-directory list, so a crash between this call and the
/// claim flip — or a rollback of the publish — leaves an empty directory
/// behind. That is inert: the next run treats it exactly like an empty output
/// an operator created by hand.
fn ensure_output_identity(output: &Path) -> Result<OutputIdentity, String> {
    match output_directory_identity(output)? {
        OutputDirectory::Directory(identity) => return Ok(identity),
        OutputDirectory::NotADirectory => {
            return Err(format!(
                "Refusing Wiki export: output '{}' is not a directory",
                output.display()
            ));
        }
        OutputDirectory::Missing => {}
    }
    fs::create_dir_all(output)
        .map_err(|e| format!("create export output '{}': {e}", output.display()))?;
    match output_directory_identity(output)? {
        OutputDirectory::Directory(identity) => Ok(identity),
        _ => Err(format!(
            "export output '{}' is not a directory immediately after creating it",
            output.display()
        )),
    }
}

fn commit_export(
    output: &Path,
    plan: &ExportPlan,
    existing: &ExistingExport,
    stage: &mut ExportStage,
    hook: &dyn ExportTestHook,
) -> Result<(), String> {
    let mut install_paths = plan
        .entries
        .iter()
        .map(|planned| planned.output_path.clone())
        .collect::<Vec<_>>();
    install_paths.push(PathBuf::from(EXPORT_INDEX_PATH));
    install_paths.push(PathBuf::from(EXPORT_MANIFEST_PATH));
    let backup_paths = existing.managed_paths.iter().cloned().collect::<Vec<_>>();
    let output_identity = ensure_output_identity(output)?;
    // Arm before the first output mutation: from here on, every interruption is
    // repairable by rolling forward from the staged tree.
    stage.arm_commit(output_identity, &install_paths, &backup_paths)?;

    let mut moved_old = Vec::new();
    let mut installed = Vec::new();
    let mut created_dirs = Vec::new();
    let commit_result: Result<(), String> = (|| {
        for relative in &existing.managed_paths {
            hook.before_backup(moved_old.len(), relative)?;
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

        for relative in &install_paths {
            hook.before_install(installed.len(), relative)?;
            ensure_output_parent(output, relative, &mut created_dirs)?;
            let staged = stage.new_root.join(relative);
            let destination = output.join(relative);
            fs::rename(&staged, &destination).map_err(|e| {
                format!(
                    "install staged managed file '{}' -> '{}': {e}",
                    staged.display(),
                    destination.display()
                )
            })?;
            installed.push(relative.clone());
        }
        Ok(())
    })();

    if let Err(error) = commit_result {
        if hook.simulates_process_death() {
            // Simulated SIGKILL: skip rollback and keep the armed staging root
            // on disk, so the next run has to reach the same consistent state
            // through recovery alone.
            stage.leak();
            return Err(format!(
                "Wiki export test hook simulated process death: {error}"
            ));
        }
        if let Err(claim_error) = stage.arm_rollback(output_identity, &install_paths, &backup_paths)
        {
            // The rollback intent could not be recorded, so do not start one:
            // leave the commit claim armed and let the next run roll the
            // publish forward to a complete new version instead.
            stage.leak();
            return Err(format!(
                "Wiki export transaction failed: {error}; rollback not attempted ({claim_error}); the next export run completes the publish"
            ));
        }
        let rollback = rollback_export(output, stage, &installed, &moved_old, &created_dirs);
        return match rollback {
            Ok(()) => {
                stage.finish();
                Err(format!(
                    "Wiki export transaction failed and prior managed state was restored: {error}"
                ))
            }
            Err(rollback) => {
                // Keep the rolling-back claim on disk: the restore is now the
                // next run's job, not a human's.
                stage.leak();
                Err(format!(
                    "Wiki export transaction failed: {error}; ROLLBACK INCOMPLETE: {rollback}; the next export run completes the restore"
                ))
            }
        };
    }
    Ok(())
}

fn obsidian_file_stem(entry: &MemoryEntry) -> String {
    let topic = entry.topic.trim();
    let raw = if !topic.is_empty() {
        topic
    } else {
        let summary = entry.summary.trim();
        if !summary.is_empty() {
            summary
        } else {
            entry.id.as_str()
        }
    };
    bounded_name(
        &sanitize_safe_path_name(raw),
        EXPORT_FILE_STEM_MAX_BYTES,
        "",
    )
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
    // One canonical key identifies this output for both the cooperative lock
    // and the staging claims, so a staging root is only ever reclaimed by a run
    // that holds the lock for the same physical directory.
    let output_key = canonical_output_lock_key(output)?;
    let _lock = acquire_export_lock(server, &output_key)?;
    hook.after_lock_acquired()?;
    // Repair anything a killed run left behind before reading the output's
    // ownership state: a torn publish is our own artifact, not a foreign
    // directory, and must not turn into a permanent manual-cleanup refusal.
    recover_interrupted_exports(output, &output_key)?;
    let plan = WikiReadPlan::from_project(Some(project))?;
    let entries = list_wiki_entries_for_plan(server, &plan, "/wiki", 100_000)?;

    // The complete plan, rendered entry content, index, and manifest are all
    // built before the first output-directory mutation. This is the boundary
    // that prevents a late collision or manifest error from leaving a partial
    // export behind.
    let export_plan = build_export_plan(entries)?;
    let existing = validate_output_targets(output, &export_plan)?;
    let mut stage = stage_export(output, &export_plan, &output_key)?;
    if let Err(error) = hook.after_stage_ready() {
        if hook.simulates_process_death() {
            stage.leak();
            return Err(format!(
                "Wiki export test hook simulated process death: {error}"
            ));
        }
        return Err(error);
    }
    commit_export(output, &export_plan, &existing, &mut stage, hook)?;
    stage.finish();

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
