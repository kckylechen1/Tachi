# Foundry LLM Lane → Claude CLI / API Backend 设计

> 日期: 2026-05-03
> 状态: Draft v3
> 来源: Antigravity Handoff Memo #2ba2fd00 + 舰长讨论

## v3 结论

方向正确，但 v2 范围过大，不能一次性统一所有 lane。当前建议：

1. **Security Scan 可以先做**：低频、非实时、安全判断质量重要，适合先接 Claude Code harness。
2. **Foundry memory_distill 单独做 Daily Batch MVP**：per-project batch，不跨项目混 prompt。
3. **保留 backend 选择**：每个迁移点都支持 `claude_cli` 与 `raw_api`，必要时 fallback。
4. **raw API prompt 必须升级**：不能继续用弱 prompt；raw API 也要结构化 JSON 输出。
5. **暂不动实时路径**：`hub_ops/call.rs` 和 `recall.rs compact_context` 不纳入本轮。

## v3 调用点分层

| # | 文件 | 功能 | 现状 | v3 决策 |
|---|------|------|------|---------|
| 1 | `hub_ops/security_scan.rs` | skill 安全扫描 | Qwen3.5-27B + JSON prompt | **Phase 0 先做**，支持 `claude_cli/raw_api/disabled` |
| 2 | `maintenance.rs` | memory 蒸馏 | MiniMax-M2.7 + 单句 prompt | **Phase 1 做 Daily Batch MVP**，支持 `claude_cli/raw_api` |
| 3 | `register.rs` | skill 分析 | GLM-5.1 raw API | 暂缓，后续可迁移 |
| 4 | `evolve.rs` | skill 优化 | GLM-5.1 raw API | 暂缓，后续可迁移 |
| 5 | `foundry_ops.rs` | agent evolution 综合 | GLM-5.1 raw API | 暂缓，后续可迁移 |
| 6 | `recall_cache.rs` | recall query 生成 | GLM-5.1 raw API | 暂缓；输出只是一句 query |
| 7 | `recall.rs` | context 压缩 | MiniMax-M2.7 raw API | 暂不动，半实时路径 |
| 8 | `call.rs` | skill 执行 | GLM-5.1 raw API | 暂不动，用户实时路径 |

## Backend 选择

配置建议：

```env
SKILL_SECURITY_SCAN_BACKEND=claude_cli  # claude_cli | raw_api | disabled
FOUNDRY_DISTILL_BACKEND=claude_cli      # claude_cli | raw_api
```

fallback 逻辑：

```text
backend=claude_cli:
  1. 先走 Claude Code CLI / harness
  2. CLI 不可用、超时、JSON parse fail → fallback raw_api
  3. audit status.json 记录 fallback_used=true

backend=raw_api:
  1. 直接走 raw API
  2. 不调用 CLI

backend=disabled:
  1. 仅用于 security scan
  2. 跳过 LLM，只保留静态规则
```

旧 MiniMax / Reasoning API key **先保留**，不要立即删除。稳定 1-2 周后再废弃。

## Security Scan Phase 0

Security Scan 可以先做，因为：

- **低频**：只在 skill 注册/更新时触发。
- **非实时**：不影响用户主路径。
- **质量敏感**：安全判断比速度重要。
- **现状偏弱**：当前默认 Qwen，安全扫描更适合 Claude Code harness。

### 配置

```env
SKILL_SECURITY_SCAN_BACKEND=claude_cli
SKILL_SECURITY_SCAN_RAW_MODEL=Qwen/Qwen3.5-27B
SKILL_SECURITY_SCAN_TIMEOUT_SECS=120
```

### 执行逻辑

```text
scan_skill_definition_with_llm()
  → static heuristic scan 先跑（保留现有逻辑）
  → backend=disabled: 返回静态结果
  → backend=claude_cli: 调 ClaudeAuditRunner(label="security-scan")
      - 成功: parse JSON
      - 失败: fallback raw_api
  → backend=raw_api: 用升级版 prompt 调 API
  → 合并 static + LLM 结果
```

### raw API prompt 输出格式

raw API 也不能泛泛扫描，必须要求结构化输出：

