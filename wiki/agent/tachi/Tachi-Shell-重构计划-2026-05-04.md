# Tachi Shell 重构计划 — 2026-05-04

## 背景

本计划承接 Tachi 工具面 v2 之后的下一阶段：把 Skill/SOP、MCP facade、异步 dispatch、Kanban 状态、GitHub 生命周期和 memory artifact 串成一个统一入口。

核心结论：新增 `tachi_shell` 作为面向 agent 的 flow orchestration facade。`tachi_shell` 不替代底层 `tachi_task`、`tachi_skill`、`tachi_memory`、`tachi_gh`，而是把它们按强制工作流组合起来。

## 目标

1. **Skill 变成 workflow gate**：不是 optional advice，而是每个阶段的必经 SOP。
2. **长任务异步化**：测试、gitleaks、review、commit、PR、merge、distill 不挂主 chat。
3. **状态外置**：所有 flow run 都落到 Kanban、events、artifacts、memory。
4. **GitHub 只接 lifecycle**：memo/design/plan 需要推进时才 promote 到 issue / PR。
5. **Dispatch 派 clanker 干活**：Claude Code、Codex、runner 等是 concrete execution runtimes。

## 命名

### 外部 MCP facade

- `tachi_memory`：记忆、memo、handoff、artifact、promote。
- `tachi_shell`：skill-gated async flow orchestration。
- `tachi_skill`：skill registry / runner。
- `tachi_wiki`：compiled knowledge base。

### 内部概念

| 名称 | 含义 |
|---|---|
| `flow` | 一次可追踪 workflow run |
| `stage` | flow 当前阶段：brainstorm / plan / dispatch / review / ship / distill |
| `clanker` | 具体执行机佬：Claude Code / Codex / runner / Gemini 等 |
| `kanban` | durable async task 状态视图，替代 `board` 产品语义 |
| `artifact` | 计划、审查、日志、PR body、总结等持久产物 |
| `handoff` | global memo，不默认进 project DB |

## `tachi_shell` actions

| action | 作用 | 内部依赖 |
|---|---|---|
| `brainstorm` | 需求澄清和方案分叉 | `tachi_skill`, `tachi_memory` |
| `plan` | 生成执行计划 | `tachi_skill`, `tachi_memory` |
| `dispatch` | 派 clanker 到后台执行 | `tachi_task`, `tachi_memory` |
| `kanban` | 查看所有异步 flow/task 状态 | `tachi_task(action=board)` |
| `status` | 查看单个 flow/task 详情 | `tachi_task`, run artifacts |
| `review` | 代码审查 gate | `tachi_skill`, `tachi_task`, optional Codex |
| `ship` | 测试、gitleaks、commit、push feature branch、PR、review gate、merge PR、distill | `tachi_task`, `tachi_gh`, `tachi_complete` |

## Skill gate 映射

| stage | required skill |
|---|---|
| `brainstorm` | `skill:superpowers-brainstorming` |
| `plan` | `skill:superpowers-writing-plans` |
| `dispatch` | `skill:superpowers-executing-plans` / `skill:superpowers-subagent-driven-development` |
| `review` | `skill:superpowers-requesting-code-review` |
| `ship` | `skill:superpowers-finishing-a-development-branch` |

规则：调用 `tachi_shell` 进入阶段时，Tachi 内部必须加载并执行对应 skill gate；主 agent 不需要手动调用 `tachi_skill`。

## Meta skill 与 SkillHub 边界

SkillHub 放可发现、可复用、可由 agent 显式调用的能力；`superpowers`、Gastown 协作纪律、dispatch/ship 这种流程型 SOP 属于 **meta skill**，不应该混进普通 SkillHub 搜索面让 agent 自己选择。

### 分类

