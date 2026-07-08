use memory_server_manifest_audit::{PlannedAction, ProjectDbClass, RelocationItem};

#[test]
fn relocation_item_field_types_are_importable_from_crate_root() {
    let item = RelocationItem {
        project_name: "demo".to_string(),
        db_path: "/tmp/demo/memory.db".to_string(),
        class: ProjectDbClass::SymlinkAlias,
        action: PlannedAction::KeepAlias,
        relocate_to: None,
        symlink_target: Some("/repo/.tachi/memory.db".to_string()),
        note: "healthy alias".to_string(),
    };

    assert_eq!(item.class.as_str(), "symlink-alias");
    assert_eq!(item.action.as_str(), "keep-alias");
}
