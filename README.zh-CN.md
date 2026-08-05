<div align="center">
  <img src="assets/banner.png" alt="Tachi Banner" width="800" style="margin-bottom: 20px;" />
  <h1>✧ 藏经阁（Tachi）</h1>
  <p><strong>面向自主 AI Agent 的本地优先记忆与工作流控制平面</strong></p>
  <p>
    <a href="README.md">English</a> ·
    <a href="README.zh-CN.md"><b>简体中文</b></a> ·
    <a href="README.classical.md">文言文</a>
  </p>
  <p>
    <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/License-AGPLv3-blue.svg" alt="License: AGPLv3"></a>
    <img src="https://img.shields.io/badge/Rust-Edition_2021-orange.svg" alt="Rust">
    <img src="https://img.shields.io/badge/Protocol-MCP-purple" alt="MCP">
    <img src="https://img.shields.io/badge/Backend-SQLite_+_sqlite--vec-green.svg" alt="SQLite">
    <img src="https://img.shields.io/github/v/release/kckylechen1/tachi.svg" alt="Release">
  </p>
</div>

---

## 一句话介绍

Tachi 是一个单二进制、本地优先的 Agent 记忆与协调后端。它以 [MCP](https://modelcontextprotocol.io/) 服务器形态（`tachi-server`）运行，为 Agent 提供：

- **持久记忆**，支持混合语义 + 词法 + 图谱检索
- **层级命名空间**（`/user/preferences`、`/project/architecture`）
- **因果图谱边**，连接记忆、实体与决策
- **域标签存储**——每条记忆自带自由文本 `domain` 字段，`save_memory`/`search_memory` 可按域过滤
- **本地加密保险库**，用于 API 密钥与机密
- **Agent 协调**：交接令牌、看板、发布订阅（幽灵低语）
- **技能包与能力中心**：一次注册，各 Agent 共用
- **工作流控制平面**：`tachi_task`、`tachi_staff`、`tachi_verify`、`tachi_gh`

所有状态都存储在嵌入式 SQLite 中。**无需任何外部数据库。**

名字取自《攻壳机动队》中的塔奇克马：通过共享记忆不断进化的 AI 单元。

---

## 为什么用 Tachi

如今的 Agent 记忆通常是这样的：每次会话冷启动，重要上下文被塞进扁平向量库，几周后提示窗口里塞满无关碎片，而关键决策背后的"为什么"早已消失。

Tachi 基于四个信念构建：

1. **记忆应该结构化，而不是乱堆。** 层级 `path` 命名空间、因果图谱边和域标签让长期上下文保持有序、可追踪。
2. **检索应该是混合且快速的。** 语义（sqlite-vec + Voyage）、词法（FTS5 + CJK）、时间衰减（ACT-R）和图谱激活蔓延通过 RRF 融合。我们针对本地低延迟查找优化；可复现的基准测试已在路线图中。
3. **Agent 应该共享基础设施，而不是各自 spawn 混乱。** Tachi Hub 一次注册 MCP 服务器和技能，已连接 Agent 共享连接池、空闲清理、熔断器和清洗后的环境。告别僵尸进程。
4. **长期状态应留在本地。** 所有数据库都是 SQLite 文件，无需云数据库。云同步应传输加密 bundle 和事件日志，而不是活的 WAL 文件。

## Tachi 与同类方案对比

Tachi 不是通用向量数据库，也不是托管记忆云服务。它是面向 MCP 智能体的本地优先协调后端。

| 维度 | Tachi | Mem0 | Letta | Chroma | 裸向量库 |
|---|---|---|---|---|---|
| **接入协议** | MCP server（STDIO / Streamable HTTP） | 多语言 SDK | Python SDK + ADE | HTTP API + SDK | 无 |
| **部署形态** | 单 Rust 二进制 | 库 + 可选服务 | 服务 + 前端 | 服务 + 可选 Cloud | 依赖实现 |
| **存储后端** | 本地 SQLite + sqlite-vec | 通常需 PG / Redis / 向量库 | PG + Qdrant / Chroma | Chroma 索引 | 多种 |
| **外部依赖** | 零（embedding provider 可选） | 中等 | 中等 | 低到中 | 高 |
| **默认数据位置** | 本地优先 | 云优先，可自托管 | 自托管 | 自托管 / Cloud | 依赖实现 |
| **记忆组织** | `path` 层级 + 因果图谱 + 域 | Entity + session | Agent 状态 + memory block | Collection + metadata | 无 |
| **工作流控制** | `task` / `staff` / `verify` | 无 | Agent 编排 | 无 | 无 |
| **目标用户** | 个人/小团队运行自主 Agent | 应用开发者集成记忆 | 构建有状态 Agent | 需要向量检索的系统 | 基础设施工程师 |

---

## 快速开始

### 1. 安装

```bash
brew tap kckylechen1/tachi && brew install tachi
```

或使用 shell 安装脚本（检测到 OpenClaw 时会自动安装插件）：

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.9.0/scripts/install.sh)"
```

验证：

```bash
tachi --version
```

### 2. 配置你的 Agent

将 Tachi 添加到 Agent 的 MCP 配置中。Profile 决定 Agent 能看到多少工具：

```json
{
  "mcpServers": {
    "tachi": {
      "command": "tachi",
      "env": {
        "VOYAGE_API_KEY": "<your-key>",
        "SILICONFLOW_API_KEY": "<your-key>",
        "TACHI_PROFILE": "standard"
      }
    }
  }
}
```

- `VOYAGE_API_KEY` —— 混合语义检索所需（无此 key 时服务仍可启动，仅提供词法/图谱召回）。
- `SILICONFLOW_API_KEY` —— 结构化抽取、摘要、熔炉蒸馏，建议填写。
- `TACHI_PROFILE` —— 参见下方的 [工具暴露面 Profile](#工具暴露面-profile)。

服务器启动时还会自动加载项目根目录的 `.env`。将 `.env.example` 复制为 `.env` 以进行项目级配置。

### 3. 使用

以下示例展示传给 MCP 工具的 JSON 参数。门面工具暴露的字段与其底层原生工具一致；完整 schema 见 `crates/tachi-params/src/facade.rs` 及其 `facade/` 子模块。

```json
// tachi_save —— 结构化记忆
{
  "tool": "tachi_save",
  "arguments": {
    "text": "前端必须使用 Vite，严禁 Webpack。允许 Tailwind。",
    "path": "/project/frontend",
    "importance": 0.8,
    "keywords": ["vite", "webpack", "tailwind"],
    "retention_policy": "durable"
  }
}