| 类型 | 是否进 SkillHub | 调用方式 | 例子 |
|---|---|---|---|
| 普通 skill | 是 | `tachi_skill(discover/run)` 或按需注入 | domain skill、工具使用指南、项目知识 |
| meta skill | 默认不进普通发现面 | `tachi_shell` / `dispatch` 按 stage 强制注入 | Superpowers workflow、Gastown policy、ship ritual |
| gstack / super-skillset index | 可进 SkillHub，但作为目录/索引 | 人或 agent 显式查阅，不能自动替代 stage gate | `gstack-index` |

### 注入原则

1. `tachi_shell` 根据 `stage` 决定需要注入哪些 `.md`。
2. `dispatch` 派 clanker 时，不要求 clanker 自己去搜 SkillHub；prompt 里直接写明要读取并执行哪些注入文件。
3. 注入文件必须随 run 落盘，形成可审计 artifact。
4. clanker 完成后必须写开始时间、结束时间、执行结果、验证证据、阻塞和下一步。
5. meta skill 的版本和来源要写入 `status.json` / `events.jsonl`，避免事后不知道按哪个 SOP 执行。

### Prompt 形态

派 clanker 时，`dispatch` 生成的 prompt 应该类似：

```text
You are executing Tachi flow <flow_id>, stage <stage>.

Read and follow these injected SOP files before changing code:
- .tachi/runs/<flow_id>/injected/superpowers-writing-plans.md
- .tachi/runs/<flow_id>/injected/gastown-policy.md

You must:
1. Record start time in events.jsonl.
2. Execute the stage instructions.
3. Write artifacts under .tachi/runs/<flow_id>/artifacts/.
4. Record validation evidence.
5. Record final status, blockers, and next action.
```

这等价于当前 Claude Code MCP 的 tracked 模式：不是靠聊天记忆，而是把 prompt、plan、status、events、result 全部落盘。

## MD 指令包与后台执行模型

当前在 Antigravity / Windsurf 中已经有人工版流程：

1. 主 agent 先让 Codex 审查方案。
2. Codex 输出 `output.md` / `result.md`。
3. 主 agent 读取这个 markdown，提取执行建议。
4. 主 agent 再派 Claude Code tracked 修改代码。
5. Claude Code tracked 再输出 `result.md`。

`tachi_shell` 要做的是把这个模式产品化并异步化：主 agent 不再把自己挂在中间等待，而是通过 `writing-plans` / dispatch 组装出一个 **subagent prompt artifact**，交给后台 clanker 读取并执行。

### Subagent prompt artifact

每次 dispatch / review / ship 都应生成一个明确的执行包：

```text
.tachi/runs/<flow_id>/
  instruction.md        # 由 writing-plans / dispatch 组装出的 subagent prompt
  input.md              # 上游 reviewer / planner 的输出，可选
  injected/
    superpowers-*.md
    gastown-policy.md
  status.json
  events.jsonl
  artifacts/
```

`instruction.md` 本质上就是“要交给 subagent 的工作”被组装成的 markdown prompt。它是 clanker 的入口，不是聊天上下文的一段临时文字。它应该包含：

1. 本次 flow/stage 的目标。
2. 必须读取的上游 markdown：例如 Codex review output、plan、spec。
3. 必须读取的 meta skill / SOP 文件。
4. 允许修改的范围和禁止事项。
5. 必须执行的验证命令。
6. 必须写回的 artifact：`result.md`、`validation.md`、`status.json`、`events.jsonl`。

### 异步化目标

当前 tracked 模式能落盘，但仍由主 agent 同步等待结果；`tachi_shell` 的目标是：

```text
主 agent / tachi_shell 调用 writing-plans 生成 instruction.md
  -> tachi_shell 创建 flow run
  -> dispatch 后台 clanker
  -> clanker 读取 instruction.md 并执行
  -> clanker 写 result.md / status.json / events.jsonl
  -> kanban/status 展示进度
  -> 主 agent 之后读取结果，不需要一直挂着
```

这允许多个 flow 并行执行。主 agent 只负责：

- 创建/批准 instruction packet；
- 触发 writing-plans 把任务组装成 subagent prompt；
- 查看 `kanban` / `status`；
- 在需要人工判断时介入；
- 读取最终 `result.md` 并推进下一阶段。

