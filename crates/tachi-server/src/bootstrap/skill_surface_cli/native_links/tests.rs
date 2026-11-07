use super::*;

struct Fixture {
    _temp: tempfile::TempDir,
    source: PathBuf,
    target: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("central").join("example-skill");
        let target = temp.path().join("native-skills");
        fs::create_dir_all(source.join("references")).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(
            source.join("SKILL.md"),
            "---\nname: example-skill\ndescription: Test fixture\n---\nRead references/guide.md.",
        )
        .unwrap();
        fs::write(source.join("references/guide.md"), "first version").unwrap();
        Self {
            _temp: temp,
            source,
            target,
        }
    }

    fn native(&self) -> PathBuf {
        self.target.join("example-skill")
    }
    fn staged(&self) -> PathBuf {
        self.target
            .join(STORE_NAME)
            .join("example-skill")
            .join(STAGED_NAME)
    }
    fn receipt(&self) -> PathBuf {
        self.target
            .join(STORE_NAME)
            .join("example-skill")
            .join(RECEIPT_NAME)
    }
}

#[test]
fn whole_directory_is_shared_across_hosts_and_relink_is_idempotent() {
    let f = Fixture::new();
    let other = f._temp.path().join("another-host");
    fs::create_dir(&other).unwrap();
    assert_eq!(link(&f.source, &f.target).unwrap().links[0].state, "linked");
    let original = identity(&f.native()).unwrap();
    link(&f.source, &other).unwrap();
    assert_eq!(
        link(&f.source, &f.target).unwrap().links[0].state,
        "unchanged"
    );
    assert_eq!(identity(&f.native()).unwrap(), original);
    fs::write(f.source.join("references/guide.md"), "updated once").unwrap();
    for root in [&f.target, &other] {
        let native = root.join("example-skill");
        assert!(fs::symlink_metadata(&native)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(native.join("references/guide.md")).unwrap(),
            "updated once"
        );
    }
    unlink("example-skill", &f.target).unwrap();
    assert_eq!(
        fs::read_to_string(other.join("example-skill/references/guide.md")).unwrap(),
        "updated once"
    );
    assert!(f.source.join("SKILL.md").is_file());
    assert_eq!(
        unlink("example-skill", &f.target).unwrap().links[0].state,
        "already_removed"
    );
}

#[test]
fn unmanaged_directories_and_links_are_never_adopted_or_replaced() {
    let f = Fixture::new();
    fs::create_dir(f.native()).unwrap();
    fs::write(f.native().join("precious.txt"), "keep").unwrap();
    assert!(link(&f.source, &f.target)
        .unwrap_err()
        .contains("unmanaged"));
    assert!(unlink("example-skill", &f.target)
        .unwrap_err()
        .contains("unmanaged"));
    assert_eq!(
        fs::read_to_string(f.native().join("precious.txt")).unwrap(),
        "keep"
    );
    fs::rename(f.native(), f.target.join("preserved-directory")).unwrap();
    for source in [&f.source, &f.target.join("missing-source")] {
        filesystem::create_directory_link(source, &f.native()).unwrap();
        assert!(link(&f.source, &f.target).is_err());
        assert!(unlink("example-skill", &f.target).is_err());
        assert_eq!(fs::read_link(f.native()).unwrap(), *source);
        filesystem::remove_directory_link(&f.native()).unwrap();
    }
}

#[test]
fn moved_source_is_reported_broken_and_owned_link_can_still_be_removed() {
    let f = Fixture::new();
    link(&f.source, &f.target).unwrap();
    let moved = f.source.with_file_name("moved-skill");
    fs::rename(&f.source, &moved).unwrap();
    assert_eq!(list(&f.target).unwrap().links[0].state, "broken");
    unlink("example-skill", &f.target).unwrap();
    assert!(!present(&f.native()).unwrap());
    assert!(moved.join("SKILL.md").is_file());
    assert!(list(&f.target).unwrap().links.is_empty());
}

#[test]
fn replaced_link_is_preserved_even_when_it_points_at_the_same_source() {
    let f = Fixture::new();
    link(&f.source, &f.target).unwrap();
    // Keep the old inode alive to exercise identity independently of target text.
    fs::rename(f.native(), f.target.join("old-link")).unwrap();
    filesystem::create_directory_link(&fs::canonicalize(&f.source).unwrap(), &f.native()).unwrap();
    let replacement = identity(&f.native()).unwrap();
    assert_eq!(list(&f.target).unwrap().links[0].state, "changed");
    assert!(unlink("example-skill", &f.target).is_err());
    assert!(link(&f.source, &f.target).is_err());
    assert_eq!(identity(&f.native()).unwrap(), replacement);
    filesystem::remove_directory_link(&f.native()).unwrap();
    filesystem::create_directory_link(&f.target.join("missing"), &f.native()).unwrap();
    assert!(unlink("example-skill", &f.target).is_err());
    assert_eq!(fs::read_link(f.native()).unwrap(), f.target.join("missing"));
}

