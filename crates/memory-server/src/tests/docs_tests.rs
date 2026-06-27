use super::make_server;
use chrono::Utc;
use serde_json::json;
use std::fs;
use tempfile::tempdir;

#[tokio::test]
async fn test_docs_organization_and_task_sync() {
    let server = make_server();
    let temp_docs = tempdir().expect("create temp docs dir");
    let docs_path = temp_docs.path();

    // 1. 在 DB 中插入一些测试卡片（Kanban/Handoff）
    let resolved_card_1 = memory_core::MemoryEntry {
        id: "card-123".to_string(),
        path: "/kanban/agent-a/agent-b".to_string(),
        summary: "Fix memory leak".to_string(),
        text: "Fixing a memory leak in the core system.".to_string(),
        importance: 0.8,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "kanban".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "kanban".to_string(),
        scope: "global".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        metadata: json!({
            "status": "resolved",
        }),
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    let resolved_card_2 = memory_core::MemoryEntry {
        id: "card-456".to_string(),
        path: "/kanban/agent-a/agent-b".to_string(),
        summary: "P1: Implement auth".to_string(),
        text: "Implementing authentication module.".to_string(),
        importance: 0.9,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "kanban".to_string(),
        topic: String::new(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        source: "kanban".to_string(),
        scope: "global".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        metadata: json!({
            "status": "resolved",
        }),
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    server
        .with_global_store(|store| {
            store.upsert(&resolved_card_1).map_err(|e| e.to_string())?;
            store.upsert(&resolved_card_2).map_err(|e| e.to_string())?;
            Ok(())
        })
        .unwrap();

    // 2. 准备 docs/ 物理结构
    // 根目录下的 README.md (白名单，不应移动)
    let readme_path = docs_path.join("README.md");
    fs::write(&readme_path, "# Test Workspace\nThis is a readme.").unwrap();

    // 一个散落的 Markdown 文件，包含待勾选任务与 Frontmatter
    let doc1_path = docs_path.join("design-prd.md");
    fs::write(
        &doc1_path,
        r#"---
title: "Design PRD File"
summary: "This is a product requirement document"
organize: true
---
# Design Requirements

- [ ] P0: Fix memory leak <!-- tachi:card-123 -->
- [ ] P1: Implement auth
- [ ] P2: Do other things (unresolved)
"#,
    )
    .unwrap();

    // 一个设置了 organize: false 的文件 (逃生舱)
    let no_organize_path = docs_path.join("ignored.md");
    fs::write(
        &no_organize_path,
        r#"---
organize: false
---
# Ignored File
- [ ] P0: Fix memory leak <!-- tachi:card-123 -->
"#,
    )
    .unwrap();

    // 3. 执行 Wiki Organize 整理流程
    let res_str =
        crate::docs_ops::handle_wiki_organize(&server, &docs_path.to_string_lossy(), false)
            .await
            .unwrap();
    let res: serde_json::Value = serde_json::from_str(&res_str).unwrap();

    assert_eq!(res["status"], "success");

    // 4. 验证白名单与逃生舱文件是否原地保留
    assert!(readme_path.exists());
    assert!(no_organize_path.exists());

    // 验证 ignored.md 虽然保留原地，但任务项依然被同步勾选
    let ignored_content = fs::read_to_string(&no_organize_path).unwrap();
    assert!(ignored_content.contains("- [x] P0: Fix memory leak <!-- tachi:card-123 -->"));

    // 5. 验证 design-prd.md 被正确分类物理移动
    // 根据 test_fallback 逻辑，文件名含有 "prd" 归类至 "docs/product/test_product"
    let dest_prd_dir = docs_path.join("product").join("test_product");
    let dest_prd_path = dest_prd_dir.join("design-prd.md");

    assert!(
        !doc1_path.exists(),
        "Original scattered file should be moved"
    );
    assert!(
        dest_prd_path.exists(),
        "Moved file should exist in standard path"
    );

    let dest_content = fs::read_to_string(&dest_prd_path).unwrap();
    // card-123 matches by explicit id; card-456 matches by raw task text.
    assert!(dest_content.contains("- [x] P0: Fix memory leak <!-- tachi:card-123 -->"));
    assert!(dest_content.contains("- [x] P1: Implement auth"));
    assert!(dest_content.contains("- [ ] P2: Do other things (unresolved)"));

    // 6. 验证 _index.md 的生成
    let index_path = docs_path.join("_index.md");
    assert!(index_path.exists());
    let index_content = fs::read_to_string(&index_path).unwrap();
    assert!(index_content.contains("# Tachi Workspace Documents Index"));
    assert!(index_content.contains("](<product/test_product/design-prd.md>)"));
    assert!(!index_content.contains("file://"));
    assert!(index_content.contains("Design PRD File"));
}

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

#[tokio::test]
async fn test_docs_mtime_conflict_resolution() {
    let server = make_server();
    let temp_docs = tempdir().expect("create temp docs dir");
    let docs_path = temp_docs.path();

    // 准备一个已经在标准目录的文件，比如 "engineering/architecture/api.md"
    let arch_dir = docs_path.join("engineering").join("architecture");
    fs::create_dir_all(&arch_dir).unwrap();
    let dest_api_path = arch_dir.join("api.md");
    fs::write(&dest_api_path, "Existing architecture doc content.").unwrap();

    // 准备一个在根目录下的同名散落文件，比如 "api.md"
    // The resolver treats equal mtimes as "source wins" (`>=`), so same-tick
    // writes exercise the intended conflict path without a wall-clock sleep.
    let src_api_path = docs_path.join("api.md");
    fs::write(&src_api_path, "Newer scattered API doc content.").unwrap();

    // 执行整理
    crate::docs_ops::handle_wiki_organize(&server, &docs_path.to_string_lossy(), false)
        .await
        .unwrap();

    // 源文件应该被移动并覆盖目的地，旧目的地文件应该被移入 docs/archive/
    assert!(!src_api_path.exists());
    assert!(dest_api_path.exists());

    let current_content = fs::read_to_string(&dest_api_path).unwrap();
    assert!(current_content.contains("Newer scattered API doc content."));

    // 验证 archive 下有归档文件
    let archive_dir = docs_path.join("archive");
    assert!(archive_dir.exists());
    let entries = fs::read_dir(archive_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    let archive_filename = entries[0].file_name().to_string_lossy().to_string();
    assert!(archive_filename.starts_with("api."));
}

#[tokio::test]
async fn test_docs_conflict_archive_names_are_unique() {
    let server = make_server();
    let temp_docs = tempdir().expect("create temp docs dir");
    let docs_path = temp_docs.path();

    let arch_dir = docs_path.join("engineering").join("architecture");
    fs::create_dir_all(&arch_dir).unwrap();
    let dest_api_path = arch_dir.join("api.md");
    fs::write(&dest_api_path, "Original destination API doc.").unwrap();

    let scattered_a = docs_path.join("a");
    let scattered_b = docs_path.join("b");
    fs::create_dir_all(&scattered_a).unwrap();
    fs::create_dir_all(&scattered_b).unwrap();
    fs::write(scattered_a.join("api.md"), "Scattered API doc A.").unwrap();
    fs::write(scattered_b.join("api.md"), "Scattered API doc B.").unwrap();

    crate::docs_ops::handle_wiki_organize(&server, &docs_path.to_string_lossy(), false)
        .await
        .unwrap();

    let archive_dir = docs_path.join("archive");
    let entries = fs::read_dir(archive_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
}

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
