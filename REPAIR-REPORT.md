# PR #2294/#2295/#2296 本地修复交接

状态：**infra_blocked / incomplete，未完成验收，不可据此接受候选。**

- 工作树：`/Users/kckylechen/.cache/tachi/repair-2296`；分支 `repair/2252-acceptance`。
- 起点及当前 HEAD：`8f5bbc408abe03cdd097b74d8ea3eaf2820bb97f`。本轮改动留在未暂存 diff，尚无修复 commit。
- 11 个源码文件的交接补丁：`/tmp/repair-2296-fixtures.patch`；SHA-256 `c05f1a34e5f6749eb8d813d2c933b85d37465438903edd10acbe465aedfb6a27`（不含本报告）。
- 开始时工作树干净；已完整读取 `/tmp/astra-2296-verdict.md`、失败清单及仓库交付规则。蓝本仍是后续修复依据。
- 主机：macOS 26.6.2，Build 25G83，Apple Silicon。
- 整项任务按 High 风险要求收尾；本轮实际只改测试及 fixture，没有修改生产路由、准入、协议或迁移实现。最终 exact-candidate 独立只读复核尚未执行。
- 没有远端操作、生产 daemon 操作或工作树删除。共享 target 仅作只读产物核验及一次失败的正常 Cargo 构建尝试。

## 实际阻塞

指定构建配方下，首次 D 定向 nextest 在编译前被沙箱拒绝，原始输出在 `/tmp/repair-2296-d-initial.log`：

```text
error: failed to open: /Users/kckylechen/Projects/Sigil/target/debug/.cargo-build-lock

Caused by:
  Operation not permitted (os error 1)
error: command `/Users/kckylechen/.rustup/toolchains/1.99.1-aarch64-apple-darwin/bin/cargo test --no-run --message-format json-render-diagnostics --package tachi-server --locked` exited with code 101
```

对 A 的两个已修改文件执行正常 `git add` 也被拒绝，工具原始输出：

```text
fatal: Unable to create '/Users/kckylechen/Projects/Sigil/.git/worktrees/repair-2296/index.lock': Operation not permitted
```

本会话可写范围只有修复工作树和临时目录，不包含共享 target 或 worktree 的实际 Git 元数据。没有另建 target、另造 Git 仓库或绕开 cargo-gate。

首次 Cargo 已报退出 101，但本轮启动的 cargo-gate guardian 未能完成回收。`/tmp/cargo-gate/holder.json` 记录 PID `48428`、本工作树 cwd 和本轮 submission 定向命令；一次非阻塞探测确认共享 gate 仍持锁。向这个自有进程组发出的清理信号也被沙箱拒绝（`PermissionError: [Errno 1] Operation not permitted`），工具会话中断未使其结束。未向任何其他进程发信号。调度方恢复构建前须先核实并回收该自有 guardian；不要删除或替换锁文件。

## 逐簇改动

下表的“已修改”只代表 diff 已形成，**不代表对应 Rust 测试通过**。

| 簇 | 文件与位置 | 本轮改动及守护行为 |
| --- | --- | --- |
| A / claims（已修改） | `crates/tachi-server/src/claims_ops/ownership_transition_tests.rs:98`、`:168`、`:210`、`:259` | 四个测试复用 `make_server_with_project_fixture("alpha")`；跨项目场景另在 server 自身 home 建 beta DB。保留隐藏/缺失等价、holder/version 诊断、事务回滚、冲突无部分写入和内部错误不泄漏断言。 |
| A / ownership（已修改） | `crates/tachi-server/src/task_lifecycle/ownership/tests.rs:81`、`:125`、`:178`、`:228`、`:262`、`:287`、`:325`、`:348`、`:411`、`:489`、`:553`、`:663`、`:687`、`:713`、`:761`、`:831` | 16 个使用项目准入的测试显式复用 `create_named_project_db` 建库；有 beta 切换的场景显式建 beta。保留现有 TempHomeGuard 对全局 flow/run 路径的隔离；未改通用 `local()`。原 unsupported-producer 场景的手写建库合并进现成 helper。 |
| B（未修改） | `crates/tachi-dispatch/src/routing.rs:349`；`crates/tachi-server/src/agent_eval/projection.rs:157`；`crates/tachi-server/src/dispatch_profile/routing/recommendation.rs:107` | 两处实现回归及 19 项 fixture 迁移仍待处理。未获得所要求的先红后绿执行证据，未擅自改变规划推荐产品语义。列表过滤和 start 门控保持原样。 |
| C（未修改） | `crates/tachi-server/src/dispatch_ops/dispatch.rs` 的 descriptor-bound 测试 | 仍待把源码串断言替换为真实 backend/cwd 边界行为测试；没有删除现有断言以制造通过。 |
| D（阻塞） | `crates/tachi-server/tests/http_dogfood_native_session.rs`、`http_dogfood_submission.rs` | 见下文。没有修改 submission canonicalization、幂等规则或 identity-drift 防护。 |
| cancellation 超时（已修改） | `crates/tachi-server/src/managed_run_control/repair_tests.rs:510`、`:523`、`:538`、`:563` | 在 fixture 建立 canonical runs 路径，使 hook 与 `open_ordinary()` 使用相同路径身份；增加注入已触发的显式 witness。失败请求和随后重试均有 5 秒有界等待，保留原 anchor 身份、revision 和 channel 断言；未改变 nextest timeout 或生产 hook。 |
| memcore body CHECK（已修改） | `crates/memcore/src/db/schema/a2a.rs:294`、`:323`；原位置 `crates/memcore/src/db/schema.rs:4005` | 原测试移到已有 A2A 模块下。当前 schema 测试先验证正常 v42 mailbox，再从 SQLite 读取实际 tables/indexes DDL，只变异 body CHECK，断言拒绝来自 body 约束且没有修复；另保留独立的真实 v32 合法/变异 fixture 验证。避免向已有超长 schema.rs 继续加行。 |
| memcore sentinel（已修改） | `crates/memcore/src/store/profile_identity_tests.rs:967` | Portable 的独立预期显式排除 v40、v42 两个 post-floor Product sentinel；Full 全集及 Portable strict subset 断言保留。 |
| memcore typed migration（已修改） | `crates/memcore/src/typed_records/tests.rs:565`、`:598`、`:613` | 独立预期改为 Portable 41 / Full 42，拒绝迁移的错误版本和升级后版本使用同一场景预期；保留拒绝零写入、旧 notes、可追加 typed record 和原版本备份断言。 |
| FD sweep（已修改） | `crates/tachi-dispatch/src/native_sandbox.rs:682`、`:719` | 复用 `/usr/bin/true` executable fixture，包括 Linux 的同类高 FD 场景；CLOEXEC、pipe 和 ENOENT/exec-error 断言不变。 |
| staffing 子进程选择器（已修改） | `crates/tachi-server/src/staffing_ops/mod.rs:816` | `--exact` 接回现存的 `retained_kernel_publication_failure_retains_worktree_for_certified_cleanup`（定义在 `:986`），仍守护 publication failure 的 lease 零发布、worktree 保留和 certified cleanup；必须恰好执行一个测试的断言保留。 |
| launcher argv（已修改） | `crates/tachi-server/src/dispatch_ops/launcher.rs:491` | command fixture 显式带 `--model operator-fixture-model`，符合 retained custom launcher 原样传递 argv 的合同；精确 argv、cwd、PWD 清除和权限拒绝断言不变。 |
| ACP postflight 事件（已修改） | `crates/tachi-server/src/dispatch_ops/acp_native/tests.rs:393` | fixture 改发真实 peer 的 `agent_message_chunk`；敏感 artifact 在 parent release 前不可见的全部断言保留，未改生产 collector。 |

