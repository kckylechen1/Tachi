---
title: "Tachi/Hub MCP & Host Environment Playbook"
summary: "Architectural guide for Tachi/Hub MCP integration, Vault credential management, and host environment constraints."
category: "engineering/architecture"
organize: true
---
# Tachi / Hub、MCP 与宿主环境 — 讨论整理

本文档源自 2026-05 前后关于 **Sigil / Tachi** 的讨论。当前接入边界为原生 Streamable HTTP、显式 Vault 连接/项目绑定，以及只读 GitHub 来源；末尾的宿主对比保留为历史讨论。用于内部对齐，**不包含任何真实 API Key 或令牌**。

---

## 1. 核心结论（一张图式的心智模型）

- **MCP** 把「能力 + 入参契约」暴露给 Agent：`list_tools` 的 **description** 与 **JSON Schema** 决定模型**如何被引导**去调用；宿主侧还可限制暴露哪些工具。
- **Tachi / Hub** 提供记忆、检索、受控的第三方 MCP 代理和凭证连接；GitHub 在 Tachi 内是只读来源。GitHub 写操作由宿主或用户明确授权的外部工具完成。
- **`AGENTS.md` / 项目规则** 负责**叙述与约定**；**真正「强制」**依赖 **不配敏感 env、网关/沙箱、可选的环境隔离**（见 §6）。

### 1.1 Transport boundary: 原生 Streamable HTTP

Tachi 主服务只提供 Streamable HTTP。由用户或既有宿主 supervisor 显式启动
常驻服务，例如 `tachi --daemon --no-project-db --port 6919 serve`；支持原生
HTTP 的 MCP 宿主连接 `http://127.0.0.1:6919/mcp`。

- daemon 持有 DB handles，负责 project/global routing、background jobs、
  provider runtime 和 Hub；客户端声明自己的 workspace/project 上下文。
- 旧的第一方 `command`/`args` 配置需要改成 HTTP URL。裸 `tachi`、
  `tachi serve` 和显式 stdio 选择都会拒绝；没有 stdio proxy、自动启动 daemon
  或 embedded fallback。客户端不能自动重放结果不明的写操作。
- `scripts/setup_agent_mcp.py` 只生成已验证的原生 HTTP 配置。缺少兼容 daemon
  或宿主不支持该配置时应修正接入，不能回退为新的数据库持有进程。
- 旧 OpenClaw memory plugin 已退役，不能作为现成 HTTP 适配器使用；安装器
  不再安装或接入它。宿主应显式调用 `tachi_memory` 的 save/checkpoint。
  Rust 内部的 `capture_session` owner/runtime 能力仍保留，不代表旧插件可用。

第三方 Hub MCP 的 stdio 传输、ACP harness 协议管道及独立 portable-server
保留各自的传输契约。具体配置与重连规则见
[HTTP Direct-Connect Cookbook](http-direct-connect.md)。

---

## 2. Vault：凭证与连接

- 初始化使用 `tachi vault init`；主密码经 Argon2id 派生密钥，不以明文存库。
- 解锁/上锁使用 `tachi vault unlock` / `tachi vault lock`；会话状态保存在本机 daemon。
- 写入使用 `tachi vault set NAME`；Vault 继续用 AES-256-GCM 加密并保留 ACL、访问记录、池轮换和健康状态。
- Agent 通过 `vault_connections` 的 `list` / `describe` / `prepare` 获取有权限的 API/SSH 连接元数据和启动参数，不获取明文 Key。
- API 运行用 `tachi vault connection run NAME -- <cmd>`；SSH 连接引用系统 OpenSSH 别名，不导入私钥或改写 SSH 配置。

实现与边界见 [Vault connections](vault-connections.md)。原始秘密管理 MCP 工具已退役；owner CLI 的固定管理通道复用现有 daemon 和本机认证能力。

## 3. 项目凭证使用

`.tachi/vault.env` 只保存 `ENV_NAME=vault:NAME` 引用。

- `tachi env plan`：查看绑定，不解密。
- `tachi env --keychain run -- <cmd>`：将声明的 Key 注入子进程。缺失、拒绝或空值会阻止启动，已声明的值覆盖子进程同名环境变量。
- `tachi env --keychain export`：显式向 stdout 导出项目绑定，适合用户管理的 shell/direnv 集成。

裸 `tachi env`、`env sync` 与旧 `vault exec` 已退役。新流程不生成持久明文 env 文件，也不修补 harness 的持久 auth/config。需要临时配置文件时使用受 run-dir 约束的 credential profile overlay。

---

## 4. GitHub / `gh` 与代码现状

