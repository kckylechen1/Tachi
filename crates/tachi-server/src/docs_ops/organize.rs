use super::classify::classify_and_extract_metadata;
use super::frontmatter::{parse_frontmatter, serialize_frontmatter, Frontmatter};
use super::paths::{is_archive_dir, is_markdown_file};
use super::tasks::sync_tasks_in_content;
use crate::server_state::MemoryServer;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhysicalIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    canonical: PathBuf,
}

impl PhysicalIdentity {
    fn capture(path: &Path, expected_kind: &str) -> Result<(Self, fs::Metadata), String> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            format!(
                "Refusing Wiki organize: protected invariant: inspect {expected_kind} '{}': {error}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: {expected_kind} '{}' is a symlink",
                path.display()
            ));
        }
        let canonical = path.canonicalize().map_err(|error| {
            format!(
                "Refusing Wiki organize: protected invariant: canonicalize {expected_kind} '{}': {error}",
                path.display()
            )
        })?;
        let identity = Self {
            #[cfg(unix)]
            dev: metadata.dev(),
            #[cfg(unix)]
            ino: metadata.ino(),
            canonical,
        };
        Ok((identity, metadata))
    }

    fn matches(&self, actual: &Self) -> bool {
        #[cfg(unix)]
        {
            self.dev == actual.dev && self.ino == actual.ino
        }
        #[cfg(not(unix))]
        {
            self.canonical == actual.canonical
        }
    }
}

#[derive(Debug, Clone)]
struct AuthorizedDocs {
    worktree_root: PathBuf,
    docs_root: PathBuf,
    requested_root: PathBuf,
    worktree_identity: PhysicalIdentity,
    docs_identity: PhysicalIdentity,
    requested_identity: PhysicalIdentity,
}

// These checks bind every planned object to its physical identity and close
// deterministic path-swap windows, while O_NOFOLLOW protects the final
// component opened on Unix. The remaining hostile-process limit is that the
// standard-library path operations below are not one dirfd-relative kernel
// transaction: another process with write access can still race between a
// final validation and the following syscall. The docs lock serializes
// cooperating Tachi applies; it is not claimed as protection from that OS-level
// nanosecond race.

