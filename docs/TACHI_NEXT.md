# Tachi Next — 综合待办方案

> 综合 Roadmap v1.0 未完成项 + 2026-04-04 巡检发现 + 新增需求

---

## P0 — 即刻修复（已完成 ✅ / 本周内）

| # | 事项 | 状态 |
|---|---|---|
| 0.1 | `ENABLE_PIPELINE=true` 写入 config.env | ✅ 已完成 |
| 0.2 | Skill 收口：gstack 31 个 symlink → 移入 `~/.tachi/skills/`，留 `gstack-index` 索引 | ✅ 已完成 |
| 0.3 | `~/.claude/AGENTS.md` 加 `recommend_skill` 指引 | ✅ 已完成 |
| 0.4 | Amp MCP 配置统一 DB 指向（`settings.json` 加 `--project-db`） | ✅ 已完成 |

---

## P1 — 运维工具链（1-2 周）

### 1.1 `tachi setup` — Onboarding TUI

首次安装后的交互式引导，一站式完成初始化：

```
tachi setup

1/5 🔑 API Keys     — Voyage / SiliconFlow / MiniMax / GLM-5.1，写入 config.env，验证可用性
2/5 🔍 Skills        — 扫描 ~/.claude/skills、~/.codex/、~/.tachi/skills 等
                       策略选择：Hub-only / Hybrid / Keep-local
3/5 🤖 Agents        — 检测 Amp/Claude/Cursor/Gemini/Codex
                       自动注入 MCP 配置
4/5 ⚡ Pipeline      — 是否启用因果提取（ENABLE_PIPELINE）
5/5 🔐 Vault         — 可选初始化 master password
```

框架：Rust `dialoguer` (MVP) → `ratatui` (V2)

### 1.2 `tachi tidy` — 记忆归档整理

解决 22 个 DB 碎片、4297 条记忆散落的问题：

```
tachi tidy

🔍 Found 22 databases, 4,297 memories

Fragmented sources:
  ~/.gemini/antigravity/memory.db       724  → merge into project:antigravity?
  ~/.openclaw/.../main/memory.db        1672 → merge into global?
  ~/Desktop/Sigil/.tachi/memory.db      9    → keep as project:sigil
  ...

Strategy: [Auto-tidy / Interactive / Dry-run report]
```

核心能力：
- 扫描发现所有 memory.db
- 智能分类建议（global vs project vs archive）
- 带 dry-run 的批量迁移（保留 provenance、backfill vectors）
- Agent MCP 配置一致性检查

### 1.3 `recommend_skill` 排序质量修复

当前 bug：所有 skill `uses=0` 时退化为按 description 长度排序，导致 baoyu 系列总排第一。

**验证失败的 case：**
- `recommend_skill(query="code review")` → 返回 `baoyu-markdown-to-html` 第一 ❌（期望 `skill:review`）
- `recommend_skill(query="debug 500 error")` → 返回 `feishu-doc-reader` 第一 ❌（期望 `skill:investigate`）
- `recommend_skill(query="ship this code, create a PR")` → 返回 `skill:ship` 第一 ✅

**实现指引（给 Codex）：**

1. 在 `crates/memory-server/src/` 下搜索 `recommend` 关键词，找到 `recommend_skill` / `recommend_capability` 的实现（可能在 `capability_ops.rs` 或 `hub_ops/` 子目录）
2. 阅读当前评分公式，定位 `score` 的计算逻辑
3. 修改评分：
   - **方案 A（推荐）**：给 query 和每个 skill 的 description 做关键词 token 化（空格+标点分词，转小写），计算 **token overlap ratio**（交集/并集，Jaccard 系数），作为主排序信号
   - **方案 B（更好但更重）**：调 `self.llm.embed_voyage_batch()` 对 query 做 embedding，与 Hub 里 skill description 的 embedding 做 cosine similarity。需要给 `hub_capabilities` 表加 `description_vec BLOB` 列，注册时自动 embed
   - 无论哪个方案，`uses` 计数在 `uses=0` 时权重应为 0，不应影响排序
4. `cargo check -p memory-server` 通过
5. `cargo test -p memory-server` 通过
6. 验证上述三个 case 排序正确

**验收标准：**
```
recommend_skill("code review")        → skill:review 排第一
recommend_skill("debug 500 error")    → skill:investigate 排第一
recommend_skill("ship code, create PR") → skill:ship 排第一
```

---

## P2 — Skill 生态收口（2-3 周）

### 2.1 Skill Pack 自动归集

`tachi setup` 的延伸——把所有 skill 源自动 ingest 进 Hub：

| 源 | 格式 | 数量 | 归集方式 |
|---|---|---|---|
| gstack | SKILL.md | 31 | ✅ 已移入 ~/.tachi/skills/ |
| superpowers | SKILL.md (plugin) | 14 | 只读 scan → Hub index（不动文件） |
| baoyu | SKILL.md | 17 | ✅ 已在 ~/.tachi/skills/ |
| Amp builtin | 内置 | 4 | 不可控，跳过 |

### 2.2 `pack_project` 投射完善

当前 `pack_project` 继续保留主流 agent 的技能投射能力；废弃生态不再作为目标。

---

## P3 — 架构优化（3-4 周）

