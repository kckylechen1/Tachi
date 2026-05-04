import json, time, urllib.request, os

SILICONFLOW_KEY = "sk-toybvbtdqznklazzzlkgttivrvsaxejfsckkyymqhoycxgdz"
MINIMAX_KEY = "sk-api-lwbVEBTO32xn0Y54o0PKFKJ1O2z1koMYe_p1wq8NsRj4Zg_rWCBbMbQsLv1AdWuMc_fsB4Rd32cPXQF7L1ZQDJ9ukPXKmI2lQwVePg8oKY7eYoXMnkKG48c"

DISTILL_PROMPT = """你是一个知识蒸馏专家。以下是5条原始记忆碎片，请合并蒸馏成1-2条精炼的结构化知识，输出JSON数组。去掉对话语气，保留具体数字和参数。

碎片1: [决策] V8 评分从二维改成三维：原来只有 trend_score + reversion_score，加入 momentum_factor 后在回测中平均提升了 2.3% 年化。momentum_factor 由 5日/20日量比 + OBV斜率 构成。
碎片2: 舰长说 V8 要加 momentum，我觉得很有道理。量能是最诚实的指标嘛。先在 v8_score.py 里加一个 calc_momentum 函数。
碎片3: V8 momentum_factor 回测结果：2023年数据集上胜率从 52% 提升到 57%，但最大回撤从 -12% 扩大到 -15%。需要加风控约束。
碎片4: 把 momentum_factor 的权重从 0.3 降到 0.2，回撤控制住了 -13%，胜率维持在 56%。这个参数最终定稿。
碎片5: V8 三维评分确认上线。最终配比 trend:reversion:momentum = 0.4:0.4:0.2。PositionSizer 同步更新。"""

REASONING_PROMPT = """分析以下交易场景，给出决策建议：

某A股标的当前状态：
- V8 Grade: A（最高档）
- trend_mode: acceleration（加速上升）
- 60分钟级别：未出现顶分型
- MA60 斜率：+2.3%（强上行）
- 今日成交量：昨日的 0.7 倍（缩量）
- 持仓浮盈：+18%
- 明天开始五一长假（休市5天）

问题：应该在长假前全部清仓、减半仓还是继续持有？请给出具体建议和理由。"""

models = [
    ("DeepSeek-V4-Flash", "https://api.siliconflow.cn/v1/chat/completions", "deepseek-ai/DeepSeek-V4-Flash", SILICONFLOW_KEY),
    ("Qwen3.6-27B", "https://api.siliconflow.cn/v1/chat/completions", "Qwen/Qwen3.6-27B", SILICONFLOW_KEY),
    ("MiniMax-M2.7", "https://api.minimaxi.com/v1/chat/completions", "MiniMax-M2.7", MINIMAX_KEY),
]

def call_model(name, url, model, key, prompt, max_tokens=500):
    payload = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
        **({"enable_thinking": False} if "siliconflow" in url else {})
    }).encode()
    req = urllib.request.Request(url, data=payload, headers={
        "Content-Type": "application/json",
        "Authorization": f"Bearer {key}"
    })
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            d = json.loads(resp.read())
        elapsed = time.time() - t0
        content = d["choices"][0]["message"]["content"]
        usage = d.get("usage", {})
        return content, elapsed, usage.get("prompt_tokens", 0), usage.get("completion_tokens", 0)
    except Exception as e:
        return f"ERROR: {e}", time.time() - t0, 0, 0

print("=" * 60)
print("DISTILL BENCHMARK")
print("=" * 60)
for name, url, model, key in models:
    content, elapsed, inp, out = call_model(name, url, model, key, DISTILL_PROMPT)
    print(f"\n### {name} ({elapsed:.1f}s, in={inp}, out={out})")
    print(content[:600])
    print()

print("=" * 60)
print("REASONING BENCHMARK")
print("=" * 60)
for name, url, model, key in models:
    content, elapsed, inp, out = call_model(name, url, model, key, REASONING_PROMPT, 600)
    print(f"\n### {name} ({elapsed:.1f}s, in={inp}, out={out})")
    print(content[:600])
    print()
