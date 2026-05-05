# Tachi 演进路线详细报告（2026-03-25）

## 1. 报告目的

本报告用于将当前 Tachi 的能力状态与下一阶段演进路径对齐，给研发团队一个可执行的实施蓝图。目标是把 Tachi 从“能力集合”稳态演进为：

- 控制面（Capability Control Plane）：注册、发现、治理、审计
- 执行面（Execution Governor）：受限执行、资源控制、隔离
- 记忆面（Collaborative Memory Bus）：持久化协作、反思、衰减

本报告重点覆盖短中期最关键的两条落地线：

1. PR1：Hub 治理化
2. PR2：Sandbox 执行器化

并给出后续 Ghost 持久化与 Virtual Capability 的演进路径。

---

## 2. 现状审计（基于当前代码）

- **MCP 命名标准化**：已将 `memory` MCP 统一更名为 `tachi` (Hub + Memory)
- **能力内敛化 (Hub Consolidation)**：已成功将 `exa` 和 `context7` 从顶层配置剥离，归结为 `tachi` Hub 下的 Capability。
- 已有 `hub_capabilities` 表，支持 `register/get/list/search/set_enabled/feedback`
- 已有 `hub_register/hub_discover/hub_get/hub_stats/hub_call`
- 已有 MCP 命令 allowlist 校验与 discovery 失败降级
- 已有 proxy tool 缓存与基础审计日志

对应代码位置：

- `crates/memory-core/src/hub.rs`
- `crates/memory-core/src/db.rs`
- `crates/memory-core/src/lib.rs`
- `crates/memory-server/src/hub_ops.rs`
- `crates/memory-server/src/main.rs`
- `crates/memory-server/src/server_handler.rs`

核心缺口：

- 缺少一等治理字段：`review_status / health_status / last_error / fail_streak`
- 缺少明确版本路由模型（active version）
- 缺少“可调用态”统一判定（不仅是 `enabled`）

### 2.2 Sandbox：当前是策略判定，不是执行隔离

现有能力：

- `sandbox_rules` 表支持 `agent_role + path_pattern + access_level`
- `sandbox_set_rule` / `sandbox_check` 工具可配置与查询规则

对应代码位置：

- `crates/memory-core/src/db.rs`
- `crates/memory-core/src/lib.rs`
- `crates/memory-server/src/sandbox_ops.rs`
- `crates/memory-server/src/main.rs`

核心缺口：

- 当前只能回答“允许/拒绝”，不能实际约束进程资源
- 不能限制 `CPU/内存/超时/host call/并发/cwd/fs roots`
- 不能作为 runtime sandbox 执行闸门

### 2.3 Ghost：当前是内存态 pub/sub，不可持久恢复

现有能力：

- `ghost_publish / ghost_subscribe / ghost_topics`
- 有 ring buffer、游标和 LRU 驱逐

对应代码位置：

- `crates/memory-server/src/main.rs`
- `crates/memory-server/src/ghost_ops.rs`

核心缺口：

- 消息在进程内存中，重启丢失
- 无 ack/retry/replay/TTL
- 无“高价值消息提升为长期记忆”的链路

---

## 3. 外部方案可借鉴点（提炼）

### 3.1 MCP Registry 生态

可借鉴原则：`registry-first, proxy-optional`

- Hub 先做注册发现和治理，不强制所有调用都经 Hub 转发
- 避免把 Hub 早期做成性能瓶颈与单点故障

### 3.2 WASM Sandbox 生态

可借鉴原则：策略声明式 + 运行时强制执行

- `policy.toml`/策略表负责能力声明
- 未声明能力即不可用（默认拒绝思路）

### 3.3 Ghost Memory 生态

可借鉴原则：消息流与记忆流分层

- Transport 负责传递
- Memory Lift 负责沉淀、衰减、反思和图关联

### 3.4 Skill 安检危险信号字典（已纳入实现）

当前 Skill 导入扫描已采用“静态规则 + 27B LLM 复核”的危险信号模型，核心信号包括：

- Prompt 注入/越权：`ignore previous instructions`、`bypass safety`、`reveal system prompt`
- 破坏性操作：`rm -rf`、`mkfs`、`dd if=`、`shred`
- 提权行为：`sudo`、root 级权限改写
- 远程引导执行：`curl|sh`、`wget|sh`、`Invoke-Expression`
- 凭证泄露：私钥标记、`AWS_SECRET_ACCESS_KEY`、GitHub token 前缀
- 数据外泄：读取 `.env`/`~/.ssh`/`/etc/passwd` 并外发
- 无约束执行：`eval/exec/os.system/subprocess` 等执行路径

统一扫描输出格式（Skill definition 内 `security_scan`）：