### Reference A：Antigravity 人工串联流程

Antigravity 侧已有一个人工版 orchestration 参考：

```text
主 agent：“先去问问 codex 的意见，然后让 claudecode 改好”
  -> Codex 审查并输出 output.txt / output.md
  -> 主 agent 读取 Codex 输出，提炼关键调整建议
  -> 主 agent 组装 Claude Code tracked prompt
  -> Claude Code 读取 prompt 执行改动
```

这个流程已经体现了 `tachi_shell` 的核心形态：

1. reviewer clanker 的输出是 markdown artifact；
2. coordinator 读取 artifact 后生成下游 subagent prompt；
3. executor clanker 根据 prompt 执行；
4. 当前问题是主 agent 同步挂着，不能后台并行。

`tachi_shell` 的目标不是改变这个工作方式，而是把它变成 durable async run。

### Reference B：Claude Code tracked plan review run

本计划已有一次真实 tracked 模拟：

```text
.agent/claude-code-runs/2026-05-04T16-11-33-020Z-review-tachi-shell-plan/
  prompt.md
  plan.md
  result.md
  status.json
  stdout.log
  stderr.log
```

输入 prompt 摘要：

```text
你是 Tachi Shell flow 的计划审阅 clanker。本次是模拟 tachi_shell + dispatch + injected meta skill 的 tracked 流程。

读取：
- wiki/agent/tachi/Tachi-Shell-重构计划-2026-05-04.md
- skill/superpowers/skills/writing-plans/plan-document-reviewer-prompt.md

按 reviewer prompt 审阅计划，输出中文 review：
- 总体结论
- P0/P1/P2
- 具体修改建议
- 下一步执行顺序
- 是否建议进入 implementation
```

该 run 产物：

- `prompt.md`：主 agent 组装出的 subagent prompt。
- `plan.md`：Claude Code tracked 生成的执行计划。
- `result.md`：审阅结果，结论为 `Approve with Comments`。
- `status.json`：tracked run 状态。

这就是 `instruction.md` / `status.json` / `result.md` / artifact 模型的现成样板。

## Gastown 嵌入

Gastown 不作为新工具暴露，而是作为 `tachi_shell` 的协作协议：

1. **状态外置**：flow 状态必须写入 Kanban / events / artifacts / memory。
2. **Mayor / clanker 分工**：主 agent 是 coordinator；clanker 是后台执行体。
3. **Convoy 思维**：多任务 flow 要有 `flow_id` / owner / stage / done condition。
4. **Propulsion loop**：定位当前步骤 → 执行最小可验证动作 → 记录结果 → 推进下一步。
5. **完整 handoff**：失败、阻塞、完成都必须写明文件、验证、风险、下一步。

## Flow 数据模型草案

```json
{
  "flow_id": "flow_...",
  "title": "...",
  "stage": "dispatch",
  "state": "running",
  "scope": "project",
  "project_root": "/path/to/repo",
  "issue": "owner/repo#123",
  "pr": "owner/repo#456",
  "branch": "feat/...",
  "clanker": "claude-code",
  "required_skill": "skill:superpowers-executing-plans",
  "artifacts": [
    "prompt.md",
    "plan.md",
    "events.jsonl",
    "test.log",
    "gitleaks.json",
    "diff-summary.md"
  ],
  "next_action": "wait_for_preflight",
  "blocked_reason": null,
  "created_at": "...",
  "updated_at": "..."
}
```

## Artifact 布局

沿用并强化现有 dispatch run 目录：

```text
.tachi/runs/<flow_id>/
  instruction.md
  input.md
  prompt.md
  plan.md
  status.json
  events.jsonl
  injected/
    superpowers-*.md
    gastown-policy.md
  artifacts/
    test.log
    gitleaks.json
    diff-summary.md
    review.md
    pr-body.md
    ship-summary.md
```

