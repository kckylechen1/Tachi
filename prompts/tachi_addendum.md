<!-- TACHI:BEGIN v0.16 -->
# Tachi 使用指南 / Tachi Usage Addendum

> 给所有接入 Tachi MCP 的 Agent。**舰长**写于 v0.16。把这一段 include 到你的 root prompt（`AGENTS.md` / `CLAUDE.md` / `GEMINI.md`），或让用户手动复制。

## 角色

| 你 | 工具 |
|---|---|
| 接入 Tachi 的 Agent（Claude Code / Codex / Gemini-CLI / Cursor / OpenClaw / Amp / Antigravity） | `tachi-*` MCP 工具集 + `tachi hub` CLI |

## 三条铁律

1. **先搜后写**。任何"我记得我们之前……"念头，先 `tachi_search_memory`。命中就引用，未命中再 `tachi_save_memory`。
2. **结构化保存**。`save_memory` 必须带 `path`、`topic`、`entities`、`keywords`。乱写一句话进 `/` 是垃圾，会被 distill 误吞。
3. **Skill 优先**。复杂任务先 `tachi_recommend_skill` / `tachi_run_skill`，不要自己重写 prompt。

## 常用工具速查

| 场景 | 工具 | 备注 |
|---|---|---|
| 检索历史 | `tachi_search_memory` | 默认 hybrid（vector+FTS+symbolic）。指定 `path_prefix` 可大幅提速。 |
| 抽取事实 | `tachi_extract_facts` | LLM 自动从一段对话抽 fact 入库。比手动 save 更省 token。 |
| 写入事实 | `tachi_save_memory` | `path` 形如 `/<project>/<topic>/<subtopic>`，**不要**用 `/`。 |
| 查关联 | `tachi_memory_graph` | 给 memory_id 或 query，返回邻居 + 边。 |
| 跨 Agent 投递 | `tachi_post_card` / `tachi_check_inbox` | Kanban 模式，比 ghost 重，适合"任务交接"。 |
| 实时广播 | `tachi_ghost_publish` / `tachi_ghost_subscribe` | 轻量 pub/sub。`tachi_ghost_whisper` 是别名。 |
| 跨 session 交接 | `tachi_handoff_leave` / `tachi_handoff_check` | session 开头先 check。 |
| 找技能 | `tachi_recommend_skill` `tachi_recommend_capability` `tachi_recommend_toolchain` | 按自然语言任务找技能。 |
| 执行技能 | `tachi_run_skill` | 入参 `skill_id` + `args`。 |
| 列举技能 | `tachi hub list` (CLI) | 见下文 §tachi hub。 |

## save_memory 范式

```jsonc
// ✅ 好
{
  "text": "Sigil v0.16 引入 coherent_distill_buckets，按 topic/entity 分桶蒸馏。",
  "path": "/sigil/architecture/distill",
  "topic": "foundry-distill",
  "category": "decision",
  "entities": ["coherent_distill_buckets", "process_memory_distill_job"],
  "keywords": ["distill", "foundry", "coherence"],
  "importance": 0.8
}

// ❌ 坏（会被 GC / distill 误处理）
{ "text": "fixed it" }
```

## `tachi hub` CLI

用 `tachi hub` 子命令不开 MCP 查技能/包/虚拟绑定（只有一个 `tachi` 可执行文件）。

```bash
tachi hub list                  # 列全部已注册技能/插件/MCP
tachi hub list --type skill     # 只看 skill
tachi hub show skill:code-review
tachi hub packs                 # 已安装的 pack
tachi hub stats                 # 总量统计
tachi hub doctor                # 轻量 schema 巡检（与 `tachi doctor` v2 不同）
```

输出与 `tachi_recommend_*` MCP 工具一致；CLI 走的是 `~/.tachi/global/memory.db`。

## Path 命名约定

- `/<project>/<topic>/...` — 项目内事实（hapi、quant、sigil、openclaw、tachi、antigravity、hyperion、wiki）
- `/user/<topic>` — 用户层级 preference / credential
- `/ghost/messages/...` — Ghost 消息（不要手动写）
- `/foundry/...` — **保留给 foundry 自己**，外部不要写。v0.16 之前误写的 47 条已硬删。

## 反模式（别犯）

- 不要往 `/foundry/*` 手动写。
- 不要 `path = "/"` + `topic = ""`。
- 不要把整段对话当 text 塞进 save_memory；用 `extract_facts` 或 `ingest_event`。
- 不要在 ghost topic 上 publish 后立刻自己 subscribe 同一 topic 自我喂养。
- 不要无 `coherence_key`/`topic`/`entity` 的高频写入 —— 会被 distill 跳过，浪费配额。

## 出错怎么办

| 现象 | 原因 | 处理 |
|---|---|---|
| `no such column: retention_policy` | 老库 schema drift | `tachi hub doctor --fix` 或重启 memory-server（启动会 migrate） |
| `vec0 module not loaded` | sqlite-vec 扩展未装 | brew 安装的二进制自带；裸 `sqlite3` CLI 没有 |
| `distill produced empty` | bucket 不满 `FOUNDRY_DISTILL_MIN_BATCH=3` | 正常，等够 3 条同 topic/entity 的记忆再触发 |
| `VOYAGE_RERANK_API_KEY missing` | 未配置 rerank | 可选项，不配置不影响核心检索 |

<!-- TACHI:END v0.16 -->
