#[cfg(test)]
use super::organize::{record_directory_sync, run_organize_test_hook, OrganizeTestPoint};
use super::organize::{AuthorizedDocs, PhysicalIdentity};
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

// Keep the crash-durability contract at a physical boundary separate from
// organize planning. Every rename-only publication syncs verified source
// bytes first. A cross-directory rename then proves the destination namespace
// before making source deletion durable; a same-directory kernel rename needs
// one parent sync. The link fallback must sync publication before unlinking
// the source, even when both names share a parent.
impl AuthorizedDocs {
    pub(super) fn rename_file(
        &self,
        source: &Path,
        source_identity: &PhysicalIdentity,
        destination: &Path,
        destination_parent: &PhysicalIdentity,
    ) -> Result<(), String> {
        self.rename_file_with_publication(
            source,
            source_identity,
            destination,
            destination_parent,
            cfg!(any(target_os = "macos", target_os = "linux")),
        )
    }

    pub(super) fn rename_file_with_publication(
        &self,
        source: &Path,
        source_identity: &PhysicalIdentity,
        destination: &Path,
        destination_parent: &PhysicalIdentity,
        kernel_rename: bool,
    ) -> Result<(), String> {
        self.revalidate_roots()?;
        self.revalidate_object(source, source_identity, false, "rename source")?;
        let source_parent = source.parent().ok_or_else(|| {
            format!(
                "Refusing Wiki organize: protected invariant: rename source '{}' has no parent",
                source.display()
            )
        })?;
        let source_parent_identity = self.ensure_existing_directory(source_parent)?;
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
        #[cfg(test)]
        run_organize_test_hook(OrganizeTestPoint::DestinationParentReady, parent);
        self.revalidate_roots()?;
        self.revalidate_object(source, source_identity, false, "rename source")?;
        self.revalidate_object(
            parent,
            destination_parent,
            true,
            "destination parent before rename",
        )?;
        self.sync_file_bytes(source, source_identity, "rename source file")?;
        #[cfg(test)]
        if run_organize_test_hook(OrganizeTestPoint::RenameFileFailure, destination) {
            return Err("Failed to rename file: injected archive failure".to_string());
        }
        // The empty-path check above is advisory: another writer can reserve
        // the archive name afterward. Publication itself must never replace it.
        let publication = if kernel_rename {
            crate::bootstrap::wiki_corpus::fs::atomic_noreplace(source, destination)
        } else {
            // Do not use atomic_noreplace's link-and-unlink fallback: the
            // destination namespace must be synced before removing the source.
            fs::hard_link(source, destination)
        };
        publication.map_err(|error| {
            format!(
                "Failed to rename '{}' -> '{}': {error}",
                source.display(),
                destination.display()
            )
        })?;
        if !kernel_rename {
            let published_identity = source_identity.relocated_to(destination);
            self.revalidate_roots()?;
            self.revalidate_object(
                destination,
                &published_identity,
                false,
                "linked destination",
            )?;
            self.sync_directory(parent, destination_parent, "rename destination parent")?;
            self.revalidate_object(
                source_parent,
                &source_parent_identity,
                true,
                "rename source parent",
            )?;
            self.revalidate_object(
                source,
                source_identity,
                false,
                "rename source before unlink",
            )?;
            self.revalidate_object(
                destination,
                &published_identity,
                false,
                "linked destination before unlink",
            )?;
            fs::remove_file(source).map_err(|error| {
                format!(
                    "Failed to remove rename source '{}': {error}",
                    source.display()
                )
            })?;
            self.sync_directory(
                source_parent,
                &source_parent_identity,
                "rename source parent",
            )?;
        } else if source_parent == parent {
            self.sync_directory(
                source_parent,
                &source_parent_identity,
                "rename source and destination parent",
            )?;
        } else {
            self.sync_directory(parent, destination_parent, "rename destination parent")?;
            // Reversing these fsyncs can lose both names after a crash even
            // though rename returned successfully.
            self.sync_directory(
                source_parent,
                &source_parent_identity,
                "rename source parent",
            )?;
        }
        self.revalidate_object(
            destination,
            &source_identity.relocated_to(destination),
            false,
            "renamed destination",
        )?;
        Ok(())
    }

    fn sync_file_bytes(
        &self,
        path: &Path,
        expected: &PhysicalIdentity,
        label: &str,
    ) -> Result<(), String> {
        self.revalidate_roots()?;
        self.revalidate_object(path, expected, false, label)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let file = options.open(path).map_err(|error| {
            format!(
                "Failed to open {label} '{}' for durable rename: {error}",
                path.display()
            )
        })?;
        let metadata = file.metadata().map_err(|error| {
            format!(
                "Failed to inspect {label} '{}' before durable rename: {error}",
                path.display()
            )
        })?;
        #[cfg(unix)]
        let opened_identity = PhysicalIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
            canonical: path.to_path_buf(),
        };
        #[cfg(not(unix))]
        let opened_identity = PhysicalIdentity {
            canonical: path.canonicalize().map_err(|error| {
                format!("Failed to canonicalize {label} before durable rename: {error}")
            })?,
        };
        #[cfg(not(unix))]
        let _ = metadata;
        if !opened_identity.matches(expected) {
            return Err(format!(
                "Refusing Wiki organize: protected invariant: {label} '{}' changed before byte sync",
                path.display()
            ));
        }
        #[cfg(test)]
        record_directory_sync(&format!("attempt {label}"), path);
        #[cfg(test)]
        if run_organize_test_hook(OrganizeTestPoint::RenameSourceSyncFailure, path) {
            return Err(format!(
                "Failed to sync {label}: injected file sync failure"
            ));
        }
        file.sync_all()
            .map_err(|error| format!("Failed to sync {label} '{}': {error}", path.display()))?;
        #[cfg(test)]
        record_directory_sync(&format!("complete {label}"), path);
        self.revalidate_roots()?;
        self.revalidate_object(path, expected, false, label)?;
        Ok(())
    }

    pub(super) fn sync_directory(
        &self,
        path: &Path,
        expected: &PhysicalIdentity,
        label: &str,
    ) -> Result<(), String> {
        self.revalidate_roots()?;
        self.revalidate_object(path, expected, true, label)?;
        #[cfg(test)]
        record_directory_sync(&format!("attempt {label}"), path);
        sync_directory_entry(path)
            .map_err(|error| format!("Failed to sync {label} '{}': {error}", path.display()))?;
        #[cfg(test)]
        record_directory_sync(&format!("complete {label}"), path);
        self.revalidate_roots()?;
        self.revalidate_object(path, expected, true, label)?;
        Ok(())
    }
}

pub(super) fn sync_directory_entry(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if run_organize_test_hook(OrganizeTestPoint::DirectorySyncFailure, path) {
        return Err(std::io::Error::other("injected directory sync failure"));
    }

    #[cfg(unix)]
    File::open(path)?.sync_all()?;

    #[cfg(windows)]
    {
        // Windows exposes no documented directory equivalent of POSIX fsync;
        // FlushFileBuffers is documented for file/volume handles, not
        // directory handles. Do not issue the known-invalid File::open call or
        // claim an undocumented flush. New/replaced files are synced while
        // open for writing, and rename-only sources are explicitly synced
        // before their namespace operation. Ordered directory revalidation
        // remains mandatory but is not Unix-equivalent crash durability.
        let metadata = fs::metadata(path)?;
        if !metadata.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "directory synchronization target is not a directory",
            ));
        }
    }

    #[cfg(not(any(unix, windows)))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "durable directory synchronization is unsupported on this platform",
    ));

    Ok(())
}
