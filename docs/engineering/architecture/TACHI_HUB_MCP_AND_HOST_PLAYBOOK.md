---
title: "Tachi/Hub MCP & Host Environment Playbook"
summary: "Architectural guide for Tachi/Hub MCP integration, Vault credential management, and host environment constraints."
category: "engineering/architecture"
organize: true
---
# Tachi / Hub、MCP 与宿主环境 — 讨论整理

本文档整理自 2026-05 前后关于 **Sigil / Tachi** 的一次对话，主题包括：MCP 收口、Vault 与凭据、GitHub/`gh`、强制策略、与 IDE/终端 的对比。用于内部对齐，**不包含任何真实 API Key 或令牌**。

---

## 1. 核心结论（一张图式的心智模型）

- **MCP** 把「能力 + 入参契约」暴露给 Agent：`list_tools` 的 **description** 与 **JSON Schema** 决定模型**如何被引导**去调用；宿主侧还可限制暴露哪些工具。
- **Tachi / Hub** 适合作为 **高权限操作的收敛点**：记忆、检索、Hub 代理的外围 MCP、（规划中的）GitHub 操作等，由**少数受控路径**完成，而不是让每个 Agent 各自带 `GH_TOKEN`、各自 `gh`。
- **`AGENTS.md` / 项目规则** 负责**叙述与约定**；**真正「强制」**依赖 **不配敏感 env、网关/沙箱、可选的环境隔离**（见 §6）。

### 1.1 Transport boundary: Streamable HTTP kernel, stdio adapter

Tachi 的长期运行时边界是 Streamable HTTP daemon，不是每个 MCP
宿主各自启动的 stdio 子进程。stdio 仍然是必要入口，因为 Claude
Code、Cursor、Codex、OpenClaw 等宿主最稳定的本地接入面通常是
stdio；但 stdio 进程只应是 `stdio -> daemon Streamable HTTP` 的
thin proxy。

目标形态：

```text
Agent / IDE / CLI
  -> stdio MCP adapter
      -> Tachi Streamable HTTP daemon
          -> Memory DB / Hub / lifecycle / enrichment / distill
```

工程约束：

- daemon 是唯一权威 kernel，负责 DB handles、project/global routing、
  background jobs、provider runtime、Hub/GitHub lifecycle。
- stdio adapter 不构造完整 `MemoryServer`，不打开 SQLite DB；daemon 是
  provider/runtime 的权威。`--no-project-db` serve 会跳过项目 `.env`
  读取并切到 `~/.tachi/runtime`，普通项目 stdio 入口在当前启动顺序下仍会
  经过既有 project-local dotenv 加载，后续要彻底隔离可把 proxy 判定前移到
  dotenv 之前。
- `runtime_info` 在 stdio adapter 中必须暴露 `process_role=stdio_proxy`
  和 `db_handles=0`，方便排查多进程与锁问题。
- 当 stdio adapter 能从当前 cwd 推导 named project 时，应把 project
  context 附加到 briefing/write/lifecycle 类调用；不要把 `scope=all`
  或显式 `scope=global` 的读写请求缩窄成项目请求。
- 如果没有兼容 daemon，stdio 可以退回本地 embedded mode 作为 first-run
  fallback；一旦请求已经发往 daemon，写操作不能本地重放。

---

## 2. Vault：密钥如何进 Tachi

- **初始化**：`vault_init` 在全局库写入 `vault_config`（salt、verifier、KDF 元数据）；主密码经 **Argon2id** 派生 32 字节密钥，**不以明文存库**。
- **解锁**：`vault_unlock` 校验 verifier 后，将派生密钥缓存在 **服务端进程内存**（`vault_key`）；`vault_lock` 清除。
- **写入**：`vault_set` 用 **AES-256-GCM** 加密 secret，密文 + nonce 写入 `vault_entries`。
- **LLM 自动注入**：仅当 `secret_type == api_key` 且名称以 **`_API_KEY` 结尾**、且未配置 `allowed_agents` 时，会进入 `LlmClient` 的内存表（如 `VOYAGE_API_KEY`、`SILICONFLOW_API_KEY`）。
- **`GH_TOKEN` 等**：可进 Vault，但**不会**走上述 `*_API_KEY` 自动灌 LLM 的分支；典型用法是 **`tachi env`** 解密后输出 `export NAME='value'`，供本机 shell 或运维流程使用。

实现参考：`crates/memory-server/src/vault_ops.rs`、`vault_crypto.rs`、`crates/memory-core/src/db/vault_db.rs`。

---

## 3. CLI 与「把 Key 放进 Vault」

