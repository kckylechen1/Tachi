use super::Publisher;
use std::fs;
use std::os::unix::fs::symlink;

fn fixture() -> tempfile::TempDir {
    // macOS /var and /tmp may themselves be system symlinks. Fixtures use
    // their resolved location; publisher inputs deliberately never follow links.
    tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
}

#[test]
fn publishes_new_skill_and_link_without_replacing_on_repeat() {
    let temp = fixture();
    let store = temp.path().join("store");
    let projection = temp.path().join("projection");
    let mut publisher = Publisher::open(&store, &projection).unwrap();
    let (file, link) = publisher.publish("new-skill", "first").unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "first");
    assert_eq!(fs::read_link(&link).unwrap(), store.join("new-skill"));
    assert!(publisher.publish("new-skill", "replacement").is_err());
    assert_eq!(fs::read_to_string(file).unwrap(), "first");
}

#[test]
fn preserves_foreign_directory_and_same_named_symlink() {
    let temp = fixture();
    let store = temp.path().join("store");
    let projection = temp.path().join("projection");
    let foreign = temp.path().join("foreign");
    fs::create_dir_all(projection.join("directory")).unwrap();
    fs::write(projection.join("directory/SKILL.md"), "directory owner").unwrap();
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("SKILL.md"), "link owner").unwrap();
    symlink(&foreign, projection.join("link")).unwrap();
    symlink(temp.path().join("absent"), projection.join("dangling")).unwrap();
    let mut publisher = Publisher::open(&store, &projection).unwrap();
    for name in ["directory", "link", "dangling"] {
        assert!(publisher.publish(name, "replacement").is_err());
        assert!(!store.join(name).exists());
    }
    assert_eq!(
        fs::read_to_string(projection.join("directory/SKILL.md")).unwrap(),
        "directory owner"
    );
    assert_eq!(
        fs::read_to_string(foreign.join("SKILL.md")).unwrap(),
        "link owner"
    );
    assert_eq!(fs::read_link(projection.join("link")).unwrap(), foreign);
    assert_eq!(
        fs::read_link(projection.join("dangling")).unwrap(),
        temp.path().join("absent")
    );
}

#[test]
fn preserves_existing_export_directory_and_symlink() {
    let temp = fixture();
    let store = temp.path().join("store");
    let projection = temp.path().join("projection");
    let foreign = temp.path().join("foreign");
    fs::create_dir_all(store.join("directory")).unwrap();
    fs::write(store.join("directory/SKILL.md"), "existing").unwrap();
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("SKILL.md"), "foreign").unwrap();
    symlink(&foreign, store.join("link")).unwrap();
    let mut publisher = Publisher::open(&store, &projection).unwrap();
    assert!(publisher.publish("directory", "replacement").is_err());
    assert!(publisher.publish("link", "replacement").is_err());
    assert_eq!(
        fs::read_to_string(store.join("directory/SKILL.md")).unwrap(),
        "existing"
    );
    assert_eq!(
        fs::read_to_string(foreign.join("SKILL.md")).unwrap(),
        "foreign"
    );
    assert_eq!(fs::read_link(store.join("link")).unwrap(), foreign);
    assert_eq!(fs::read_dir(&projection).unwrap().count(), 0);
}

#[test]
fn refuses_symlink_in_either_root_without_creating_foreign_files() {
    for store_is_link in [true, false] {
        let temp = fixture();
        let foreign = temp.path().join("foreign");
        let link = temp.path().join("link");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("sentinel"), "unchanged").unwrap();
        symlink(&foreign, &link).unwrap();
        let (store, projection) = if store_is_link {
            (link.join("nested"), temp.path().join("projection"))
        } else {
            (temp.path().join("store"), link.join("nested"))
        };
        assert!(Publisher::open(&store, &projection).is_err());
        assert_eq!(fs::read_dir(&foreign).unwrap().count(), 1);
        assert_eq!(
            fs::read_to_string(foreign.join("sentinel")).unwrap(),
            "unchanged"
        );
    }
}

#[test]
fn root_replacement_does_not_redirect_writes_into_a_foreign_directory() {
    let temp = fixture();
    let store = temp.path().join("store");
    let projection = temp.path().join("projection");
    let foreign = temp.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("sentinel"), "unchanged").unwrap();
    let mut publisher = Publisher::open(&store, &projection).unwrap();
    fs::rename(&store, temp.path().join("detached-store")).unwrap();
    symlink(&foreign, &store).unwrap();
    assert!(publisher.publish("new", "payload").is_err());
    assert_eq!(fs::read_dir(&foreign).unwrap().count(), 1);
    assert_eq!(
        fs::read_to_string(foreign.join("sentinel")).unwrap(),
        "unchanged"
    );
}

#[test]
fn refuses_traversal_and_same_root() {
    let temp = fixture();
    assert!(Publisher::open(
        &temp.path().join("../escape"),
        &temp.path().join("projection")
    )
    .is_err());
    assert!(Publisher::open(temp.path(), temp.path()).is_err());
    let mut publisher =
        Publisher::open(&temp.path().join("store"), &temp.path().join("projection")).unwrap();
    for name in ["", ".", "..", "../escape", "slash/name", "nul\0name"] {
        assert!(publisher.publish(name, "payload").is_err());
    }
    assert_eq!(fs::read_dir(temp.path().join("store")).unwrap().count(), 0);
}
