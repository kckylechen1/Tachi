<!-- TACHI:BEGIN v1.6.2 -->
# Tachi 使用指南 / Tachi Usage Addendum

> 给所有接入 Tachi MCP 的 Agent。**舰长**写于 v1.6.2。把这一段 include 到你的 root prompt（`AGENTS.md` / `CLAUDE.md` / `GEMINI.md`），或让用户手动复制。

## 角色

| 你 | 工具 |
|---|---|
| 接入 Tachi 的 Agent（Claude Code / Codex / Gemini-CLI / Cursor / OpenClaw / Amp / Antigravity） | `tachi_*` MCP facade 工具集 + `tachi` CLI |

## 三条铁律

1. **先搜后写**。任何"我记得我们之前……"念头，先 `tachi_memory(action="search")` 或 `tachi_wiki(action="search")`。命中就引用，未命中再 `tachi_save` / `tachi_memory(action="save")`。
2. **结构化保存**。`tachi_save` 必须带 `path`、`topic`、`entities`、`keywords`。乱写一句话进 `/` 是垃圾，会被 capture gate 拦截或 distill 误吞。
3. **Skill 优先**。复杂任务先 `tachi_skill(action="discover")` / `tachi_skill(action="run")`，不要自己重写 prompt。
4. **原生 subagent 优先**。普通本地派工使用宿主 harness 自带的 subagent；Tachi 默认只负责 memory、policy、claims、ledger、receipt 和 eval。只有用户明确要求、任务必须跨当前会话持久化、跨设备/远程接力，或宿主没有可用 subagent 时，才使用带 typed `staffing_reason` 的 `tachi_staff(action="start")`。

## 常用工具速查

| 场景 | 工具 | 备注 |
|---|---|---|
| 检索历史 | `tachi_memory(action="search")` | 默认 hybrid（vector + FTS + graph + decay）。指定 `path_prefix` 可大幅提速。 |
| 写入事实 | `tachi_save` | `path` 形如 `/<project>/<topic>/<subtopic>`，**不要**用 `/`。 |
| 统一记忆面 | `tachi_memory(action=...)` | `search` / `get` / `save` / `extract_facts` / `briefing` / `ask` / `consolidate` / `progress` / `readiness`。 |
| 任务与回执 | `tachi_task(action=...)` | `intake` / `claim` / `heartbeat` / `handoff` / `release` / `board` / `status` / `complete` / `adjudicate` / `brief` 为 memory/ledger 面；带 `flow_id`/issue/PR 引用的 `status` 返回嵌套 cycle read model。外部 worker 例外见下方 `tachi_staff`，不是默认 subagent。PR 生命周期用 `tachi_gh`。Operator-only dispatch diagnostics stay outside the model-facing Task surface. |
| 工作验证 | `tachi_verify(action=...)` | `start` / `record` / `status` / `board`，记录后台验证证据。 |
| 外部 staffing | `tachi_staff(action=...)` | `start`（要求 typed `staffing_reason`）派出可跟踪 worker，`status` 读运行状态；普通本地并行仍使用宿主原生 subagent，不因 Tachi 存在而切换执行器。 |
| 查关联 | `tachi_memory(action="ask")` | 给 memory_id 或 query，返回邻居 + 边（底层 graph 原语已内化，非 MCP 表面）。 |
| GitHub 生命周期 | `tachi_gh(action=...)` | issue/PR/review/safe-merge/close-loop。 |
| 找技能 | `tachi_skill(action="discover")` | 按自然语言任务找技能。 |
| 执行技能 | `tachi_skill(action="run")` | 入参 `skill_id` + `args`。 |
| 列举技能 | `tachi hub list` (CLI) | 见下文 §tachi hub。 |

## Staffing 快速链路

只有用户明确要求、任务必须跨当前会话持久化、跨设备/远程接力，或宿主没有可用 subagent 时，才用 `tachi_staff` 派出可跟踪 worker；普通本地并行的 worker 生命周期仍由宿主 harness 管理，不要把临时 worker 状态塞进 memory。

1. `tachi_staff(action="start", task=..., staffing_reason=...)` 派出 worker。`staffing_reason` 是必填 typed 值：`explicit_user_request` | `durable_cross_session` | `cross_device_remote` | `native_subagent_unavailable`；Tachi 可用性、并行度或追踪需求本身都不算理由。可选 `profile`、`worker`、`flow_id`、`issue_ref`、`pr_ref` 绑定上下文。
2. 响应带回 canonical `dispatch_id`；用 `tachi_staff(action="status", dispatch_id=...)` 查询该 worker 的运行状态。
3. Worker 进程生命周期、超时与结果落盘由承接的 staffing kernel 管理；不再有 arena 的 open/spawn/board/collect/close 流程——该 API 已随 #1319 移除。

