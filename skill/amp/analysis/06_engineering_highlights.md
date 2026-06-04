# Amp 工程亮点分析

> 来源：`~/.amp/bin/amp` 67MB Bun 二进制逆向

---

## 1. Prompt-Model 联合优化

不是一套 prompt 跑所有模型，而是 **9 套 prompt 对应不同模型+模式**：

```
GPT-5.5 Deep  → sFR()  自主推理，最小改动，"carry through implementation"
GPT-5.4 Deep  → nFR()  更详细的规则，反 AI slop，前端指导
GPT (通用)     → yFR()  GPT 系列特化
GPT Codex     → kFR()  Codex 专用
Gemini        → lFR()  Gemini 特化（可选 oracle/diagnostics）
Kimi          → mFR()  长上下文优化
xAI           → SFR()  xAI 特化
Rush          → qFR()  快速执行
Aggman        → _FR()  Slack 集成/项目管理
Default       → oFR()  Pair Programming 协作模式
```

每个 prompt 针对特定模型的能力和弱点做了调整。比如 GPT-5.5 的 Deep prompt 不需要反 AI slop 指导（模型本身足够好），但 GPT-5.4 的 fallback prompt 加了完整的前端和反 AI slop 规则。

## 2. 双通道输出（commentary + final）

```
commentary channel → 实时进展更新，1-2 句话
final channel      → 最终结果，精炼输出
```

这解决了一个核心问题：用户在 agent 工作时需要看到进展，但不想被刷屏。commentary 是"正在干"的反馈，final 是"干完了"的结论。

规则：
- commentary 只在**改变用户理解**时发送（发现、决策、阻碍、计划）
- 不播报例行操作（搜文件、读代码、明显的下一步）
- 相关进展合并成一条，不要连续的小状态消息

## 3. Scaffold Customization（用户可定制）

```json
{
  "systemPrompt": {
    "type": "replaceAll" | "replaceBase",
    "value": "自定义 prompt..."
  },
  "enableToolSpecs": [{"name": "oracle"}],
  "disableTools": ["bash"]
}
```

三种模式：
- `replaceAll`：完全替换整个 prompt
- `replaceBase`：只替换基础 prompt，保留 context blocks 和 additional blocks
- 未配置：自动创建模板文件给用户填写

比 opencode 的 `oh-my-openagent.jsonc` 灵活得多——不仅能改 prompt，还能精确控制哪些工具可用。

## 4. Feature Flag 驱动的模型升级

```javascript
if (agentMode === "deep") {
  let canUseGPT55 = ATR(serverStatus);  // 查服务端 flag
  return canUseGPT55 ? "deep" : "deep-gpt5.4";
}
```

- 模型选择通过服务端 feature flag 控制
- 新模型灰度发布：先给内部用户，再逐步扩大
- 遇到问题秒级回滚：关掉 flag 立刻切回旧模型
- 用户完全无感知，CLI 不需要更新

## 5. 并行执行的精确控制

Amp 对并行有明确规则，不是"能并行就并行"：

```
可并行：
- Oracle 的不同关注点（架构 / 性能 / 竞态）
- 不同路径的 Codebase Search
- 写目标不重叠的多个 Task
- 所有 reads / searches / diagnostics

必须串行：
- Plan → Code（规划完成后才能编辑）
- 同一文件的多个 Task（写冲突）
- 链式变换（B 依赖 A 的产物）
```

这避免了 subagent 之间的写冲突和依赖错乱。

## 6. Verification 风险分级

```
typo fix         → 不需要验证
localized change → 针对性检查
cross-module     → 广泛覆盖
read-only task   → 跳过验证
```

而不是所有改动都跑全量测试。这大幅减少了无意义的验证开销。

## 7. 文件变更记录（before/after JSON）

```json
{
  "id": "uuid",
  "uri": "file:///path/to/file",
  "before": "原始内容...",
  "after": "修改后内容..."
}
```

直接存全文 before/after，不是 patch 格式。这让用户可以：
- 回溯每次文件修改的完整上下文
- 不需要理解 diff 语法
- 直接看到文件的全貌

## 8. MCP 工程化

- **OAuth 支持**：`amp mcp oauth login/logout`，不是所有 MCP server 都支持 API key
- **自动发现**：从 MCP Registry 拉取可用服务器列表
- **权限隔离**：每个 MCP server 单独 approve（`amp mcp approve`）
- **延迟加载**：MCP 工具标记 `deferred: true`，只在关联 skill 激活时加载
- **Skill-MCP 联动**：OpenAI provider 下，activated skills 自动激活对应的 deferred MCP 工具

## 9. Git 安全纪律

```
NEVER revert changes you did not make
NEVER use git reset --hard
ALWAYS prefer non-interactive commands
Don't amend commits unless explicitly requested
If dirty worktree has unrelated changes, just ignore them
```

以及关键认知：**There can be multiple agents working in the same codebase.** 这说明 Amp 原生支持多 agent 并行工作。

## 10. Anti-AI-Slop 前端指导

```
Typography: 避免默认字体栈 (Inter, Roboto, Arial, system)
Color: 避免 purple-on-white defaults，定义 CSS variables
Motion: 有意义的动画，不要 generic micro-motions
Background: 渐变、形状、图案，不要 flat single-color
Layout: 避免 boilerplate 布局和可互换的 UI patterns
```

这针对的是 LLM 生成前端的通病——千篇一律的紫白配色、Inter 字体、safe but boring。Amp 直接在 prompt 里要求 bold and surprising。

## 11. Discovery Discipline（读代码纪律）

> "Read enough code to avoid guessing, then stop."
>
> "Each read should answer a specific uncertainty. Once clear, move to the edit."

不是"读更多代码就更好"，而是**有目的的读**。读完就动手，不要过度探索。

## 12. Diagram 系统

不是 Mermaid（太重），而是用 box-drawing characters（╭╮╰╯）画 ASCII 架构图。只在 Mermaid 时用 Mermaid。

```
╭────────╮     ╭──────╮     ╭──────────╮
│ Client │─────▶│ API  │─────▶│ Database │
╰───┬────╯     ╰──┬───╯     ╰──────────╯
    │             │
    │          ╭───────╮
    └─────────▶│Worker │
               ╰───────╯
```

轻量、monospace 友好、不需要渲染引擎。

## 13. Aggman 模式的 Workflow 设计

Aggman 是 Amp 的 Slack 集成模式，有一套完整的项目管理工作流：

- 发现相关 threads → 阅读内容 → 创建/回复 threads → 管理 thread 状态
- Merge workflow：`workflow: "merge_changes"` 发送 canonical merge prompt（不是自由文本）
- Code review：`workflow: "code_review"` 同理
- Callback 机制：execution thread 不会自动回报，需要显式指令调 callback tool
- 状态检查：用户问 "how's it going?" 只给 brief update，不停止工作