```json
{
  "risk": "low | medium | high | critical",
  "blocked": false,
  "findings": [
    {
      "severity": "low | medium | high | critical",
      "category": "prompt_injection | data_exfiltration | command_execution | credential_leak | unsafe_network | destructive_action | other",
      "evidence": "触发判断的片段",
      "reason": "为什么有风险",
      "recommendation": "建议如何修改"
    }
  ],
  "safe_summary": "一句话结论"
}
```

prompt 约束：

- 扫描 skill prompt/definition 是否诱导泄密、执行危险命令、绕过权限、读取敏感文件。
- 不要因为包含代码/命令就直接判高危，要区分文档示例和实际执行意图。
- 不确定风险标 `medium`，不要过度 block。

### 审计链

```text
~/.tachi/foundry-runs/security-scan/<capability_id>/<run_id>/
  prompt.md
  result.md
  status.json
```

`status.json` 记录：

- capability_id
- backend
- fallback_used
- risk
- blocked
- findings_count
- duration_ms

## Foundry Daily Distill Phase 1

### 关键修正

现有蒸馏有两条入口：

1. `bootstrap.rs` 每 1800 秒调用 `schedule_pending_distill_jobs()`
2. `enqueue_capture_maintenance_jobs()` 在 capture 后立即 enqueue `MemoryDistill`

所以要真正 daily batch，必须同时改：

- 停掉 30min scheduler，或改成 daily/startup catch-up。
- capture 后不再立即 enqueue `MemoryDistill`。
- 保留 `MemoryNeighborhood`、`RecallRerankCache`，`ForgetSweep` 视实现保留。

### 调度

```text
启动后 60 秒检查 last_distill_run
如果 > 24h 或不存在：运行一次
之后每 24h 检查一次
```

不要依赖凌晨 3 点机器常开。

### Project 边界

Daily batch 必须 **per-project**，不能跨项目混成一个 prompt：

```text
for each project_db:
  collect pending groups
  split into batches of max 10-20 groups
  run backend
  write results back to same project_db
```

模型只输出 `group_id` 和内容字段，不允许决定 `project/path/source`。

程序侧注入：

- `project`
- `path`
- `source`
- `scope`
- `source_memory_ids`
- `coherence_key`
- `batch_run_id`

### Distill raw API prompt

如果选择 `FOUNDRY_DISTILL_BACKEND=raw_api`，也要用结构化 prompt：

```text
你是 Tachi 记忆蒸馏引擎。阅读同一主题的一组 memory，提取核心技术知识，去重、去对话语气、去过时建议，保留具体参数和架构决策。

只输出 JSON：
{
  "outputs": [
    {
      "summary": "50字内摘要",
      "text": "200-500字结构化知识",
      "category": "fact 或 decision",
      "keywords": ["3-5个关键词"],
      "importance": 0.7,
      "topic": "主题名"
    }
  ]
}
```

### 审计目录

不要复用现有 `tachi_dispatch` 的 workspace，因为它会删除 `~/.tachi/runs/<dispatch_id>`。

新建 durable 目录：

```text
~/.tachi/foundry-runs/distill/<project>/<run_id>/
  prompt.md
  result.md
  status.json
  source_manifest.json
```

`source_manifest.json` 记录：

```json
{
  "project": "wiki",
  "run_id": "distill-wiki-20260503T050000Z",
  "groups": [
    {
      "group_id": "g1",
      "path_prefix": "/wiki/quant/strategy",
      "coherence_key": "topic:V8策略",
      "memory_ids": ["..."]
    }
  ]
}
```

### 正确性约束

每条新 `foundry_distill` memory 必须写 metadata：

```json
{
  "source_memory_ids": ["..."],
  "source_path_prefix": "/wiki/quant/strategy",
  "coherence_key": "topic:V8策略",
  "batch_run_id": "distill-wiki-...",
  "group_id": "g1",
  "backend": "claude_cli",
  "fallback_used": false
}
```

`source_memory_ids` 是现有去重核心，不能丢。

## 暂不纳入本轮

### `hub_ops/call.rs`

用户实时路径，不动。后续可以做 per-skill lane routing：