`a2a_ops.rs::project_key` 没有改动：未绑定项目与绑定后解析失败仍然区分。所有测试 helper 的建库均在测试调用处显式发生；通用身份准入 helper 未自动建库。

## D 的取证与结局

已确认 `dogfood_e2e.py` 有 `--keep`（`:1250`），清理分支位于 `:1228`。原始产物取证必须走原候选及隔离 daemon。

实际尝试的命令如下，前置环境按舰长配方设置，且每次新建 `MEMCORE_TEST_TEMPLATE_DIR`：

```sh
cargo nextest run -p tachi-server --locked --profile ci --no-tests=fail \
  -E 'test(packaged_staff_submission_recovery_http_dogfood)'
```

结果是上面的 `.cargo-build-lock` 拒绝；nextest 没有执行到 HTTP 测试，**没有生成本轮 Summary**。

只读核验了共享 target 顶层 `debug/tachi-server`，以及 `deps/tachi_server-*`、`deps/tachi-*` 中能接受 `--version` 的 CLI 产物；均自报 `ed6360544621756a2e0dd85c16725e5af10b437e`。libtest 产物对该参数报不支持，没有运行其测试。没有找到可用于本轮重现的 `8f5bbc408…` CLI 产物，因此没有拿 main 二进制冒充候选手工运行 harness。

结局：**原始 events.jsonl / protocol.jsonl / daemon log / 完整 status 的新证据尚未取得，D 根因未定。** 本轮没有证据将 D 定责为 identity drift，也没有证据认定它源自本树之外。必须恢复构建后先冻结并核验候选 CLI SHA，再用 `--keep --only native-session,submission` 取原始产物；如根因落在本树外，按舰长红线 STOP。

## 门禁及逐字 Summary

| 本轮检查 | 结果 | 原始输出 |
| --- | --- | --- |
| 上述 D 定向 nextest | `infra_blocked`，编译前拒写，无测试 Summary | `/tmp/repair-2296-d-initial.log` |
| `cargo fmt --check`（最终 diff） | exit 0，stdout/stderr 为空 | `/tmp/repair-2296-fmt-final.log` |
| `git diff --check`（最终 diff） | exit 0，无输出 | 本轮工具执行记录 |
| `cargo nextest run --workspace --locked --profile ci --no-tests=fail` | 未执行；尚有 B/C/D 工作且指定 target 已证实不可写 | 无 Summary |
| `cargo test --workspace --locked --doc` | 未执行；同一构建权限阻塞 | 无 Summary |
| 修复后 focused Rust checks | 未执行；同一构建权限阻塞 | 无 Summary |

以下是舰长提供的历史日志逐字摘录，**不是本轮修复 diff 的验收**：

`/tmp/accept-2296.log:7244`：

```text
     Summary [ 689.411s] 4319 tests run: 4283 passed (1 slow), 35 failed, 1 timed out, 4 skipped
```

`/tmp/accept-main.log:40`：

```text
     Summary [ 420.596s] 4317 tests run: 4317 passed, 4 skipped
```

CI 的 `4754c5b…` 没有作为本地 exact-head 证据使用。不能从这些历史结果扣减本轮“已修改”的测试数来推算剩余失败数。

## 对蓝本的偏离及交接顺序

1. 未重排三条 PR 分支，也未产生按簇 commit：当前任务限制为本树且禁止远端操作，而 worktree Git index 写入也实际被沙箱拒绝。保留现状，未创建替代仓库或改其他分支。
2. ownership 测试保留 `make_server_with_temp_home`，在其 server home 显式调用现有建库 helper；原因是这些场景还直接使用进程级 `flow_runs_root()`，不能丢掉已有 TempHomeGuard 的 run/home 隔离。claims 的独立 server 测试使用蓝本建议的项目 fixture。
3. body CHECK 测试移入现有 A2A 实现模块的 test 子模块并保留 v32 独立覆盖。原 schema.rs 从 4067 行减为 4028 行，其既有超长问题未扩大；没有新增 crate 或生产模块。
4. D 重跑在编译前阻塞，先完成可静态确认的测试 fixture 修正；B 的实现保持未动，尚未满足先红后绿要求。

恢复工作所需的最小环境变更：允许指定共享 target 的正常构建写入，以及此 repair 分支正常提交所需的共享 Git 元数据写入；主仓工作文件仍保持只读。先核实并回收本轮自有 guardian，再恢复 Cargo。

随后从现有 diff 继续：先验证 A / memcore / 其余 fixture 与取消测试，按簇提交；再按蓝本完成 B 的先红后绿、C 的真实边界测试和 D 的 `--keep` 原始取证。建议的待执行 commit 主题：

- `test(claims): seed physical project databases for ownership isolation`
- `test(memcore): preserve versioned mailbox and migration refusal coverage`
- `test(dispatch): restore retained launcher and publication fixtures`
- `test(control): witness cancellation persistence faults on canonical paths`

这些只是待执行的提交主题，不是已存在的 commits。

## Not-checked / 剩余工作

- 本轮所有 Rust 改动的编译和定向行为检查。
- B 两处生产实现 bug 的修复、可用候选集全/部分/不可用注入单测、discovery/HTTP 的真实 probe fixture，以及 19 项相关迁移。
- C 的三条真实 backend/cwd 边界保障。
- D 原始产物、native 启动协议边界根因，以及 42/42、13/13 完整通过。
- final exact-candidate 独立只读复核。
- workspace nextest、workspace doc tests、按簇提交及最终候选 SHA。
- 底层 PR 各自的 hosted gate；本轮没有执行任何远端查询。

本轮不能宣布完成。原候选仍为已知红候选，修复 diff 尚未验证。


## Round 2 — repair-2296b（2026-11-11）

本节是 round 2 的追加记录；上方 round 1 原文保留。当前状态：**incomplete，触发舰长预设的规划合同 STOP 条件；不可据此接受候选。**

### 工作区、权限及候选

- 独立 clone：`/Users/kckylechen/.cache/tachi/repair-2296b`，分支 `repair/2252-acceptance`。
- 起点/源码候选：`0a1f5c24c3f0d101c7664ce7e4c542a8f877c548`。四个交接 commit 已存在；没有把未编译状态当成验证通过。
- 开始时源码工作树干净，只有调度方交接的未跟踪 `REPAIR-REPORT.md`。本轮不改生产代码，不做远端操作，不操作生产 daemon，不碰主仓工作文件，不 kill 进程、不动 cargo-gate 锁文件。
- 主机：macOS 26.6.2 / Build 25G83 / Apple Silicon。构建只用本 clone 的 `target`，`CARGO_BUILD_JOBS=6`，`NEXTEST_TEST_THREADS=4 RUST_TEST_THREADS=4`；每条 Cargo 运行新建 `MEMCORE_TEST_TEMPLATE_DIR`。

