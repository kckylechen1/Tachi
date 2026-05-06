# Dispatch V2: Two-Stage Plan-Execute with Full Trajectory

**创建时间**: 2026-05-03
**状态**: 设计稿
**交接目标**: Windsurf / worker agents

---

## 核心理念

从 obra/superpowers 的 Planning ≠ Execution 哲学出发，将 `tachi_dispatch` 从"一把梭"升级为两阶段模式，并在每个阶段留存完整的审计轨迹。

## 架构总览

```
┌─────────────────────────────────────────────────────┐
│                  tachi_dispatch_v2                    │
│                                                      │
│  Stage 1: PLAN                                       │
│  ┌─────────────────────────────────────────┐         │
│  │ task + eval_history + wiki_lessons      │         │
│  │ + skill:writing-plans                   │         │
│  │ → Claude Code → plan.md                 │         │
│  └─────────────────────────────────────────┘         │
│              ↓ (可选人审 / LLM review)                │
│  Stage 2: EXECUTE                                    │
│  ┌─────────────────────────────────────────┐         │
│  │ plan.md + skill:executing-plans         │         │
│  │ + skill:implement-plan                  │         │
│  │ → Claude Code → trajectory + result.md  │         │
│  └─────────────────────────────────────────┘         │
│              ↓                                       │
│  Stage 3: EVAL + DISTILL                             │
│  ┌─────────────────────────────────────────┐         │
│  │ tachi_complete(trajectory, outcome)     │         │
│  │ → eval ledger + kanban update           │         │
│  │ → 自动蒸馏教训进 Wiki                     │         │
│  └─────────────────────────────────────────┘         │
└─────────────────────────────────────────────────────┘
```

## 每次 Dispatch 产出的持久化文件

```
~/.tachi/runs/{dispatch_id}/
├── prompt.md          # 原始任务描述
├── context.md         # 注入的上下文（eval 历史 + Wiki 踩坑 + memory hits）
├── plan.md            # Stage 1 产出：结构化执行计划
├── plan_review.md     # 可选：人审或 LLM review 意见
├── trajectory.jsonl   # Stage 2 过程：每一步的 tool call + 输出
├── result.md          # Stage 2 产出：最终报告
├── diff.patch         # 代码变更的 unified diff
└── status.json        # 状态机：planning → review → executing → completed/failed
```

## Stage 1: Plan（assemble_prompt_v2）

### 自动注入的上下文

```rust
async fn assemble_prompt_v2(server: &MemoryServer, params: &DispatchParams) -> String {
    let mut sections = vec![];

    // 1. 原始任务
    sections.push(format!("# Task\n{}", params.task));

    // 2. Eval 历史：同类任务的成功/失败记录
    let eval_hits = search_eval_ledger(server, &params.task, 5).await;
    if !eval_hits.is_empty() {
        sections.push(format!("# Past Eval Records\n{}", format_evals(&eval_hits)));
    }

    // 3. Wiki 踩坑记录
    let wiki_lessons = tachi_search(server, &params.task, scope="wiki", top_k=5).await;
    if !wiki_lessons.is_empty() {
        sections.push(format!("# Lessons from Wiki\n{}", format_lessons(&wiki_lessons)));
    }

    // 4. Memory context（相关记忆）
    if let Some(ref q) = params.context_query {
        let hits = recall_context(server, q, top_k=6).await;
        sections.push(format!("# Relevant Context\n{}", format_context(&hits)));
    }

    // 5. Skill 注入
    for skill_id in &params.skills {
        if let Some(prompt) = load_skill_prompt(server, skill_id).await {
            sections.push(format!("# Skill: {}\n{}", skill_id, prompt));
        }
    }

    // 6. 强制附加 writing-plans skill（Stage 1）
    sections.push(include_str!("skills/writing-plans.md").to_string());

    sections.join("\n\n---\n\n")
}
```

### Plan 输出格式要求

Plan 必须包含：
- **Goal**: 一句话目标
- **Steps**: 编号步骤列表，每步包含具体的文件/命令/预期结果
- **Verification**: 如何验证每步成功
- **Rollback**: 失败时的回退策略
- **Estimated Complexity**: 预计代码行数/文件数