- **当前没有** `tachi vault set` 这类纯 CLI 子命令；**写 Vault** 的标准路径是 MCP：`vault_init` / `vault_unlock` / `vault_set`（`tachi serve` 期间）。
- **`tachi env`**：只读打开库 + 主密码，导出 `export ...` 行；见 `bootstrap/env_cmd.rs` 中 `run_env_command`。
  - **`tachi env plan`**：查看 `.tachi/vault.env` 绑定，不解密。
  - **`tachi env export`**：stdout 输出 shell exports（适合 `eval "$(tachi env export --keychain)"`）。
  - **`tachi env sync`**：默认 **preview only**；必须 `--apply` 才写入 `.tachi/env.generated`（0600，含 DO NOT COMMIT 头）。
  - **`tachi env run -- <cmd>`**：在注入项目 secrets 的 env 下执行命令。
- **自研 Go TUI**：最稳妥是与 **MCP 客户端** 一样调上述工具；或在应用内复刻加密与表结构（成本高，需与 Rust 行为字节级一致）。

---

## 4. GitHub / `gh` 与代码现状

- **仓库内已提供 `tachi_gh`**：`repo_view`、issue/PR list/read、`pr_comments`、`pr_review_digest` 与 `safe_merge` 走 Tachi 原生 MCP facade；底层使用受控 `gh` 子进程，并通过 Vault/env 读取 `GH_TOKEN`。
- **`tachi_gh(action="safe_merge")`**：针对 **GitHub PR** 的 merge gate 与可选 `gh pr merge`。
  - 默认 `dry_run=true`，返回 `requested_mode="preview"`，只做预检。
  - 只有 `confirm=true` 且 `dry_run!=true` 时才会进入 `requested_mode="merge_requested"`；仍需 gate 为 ready 才会调用 `gh pr merge`。
  - 用 `merge_attempted` / `merge_executed` 判断是否真的调用或完成 merge，不要只看请求模式。
  - 默认 `merge_policy="standard"`，缺失 checks 或 review decision 会等待，不会静默当作绿色。
  - 传入 `flow_id` 时，会把 `pending|blocked|ready|merged` 写入 `.tachi/runs/<flow_id>/status.json`，并追加 GitHub 事件到 `events.jsonl`。
- **`approve_merge` / `tachi_task merge`**：针对 **Tachi dispatch 产生的本地 git worktree** 的 `git merge` 预览/执行与可选 `worktree remove`；不含 `git push` / `gh pr merge`。
- **`tachi_dispatch`**：拉起 Claude/Codex 等子进程；若子进程配置里自带 MCP/shell，那是外围 harness 配置，不代表 Tachi 自动查 PR 或自动 merge。
- **Hub proxy**：已注册 MCP 可通过 **`hub_call` / `server__tool`** 暴露；可对子能力配 **sandbox policy**（目录、env 白名单、超时等）。
- **规则**：PR 生命周期用 `tachi_gh`；本地 subagent worktree 回收用 `approve_merge` / `tachi_task`。不要把这两条 merge 路径混用。

---

## 5. 凭据收敛：`GH_TOKEN` 与多 Agent

- **目标**：各 IDE/Agent 的 MCP `env` 里**不再**写 `GH_TOKEN`；仅在 **Tachi/Hub 进程**或 **Vault + 按需解密** 的路径上出现。
- **实践**：
  - Vault 存 `GH_TOKEN`（或 `GITHUB_TOKEN`，与 `gh` 约定一致），**不**填 `allowed_agents` 若希望被 `tachi env` 导出。
  - 需要在本机用 `gh` 的终端：`eval "$(tachi env --keychain)"` 等（见 `cli.rs` 中 `Env` 子命令说明）。
- **下一步（代码向）**：若希望主 Agent **完全不碰 shell**，可增加 **「带 Vault 解密 env 的子进程封装」**（例如内部执行 `gh` 时注入 env），与「用户自己 eval」二选一或并存。

---

## 6. 如何尽量「强制」走 Tachi（不仅靠 `AGENTS.md`）

| 层级 | 做法 |
|------|------|
| 提示 / 文档 | `AGENTS.md`、Cursor rules：写明 **唯一允许的 GitHub 路径**（具体工具名与参数）。 |
| 凭据 | 所有 MCP 宿主 **移除** `GH_TOKEN` / `GITHUB_TOKEN`；令牌只在 Tachi/Vault/运维 shell。 |
| 工具面 | **网关 / 精简 profile**：禁用或严格限制 **任意 shell**；Hub 上对终端类 MCP 配置 **sandbox policy**。 |
| 环境 | 可选：**独立容器/用户**，不装 `gh` 或不挂可 push 的 SSH key。 |
| 远端 | **Branch protection**、最小权限 PAT、与人用令牌分离。 |

说明：**无法**从软件上 100% 禁止人类在本机安装 `gh` 并粘贴 token；工程能收敛的是 **接 MCP 的 Agent 与自动化路径**。

---

## 7. Bash / Shell 与「装进 Tachi」

- **Shell / bash / zsh**：Shell 是总称；**bash** 偏脚本与通用；**zsh** 在 macOS 上常为默认交互 shell，补全与插件生态好；脚本可移植性常选 **bash** 或 **POSIX sh**。
- **Tachi 现状**：`sandbox_exec_audit` 等用于 **审计查询**，**不是**通用「执行任意 bash」；通用执行可通过 **注册外部 MCP** + Hub **sandbox policy**，或在 Rust 侧未来增加 **受控 `sandbox_exec`**（需单独安全设计）。