### B / 规划合同的 STOP 依据

以下均在本轮当前候选源码中核对，不是沿用旧日志结论：

1. `crates/tachi-dispatch/src/routing.rs:349`：`plan_request` 在 medium/low 风险时的 `required_profiles` 唯一为 `claude_plan`（high/critical 则还有 `codex_astra_review`）。因此不能泛称所有风险的规划请求都必然空集，确定空集的是仅要求 `claude_plan` 的路径。
2. `crates/tachi-dispatch/src/profiles.rs:213`：`claude_plan` 是 planner / plan，`native_session=None`；`profiles.rs:92` 的六个 native profile 只生成 reviewer/review 或 executor/execute，没有 planner。
3. `crates/tachi-server/src/host_profile.rs:191`：可用性 predicate 首先要求 `native_session.is_some()`；`dispatch_profile/routing/recommendation.rs:107` 再以此删除候选。无论六个 adapter 是否全部安装、sandbox probe 是否成功，`claude_plan` 都不会存活。
4. `crates/tachi-server/src/agent_eval/projection.rs:340` 的 gate 将非空 required set 作为硬性限制；不能丢弃该限制，以任意 reviewer 顶替 planner。
5. `docs/engineering/architecture/native-session-profiles.md:7` 固定 native 的 review/execute stage；`:39` 保留 operator-only legacy 条目而不提升为 native Staff；`:52` 要求保留列表/推荐的可用性过滤。`profiles.rs:934` 的 Staff resolver 明确拒绝无 native 合同的 `claude_plan`，旧 operator resolver 则仍保留。
6. 共享 briefing 的真实调用方 `crates/tachi-server/src/copilot_ops/feature_briefing/dispatch.rs:10` 仍调用同一个 recommendation；`:107` 明确 advisory profile 不授权 Staff 启动。故不能把共享规划建议消失默认为获准退役，也不能把列表/start 防线删除后声称修复。

最小待决策：保留规划建议时，明确其承接合同及作用域（仅 operator/native-harness advisory 独立路径，或明示某个 native planner 合同）；若退役 Staff 的规划推荐，则明确 Staff 的“不支持规划”语义及共享/operator 规划路径。现有六个 reviewer/executor 不能被本修复擅自解释成 planner。

没有修改 classifier、扩大 native admission、移除推荐过滤、强制 availability 或放宽断言。B 两个 bug 的新判别红测试及 green 证据均尚未完成，ledger 候选集统一和 19 项 fixture 迁移均未实施。不能用这个 STOP 把 B 标成通过。

### 本轮验证记录

交接簇窄范围构建/测试已启动，日志 `/tmp/repair-2296b-handoff.log`，本次最终结果在下方记录。初始过滤覆盖 claims/ownership、mailbox widened-body、sentinel、cancellation、FD sweep、retained launcher/postflight/publication；typed migration 与 staffing concurrent selector 未命中该过滤，须另以精确名称补验，不能由筛选近似推定覆盖；后述 guardian 阻塞使本轮未能补验。

### C / D / 终局门禁剩余

- C 的源码串断言与真实 backend/cwd 边界行为测试尚未替换。
- D 未手工运行 dogfood、未生成新的 events/protocol/status/daemon 原始产物；未对 native 启动根因定责，未动 submission canonicalization/幂等或 identity-drift 防护。
- 没有运行最终 workspace nextest、workspace doctests、42/42 和 13/13 HTTP 场景；它们仍是未完成门禁。
- 全部最终候选的独立只读复核尚未执行。规划合同的独立只读核实仅能支持 STOP 判定，不能替代最终修复候选 review。

### 对蓝本的偏离及原因

沿用调度方已提交的四个交接 commit，不重排 PR 链，不做远端操作。遵守舰长本轮“规划请求的产品语义若需退役决策 → STOP 写报告”，在发现规划候选与冻结 native 合同不相容后收回已启动的交接簇验证结果并停止后续实现，不擅自决定产品退役/增加 planner 合同。无 B green 输出、无最终验收 PASS。


### 规划 STOP 的独立只读核实

独立 reviewer：`/root/planning_contract_review`，exact HEAD `0a1f5c24c3f0d101c7664ce7e4c542a8f877c548`，未参与实现、未写文件、未启动构建、未做远端操作。

- **BUG**：确认非 high/critical `plan_request` 恒空，证据 `routing.rs:349` / `profiles.rs:213` / `host_profile.rs:191` / `recommendation.rs:107`；安装全部 adapter 不能改变它。
- **OK**：现有六个 native profile 只能是 `executor/execute` 或 `senior_reviewer/review`；不能擅自用 reviewer 替代 planner 或为 legacy planner 加 native 标记。
- **CONCERN**：共享 briefing 的 advisory 路径与 legacy `native_cli` transport 仍存在，但拆分 audience 或定义 legacy availability 是需要明确的产品范围，不能只删除 native predicate。
- reviewer 结论：确认需要舰长定稿的规划合同缺口，可按本轮 STOP 条件报告；最小待决策为 operator-only `claude_plan` 是否继续承接并与 native Staff 推荐明确区分，或指定新的 native planner 合同。该复核没有执行测试，不是最终修复候选验收。


### 交接簇执行结果与逐字 Summary

本轮对源码候选 `0a1f5c24c3f0d101c7664ce7e4c542a8f877c548` 执行：

```sh
export CARGO_TARGET_DIR=/Users/kckylechen/.cache/tachi/repair-2296b/target
export CARGO_BUILD_JOBS=6 NEXTEST_TEST_THREADS=4 RUST_TEST_THREADS=4
export MEMCORE_TEST_TEMPLATE_DIR="$(mktemp -d)"
cargo nextest run -p memcore -p tachi-dispatch -p tachi-server --locked --profile ci --no-tests=fail \
  -E 'test(claims_ops::ownership_transition_tests) | test(task_lifecycle::ownership::tests) | test(widened_body) | test(sentinel) | test(typed_store) | test(cancellation) | test(fd_sweep) | test(retained_kernel_publication) | test(retained_opencode) | test(postflight)'
```

原始日志 `/tmp/repair-2296b-handoff.log`，原始 JUnit 已保存 `/tmp/repair-2296b-handoff-junit.xml`（也在 clone 的 `target/nextest/ci/junit.xml`）。本轮实际编译成功，无 Rust 编译错误；不是 round 1 的未执行记录。

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1m 50s
────────────
 Nextest run ID 71285d19-d7a5-4c8d-96e8-18f4aa16c919 with nextest profile: ci
    Starting 175 tests across 13 binaries (6280 tests skipped)
     Summary [  45.106s] 175 tests run: 166 passed, 9 failed, 6280 skipped
