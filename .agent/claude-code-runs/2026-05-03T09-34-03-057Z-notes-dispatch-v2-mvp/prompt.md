# Delegated Task

## 任务：Tachi P0 Notes + Dispatch V2 MVP

请在 `/Users/kckylechen/Desktop/Sigil` 实现以下 MVP，保持改动最小、可测试、不过度工程化。不要修改 archive/。不要新增无关文件。中文注释不用新增，除非现有代码需要更新过时说明。

### 背景
Tachi 现在要进入文件驱动开发 + dispatch v2：
- Notes 层：`~/.tachi/notes/` 人类可读 markdown，DB 只做索引
- Dispatch V2：确定性 prompt assembly，不用 LLM 编排，delegate agent 自己通过 MCP 搜 Wiki
- tracked run/trajectory：每次 dispatch 产出可审计文件

### 必做 1：Notes 目录结构 + `tachi_save scope=note`

当前 `tachi_save` 的 note 分支只走 `handle_remember` 存 DB。请改为：
1. 在保存 note 时，把内容写入文件系统 `~/.tachi/notes/`（或 `TACHI_HOME/notes/`）
2. 支持两种触发方式：
   - `kind="note"`
   - `scope="note"`（即使 kind 为空也按 note 处理）
3. 默认目录结构按需创建：
   - `inbox/`
   - `brainstorm/`
   - `dispatch/`
   - `handoff/`
   - `reflections/`
   - `proposals/`
4. path 规则：
   - 如果 params.path 是相对路径且以 `.md` 结尾，写到 `notes/<path>`
   - 如果 params.path 是目录或无 `.md`，写到 `notes/<path>/<timestamp>-<slug>.md`
   - 禁止让绝对路径逃出 notes 根目录
   - 默认 path：`inbox/`
5. markdown 内容要人类可读，包含少量 frontmatter：title/created_at/topic/category/keywords/source，然后正文。
6. DB 索引仍然调用 `handle_remember`，但 path 应指向 `/notes/<relative_path>`，category 默认 `note`，retention 默认 `durable`。
7. 返回 JSON 里包含 `note_file` 或 `note_path`，方便用户打开。

### 必做 2：`tachi_dispatch` 新增 `stage`

在 `TachiDispatchParams` 增加可选字段：
- `stage: Option<String>`，允许 `plan` / `execute` / `auto` / 空

行为：
- `plan`：默认注入 `skill:superpowers-writing-plans`（如果用户没显式传 skills）
- `execute`：默认注入 `skill:superpowers-executing-plans`（如果用户没显式传 skills）
- `auto`：默认注入 `skill:superpowers-writing-plans`，并在 prompt 里明确“先产出 plan，不要直接执行；等待 review/execute 阶段”
- 空：保持兼容，不改变用户显式传入技能的行为

### 必做 3：`assemble_prompt_v2` 最小增强

在现有 `assemble_prompt` 基础上增强即可，不需要新文件：
1. 如果没有 context_query，则默认用 task 自身作为 recall query，搜索 top_k=5
2. 注入 context 时标题明确为 `## Relevant context from Tachi memory/wiki`
3. skill 注入：保留显式 skills，同时应用 stage 默认 skill
4. avoidance 注入：用 task + `failure OR partial OR watchdog` 搜 `/eval` 相关失败经验 top_k=3，并以 `## Prior pitfalls / avoidance notes` 写入；失败时静默跳过
5. prompt 里加入 `## Operating instructions`：
   - Use Tachi MCP tools if available for additional context.
   - Call `tachi_complete` when done, including dispatch_id if provided.

### 必做 4：trajectory.jsonl 审计文件

在 `tachi_dispatch` 创建 workspace 时，写这些文件：
- `prompt.md`：完整 prompt
- `context.md`：注入的 context/skills/avoidance 摘要（可以和 prompt 同内容或拆分，MVP 可以写 prompt 的上下文部分）
- `trajectory.jsonl`：至少写两行事件：`dispatch_started`、`subprocess_finished`（含 dispatch_id、agent、stage、exit_code、timestamp、output_tail，避免长输出）

注意：现在已有 `plan.md`，可以保留，但新增 `prompt.md` 更符合 tracked run。

### 必做 5：post_complete_hooks MVP

在 `handle_tachi_complete` 后面加最小 hook：
- 如果 outcome 是 failure/partial，并且 notes 非空：保存一条 memory/wiki? 请选择轻量 DB memory，path `/eval/lessons/<date>/<task_id>`，category `lesson`，importance 0.75，内容包含 task/outcome/notes/skills/dispatch_id。
- 如果 outcome success 且有 trajectory：现有 distill 逻辑保持。
- 返回的 review_bundle.pipeline 里加入 `post_complete_hooks` 状态。

### 验证
请运行：
- `cargo test -p memory-server`

### 输出
完成后在 tracked result 里说明：
- 修改文件
- 行为变更
- 测试结果
- 剩余风险

