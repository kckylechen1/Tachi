# 🔧 Tachi Wiki Enhancement — Task Brief

> **Branch**: `feat/wiki-enhancement`
> **Base**: `main`
> **Repo**: `/Users/kckylechen/Desktop/Sigil`
> **Time budget**: ~2h agent time

---

## 0. 前置：开分支 + 调查

But first, read these two reference materials to understand where the industry is heading:

---

## 参考资料

### 参考 A: Karpathy LLM-Wiki (原始构想)

> **Link**: https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f

核心思想：LLM 持续构建和维护一个 **persistent, compounding wiki**，而非一次性 RAG 回答。

**三层架构**:
1. **Raw sources** — 不可变的源文档（论文、文章、数据）
2. **The wiki** — LLM 维护的 markdown 文件集合（摘要、实体页、概念页、对比、综合）
3. **The schema** — 告诉 LLM 如何组织 wiki 的配置文件（CLAUDE.md / AGENTS.md）

**五大操作**:
- **Ingest**: 读 source → 写 summary → 更新 index → 更新 10-15 个关联 wiki 页
- **Query**: 搜 wiki → 综合回答 → 好答案可回写 wiki 作为新页面
- **Lint**: 健康检查（矛盾/过期/孤儿/缺页/缺引用）
- **Index** (`index.md`): 按分类列出所有页面，LLM 用它导航
- **Log** (`log.md`): append-only 时间线操作日志

**关键设计原则**:
- wiki 是 git repo，天然版本控制
- 页面间用 `[[wikilink]]` 互联
- 人类只策展和提问，LLM 做所有苦力（摘要、交叉引用、归档、记账）
- 随着 wiki 变大，可引入搜索引擎（如 qmd: BM25 + vector + LLM rerank）
- Obsidian 做可视化浏览

### 参考 B: OmegaWiki (北大 DAIR Lab 实现)

> **Link**: https://github.com/skyllwt/OmegaWiki (442⭐, 67 forks)

在 Karpathy 基础上做到了**学术研究全生命周期**:

**24 个 Claude Code Skills**:
- Phase 1 — Knowledge: `/init`, `/ingest`, `/discover`, `/edit`, `/ask`, `/check`, `/prefill`
- Phase 2 — Research: `/daily-arxiv`, `/ideate`, `/novelty`, `/review`, `/exp-design`, `/exp-run`, `/exp-eval`, `/refine`
- Phase 3 — Writing: `/survey`, `/paper-plan`, `/paper-draft`, `/paper-compile`, `/research`, `/rebuttal`

**9 种实体类型**: papers / concepts / topics / people / ideas / experiments / claims / Summary / foundations

**知识图谱**: `graph/edges.jsonl` + `graph/citations.jsonl`，9 种语义边类型（same_problem_as, builds_on, improves_on, challenges, surveys 等）

**关键特性**:
- Obsidian `[[wikilink]]` 原生格式
- 每个条目有 YAML frontmatter
- Cross-model review（第二 LLM 审稿）
- GitHub Actions daily-arxiv 自动抓取
- Bilingual i18n (EN + ZH)
- deterministic Python tools (`tools/research_wiki.py`, `tools/lint.py`)

**对 Tachi 的启示**:
1. Wiki 条目应该是**互联的 markdown**，不是孤立的 DB row
2. Ingest 是核心操作 — 一个 source 触发多页更新
3. 知识图谱 (edges) 让关系显式化
4. Lint 保证数据质量随时间不退化
5. Index + Log 提供导航和审计

---

### 0.1 开分支

```bash
cd /Users/kckylechen/Desktop/Sigil
git checkout main && git pull
git checkout -b feat/wiki-enhancement
```

### 0.2 调查现有 wiki 数据

在动代码之前，先了解数据现状。用 `tachi` CLI 或直接查 SQLite：