## 实施阶段

### Phase 1：Spec 与 facade 壳

目标：先建立 `tachi_shell` 参数、路由和空实现，不改底层行为。

任务：
1. 新增 `TachiShellParams`。
2. 注册 `tachi_shell` MCP tool。
3. 支持 actions：`brainstorm`, `plan`, `dispatch`, `kanban`, `status`, `review`, `ship`。
4. `kanban` 先转发到底层 `tachi_task(action="board")`。
5. `status` 先读取现有 run/status 信息。
6. standard profile 暴露 `tachi_shell`，逐步弱化 `tachi_task` 作为用户入口。

验收：
- `cargo test -p memory-server` 通过。
- `tachi_shell(action="kanban")` 能返回现有任务状态。

### Phase 2：Skill gate 接入

目标：让 `tachi_shell` 每个阶段强制加载对应 skill。

任务：
1. 建立 stage → meta skill 文件映射。
2. 在 `brainstorm` / `plan` / `dispatch` / `review` / `ship` 前解析并落盘注入文件。
3. 将注入清单、版本、来源写入 `status.json` / `events.jsonl`。
4. 派 clanker 时在 prompt 中强制要求读取注入文件并执行。
5. meta skill 缺失时返回明确 warning，不静默跳过。

验收：
- 每个 action 返回 required meta skill / injected file 信息。
- `.tachi/runs/<flow_id>/injected/` 中能看到实际注入的 SOP。
- 缺失 meta skill 时不会进入误导性成功状态。

### Phase 3：异步 flow run

目标：把长任务变成 durable async run。

任务：
1. 创建 `flow_id` 和 `.tachi/runs/<flow_id>/`。
2. 写 `instruction.md` / `status.json` / `events.jsonl`。
3. `dispatch` 创建后台任务后立即返回。
4. 后台 clanker 从 `instruction.md` 读取任务，而不是依赖主 chat 上下文。
5. `kanban` 聚合 running / blocked / waiting_approval / done / failed。
6. 失败时保留 artifact 和 next action。

验收：
- 主 MCP 调用不等待长任务完成。
- run 目录中能看到可独立执行的 `instruction.md`。
- crash/restart 后能从 status/artifacts 恢复状态。

### Phase 4：Ship ritual

目标：实现 `ship` 的后台交付流程。

发布顺序默认采用 PR-first，不直推主分支：

```text
1. feature branch 完成
2. test
3. push feature branch
4. open PR
5. PR gate：CI checks + review gate
6. merge PR
7. deploy/release
```

除非人类明确授权“直推 main / protected branch”，否则 `ship` 只能生成 PR、等待 review gate 和 merge gate。

任务：
1. preflight：`git status`, tests, fmt/clippy, `gitleaks`。
2. diff summary：生成变更摘要和风险点。
3. review gate：默认 Claude Code 自审；高风险可 Codex gate。
4. commit：生成 commit message；默认可配置是否自动 commit。
5. push feature branch：推送工作分支，不直接推主分支。
6. PR：生成 PR body；通过 `tachi_gh` 创建/更新 PR。
7. merge：PR gate 通过后合并；默认需要确认；可配置自动 merge。
8. distill：完成后调用 `tachi_complete` / memory save。

验收：
- `ship` 能异步跑完整 preflight。
- gitleaks 结果保存为 artifact。
- PR body / ship summary / CI 结果保存为 artifact 和 memory。

### Phase 5：Memory / GitHub lifecycle

目标：统一 memo、handoff、artifact、issue/PR link。

任务：
1. handoff 默认保存到 global memo。
2. project design / plan / review / ship summary 默认保存到 project memory。
3. `promote` 将 memo/design/plan 转成 GitHub issue，并回写 link metadata。
4. PR / issue / flow 互相关联。

验收：
- memo 不会强制变 issue。
- issue/PR 只在 lifecycle 需要时创建。
- memory entry 可追溯到 flow / issue / PR。

## 关键代码区域

