# CHANGELOG

## 0.8.0 - 2026-04-01

### Changed

- OpenClaw integration plugin identity renamed from `memory-hybrid-bridge` to `tachi`.
- Integration runtime is now single-entry: memory bridge, session intelligence, task tracking, and run audit are loaded from one plugin instead of four separate OpenClaw extensions.

### Added

- Added `compact_context`, todo/spawn tracking tools, and explicit `tachi_*` passthrough tools for skill / hub / vault / ghost / kanban / graph / state / identity / handoff.

### Docs

- README updated to reflect the consolidated architecture and the new `plugins.slots.memory = "tachi"` setup.

## 0.7.2 - 2026-03-24

### 依赖升级与底层加固

- **内核同步**：完全对齐原生服务端内核跨入 v0.7.2。
- **并发与注入加固**：随着内核 SRE 安全审计通过，彻底堵住向量写入时序漏洞及沙箱越权 Bypass，现在可放心承载高度并发的 Agent 请求。

## 0.3.0 - 2026-03-14

### 架构大瘦身与提速

- **内核整合**：对齐原生 Rust `memcore` [v0.3.0]，全面移除了重排器（Reranker）的强耦合，改为支持底层的三通道直连查询。
- **配置项精简**：废弃部分过时的 Rerank API 配置依赖，重写桥接层的类型绑定使其更轻量。
- **状态隔离**：适配最新规范，桥接器内部现在完全将 `hard_state` 的确定性写入及 `memories` 的向量抽象分离开来。
- **按需唤醒**：对接内核的 `ENABLE_PIPELINE` 标志，插件运行在 OpenClaw 中时默认静默异步推导管道，极大改善扩展模块初始化时的等待时延。

## 0.2.0 - 2026-02-12

### P0 安全与稳健

- extractor 前新增输入清洗：控制字符、零宽字符、常见注入短语与伪角色标签中和。
- prompt 加载新增内置 fallback，外部模板缺失时自动降级可用。
- memory 关键写操作新增审计日志 `data/audit-log.jsonl`：append / overwrite / delete。

### P1 质量与性能

- 新增提炼去重机制（默认阈值 `0.9`），避免 shadow store 膨胀。
- 新增自动捕获触发器（关键词 + 最小长度），减少无意义提炼调用。
- hybrid search 新增“最近 N 条限量读取”（默认 2000）与短 TTL 缓存，降低全量扫描开销。

### 工具与配置

- 新增工具：`memory_delete_entry`（删除 entry 并审计）。
- 配置新增：`auditLogPath`、`searchReadLimit`、`dedupThreshold`、`captureMinChars`、`captureTriggerKeywords`。

### 文档

- README 增补“为什么这些改造”与最小验证命令。