// tachi_memory —— 混合检索
{
  "tool": "tachi_memory",
  "arguments": {
    "action": "search",
    "query": "前端构建策略是什么？",
    "path_prefix": "/project",
    "top_k": 6,
    "scope": "memory"
  }
}

// tachi_staff —— 显式例外场景下派出可跟踪的外部 worker（普通本地委派仍用宿主原生 subagent）
{
  "tool": "tachi_staff",
  "arguments": {
    "action": "start",
    "task": "审阅 API 边界，并把发现写入 result.md。",
    "staffing_reason": "native_subagent_unavailable"
  }
}

// tachi_staff —— 查询上一步返回的 dispatch_id 的运行状态
{
  "tool": "tachi_staff",
  "arguments": {
    "action": "status",
    "dispatch_id": "<start 返回的 dispatch_id>"
  }
}
```

各宿主对应的配置文件路径和高级设置见 [`docs/INSTALL.md`](docs/INSTALL.md)。

---

## 系统架构

```mermaid
graph TD
    subgraph Clients["客户端"]
        CLI["tachi CLI"]
        RMCP["MCP 服务器 (Rust 二进制)"]
        Node["@chaoxlabs/tachi-node"]
        Hosts["宿主 MCP 客户端"]
    end

    subgraph Cloud["可选 API"]
        VOYAGE["Voyage-4 嵌入"]
        SILICON["SiliconFlow / Qwen"]
    end

    subgraph Workers["异步工作站"]
        EXTRACT["事实抽取"]
        DISTILL["上下文蒸馏"]
        CAUSAL["因果管道"]
        GC["垃圾回收"]
    end

    subgraph Core["核心 (Rust memcore)"]
        API["存储 API"]
        SEARCH["五通道混合检索"]
        GRAPH["记忆图谱"]
        VAULT["Vault 元数据"]
        API --> SEARCH
        API --> GRAPH
        API --> VAULT
        SEARCH --> DB
        GRAPH --> DB
        VAULT --> DB
    end

    DB[(SQLite + sqlite-vec)]

    RMCP --> VOYAGE
    RMCP --> SILICON
    CLI --> RMCP
    Hosts --> RMCP
    Node --> Core
    Workers --> RMCP