```text
skill definition 指定 lane/model → 尊重配置
默认小模型 Qwen
复杂/高价值 skill 可指定 reasoning 或 claude_cli
```

### `recall.rs compact_context`

半实时路径，不动。后续如要淘汰 MiniMax，可单独改为 Qwen raw API。

### `register/evolve/agent_evolution`

都适合 Claude CLI，但先不扩大范围。等 distill + security_scan 稳定后再迁移。

## 推荐实施顺序

1. **Phase 0: Security Scan backend abstraction**
   - 增加 `SKILL_SECURITY_SCAN_BACKEND`
   - Claude CLI harness + raw API fallback
   - 升级 raw API prompt
   - 写审计链

2. **Phase 1: Daily Distill MVP**
   - capture 后停止立即 enqueue `MemoryDistill`
   - daily / startup catch-up runner
   - per-project batch
   - Claude CLI / raw API backend 可选
   - durable audit dir
   - 保留 MiniMax fallback

3. **Phase 2: 质量评估**
   - parse failure rate
   - fallback rate
   - distill output 人工抽检
   - recall 命中质量变化
   - foundry job skipped/failed 下降情况

4. **Phase 3: 迁移其他后台 reasoning**
   - skill analysis
   - skill evolution
   - agent evolution synthesis

## v2 历史草案（保留供对照）

## 背景

### 现状问题

Foundry 后台 LLM 调用分散在 8 个点，使用 3 个不同模型/provider：

| # | 文件 | 功能 | 模型 | 触发 | 延迟敏感 |
|---|------|------|------|------|:---:|
| 1 | maintenance.rs | memory 蒸馏 | MiniMax-M2.7 | 后台定时 30min | ❌ |
| 2 | recall.rs | context 压缩 | MiniMax-M2.7 | operate 工具调用 | ⚠️ |
| 3 | recall_cache.rs | recall query 生成 | GLM-5.1 | 后台 Foundry job | ❌ |
| 4 | foundry_ops.rs | agent evolution 综合 | GLM-5.1 | 后台 Foundry job | ❌ |
| 5 | hub_ops/register.rs | skill 分析 | GLM-5.1 | 注册时 tokio::spawn | ❌ |
| 6 | hub_ops/evolve.rs | skill 优化 | GLM-5.1 | 用户触发但 async | ❌ |
| 7 | hub_ops/security_scan.rs | skill 安全扫描 | Qwen3.5-27B (硬编码) | 注册时 | ❌ |
| 8 | hub_ops/call.rs | skill 执行 | GLM-5.1 | **用户实时** | **✅** |

**质量问题**：MiniMax-M2.7 蒸馏只有一句话 prompt（`"Compress into a single sentence"`），
产出质量差。Claude Code CLI（底层同样是 GLM-5.1）因有 65K system prompt + agent 框架，
产出质量显著更高（wiki DB 已验证）。

**效率问题**：antigravity 项目有 2244 个 distill job，其中 814 failed、1360 skipped、
仅 70 completed。每 30 分钟调度一次，每次产生大量细碎 job，大部分浪费。

**可观测性问题**：所有 LLM 调用只有 eprintln 日志，无审计链。

### 各项目未蒸馏积压

| 项目 | raw memory | 已蒸馏 | 积压率 |
|------|-----------|--------|--------|
| hapi | 711 | 11 | 98% |
| wiki | 681 | 52 | 92% |
| antigravity | 196 | 8 | 96% |
| openclaw | 104 | 0 | 100% |
| tachi | 96 | 0 | 100% |
| hyperion | 64 | 1 | 98% |

大量积压 + 高失败率说明当前 30min 碎片化调度模式不 work。

## 目标

1. **统一 LLM 后端**：7 个后台调用点全部走 Claude CLI Pool（共享信号量）
2. **每日批量蒸馏**：distill 从 30min/次 → 1天/次，一个 prompt 处理所有积压
3. **完整审计链**：每次 LLM 调用留 prompt.md / result.md / status.json
4. **统一可观测**：kanban + eval_ledger 记录所有 Foundry LLM 工作
5. **保留 fallback**：Claude CLI 不可用时回退原 API