#[test]
fn replacement_between_preflight_and_unlink_is_restored_without_deletion() {
    let f = Fixture::new();
    link(&f.source, &f.target).unwrap();
    let error = unlink_inner("example-skill", &f.target, || {
        filesystem::remove_directory_link(&f.native()).unwrap();
        fs::create_dir(f.native()).unwrap();
        fs::write(f.native().join("precious.txt"), "foreign content").unwrap();
    })
    .unwrap_err();
    assert!(error.contains("replacement restored"), "{error}");
    assert_eq!(
        fs::read_to_string(f.native().join("precious.txt")).unwrap(),
        "foreign content"
    );
    assert!(f.receipt().is_file());
    assert!(!present(&f.staged()).unwrap());
}

#[test]
fn interrupted_publish_or_unlink_can_be_resolved_explicitly() {
    let f = Fixture::new();
    link(&f.source, &f.target).unwrap();
    move_exclusive(&f.native(), &f.staged()).unwrap();
    assert_eq!(list(&f.target).unwrap().links[0].state, "staged");
    assert_eq!(link(&f.source, &f.target).unwrap().links[0].state, "linked");
    move_exclusive(&f.native(), &f.staged()).unwrap();
    unlink("example-skill", &f.target).unwrap();
    assert!(list(&f.target).unwrap().links.is_empty());
    assert!(f.source.join("SKILL.md").is_file());
}

#[test]
fn exclusive_moves_preserve_competing_destination_and_missing_links_require_reset() {
    let f = Fixture::new();
    link(&f.source, &f.target).unwrap();
    move_exclusive(&f.native(), &f.staged()).unwrap();
    fs::write(f.native(), "competing file").unwrap();
    assert!(move_exclusive(&f.staged(), &f.native()).is_err());
    assert_eq!(fs::read_to_string(f.native()).unwrap(), "competing file");
    assert!(present(&f.staged()).unwrap());
    assert!(link(&f.source, &f.target).is_err());
    assert!(unlink("example-skill", &f.target).is_err());
    fs::remove_file(f.native()).unwrap();
    filesystem::remove_directory_link(&f.staged()).unwrap();
    assert_eq!(list(&f.target).unwrap().links[0].state, "missing");
    assert!(link(&f.source, &f.target)
        .unwrap_err()
        .contains("run unlink"));
    unlink("example-skill", &f.target).unwrap();
    link(&f.source, &f.target).unwrap();
}

#[test]
fn ownership_receipts_and_metadata_cannot_redirect_unlink() {
    let f = Fixture::new();
    link(&f.source, &f.target).unwrap();
    let original = fs::read(f.receipt()).unwrap();
    for key in ["name", "target_dir", "version"] {
        let mut receipt: serde_json::Value = serde_json::from_slice(&original).unwrap();
        receipt[key] = if key == "version" {
            2.into()
        } else {
            "../outside".into()
        };
        fs::write(f.receipt(), serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(unlink("example-skill", &f.target).is_err());
        assert!(f.native().join("SKILL.md").is_file());
    }
    fs::write(f.receipt(), original).unwrap();
    assert!(unlink("../example-skill", &f.target).is_err());
    let metadata = f.target.join(STORE_NAME);
    let saved = f.target.join("saved-metadata");
    fs::rename(&metadata, &saved).unwrap();
    filesystem::create_directory_link(&saved, &metadata).unwrap();
    assert!(unlink("example-skill", &f.target).is_err());
    assert!(saved.join("example-skill/receipt.json").is_file());
    assert!(f.native().join("SKILL.md").is_file());
}

#[test]
fn inventory_is_read_only_and_mutations_require_a_valid_explicit_directory() {
    let f = Fixture::new();
    assert!(list(&f.target).unwrap().links.is_empty());
    assert!(!f.target.join(STORE_NAME).exists());
    assert!(link(&f.source, &f.target.join("not-created")).is_err());
    assert!(!f.target.join("not-created").exists());
    fs::remove_file(f.source.join("SKILL.md")).unwrap();
    assert!(link(&f.source, &f.target).is_err());
    assert!(!f.target.join(STORE_NAME).exists());
    fs::write(f.source.join("SKILL.md"), "restored").unwrap();
    assert!(link(&f.source, &f.source.join("references")).is_err());
    // macOS rejects an invalid UTF-8 filename before the CLI can inspect it.
    // Exercise this fixture on Linux; the common assertions above run on every host.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt;
        let invalid = f
            ._temp
            .path()
            .join(std::ffi::OsString::from_vec(vec![0xff]));
        let source = invalid.join("example-skill");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "test").unwrap();
        assert!(link(&source, &f.target).unwrap_err().contains("UTF-8"));
        assert!(!f.target.join(STORE_NAME).exists());
    }
}

#[test]
fn stable_lock_serializes_mutations_and_never_disappears_on_unlink() {
    let f = Fixture::new();
    link(&f.source, &f.target).unwrap();
    let root = filesystem::target_directory(&f.target).unwrap();
    let held = Store::open(root, false).unwrap().unwrap();
    assert!(unlink("example-skill", &f.target)
        .unwrap_err()
        .contains("busy"));
    assert!(list(&f.target).unwrap_err().contains("busy"));
    drop(held);
    unlink("example-skill", &f.target).unwrap();
    assert!(f.target.join(STORE_NAME).join(".lock").is_file());
    link(&f.source, &f.target).unwrap();
}
