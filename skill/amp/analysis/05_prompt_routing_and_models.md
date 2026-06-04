# Amp Prompt 路由 & 模型配置

> 来源：`~/.amp/bin/amp` 二进制逆向提取

---

## System Prompt 路由逻辑（`NFR` 函数）

```
agentMode === "aggman"    → _FR()  // Aggman 模式（Slack 集成、项目管理）
agentMode === "rush"      → qFR()  // Rush 模式（快速执行）
agentMode === "deep"      → sFR()  // Deep Autonomous（GPT-5.5）
agentMode === "deep" + FF → nFR()  // Deep Fallback（GPT-5.4，feature flag 关闭时）
provider === "openai"     → yFR()  // GPT 模式
model === "gpt-5-codex"   → kFR()  // Codex 模式
provider === "xai"        → SFR()  // xAI 模式
model === "kimi-k2"       → mFR()  // Kimi 模式
provider === "vertexai"   → lFR()  // Gemini 模式（可选 oracle/diagnostics）
default                   → oFR()  // Pair Programming（通用）
```

### 关键发现

1. **9 套不同 prompt**，根据 agentMode + 模型动态切换
2. Deep 模式下通过**服务端 feature flag** 决定用 GPT-5.5 还是 5.4
3. 每套 prompt 针对特定模型特性优化过
4. 用户可通过 `scaffoldCustomizationFile` 替换或追加 prompt

---

## 三种 Prompt 风格对比

| | Deep (sFR) | Deep Fallback (nFR) | Pair Programming (oFR) |
|---|---|---|---|
| **开头** | "You are Amp, an autonomous coding agent" | "You are a pragmatic, effective software engineer" | "You are pair programming with a user" |
| **自驱力** | "carry through implementation and verification rather than stopping at a proposal" | "implement the change" | "follow the user's instructions" |
| **遇到阻碍** | "diagnose why before switching tactics" | 同 Deep | 同 Deep |
| **规划 vs 执行** | "assume they want you to solve the problem" | "assume the user wants you to make code changes" | "assume the user wants you to make code changes" |
| **Read 纪律** | "Read enough code to avoid guessing, then stop" | 同 Deep | "Read enough code to avoid guessing" |
| **反 AI slop** | 无（Deep 模式给 GPT-5.5 不需要） | 有完整前端指导 | 有完整前端指导 |
| **Response Channel** | commentary + final 双通道 | 同 Deep | 同 Deep |
| **验证** | "scale with risk and blast radius" | "Verify your work before reporting" | "verify that the result works" |

---

## 模型注册表

### Anthropic

| 内部 ID | Model ID | 显示名 | 上下文 | 最大输出 | Input/Output ($/M) |
|---|---|---|---|---|---|
| CLAUDE_SONNET_4 | claude-sonnet-4-20250514 | Claude Sonnet 4 | 1M | 32K | $3 / $15 |
| CLAUDE_SONNET_4_5 | claude-sonnet-4-5-20250929 | Claude Sonnet 4.5 | 1M | 32K | $3 / $15 |
| CLAUDE_SONNET_4_6 | claude-sonnet-4-6 | Claude Sonnet 4.6 | 1M | 64K | $3 / $15 |
| CLAUDE_OPUS_4 | claude-opus-4-20250514 | Claude Opus 4 | 200K | 32K | $15 / $75 |
| CLAUDE_OPUS_4_1 | claude-opus-4-1-20250805 | Claude Opus 4.1 | 200K | 32K | $15 / $75 |
| CLAUDE_OPUS_4_5 | claude-opus-4-5-20251101 | Claude Opus 4.5 | 200K | 32K | $5 / $25 |
| CLAUDE_OPUS_4_6 | claude-opus-4-6 | Claude Opus 4.6 | **332K** | 32K | $5 / $25 (fast x6) |
| CLAUDE_OPUS_4_7 | claude-opus-4-7 | Claude Opus 4.7 | **332K** | 32K | $5 / $25 (fast x6) |
| CLAUDE_OPUS_4_8 | claude-opus-4-8 | Claude Opus 4.8 | **332K** | 32K | $5 / $25 (fast x2) |
| CLAUDE_HAIKU_4_5 | claude-haiku-4-5-20251001 | Claude Haiku 4.5 | 200K | 64K | $1 / $5 |

### OpenAI

