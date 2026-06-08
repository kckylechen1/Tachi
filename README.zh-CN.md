<div align="center">
  <img src="assets/banner.png" alt="Tachi Banner" width="800" style="margin-bottom: 20px;" />
  <h1>✧ 藏经阁 (Tachi)</h1>
  <p><strong>专为自主智能体（AI Agents）打造的本地优先记忆与工作流控制平面</strong></p>

  <p>
    <a href="README.en.md">English</a> | <a href="README.zh-CN.md"><b>简体中文</b></a> | <a href="README.md">文言文</a>
  </p>

  <p>
    <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/License-AGPLv3-blue.svg" alt="License: AGPLv3"></a>
    <img src="https://img.shields.io/badge/Language-Rust_Edition_2021-orange.svg" alt="Language: Rust">
    <img src="https://img.shields.io/badge/Integration-MCP_Server-purple" alt="Integration: MCP">
    <img src="https://img.shields.io/badge/Integration-OpenClaw-cyan" alt="Integration: OpenClaw">
    <img src="https://img.shields.io/github/v/release/kckylechen1/tachi.svg" alt="Release Version">
  </p>
</div>

---

## 📖 目录

- [概览](#-概览)
- [Agent 工程控制平面](#-agent-工程控制平面)
- [造物理念 (Why Tachi)](#-造物理念-why-tachi)
- [快速开始: Coding Agents (MCP)](#-快速开始-coding-agents-mcp)
- [快速开始: OpenClaw 框架](#-快速开始-openclaw-框架)
- [核心特性](#-核心特性)
- [因果工作台与记忆关联](#-因果工作台与记忆关联)
- [系统架构](#-系统架构)
- [模型栈](#-模型栈)
- [代码接入与 APIs](#-代码接入与-apis)
- [环境变量配置](#-环境变量配置)
- [性能基准](#-性能基准)
- [贡献指南](#-贡献指南)
- [开源协议](#-开源协议)

---

## 💡 概览

**藏经阁（Tachi）** 是一个专为全自主智能体（Autonomous AI Agents）设计的嵌入式上下文与记忆管理数据库系统。名字源自《攻壳机动队》中的藏经阁科马——通过共享记忆进化出自我意识的 AI 单元。

当前的 AI 记忆模型大多依赖于向量数据库存储扁平化的文本片段。这种设计极易导致 Agent 的上下文视窗膨胀，并在长时间运行中丢失关键的因果和时间联系。

**藏经阁** 引入了由 Rust 高度优化的**层级化、类文件系统管理范式**与**图谱级因果关联**。无论是作为 [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) 服务器独立运行，还是内嵌于 OpenClaw 等原生框架中，它均能提供亚毫秒级的多模态混合语义检索，且**无需任何外部独立数据库依赖**。

---

## 🧭 Agent 工程控制平面

Tachi 已经不只是一个记忆库。它正在变成一套本地优先的 Agent 工程控制平面：Agent 在这里保存经验、发现工具和 Skill、派发 worker、记录验证证据，并把反复出现的 UX / workflow 失败沉淀成下一轮更好的流程。

这套系统里，长期状态应该分层：

- **语义记忆**：记录“我们知道什么”，例如决策、根因、偏好、架构笔记、领域 lesson。
- **工作流账本**：记录“我们怎么干活”，例如 flow、worker mission、tracked docs、handoff、PR / release 状态，以及 `tachi_verify` 的验证结果。
- **运行状态**：记录“现在正在发生什么”，例如 daemon、profile、pending worker、verification gate、Vault 状态和健康警告。
- **性能证据**：作为下一层调度记忆，记录哪个 harness、哪个 Skill、哪种小弟模式真的省时间、抓到真 bug，或制造了不必要噪音。

所以 Tachi 不绑定单一 harness。Claude Code dynamic workflows、Codex subagents、OpenCode、Kimi、GLM、Gemini，以及 HyperMem 这类领域系统，都可以是执行 lane。Tachi 负责把跨工具的硬状态、调度证据、验证门、Vault 策略和记忆边界统一起来。

### 语言约定

面向人的记忆可以用中文写，因为很多判断、取舍和坑用中文更高密度。但代码、CLI、schema、公开文档、commit、PR 和测试名应该用英文。好的中文记忆仍然要保留英文检索锚点，例如 `safe_merge`、`head_sha`、`verify_ops.rs`、PR 编号、测试名、tags 和 entities。

### 同步方向

活的 SQLite 数据库应该留在本地。云端同步不应该直接同步 WAL/SHM 和运行锁，而应该同步加密 bundle、append-only event log、Vault 密文、workflow 摘要、wiki / skill 产物，以及 performance aggregate。这样既保留本地写入速度和安全性，也能让同一个用户和他的 Agent 群体在多台机器上共享长期工程状态。

---

## 🎯 造物理念 (Why Tachi)

### 1. 破局“上下文膨胀”与“因果遗忘”
现在大多数的 Agent 开发都在无脑接入扁平的向量数据库（如 Chroma 或 Pinecone）。但在长期运行中，**随时间堆积的无序记忆碎片会让大模型的上下文急剧膨胀且充满幻觉**。
Tachi 通过**文件系统级的树状命名空间**、**基于 ACT-R 遗忘曲线的混合检索**，以及**图谱级因果网络（Graph Edges）**，将散乱的文本片段重塑成了一套具有时间线和逻辑链条的**“数字海马体”**。

### 2. 终结 MCP 协作生态的“进程之灾”
在多 Agent 并发协作的生态中，如果每个 Agent 都各自去 spawn 子 MCP 进程，必然导致系统资源耗尽、端口冲突和一堆僵尸进程。
Tachi 的 **Hub & Proxy 架构** 完美解决了这个问题：它扮演了一个总调度中心。任何 MCP 工具只需在 Tachi 注册一次，所有 Agent 就能透明地跨库共享调用。Tachi 在底层优雅接管了连接池复用、空闲回收、熔断保护和**环境变量白名单清洗**，彻底打通了全生态的工具壁垒。

### 3. 极端的本地性能与数据主权
AI 的长期记忆和工程级上下文属于核心隐私，绝不应该传给云端数据库。Tachi 完全**不依赖任何外部数据库**，在本地采用纯 Rust 编写的超强底座（`sqlite-vec` + 原生 FTS5 `libsimple`）。
它创新性地实现了**双库物理隔离**（全局通用知识 vs 项目专属架构），在亚毫秒级别即可完成多路融合搜索。

### 4. “极度洁癖”的生命周期管理（长生久视的基础）
Agent 在长达数月的运行中必定会产生大量冗余日志。Tachi 不仅前置了 **AI 噪音拦截（`is_noise_text`）** 和 **无效查询阻断（`should_skip_query`）**，还实装了强悍的后台**定时垃圾回收（GC）**和 **带级联消除的硬粉碎（`delete_memory` CASCADE）**。这些“洁癖”级别的清理机制，确保了 Tachi 在海量交互后依然能保持纯粹，坚决抵制“幻觉病”。

### 5. Skill 插槽：Agent 的外挂神经束 (“Skill-as-a-Tool”)
Tachi 不仅是工具集线器，更是标准 Workflow 的“主板”。通过“Skill 插槽”，开发者可将复杂的 Prompt 链、SOP 流程或领域知识直接打包为纯文本（Markdown/YAML）。Tachi 会在底层将其自动融编为即插即用的原生 MCP 工具（`run_skill`）。Agent 从此彻底告别臃肿的 System Prompt，只需像“插卡带”一样接入 Tachi，即可在极低心智负担下按需获取全新的专业技能。

---

## 🤖 快速开始: Coding Agents (MCP 协议)

适用于 Claude Desktop, Cursor, Gemini CLI 或 AutoGen 等环境。

### 方式一：AI 自动安装（推荐 — 粘贴链接至你的 AI 助手）

> 将下方链接复制到你的 AI 对话中，它会自动阅读安装指南并完成全部配置。
>
> ```
> https://raw.githubusercontent.com/kckylechen1/tachi/v1.5.3/docs/INSTALL.md
> ```

### 方式二：一键脚本安装

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.5.3/scripts/install.sh)"
```

安装脚本将自动：
- 通过 Homebrew 安装 `tachi`（或下载预编译二进制）
- 自动检测本地 AI Agent 配置文件（Claude, Cursor, Gemini 等）
- 向每个检测到的配置中注入 Tachi MCP 服务入口

### 方式三：手动 Homebrew 安装

```bash
brew tap kckylechen1/tachi && brew install tachi
```

然后添加到 Agent 的 MCP 配置文件中：
```json
{
  "mcpServers": {
    "tachi": {
      "command": "tachi",
      "env": {
        "TACHI_PROFILE": "coordinate"
      }
    }
  }
}
```

> **v1.0.1 起默认 profile 变更**：如果不设置 `TACHI_PROFILE`，Tachi 现在默认使用 `standard`（12 个精选工具），不再是 `admin`（全部 148 个工具）。大多数 Agent 用 `standard` 或 `coordinate` 就够了。只有需要直接访问 vault、hub 等底层工具时才设 `TACHI_PROFILE=admin`。
>
> **注意**：配置 `"TACHI_PROFILE": "coordinate"`（或 `"remember"`）权限，可保证 Cursor、Claude Desktop 等外部界面的工具侧边栏**不会**被海量 `tachi_skill_*` 强行霸屏。
> 
> **🤖 针对“脑力平平” Agent 的调教指南**：因为隐藏了展平的直调工具，某些依赖穷举列表的 IDE Agent 可能不知道如何调用框架。建议在你的 Cursor Rules（`.cursorrules`）或全局 System Prompt 中强制补充以下“死命令”：
> *“你已连接 Tachi MCP 记忆中枢。由于安全限制，你无法在工具列表里直接看到所有细分技能。当你需要处理业务任务时，**禁止自己瞎猜或乱写脚本**，你【必须】先调用 `recommend_skill` 工具查询可用的技能库组合，找到对应能力后，【必须】使用 `run_skill` 工具执行它。”*
>
> **注意**：服务器会自动从项目根目录加载 `.env` 文件。完整配置项请参见 `.env.example`。
>
> **数据库安全**：切勿将数据库放置在云同步文件夹中（iCloud、Dropbox、OneDrive）。SQLite WAL 模式与网络文件系统不兼容。

---

## 🦞 快速开始: OpenClaw 框架

Tachi 支持以外部扩展插件的形式桥接运行于 OpenClaw 内核。

**将以下指令发送至你的 OpenClaw 对话窗交由 Agent 处理：**

```text
请协助执行自动化安装流，在 OpenClaw 中扩展部署 Tachi 组件。

1. 一键安装 Tachi + OpenClaw 扩展（推荐）：
   bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.5.3/scripts/install.sh)"

   该脚本会自动：
   - 通过 Homebrew 安装或升级 `tachi`
   - 下载并安装 OpenClaw `tachi` 插件
   - 若检测到 `~/.openclaw/openclaw.json`，则自动写入 `plugins.allow`、`plugins.load.paths` 与 `plugins.slots.memory = "tachi"`

   注：OpenClaw 的 Node.js 原生桥接层已于 v1.0.0 移除，现全面采用原生 Tachi MCP 二进制通信。

   可选：自动扫描本机常见 Agent 配置并写入 Tachi MCP 入口：
   python3 scripts/setup_agent_mcp.py --apply

   可选：自动把本地 Skills / MCP 注册进 Hub：
   python3 scripts/load_skills_to_hub.py
   python3 scripts/register_mcps_to_hub.py
   # 默认会自动同步常见 Agent 的 mcp 配置，并把 Hub 中已注册 MCP 的直连项收敛到 tachi
   # 如需跳过：python3 scripts/register_mcps_to_hub.py --no-sync-agent-config

2. 若脚本未能自动找到 `openclaw.json`，请手动确认：
   - `plugins.allow` 中包含 `tachi`
   - `plugins.slots.memory` 设置为 `tachi`

3. 在项目根目录的 `.env` 中配置 API 密钥（参见 `.env.example`）：
   - `VOYAGE_API_KEY` (向量与重排)
   - `SILICONFLOW_API_KEY` (结构化提取的默认底座)
   - 可选的工作站独立通道覆盖：`EXTRACT_*`, `DISTILL_*`, `SUMMARY_*`, `REASONING_*`

运行注意事项：
- 当前 OpenClaw 的运行时拓扑已经是按 agent 分库：`data/agents/<agent>/memory.db`。
- 根目录下的 `data/memory.db` 现在只作为历史或迁移遗留库保留，不再是新记忆的默认写入目标。
- 如果要升级到最新本地二进制，推荐在 tap formula 更新后执行 `brew reinstall tachi`，或者直接把各 agent 配置指向 freshly built 的 `target/release/memory-server`。
```

---

## ✨ 核心特性

- **⚡ 高性能 Rust 内核 (`memory-core`)**：计分、存储、实体提取与检索等底层引擎完全由 Rust 实现。各路客户端（MCP、OpenClaw 等）均通过稳定、跨平台的 MCP stdio 协议直连 Tachi 原生二进制。最终暴露工具数由内置工具 + 已注册 MCP/Skill 动态决定。
- **🗂️ 文件系统命名空间**：记忆信息摒弃扁平存储，采用 `path` 路径参数（如 `/user/preferences`, `/project/architecture`）进行拓扑层级管理，有效实现业务数据的隔离与精准定向。
- **🔍 三通道分流检索引擎**：
  - **语义级（Semantic）**：内建基于 `sqlite-vec` 的 Voyage-4 向量聚类查询（KNN）。
  - **词法级（Lexical）**：基于 `libsimple` 和 `FTS5` 构建优化的 CJK（中日韩文）全文索引库。
  - **遗忘曲线（Decay）**：借鉴 ACT-R 经典认知模型的时间惩罚衰减机制。
- **🔒 强状态隔离**：引入了确定性并独立于向量的强状态 `hard_state` KV 引擎，适合存放监视清单、明确仓位等避免幻觉影响的事务。
- **🧠 三级自适应上下文分层**：数据接入时将自动被提纯为三个深度：`L0`（摘要提要）, `L1`（段落概览）, 与 `L2`（完整内容）。
- **🔄 两阶演化（记忆去重）**：首创基于数学相似度阈值的 `HARD_SKIP` 与 `EVOLVE` 两阶段查重去重算法。
- **🔌 双库架构**：全局记忆 (`~/.Tachi/global/memory.db`) 跨项目共享（用户偏好、通用知识），每个项目独立数据库 (`.Tachi/memory.db` 位于 git 仓库根目录) 存储项目级上下文。自动检测 git 根目录，自动迁移旧版数据库。无需任何外部独立数据库依赖。
- **🎯 Tachi Hub 能力中心**：统一的 Skill / Plugin / MCP Server 注册与发现中心。注册一次，所有 Agent 均可发现并使用。内置使用追踪、反馈评分、双库继承（项目级覆盖全局）。预置 Skill 数量取决于当前安装的技能包与注册结果。
- **🔀 MCP 代理**：在 Tachi 中注册子 MCP Server，可通过 `tool_exposure=flatten` 展开为 `server__tool`，也可通过 `tool_exposure=gateway` 收敛为 `hub_call` 单入口透传。共享连接池，按需连接，空闲自动清理，熔断器保护，并发控制。环境变量清洗保留 21 个系统关键变量。传输协议别名 (`http`、`streamable-http` → `sse`)。告别僵尸进程。
- **🗑️ 记忆生命周期管理**：完整的生命周期控制——`delete_memory`（永久删除，CASCADE 清理关联数据）、`archive_memory`（软删除，可恢复）、`memory_gc`（清理过期访问历史、事件日志、审计记录）。每条记忆支持 `retention_policy` 字段（`Ephemeral` / `Durable` / `Permanent` / `Pinned`），其中 `Permanent` 和 `Pinned` 条目免受垃圾回收。
- **🏷️ 域感知路由（Domain-Aware Routing）**：通过 `register_domain` 注册域，每域可配独立回收期限（`gc_threshold_days`）、默认保留策略和路径前缀。`get_domain` / `list_domains` / `delete_domain` 管理域。`save_memory` 写入时标注域，`search_memory` 检索时按域过滤，实现业务领域逻辑隔离。
- **🧹 噎声过滤**：保存时自动拦截无价值内容 (`is_noise_text`)，检索时自动跳过无意义查询 (`should_skip_query`)。节省 Embedding API 调用成本，保持记忆库清洁。可通过 `force=true` 绕过。
- **🩺 向量回填维护**：新增 `tachi backfill-vectors --db <path> [--dry-run]`，可扫描任意库缺失的 embedding 并批量补齐，适合迁移后或 agent 本地库向量缺口修复。
- **⏰ 后台自动垃圾回收**：周期性后台 GC 定时器（默认每 6 小时，通过 `MEMORY_GC_INTERVAL_SECS` 可配置）。过期记忆归档窗口通过 `MEMORY_GC_STALE_DAYS` 控制（默认 90 天）。所有 GC 阈值通过 `GcConfig` 全面外部化。保留策略感知归档，尊重每域覆盖与 GC 豁免策略。
- **🕸️ 知识图谱操作**：通过 `add_edge` 和 `get_edges` MCP 工具直接操作记忆图谱。支持创建因果、时序和实体关联边，可附带元数据和权重。
- **🔗 保存时自动链接**：`save_memory` 自动发现共享相同实体的已有记忆，并在它们之间创建图谱边（异步、非阻塞）。默认开启，通过 `auto_link=false` 可禁用。
- **🧾 写入血缘标记**：主要写入链路现在会自动携带 `metadata.provenance`，记录调用工具、最终落库 scope/path、当前注册 agent 身份，以及可选的 `TACHI_PROFILE` / `TACHI_DOMAIN` 标签，便于后续排查冲突与过期事实。
- **👤 灵核身令（Agent Profile）**：每个 Agent 会话可通过 `agent_register` 注册身份——声明 agent ID、显示名、能力集、工具白名单（glob 匹配）及每 Agent 限速覆盖。通过 `agent_whoami` 查询当前身份。
- **🤝 跨界交接（Handoff）**：Agent 退出时通过 `handoff_leave` 留下结构化交接备忘录，下一个 Agent 通过 `handoff_check` 接领。支持跨重启持久化。
- **⚡ 关隘限速（Rate Limiter）**：每会话滑窗限速（RPM）与相同调用爆发检测。可通过环境变量或 `agent_register` 配置。
- **📤 仙诀导出（Skill Export）**：`hub_export_skills` 支持导出为 Claude、OpenClaw、Cursor、通用格式。
- **🧬 仙诀进化（Skill Evolve）**：`skill_evolve` 基于 LLM 分析生成改良版本。
- **🔮 虚灵法契（Virtual Capability）**：Hub 之上的抽象层，支持多路绑定与优先级路由。
- **🔐 加密保险库（Tachi Vault）**：本地加密密钥库，Argon2id + AES-256-GCM。9 个 MCP 工具，支持自动锁定、暴力破解保护、逐秘准入控制、多钥轮换。
- **📧 跨 Agent 看板（Kanban）**：全局看板通信。ACPX 协议扩展。支持工作区与会话上下文筛选。
- **👻 幽灵低语（Ghost Whispers）**：持久化的 Agent 间发布/订阅消息。支持 `ghost_publish`、`ghost_subscribe`、`ghost_ack`、`ghost_reflect`、`ghost_promote`。
- **🏭 神经熔炉（Neural Foundry）**：服务端上下文生命周期管理——`recall_context`、`capture_session`、`compact_context`、`section_build` 等。
- **📦 技能包管理（Skill Packs）**：安装、投射和管理技能集合。支持多 Agent 格式投射。
- **🧠 能力推荐**：`recommend_capability`、`recommend_skill`、`recommend_toolchain`——Tachi 为任务推荐最优工具组合。
- **🩺 记忆治理（Memory Governance）**：`tachi doctor` 巡检 SQLite 数据库健康状态（扩展缺失、WAL 孤子、腐坏、占位等），自动隔离或修复；`tachi manifest` 维护 `~/.tachi/manifest.json` 总册，统一管理已归属数据库的权限与路由；`tachi rescue` 可将 antigravity 混库按项目分拨归宗，源库永留备份。
- **🛡️ 入库把关（Capture Gate）**：`save_memory` 时自动校验 domain、path 合规性、内容质量（防 markdown dump）与最小字数（默认 200，scratch 路径除外）。支持 `warn`（默认）/`enforce`/`off` 三档，由 `TACHI_CAPTURE_GATE` 环境变量控制。
- **🏭 Foundry 任务生命周期加固**：任务终了原因以原子方式写入 metadata，GC Retention 延长至 30 天，新增 `job_status_histogram` 总览。
- **🔌 OpenClaw 总录桥接**：OpenClaw 现在通过 `~/.tachi/manifest.json` 路由自身数据库，默认捕获门坎升至 200 字，MCP JSON 解析增强容错（BOM/空格/括号失衡）。
- **🎭 工具门面 v2 (Facade & Delegation)**：提供紧凑、聚焦任务的统一工具入口 (`tachi_search`, `tachi_web_search`, `tachi_save`) 以及高阶的智能体调度与评估能力 (`tachi_dispatch`, `approve_merge`, `tachi_complete`)，专为跨 Agent 编排设计。
- **📚 Wiki 知识库系统**：内置的持久化知识管理系统 (`tachi_wiki_write`, `tachi_wiki_search`, `wiki_browse`, `wiki_lint`)，针对多智能体间的长效知识共享与沉淀进行了优化。

---

## ⚙️ 因果工作台与记忆关联

为保证 Agent 系统级别的长期逻辑稳定性，Tachi 引入了深度推理组件（注：为减小资源消耗提升极速响应，这些重型处理模型管道已**默认禁用**，可配置环境变量 `ENABLE_PIPELINE=true` 激活）：

### 1. 结构化因果提取管道 (The Causal Extraction Pipeline)
当 Agent 完成一轮复杂交互后，Tachi 的完全异步工作站将被唤醒。利用通过 SiliconFlow 接入的 **Qwen3.5-27B** 模型，工具站将解构 Agent 的日志并提取：
*   `Causes`：触发本次操作的根本起因。
*   `Decisions`：采取方案背后的推演逻辑。
*   `Results`：落地的具体结果状态。
*   `Impacts`：对空间可能存在的前置及后置波及影响。

### 2. 万法归里（原生物理隔离）
那些经由管线推断出的因果记忆以及被蒸馏器提取的经验，将被统一迁移隔离到绝对绝缘的 `derived_items` 表。这样即可免去任何大模型的“自我想象”不慎污染了主体记忆真库 `memories` 发生历史重叠的致命缺陷。

---

## 🏗️ 系统架构

```mermaid
graph TD
    subgraph Clients["支持的集成端"]
        CLI["Tachi CLI (npm)"]
        RMCP["MCP Server (Rust 5.2MB 原生二进制)"]
        OC["OpenClaw Extension (Node.js)"]
        NATIVE["Native Rust Crates"]
    end

    subgraph Cloud["云端 API"]
        VOYAGE["Voyage-4 向量嵌入"]
        SILICON["SiliconFlow Qwen LLM"]
    end

    subgraph Operations["异步工作站"]
        EXTRACT["事实提取器 (Qwen)"]
        DISTILL["上下文蒸馏器 (Qwen)"]
        CAUSAL["因果关系流水线"]
        CONSOLIDATE["记忆碎片合并清理站"]
    end

    subgraph Core["Tachi 核心 (Rust memory-core)"]
        LIB[/"lib.rs (Store API)"/]

        LIB --> SEARCH["五通道混合检索引擎"]
        LIB --> GRAPH["记忆图谱 (PageRank)"]

        SEARCH --> SQLITE[("Embedded SQLite + vec0")]
        GRAPH --> SQLITE
    end

    RMCP -->|"reqwest"| VOYAGE
    RMCP -->|"async-openai"| SILICON
    CLI -->|"MCP stdio"| RMCP
    OC -->|"MCP stdio"| RMCP

    classDef client fill:#3b2e5a,stroke:#8a5cf5,stroke-width:2px,color:#fff;
    classDef cloud fill:#2e3d5a,stroke:#5a9cf5,stroke-width:2px,color:#fff;
    classDef worker fill:#5a4f2e,stroke:#f5c55a,stroke-width:2px,color:#fff;
    classDef rust fill:#5a2e2e,stroke:#f55c5c,stroke-width:2px,color:#fff;
    classDef db fill:#2e5a40,stroke:#5cf58a,stroke-width:2px,color:#fff;

    class CLI,RMCP,OC,NATIVE client;
    class VOYAGE,SILICON cloud;
    class EXTRACT,DISTILL,CAUSAL,CONSOLIDATE worker;
    class LIB,SEARCH,GRAPH rust;
    class SQLITE db;
```

---

## 🧩 模型栈

经过严苛测试，以下是系统默认推荐的组件栈模型，能够在延迟、质量与计算成本中取得最佳平衡：

| 职位角色 | 推荐选用方案 | 原理说明 |
|------|-------------------|------------------|
| **特征向量 / 重排 (Embedding / Rerank)** | [Voyage-4](https://voyageai.com/) | 提供领先的多语种文本检索能力，仍是默认的向量底座。 |
| **事实提取 (Extract)** | [Qwen3.5-27B](https://cloud.siliconflow.cn/) via SiliconFlow | 针对当前 Tachi/OpenClaw 基准测试中，极其稳定可靠的结构化事实提取。 |
| **上下文蒸馏 (Distill)** | MiniMax M2.7 | 能够提供高保真度的压缩块并在多场景复用。可接入兼容 OpenAI 格式的 MiniMax 终点。 |
| **状态快摄 (Summary)** | MiniMax M2.7 | 拥有强大的低 Token 消耗摘要能力，同时保留极高的信号密度。 |
| **架构裁决与进化 (Reasoning / Skill Audit)** | GLM-5.1 via Z.AI | 在架构级判断、进化优先级划分以及 Skill 版本演进的最终决策上表现最优秀。 |
| **快速勘界 (Fast Pre-Audit / Scout)** | Gemini Flash 或 MiniMax M2.7 | (可选) 在重型 GLM 决策介入前，作为低成本低延迟的初筛层使用。 |

实现补充说明：
- 分体架构现已将各个独立通道（Lanes）解耦。
- 开箱使用默认通过 Voyage + SiliconFlow 作为核心，但允许你在 `.env` 中精细覆盖每一个角色的选用配置。

---

## 💻 代码接入与 APIs

供开发者在原生环境中直接内嵌使用核心引流层：

### ⚙️ MCP 工具调用示例（通过任意 MCP 客户端）
```python
# 1. 写入结构化软记忆 (Vector + FTS + Time-衰减，异步摘要)
save_memory(
    text="前端项目强制使用 React 与 Vite 构建，严禁混入 Webpack 相关生态配置。支持 Tailwind。",
    path="/user/project_preferences",
    importance=0.8,
    keywords=["react", "vite", "webpack", "tailwind"]
)

# 2. 调用原生多路混合检索
results = search_memory(
    query="针对当前工程构建工具的禁忌有哪些？",
    path_prefix="/user",
    top_k=3
)

# 3. 强一致性硬状态存储 (0 向量感知，极简 KV 持久化)
set_state(
    namespace="trading",
    key="watchlist",
    value={"600089": "TBEA", "688256": "Cambricon"}
)
```

### ⚙️ 环境变量配置 (`.env`)
通过拷贝根目录下 `.env.example` 文件作为 `.env` 参数映射。
```bash
# Core 向量查询底座
VOYAGE_API_KEY="your_voyage_key_here"

# 同宗同源·总门基础之气 (共享通道)
SILICONFLOW_API_KEY="your_siliconflow_key_here"
SILICONFLOW_BASE_URL="https://api.siliconflow.cn/v1/chat/completions"
SILICONFLOW_MODEL="Qwen/Qwen3.5-27B"

# 千机百晓·特科真气 (独立通道覆盖，皆作可选配)
EXTRACT_API_KEY=""
EXTRACT_BASE_URL=""
EXTRACT_MODEL="Qwen/Qwen3.5-27B"

DISTILL_API_KEY="your_minimax_key_here"
DISTILL_BASE_URL="https://api.minimaxi.com/v1/chat/completions"
DISTILL_MODEL="MiniMax-M2.7"

SUMMARY_API_KEY="your_minimax_key_here"
SUMMARY_BASE_URL="https://api.minimaxi.com/v1/chat/completions"
SUMMARY_MODEL="MiniMax-M2.7"

REASONING_API_KEY="your_glm_key_here"
REASONING_BASE_URL="https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"
REASONING_MODEL="glm-5.1"

# 本地 SQLite 文件（可选 — 默认自动解析为 ~/.Tachi/global/memory.db + 每个项目 .Tachi/memory.db）
MEMORY_DB_PATH="~/.Tachi/global/memory.db"
```

---

## 🏎️ 性能基准 (Benchmarks)

- **原生核心响应 (P95)**：单一混合调度的检索耗时保持在 `< 1.2ms` 范畴。
- **并发提取剥离**：底层的 Python ThreadPool 与协程彻底屏蔽在提取计算时的网络瓶颈，消除对于主事件循环的任何 IO 干扰。
- **Token 保真与利用率**：分层策略 (`L0` → `L1` → `L2`) 搭配严格剪枝，相较传统基于死板文本块的 RAG 设计可大幅降低近 **85%** 的上下文冗余，显著提高大语言模型的指令依从性。

---

## 🤝 贡献指南

我们期待社区的代码提交与架构优化方案。要在本地设立核心开发构建环境：
1. 请确保系统中安装了支持最新版规范的 Rust 编译器 (`rustc>=1.75`)。
2. 安装环境所需的 `maturin` 以及 `cargo-watch` 库。
3. 数据基石入口请查阅：`crates/memory-core/src/lib.rs`。
4. 在任何变更提报前，请落实通过全部编译时断言与代码检查：`cargo test --all`。

请确保所有提交日志遵循 [Conventional Commits](https://www.conventionalcommits.org/) 的基本规范准则。

---

## 🙏 致谢

Tachi 的设计灵感来源于多个在 AI Agent 长期记忆领域的开创性研究：

- **[LongMem](https://github.com/Victorwz/LongMem)** (NeurIPS 2023) — Wang 等人提出的解耦记忆架构，使用冻结的骨干 LLM 作为记忆编码器，自适应残差 SideNet 作为记忆检索器。这启发了 Tachi 的双库隔离设计和缓存长期上下文以避免记忆过期的理念。

- **[gbrain](https://github.com/garrytan/gbrain)** — Garry Tan 为 OpenClaw/Hermes 打造的高度个性化 Agent Brain。其 brain-vs-memory 分层哲学（世界知识存 brain，运行状态存 agent memory，当前上下文存 session）直接影响了 Tachi 的路径命名空间设计与全局/项目级记忆隔离。Dream cycle 概念启发了 Tachi 的后台 GC 与记忆合并管线。

- **[ENGRAM](https://arxiv.org/abs/2511.12960)** (Stockham 等人) — 情景/语义/程序性记忆分类系统证明了：精心的记忆分类配合直接的稠密检索，比复杂的知识图谱更有效。这验证了 Tachi 采用类型化记忆记录（`category`、`domain`、`retention_policy`）配合混合检索的设计方向。

- **[Karpathy's LLM Wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f)** — Andrej Karpathy 提出的 LLM Wiki 理念：让 LLM 维护结构化的 wiki 页面，而非每次重新搜索原始文档。这直接启发了 Tachi 的 wiki 系统：LLM 增量构建和维护持久的知识页面，交叉引用随时间更新，回答时引用已包含综合知识的 wiki 页面——知识随每个来源和每个问题而复利增长。

---

## 📜 开源协议

基于 [AGPLv3 License](LICENSE) © 2026 Tachi Authors。