## tachi_save 范式

```jsonc
// ✅ 好
{
  "text": "Tachi v1.5 引入 tachi_verify，用于记录后台验证证据并供 safe_merge gate 消费。",
  "path": "/tachi/workflow/verification",
  "topic": "verify-ledger",
  "category": "decision",
  "entities": ["tachi_verify", "safe_merge", "verification.json"],
  "keywords": ["verify", "merge", "evidence"],
  "importance": 0.8
}

// ❌ 坏（会被 capture gate 拦截或 GC / distill 误处理）
{ "text": "fixed it" }
```

## `tachi` CLI

用 `tachi` 子命令不开 MCP 也能巡检万宝楼、管理库、触发维护：

```bash
tachi hub list                  # 列全部已注册技能/插件/MCP
tachi hub list --type skill     # 只看 skill
tachi hub show skill:code-review
tachi hub packs                 # 已安装的 pack
tachi hub stats                 # 总量统计
tachi doctor                    # 巡检 SQLite 藏库健康
tachi env plan                  # 查看 .tachi/vault.env 绑定（不解密）
tachi env sync --keychain       # 预览 env.generated（默认不写盘）
tachi env sync --apply --keychain  # 写入 .tachi/env.generated（0600）
tachi backfill-vectors --db ~/.tachi/global/tachi-memory.db
tachi clean --dry-run           # 安全清理 target/worktree/temp（默认 dry-run）
```

输出与 Hub MCP 工具一致；CLI 默认走 `~/.tachi/global/tachi-memory.db`。

## Path 命名约定

- `/<project>/<topic>/...` — 项目内事实（tachi、sigil、openclaw、hapi、quant、hyperion、antigravity、wiki）
- `/user/<topic>` — 用户层级 preference / credential
- `/ghost/messages/...` — Ghost 消息（不要手动写）
- `/foundry/...` — **保留给 foundry 自己**，外部不要写
- `/handoff/...` / `/kanban/...` — 协调上下文，retention 自动 pinned

## 反模式（别犯）

- 不要往 `/foundry/*` 手动写。
- 不要 `path = "/"` + `topic = ""`。
- 不要把整段对话当 text 塞进 `tachi_save`；用 `extract_facts` 或 `ingest_event`。
- 不要在 ghost topic 上 publish 后立刻自己 subscribe 同一 topic 自我喂养。
- 不要无 `coherence_key`/`topic`/`entity` 的高频写入 —— 会被 distill 跳过，浪费配额。
- 不要把活的 SQLite 库放进 iCloud / Dropbox / OneDrive；同步加密 bundle 和 event log 即可。
- **`tachi env sync` 必须加 `--apply` 才写盘**；默认只是 preview。`.tachi/env.generated` 含明文，勿 commit。

## 出错怎么办

| 现象 | 原因 | 处理 |
|---|---|---|
| `no such column: retention_policy` | 老库 schema drift | 重启 tachi-server（启动会 migrate），或 `tachi doctor --fix` |
| `vec0 module not loaded` | sqlite-vec 扩展未装 | brew 安装的二进制自带；裸 `sqlite3` CLI 没有 |
| `distill produced empty` | bucket 不满 `FOUNDRY_DISTILL_MIN_BATCH=3` | 正常，等够 3 条同 topic/entity 的记忆再触发 |
| `tool not found` | Profile 隐藏了该工具 | 检查 `TACHI_PROFILE`，必要时切 `admin` 或加 `TACHI_EXTRA_TOOLS` |
| `database is locked` | 多实例同时写同一 DB | 确保每个 DB 只有一个 Tachi 进程 |

## 模型栈（Phase 2）

日常部署只需两把钥匙：

- `VOYAGE_API_KEY` — 嵌入 / rerank
- `SILICONFLOW_API_KEY` — 抽取、摘要、蒸馏（`Qwen/Qwen3.5-27B`）

后台抽取 / 摘要 / 每日蒸馏调用直接走已配置的 OpenAI-compatible API lane；已配置的不同 provider 回退耗尽时才显式记录 `LANE_OUTAGE`。`FOUNDRY_DISTILL_BACKEND=claude_cli` 是兼容选择器，仍调用 `call_distill_llm`，不会启动 Claude 子进程。常规 reasoning/chat 先尝试 Claude CLI，再回退到配置好的 reasoning API；provider-only 调用刻意跳过 CLI。`DISTILL_*` / `REASONING_*` 仍是 resolver 消费的配置前缀，可按各自优先级覆盖。

<!-- TACHI:END v1.6.2 -->