| 内部 ID | Model ID | 上下文 | 最大输出 | Input/Output |
|---|---|---|---|---|
| GPT_5 | gpt-5 | 400K | 128K | $1.25 / $10 |
| GPT_5_MINI | gpt-5-mini | 400K | 128K | $0.25 / $2 |
| GPT_5_NANO | gpt-5-nano | 400K | 128K | $0.05 / $0.4 |
| GPT_5_1 | gpt-5.1 | 400K | 128K | $1.25 / $10 |
| GPT_5_2 | gpt-5.2 | 400K | 128K | $1.75 / $14 |
| GPT_5_4 | gpt-5.4 | 400K | 128K | $2.5 / $15 |
| **GPT_5_5** | **gpt-5.5** | **400K** | **128K** | **$5 / $30** |
| GPT_5_4_PRO | gpt-5.4-pro | **1.05M** | 128K | $30 / $180 |
| GPT_5_5_PRO | gpt-5.5-pro | **1.05M** | 128K | $30 / $180 |
| GPT_5_CODEX | gpt-5-codex | 400K | 128K | $1.25 / $10 |
| GPT_5_1_CODEX | gpt-5.1-codex | 400K | 128K | $1.25 / $10 |
| GPT_5_2_CODEX | gpt-5.2-codex | 400K | 128K | $1.75 / $14 |
| GPT_5_3_CODEX | gpt-5.3-codex | 400K | 128K | $1.75 / $14 |
| GPT_OSS_120B | openai/gpt-oss-120b | 128K | 32K | — |
| O3_MINI | o3-mini | 200K | 100K | $1.1 / $4.4 |

### Google (Vertex AI)

| 内部 ID | Model ID | 上下文 | 最大输出 | 能力 |
|---|---|---|---|---|
| GEMINI_3_PRO_PREVIEW | gemini-3-pro-preview | 1M | 64K | tools, reasoning, vision |
| GEMINI_3_1_PRO_PREVIEW | gemini-3.1-pro-preview | 1M | 64K | tools, reasoning, vision |
| GEMINI_3_5_FLASH | gemini-3.5-flash | 1M | 64K | tools, reasoning, vision |
| GEMINI_3_PRO_IMAGE | gemini-3-pro-image-preview | 1M | 64K | vision, imageGeneration |

### Moonshot (多 provider)

| 内部 ID | Model ID | Provider | 上下文 | 最大输出 |
|---|---|---|---|---|
| KIMI_K2_INSTRUCT | kimi-k2-instruct-0905 | Moonshot 直连 | **1M** | 32K |
| KIMI_K2_INSTRUCT | accounts/fireworks/models/kimi-k2-instruct-0905 | Fireworks | 230K | 32K |
| KIMI_K2_0905 | moonshotai/kimi-k2-0905 | OpenRouter | 262K | 32K |
| KIMI_K2P5 | moonshotai/Kimi-K2.5 | Basenten | 262K | 32K ($0.6/$3) |

---

## Provider 配置

```javascript
const providers = {
  ANTHROPIC: "anthropic",
  BASENTEN: "baseten",
  OPENAI: "openai",
  XAI: "xai",
  CEREBRAS: "cerebras",
  FIREWORKS: "fireworks",
  GROQ: "groq",
  MOONSHOT: "moonshotai",
  OPENROUTER: "openrouter",
  VERTEXAI: "vertexai",
}
```

---

## Model-Prompt 联合路由（`wFR` / `HFR` 函数）

```javascript
// 根据模型对象路由
function wFR(model) {
  if (model.name === "gpt-5-codex") return "gpt-5-codex";
  if (model.name.includes("kimi-k2")) return "kimi";
  if (model.provider === "openai") return "gpt";
  if (model.provider === "xai") return "xai";
  if (model.provider === "vertexai") return "gemini";
  return "default";
}

// 根据模型名+provider 路由
function HFR(modelName, provider) {
  if (modelName.includes("gpt-5-codex")) return "gpt-5-codex";
  if (modelName.includes("kimi-k2")) return "kimi";
  if (modelName.includes("gpt")) return "gpt";
  if (provider === "xai") return "xai";
  if (provider === "vertexai") return "gemini";
  return "default";
}
```

---

## Feature Flag 驱动的模型选择

```javascript
function NFR({ agentMode, model, provider, serverStatus }) {
  if (agentMode === "aggman") return "aggman";
  if (agentMode === "rush") return "rush";
  if (agentMode === "deep") {
    let canUseGPT55 = ATR(serverStatus);  // 服务端 feature flag
    return canUseGPT55 ? "deep" : "deep-gpt5.4";
  }
  // 根据模型+provider 选 prompt
  let modelConfig = parseModel(`${provider}/${model}`);
  if (modelConfig) return wFR(modelConfig);
  return HFR(model, provider);
}
```

**意义**：模型选择不是写死的，通过服务端 feature flag 动态控制。灰度发布新模型，遇到问题秒级回滚。

---

## Cache 配置

所有 Anthropic 模型配置了 prompt caching：
```javascript
pricing: {
  input: X,
  output: Y,
  cached: X * 0.1,      // cache 读取价格 = input 的 10%
  cacheWrite: X * 1.25,  // cache 写入价格 = input 的 125%
  cacheTTL: 300           // cache 5 分钟
}
```

System prompt 发送时带 `cache_control: { type: "ephemeral" }` 标记。