impl AuthorizedDocs {
    fn bind(dir_path: &str) -> Result<Self, String> {
        let requested = Path::new(dir_path);
        if !requested.is_absolute()
            || requested
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: dir_path '{}' must be an absolute path without '..' inside the current worktree's docs subtree",
                dir_path
            ));
        }

        let current_dir = std::env::current_dir()
            .map_err(|error| format!("cannot determine current worktree path: {error}"))?;
        let worktree_root = find_worktree_root_from(&current_dir)?;
        let (worktree_identity, worktree_metadata) =
            PhysicalIdentity::capture(&worktree_root, "worktree root")?;
        if !worktree_metadata.is_dir() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: worktree root '{}' is not a directory",
                worktree_root.display()
            ));
        }

        let docs_path = worktree_root.join("docs");
        let (docs_identity, docs_metadata) = PhysicalIdentity::capture(&docs_path, "docs root")?;
        if !docs_metadata.is_dir()
            || !docs_identity
                .canonical
                .starts_with(&worktree_identity.canonical)
        {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: canonical docs directory '{}' is not a directory inside current worktree '{}'",
                docs_identity.canonical.display(),
                worktree_identity.canonical.display()
            ));
        }

        reject_symlink_components(requested, &docs_identity.canonical)?;
        let requested_canonical = requested.canonicalize().map_err(|error| {
            format!(
                "Refusing Wiki organize: protected invariant: cannot canonicalize requested root '{}': {error}",
                dir_path
            )
        })?;
        if !requested_canonical.starts_with(&docs_identity.canonical) {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: root '{}' must resolve inside canonical docs subtree '{}' (repo root, home, temp, and arbitrary outside roots are not eligible)",
                dir_path,
                docs_identity.canonical.display()
            ));
        }
        let (requested_identity, requested_metadata) =
            PhysicalIdentity::capture(&requested_canonical, "requested organize root")?;
        if !requested_metadata.is_dir() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: requested root '{}' is not a directory",
                dir_path
            ));
        }

        let authorized = Self {
            worktree_root: worktree_identity.canonical.clone(),
            docs_root: docs_identity.canonical.clone(),
            requested_root: requested_identity.canonical.clone(),
            worktree_identity,
            docs_identity,
            requested_identity,
        };
        authorized.revalidate_roots()?;
        Ok(authorized)
    }

    fn revalidate_roots(&self) -> Result<(), String> {
        self.revalidate_object(
            &self.worktree_root,
            &self.worktree_identity,
            true,
            "worktree root",
        )?;
        self.revalidate_object(&self.docs_root, &self.docs_identity, true, "docs root")?;
        self.revalidate_object(
            &self.requested_root,
            &self.requested_identity,
            true,
            "requested organize root",
        )?;
        if !self.docs_root.starts_with(&self.worktree_root)
            || !self.requested_root.starts_with(&self.docs_root)
        {
            return Err(
                "Refusing Wiki organize: protected invariant: authorized roots are no longer canonically contained"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn revalidate_object(
        &self,
        path: &Path,
        expected: &PhysicalIdentity,
        require_dir: bool,
        label: &str,
    ) -> Result<fs::Metadata, String> {
        if path != self.worktree_root {
            self.validate_contained_path(path)?;
        }
        let (actual, metadata) = PhysicalIdentity::capture(path, label)?;
        if !actual.matches(expected) {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: {label} '{}' changed physical identity",
                path.display()
            ));
        }
        if require_dir && !metadata.is_dir() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: {label} '{}' is no longer a directory",
                path.display()
            ));
        }
        if !require_dir && !metadata.is_file() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: {label} '{}' is no longer a regular file",
                path.display()
            ));
        }
        Ok(metadata)
    }

    fn validate_contained_path(&self, path: &Path) -> Result<(), String> {
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
            || !path.starts_with(&self.docs_root)
        {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: path '{}' is outside canonical docs subtree '{}'",
                path.display(),
                self.docs_root.display()
            ));
        }
        reject_symlink_components(path, &self.docs_root)?;

        let mut existing = path.to_path_buf();
        while !existing.exists() {
            if !existing.pop() {
                return Err(format!(
                    "Refusing Wiki organize: protected invariant: no existing ancestor for '{}',",
                    path.display()
                ));
            }
        }
        let canonical_existing = existing.canonicalize().map_err(|error| {
            format!(
                "Refusing Wiki organize: protected invariant: canonicalize existing ancestor '{}': {error}",
                existing.display()
            )
        })?;
        if !canonical_existing.starts_with(&self.docs_root)
            || !canonical_existing.starts_with(&self.worktree_root)
        {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: path '{}' resolves outside authorized docs subtree",
                path.display()
            ));
        }
        Ok(())
    }

    fn safe_join(&self, relative: &str) -> Result<PathBuf, String> {
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: destination '{}' is not a safe relative path",
                relative
            ));
        }
        let path = self.requested_root.join(relative_path);
        self.validate_contained_path(&path)?;
        Ok(path)
    }

    fn optional_file(
        &self,
        path: &Path,
    ) -> Result<Option<(PhysicalIdentity, fs::Metadata)>, String> {
        self.revalidate_roots()?;
        self.validate_contained_path(path)?;
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(format!(
                        "Refusing Wiki organize: protected invariant: expected regular non-symlink file '{}',",
                        path.display()
                    ));
                }
                let canonical = path.canonicalize().map_err(|error| {
                    format!(
                        "Refusing Wiki organize: protected invariant: canonicalize file '{}': {error}",
                        path.display()
                    )
                })?;
                let identity = PhysicalIdentity {
                    #[cfg(unix)]
                    dev: metadata.dev(),
                    #[cfg(unix)]
                    ino: metadata.ino(),
                    canonical,
                };
                if !identity.canonical.starts_with(&self.docs_root) {
                    return Err(format!(
                        "Refusing Wiki organize: protected invariant: file '{}' resolves outside docs",
                        path.display()
                    ));
                }
                Ok(Some((identity, metadata)))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "Refusing Wiki organize: protected invariant: inspect file '{}': {error}",
                path.display()
            )),
        }
    }

    fn read_text(&self, path: &Path, expected: &PhysicalIdentity) -> Result<String, String> {
        self.revalidate_roots()?;
        self.revalidate_object(path, expected, false, "planned source file")?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let mut file = options.open(path).map_err(|error| {
            format!(
                "Refusing Wiki organize: protected invariant: open planned source '{}': {error}",
                path.display()
            )
        })?;
        let file_metadata = file.metadata().map_err(|error| {
            format!(
                "Refusing Wiki organize: protected invariant: stat planned source '{}': {error}",
                path.display()
            )
        })?;
        #[cfg(unix)]
        let file_identity = PhysicalIdentity {
            dev: file_metadata.dev(),
            ino: file_metadata.ino(),
            canonical: path.to_path_buf(),
        };
        #[cfg(not(unix))]
        let file_identity = PhysicalIdentity {
            canonical: path
                .canonicalize()
                .map_err(|error| format!("canonicalize planned source: {error}"))?,
        };
        if !file_identity.matches(expected) {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: planned source '{}' changed before read",
                path.display()
            ));
        }
        self.revalidate_object(path, expected, false, "planned source file")?;
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|error| format!("Failed to read file {}: {error}", path.display()))?;
        self.revalidate_object(path, expected, false, "planned source file")?;
        Ok(content)
    }

    fn ensure_directory(&self, path: &Path) -> Result<PhysicalIdentity, String> {
        self.revalidate_roots()?;
        self.validate_contained_path(path)?;
        if let Some((identity, metadata)) = self.optional_directory(path)? {
            if !metadata.is_dir() {
                return Err(format!(
                    "Refusing Wiki organize: protected invariant: '{}' is not a directory",
                    path.display()
                ));
            }
            return Ok(identity);
        }
        let parent = path.parent().ok_or_else(|| {
            format!(
                "Refusing Wiki organize: protected invariant: directory '{}' has no parent",
                path.display()
            )
        })?;
        let parent_identity = self.ensure_directory(parent)?;
        self.revalidate_roots()?;
        self.revalidate_object(parent, &parent_identity, true, "destination parent")?;
        fs::create_dir(path)
            .map_err(|error| format!("Failed to create directory '{}': {error}", path.display()))?;
        self.revalidate_roots()?;
        let (identity, metadata) = PhysicalIdentity::capture(path, "created directory")?;
        if !metadata.is_dir() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: created path '{}' is not a directory",
                path.display()
            ));
        }
        if !identity.canonical.starts_with(&self.docs_root) {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: created directory '{}' escapes docs",
                path.display()
            ));
        }
        Ok(identity)
    }

    fn optional_directory(
        &self,
        path: &Path,
    ) -> Result<Option<(PhysicalIdentity, fs::Metadata)>, String> {
        self.revalidate_roots()?;
        self.validate_contained_path(path)?;
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(format!(
                        "Refusing Wiki organize: protected invariant: directory '{}' is a symlink",
                        path.display()
                    ));
                }
                if !metadata.is_dir() {
                    return Ok(Some((
                        PhysicalIdentity::capture(path, "directory")?.0,
                        metadata,
                    )));
                }
                let (identity, _) = PhysicalIdentity::capture(path, "directory")?;
                Ok(Some((identity, metadata)))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "Refusing Wiki organize: protected invariant: inspect directory '{}': {error}",
                path.display()
            )),
        }
    }

    fn ensure_existing_directory(&self, path: &Path) -> Result<PhysicalIdentity, String> {
        self.revalidate_roots()?;
        self.validate_contained_path(path)?;
        let (identity, metadata) = PhysicalIdentity::capture(path, "directory")?;
        if !metadata.is_dir() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: '{}' is not a directory",
                path.display()
            ));
        }
        Ok(identity)
    }

    fn read_directory(
        &self,
        path: &Path,
        expected: &PhysicalIdentity,
    ) -> Result<fs::ReadDir, String> {
        self.revalidate_roots()?;
        self.revalidate_object(path, expected, true, "queued docs directory")?;
        let entries = fs::read_dir(path)
            .map_err(|error| format!("Failed to read dir {}: {error}", path.display()))?;
        self.revalidate_object(path, expected, true, "queued docs directory")?;
        Ok(entries)
    }

    fn write_new_file(&self, path: &Path, content: &str) -> Result<PhysicalIdentity, String> {
        self.revalidate_roots()?;
        self.validate_contained_path(path)?;
        let parent = path.parent().ok_or_else(|| {
            format!(
                "Refusing Wiki organize: protected invariant: '{}' has no parent",
                path.display()
            )
        })?;
        let parent_identity = self.ensure_directory(parent)?;
        if self.optional_file(path)?.is_some() || fs::symlink_metadata(path).is_ok() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: refusing to overwrite existing destination '{}',",
                path.display()
            ));
        }
        self.revalidate_object(parent, &parent_identity, true, "destination parent")?;
        run_organize_test_hook(OrganizeTestPoint::DestinationParentReady, parent);
        self.revalidate_roots()?;
        self.revalidate_object(parent, &parent_identity, true, "destination parent")?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let mut file = options
            .open(path)
            .map_err(|error| format!("Failed to create file '{}': {error}", path.display()))?;
        if let Err(error) = file
            .write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
        {
            drop(file);
            let _ = self.remove_created_file(path);
            return Err(format!(
                "Failed to write file '{}': {error}",
                path.display()
            ));
        }
        self.revalidate_roots()?;
        self.revalidate_object(parent, &parent_identity, true, "destination parent")?;
        let (identity, metadata) = PhysicalIdentity::capture(path, "created file")?;
        if !metadata.is_file() || !identity.canonical.starts_with(&self.docs_root) {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: created file '{}' is not authorized",
                path.display()
            ));
        }
        Ok(identity)
    }

    fn write_existing_file(
        &self,
        path: &Path,
        expected: &PhysicalIdentity,
        content: &str,
    ) -> Result<(), String> {
        self.revalidate_roots()?;
        self.revalidate_object(path, expected, false, "file before update")?;
        let parent = path.parent().ok_or_else(|| {
            format!(
                "Refusing Wiki organize: protected invariant: '{}' has no parent",
                path.display()
            )
        })?;
        let parent_identity = self.ensure_existing_directory(parent)?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("file '{}' has no valid name", path.display()))?;
        let temporary = parent.join(format!(
            ".{filename}.tachi-organize-{}.tmp",
            uuid::Uuid::new_v4()
        ));
        self.validate_contained_path(&temporary)?;
        self.revalidate_roots()?;
        self.revalidate_object(parent, &parent_identity, true, "destination parent")?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let mut file = options.open(&temporary).map_err(|error| {
            format!(
                "Failed to create temporary file '{}': {error}",
                temporary.display()
            )
        })?;
        let write_result = file
            .write_all(content.as_bytes())
            .and_then(|_| file.sync_all());
        drop(file);
        if let Err(error) = write_result {
            let _ = self.remove_created_file(&temporary);
            return Err(format!(
                "Failed to write temporary file '{}': {error}",
                temporary.display()
            ));
        }
        let (temporary_identity, temporary_metadata) =
            PhysicalIdentity::capture(&temporary, "temporary update file")?;
        if !temporary_metadata.is_file()
            || !temporary_identity.canonical.starts_with(&self.docs_root)
        {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: temporary update file '{}' is not authorized",
                temporary.display()
            ));
        }
        self.revalidate_roots()?;
        self.revalidate_object(parent, &parent_identity, true, "destination parent")?;
        self.revalidate_object(path, expected, false, "file before rename")?;
        self.revalidate_object(
            &temporary,
            &temporary_identity,
            false,
            "temporary update file",
        )?;
        run_organize_test_hook(OrganizeTestPoint::DestinationParentReady, parent);
        self.revalidate_roots()?;
        self.revalidate_object(parent, &parent_identity, true, "destination parent")?;
        self.revalidate_object(path, expected, false, "file before rename")?;
        self.revalidate_object(
            &temporary,
            &temporary_identity,
            false,
            "temporary update file",
        )?;
        fs::rename(&temporary, path).map_err(|error| {
            let _ = self.remove_created_file(&temporary);
            format!("Failed to replace file '{}': {error}", path.display())
        })?;
        self.revalidate_roots()?;
        self.revalidate_object(parent, &parent_identity, true, "destination parent")?;
        let (updated, metadata) = PhysicalIdentity::capture(path, "updated file")?;
        if !metadata.is_file() || !updated.canonical.starts_with(&self.docs_root) {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: updated file '{}' is not authorized",
                path.display()
            ));
        }
        Ok(())
    }

    fn remove_file(&self, path: &Path, expected: &PhysicalIdentity) -> Result<(), String> {
        self.revalidate_roots()?;
        self.revalidate_object(path, expected, false, "source before removal")?;
        let parent = path.parent().ok_or_else(|| {
            format!(
                "Refusing Wiki organize: protected invariant: '{}' has no parent",
                path.display()
            )
        })?;
        let parent_identity = self.ensure_existing_directory(parent)?;
        self.revalidate_object(parent, &parent_identity, true, "source parent")?;
        self.revalidate_object(path, expected, false, "source before removal")?;
        fs::remove_file(path)
            .map_err(|error| format!("Failed to remove source file {}: {error}", path.display()))?;
        self.revalidate_roots()?;
        self.revalidate_object(parent, &parent_identity, true, "source parent")?;
        if fs::symlink_metadata(path).is_ok() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: source '{}' remained after removal",
                path.display()
            ));
        }
        Ok(())
    }

    fn remove_created_file(&self, path: &Path) -> Result<(), String> {
        let Some((identity, _)) = self.optional_file(path)? else {
            return Ok(());
        };
        self.remove_file(path, &identity)
    }

    fn rename_file(
        &self,
        source: &Path,
        source_identity: &PhysicalIdentity,
        destination: &Path,
        destination_parent: &PhysicalIdentity,
    ) -> Result<(), String> {
        self.revalidate_roots()?;
        self.revalidate_object(source, source_identity, false, "rename source")?;
        let parent = destination.parent().ok_or_else(|| {
            format!(
                "Refusing Wiki organize: protected invariant: rename destination '{}' has no parent",
                destination.display()
            )
        })?;
        self.revalidate_object(
            parent,
            destination_parent,
            true,
            "destination parent before rename",
        )?;
        if self.optional_file(destination)?.is_some() || fs::symlink_metadata(destination).is_ok() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: rename destination '{}' is not empty",
                destination.display()
            ));
        }
        run_organize_test_hook(OrganizeTestPoint::DestinationParentReady, parent);
        self.revalidate_roots()?;
        self.revalidate_object(source, source_identity, false, "rename source")?;
        self.revalidate_object(
            parent,
            destination_parent,
            true,
            "destination parent before rename",
        )?;
        fs::rename(source, destination).map_err(|error| {
            format!(
                "Failed to rename '{}' -> '{}': {error}",
                source.display(),
                destination.display()
            )
        })?;
        self.revalidate_object(destination, source_identity, false, "renamed destination")?;
        Ok(())
    }

    fn unique_archive_target(
        &self,
        archive_dir: &Path,
        stem: &str,
    ) -> Result<(PathBuf, String), String> {
        let directory_identity = self.ensure_existing_directory(archive_dir)?;
        let mut suffix = 0usize;
        loop {
            self.revalidate_roots()?;
            self.revalidate_object(archive_dir, &directory_identity, true, "archive directory")?;
            let filename = if suffix == 0 {
                format!("{stem}.md")
            } else {
                format!("{stem}.{suffix}.md")
            };
            let candidate = archive_dir.join(&filename);
            self.validate_contained_path(&candidate)?;
            match fs::symlink_metadata(&candidate) {
                Ok(_) => {
                    suffix = suffix.checked_add(1).ok_or_else(|| {
                        "Refusing Wiki organize: protected invariant: archive suffix exhausted"
                            .to_string()
                    })?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.revalidate_object(
                        archive_dir,
                        &directory_identity,
                        true,
                        "archive directory",
                    )?;
                    return Ok((candidate, filename));
                }
                Err(error) => {
                    return Err(format!(
                        "Refusing Wiki organize: protected invariant: inspect archive target '{}': {error}",
                        candidate.display()
                    ));
                }
            }
        }
    }
}

