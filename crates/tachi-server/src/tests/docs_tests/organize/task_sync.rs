use super::*;

#[tokio::test]
async fn test_docs_organization_and_task_sync() {
    let server = make_server();
    let temp_docs = tempdir().expect("create temp docs dir");
    let docs_path = temp_docs.path();

    // 1. 在 DB 中插入一些测试卡片（Kanban/Handoff）
    let resolved_card_1 = memcore::MemoryEntry {
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
        last_use_at: None,
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

    let resolved_card_2 = memcore::MemoryEntry {
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
        last_use_at: None,
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
