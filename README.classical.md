<div align="center">
  <img src="assets/banner.png" alt="Tachi Banner" width="800" style="margin-bottom: 20px;" />
  <h1>✧ 藏经阁（Tachi）记事</h1>
  <p><strong>为自主灵核所筑之本地首储、工务调度与混合识海总枢</strong></p>
  <p>
    <a href="README.md">English</a> ·
    <a href="README.zh-CN.md">简体中文</a> ·
    <a href="README.classical.md"><b>文言文</b></a>
  </p>
  <p>
    <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/License-AGPLv3-blue.svg" alt="License: AGPLv3"></a>
    <img src="https://img.shields.io/badge/Rust-Edition_2021-orange.svg" alt="Rust">
    <img src="https://img.shields.io/badge/Protocol-MCP-purple" alt="MCP">
  </p>
</div>

---

## 概览

**藏经阁（Tachi）** 者，专为机巧巨构（Autonomous AI Agents）所塑之潜渊识海也。其以 [MCP](https://modelcontextprotocol.io/) 之契显世，集持久记忆、混合搜魂、因果缘线、按域分藏、加密宝库、跨灵核传信、仙诀派发、工务调度于一体，尽以 SQLite 为基，**不假外物**。

其名取自《攻壳机动队》之塔奇克马——以共享记忆进化出灵识之机巧战车。

---

## 铸器

```bash
brew tap kckylechen1/tachi && brew install tachi
```

或颁此符诏：

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.9.0/scripts/install.sh)"
```

---

## 启阵

于灵核 MCP 道籍中添此一段：

```json
{
  "mcpServers": {
    "tachi": {
      "command": "tachi",
      "env": {
        "VOYAGE_API_KEY": "<your-key>",
        "SILICONFLOW_API_KEY": "<your-key>",
        "TACHI_PROFILE": "standard"
      }
    }
  }
}
```

`VOYAGE_API_KEY` 为搜魂引之必需；`SILICONFLOW_API_KEY` 为抽绎摘要之荐配。详询 [`docs/INSTALL.md`](docs/INSTALL.md)。

---

## 镇派绝学

- **⚡ 玄铁剑心**：Rust 纯血核心，SQLite + sqlite-vec，亚十毫秒级五脉混合检索（设计目标）。
- **🗂️ 藏经阁流**：以 `path` 分层（如 `/user/preferences`、`/project/architecture`），各阁互不沾染。
- **🔍 五脉归元**：语义、词法、时间衰减、图谱激活蔓延、RRF 融合五路合一。
- **🕸️ 因果千丝**：图谱引擎织就因果、时序、实体之缘；`add_edge` / `get_edges` / `memory_graph` 深藏不出（`memcore::MemoryStore` 内秘之器，不列 MCP 曲面，#757 已收），唯托 `tachi_save` / `tachi_memory` 自动牵丝、暨五脉归元之图谱蔓延一脉而显其效，别无单列寻迹之诀。
- **🔌 两界分治**：大千识海 `~/.tachi/global/memory.db`，宗门密库 `<git-root>/.tachi/memory.db`。
- **🔐 藏经密室**：Argon2id + AES-256-GCM 本地加密宝库，逐秘 ACL，多钥轮换。
- **🎯 万宝楼**：Skill、MCP、仙诀一次登录，诸路灵核共享。
- **👻 跨界传信**：幽灵低语、看板、交接令牌，跨灵核协同。
- **⚔️ 工务总枢**：`tachi_task` 遣偏师（`action='dispatch'`），`tachi_arena` 记工籍，`tachi_verify` 存验据，`tachi_complete` 录因果。
- **🏭 神经熔炉与维基**：上下文生灭、Agent 进化提案、薪火相传之典籍。

---

## 令旗 Profile

| 令旗 | 用途 |
|------|------|
| `standard` | IDE 灵核之默认，14 器门面集。 |
| `coordinate` | 主尊调度，兼掌 handoff / workflow / orchestrator / task / approve_merge / verify。 |
| `operate` | 运行时适配与 OpenClaw，兼掌 Foundry / Vault 会话。 |
| `delegate` | 小弟偏师，极简 9 器，无派发、无交接。 |
| `admin` | 维护治理，全量法器。 |

未设令旗者，自 v1.0.1 起默认 `standard`。

---

## 天地灵气

```bash
VOYAGE_API_KEY=your_voyage_key_here
SILICONFLOW_API_KEY=your_siliconflow_key_here
SILICONFLOW_BASE_URL=https://api.siliconflow.cn/v1/chat/completions
SILICONFLOW_MODEL=Qwen/Qwen3.5-27B
```

Phase 2 之后，后台调用先走 Claude CLI pool，落败方回退 SiliconFlow。寻常部署只需 Voyage + SiliconFlow 二脉即可。

---

## 藏经禁忌

- 每个 SQLite 库同一时间只容一实例写入。
- 切勿将宝库置于云同步之地（iCloud、Dropbox、OneDrive）。
- 勿在服务器运行时以 `sqlite3` 直接写入。
- 休以 `kill -9` 强断，宜待其优雅自散。

---

## 门规

尊奉 [AGPLv3](LICENSE) 誓约 © 2026 Tachi Authors 保其长青。