```

日志结尾是 `error: test run failed`。Cargo wrapper 尚未退出（见 guardian 阻塞），所以这里不伪造 wrapper exit code。

| 交接项 | 本轮实测 |
| --- | --- |
| `28a9919ff` claims / ownership | claims 模块 4/4、ownership 模块 19/19 通过；含原先失败的项目 admission 路径。 |
| `c1dbb0ec8` memcore | 当前 stamp widened-body、历史 v32 widened-body、sentinel strict subset 通过。`shipped_typed_migration_requires_authority_and_preserves_old_notes_and_backup` 未命中过滤，未运行。不能说三项全过。 |
| `08e9f6305` dispatch | FD sweep、retained OpenCode launcher、retained publication-failure cleanup 通过。ACP postflight staging 已编译但执行失败；staffing concurrent selector 测试未命中过滤，未运行。 |
| `0a1f5c24c` cancellation | `launch_retained_anchor_survives_failed_cancellation_persistence` 通过，未超时。 |

初始 substring 过滤选中了 175 个测试，超出交接簇的最窄范围；这是本轮验证范围偏差，不隐藏其中额外失败。JUnit 中 memcore 20/20、tachi-dispatch 1/1、tachi-server 144/153、doctor_run_daily_process 1/1。以下 9 项都仍然是失败，无 retries 后变绿冒充通过：

```text
dispatch_ops::acp_native::tests::required_postflight_stages_native_acp_artifacts_until_parent_release
dispatch_ops::acp_native::approval::tests::native_permission_required_postflight_denies_without_publishing_pending_worker_input
dispatch_ops::dispatch::tests::dsh::dsh_required_postflight_publication_failure_fences_the_lease
dispatch_ops::subprocess::required_postflight_kernel_containment_discriminates_setsid_escape
staffing_ops::tests::native_lifecycle::retained_kernel_runs_strict_postflight_receipt_lifecycle
tests::dispatch_tests::exec_env_postflight_dispatch::postflight_dispatch_rejects_and_withholds_when_untracked_file_created_outside_scope
tests::dispatch_tests::exec_env_postflight_dispatch::postflight_dispatch_rejects_and_withholds_when_worker_mutates_out_of_scope
tests::dispatch_tests::exec_env_postflight_dispatch::aborting_a_paused_postflight_gate_cannot_release_cleanup_before_resolution
tests::dispatch_tests::exec_env_postflight_dispatch::postflight_dispatch_with_declared_scope_accepts_in_scope_write
```

前两项的原始边界错误分别在日志 `:974` / `:993`，逐字为：

```text
Native ACP adapter exited before responding; adapter stderr: sandbox-exec: sandbox_apply: Operation not permitted\n
```

这两项是本轮环境仍无法执行所需 sandbox 的直接证据，记 `infra_blocked`，不能据此撤掉 confinement 或改协议 fixture。其余七项只得到失败断言：包括预期 0 实际 exit 71、预期 completed 实际 failed、lease 未 quarantine 等；本轮没有完整原始运行产物证明它们共享同一根因，保留为未定责的 `test_failed`，不一并推定为已修复或全属环境。该 ACP 单测证据也不能冒充 D 的 `--keep` HTTP 取证。

### 本轮实际环境阻塞（按实测纠正环境前提）

本 clone 的 `target` 已实际编译成功；`.git` 写入在下述提交尝试仍被沙箱拒绝。另有 `ps -axo ...` 实测返回 `zsh:1: operation not permitted: ps`。只读检查 `/Users/kckylechen/bin/cargo` 可见其 `group_drained()` 调用 `ps`，失败时 guardian 保留 custody；日志已出最终 Summary 后，wrapper 会话仍未结束。

`/tmp/cargo-gate/holder.json` 当前记录的是本轮自有 guardian **PID 62446**、本 clone cwd 和上面的窄过滤命令。对现存 lock 只读打开、单次 `LOCK_EX|LOCK_NB` 探测返回 `cargo-gate read-only probe: held`。没有删除、更换或写入任何 gate 文件，没有发信号，也没有绕过 gate 启动第二个 Cargo 构建。后续补验无法正常开始；需调度方在能检查进程组的宿主环境核实并回收这一本轮 guardian。不要仅凭 PID 盲杀，不要删锁文件。

### 其他门禁与收尾

| 检查 | 本轮结果 / 原始输出 |
| --- | --- |
| 上述 nextest 交接簇 | 失败 Summary 如上；wrapper guardian 未释放，不能记验收通过。 |
| `cargo fmt --check` | exit 0，`/tmp/repair-2296b-fmt.log` 为空，无 Summary。 |
| `git diff --check` | exit 0，无输出（报告提交前再核对）。 |
| `cargo nextest run --workspace --locked --profile ci --no-tests=fail` | 未执行，无本轮 Summary。规划合同 STOP，且 guardian 当前阻塞后续构建。 |
| `cargo test --workspace --locked --doc` | 未执行，无本轮 Summary。 |
| native-session / submission HTTP 42/42、13/13 | 未执行，未通过；没有核验并运行候选 CLI，没有 D 原始产物。 |
| B 新判别 red / green | 未执行；先决定规划承接合同，不能自行写入退役语义。 |
| C backend/cwd 三条行为验证 | 未实现、未执行。 |

本轮源码与测试文件零改动，没有需要拆分的实现 repair commit；只形成保留 round 1 原文并追加本节的报告，提交因下述 Git 权限阻塞未能生成。源码候选仍为上述 `0a1f5c24c…`。当前结果 **incomplete / infra_blocked，非 PASS**。下一步先由舰长明确规划推荐作用域/承接合同，由调度方恢复可执行 sandbox 与 guardian 进程检查；然后从两个未运行的交接测试继续，进入 B 的判别红测试和统一候选集修复，随后 C、D 与全部终局门禁。


### 报告提交失败：独立 clone 的 Git index 仍不可写

报告完成后尝试正常 `git add REPAIR-REPORT.md`，在第一个命令即失败，没有执行后续 commit。工具逐字输出：

```text
fatal: Unable to create '/Users/kckylechen/.cache/tachi/repair-2296b/.git/index.lock': Operation not permitted
```

该结果与本轮“git add/commit 正常”的环境前提不符，记 `infra_blocked`。没有替换 `.git`、没有复制另造 Git 元数据、没有重试绕过沙箱，也没有生成修复或报告 commit。最终源码 HEAD 仍为 `0a1f5c24c3f0d101c7664ce7e4c542a8f877c548`；`REPAIR-REPORT.md` 留在 clone 根，未跟踪状态。后续恢复还需要给该 clone 的 Git index 正常写入权限。


## Round 3 — repair-2296b（2026-11-11 / 12）

本节覆盖 round 3；上方 round 1/2 原文保留。**第三轮本地修复与指定终局门禁全部通过；固定候选为 `44c131b893678158d08a1f1106991da80afa9248`。未提交远端，未 merge/deploy。**

### 范围、身份及权限

- 独立 clone、分支与起点保持不变：`/Users/kckylechen/.cache/tachi/repair-2296b` / `repair/2252-acceptance` / `0a1f5c24c3f0d101c7664ce7e4c542a8f877c548`。起始只有未跟踪交接报告。
- 舰长确认四个交接 commit 全部验证完成。本轮不重做其修复；调度方提供的补验 Summary 原样记录（非本轮执行）：

```text
     Summary [  13.844s] 12 tests run: 12 passed, 6312 skipped
