# tachi (OpenClaw Plugin)

OpenClaw 统一记忆插件 — 作为 Tachi kernel 的轻量 runtime facade，负责 agent-facing 的记忆工具面、生命周期 hooks，以及上下文注入。

## 架构

```
OpenClaw Gateway (Node.js)
  └─ tachi plugin (this package)
       └─ MCP client ──→ tachi / memory-server (Rust binary, stdio transport)
             └─ SQLite + sqlite-vec (memory.db)
```

**MCP-only**：插件默认通过 MCP stdio 协议调用 Tachi 二进制，记忆提炼、embedding、rerank、distill、graph maintenance 都在 Tachi 侧完成。

**当前运行时拓扑**：OpenClaw 插件不再维护本地 shadow store、SQLite FTS 或 capture spool；它只负责 hook timing、tool exposure 和调用 Tachi runtime APIs。

## 安装

### 一键安装（推荐）

```bash
curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.5.3/scripts/install.sh | bash
```

该脚本会：
- 通过 Homebrew 安装或升级 `tachi`
- 下载并安装 OpenClaw `tachi` 插件
- 自动更新 `~/.openclaw/openclaw.json` 中的 `plugins.allow`、`plugins.load.paths` 与 `plugins.slots.memory`

### 仅安装 OpenClaw 插件

```bash
curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.5.3/scripts/install_openclaw_ext.sh | bash
```

这是兼容旧流程的包装脚本，等价于执行 `scripts/install.sh --skip-brew`。

## 关键文件

| 文件 | 职责 |
|------|------|
| `index.ts` | 插件入口：仅暴露 `memory_search / memory_save / memory_get / memory_graph`，并在 hooks 中调用 Tachi runtime APIs |
| `mcp-client.ts` | MCP stdio client — 多候选启动、连接恢复、JSON 解析 |
| `config.ts` | 类型定义 + 默认配置（从环境变量读取） |
| `constants.ts` | 环境加载：`.env` + 运行时环境变量 |

## 环境变量

将 `.env.example` 拷贝为 `.env`（项目根目录或插件目录均可），填入运行所需变量。
插件运行时会自动从 `.env` 加载环境变量。

| 变量 | 必填 | 说明 |
|------|------|------|
| `TACHI_BIN` / `OPENCLAW_MEMORY_SERVER_BIN` | 否 | 显式指定 `tachi` / `memory-server` 二进制路径；未设置时会优先使用 Homebrew 安装，其次才回退到本地构建与 PATH |
| `TACHI_OPENCLAW_EXPERIMENTAL_TACHI_TOOLS` | 否 | 设为 `1` / `true` 时，重新暴露 `memory_delete`、`compact_context` 与一组直通 Tachi 的 passthrough tools |
| `MEMORY_BRIDGE_CAPTURE_MIN_CHARS` | 否 | 自动捕获最小字符数阈值 |
| `MEMORY_BRIDGE_CAPTURE_TRIGGERS` | 否 | 自动捕获关键词列表 |

默认每个 OpenClaw agent 使用独立记忆库与独立 `/openclaw/agent-<id>` 命名空间。
当前默认共享规则只有一条：`ops -> main`，即 ops 与 yaya/main 共享同一套记忆。

完整列表见 [`.env.example`](./.env.example)。

记忆提炼、embedding、rerank 和 distill 所需的模型密钥现在应配置在 Tachi 侧，而不是 OpenClaw 插件侧。

## 注册的 Tools

| Tool 名称 | 说明 |
|-----------|------|
| `memory_search` | 语义混合检索（向量 + FTS + rerank） |
| `memory_save` | 显式写入 durable memory |
| `memory_get` | 按 ID 获取单条记忆 |
| `memory_graph` | 只读查看记忆图谱邻域 |

实验性直通 tools 默认关闭；如启用 `TACHI_OPENCLAW_EXPERIMENTAL_TACHI_TOOLS`，插件还会暴露 `memory_delete`、`compact_context`、`tachi_kanban_*`、`tachi_ghost_*`、`tachi_vault_*` 等高阶 passthrough。

## 注册的 Hooks

| Hook | 说明 |
|------|------|
| `before_agent_start` | 调用 `recall_context`，注入 `<relevant-structured-memories>` 上下文 |
| `llm_input` / `llm_output` | 记录模型输入输出与 usage 审计，用于后续分析与 memory 维护 |
| `after_tool_call` | 记录工具调用结果，并统计 subagent / spawn 相关行为 |
| `before_compaction` / `after_compaction` | 记录 compact 前后的窗口与摘要信息，供后续 memory/compaction 分析 |
| `tool_result_persist` | 对 `compact_context` 的持久化结果做补充审计 |
| `subagent_spawned` / `subagent_ended` | 记录子代理生命周期与耗时 |
| `session_end` | 记录会话结束事件，用于 run audit 闭环 |
| `agent_end` | 自动捕获：会话窗口提交到 Tachi，后续维护由 Foundry worker 异步处理 |

`compact_context`、`section.build`、`compact.rollup`、`compact.session_memory` 已经在 Tachi 侧可用；当前插件也已经接入 OpenClaw runtime hooks（包括 compaction / subagent / session audit），这里只保留 MCP-only memory runtime，不再维护本地 shadow store。

## 回滚

1. 在 `openclaw.json` 中禁用 `tachi`
2. 如需清理数据：删除 `data/agents/<agent>/memory.db` 或整个插件 `data/agents/` 目录
3. 重启 OpenClaw gateway