```bash
# 查看 wiki DB 路径和大小
ls -la ~/.tachi/global/memory.db
ls -la ~/.tachi/wiki/memory.db  # wiki 专用 DB（如果存在）

# 查看 wiki 条目总数
sqlite3 ~/.tachi/wiki/memory.db "SELECT COUNT(*) FROM memories WHERE path LIKE '/wiki/%';"

# 查看分类分布
sqlite3 ~/.tachi/wiki/memory.db "SELECT substr(path, 1, instr(substr(path, 7), '/') + 6) as cat, COUNT(*) FROM memories WHERE path LIKE '/wiki/%' GROUP BY cat ORDER BY COUNT(*) DESC;"

# 查看 vector 覆盖率
sqlite3 ~/.tachi/wiki/memory.db "SELECT COUNT(*) as total FROM memories WHERE path LIKE '/wiki/%';"
sqlite3 ~/.tachi/wiki/memory.db "SELECT COUNT(*) as with_vec FROM memories_vec;"

# 查看脏数据（<think） 标签泄漏）
sqlite3 ~/.tachi/wiki/memory.db "SELECT id, substr(summary, 1, 80) FROM memories WHERE summary LIKE '%<think）%' OR text LIKE '%<think）%';"

# 查看 entities 字段使用情况
sqlite3 ~/.tachi/wiki/memory.db "SELECT id, entities FROM memories WHERE path LIKE '/wiki/%' AND entities != '[]' LIMIT 10;"
```

把调查结果记录下来，作为改进的基线数据。

---

## 关键文件地图

| 文件 | 职责 |
|------|------|
| `crates/memory-server/src/wiki_ops.rs` | wiki browse / search / lint 核心逻辑 |
| `crates/memory-server/src/copilot_ops.rs` | tachi_wiki_write / tachi_wiki_search (旧 API) |
| `crates/memory-server/src/memory_search_ops.rs` | 混合搜索引擎 (FTS + vector + symbolic) |
| `crates/memory-server/src/tool_params/facade.rs` | tachi_save / tachi_search 参数定义 |
| `crates/memory-server/src/tool_params/memory.rs` | wiki search/browse/lint 参数定义 |
| `crates/memory-server/src/tools.rs` | MCP 工具注册和路由 |
| `crates/memory-server/src/bootstrap.rs` | 服务器启动、search_memory_rows |
| `crates/memory-server/src/enrichment.rs` | Enrichment pipeline (Voyage embed) |
| `crates/memory-server/src/cli.rs` | CLI 子命令定义 |
| `crates/memory-core/src/search.rs` | 底层搜索引擎 (channel merge) |
| `crates/memory-core/src/types.rs` | MemoryEntry / ScoredEntry 类型 |
| `crates/memory-server/src/tests.rs` | 集成测试 |

---

## P0: 修复 wiki search 的 vector embedding 🔴

### 问题

`wiki_ops.rs:427` 中，wiki search 调 `search_memory_rows` 时传入 `query_vec: None`，导致 vector channel 永远不会被激活。所有 wiki 搜索结果的 `score.vector` 都是 `0.0`。

但 `memory_search_ops.rs:272-275` 显示 `search_memory_rows` 在 `query_vec` 为 None 时会自动尝试调用 Voyage API 生成 embedding。所以问题可能在于：

1. wiki search 走的是 `search_memory_rows`（在 bootstrap.rs 中定义），检查它是否也有 auto-embed 逻辑
2. 或者 wiki DB（named project "wiki"）的 `vec_available` flag 是 false

### 修复步骤

1. 读 `crates/memory-server/src/bootstrap.rs` 中 `search_memory_rows` 的完整实现
2. 读 `crates/memory-server/src/memory_search_ops.rs` 中 vector auto-embed 的逻辑
3. 确认 wiki search 是否走了和普通 search 一样的 auto-embed 路径
4. 如果不是，让 wiki search 也能自动生成 query_vec（或者把 auto-embed 移到更上层）
5. 确认 `vec_available` 在 wiki project DB 上也为 true

### 验证

```bash
cargo test -p memory-server -- wiki
# 应该所有 wiki 相关测试通过
```

修完后手动验证：搜 "debugging lessons" 应该能命中 engineering/debugging 分类的中文条目。

---

## P1: Wiki 条目互联 (entities cross-reference) 🟡

### 问题

Wiki 条目是孤岛，entities 字段有数据但搜索/浏览时不展示关联条目。

### 实现

在 `wiki_ops.rs` 的 `handle_wiki_browse` 中：

1. 对返回的每个 entry，提取其 `entities` 字段
2. 对每个 entity，搜索同 wiki DB 中 `entities` 包含该 entity 的其他条目
3. 在返回结构中添加 `related_entries` 字段（仅返回 id + path + summary，不递归）

**注意控制性能**：只在 browse 模式（非 stats 模式）且 limit <= 20 时做关联查询。

在 `handle_wiki_search` 返回中也添加 `related_entries`，但仅对 top 3 结果做关联。

### 新增方法建议