### 3.1 DB 碎片归一化长期方案

原则：
- **Global DB** = 跨项目知识（用户偏好、通用决策、Tachi 自身配置）
- **Named Project DB** = 项目特定知识（架构、bugfix、changelog）
- **Agent 不是 Project** — Antigravity 不应有独立 project DB，它的跨项目记忆应按项目分流或存 global

迁移计划：
1. `~/.gemini/antigravity/memory.db` (724) → 按 path 前缀分流到 global + 各 project DB
2. `~/.openclaw/extensions/tachi/data/agents/*/memory.db` (1672+182+140) → global（OpenClaw 遗留）
3. `~/.tachi/projects/antigravity/memory.db` (162) → 按 path 分流（/hapi → project:hapi, /tachi → global）
4. 废弃 per-agent DB 模式，统一为 global + per-project

### 3.2 Tachi Desktop Dashboard

已有 `apps/tachi-desktop/`（Vite+React+TS），Ghost in the Shell 风格 UI，端口 5111。

待完成：
- 连接 tachi daemon（端口 6919）的 HTTP endpoints
- 可视化：记忆图谱、Hub 技能列表、Ghost Whispers 消息流、Kanban 看板
- Skill 管理 UI（enable/disable/search/run）
- DB 健康度仪表盘（各 DB 条目数、向量覆盖率、碎片化程度）

### 3.3 Context Diffing 省 Token

Roadmap v1.0 Wave 1 未完成项：
- `sync_memories` 已实现增量同步，但 Agent 端没有利用
- 需要在 `recall_context` 返回时附带 diff 标记，Agent 只注入变化部分

---

## P3.5 — 防御性工程（穿插进行）

### 3.5.1 `tachi doctor` — 自检诊断

今天所有问题（pipeline 没开、DB 碎片、skill 没收口）都是"配了但没生效"或"以为配了其实没配"。需要一个一键自检：

```
tachi doctor

🔑 API Keys
  ✅ VOYAGE_API_KEY      — verified (embed test OK)
  ✅ SILICONFLOW_API_KEY — verified
  ⚠️ DISTILL_API_KEY     — set but unreachable (timeout)
  ❌ REASONING_API_KEY   — not set

⚡ Pipeline
  ✅ ENABLE_PIPELINE=true
  ✅ Causal worker: running
  ⚠️ Distiller: last error 2h ago (DISTILL endpoint timeout)

💾 Databases
  ✅ Global DB: 160 entries, vectors 100%
  ⚠️ Project DB: 9 entries, vectors 100%
  ❌ Fragmentation: 22 DBs detected, run `tachi tidy`

🔌 Agent MCP Configs
  ✅ Amp:     points to ~/.tachi/projects/antigravity/memory.db
  ⚠️ Gemini:  no --project-db, will use CWD (fragmentation risk)
  ❌ Cursor:  tachi not configured

📦 Skills
  ✅ Hub: 75 skills registered
  ⚠️ System prompt: 20 skills (14 from superpowers plugin, not controllable)
  ✅ gstack-index: present

Overall: 3 warnings, 2 errors
```

### 3.5.2 智能 scope 路由

今天存记忆到 project DB 导致其他 agent 看不到。`save_memory` 应该有更智能的默认路由：

- 如果 text 包含 "Tachi" / "tachi" 且 path 以 `/tachi/` 开头 → 建议存 global
- 如果 text 是跨项目知识（用户偏好、通用决策）→ 建议存 global
- 可在返回值里加 `"scope_suggestion": "global"` 提示 Agent

### 3.5.3 `tachi-skill` 收编进 Rust CLI

当前 `tachi-skill` 是独立的 bash 脚本（`/opt/homebrew/bin/tachi-skill`）。应作为 `tachi skill` 子命令收编进 Rust binary：

```
tachi skill scan          # 扫描注册
tachi skill list          # 列出状态
tachi skill enable <name> # 启用
tachi skill disable <name># 禁用
tachi skill install <path># 安装
```

好处：共享 Rust 的 DB 访问逻辑、不依赖 sqlite3 CLI、可集成到 `tachi setup` 流程。

---

## P4 — 高级特性（远期）

| 特性 | 来源 | 说明 |
|---|---|---|
| Shadow Agents | Roadmap Wave 2 | 影子 agent 异步跑任务，通过 Ghost Whispers 汇报 |
| Skill Bounties | Roadmap Wave 3 | 社区 skill 市场，质量评分 + 自动进化 |
| Chronicle | Roadmap Wave 4 | 记忆时间线可视化，因果链追溯 |
| Oracle | Roadmap Wave 5 | 基于记忆图谱的主动洞察推送 |
| Tachikoma Swarm | Roadmap Wave 5 | 多 Tachi 实例间记忆同步（分布式） |

---

## 优先级总结

```
现在 ──→ P0 (已完成) ──→ P1.3 (recommend_skill fix, 1天)
                        ──→ P1.1 (tachi setup, 1周)
                        ──→ P1.2 (tachi tidy, 1周)
                        ──→ P2.1 (skill pack 归集, 2周)
                        ──→ P3.1 (DB 归一化, 2周)
                        ──→ P3.2 (Desktop Dashboard, 持续)
```
