# Handoff: Tachi 原生 GitHub MCP 代理与 Hub-Driven Engineering

**创建时间**: 2026-05-03
**交接目标**: Windsurf / glm5.1 / Codex（任一接手均可）
**前置动作**: 接手前必须执行 `tachi_handoff(action="check")` 读取记忆中的架构上下文。

本次交接包含两个核心模块，请在动手开发前仔细阅读本文件及 Tachi 记忆库中的相关思维流。

---

## 模块一：GitHub (`gh`) 原生 MCP 代理层开发

### 背景 (Context)
正如 `docs/TACHI_HUB_MCP_AND_HOST_PLAYBOOK.md` 所述，我们必须彻底剥夺子 Agent 的 `gh` Shell 权限，防止供应链攻击和不可控的命令行注入。所有的 GitHub 操作必须收口至 Tachi。

### 开发任务 (Next Steps)
1. **新建文件**：在 `crates/tachi-server/src/` 下创建 `gh_ops.rs`。
2. **凭据注入**：实现一个安全的执行外壳，从 `vault_ops.rs` 中提取 `GH_TOKEN`，并严格控制子进程环境。
3. **强类型工具**：暴露语义化的 MCP 工具（如 `tachi_gh_pr_create` 和 `tachi_gh_issue_read`）。

### 架构铁律 (Codex Review)
- **绝对隔离**：拉起 `gh` 子进程前必须调用 `cmd.env_clear()`，仅注入 `GH_TOKEN`, `GH_PROMPT_DISABLED=1`, `NO_COLOR=1`。
- **优先 HTTP**：读操作（如读 Issue）应优先使用 `reqwest` + HTTP Bearer Auth，而不是拉起 `gh`。
- **绝对路径**：必须校验并使用 `gh` 的绝对路径，禁止依赖系统 `$PATH`。
- **语义化参数**：禁止透传 `args` 数组给 `gh`，必须在 Rust 侧用强类型 JSON 组装固定命令行模板。
- **Token 脱敏**：所有 stderr/stdout/audit log 必须过滤 Token 明文。
- **Vault 访问控制**：使用内部固定 agent_id（如 `tachi_gh_ops`）调用 `read_unlocked_vault_secret`，尊重 `allowed_agents` 约束。

---

## 模块二：Hub-Driven Engineering 与长效思维流

### 理论背景
我们正在从传统的"文件驱动开发 (File-Driven)"升级为**"中枢驱动 (Hub-Driven)"**。
这不仅仅局限于头脑风暴，而是意味着**将 `superpowers` 框架和整个 `gstack` 技能组深度嵌入到开发环境中**。由于 Tachi Hub 自带轻量级模型驱动（小模型路由/处理），我们可以最大化、无感地调用这些高阶能力。

### Tachi 三层存储模型（核心共识）
本次讨论确立了 Tachi 的立体存储架构，严禁混淆：

| 层级 | 用途 | 寿命 | 检索可见性 |
|------|------|------|-----------|
| **Handoff** | Agent 之间的短期任务接力 | 数小时 ~ 数天 | 仅目标 Agent 主动 check |
| **Issue / Kanban Note** | 特征讨论、发散思维的"沙盒工作台" | 数周 ~ 数月 | 隔离于主检索，不污染 Wiki |
| **Wiki / Eval** | 经蒸馏后的永久真理和执行轨迹 | 永久 | 全局可搜索，子 Agent 默认召回 |

### 工作流四阶段
1. **发散捕捉与全周期技能**：不仅是 `skill:brainstorm`，研发全周期皆可触发 Hub 技能。发散想法作为 `note` 存入 Tachi 特殊路径（如 `/board/feat/...`）。
2. **慢思考接力**：下任 Agent 被唤醒时，通过 `tachi_plan` 读取思维流，继承前人的推演逻辑。
3. **Hub 边缘计算辅助**：在关键节点（代码完成、重构决策），主动向 Hub 呼叫 `gstack` 或 `superpowers` 技能做初筛和 Review。
4. **闭环蒸馏**：当 Feature 落地时，利用 Hub 的大蒸馏能力，将长效思维流及最终成果提炼为正式的 **Wiki**。

### 给下任 Agent 的要求
你在开发 `gh_ops.rs` 的过程，本身就是"Hub-Driven"模式的实战。请保持代码的干净，遇到架构阻塞时，调用 `tachi_task_brief` 回溯今天的设计初衷；在关键节点，可主动探查 Hub 中的 `gstack` 技能辅助开发。

---

## 相关参考
- `docs/TACHI_HUB_MCP_AND_HOST_PLAYBOOK.md` — Vault/凭据/强制策略的完整讨论
- `docs/archive/code-review-superpowers.md` — Superpowers 框架的实战审查案例
- Tachi 记忆 ID `791983b9` — gh-native-mcp 架构蓝图（含 Codex 完整审查意见）
- Tachi 记忆 ID `2ea61d7a` — Hub-Driven Brainstorming 宏观架构设计 V0.2