预计涉及：

- `crates/memory-server/src/tools.rs`：tool registration / call routing。
- `crates/memory-server-params/src/facade.rs`：facade 参数。
- `crates/memory-server/src/profiles.rs`：standard/delegate profile 暴露。
- `crates/memory-server/src/dispatch_ops.rs`：后台 clanker 执行。
- `crates/memory-server/src/task_ops.rs` 或现有 `tachi_task` facade：kanban/status 复用。
- `crates/memory-server/src/skill_ops.rs` / Hub skill runner：skill gate。
- `crates/memory-server/src/gh_ops.rs`：GitHub issue / PR lifecycle。
- `crates/memory-server/src/memory_ops.rs` / save handlers：artifact/memo metadata。

## 风险与约束

1. 不要一次性删除旧工具；先 facade 包装，再 profile 收敛。
2. 不要让 `tachi_shell` 变成另一个巨型文件；每个 action 拆独立 handler。
3. 不要把 handoff 默认写 project；handoff 是 global memo。
4. 不要让长任务阻塞 MCP call。
5. 不要默认自动 push / merge；需要 policy gate。
6. 不要让 clanker 自己绕过 skill gate。
7. 不要新建独立 memo/artifact 系统；继续用 memory + metadata。
8. 不要把 meta skill 混入普通 SkillHub 发现面，避免 agent 把 workflow gate 当普通可选 skill。

## 推荐落地顺序

1. 写 `tachi_shell` facade 壳和 action schema。
2. 接 `kanban/status`，复用现有 `tachi_task board`。
3. 接 meta skill 注入，只记录和返回，不先执行复杂工作流。
4. 接 async dispatch flow run。
5. 实现 `ship` preflight：tests + gitleaks + diff summary。
6. 接 GitHub PR lifecycle。
7. 接 distill 和 eval ledger，为 clanker 路由积累经验。

## 完成标准

当以下流程能跑通时，视为 MVP 完成：

```text
tachi_shell(action="plan")
  -> required skill loaded
  -> plan artifact saved

tachi_shell(action="dispatch")
  -> async clanker run created
  -> kanban shows running

tachi_shell(action="ship")
  -> tests + gitleaks run in background
  -> PR body artifact generated
  -> memory summary saved
  -> status becomes done / waiting_approval / blocked
```

## P0/P1 澄清（2026-05-05 补丁）

以下条目对应 plan reviewer 在 `result.md` 中的 P0/P1 反馈，作为 MVP 实施的硬约束。

### P0-1：`flow_id` vs `dispatch_id`

- `dispatch_id` 是底层 `dispatch_ops` 现有概念，对应单次 clanker 子进程运行。MVP 不重命名。
- `flow_id` 是 `tachi_shell` 引入的更高层概念，对应一次跨多个 stage 的 workflow run。
- 关系：一个 `flow_id` 在执行期间可包含 0..N 个 `dispatch_id`。
- MVP 实现：`tachi_shell` 创建 `flow_id`（格式 `flow_<utc>_<slug>`），并在 `status.json` 中记录由本 flow 触发的 `dispatch_ids: [...]`。当下游 `tachi_shell(action="dispatch")` 调用现有 `dispatch_ops::handle_tachi_dispatch` 时，把生成的 `dispatch_id` append 到 flow `status.json`。
- 不替换现有 `dispatch_id` 用法；现有 kanban / eval ledger 字段保持不变。

### P0-2：Stage 转移规则

合法序列（MVP）：

```
brainstorm -> plan -> dispatch -> review -> ship
                    \-> review -> ship
```

- 任何 stage 都可被显式跳过，但跳过会写入 `events.jsonl` 一条 `stage_skipped` 事件。
- `kanban` / `status` 是只读，不参与 stage 转移。
- 非法转移（例如 `ship -> brainstorm` 同 flow_id 内）在 MVP 返回 warning 但不强制阻断；P1 再加严格 gate。
- 每次 stage 推进必须更新 `status.json.stage` 并 append `events.jsonl` 一条 `stage_entered` 事件。

