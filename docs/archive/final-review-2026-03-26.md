# Tachi Final Review (2026-03-26)

## Executive Summary

当前仓库状态可以给出一个明确结论：

- 底层 Phase 1 / 2 / 3 已完成并合并，发布状态干净。
- `v0.10.0` 已发布，可作为一个明确里程碑。
- 当前主要剩余工作不在底层，而在两块：
  - `Phase 4: Virtual Capability`
  - 应用层产品化流程（安装、治理、运行）

结论上，这个项目已经从“底层能力建设期”进入“应用层编排与产品化期”。

## Verified State

### Git / Release

- 当前分支：`main`
- 工作区：干净
- 远端同步：`main...origin/main`
- 最新 tag：`v0.10.0`
- Release：`v0.10.0 — Hub Governance, Sandbox Audit, Ghost Persistence`

### Tests

已验证：

- `cargo test -p memory-server`：17/17 通过
- `apps/tachi-desktop`：`npm run build` 通过

## What Is Done

### Phase 1: Hub Governance

已完成能力注册、审核、代理暴露与治理基础。

### Phase 2: Sandbox Executor

已完成策略配置、执行审计与拒绝前置检查能力。

### Phase 3: Ghost Persistence

已完成 Ghost 消息持久化、cursor/ack、promote 到 memory 等能力。

### Naming Layer

已完成 Ghost in the Shell 风格 alias：

- `ghost_whisper`
- `ghost_listen`
- `ghost_channels`
- `shell_*`
- `section9_*`
- `cyberbrain_*`

## What Is Not Done

### 1. Phase 4: Virtual Capability

该项已进入代码落地阶段，不再只是 roadmap / design。

当前分支已经补上：

- Virtual Capability registry（基于 `hub_capabilities.type = virtual`）
- binding table（`virtual_capability_bindings`）
- deterministic resolve
- `hub_call(vc:...)` 路由到 concrete MCP
- version pin
- policy inheritance（VC -> concrete fallback）

当前仍未完全收口的部分：

- app 侧 VC 安装/编辑 UI
- 更完整的 fallback / weighted routing
- 更细的 rollout / rollback 产品流

### 2. `tachi_init_project_db` 仍未落地

roadmap 明确提过该工具，但当前代码搜索只在文档中发现，没有实现落点。

这会带来一个实际问题：

- 现在 Memory 更像“已有库上的能力增强”
- 但“为一个新项目显式创建并接入隔离 DB”的产品化链路仍不完整

### 3. App Layer 仍是可演示原型，不是完整产品流

`apps/tachi-desktop` 目前已经不是空壳，它是一个可运行、可构建、可连接 daemon 的桌面前端；但它还不是“安装器 + 治理台 + 运行中心”的完整应用层产品。

当前已有：

- daemon 连接状态
- memory 搜索/查看
- ghost / graph / kanban 等界面骨架
- hub dashboard 与部分审计/GC 视图

当前缺失的关键产品流：

- MCP / plugin 安装向导
- tool discovery -> capability proposal -> policy proposal 的自动计划页
- `pending` 审核队列与批准流
- Virtual Capability 映射与编辑 UI
- smoke test / rollback / activate 的完整安装闭环

## Product Judgment

如果问题是“底层是不是基本做完了”，答案是：

是，基本做完了。

如果问题是“现在能不能说 Tachi app 已经做完”，答案是：

还不能。

更准确的说法应该是：

- 内核能力已经基本到位
- 应用层编排还没有产品化收口

## Most Important Next Step

最值得做的不是继续加零散底层接口，而是把应用层第一条闭环做出来：

### Installer MVP

建议优先做一条完整路径：

1. 用户输入一个 MCP / plugin
2. Agent 调 discovery
3. 系统生成安装计划
4. 显示 capability / policy / risk / VC 映射建议
5. 用户批准
6. 系统执行 register / sandbox / activate
7. 做 smoke test
8. 成功上线，失败回滚

这是最短的“从底层能力到用户价值”的路径。

## Suggested Sequencing

1. 先补 `tachi_init_project_db`
2. 再做 `Phase 4: Virtual Capability`
3. 以 VC 为中心做 app installer / governance workflow
4. 最后把 dashboard 从“状态展示”升级成“操作中枢”

## Final Answer

最终结论：

- 这个版本的底层工作已经基本完成，`v0.10.0` 可以成立。
- 当前最大的技术缺口不是稳定性，而是“抽象层”和“产品层”：也就是 `Virtual Capability` 与应用层安装治理闭环。
- `apps/tachi-desktop` 已经具备继续产品化的基础，但现在仍应被定义为一个可运行原型，而不是完整交付版 Tachi Computer app。
