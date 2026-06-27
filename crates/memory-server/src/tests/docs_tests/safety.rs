use super::*;

#[cfg(unix)]
#[tokio::test]
async fn test_docs_organize_skips_symlinked_directories() {
    let server = make_server();
    let temp_docs = tempdir().expect("create temp docs dir");
    let outside = tempdir().expect("create outside dir");
    let docs_path = temp_docs.path();
    let outside_doc = outside.path().join("escape.md");
    fs::write(&outside_doc, "# Outside\nThis file must not be organized.").unwrap();

    std::os::unix::fs::symlink(outside.path(), docs_path.join("linked")).unwrap();

    crate::docs_ops::handle_wiki_organize(&server, &docs_path.to_string_lossy(), false)
        .await
        .unwrap();

    assert!(outside_doc.exists());
    assert!(!docs_path
        .join("engineering")
        .join("architecture")
        .join("escape.md")
        .exists());
}

#[tokio::test]
async fn test_docs_organize_dry_run_makes_no_changes() {
    let server = make_server();
    let temp_docs = tempdir().expect("create temp docs dir");
    let docs_path = temp_docs.path();

    // A scattered file that would normally be classified/moved.
    let src = docs_path.join("design-prd.md");
    fs::write(
        &src,
        r#"---
title: "Design PRD File"
summary: "A product requirement document"
organize: true
---
# Design Requirements
- [ ] P2: Do other things (unresolved)
"#,
    )
    .unwrap();
    let original = fs::read_to_string(&src).unwrap();

    let res_str =
        crate::docs_ops::handle_wiki_organize(&server, &docs_path.to_string_lossy(), true)
            .await
            .unwrap();
    let res: serde_json::Value = serde_json::from_str(&res_str).unwrap();

    // Reports success in dry-run mode and flags it.
    assert_eq!(res["status"], "success");
    assert_eq!(res["dry_run"], true);
    assert_eq!(res["moved_files"], 1);

    // The plan is described in the log.
    let log = res["log"].as_array().unwrap();
    assert!(
        log.iter()
            .any(|m| m.as_str().unwrap_or("").contains("[dry-run] Would move")),
        "dry-run log should describe the planned move; got {log:?}"
    );

    // Crucially: nothing on disk changed.
    assert!(src.exists(), "source file must NOT be moved in dry-run");
    assert_eq!(
        fs::read_to_string(&src).unwrap(),
        original,
        "source content must be untouched in dry-run"
    );
    assert!(
        !docs_path.join("product").exists(),
        "standard directories must NOT be created in dry-run"
    );
    assert!(
        !docs_path.join("_index.md").exists(),
        "_index.md must NOT be written in dry-run"
    );
}