fn find_worktree_root_from(start: &Path) -> Result<PathBuf, String> {
    let mut dir = start.canonicalize().map_err(|e| {
        format!(
            "cannot canonicalize current worktree path '{}': {e}",
            start.display()
        )
    })?;
    if !dir.is_dir() {
        dir.pop();
    }

    loop {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() || dot_git.is_file() {
            // Do not call utils::find_git_root_from here: linked worktrees have
            // a .git file whose common directory belongs to the primary
            // checkout, but Wiki organize must authorize this worktree.
            return Ok(dir);
        }
        if !dir.pop() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: current path '{}' is not inside a git worktree",
                start.display()
            ));
        }
    }
}

fn reject_symlink_components(path: &Path, canonical_docs_root: &Path) -> Result<(), String> {
    let Some(relative) = path.strip_prefix(canonical_docs_root).ok() else {
        return Ok(());
    };
    let mut cursor = canonical_docs_root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(name) => cursor.push(name),
            Component::CurDir => continue,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "Refusing Wiki organize: protected invariant: path '{}' contains an unsafe component",
                    path.display()
                ));
            }
        }
        let metadata = match fs::symlink_metadata(&cursor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "Refusing Wiki organize: protected invariant: inspect path component '{}': {error}",
                    cursor.display()
                ));
            }
        };
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: symlink component '{}' is not authorized",
                cursor.display()
            ));
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn symlink_components_escaping_docs(path: &Path, canonical_docs_root: &Path) -> Result<(), String> {
    // Inspect only the portion below the canonical docs root. System-level
    // aliases such as /var -> /private/var are harmless path spelling
    // details; the final canonical-root check below still catches any actual
    // escape from docs.
    let Some(relative) = path.strip_prefix(canonical_docs_root).ok() else {
        return Ok(());
    };
    let mut cursor = canonical_docs_root.to_path_buf();
    for component in relative.components() {
        cursor.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&cursor) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let canonical_target = cursor.canonicalize().map_err(|e| {
            format!(
                "Refusing Wiki organize: protected invariant: cannot resolve symlink component '{}': {e}",
                cursor.display()
            )
        })?;
        if !canonical_target.starts_with(canonical_docs_root) {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: symlink component '{}' escapes canonical docs subtree '{}' (target '{}')",
                cursor.display(),
                canonical_docs_root.display(),
                canonical_target.display()
            ));
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn canonical_organize_root(dir_path: &str) -> Result<PathBuf, String> {
    let requested = Path::new(dir_path);
    if !requested.is_absolute() {
        return Err(format!(
            "Refusing Wiki organize: protected invariant: dir_path '{}' must be an absolute path inside the current worktree's docs subtree",
            dir_path
        ));
    }

    let current_dir = std::env::current_dir()
        .map_err(|e| format!("cannot determine current worktree path: {e}"))?;
    let worktree_root = find_worktree_root_from(&current_dir)?;
    let docs_path = worktree_root.join("docs");
    let canonical_docs_root = docs_path.canonicalize().map_err(|e| {
        format!(
            "Refusing Wiki organize: protected invariant: current worktree '{}' has no usable canonical docs directory '{}': {e}",
            worktree_root.display(),
            docs_path.display()
        )
    })?;
    if !canonical_docs_root.is_dir() || !canonical_docs_root.starts_with(&worktree_root) {
        return Err(format!(
            "Refusing Wiki organize: protected invariant: canonical docs directory '{}' is not a directory inside current worktree '{}'",
            canonical_docs_root.display(),
            worktree_root.display()
        ));
    }

    symlink_components_escaping_docs(requested, &canonical_docs_root)?;
    let canonical_root = requested.canonicalize().map_err(|e| {
        format!(
            "Refusing Wiki organize: protected invariant: cannot canonicalize requested root '{}': {e}",
            dir_path
        )
    })?;
    if !canonical_root.is_dir() || !canonical_root.starts_with(&canonical_docs_root) {
        return Err(format!(
            "Refusing Wiki organize: protected invariant: root '{}' must resolve inside canonical docs subtree '{}' (repo root, home, temp, and arbitrary outside roots are not eligible)",
            dir_path,
            canonical_docs_root.display()
        ));
    }
    Ok(canonical_root)
}