## Stage 2: Execute

### Trajectory 记录格式

```jsonl
{"step": 1, "tool": "Read", "target": "src/llm.rs", "result": "ok", "ts": "2026-05-03T12:00:00Z"}
{"step": 2, "tool": "Edit", "target": "src/llm.rs:45-60", "result": "ok", "ts": "2026-05-03T12:01:00Z"}
{"step": 3, "tool": "Bash", "cmd": "cargo check", "exit_code": 0, "ts": "2026-05-03T12:02:00Z"}
{"step": 4, "tool": "Bash", "cmd": "cargo test", "exit_code": 1, "error": "test_foo failed", "ts": "2026-05-03T12:03:00Z"}
```

### 执行约束（inject 给 sub-agent）

```markdown
## EXECUTION PROTOCOL
1. Follow the plan step-by-step. Do NOT skip steps.
2. After each step, log progress via tachi_save(kind="note").
3. If a step fails 3 times, call tachi_complete(outcome="partial").
4. On completion, call tachi_complete with full trajectory.
5. DO NOT deviate from the plan without documenting the reason.
```

## Stage 3: Eval + Distill

### 自动闭环

```rust
// tachi_complete 被调用后自动触发：
async fn post_complete_hooks(server: &MemoryServer, eval: &EvalEntry) {
    // 1. 更新 Kanban
    update_kanban_state(server, &eval.dispatch_id, &eval.outcome).await;

    // 2. 如果失败，自动提取教训写入 Wiki
    if eval.outcome == "failure" || eval.outcome == "partial" {
        let lesson = distill_failure_lesson(server, &eval).await;
        wiki_write(server, &lesson).await;
    }

    // 3. 如果成功，记录 trajectory 供未来蒸馏
    if let Some(ref trajectory) = eval.trajectory {
        save_trajectory(server, &eval.dispatch_id, trajectory).await;
    }
}
```

## 与现有系统的兼容性

| 现有组件 | 变更 |
|---------|------|
| `tachi_dispatch` | 新增 `stage` 参数（"plan" / "execute" / "auto"），默认 "auto" 保持向后兼容 |
| `tachi_complete` | 新增 `trajectory` 字段（已设计，待实现） |
| `tachi_board` | 新增 `planning` / `review` 状态 |
| `assemble_prompt` | 升级为 `assemble_prompt_v2`，注入 eval + wiki |
| Hub skills | 导入 obra/superpowers 的 writing-plans + executing-plans |

## Superpowers Skill 导入清单

需要从 [obra/superpowers](https://github.com/obra/superpowers) 导入并注册到 Hub 的 skill：

| Skill | Hub ID | 用途 |
|-------|--------|------|
| writing-plans/SKILL.md | skill:writing-plans | Stage 1 计划编写 |
| executing-plans/SKILL.md | skill:executing-plans | Stage 2 计划执行 |
| requesting-code-review/code-reviewer.md | skill:code-review | 代码审查 |
| verification-before-completion/SKILL.md | skill:verification | 完成前验证 |

## 实现优先级

1. **P0**: `assemble_prompt_v2` — 注入 eval 历史和 Wiki 踩坑（~50 行 Rust）
2. **P0**: 导入 4 个 superpowers skills 到 Hub
3. **P1**: 两阶段 stage 参数 + plan review 卡点
4. **P1**: trajectory.jsonl 记录和存储
5. **P2**: 失败自动蒸馏教训进 Wiki
6. **P2**: trajectory → eval → distill 完整闭环

---

## 参考文档
- `docs/handoff_gh_and_brainstorming.md` — Hub-Driven Engineering 理念
- `.tachi-plans/async-delegate-phase1.md` — 异步 Dispatch + Watchdog + Kanban 设计
- [obra/superpowers](https://github.com/obra/superpowers) — Planning/Execution 方法论
- `docs/archive/code-review-superpowers.md` — Superpowers 实战审查案例
