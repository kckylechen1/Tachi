use super::*;

#[tokio::test]
async fn test_docs_accepts_docs_prefixed_category_frontmatter() {
    let server = make_server();
    let temp_docs = tempdir().expect("create temp docs dir");
    let docs_path = temp_docs.path();
    let source = docs_path.join("random.md");
    fs::write(
        &source,
        r#"---
title: "Pinned Category"
category: "docs/engineering/devops"
---
# Runbook
"#,
    )
    .unwrap();

    crate::docs_ops::handle_wiki_organize(&server, &docs_path.to_string_lossy(), false)
        .await
        .unwrap();

    assert!(!source.exists());
    assert!(docs_path
        .join("engineering")
        .join("devops")
        .join("random.md")
        .exists());
}
