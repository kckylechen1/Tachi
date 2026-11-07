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

**藏经阁（Tachi）** 者，专为机巧巨构（Autonomous AI Agents）所塑之潜渊识海也。一器常驻，以 [MCP](https://modelcontextprotocol.io/) 之契经 HTTP 显世；持久记忆、混合搜魂、因果缘线、按域分藏、加密宝库、跨灵核传信、工务调度，尽归一阁，皆以 SQLite 为基，**不假外物**。

其名取自《攻壳机动队》之塔奇克马——以共享记忆进化出灵识之机巧战车。

---

## 版次

今版 `v2.0.0`，藏库卷册之 schema 为三十九。`main` 之上另有未刊之新法：一方之契唯走 HTTP，入门归于一途，宗门辨识、简报、汉文搜魂皆有精进。详询 [CHANGELOG.md](CHANGELOG.md)。

自 1.9.x 升册，或旧以 stdio 子进程唤起本阁者，必先读 [docs/UPGRADING.md](docs/UPGRADING.md)，再依 [`docs/INSTALL.md` Step 1b](docs/INSTALL.md) 之序：先停守护，备份库册与旧器，离线改名，乃升册，逐条核其结果。旧器拒不启新册，回退当以备份为本。

---

## 铸器

```bash
brew tap kckylechen1/tachi && brew install tachi
```

或颁此符诏：

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v2.0.0/scripts/install.sh)" -- --skip-plugin
```

---

## 启阵

先启常驻之阵：

```sh
tachi --daemon --no-project-db --port 6919 serve
```

本阁一方之契唯走 HTTP；客端不得自唤守护，单呼 `tachi` 或 `tachi serve` 亦不开 MCP 之管。再于宿主道籍添此配置，他种宿主宜用 `scripts/setup_agent_mcp.py` 生其对应格式：

```json
{
  "mcpServers": {
    "tachi": {
      "type": "http",
      "url": "http://127.0.0.1:6919/mcp",
      "headers": {
        "x-tachi-workspace-root": "/absolute/workspace"
      }
    }
  }
}
```

宿主若不能报其宗门，本阁亦可凭每次调用所附之宗门元数据或 MCP Roots 自辨所属之库。模型密钥只入常驻守护之环境，不入客端道籍。`VOYAGE_API_KEY` 为搜魂之引；`SILICONFLOW_API_KEY` 为抽绎摘要之荐配。详询 [`docs/INSTALL.md`](docs/INSTALL.md)。

---

## 镇派绝学

- **⚡ 玄铁剑心**：Rust 纯血核心，SQLite + sqlite-vec，亚十毫秒级五脉混合检索（设计目标）。
- **🗂️ 藏经阁流**：以 `path` 分层（如 `/user/preferences`、`/project/architecture`），各阁互不沾染。
- **🔍 五脉归元**：语义、词法、时间衰减、图谱激活蔓延、RRF 融合五路合一；汉文长句须词组足覆方得入选，零散单字不复为噪。
- **🕸️ 因果千丝**：存录之时，同实体者自相牵丝；搜魂之时，图谱一脉循丝蔓延。
- **🔌 宗门为先**：大千识海 `~/.tachi/global/tachi-memory.db`，宗门密库 `<git-root>/.tachi/tachi-memory.db`；先查宗门，后汇大千与共享 Wiki，诸宗互不相扰。
- **📜 简报与工务榜**：谁司何事、事至何阶，一览可知；“行毕”与“交付”分而记之；未改之典籍不复重送，以省 token。
- **🔐 藏经密室**：Argon2id + AES-256-GCM 本地加密宝库，逐秘 ACL，多钥轮换；外部 MCP 之钥与 OAuth 之令亦藏于此，不入 Agent 道籍。
- **🎯 万宝楼**：外部 MCP 之器登记一次，诸 Agent 经同一连接即见其器，名作 `server__tool`。

---

## Agent 一门

寻常 Agent 不必择令旗，接入即得 `tachi_memory`、`tachi_task`、`tachi_agent_eval`、`tachi_staff`、`tachi_a2a` 与只读 `vault_connections` 六器。旧 `standard` 之名仍通此门；旧 `observe`、`coordinate` 保留各自动作之限。Worker 合同仍用 `delegate`，减 `tachi_agent_eval` 为五器，不得递遣偏师。宿主若通 MCP Skills 之法，可自 `skill://tachi/SKILL.md` 随需取用法之卷。

`operate`/`ops`、`admin`/`emergency` 及特权别名皆已退役，不再受 CLI、环境变量或 HTTP 元数据之选。维护当走受支持之 owner CLI，逐次验其私令，不扩 Agent 之目录，私令亦不入客端道籍。其所用之 handler 留于内部，为 Ops/admin 兼容之路；公开之面虽减，非物理尽删。

---

## 天地灵气

```bash
VOYAGE_API_KEY=your_voyage_key_here
SILICONFLOW_API_KEY=your_siliconflow_key_here
SILICONFLOW_BASE_URL=https://api.siliconflow.cn/v1/chat/completions
SILICONFLOW_MODEL=Qwen/Qwen3.5-27B
```

后台抽取、摘要、每日蒸馏径走所配 OpenAI-compatible API 脉；别有已设 provider 可退，穷尽方明记 `LANE_OUTAGE`。寻常 reasoning/chat 则先试 Claude CLI，后转所配 API。寻常部署只需 Voyage + SiliconFlow 二脉即可。

---

## 藏经禁忌

- 每个 SQLite 库同一时间只容一实例写入。
- 切勿将宝库置于云同步之地（iCloud、Dropbox、OneDrive）。
- 勿在服务器运行时以 `sqlite3` 直接写入。
- 休以 `kill -9` 强断，宜待其优雅自散。

---

## 门规

尊奉 [AGPLv3](LICENSE) 誓约 © 2026 Kyle Chen 保其长青。本阁今不纳外来之 PR，详见 [CONTRIBUTING.md](CONTRIBUTING.md)。