---

## 8. Windows 与终端（简表）

- **cmd.exe**：非 Unix shell，语法不同。
- **PowerShell**：对象管道，与 bash 不同；**复制粘贴**问题多与 **Windows Terminal / VS Code 集成终端** 的快捷键与设置有关，而非 PowerShell 语言本身。
- **WSL / Git Bash**：在 Windows 上获得 **类 bash** 环境的主要方式。

---

## 9. 宿主工具链观感（非规范，供选型参考）

- **Cursor**：在 VS Code 系之上强化 **AI 与集成终端**；对「日常写代码 + Agent」路径，常比 **再堆一层 Codex CLI** 更轻。
- **macOS**：默认 **zsh**、POSIX 工具链、Terminal.app / iTerm2 与剪贴板集成普遍较顺，与 **Windows 上选 shell/终端** 的摩擦不同。

---

## 10. 仓库与发布（对话中涉及）

- **GitHub 仓库改为 Private** 后：**匿名 `brew install`** 若仍指向私有源码/Release，会失败；常见做法是 **私有源码 + 公开 Release/CDN + 自建 tap**。
- **源码与本地二进制**：以 **`cargo build --release`** 产物为准；工作区未解决合并冲突时无法代表已安装二进制；**`tachi 1.0.0` 不嵌入 git SHA**，无法从二进制反查提交。

---

## 11. 检索与网络（与 PROMPT 调查衔接）

- **嵌入失败**：服务端在 `memory_search_ops` 中已对 Voyage 嵌入失败打日志并 **继续走检索**；hybrid 通道在 **无 `query_vec`** 时仍可走 **FTS**。若仍见 0 条，需另查 **代理、`VOYAGE_API_KEY`、sandbox、路径/库作用域** 等。
- **不要在仓库中提交真实 Key**；曾出现在本地笔记中的密钥应 **轮换**。

---

## 12. 「完全做好」仍差的工作（工程清单摘要）

1. **GitHub/`gh` 后续增强**：CLI parity、branch-protection metadata、unresolved review threads、独立 checks/review head SHA 证明。
2. **Vault → 子进程 env** 的通用封装（减少依赖用户 `eval "$(tachi env)"`）。
3. **`approve_merge` 之后**：可选 **受控 push**；GitHub PR 合并使用 `tachi_gh(action="safe_merge", confirm=true)`（强审计、确认参数）。
4. **通用受控 shell**：外部 MCP + policy **或** 内置 `sandbox_exec` 设计评审。
5. **setup 向导**：可选增加 `GH_TOKEN` 说明位（`SETUP_API_KEYS` 扩展）。
6. **发版**：私有仓与 Homebrew/制品发布的流程文档化。

---

## 13. 维护说明

- 若实现与本文描述不一致，**以代码为准**；欢迎在本文件末尾更新 **修订记录**（日期 + 摘要）。

**修订记录**

- 2026-05-03：初稿，整理 MCP/Hub/Vault/gh/强制策略/终端与宿主等讨论要点。
- 2026-05-03 (v1.1)：增补关于 Claude Code MCP 对比、物理沙箱必要性、以及 Tachi 作为通用 Action Layer 的深度讨论。

---

## 14. 增补：Tachi Dispatch 与 Claude Code MCP 的深度对比

在 2026-05-03 的讨论中，明确了 Tachi 在多 Agent 编排中的生态位：

| 维度 | Claude Code MCP (本地助理) | Tachi Dispatch (中枢系统) |
|------|---------------------------|--------------------------|
| **跟踪机制** | **本地文件挂载** (.agent/)：适合单项目审计。 | **全局记忆集成** (Memory DB)：跨项目、语义可搜索。 |
| **异步性** | **同步阻塞**：主 Agent 需等待子进程结束。 | **完全异步**：秒回 ID，主 Agent 立即释放。 |
| **知识沉淀** | **静态 Markdown**：记录即结束。 | **动态蒸馏 (Distill)**：自动从轨迹中提炼 Wiki 知识。 |
| **安全模型** | 依赖用户手动确认。 | **看门狗 (Watchdog)** + 指令白名单：强行回收异常任务。 |

### 关键共识：
1. **中枢化 (Centralization)**：所有的风险操作（`gh`、敏感 `shell`）应逐步从各 Agent 的原生工具中剥离，统一收口至 Tachi Dispatch。
2. **物理沙箱 (Hard Sandboxing)**：对于涉及 `git pull` 等外部不可信代码的任务，Tachi Dispatch 应支持在 Docker/Firejail 中运行，以防止供应链攻击。
3. **从“工具”到“岗位”**：Tachi Dispatch 将 Agent 的每一次执行都视为一次“知识采集”，确保每一次尝试（无论成败）都能转化为系统的复利。