```

---

## 项目结构

| 路径 | 说明 |
|------|------|
| `crates/memcore` | Rust 核心：SQLite 存储、迁移、混合检索、图谱、域、Vault 元数据、sqlite-vec。 |
| `crates/tachi-server` | MCP/CLI 二进制、Profile 过滤、Hub 路由、派发/工作流工具、Wiki、Vault 加密、守护锁、Foundry 后台任务。 |
| `crates/memory-node` | Node.js 原生绑定（`@chaoxlabs/tachi-node`）。 |
| `packages/tachi-cli` | TypeScript CLI 与 npm 封装。 |
| `tools/cleaner` | `tachi-clean` 清理工具，用于安全的 target / worktree / temp 清理。 |
| `skill/` | 内置技能包：`amp`、`codex`、`superpowers`、`waza`。 |
| `integrations/openclaw` | OpenClaw 插件。 |
| `docs/` | Agent 生态规范、安装指南与工程文档。 |
| `bin/` | 本地编译的发布二进制。 |

---

## 核心能力

### 1. 层级化记忆
记忆以 `path` 命名空间存储（例如 `/user/preferences`、`/project/architecture`、`/handoff/active`），而非平铺索引。项目、用户和协调上下文因此自然隔离且可组合。

### 2. 五通道混合检索
- **语义** —— `sqlite-vec` KNN + Voyage-4 嵌入。
- **词法** —— 针对 CJK 优化的 FTS5（`libsimple`），支持查询扩展（同义词、缩写、短语变体）以提升稀疏语料召回。
- **时间衰减** —— 受 ACT-R 遗忘曲线启发。
- **图谱激活蔓延** —— 沿因果/实体边从种子权重传播；同跳内 noisy-OR 累积，防止密集节点垄断结果。
- **RRF 融合** —— 互惠排名融合汇总各通道，向量余弦再加权，减少高语义查询的排名倒置。

### 3. 因果图谱
图谱引擎创建并遍历因果、时序和实体关系。`save_memory` 可自动为共享实体的记忆建立链接（`auto_link`）。`add_edge` / `get_edges` / `memory_graph` 是内部 `MemoryStore` 原语，已不在 MCP 曲面上（#757）；Agent 通过 `tachi_save`/`tachi_memory` 自动链接和召回的图谱激活蔓延通道（见上文第 2 节）触达图谱能力——没有独立的图谱遍历动作。

### 4. 域标签存储
每条记忆自带自由文本 `domain` 字段（如 `"code-review"`、`"personal"`）。`save_memory` 和 `search_memory` 可按域过滤。不存在独立的域注册表——域只是记忆行上的临时标签，不是需要配置的资源。

### 5. 加密保险库（Vault）
本地优先的密钥存储：Argon2id KDF + AES-256-GCM、每秘独立 nonce、空闲自动上锁、暴力破解保护、按 Secret 的 Agent ACL、多钥轮换。项目内 Agent 可通过 `.tachi/vault.env` 别名解析 Vault 密钥。`tachi vault exec --require NAME -- <cmd>` 可在子进程中注入 Vault 凭证运行命令（Vault 只补全调用方未设置的环境变量）；默认情况下，若 Vault 不可用则拒绝派生无凭证的子进程，`--allow-unauthenticated` 可显式选择回退为继承当前环境运行。详见 [`docs/INSTALL.md`](docs/INSTALL.md)。

### 6. Tachi Hub 与技能包
一次注册 MCP 服务器、技能和工作流，所有已连接 Agent 都能发现并调用。`pack_register` / `pack_project` 安装 curated 技能集合并投射到 Claude、Cursor、Codex、Gemini、OpenCode 等格式。`tachi_skill(action="discover"|"run"|"bundle")` 是 canonical 技能门面；独立 `run_skill`、`prepare_capability_bundle` 和面向技能发现的 `hub_discover` 仍作为旧客户端兼容入口保留。

### 7. 跨 Agent 协调
- **幽灵低语** —— Agent 间持久化主题发布/订阅（`ghost_publish`、`ghost_subscribe`、`ghost_ack`、`ghost_reflect`、`ghost_promote`）。
- **看板** —— 跨 Agent 卡片，支持 `ack` / `progress` / `result` 状态（`post_card`、`check_inbox`、`update_card`）。
- **交接 Issue 晋升** —— 从已有交接备忘录创建/关联 GitHub issue（`tachi_handoff(action='promote_issue')`）。#1099:旧的 `handoff_leave`/`handoff_check` 备忘录传递路由已退役——短消息用 `tachi_memory(action='sticky_leave'|'sticky_check')`,结构化任务交接用 `tachi_orchestrator(action='handoff_write'|'handoff_read')`。

> 幽灵与看板工具属于 `admin` Profile 的原生门面（未纳入 `standard`/`coordinate` bundle）。大多数 Agent 通过 `tachi_handoff`、`tachi_workflow`、`tachi_orchestrator`、`tachi_task` 门面进行协调。

### 8. 工作流控制平面
Tachi 不只是记忆库；它正在演变为 Agent 工程的持久控制平面：

- **`tachi_task`** —— 贯通 issue intake、docs/spec、路由建议、工作账本和 close-loop 写回；普通本地委派使用宿主原生 subagent。
- **`tachi_staff`** —— 显式例外场景（用户明确要求、跨会话持久、跨设备/远程接力、原生 subagent 不可用）下的外部 staffing 出口（`action='start'`/`'status'`），要求填写 typed `staffing_reason`；不是 `standard` Agent 的日常 subagent 入口。
- **`tachi_verify`** —— 记录后台验证证据（测试、类型检查、safe-merge gate）到 `.tachi/runs/<flow_id>/verification.json`。
- **`tachi_gh`** —— 读取 issue/PR，写评论，汇总 review 状态，并用 lifecycle/verification 证据运行 safe-merge 检查。

### 9. 神经熔炉与 Wiki
- **Foundry** —— 服务端上下文生命周期：`recall_context`、`capture_session`、`compact_context`、`section_build`、`compact_rollup`、`compact_session_memory`，以及 Agent 进化提案。
- **Wiki** —— Agent 维护的持久知识页：`tachi_wiki`、`tachi_browse`、`tachi_wiki_write`、`tachi_wiki_search`、`wiki_lint`。

### 10. 便携 Memory Kernel
Tachi 是下游产品（Hypermem 交易适配器、zeroclaw 通用 chat-agent 适配器、RomanBath 前端）的共享 memory kernel，而非让每个产品各自 fork。**便携 kernel manifest**（`docs/engineering/architecture/kernel-surface-v1.fixture.json`）将持久 schema、召回原语、向量/FTS 回退行为和就绪诊断冻结为产品无关的契约——下游适配器无需继承 Tachi 的 GitHub/派发/发布面。启动时运行 manifest 全局 DB 写入防护（WalOrphan 检查）；`TACHI_BYPASS_MANIFEST=1` 可在开发或崩溃恢复时跳过该防护。

---

## 工具暴露面 Profile

Tachi 根据 `TACHI_PROFILE` 暴露经过过滤的 MCP 工具面。`admin` 目录很大；大多数 Agent 应使用更小的 Profile。

| Profile | 暴露内容 | 适用场景 |
|---------|----------|----------|
| `standard` | 日常 Agent 意图面：`tachi_save`、`tachi_memory`、`tachi_task` 的非派发动作、`tachi_verify`、`tachi_web_search`、`tachi_wiki`、`tachi_skill`、`tachi_gh`、`peer_query`、Vault 会话/状态工具，以及 `runtime_info`、`tachi_status`、`tachi_briefing` 和 `tachi_tools`。`tachi_staff` 和手工 eval intake 不暴露。 | IDE Agent：Claude、Cursor、Codex、Windsurf、Trae、Antigravity；普通委派使用宿主原生 subagent。 |
| `coordinate` | `remember` + `coordinate` bundles：增加高级 handoff/workflow/orchestrator/agents/staff 工具；Tachi 自有 worker launch（`tachi_staff(action='start')`）仍是显式例外，不是默认执行器。 | 高级协调与适配器工作流，不替代宿主原生 subagent。 |
| `operate` | `remember` + `operate` bundles：增加 Foundry 生命周期、`hub_call`、`vault_unlock`/`lock`/`status`、`wiki_lint`。 | 运行时适配器、OpenClaw、运维自动化。 |
| `delegate` | 精选 worker 工具面：`tachi_tools`、`runtime_info`、`tachi_memory`、`tachi_event`、`tachi_web_search`、`tachi_browse`、`tachi_unstick`、`tachi_task`、`tachi_complete`、`tachi_skill(action='discover'|'run'|'bundle')` 和只读 `peer_query`。 | 由显式准入的 admin 派发产生的工作 Agent；无递归派发、无交接、无技能候选注册。 |
| `admin` | 完整目录，包括有类型理由的 durable/remote Tachi worker 例外。 | 维护、开发、治理和 operator 批准的执行例外。 |

宿主别名自动解析：`claude`、`claude-code`、`codex`、`cursor`、`trae`、`windsurf`、`ide`、`antigravity` → `standard`；`worker`、`subagent`、`delegate` → `delegate`；`openclaw`、`hermes`、`runtime`、`adapter`、`ops` → `operate`。

未设置 Profile 时，Tachi 自 v1.0.1 起默认使用 `standard`。

---

## 模型栈

Phase 2 已简化后台 Lane。抽取、摘要和每日蒸馏走配置好的 OpenAI-compatible API lane；已配置的不同 provider 回退耗尽后才显式记录 `LANE_OUTAGE`。`FOUNDRY_DISTILL_BACKEND=claude_cli` 仍是兼容选择器，但实际仍调用 API distill lane，不会启动 Claude 子进程。常规 reasoning/chat 则先尝试 Claude CLI，再回退到配置好的 API；provider-only 调用会刻意跳过 CLI。大多数部署只需：

| 用途 | 是否必填 | 默认值 |
|------|----------|--------|
| 嵌入 | **是** | Voyage-4，通过 `VOYAGE_API_KEY` |
| 抽取 / 摘要 / 蒸馏 | 建议 | SiliconFlow `Qwen/Qwen3.5-27B`，通过 `SILICONFLOW_API_KEY` |

高级场景仍支持按 Lane 覆盖（`EXTRACT_*`、`DISTILL_*`、`SUMMARY_*`、`REASONING_*`），详见 `.env.example`。

---

## 环境变量配置

将 `.env.example` 复制为项目根目录的 `.env`：

```bash
# 必填
VOYAGE_API_KEY=your_voyage_key_here