```

- 主机实测 macOS 26.6.2 / Build 25G83 / Apple Silicon；danger-full-access。只用指定 clone target、6 build jobs / 4 nextest threads / 4 Rust threads；每条 Cargo 检查新建模板目录并在执行结束删除自建目录。
- 未手工进行任何 push/fetch/PR 等远端操作，未写主仓、未操作 :6919/:6888、未 kill 他人进程、未改 cargo-gate 文件。初次按授权运行原 harness 时，发现其自有 fixture worktree 清理会自动尝试远端分支删除；原 daemon log 记录该删除未成功，不能声称从未发生远端尝试。后续 harness 的隔离 daemon 注入 `protocol.allow=never`，在 Git transport 开启前拒绝所有远端访问；本轮未从该尝试推断远端状态。

### B：已定稿的分轨合同与统一候选集

- `d3a663836`：advisory 可用集合保留现存 legacy operator profiles（包括 `claude_plan`），native 候选仍走真实 host/adapter predicate。Staff 公共列表及 start 的 native 门控未修改。
- `resolve_route_evidence` 显式接收可用集合；先构造含 availability/risk 限制的 gates，再用于 row usability、ledger decision、domain scorer。序列化与持久化直接沿用同一 gated scored 集合，不再决策后另删候选。
- 纯推荐/ledger 单测显式注入 registry 全可用、部分可用、不可用集合。部分集合种入 3 条 unavailable `dsh_executor` 有效历史，确认其不能污染推荐 decision；拒绝路径不写 recommendation row。
- contradictory-required-set 换现存 `codex_sol_review`；discovery fixture 移入现有 `test_support.rs` 供 registry/C3/overlay/evidence-contract/briefing 共用，仍执行真实 confinement probe，没有强制可用开关。保留 MBIT 删除、overlay 历史只读、evidence-contract enforcement 和 briefing 分层断言。
- `1c03af320`：新增 HTTP MCP advisory 场景，验证 low/medium briefing 返回 `claude_plan`、Staff start 仍拒绝该 legacy profile。原 native 42 项、submission 13 项门槛未改。

先红后绿原始日志：`/tmp/repair-2296b-r3-b-red.log`、`/tmp/repair-2296b-r3-b-green.log`。红阶段只抽出可注入可用集合的 late-filter seam；ledger 仍按原 registry 决策。两条 regression 分别命中原缺陷，逐字证据：

```text
legacy planner remains available as an advisory: "no_admissible_dispatch_profile: the low risk classification admits no profile (required: [claude_plan], blocked: []); no dispatch profiles are configured"

assertion `left == right` failed: ledger gates must use the available advisory set
  left: ["claude_plan", "codex_astra_review", "codex_sol_review", "codex_terra_write", "deepseek_explore", "dsh_executor", "glm_impl", "kimi_arch", "kimi_ux", "opencode_astra_review", "opencode_sol_review", "opencode_terra_write"]
 right: ["codex_sol_review"]

     Summary [   1.121s] 2 tests run: 0 passed, 2 failed, 4312 skipped
```

绿色定向检查覆盖两个新 regression 及推荐、projection/replay、C3/overlay、evidence-contract、workflow briefing、copilot 等现有行为，逐字 Summary：

```text
     Summary [   6.574s] 91 tests run: 91 passed, 4365 skipped
```

### C：真实 backend/cwd 边界

`6aa834fc7` 删除两条过时源码拼写断言，保留 helper 的 `.`/unmanaged 行为断言。新增三条执行验证：

1. 实际 custom backend 经 finalize 绑定目录 FD 后 rename 原目录并创建同名 replacement；子进程读取 admitted marker、只在旧目录落 execution marker。
2. 实际 native backend 的协议 cwd、sandbox cwd 与 `AnchoredDirectory` 物理身份相同；真实 sandbox 下运行 ACP peer，比较其实际 process cwd 与收到的 session/new cwd。
3. unmanaged custom backend 保留请求的 cwd，真实进程在该目录执行。

原始日志 `/tmp/repair-2296b-r3-c.log`；逐字 Summary：

```text
     Summary [   3.000s] 4 tests run: 4 passed, 4313 skipped
```

### D：先冻结取证，再沿首条协议边界修复

原候选 CLI `--version` 确认 `0a1f5c24c3f0d101c7664ce7e4c542a8f877c548`，实际执行要求的 `--keep --only native-session,submission`，结果 `7 passed, 6 failed`（原始日志 `/tmp/repair-2296b-r3-d-initial.log`）。原始产物完整保留于：

`/var/folders/mz/zblh0pcn7m9cms5k0yzczrc80000gn/T/tachi-dogfood-5d9mnear`

完整 status、trajectory、events、daemon log 路径及 SHA-256 清单见 `/tmp/repair-2296b-r3-d-initial-artifacts.json`。两个 run 的 status 都是 `TASK_STATE_FAILED` / native turn 0 / phase failed，完整 failure 为 `native sandbox runtime must use its canonical physical path`；events 文件均为 0 字节，protocol.jsonl 数量为 0（未创建，绝不伪造缺失产物）。

根因在树内：`acp_native/runner.rs::native_command` 从逻辑 `run_dir` 直接拼 runtime，在 macOS `/var` alias 下撞上 `native_sandbox.rs::canonical_directory` 的物理路径守卫，尚未 spawn/initialize。共同启动失败导致 submission 的 launches 计数保持 0；不是 submission canonicalization、幂等或 identity-drift 缺陷。

`44c131b893678158d08a1f1106991da80afa9248` 只先解析服务器持有的 run 父目录，再创建 runtime；runtime 本身仍走严格 canonical/隔离检查。HTTP native 场景显式设置 TACHI_HOME symlink alias，在所有平台覆盖此路径；保留 42 项计数并增强首个真实 worker HOME 观察。peer 的 protocol 日志增加实际 cwd/home 和请求 params，便于核对启动链。未改 submission 规则或 identity-drift 防护。

最终候选构建/版本 receipt：`/tmp/repair-2296b-r3-build-final.log`、`/tmp/repair-2296b-r3-candidate.json`。HTTP 绿色产物、workspace 门禁与 exact-candidate 独立只读复核结果见下方。


### 冻结候选的 HTTP 执行与独立复核

候选：`44c131b893678158d08a1f1106991da80afa9248`。`/tmp/repair-2296b-r3-candidate.json` 记录二进制路径、完整版本和 SHA-256；实际版本：

```text
tachi 2.1.0+git-44c131b893678158d08a1f1106991da80afa9248 (built 2026-11-11T15:59:02Z)
```

实际执行：

```sh
python3 scripts/dogfood_e2e.py --tachi target/debug/tachi --keep --only native-session,submission
python3 scripts/dogfood_e2e.py --tachi target/debug/tachi --keep --only dispatch-advisory
```

原始日志 `/tmp/repair-2296b-r3-d-green.log`、`/tmp/repair-2296b-r3-b-http.log`；exit receipt `/tmp/repair-2296b-r3-http-exits.json` 两项均为 0。逐字汇总：

```text
55 passed, 0 failed
3 passed, 0 failed
```

前者由未降门槛的 native 42 项与 submission 13 项组成；最终 workspace 中的两个 HTTP test 已各自核对原 42/42 与 13/13 输出，未用合计数替代原断言。后者是新增 low/medium advisory/Staff refusal 场景。

D 绿色完整产物目录：

`/var/folders/mz/zblh0pcn7m9cms5k0yzczrc80000gn/T/tachi-dogfood-ujo4a_yc`

`/tmp/repair-2296b-r3-d-green-artifacts.json` 记录完整路径/大小/SHA-256：17 份 status、17 份 events、17 份 trajectory、16 份 protocol、3 份 daemon log。缺 auth 的拒绝用例在 spawn 前失败，未造 protocol 证据。成功协议可观察到 initialize → session/new → 两次 set_config_option → session/prompt。advisory 独立产物目录为 `.../T/tachi-dogfood-0tqp5hi_`。

独立只读 reviewer `/root/final_candidate_review` 未参与实现，固定完整 SHA，审查 `8f5bbc408… → 44c131b89…` 全修复差异，并重点核实 round 3：

1. **OK** — legacy advisory / native Staff 分轨，`host_profile.rs:198`；Staff list `dispatch_profile/mod.rs:33`、legacy start 拒绝 `profiles.rs:934` 未变。
2. **OK** — `projection.rs:158/201/358` 在 ledger 判定前 gating；`recommendation.rs:97/183/207`、`routing.rs:674` 的评分、fallback、持久化与输出复用同集。
3. **OK** — `cwd_boundary.rs:59/99/138` 的真实 descriptor replacement / unmanaged / native 执行，没有降低原保障。
4. **OK** — `runner.rs:55` 只解析 run parent；`native_sandbox.rs:404/405` 的严格物理路径校验保留；submission/identity 防线未变。
5. **OK** — 交接的 project DB fixture、cancellation 注入 witness/有界等待与 current-schema 拒绝保障保留。

未发现具体可达 BUG 或阻塞 CONCERN。review verdict：**MERGE-SAFE（独立源码审查）**；reviewer 未执行测试，其返回时 workspace/doc/fmt 尚未完成。该静态 verdict 不冒充最终 acceptance PASS；远端 CI 与真实 vendor cohort 均 Not-checked。


### 终局门禁 — exact candidate 本地全绿

四条命令均在 `44c131b893678158d08a1f1106991da80afa9248` 上执行并返回 0；完整命令/日志路径/退出码/SHA 收据：`/tmp/repair-2296b-r3-final-gates.json`。未重试失败项来取得 PASS。

```sh
cargo nextest run --workspace --locked --profile ci --no-tests=fail
cargo test --workspace --locked --doc
cargo fmt --check
git diff --check
```

workspace nextest 逐字 Summary（完整日志 `/tmp/repair-2296b-r3-workspace-nextest.log`）：

```text
     Summary [ 838.764s] 8115 tests run: 8115 passed (4 slow), 19 skipped