## 调用点详解 + 改造方案

### #1 maintenance.rs — memory 蒸馏 ⭐ 核心改动

**现状**：
```
scheduler 每 30min → 找 ≥3 条同 topic memory → 每组 1 个 job
→ build_distill_input() 拼接文本 → MiniMax API → 一句话
→ 1 job = 1 条 distill memory
```

**问题**：
- 30min 间隔积累不够，经常 < 3 条被 skip（1360 个 skipped job）
- 每组独立调 API，无法跨组归并
- prompt 太弱，产出一句话

**改造**：30min → 24h，**一次 claude CLI 调用处理所有积压分组**

```
daily scheduler (凌晨 3:00)
  ↓ collect_all_pending_groups()  ← 新函数
  ↓ 跨项目收集所有 ≥3 条的 coherent group
  ↓ 组装 mega prompt:
  │   "## 分组 1: V8策略 (5条) [memories...]"
  │   "## 分组 2: DuckDB管线 (3条) [memories...]"
  │   "## 分组 3: MCP架构 (4条) [memories...]"
  ↓ 一次 claude CLI 调用
  ↓ 解析 JSON → 每个分组 1-3 条结构化产出
  ↓ 批量 upsert
  ↓ 审计链 + kanban + eval
```

**收益**：
- 1 次 CLI 调用 vs 原来 N 次 API 调用
- 65K system prompt 开销摊到所有分组
- 24h 积累 → 每组 memory 更多 → 蒸馏质量更高
- 跨组可以发现重复/关联

### #2 recall.rs — context 压缩 ⚠️ 保持 API

**现状**：`handle_compact_context` → `run_compaction_model` → MiniMax API
**触发**：`operate` profile 的 `compact_context` 工具，由宿主 runtime 调用
**特点**：半实时（宿主 token 压力时触发），不适合排队到 daily batch

**改造**：保持直接 API 调用，但改用 Qwen3.5-27B（与 Extract/Summary 统一）。
MiniMax API Key 可以废弃。只需改 config.env 中 DISTILL_* 指向 SiliconFlow。

### #3 recall_cache.rs — recall query 生成

**现状**：Foundry job → `generate_recall_cache_query_via_llm` → GLM-5.1 → 一句话 query
**触发**：`enqueue_capture_maintenance_jobs` 中的 `RecallRerankCache` job
**特点**：纯后台、低频、输出简单（一句搜索 query）

**改造**：进 Claude CLI Pool。但输出太简单（一句话），单独调 CLI 不划算。
→ 可以合并到 daily batch 的 "附加任务" 中。

### #4 foundry_ops.rs — agent evolution 综合

**现状**：`run_agent_evolution_synthesis` → GLM-5.1 → JSON
**触发**：`AgentEvolution` Foundry job，极低频
**特点**：需要强推理、JSON 输出

**改造**：进 Claude CLI Pool（按需调用，不 batch）。

### #5 hub_ops/register.rs — skill 分析

**现状**：注册 skill 时 tokio::spawn → GLM-5.1 → JSON (summary + issues)
**触发**：`hub_register` 调用时，非阻塞
**特点**：不阻塞注册流程、输出简单

**改造**：进 Claude CLI Pool（按需调用）。

### #6 hub_ops/evolve.rs — skill 优化

**现状**：`skill_evolve` → GLM-5.1 → JSON (improved prompt + reasoning)
**触发**：用户调 `skill_evolve` 时
**特点**：需要理解现有 prompt + telemetry、做推理

**改造**：进 Claude CLI Pool（按需调用）。

### #7 hub_ops/security_scan.rs — skill 安全扫描

**现状**：注册时 → Qwen3.5-27B (硬编码) → JSON
**触发**：`hub_register` 时，与 #5 同步
**特点**：安全相关，需要较强判断力

**改造**：进 Claude CLI Pool（按需调用）。Qwen 做安全扫描不够强。

### #8 hub_ops/call.rs — skill 执行 ❌ 不改

**现状**：用户调 `run_skill` → GLM-5.1 → response
**触发**：用户实时操作
**特点**：延迟敏感，需要快速响应