```json
{
  "risk": "low|medium|high",
  "blocked": true,
  "signals": ["..."],
  "findings": ["..."],
  "reason": "..."
}
```

治理策略：出现高危信号即默认 `enabled=false`，需人工复核后显式启用。

---

## 4. 目标架构（建议）

### 4.1 控制面（Hub）

职责：

- capability 注册、发现、启停、版本路由
- 静态安检与隔离管控（导入时的自动化代码扫描 + 运行时的 Review Gating）
- health/circuit 状态管理
- 审计统一出口

### 4.2 执行面（Sandbox）

职责：

- 调用前策略检查（policy preflight）
- 调用中资源限制（timeout/concurrency/cwd/fs/env）
- 调用后审计记录（成功/失败/超时/拒绝）

### 4.3 记忆面（Ghost + Memory）

职责：

- 异步 agent 协作消息总线
- 高价值消息提升为长期记忆
- 周期性反思与聚合

---

## 5. 实施路线图（分阶段）

## Phase 1：Hub 治理化（优先级 P0）

### 目标

- 让 Hub 从“可注册”升级为“可治理”
- 提供明确“是否可调用”的统一语义

### 主要改动

1. 扩展 `hub_capabilities` 字段：
   - `review_status`：`pending|approved|rejected`
   - `health_status`：`unknown|healthy|degraded|open`
   - `last_error`
   - `last_success_at`
   - `last_failure_at`
   - `fail_streak`
   - `active_version`（或单独路由表）
   - `exposure_mode`
2. 新增版本路由表 `hub_version_routes`（别名 -> 实际 capability id）
3. `hub_register` 对 MCP 默认进入 `pending + disabled`，并挂载自动化扫描 Hook。
4. 新增 **Pre-flight 静态安检机制**：导入 Skill 时自动执行缺陷分析（AST 扫描或小模型 Review），拦截高危系统指令和无约束网络外拨，通过后流转至 `approved`。
5. 新增 `hub_review` 工具（人工介入审核兜底）
6. 新增 `hub_set_active_version` 工具
7. **补充 Memory 基础设施：新增 `tachi_init_project_db` 工具**（用于显式在当前/目标目录创建并注册“项目级隔离的 Memory DB”，补齐只有全局库而缺乏按需创建离散项目库的自动化链路缺漏）。
8. `hub_discover/hub_get` 返回 `callable` 字段（enabled + approved + health）
9. `hub_call` 前置治理闸门，失败回写健康状态

### 验收标准

- 未审批 capability 无法被 `hub_call` 调用
- 切换 active version 后调用路径生效
- `hub_discover` 可清晰展示治理态和可调用态

## Phase 2：Sandbox 执行器（优先级 P0）

### 目标

- 从“规则查询”升级为“执行约束”
- 优先落 process sandbox facade（非一次性上 WASM）

### 主要改动

1. 新增 `sandbox_policies` 表（示例字段）：
   - `policy_id`
   - `capability_id` 或 `agent_role`
   - `runtime_type`（`process|wasm`）
   - `fs_read_roots`
   - `fs_write_roots`
   - `env_allowlist`
   - `net_allowlist`
   - `max_wall_ms`
   - `max_concurrency`
   - `enabled`
2. 新增 `sandbox_exec_audit` 表：
   - 启动前拒绝原因、运行时间、退出状态、超时信息
3. 新增工具：
   - `sandbox_set_policy`
   - `sandbox_get_policy`
   - `sandbox_list_policies`
4. 在 `connect_mcp_service`（stdio 路径）加入强制执行：
   - preflight policy check
   - **依赖隔离（环境沙箱）**：针对原生脚本（如 Python），强制通过 `uv run` 或独立虚拟环境拉起，避免依赖投毒与冲突。
   - **凭证管控（最小特权 Env）**：严格解析 `env_allowlist` 注入必要密钥，严禁全量继承宿主机环境变量，防范凭证窃取。
   - cwd 与 fs root 限制
   - timeout 与并发限制
5. 与现有 `audit_log` 联动，形成统一审计链

### 验收标准

- 无策略或策略拒绝时，子进程不会被启动
- 超时和并发限制可稳定触发
- 审计日志可区分“策略拒绝”和“运行失败”

## Phase 3：Ghost 持久化（优先级 P1）

### 目标

- 把 Ghost 从临时消息总线升级为可恢复协作层

### 主要改动

1. 新增持久化表：
   - `ghost_messages`
   - `ghost_subscriptions`
   - `ghost_cursors`
   - `ghost_topics`
   - `ghost_reflections`
2. 新增工具：
   - `ghost_ack`
   - `ghost_reflect`
   - `ghost_promote`