```

19 项为现有平台/配置跳过项；本轮未增加 skip 或降门槛。nextest JUnit 已复制到 `/tmp/repair-2296b-r3-final-junit.xml`。本次全量运行中独立提取的 HTTP test stdout 收据 `/tmp/repair-2296b-r3-http-final-receipts.json`，两个测试的 candidate 行均为上述完整 SHA；分别保留原始 stdout 为 `/tmp/repair-2296b-r3-http-native-final.log`、`/tmp/repair-2296b-r3-http-submission-final.log`。逐字汇总：

```text
tachi-server::http_dogfood_submission::packaged_staff_submission_recovery_http_dogfood
13 passed, 0 failed

tachi-server::http_dogfood_native_session::packaged_native_session_supervision_http_dogfood
42 passed, 0 failed
```

两份 stdout 分别实际包含 42 与 13 条 PASS、0 条 FAIL，与各自门槛相符。此前 `--keep` 的 55/55 与新增 advisory 3/3 也均为同一候选。

doc-test 完整日志 `/tmp/repair-2296b-r3-workspace-doc.log`；以下为逐字 crate 汇总（合计 12 passed，0 failed，1 个既有 ignored；未改忽略规则）：

```text
   Doc-tests memcore
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.94s
   Doc-tests memory_server_capture_gate
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests memory_server_hub_cli
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests memory_server_manifest_audit
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests memory_server_prompt_envelope
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests memory_server_rescue
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests memory_server_runtime
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests portable_kernel
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_bootstrap
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_bootstrap_tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_build_broker
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_clean
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_contract_tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_credential_profile
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_delivery_tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_dispatch
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_exec_env_reaper
test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_foundry
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_hub
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_lesson_forge
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_llm
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.16s
   Doc-tests tachi_params
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_runtime_core
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_server
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests tachi_vault_tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests vault_kit
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

`cargo fmt --check` 与 `git diff --check` 均无输出、exit 0；原始日志 `/tmp/repair-2296b-r3-fmt.log`、`/tmp/repair-2296b-r3-diff-check.log`。

### 本轮交付结论

B 的分轨合同、候选集一致性与测试迁移；C 的三条真实 backend/cwd 行为；D 的树内 canonical run-parent 修复均已提交为上列四个按簇 commit。源码、定向验证、真实 macOS sandbox/HTTP dogfood 和指定本地终局门禁均完成。独立 exact-candidate 源码 verdict 为 MERGE-SAFE；本地 acceptance 结果 PASS。

源码工作树干净，仅保留交接以来未跟踪的本报告；本 clone 没有附加 worktree。所有自建 Cargo 模板目录已由执行器清理，`--keep` 证据按要求保留。未推送本地修复 commit，未创建 PR、merge、部署或 owner issue closure；hosted CI、真实外部 vendor 服务与远端状态均 Not-checked。首跑原 harness 的自动远端清理尝试及后续 transport 禁止措施见本节执行边界，不能以本地全绿抹去该事实。


## Round 5 — 三层历史串行化（2026-11-12）

### 本轮约束与起始证据

- 范围仅为在本 clone 的 `serialize/2294`、`serialize/2295`、`serialize/2296` 重组既有 commits、执行指定分层门禁、在全部本地门禁通过后按序向指定 GitHub URL force-with-lease 推送。舰长已明确授权这三条 PR 分支的历史改写；不 merge、close 或修改 PR 描述，不操作生产 daemon，不写主仓，不 kill 进程或改 cargo-gate 锁。
- 起始 HEAD：`9d451642420e7210abfa0ca107c21f5a598602fc`；原树：`07f929f9b5e3dc99f7bfcab2f3018017d80e07d6`；main 基线：`ed6360544621756a2e0dd85c16725e5af10b437e`。本 clone 只有一个 worktree，源码干净，原有未跟踪 `REPAIR-REPORT.md` 保留并追加；`origin` 是主仓本地路径，所有本轮远端操作另用舰长指定的 `https://github.com/kckylechen1/Tachi.git`。
- 已读取根报告全部 474 行；实际只有 Round 1–3，没有 Round 4 正文。没有补造第四轮记录；核对了 `9d4516424` 的真实 Python interpreter fixture 修复和舰长指定的最终基准树。
- 本轮是可直接验证、可恢复的既有历史重组，不新增产品或测试语义。构建只使用 `<clone>/target`；`CARGO_BUILD_JOBS=6`、`NEXTEST_TEST_THREADS=4`、`RUST_TEST_THREADS=4`；每条 Cargo 命令单独新建 `MEMCORE_TEST_TEMPLATE_DIR` 并在命令完全退出后清理。沿用原 ci profile、原断言及原跳过规则。
- 原始日志、JSON 退出码收据及 JUnit：`/var/folders/mz/zblh0pcn7m9cms5k0yzczrc80000gn/T/tachi-serialize-r5-ru41r8i8`。原报告字节长度与 SHA-256 已保存，收尾核对追加前内容未变。