```rust
fn find_related_by_entities(
    server: &MemoryServer,
    project: &str,
    entities: &[String],
    exclude_id: &str,
    limit: usize,
) -> Vec<Value> {
    // 对每个 entity，在 wiki DB 中搜索 entities 字段包含该 entity 的条目
    // 排除自身，去重，按 importance 排序，返回 top N
}
```

### 验证

添加测试用例，确认返回结构中有 `related_entries` 字段。

---

## P2: 增强 Wiki Lint 🟡

### 现状

`wiki_ops.rs` 已有 lint 功能，支持 orphans / contradictions / stale / missing_edges 四种检查。

### 新增检查项

#### 2a. `dirty_data` — 检测 `<think）` 标签泄漏

```rust
// 在 handle_wiki_lint 的 node 遍历循环中新增：
if checks.iter().any(|c| c == "dirty_data") {
    if entry.text.contains("<think）") || entry.summary.contains("<think）") {
        dirty_data.push(json!({
            "id": entry.id,
            "path": entry.path,
            "issue": "think_tag_leak",
            "db": scope.as_str(),
        }));
    }
}
```

#### 2b. `duplicates` — 高相似度重复检测

当前 missing_edges 已有 token_cosine_similarity，加一个阈值更高的 duplicate 检测（> 0.95）：

```rust
if checks.iter().any(|c| c == "duplicates") && similarity > 0.95 {
    duplicates.push(json!({
        "left_id": left.id,
        "right_id": right.id,
        "left_path": left.path,
        "right_path": right.path,
        "similarity": similarity,
        "db": nodes[i].1.as_str(),
    }));
}
```

#### 2c. 更新 `default_checks()` 函数

```rust
fn default_checks() -> Vec<String> {
    vec![
        "orphans".into(),
        "contradictions".into(),
        "stale".into(),
        "missing_edges".into(),
        "dirty_data".into(),
        "duplicates".into(),
    ]
}
```

#### 2d. 更新返回 JSON

在最终的 `serde_json::to_string` 中加入 `dirty_data` 和 `duplicates` 字段。

### 验证

```bash
cargo test -p memory-server -- wiki_lint
# 添加新测试覆盖 dirty_data 和 duplicates
```

---

## P3: Obsidian 导出命令 🟢

### 实现

在 `crates/memory-server/src/cli.rs` 中添加子命令：

```
tachi wiki export --format obsidian --output ~/wiki-export/
```

#### 逻辑

1. 遍历 wiki DB 中所有 `/wiki/` 前缀的条目
2. 按 path 创建目录结构
3. 每个条目生成一个 `.md` 文件，包含：
   - YAML frontmatter（id, importance, keywords, entities, timestamp, category）
   - 正文（text 字段）
   - 底部 `## See Also` 区域：entities 转为 `[[wikilink]]` 格式
4. 生成 `_index.md` 汇总页

#### 文件结构

```
~/wiki-export/
├── quant/
│   ├── strategy/
│   │   ├── V8-冷启动极速改造.md
│   │   └── ...
│   └── data-pipeline/
│       └── ...
├── engineering/
│   └── ...
├── agent/
│   └── tachi/
│       └── ...
└── _index.md
```

#### Obsidian 互联

- 在 text 中出现的 entities，替换为 `[[entity-name]]` 格式
- frontmatter 中添加 `tags` 字段（从 keywords 映射）

### 验证

```bash
cargo build -p memory-server
./target/debug/tachi wiki export --format obsidian --output /tmp/wiki-test/
# 检查输出目录结构和文件内容
ls -R /tmp/wiki-test/ | head -50
cat /tmp/wiki-test/_index.md
```

---

## P4: Wiki Ingest 流程 🟡

### 实现

添加新的 MCP 工具 `tachi_wiki_ingest`：

#### 参数

```rust
pub(crate) struct TachiWikiIngestParams {
    /// URL or file path to ingest
    pub source: String,
    
    /// Optional topic hint for categorization
    pub topic: Option<String>,
    
    /// Whether to update related wiki entries
    #[serde(default = "default_true")]
    pub update_related: bool,
}
```

#### 流程

1. 读取 source（如果是 URL，用 reqwest 抓取；如果是本地文件，直接读取）
2. 用 LLM 提取 key entities、topic、summary、keywords
3. 创建 wiki 条目（调用现有的 tachi_save / wiki_write 逻辑）
4. 如果 `update_related` 为 true：
   - 搜索 wiki 中已有的相关条目（按 entities 匹配）
   - 对每个相关条目，添加 edge（`related_to` 或 `references`）