#[cfg(unix)]
struct OrganizeApplyLock {
    _file: File,
}

#[cfg(unix)]
fn acquire_organize_apply_lock(authorized: &AuthorizedDocs) -> Result<OrganizeApplyLock, String> {
    authorized.revalidate_roots()?;
    let lock_path = authorized.docs_root.join(".tachi-organize.lock");
    authorized.validate_contained_path(&lock_path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options.open(&lock_path).map_err(|error| {
        format!(
            "Refusing Wiki organize: protected invariant: acquire docs apply lock '{}': {error}",
            lock_path.display()
        )
    })?;
    authorized.revalidate_roots()?;
    let path_metadata = fs::symlink_metadata(&lock_path).map_err(|error| {
        format!(
            "Refusing Wiki organize: protected invariant: inspect docs apply lock '{}': {error}",
            lock_path.display()
        )
    })?;
    let file_metadata = file.metadata().map_err(|error| {
        format!(
            "Refusing Wiki organize: protected invariant: stat docs apply lock '{}': {error}",
            lock_path.display()
        )
    })?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_file()
        || path_metadata.dev() != file_metadata.dev()
        || path_metadata.ino() != file_metadata.ino()
    {
        return Err(format!(
            "Refusing Wiki organize: protected invariant: docs apply lock '{}' is not the opened regular file",
            lock_path.display()
        ));
    }
    // SAFETY: the fd belongs to the live File retained in OrganizeApplyLock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(format!(
            "Refusing Wiki organize: docs apply lock '{}' is already held: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    authorized.revalidate_roots()?;
    Ok(OrganizeApplyLock { _file: file })
}

#[cfg(not(unix))]
fn acquire_organize_apply_lock(_authorized: &AuthorizedDocs) -> Result<(), String> {
    Err("Refusing Wiki organize: protected invariant: cooperative docs apply locking is unsupported on this platform".to_string())
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrganizeTestPoint {
    ApplyLockAcquired,
    DirectoryDequeued,
    DestinationParentReady,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
enum OrganizeTestPoint {
    ApplyLockAcquired,
    DirectoryDequeued,
    DestinationParentReady,
}

#[cfg(test)]
struct OrganizeTestHook {
    point: OrganizeTestPoint,
    path: PathBuf,
    action: Box<dyn FnOnce() + Send + 'static>,
}

#[cfg(test)]
fn organize_test_hook() -> &'static Mutex<Option<OrganizeTestHook>> {
    static HOOK: OnceLock<Mutex<Option<OrganizeTestHook>>> = OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
pub(crate) fn set_organize_test_hook(
    point: OrganizeTestPoint,
    path: PathBuf,
    action: Box<dyn FnOnce() + Send + 'static>,
) {
    let mut slot = organize_test_hook()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(slot.is_none(), "organize test hook already installed");
    *slot = Some(OrganizeTestHook {
        point,
        path,
        action,
    });
}

#[cfg(test)]
fn run_organize_test_hook(point: OrganizeTestPoint, path: &Path) {
    let action = {
        let mut slot = organize_test_hook()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot
            .as_ref()
            .is_some_and(|hook| hook.point == point && hook.path == path)
        {
            slot.take().map(|hook| hook.action)
        } else {
            None
        }
    };
    if let Some(action) = action {
        action();
    }
}

#[cfg(not(test))]
fn run_organize_test_hook(_point: OrganizeTestPoint, _path: &Path) {}

fn collect_markdown_files(
    authorized: &AuthorizedDocs,
    root: &Path,
    root_identity: PhysicalIdentity,
    whitelist: &[&str],
) -> Result<Vec<(PathBuf, PhysicalIdentity)>, String> {
    let mut queue = vec![(root.to_path_buf(), root_identity)];
    let mut markdown = Vec::new();
    while let Some((current_dir, current_identity)) = queue.pop() {
        authorized.revalidate_object(
            &current_dir,
            &current_identity,
            true,
            "queued docs directory",
        )?;
        run_organize_test_hook(OrganizeTestPoint::DirectoryDequeued, &current_dir);
        let entries = authorized.read_directory(&current_dir, &current_identity)?;
        for entry in entries {
            authorized.revalidate_object(
                &current_dir,
                &current_identity,
                true,
                "queued docs directory",
            )?;
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("Failed to inspect path {}: {error}", path.display()))?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            authorized.validate_contained_path(&path)?;
            if metadata.is_dir() {
                if is_archive_dir(&path) {
                    continue;
                }
                let (identity, _) = PhysicalIdentity::capture(&path, "queued docs directory")?;
                queue.push((path, identity));
            } else if metadata.is_file() && is_markdown_file(&path) {
                let filename = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if path.parent() == Some(root) && whitelist.contains(&filename) {
                    continue;
                }
                let (identity, _) = PhysicalIdentity::capture(&path, "planned source file")?;
                markdown.push((path, identity));
            }
        }
    }
    markdown.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(markdown)
}

/// 执行 docs 整理与索引生成的核心主流程
pub(crate) async fn handle_wiki_organize(
    server: &MemoryServer,
    dir_path: &str,
    dry_run: bool,
) -> Result<String, String> {
    let authorized = AuthorizedDocs::bind(dir_path)?;
    let canonical_root = authorized.requested_root.clone();
    #[cfg(unix)]
    let _apply_lock = if dry_run {
        None
    } else {
        Some(acquire_organize_apply_lock(&authorized)?)
    };
    #[cfg(not(unix))]
    let _apply_lock = if dry_run {
        None
    } else {
        Some(acquire_organize_apply_lock(&authorized)?)
    };
    if !dry_run {
        run_organize_test_hook(OrganizeTestPoint::ApplyLockAcquired, &authorized.docs_root);
        authorized.revalidate_roots()?;
    }

    // 白名单保护文件列表
    let whitelist = ["README.md", "INSTALL.md", "_index.md"];

    let mut moved_count = 0;
    let mut synced_count = 0;
    let mut log_messages = Vec::new();

    // 建立标准分类结构目录
    let standard_dirs = [
        "engineering/architecture",
        "engineering/devops",
        "engineering/code-review",
        "engineering/debugging",
        "product",
        "agent",
        "archive",
    ];
    for sub in &standard_dirs {
        let path = authorized.safe_join(sub)?;
        if dry_run {
            match authorized.optional_directory(&path)? {
                None => {
                    authorized.validate_contained_path(&path)?;
                    log_messages.push(format!(
                        "[dry-run] Would create standard directory '{}'",
                        sub
                    ));
                }
                Some((_, metadata)) if !metadata.is_dir() => {
                    return Err(format!(
                        "Refusing Wiki organize: protected invariant: standard path '{}' is not a directory",
                        path.display()
                    ));
                }
                Some(_) => {}
            }
            continue;
        }
        authorized
            .ensure_directory(&path)
            .map_err(|error| format!("Failed to create standard directory '{}': {error}", sub))?;
    }

    // 1. 递归扫描 Markdown 文件并收集需要处理的列表
    // 我们用广度优先/手动队列进行递归扫描以避免递归溢出且保持对符号链接等安全的处理
    let root_identity = authorized.ensure_existing_directory(&canonical_root)?;
    let md_paths = collect_markdown_files(&authorized, &canonical_root, root_identity, &whitelist)?;

    // 记录标准分类目录前缀
    let standard_subdirs = ["engineering/", "product/", "agent/"];

    // 2. 对扫描出的每个 Markdown 执行 Frontmatter 解析、分类和物理移动
    let mut invalidated_planned_paths = BTreeSet::new();
    for (path, source_identity) in md_paths {
        if invalidated_planned_paths.remove(&path) {
            continue;
        }
        authorized.revalidate_roots()?;
        authorized.revalidate_object(&path, &source_identity, false, "planned source file")?;
        let relative_path = path
            .strip_prefix(&canonical_root)
            .map_err(|e| format!("Strip prefix failed: {e}"))?;
        let relative_str = relative_path.to_string_lossy().replace('\\', "/");

        let content = authorized.read_text(&path, &source_identity)?;

        let (fm_opt, body) = parse_frontmatter(&content);

        // 检查 organize 逃生舱
        if let Some(ref fm) = fm_opt {
            if fm.organize == Some(false) {
                log_messages.push(format!(
                    "Skipped '{}': organize is explicitly set to false",
                    relative_str
                ));

                // Task sync on organize:false files is best-effort — errors are
                // logged but must not abort the remaining file scan.
                let (new_body, task_modified) = sync_tasks_in_content(server, body);
                if task_modified {
                    if dry_run {
                        log_messages.push(format!(
                            "[dry-run] Would sync task checkmarks in-place: '{}'",
                            relative_str
                        ));
                        synced_count += 1;
                        continue;
                    }
                    let new_content = if let Some(ref fm) = fm_opt {
                        format!("{}{}", serialize_frontmatter(fm), new_body)
                    } else {
                        new_body
                    };
                    if let Err(e) =
                        authorized.write_existing_file(&path, &source_identity, &new_content)
                    {
                        log_messages.push(format!(
                            "WARN: task sync write failed for '{}': {e}",
                            relative_str
                        ));
                    } else {
                        synced_count += 1;
                    }
                }
                continue;
            }
        }

        // 确定该文件当前是否已经在标准分类目录中
        let is_already_categorized = standard_subdirs
            .iter()
            .any(|sub| relative_str.starts_with(sub));

        // 提取或预测分类
        let (target_category_path, title, summary) = if let Some(ref fm) = fm_opt {
            if let Some(ref cat) = fm.category {
                let cat_rel = cat.strip_prefix("docs/").unwrap_or(cat);
                if standard_subdirs
                    .iter()
                    .any(|sub| cat_rel.starts_with(sub) || format!("{}/", cat_rel).starts_with(sub))
                {
                    // 使用已有的合法 category
                    let standard_cat = format!("docs/{}", cat_rel);
                    (
                        standard_cat,
                        fm.title.clone().unwrap_or_else(|| {
                            relative_path
                                .file_stem()
                                .unwrap()
                                .to_string_lossy()
                                .to_string()
                        }),
                        fm.summary.clone().unwrap_or_default(),
                    )
                } else {
                    // 原 category 不合法，调用 LLM
                    classify_and_extract_metadata(server, &relative_str, &content).await
                }
            } else {
                classify_and_extract_metadata(server, &relative_str, &content).await
            }
        } else {
            classify_and_extract_metadata(server, &relative_str, &content).await
        };

        // 解析标准分类路径为相对于 docs/ 的路径
        // target_category_path 类似 "docs/engineering/architecture"
        let dest_rel_dir = target_category_path
            .strip_prefix("docs/")
            .unwrap_or(&target_category_path);

        // 构造目标物理路径
        let filename = path.file_name().ok_or("Invalid filename")?;
        let dest_dir = authorized.safe_join(dest_rel_dir)?;
        let dest_path = dest_dir.join(filename);
        authorized.validate_contained_path(&dest_path)?;

        let dest_rel_path = format!("{}/{}", dest_rel_dir, filename.to_string_lossy());

        // 更新/写入 Frontmatter
        let mut fm = fm_opt.unwrap_or(Frontmatter {
            title: Some(title),
            summary: Some(summary),
            category: Some(dest_rel_dir.to_string()),
            organize: Some(true),
            other_fields: Vec::new(),
        });

        // 保证 category 正确且同步
        fm.category = Some(dest_rel_dir.to_string());

        // 就地任务状态检测与勾选
        let (new_body, task_modified) = sync_tasks_in_content(server, body);
        let final_content = format!("{}{}", serialize_frontmatter(&fm), new_body);

        if task_modified {
            synced_count += 1;
        }

        // 如果物理路径不需要移动 (即已经在标准目录，且目的地一致)
        if is_already_categorized && path == dest_path {
            if dry_run {
                if final_content != content {
                    log_messages.push(format!(
                        "[dry-run] Would sync tasks/frontmatter in-place: '{}'",
                        relative_str
                    ));
                }
                continue;
            }
            // 只写入可能更新后的内容（就地勾选/Frontmatter 补齐）
            authorized.write_existing_file(&path, &source_identity, &final_content)?;
            log_messages.push(format!(
                "Synced tasks/frontmatter in-place: '{}'",
                relative_str
            ));
        } else {
            if dry_run {
                if let Some((_, dest_metadata)) = authorized.optional_file(&dest_path)? {
                    let source_metadata = authorized.revalidate_object(
                        &path,
                        &source_identity,
                        false,
                        "planned source file",
                    )?;
                    let mtime_src = source_metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    let mtime_dest = dest_metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    if mtime_src >= mtime_dest {
                        log_messages.push(format!(
                            "[dry-run] Would move (newer) '{}' to '{}' and archive older destination",
                            relative_str, dest_rel_path
                        ));
                    } else {
                        log_messages.push(format!(
                            "[dry-run] Would archive '{}' to 'archive/' (destination '{}' is newer)",
                            relative_str, dest_rel_path
                        ));
                    }
                } else {
                    log_messages.push(format!(
                        "[dry-run] Would move '{}' to '{}'",
                        relative_str, dest_rel_path
                    ));
                }
                moved_count += 1;
                continue;
            }
            // 处理物理移动与同名冲突
            let destination = authorized.optional_file(&dest_path)?;
            if let Some((dest_identity, meta_dest)) = destination {
                // 读修改时间 (mtime)
                let meta_src = authorized.revalidate_object(
                    &path,
                    &source_identity,
                    false,
                    "planned source file",
                )?;

                let mtime_src = meta_src.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let mtime_dest = meta_dest.modified().unwrap_or(SystemTime::UNIX_EPOCH);

                let archive_dir = authorized.safe_join("archive")?;
                let archive_identity = authorized.ensure_directory(&archive_dir)?;
                let stem = path.file_stem().unwrap().to_string_lossy().to_string();

                if mtime_src >= mtime_dest {
                    // 源文件（当前文件）更新，覆盖 dest，并将 dest 上的旧文件移至 archive
                    let (archive_path, archive_filename) =
                        authorized.unique_archive_target(&archive_dir, &stem)?;

                    authorized.rename_file(
                        &dest_path,
                        &dest_identity,
                        &archive_path,
                        &archive_identity,
                    )?;
                    authorized.write_new_file(&dest_path, &final_content)?;
                    authorized.remove_file(&path, &source_identity)?;
                    invalidated_planned_paths.insert(dest_path.clone());

                    log_messages.push(format!(
                        "Moved (newer) '{}' to '{}' (archived older to 'archive/{}')",
                        relative_str, dest_rel_path, archive_filename
                    ));
                } else {
                    // 目的地文件更新，放弃移动源文件，而是直接把源文件归档
                    let (archive_path, archive_filename) =
                        authorized.unique_archive_target(&archive_dir, &stem)?;

                    authorized.write_new_file(&archive_path, &final_content)?;
                    authorized.remove_file(&path, &source_identity)?;

                    log_messages.push(format!(
                        "Archived older '{}' directly to 'archive/{}' (destination '{}' was newer)",
                        relative_str, archive_filename, dest_rel_path
                    ));
                }
            } else {
                // 无同名冲突，直接写新路径，删旧路径
                authorized.write_new_file(&dest_path, &final_content)?;
                authorized.remove_file(&path, &source_identity)?;
                log_messages.push(format!("Moved '{}' to '{}'", relative_str, dest_rel_path));
            }
            moved_count += 1;
        }
    }

    // 3. 重新扫描标准分类目录，生成 _index.md
    let mut index_lines = Vec::new();
    index_lines.push("# Tachi Workspace Documents Index".to_string());
    index_lines.push(
        "This document tree is automatically maintained by Tachi. Do not edit manually.\n"
            .to_string(),
    );

    // 收集所有标准分类目录下的文件以建目录树
    // 标准子目录包括：engineering/architecture, engineering/devops, engineering/code-review, engineering/debugging, product, agent
    let categories = [
        (
            "Engineering: Architecture Specifications",
            "engineering/architecture",
        ),
        ("Engineering: DevOps & SOP Playbooks", "engineering/devops"),
        (
            "Engineering: Code Reviews & Decisions",
            "engineering/code-review",
        ),
        (
            "Engineering: Debugging & Post-mortems",
            "engineering/debugging",
        ),
        ("Product Roadmaps & PRDs", "product"),
        ("Agent Identities & Handoffs", "agent"),
    ];

    for (title, rel_dir) in &categories {
        let dir = authorized.safe_join(rel_dir)?;
        if let Some((directory_identity, directory_metadata)) =
            authorized.optional_directory(&dir)?
        {
            if !directory_metadata.is_dir() {
                return Err(format!(
                    "Refusing Wiki organize: protected invariant: index category '{}' is not a directory",
                    dir.display()
                ));
            }
            let paths = collect_markdown_files(&authorized, &dir, directory_identity, &[])?;

            if !paths.is_empty() {
                index_lines.push(format!("## {}", title));
                for (path, identity) in paths {
                    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let relative_url = path
                        .strip_prefix(&canonical_root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");

                    let file_content = authorized.read_text(&path, &identity)?;
                    let (fm_opt, _) = parse_frontmatter(&file_content);

                    let doc_title = fm_opt
                        .as_ref()
                        .and_then(|f| f.title.clone())
                        .unwrap_or_else(|| filename.replace(".md", ""));
                    let doc_summary = fm_opt
                        .as_ref()
                        .and_then(|f| f.summary.clone())
                        .unwrap_or_default();

                    if doc_summary.is_empty() {
                        index_lines.push(format!("- [{}](<{}>)", doc_title, relative_url));
                    } else {
                        index_lines.push(format!(
                            "- [{}](<{}>) - {}",
                            doc_title, relative_url, doc_summary
                        ));
                    }
                }
                index_lines.push("".to_string());
            }
        }
    }

    let index_content = index_lines.join("\n");
    let index_path = canonical_root.join("_index.md");
    if dry_run {
        let existing = match authorized.optional_file(&index_path)? {
            Some((identity, _)) => authorized.read_text(&index_path, &identity)?,
            None => String::new(),
        };
        if existing != index_content {
            log_messages.push("[dry-run] Would rebuild docs/_index.md".to_string());
        }
    } else if let Some((identity, _)) = authorized.optional_file(&index_path)? {
        if let Err(error) = authorized.write_existing_file(&index_path, &identity, &index_content) {
            log_messages.push(format!("WARN: failed to write _index.md: {error}"));
        }
    } else if let Err(error) = authorized.write_new_file(&index_path, &index_content) {
        log_messages.push(format!("WARN: failed to write _index.md: {error}"));
    }

    let result = json!({
        "status": "success",
        "dry_run": dry_run,
        "moved_files": moved_count,
        "synced_tasks": synced_count,
        "log": log_messages,
    });

    Ok(serde_json::to_string_pretty(&result).unwrap())
}

#[cfg(test)]
mod tests {
    use super::find_worktree_root_from;
    use std::fs;

    #[test]
    fn linked_worktree_root_is_not_normalized_to_primary_checkout() {
        let sandbox = tempfile::tempdir().expect("create fake worktree sandbox");
        let primary = sandbox.path().join("primary");
        let linked = sandbox.path().join("linked");
        fs::create_dir_all(primary.join(".git/worktrees/linked"))
            .expect("create primary git worktree metadata");
        fs::create_dir_all(linked.join("docs")).expect("create linked docs directory");
        fs::write(
            linked.join(".git"),
            "gitdir: ../primary/.git/worktrees/linked\n",
        )
        .expect("create linked worktree git file");
        fs::write(primary.join(".git/worktrees/linked/commondir"), "../..\n")
            .expect("create linked worktree common-dir metadata");

        let found = find_worktree_root_from(&linked.join("docs")).expect("find linked root");
        assert_eq!(
            found,
            linked.canonicalize().expect("canonicalize linked root")
        );
        assert_ne!(
            found,
            primary.canonicalize().expect("canonicalize primary root")
        );
    }
}