### P0-3：每阶段可执行验收场景

| stage | 验收 |
|---|---|
| `brainstorm` | 调用返回 `flow_id` + `injected_skill="brainstorming"` + `instruction_path` 指向新建的 `instruction.md`。`status.json.stage=="brainstorm"`。 |
| `plan` | 返回 `flow_id` + `injected_skill="writing-plans"` + `instruction_path` 存在且包含本 stage 的 task / required reading。 |
| `dispatch` | 返回 `flow_id` + `dispatch_id`（若启用 async hook）+ `injected_skill="executing-plans"` + `kanban_state="TASK_STATE_WORKING"`（若 async hook 启用）。MVP 若 async hook 未启用，必须返回明确 `async=false` 标志和 `instruction_path`。 |
| `kanban` | 返回 `tachi_task(action="board")` 等价 JSON：`{board, count, tasks}`。 |
| `status` | 给定 `flow_id` 时返回该 flow 的 `status.json`；未给定时返回最近 N 个 flow 的简要列表。flow_id 不存在时返回明确 `not_found`。 |
| `review` | 返回 `injected_skill="requesting-code-review"` + `instruction_path`。 |
| `ship` | 返回 `injected_skill="finishing-a-development-branch"` + `instruction_path`。MVP 不自动跑 preflight；只生成 instruction packet。 |

### P0-4：`tachi_shell(action="dispatch")` 与现有 dispatch infra 的关系

- `tachi_shell(action="dispatch")` **不重新实现** clanker 派遣。
- MVP 行为：
  1. 生成/更新 flow run 目录与 `instruction.md`。
  2. 注入 `executing-plans` meta skill。
  3. 调用现有 `dispatch_ops::handle_tachi_dispatch`，并把组装好的 `instruction.md` 内容作为 `task` 字段（或在 prompt 末尾追加 `Read: <instruction_path>` 引用），保留现有 kanban + eval ledger 路径。
  4. 把返回的 `dispatch_id` 写入 flow `status.json.dispatch_ids`。
- 失败/未安装 clanker 二进制时不要求 `tachi_shell` 自身失败：返回 `async=false` 加上明确错误说明，flow 状态置 `dispatch_unavailable`。

### P0-5：Artifact 存储策略

- **文件系统**为权威源：所有 instruction / status / events / injected SOP / clanker artifact 都落 `.tachi/runs/<flow_id>/`。
- **memory DB** 只保存 promote/distill 后的摘要（与现有 eval ledger 一致），不复制全文。
- **kanban** 只保存可索引的元数据：`flow_id`, `stage`, `state`, `summary`, `updated_at` 等。
- 这样 crash/restart 时优先重读文件系统，然后重新同步 kanban。
- run 目录路径解析顺序：`$TACHI_RUN_ROOT` → `<repo_root>/.tachi/runs/` → `$TACHI_HOME/runs/` → `$HOME/.tachi/runs/`。MVP 默认使用 `<repo_root>/.tachi/runs/` 当存在 git root 时，否则 fallback 到 `$TACHI_HOME` 风格路径（与现有 `dispatch_ops::workspace_dir` 保持一致）。

### P0-6：Meta skill 映射策略

- MVP：硬编码 stage → `skill/superpowers/skills/<name>/SKILL.md` 的相对路径表。
- 解析顺序：repo-relative `skill/superpowers/skills/...` → 失败则记录 warning，不静默跳过。
- meta skill 文件被 **拷贝** 到 `.tachi/runs/<flow_id>/injected/<basename>`，并记录原始路径 + sha256（写入 `status.json.injected[]`）。
- 后续可外置为：注册表 JSON / Hub-managed meta skill registry。但 MVP 不引入这层抽象，避免与现有 SkillHub 边界混淆。
- meta skill 永远 **不进** SkillHub 的 `tachi_skill(action="discover")` 结果面（与现有约束一致）。