**改造**：改用 Qwen3.5-27B（SiliconFlow Extract lane），更快更便宜。
不进 CLI Pool。

## 目标架构

```
┌─────────────────────────────────────────────────────────────┐
│  Claude CLI Pool (Semaphore = 2)                            │
│  位置: MemoryServer.claude_pool: Arc<ClaudePool>            │
│                                                             │
│  统一入口: claude_pool.call(system, user, label) → String   │
│  自动: 写 prompt.md/result.md/status.json 到 ~/.tachi/runs/ │
├─────────────────────────────────────────────────────────────┤
│  Daily Batch (凌晨 3:00, 1 次调用):                         │
│    #1 memory distill — 所有项目所有积压分组                  │
│    #3 recall cache query — 附带生成                          │
│                                                             │
│  On-demand (按需调用):                                       │
│    #4 agent evolution synthesis                              │
│    #5 skill analysis (register)                              │
│    #6 skill evolution                                        │
│    #7 security scan                                          │
├─────────────────────────────────────────────────────────────┤
│  不进 Pool (直接 Qwen API):                                  │
│    #2 recall compaction — 半实时，改用 Qwen                  │
│    #8 skill execution — 用户实时，改用 Qwen                  │
└─────────────────────────────────────────────────────────────┘

保留不变:
  - Extract lane (Qwen3.5-27B, SiliconFlow) — 事实提取
  - Summary lane (Qwen3.5-27B, SiliconFlow) — 简单摘要
  - Embedding (Voyage) — 向量化
  - Rerank (Voyage) — 重排序
```

## Daily Batch Distill — 核心设计

### Scheduler 变更

```rust
// bootstrap.rs — 替换当前的 30min interval

let distill_server = server.clone();
tokio::spawn(async move {
    // 计算到下一个凌晨 3:00 的等待时间
    let next_run = next_daily_3am();
    tokio::time::sleep_until(next_run).await;

    let mut interval = tokio::time::interval(Duration::from_secs(86400)); // 24h
    loop {
        interval.tick().await;
        match run_daily_batch_distill(&distill_server).await {
            Ok(report) => eprintln!("[distill-daily] {}", report),
            Err(e) => eprintln!("[distill-daily] failed: {e}"),
        }
    }
});
```

### Mega Prompt 结构

```
你是 Tachi 记忆系统的蒸馏引擎。以下是过去 24 小时积累的未蒸馏记忆，已按主题分组。

## 分组 1: topic:V8策略 (来源: wiki, 5条)
### Memory 1
- topic: V8策略
- importance: 0.80
- text: [完整文本]

### Memory 2
...

## 分组 2: topic:DuckDB数据管线 (来源: hapi, 3条)
...

## 分组 3: entity:MCP (来源: tachi, 4条)
...

---

## 任务

对每个分组：
1. 阅读全部记忆，提取核心技术知识点
2. 去除对话语气、过时建议、重复内容、Reasoning 思维链
3. 归并为 1-3 条精炼的结构化知识
4. 保留具体数字、参数、架构决策

## 输出格式（JSON）
{
  "groups": [
    {
      "group_id": 1,
      "source_project": "wiki",
      "outputs": [
        {
          "summary": "一句话摘要（50字内）",
          "text": "精炼后的核心知识（200-500字）",
          "category": "fact | decision",
          "keywords": ["k1", "k2", "k3"],
          "importance": 0.85,
          "topic": "主题名",
          "path": "/wiki/quant/strategy"
        }
      ]
    }
  ]
}
```

### 数量估算

当前积压:
- 6 个项目共 ~1850 条 raw memory
- 按 FOUNDRY_DISTILL_MIN_BATCH=3 分组，大约 100-200 个 coherent group
- 一次性处理可能超出 context window

**分批策略**: 每次最多 20 个 group（~60-100 条 memory），
如果积压超过 20 组则拆成多次 claude CLI 调用，
信号量控制最多同时 2 个进程。

日常稳态: 一天积累 10-30 条 memory → 5-10 个 group → 一次调用搞定。

## Claude CLI Pool 实现

### 核心结构

