---
name: tachi
description: "Tachi memory + task workflow — when and how to call each Tachi tool. Load this skill when starting a non-trivial session, when unsure which tool to call, or when the user asks how Tachi works."
when_to_use: "tachi, 怎么用tachi, tachi memory, tachi workflow, briefing, save memory, extract_facts, checkpoint, 什么时候存, 什么时候用briefing, tachi_memory, tachi_task, tachi_wiki, tachi_skill, tachi_shell, how to use tachi, memory workflow, when to save"
---

# Tachi — 工具使用手册

Tachi 是 agent 的操作系统：记忆、任务、Wiki、技能、流程编排全部走这里。

---

## 核心循环

```
session 开始
    └─ tachi_memory(action='briefing')        ← 读取历史上下文
           ↓
       干活（可能有多个子任务）
           ↓ 每次完成有意义的步骤
       tachi_memory(action='save', ...)        ← 主动存结论（不等 session 结束）
           ↓
       遇到瓶颈
       tachi_memory(action='alerts')           ← 检查阻塞
           ↓
       要中断/交接
       tachi_memory(action='checkpoint', ...)  ← 存进度 + 下一步
```

---

## 每个 action 什么时候用

### `save` — 主动存结论
**触发条件（满足任意一个就存）：**
- 做出了一个决策（选了方案 A 而不是 B，原因是…）
- 找到了根因（bug 的根本原因是…）
- 完成了一个子任务里程碑（阶段 N 通过测试）
- 确认了一个关键命令/文件路径/API 约定
- 修复了一个非显而易见的问题

**不要等 session 结束才存。每次有上述情况就立刻存。**

```
tachi_memory(
  action='save',
  text='根因：migrate_enum_constraints 在重建表时未携带新列 recall_count / tier，导致迁移后列丢失。修复：在 CREATE TABLE memories_new 里补上这三列并用 COALESCE fallback。',
  path='/code-review/sigil/schema-migration-bug',
  keywords=['schema', 'migration', 'sqlite', 'tier'],
  entities=['memcore', 'schema.rs', 'migrate_enum_constraints'],
  project='sigil'
)
```

### `extract_facts` — LLM 从原始文本提取
**触发条件：**
- 手里有一段你没有整理过的原始文本（日志、错误输出、长文档、对话记录）
- 想把它拆成多条可独立搜索的事实

**关键区别：`save` = 你写结论（1 次调用 = 1 条记录）；`extract_facts` = LLM 从原材料提取（1 次调用 → N 条记录）**

```
tachi_memory(
  action='extract_facts',
  text='<粘贴原始 build 日志或错误信息>'
)
```

### `briefing` — session 开始读历史
**触发条件：** 任何非简单问答的任务开始前。
- 等价于：我在做这个任务之前，已经知道什么？

```
tachi_memory(action='briefing', query='当前任务关键词')
```

### `checkpoint` — 中途暂停或交接
**触发条件：**
- 任务没完成，但 session 要结束或上下文要压缩
- 要把任务交给另一个 agent
- 上下文窗口快满了，需要先压缩再继续

**checkpoint 不能替代 save。有最终结论时用 save，中途暂停时用 checkpoint。**

```
tachi_memory(
  action='checkpoint',
  summary='完成了 Phase 1-3，下一步：实现 durable distill 回写，注意...',
  path='/code-review/sigil/phase4-handoff'
)
```

### `alerts` — 卡住了或反复失败
**触发条件：** 同一个问题修了 2 次以上还没解决，或者不知道为什么不工作。

```
tachi_memory(action='alerts')
```

### `search` / `ask` — 任务进行中查历史
- `search`：返回原始记忆列表，自己判断
- `ask`：让 LLM 综合回答一个问题（需要 `synthesize=true`）

---

## 其他核心工具的使用时机

### `tachi_wiki` — 稳定可复用的知识
| 用 wiki 的情况 | 用 tachi_memory 的情况 |
|---|---|
| 调试某类问题的通用模式 | 这次 session 的具体决策 |
| 某个库/系统的架构笔记 | 这次运行的命令输出 |
| 长期有效的 API 约定文档 | 这次找到的根因 |
| 值得以后复用的经验总结 | 中间产物和草稿 |

**在查 web 或从头推断之前，先搜一下 wiki：**
```
tachi_wiki(action='search', query='sqlite migration pattern')
```

### `tachi_task` — 管理子任务
典型流程：
1. `plan` — 复杂任务开始前，搜上下文 + 生成 todo
2. `dispatch` — 把任务切片交给 delegate agent 执行
3. `board` — 轮询进度
4. `merge` — review 后合并 worktree

### `tachi_skill` — 找现成工作流
**任何复杂任务开始前先搜一下，可能已有现成 skill：**
```
tachi_skill(action='discover', query='code review')
tachi_skill(action='discover', query='debug')
tachi_skill(action='discover', query='brainstorm')
```

### `tachi_shell` — 多 agent 协调项目
需要完整 brainstorm → plan → dispatch → review → ship 生命周期时用。
单 agent 任务用 `tachi_task`，多 agent 协调用 `tachi_shell`。

---

## 常见误区

| 误区 | 正确做法 |
|---|---|
| 等 session 结束再存记忆 | 每完成一个有意义的步骤就 save |
| 用 save 存原始日志 | 原始文本用 extract_facts，自己写摘要用 save |
| 用 checkpoint 替代 save | 有最终结论时用 save；中途暂停时用 checkpoint |
| 从头解答问题而不查 wiki | 先 `tachi_wiki(action='search')` |
| 直接写自定义多步逻辑 | 先 `tachi_skill(action='discover')` |
| 忘记传 project/path/keywords | 这三个字段决定未来能不能被 briefing 召回 |

---

## 关键参数

```
tachi_memory(
  action='save',
  text='...',            # 你自己写的结论（不是原始文本）
  path='/scratch/sigil/xxx',   # 或 /code-review/xxx
  keywords=['rust', 'mcp'],    # 用于 FTS 召回
  entities=['memory-server'],  # 具体模块/文件/repo 名
  project='sigil'              # git repo 名，决定存入哪个 DB
)
```

`project` 不传 = 存全局 DB；传了 = 存 `~/.tachi/projects/<name>/memory.db`，下次 briefing 会优先召回。
