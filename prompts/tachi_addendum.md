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

## 常用工具速查

| 场景 | 工具 | 备注 |
|---|---|---|
| 检索历史 | `tachi_memory(action="search")` | 默认 hybrid（vector + FTS + graph + decay）。指定 `path_prefix` 可大幅提速。 |
| 写入事实 | `tachi_save` | `path` 形如 `/<project>/<topic>/<subtopic>`，**不要**用 `/`。 |
| 统一记忆面 | `tachi_memory(action=...)` | `search` / `get` / `save` / `extract_facts` / `briefing` / `ask` / `consolidate` / `progress` / `readiness`。 |
| 任务调度 | `tachi_task(action=...)` | `plan` / `briefing` / `recommend` / `dispatch` / `complete` / `board` / `merge`。PR 生命周期用 `tachi_gh`。 |
| 工作验证 | `tachi_verify(action=...)` | `start` / `record` / `status` / `board`，记录后台验证证据。 |
| 工作 arena | `tachi_arena(action=...)` | `open` / `spawn` / `board` / `collect` / `close` / `reap` / `abort`，跟踪已派外部 Agent。主 Agent 需要派/收 subagent 时可用。 |
| 查关联 | `tachi_memory(action="ask")` | 给 memory_id 或 query，返回邻居 + 边（底层 graph 原语已内化，非 MCP 表面）。 |
| GitHub 生命周期 | `tachi_gh(action=...)` | issue/PR/review/safe-merge/close-loop。 |
| 找技能 | `tachi_skill(action="discover")` | 按自然语言任务找技能。 |
| 执行技能 | `tachi_skill(action="run")` | 入参 `skill_id` + `args`。 |
| 列举技能 | `tachi hub list` (CLI) | 见下文 §tachi hub。 |

## Arena 快速链路

当主 Agent 需要并行探索、审阅、实现草案或外部顾问意见时，用 `tachi_arena`，不要把临时 worker 状态塞进 memory。

1. `tachi_arena(action="open", title=..., objective=...)` 开一个 arena。
2. `tachi_arena(action="spawn", arena_id=..., prompt=..., role="explore|critic|executor|verifier", harness="opencode|claude|gemini-advisor|manual", launch=true|false)` 创建 mission。`launch=false` 会返回 `tracked_prompt`，可手动交给任意 worker。
3. `tachi_arena(action="board", arena_id=...)` 看 mission 状态；不传 `arena_id` 时列出最近 arenas，并显示 `mission_count` / `active_missions` / `pending_collect`。
4. Worker 写好 `result.md` 后，主 Agent 调 `tachi_arena(action="collect", arena_id=..., mission_id=...)` 收结果。linked dispatch 的 `result.md` 会自动导入 mission。
5. 全部收完后 `tachi_arena(action="close", arena_id=...)` 生成 `summary.md`。活跃 mission 会阻止关闭；超时 mission 用 `reap`，主动放弃用 `abort`。

`spawn launch=true` 如果启动失败，会返回 `launch_failed` 和 `prompt_path` / `status_path`，不要重开新 arena；先检查这些路径，再选择修 launcher、重新 spawn，或手动运行 `tracked_prompt`。

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
tachi backfill-vectors --db ~/.tachi/global/memory.db
tachi clean --dry-run           # 安全清理 target/worktree/temp（默认 dry-run）
```

输出与 Hub MCP 工具一致；CLI 默认走 `~/.tachi/global/memory.db`。

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
- **`queue_agent_evolution` 同输入会去重**；重复 queue 会返回 `deduped`，不要靠反复 queue 来“重试”。

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

后台 skill / foundry 调用优先走 **Claude CLI pool**，失败时回退到 `SILICONFLOW_*`。`DISTILL_*` / `REASONING_*` 等旧 lane 仅作兼容保留，新部署不必再配。

<!-- TACHI:END v1.6.2 -->