```rust
// 新文件: claude_pool.rs (~120 行)

pub struct ClaudePool {
    semaphore: Arc<Semaphore>,
    runs_dir: PathBuf,        // ~/.tachi/runs/
}

impl ClaudePool {
    pub fn new(max_concurrent: usize) -> Self {
        let runs_dir = tachi_home().join("runs");
        let _ = fs::create_dir_all(&runs_dir);
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            runs_dir,
        }
    }

    /// 通用入口: system + user → claude CLI → String
    pub async fn call(
        &self,
        system: &str,
        user: &str,
        label: &str, // "distill-daily", "security-scan", "skill-analysis"
    ) -> Result<String, String> {
        let _permit = self.semaphore.acquire().await
            .map_err(|e| format!("pool semaphore: {e}"))?;

        let run_id = format!("{}-{}", label, Utc::now().format("%Y%m%dT%H%M%SZ"));
        let run_dir = self.runs_dir.join(&run_id);
        fs::create_dir_all(&run_dir)
            .map_err(|e| format!("create run dir: {e}"))?;

        let prompt = format!("{}\n\n---\n\n{}", system, user);
        let _ = fs::write(run_dir.join("prompt.md"), &prompt);

        let start = Instant::now();
        let output = self.run_claude_cli(&prompt, 180).await?;
        let duration = start.elapsed();

        let _ = fs::write(run_dir.join("result.md"), &output);
        let _ = fs::write(run_dir.join("status.json"),
            serde_json::to_string_pretty(&json!({
                "run_id": run_id,
                "label": label,
                "outcome": "success",
                "duration_ms": duration.as_millis(),
                "output_len": output.len(),
            })).unwrap_or_default()
        );

        Ok(output)
    }

    async fn run_claude_cli(&self, prompt: &str, timeout_secs: u64) -> Result<String, String> {
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
           .arg("--output-format").arg("json")
           .arg("--dangerously-skip-permissions")
           .arg("--no-session-persistence")
           .arg(prompt);

        let output = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            cmd.output(),
        ).await
            .map_err(|_| format!("claude CLI timed out after {timeout_secs}s"))?
            .map_err(|e| format!("claude CLI spawn failed: {e}"))?;

        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout).to_string();
            // Extract result from Claude JSON envelope
            if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
                if let Some(result) = parsed.get("result").and_then(|r| r.as_str()) {
                    return Ok(result.to_string());
                }
            }
            Ok(raw)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("claude CLI exit {}: {}", output.status, stderr))
        }
    }
}
```

### 各调用点改动（每处 ~3 行）

```rust
// #4 foundry_ops.rs — agent evolution
// 之前:
server.llm.call_reasoning_llm(AGENT_EVOLUTION_SYNTHESIS_PROMPT, &user, None, 0.2, 2400).await?
// 之后:
server.claude_pool.call(AGENT_EVOLUTION_SYNTHESIS_PROMPT, &user, "evolution-synthesis").await?

// #5 register.rs — skill analysis
// 之前:
llm.call_reasoning_llm(SKILL_ANALYSIS_PROMPT, &prompt_text, None, 0.3, 500).await
// 之后:
server.claude_pool.call(SKILL_ANALYSIS_PROMPT, &prompt_text, "skill-analysis").await

// #6 evolve.rs — skill evolution
// 之前:
server.llm.call_reasoning_llm("You are a skill prompt optimization engine...", &evolution_prompt, None, 0.4, 4000).await
// 之后:
server.claude_pool.call("You are a skill prompt optimization engine...", &evolution_prompt, "skill-evolve").await

// #7 security_scan.rs — security scan
// 之前:
server.llm.call_reasoning_llm(SKILL_SECURITY_SCAN_PROMPT, &payload, Some(&model), 0.1, 800).await
// 之后:
server.claude_pool.call(SKILL_SECURITY_SCAN_PROMPT, &payload, "security-scan").await

// #8 call.rs — skill execution (改用 Qwen，不进 pool)
// 之前:
server.llm.call_reasoning_llm(system, &resolved_prompt, model, temperature, max_tokens).await
// 之后:
server.llm.call_extract_llm(system, &resolved_prompt, model, temperature, max_tokens).await
```