3. 将高价值消息写入 memories，反思写入 rules，关联写入 edges
4. **熔断与自愈闭环（Auto-Healing）**：当 Hub 触发某能力的 `fail_streak` 熔断阈值时，自动向 Ghost 总线广播告警事件，呼唤 NanoClaw 派发 Codex 等维护 Agent 进行日志分析、自动修 Bug 并重新触发流转。

### 验收标准

- 进程重启后游标和消息状态可恢复
- 可按 topic 进行 replay
- 有可观测的反思和提升结果

## Phase 4：Virtual Capability（优先级 P1）

### 目标

- 将多个底层 MCP server 抽象为逻辑能力组

### 主要改动

1. 新增 capability group 定义与映射规则
2. 支持 alias、版本 pin、策略继承
3. OpenClaw 侧只看逻辑能力，不关心底层 server

### 验收标准

- 同一逻辑能力组可替换后端而不改调用方
- 路由与审计可回溯到具体后端版本

---

## 6. PR 级别拆分建议（可直接排期）

## PR 1：Hub 治理化（建议先做 - 核心枢纽化）

交付内容：

- **[已部分完成]** MCP 统一入口化：将独立 MCP (Exa, Context7, etc.) 整合至 Tachi Hub 注册表。
- Hub 治理字段与路由表
- `hub_review`、`hub_set_active_version`
- `hub_get/hub_discover` 的 `callable` 输出
- `hub_call` 治理闸门与健康回写

收益：

- 风险低、改动集中
- 对现有链路兼容性高
- 立刻提升可控性

## PR 2：Sandbox 执行器

交付内容：

- `sandbox_policies` + `sandbox_exec_audit`
- 执行前 preflight + 执行中资源限制
- 审计闭环

收益：

- 直接降低执行面风险
- 为后续 WASM 接入打底

## PR 3：Ghost 持久化

交付内容：

- Ghost 数据持久化
- ack/replay/ttl/reflection/promote

收益：

- 多 agent 协作可恢复
- 记忆沉淀进入闭环

## PR 4：Virtual Capability

交付内容：

- 虚拟能力组与版本 pin
- 逻辑能力层抽象

收益：

- 统一入口体验明显提升
- 复杂度从调用方回收到平台侧

---

## 7. 数据模型建议（简版）

### 7.1 Hub 扩展字段（示例）

`hub_capabilities`：

- `id TEXT PRIMARY KEY`
- `type TEXT`
- `name TEXT`
- `version INTEGER`
- `definition TEXT`
- `enabled INTEGER`
- `review_status TEXT`
- `health_status TEXT`
- `last_error TEXT`
- `last_success_at TEXT`
- `last_failure_at TEXT`
- `fail_streak INTEGER`
- `exposure_mode TEXT`
- `created_at TEXT`
- `updated_at TEXT`

`hub_version_routes`：

- `alias_id TEXT PRIMARY KEY`
- `active_capability_id TEXT NOT NULL`
- `updated_at TEXT NOT NULL`

### 7.2 Sandbox 执行策略（示例）

`sandbox_policies`：

- `policy_id TEXT PRIMARY KEY`
- `capability_id TEXT`
- `agent_role TEXT`
- `runtime_type TEXT`
- `fs_read_roots TEXT`（JSON）
- `fs_write_roots TEXT`（JSON）
- `env_allowlist TEXT`（JSON）
- `net_allowlist TEXT`（JSON）
- `max_wall_ms INTEGER`
- `max_concurrency INTEGER`
- `enabled INTEGER`
- `updated_at TEXT`

---

## 8. 测试与验收建议

### 8.1 Hub 回归

- 未审批 capability 调用应失败
- 审批后调用应成功
- active version 切换后路由正确
- discovery 输出中 `callable` 状态准确

### 8.2 Sandbox 回归

- policy deny 时阻止 spawn
- timeout 生效
- 并发限制生效
- audit 记录包含拒绝原因和运行结果

### 8.3 Ghost 回归（Phase 3）

- 重启恢复
- replay 正确
- ack 后不重复投递

---

## 9. 风险与回滚策略

主要风险：

1. Hub 字段扩展导致旧数据兼容问题
2. Sandbox preflight 误拦截影响可用性
3. 路由切换引入短时不一致

回滚策略：

1. 所有新增字段保持默认值与向后兼容
2. Sandbox 执行约束支持 feature flag
3. 版本路由变更支持原子回切到前一版本

---

## 10. 推荐执行顺序（结论）

建议严格按以下顺序推进：

1. 先做 PR1（Hub 治理化）
2. 再做 PR2（Sandbox 执行器）
3. 然后做 PR3（Ghost 持久化）
4. 最后做 PR4（Virtual Capability）

这样能最快把当前最高风险的执行面收拢，同时保持系统演进可控，不把 Tachi 过早做成“大而脆”的强代理中间层。
