use super::*;

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