# 建议
SILICONFLOW_API_KEY=your_siliconflow_key_here
SILICONFLOW_BASE_URL=https://api.siliconflow.cn/v1/chat/completions
SILICONFLOW_MODEL=Qwen/Qwen3.5-27B

# 可选：覆盖全局 DB 路径。默认 ~/.tachi/global/tachi-memory.db；
# 项目库在 <git-root>/.tachi/tachi-memory.db 自动检测。
MEMORY_DB_PATH=~/.tachi/global/tachi-memory.db
```

服务器启动时会自动加载项目根目录的 `.env`。

---

## 数据库安全

Tachi 使用 WAL 模式的 SQLite。违反以下规则可能导致数据库损坏：

| 规则 | 原因 |
|------|------|
| **每个库单一实例** | 服务器持有排他文件锁（`tachi-memory.db.lock`）。同一数据库文件同一时间只应有一个 Tachi 进程写入。 |
| **不要放在云同步目录** | iCloud、Dropbox、OneDrive、Google Drive 与 SQLite WAL 不兼容。数据库请放在 `~/.tachi/` 或本地项目路径。 |
| **不要并发裸写** | 服务器运行时不要用 `sqlite3` 直接 INSERT/UPDATE。只读查询是安全的。 |
| **优雅关闭** | 服务器处理 SIGINT/SIGTERM，退出时执行 `PRAGMA optimize`。避免 `kill -9`。 |

活的 SQLite 文件应留在本地。应同步加密 bundle、append-only 事件日志、Vault 密文、工作流摘要、wiki/skill 产物等。

---

## 本地开发

```bash
# 编译发布二进制
cargo build --release