- **`tachi_gh` 已退役（2026-10-22）**：GitHub 控制 facade 和 `safe_merge` 已删除。Tachi 的 intake/status 和 current-work anchor 保留有界只读请求，底层通过 Vault/env 读取 `GH_TOKEN`；旧 flow 记录仍可作为历史证据读取。
- **`tachi_staff`**：在明确的 durable/remote 等 staffing 条件下拉起 Claude/Codex 等 worker；普通本地并行使用宿主原生 subagent。worker 自带的 MCP/shell 配置不代表 Tachi 自动查 PR 或自动 merge。
- **Hub proxy**：已注册 MCP 可通过 **`server__tool`** 暴露；上游参数、schema 和富结果原样传递；`hub_call` 与 Gateway/Flatten 双模式已退役。本地 stdio 继续使用已有 process policy；远端连接的既有策略仍然生效。
- **规则**：PR 生命周期与 PR 合并直接在 GitHub 上完成（`tachi_gh` 已退役）；不要把本地 worktree 清理路径当成 GitHub merge 路径。

---

## 5. 凭据收敛：`GH_TOKEN` 与多 Agent

- **目标**：各 IDE/Agent 的 MCP `env` 里**不再**写 `GH_TOKEN`；仅在 **Tachi/Hub 进程**或 **Vault + 按需解密** 的路径上出现。
- **实践**：
  - Vault 登记所需凭证并保留消费者 ACL。独立 owner CLI 没有绑定 agent 身份，不能通过 `tachi env run/export` 读取 agent 限制的凭证；按实际消费者配置连接与权限，见 [Vault connections](vault-connections.md)。
  - 需要在本机用 `gh` 的终端：先在 `.tachi/vault.env` 声明 `GH_TOKEN=vault:GH_TOKEN`，再用 `tachi env --keychain run -- gh auth status`；显式 shell 导出使用 `eval "$(tachi env --keychain export)"`。
- **已有运行路径**：`tachi env run` 或 `tachi vault connection run` 在启动子进程时解析显式声明的 Key；`vault_connections prepare` 仅返回启动配方，实际执行仍由宿主既有授权控制。

---

## 6. 如何尽量「强制」走 Tachi（不仅靠 `AGENTS.md`）

| 层级 | 做法 |
|------|------|
| 提示 / 文档 | `AGENTS.md`、Cursor rules：区分 Tachi 的只读 GitHub 来源与宿主获授权的 GitHub 写入路径。 |
| 凭据 | 所有 MCP 宿主 **移除** `GH_TOKEN` / `GITHUB_TOKEN`；令牌只在 Tachi/Vault/运维 shell。 |
| 工具面 | **网关 / 精简 profile**：禁用或严格限制 **任意 shell**；Hub 上对终端类 MCP 配置 **sandbox policy**。 |
| 环境 | 可选：**独立容器/用户**，不装 `gh` 或不挂可 push 的 SSH key。 |
| 远端 | **Branch protection**、最小权限 PAT、与人用令牌分离。 |

说明：**无法**从软件上 100% 禁止人类在本机安装 `gh` 并粘贴 token；工程能收敛的是 **接 MCP 的 Agent 与自动化路径**。

---

## 7. Bash / Shell 与「装进 Tachi」

- **Shell / bash / zsh**：Shell 是总称；**bash** 偏脚本与通用；**zsh** 在 macOS 上常为默认交互 shell，补全与插件生态好；脚本可移植性常选 **bash** 或 **POSIX sh**。
- **Tachi 现状**：admin 下的 `tachi_sandbox(action='exec_audit')` 用于 **审计查询**（旧名 `sandbox_exec_audit` 已退役），**不是**通用「执行任意 bash」；通用执行可通过 **注册外部 MCP** + Hub **sandbox policy**，或在 Rust 侧未来增加 **受控 `sandbox_exec`**（需单独安全设计）。

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

## 12. 当前接入检查

1. **MCP**：常驻 daemon 使用固定 HTTP 端点；替换旧第一方 stdio 配置。
2. **凭证**：配置命名连接或 `.tachi/vault.env` 显式绑定，使用 `connection run` / `env run`；需要 shell 导出时显式使用 `env export`。
3. **GitHub**：PR 生命周期和合并通过宿主获授权的 GitHub 工具完成；Tachi 保留只读来源与历史记录。
4. **第三方 MCP**：Hub 的 stdio 配置按该服务自身契约管理，不受第一方 Tachi stdio 退役影响。
5. **OpenClaw**：旧 memory plugin 已退役；本文件不承诺新的插件接入实现。

---

## 13. 维护说明

- 若实现与本文描述不一致，**以代码为准**；欢迎在本文件末尾更新 **修订记录**（日期 + 摘要）。

**修订记录**

- 2026-05-03：初稿，整理 MCP/Hub/Vault/gh/强制策略/终端与宿主等讨论要点。
- 2026-05-03 (v1.1)：增补关于 Claude Code MCP 对比、物理沙箱必要性、以及 Tachi 作为通用 Action Layer 的深度讨论。

---

## 14. 历史讨论：Tachi Dispatch 与 Claude Code MCP 的对比

以下保留 2026-05-03 的讨论背景，不作为当前能力表或开发承诺。当前以宿主原生
subagent 为默认，GitHub 写操作不属于 Tachi；实际接入遵循上文 HTTP 与 Vault 边界。

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