### 归属核对（执行中记录）

- `28a9919ff`、`c1dbb0ec8` 已按原假设放入 layer 1，`cherry-pick -x` 无冲突。
- `0a1f5c24c` 由预期 layer 2 调整到 layer 3：它只修改 `managed_run_control/repair_tests.rs::launch_retained_anchor_survives_failed_cancellation_persistence`；该测试完整地由 Phase-4 `7639dc825` 新增，layer 1 / 原恢复线没有该测试。因此不能在 layer 2 硬造测试或提前引入 Phase-4 语义。原修复完整保留到 layer 3，不拆补丁或改代码。
- 本节写入时 layer 1 候选 `6bf3efb7e78fe4e62b1f33ebe33bbf51f0195d8b` 的 fmt 已 exit 0，指定 nextest 仍在运行；尚无任何本轮远端访问。最终 SHA、逐字 Summary、树同体和推送收据在下节追加。

### Layer 1 完成 / Layer 2 构建记录

- Layer 1 最终 SHA：`6bf3efb7e78fe4e62b1f33ebe33bbf51f0195d8b`。fmt exit 0；nextest exit 0，逐字 Summary：

```text
     Summary [ 828.137s] 6311 tests run: 6311 passed (3 slow, 1 leaky), 12 skipped
```

- 原始日志 `layer1-initial-nextest.log`、`layer1-initial-fmt.log` 及对应 JSON/JUnit 位于上述证据目录。`LEAK` 是 `facade_memory_ops::tests::facade_write_actions_do_not_use_read_forwarding`，nextest 按原 profile 计为 passed；未重试取得 green、未改 leak 策略，Cargo guardian 正常退出。
- Layer 2 cherry-pick `7e8439022` 时，仅 `execution-surface-census-v1.fixture.json` 的四组汇总计数冲突。逐组核对 mailbox + submission 的加和后，完整复用 Phase-4 之前原 `07ddd8d8b` 合并树的该文件，未扩预算或改断言。`00bdfb663`、`d2353ecc2` 随后无冲突应用。当前 layer 2 与 `07ddd8d8b` 只差 layer 1 的两个既有修复簇（六个测试文件），没有新增产品差异。
- Layer 2 候选：`6a9ceb2a9c68aeabe29a24c3619faf8b36df2c54`。确认其中不存在 cancellation anchor 新测试；`0a1f5c24c` 完整留给 layer 3。
- Layer 2 的既有 submission HTTP fixture 会启动带临时 HOME 的 daemon；该层尚无 `1c03af320` 加入的 Git transport 禁用环境。为严格避免本地全绿前访问远端，运行时用证据目录内 `fixture-python/sitecustomize.py` 仅拦截 Python 启动本 clone 的 candidate + 临时 `TACHI_HOME` 的 subprocess，给其环境追加 `protocol.allow=never`。这与原顶层 fixture 的三条 Git 配置相同；不改候选树、fixture 内容、产品逻辑、期望、筛选或跳过项。实际注入 witness 写入 `transport-isolation.jsonl`。Cargo/nextest 原命令不变，唯一额外环境为该目录的 `PYTHONPATH`。

### 全部本地门禁完成 — 推送前冻结

本轮风险按 Ordinary 的局部、可逆历史变换处理：协调者直接验证既有提交重排，没有新增产品/测试语义。提交说明保留原文，所有重放 commit 均附 `cherry picked from commit` 来源；邮箱原始 `131309854` 直接保留。

| 层 / PR | 本地分支 | 最终完整 SHA | 门禁 |
| --- | --- | --- | --- |
| layer 1 / #2294 | `serialize/2294` | `6bf3efb7e78fe4e62b1f33ebe33bbf51f0195d8b` | 指定 memcore + tachi-server nextest exit 0；fmt exit 0 |
| layer 2 / #2295 | `serialize/2295` | `6a9ceb2a9c68aeabe29a24c3619faf8b36df2c54` | 指定 memcore + tachi-server nextest exit 0；fmt exit 0 |
| layer 3 / #2296 | `serialize/2296` | `95db1ddee0f570817e041a6163904a54afd932d7` | 与原 `9d4516424` diff 为空；fmt exit 0 |

Layer 1、2 均实际执行原命令，不加筛选或跳过：

```sh
cargo nextest run -p memcore -p tachi-server --locked --profile ci --no-tests=fail
cargo fmt --check
```

逐字 Summary（分别来自 `layer1-initial-nextest.log`、`layer2-initial-nextest.log`）：

```text
     Summary [ 828.137s] 6311 tests run: 6311 passed (3 slow, 1 leaky), 12 skipped
     Summary [ 538.462s] 6318 tests run: 6318 passed (1 slow), 12 skipped
```

三层 fmt 分别记录于 `layer1-initial-fmt.log`、`layer2-initial-fmt.log`、`layer3-final-fmt.log`：均 stdout/stderr 为空、exit 0，没有 Summary。`git diff --check` 也 exit 0、无输出。所有 JSON 收据绑定各自实际执行的完整 SHA；JUnit 均已独立复制保留，没有被后续运行覆盖。Layer 2 HTTP 收据明确包含该层完整 candidate SHA；其中 submission 原门槛 `13 passed, 0 failed`、邮箱 `14 passed, 0 failed`。fixture Git transport 禁用实际记录了 18 次隔离 CLI/daemon 启动，见 `transport-isolation.jsonl`；未更改断言。

### 树同体与真串行证明

实际命令：

```sh
git diff --exit-code 9d451642420e7210abfa0ca107c21f5a598602fc 95db1ddee0f570817e041a6163904a54afd932d7
```

实际输出：stdout 为空，stderr 为空，exit 0。

```text
9d451642420e7210abfa0ca107c21f5a598602fc^{tree} = 07f929f9b5e3dc99f7bfcab2f3018017d80e07d6
95db1ddee0f570817e041a6163904a54afd932d7^{tree} = 07f929f9b5e3dc99f7bfcab2f3018017d80e07d6
```

以下三条 `git merge-base --is-ancestor` 均 exit 0、无输出：

```sh
git merge-base --is-ancestor ed6360544 6bf3efb7e78fe4e62b1f33ebe33bbf51f0195d8b
git merge-base --is-ancestor 6bf3efb7e78fe4e62b1f33ebe33bbf51f0195d8b 6a9ceb2a9c68aeabe29a24c3619faf8b36df2c54
git merge-base --is-ancestor 6a9ceb2a9c68aeabe29a24c3619faf8b36df2c54 95db1ddee0f570817e041a6163904a54afd932d7
```