5. 返回：创建的条目 ID + 更新的关联条目列表

**注意**：这个功能依赖 LLM，实现时确保：
- LLM 调用失败时 gracefully degrade（只存原始文本，不做智能提取）
- 不要在 ingest 中做 blocking 的大量工作，保持 MCP 调用快速返回

### 验证

- 写一个集成测试，mock LLM 提取结果，验证 ingest 创建条目 + 添加 edges
- 手动测试：ingest 一个 URL，检查 wiki 中是否出现新条目

---

## P5: Wiki 操作日志 🟢

### 实现

在 wiki 相关操作（write / search / browse / lint / ingest）执行时，append 一条日志到 wiki DB 的特殊 path `/wiki/_log`。

#### 日志条目格式

```json
{
  "path": "/wiki/_log",
  "text": "## [2026-05-02T14:38:20Z] write | Wiki功能评测\nCreated entry fe127645 at /wiki/general/...",
  "category": "other",
  "importance": 0.3,
  "retention_policy": "durable"
}
```

#### 实现方式

在 `wiki_ops.rs` 中添加：

```rust
fn append_wiki_log(
    server: &MemoryServer,
    operation: &str,  // "write" | "search" | "browse" | "lint" | "ingest"
    details: &str,
) {
    let now = Utc::now().to_rfc3339();
    let log_text = format!("## [{}] {} | {}", now, operation, details);
    // 写入 wiki DB，path = "/wiki/_log", append 模式
    // 如果 /wiki/_log 条目已存在，append 到 text 末尾
    // 如果不存在，创建新条目
}
```

在 `handle_wiki_browse`、`handle_wiki_search`、`handle_wiki_lint` 的成功路径上调用此函数。

### 验证

```bash
cargo test -p memory-server -- wiki_log
# 添加测试：执行 browse 后检查 /wiki/_log 条目是否存在
```

---

## 完成后

### 自测清单

```bash
# 1. 编译检查
cargo check -p memory-core
cargo check -p memory-server

# 2. 全量测试
cargo test -p memory-core
cargo test -p memory-server

# 3. Clippy
cargo clippy -p memory-server -- -D warnings

# 4. 确认无 regression
cargo test -p memory-server -- wiki
cargo test -p memory-server -- tachi_wiki
cargo test -p memory-server -- wiki_lint
```

### 提交 + PR

```bash
git add -A
git commit -m "feat(wiki): enhance wiki with vector search fix, entity linking, lint improvements, obsidian export, ingest, and operation log

P0: Fix wiki search vector embedding (query_vec was always None)
P1: Add entity cross-reference in browse/search results
P2: Add dirty_data and duplicates checks to wiki lint
P3: Add 'tachi wiki export --format obsidian' CLI command
P4: Add tachi_wiki_ingest MCP tool for automated source ingestion
P5: Add wiki operation log at /wiki/_log"

git push origin feat/wiki-enhancement
gh pr create --title "feat(wiki): P0-P5 wiki enhancements" \
  --body "## Changes

### P0: Vector search fix
- Wiki search now properly generates query_vec via Voyage API
- Cross-language semantic search now works (EN query → CN wiki entries)

### P1: Entity cross-reference
- browse and search results include related_entries based on shared entities

### P2: Enhanced lint
- Added dirty_data check (detects think tag leaks)
- Added duplicates check (similarity > 0.95)

### P3: Obsidian export
- New CLI: tachi wiki export --format obsidian --output dir
- Generates wikilink cross-references and YAML frontmatter

### P4: Wiki ingest
- New MCP tool: tachi_wiki_ingest
- Auto-extracts entities, creates wiki entry, links related entries

### P5: Operation log
- All wiki operations now append to /wiki/_log
- Provides audit trail for wiki evolution

## Testing
- All existing tests pass
- New tests added for P2-P5
- Manual verification of P0 (vector search)
"
```

---

## 优先级说明

如果时间不够，按此顺序砍：

1. **必做**: P0 (vector fix) + P2 (lint dirty_data) — 最高 ROI
2. **应做**: P1 (entity linking) + P5 (log) — 中等工作量
3. **可选**: P3 (obsidian export) + P4 (ingest) — 工作量最大

P0 + P2 大约 30 分钟 agent 时间。P1 + P5 大约 30 分钟。P3 + P4 大约 1 小时。