## 可淘汰的 config

改造完成后可以从 config.env 删除:

```
# 可删除 (MiniMax)
MINIMAX_API_KEY=...
MINIMAX_CN_API_KEY=...
DISTILL_API_KEY=...
DISTILL_BASE_URL=...
DISTILL_MODEL=...

# 可删除 (GLM-5.1 直连, 改为通过 claude CLI)
ZAI_API_KEY=...
REASONING_API_KEY=...
REASONING_BASE_URL=...
REASONING_MODEL=...
```

只保留:
```
SILICONFLOW_API_KEY=...  # Extract/Summary/Qwen
VOYAGE_API_KEY=...        # Embedding/Rerank
# Claude CLI 通过 ~/.claude/settings.json 配置 (已有)
```

## 审计链

每次 claude_pool.call() 在 `~/.tachi/runs/` 下留下:

```
~/.tachi/runs/
  ├── distill-daily-20260503T190000Z/
  │   ├── prompt.md        ← mega prompt + 所有分组内容
  │   ├── result.md        ← JSON 输出
  │   └── status.json      ← 耗时/成功失败/产出数
  ├── security-scan-20260503T120530Z/
  │   ├── prompt.md
  │   ├── result.md
  │   └── status.json
  └── skill-analysis-20260503T120535Z/
      ├── ...
```

**保留策略**: 成功保留 7 天，失败保留 30 天。
ClaudePool 每次调用后 spawn cleanup 任务清理过期记录。

## 涉及文件

| 文件 | 改动 | 估算 |
|------|------|------|
| **新增 `claude_pool.rs`** | ClaudePool 结构 + call() + run_claude_cli() | ~120 行 |
| `main.rs` | MemoryServer 加 `claude_pool` 字段 | ~5 行 |
| `bootstrap.rs` | daily scheduler 替换 30min interval | ~30 行 |
| `maintenance.rs` | `run_daily_batch_distill()` 新函数 + 删除旧 per-job distill | ~100 行 |
| `foundry_ops.rs` | `call_reasoning_llm` → `claude_pool.call` | ~3 行 |
| `hub_ops/register.rs` | `call_reasoning_llm` → `claude_pool.call` | ~5 行 |
| `hub_ops/evolve.rs` | `call_reasoning_llm` → `claude_pool.call` | ~3 行 |
| `hub_ops/security_scan.rs` | `call_reasoning_llm` → `claude_pool.call` | ~3 行 |
| `hub_ops/call.rs` | `call_reasoning_llm` → `call_extract_llm` | ~1 行 |
| `recall.rs` | `call_distill_llm` → `call_extract_llm` | ~1 行 |
| `config.env` | 删除 MINIMAX/REASONING 配置 | -12 行 |

**总计**: ~270 行新增，~30 行修改，~12 行删除

## 实施顺序

1. **Phase 1**: 新建 `claude_pool.rs`，MemoryServer 集成
2. **Phase 2**: #5/#6/#7 hub_ops 切换到 pool（低频，先验证 pool 可靠性）
3. **Phase 3**: #4 foundry_ops evolution 切换
4. **Phase 4**: #1 daily batch distill（核心改造）
5. **Phase 5**: #2/#8 切换到 Qwen，删除 MiniMax/Reasoning config
6. **Phase 6**: 清理积压（一次性对 6 个项目跑 batch distill）

## 开放问题

1. **并发数 2 vs 3**: claude CLI 每进程 ~50MB，同时还有 Voyage/SiliconFlow 请求。
   建议先 2，观察后调整。
2. **Daily 时间**: 凌晨 3:00 CST？需要确认机器是否常开。如果是笔记本，
   改为 "tachi 启动后首次 + 之后每 24h"。
3. **context window**: GLM-5.1 context 128K，mega prompt 20 组 × ~5 条 × 300 字
   = ~30K 字 ≈ ~45K token，在窗口内。但积压清理时可能需要分批。
4. **DistillOutcome 多条**: 当前 1 job → 1 条。Daily batch 产出多条需要
   改 `DistillOutcome::Wrote(Vec<String>)`。