```sh
git rev-list --count --merges ed6360544..serialize/2296
```

实际输出 `0`、exit 0；同一区间共 18 条 commit，原 18 条非 merge 来源均完整保留，原 `07ddd8d8b` 合并结不在新栈中。完整来源→新 SHA 映射、树 ID、命令及退出码见 `final-history-proof.json`。

另用独立临时 Git index 重建并比对（不动工作树，完成后 index 已删除）：layer 1 = `131309854` + 原 `28a9919ff` / `c1dbb0ec8`，树为 `6bd7beebec8f933b6f124e0a276af0888e5608db`；layer 2 = 原 `07ddd8d8b` + 同两簇修复，树为 `68a54f900a3a247692048393b3b4ae49a7175901`。均与最终层树完全相同，见 `lower-layer-tree-proof.json`。

按舰长本轮明确约定，layer 3 树同体继承调度方对该内容既有 workspace `8115/8115` 双跑结论；本轮没有重跑 workspace，也不将旧执行结果伪报为新 SHA 上的新执行。Layer 3 本轮实际执行的是树同体核对与 fmt。

### 最终归属

- layer 1：原邮箱 `131309854`；`28a9919ff` → `07f9be83a`；`c1dbb0ec8` → `6bf3efb7e`。
- layer 2：原恢复 `7e8439022` → `5c94648d1`、`00bdfb663` → `9f88cd05a`、`d2353ecc2` → `6a9ceb2a9`。
- layer 3：Phase-4 五条 `7639dc825`、`f6be14169`、`0754f143a`、`29d7ac308`、`8f5bbc408`；其后 `08e9f6305`、`0a1f5c24c`、`d3a663836`、`6aa834fc7`、`1c03af320`、`44c131b89`、`9d4516424`。
- 唯一归属调整为 `0a1f5c24c`：layer 2 → layer 3（新 SHA `f726f71d1`），原因及原代码依赖已在前文记录。没有拆修复 commit、没有改产品语义。唯一冲突为上述 census 聚合 fixture，直接复用原合并结果。

全部本地门禁完成时间及冻结 heads 已先保存为 `ready-before-remote.json`；此后才允许开始本轮远端读取与三次按序推送。

### 推送前发现远端已被其它工作推进 — STOP / incomplete

**本地历史串行化、layer 1 / 2 指定执行门禁、三层 fmt 和 layer 3 原树同体均完成；远端交付未完成。三次 force-with-lease 均未执行，不能提供不存在的推送回执。**

全部本地门禁通过后，首次远端操作为：

```sh
git ls-remote --refs https://github.com/kckylechen1/Tachi.git \
  refs/heads/feat/2232-bounded-mailbox-send \
  refs/heads/fix/2290-staff-start-recovery \
  refs/heads/feat/2252-native-session-staff \
  refs/heads/fix/2290-staff-submission-recovery-20261111 \
  refs/heads/main
```

实际输出（exit 0；完整收据 `remote-before.log` / `remote-before.json`）：

```text
cc1f1fbb783c2a3e920203df208deb432d1d7118	refs/heads/feat/2232-bounded-mailbox-send
60ce48622afff619f203716fd5f831632a34a4c4	refs/heads/feat/2252-native-session-staff
3af306369a7fcd06590c5e2fd8a4ec8b5406d4ad	refs/heads/fix/2290-staff-start-recovery
ed6360544621756a2e0dd85c16725e5af10b437e	refs/heads/fix/2290-staff-submission-recovery-20261111
5b23435a3596413edb2dbc348e7ffe2a2b34f63f	refs/heads/main
```

任务指定的旧 heads 为 `497a415e2a…` / `d2353ecc28…` / `9d4516424…`，均与远端不符。预先核对因此停止，**没有将最新 head 偷换为新的 lease 后继续覆盖，也没有调用 git push**。

随后仅用 `git fetch --no-tags --no-write-fetch-head` 从同一 URL 读取上述三条完整 SHA 和 main 的对象（exit 0；无输出），没有更新任何工作分支、main、origin 跟踪引用或远端引用。逐树比对确认不是单纯的 SHA 变化：

- #2294 实际 `cc1f1fbb783c2a3e920203df208deb432d1d7118`，父节点为原 `497a415e2a…` 与 `c344ef899…`，提交说明为 `fix(a2a): preserve portable admission boundary and v42 regression fixtures`；相对本轮 layer 1 有 7 个文件、148 additions / 17 deletions 的内容差异。
- #2295 实际 `3af306369a7fcd06590c5e2fd8a4ec8b5406d4ad`，父节点为原 `d2353ecc28…` 与 `c344ef899…`，提交说明为 `fix(deps): integrate shared rustix audit repair into PR #2295`。它尚未串到 mailbox，但已有本轮原冻结树以外的内容。
- #2296 实际 `60ce48622afff619f203716fd5f831632a34a4c4`，父节点为原 `9d4516424…`、新 #2294 和新 #2295，提交说明为 `fix(staff): integrate repaired prerequisites and clear native session lint failures`；与本轮最终候选（即原 `9d4516424` 树）有 9 个文件、173 additions / 36 deletions 的内容差异，包含 Cargo.lock、mailbox admission、native profile / sandbox / routing、eval 与 HTTP fixture / dogfood 变更。
- 新共享线 `c344ef899…` 的提交说明为 rustix 1.1.6 修复，其祖先还包含已进入 main 的 #2291。这里仅记录提交和树差异，不把提交说明当作新修复已验收的结论。

因此不能在“不覆盖其它会话工作”的仓库红线下直接强推旧冻结树。要保留这些新增内容，就必须由舰长把最终树基准从 `9d4516424` 改定为新候选，再重新串行并验证；要继续原硬基准，则须明确授权覆盖这次已观察到的新增远端内容。依据是本仓 `AGENTS.md` §2：`Never discard, overwrite, stash, reset, clean, build over, or disturb another session's changes. Reconcile by commit tree, not branch name.`

| 计划推送 | 结果 / 回执 |
| --- | --- |
| layer 1 → `feat/2232-bounded-mailbox-send` | 未执行；推送前发现旧 head 不匹配 |
| layer 2 → `fix/2290-staff-start-recovery` | 未执行；按序门禁未放行推送 |
| layer 3 → `feat/2252-native-session-staff` | 未执行；按序门禁未放行推送 |

本轮没有写任何远端引用，没有 merge、close、改 PR 描述或部署。陈旧分支 `fix/2290-staff-submission-recovery-20261111` 只读观察仍为 `ed6360544…`，从未作为写目标。hosted CI 与这些新远端候选的验收未检查。

最终停点保留本 clone 的三条已验证 `serialize/*` 分支、完整原始证据与本报告；当前 HEAD 为 `serialize/2296` / `95db1ddee0f570817e041a6163904a54afd932d7`。没有新建 worktree 或 target；每条 Cargo 命令自建的模板目录均已移除。原报告前 474 行字节保持不变。远端差异完整记录为 `remote-divergence.json` 和 `layer{1,2,3}-remote-diff.patch`。

最小下一步：舰长选定保留新远端内容的新树基准，或明确授权覆盖上述新增提交；在此之前不执行任何强推。
