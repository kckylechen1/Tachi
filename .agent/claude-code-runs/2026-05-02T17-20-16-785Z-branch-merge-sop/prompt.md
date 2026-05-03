# Delegated Task

你现在的任务是执行 Tachi 仓库的代码审查与分支合并（技术债清理）。

请读取本地文件 `/Users/kckylechen/Desktop/Sigil/PROMPT-memory-investigation.md` 的最后一节“Tachi 仓库分支与 PR 代码审查及批量合并”。
严格按照里面定义的 Phase 0 到 Phase 5 的 SOP 顺序执行：
1. 审查并合并所有的 14 个待处理分支，解决冲突。
2. 解决先前审查发现的历史隐患。
3. 合并入 main 后，运行 `cargo check -p memory-server` 或 `cargo test` 确保未破坏主干。
4. 最终编译出 release 二进制，覆盖 `/Users/kckylechen/bin/tachi`。

因为你是被自动化调用的，遇到常规冲突请自动通过 git 分析并解决；如遇严重架构缺陷（如破坏了核心生命周期），你可以暂缓合并该分支并跳到下一个。
请在完成所有流程后，给我一份详细的执行报告，告诉我成功合并了哪些，卡在哪些地方。