# 运行全部测试
cargo test --all

# 从源码运行 MCP 服务器，使用 standard profile
cargo run -p tachi-server -- --profile standard
```

需要 Rust ≥ 1.75。Node 绑定基于 napi-rs（打包为 `@chaoxlabs/tachi-node`），迭代开发建议安装 `@napi-rs/cli` 和 `cargo-watch`。

---

## 性能基准

可复现的基准测试套件正在整理中。当前设计目标包括：

- **本地优先延迟**：在热 SQLite 上优化至亚 10 ms 查找。
- **混合检索**：融合语义、词法、时间、图谱多种信号。
- **分层上下文**：通过 `L0 → L1 → L2` 压缩减少提示冗余。
- **零外部数据库依赖**：单个 Rust 二进制，每个库一个 SQLite 文件。

---

## 致谢

Tachi 的设计受到以下 Agent 长期记忆领域工作的启发：

- **[LongMem](https://github.com/Victorwz/LongMem)** (NeurIPS 2023) —— 解耦记忆架构；影响双库隔离与缓存长上下文设计。
- **[gbrain](https://github.com/garrytan/gbrain)** —— brain-vs-memory 分层；影响命名空间设计、全局/项目隔离和后台 GC 管道。
- **[ENGRAM](https://arxiv.org/abs/2511.12960)** —— 类型化记忆分类 + 稠密检索；验证混合搜索方向。
- **[Karpathy's LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)** —— LLM 维护结构化 wiki 页面；直接启发 Tachi Wiki 系统。

---

## 开源协议

[AGPLv3](LICENSE) © 2026 Tachi Authors。
