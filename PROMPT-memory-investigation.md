# Tachi Memory System Investigation — Full Diagnostic

## Context

This prompt documents a comprehensive investigation of the Tachi memory system and related infrastructure issues. All findings are from a live session on 2026-05-02. The goal is to diagnose why `tachi_search` returns 0 results and identify all related problems in the stack.

---

## 1. Network / Clash Verge (Root Cause of Multiple Failures)

### Problem
Clash Verge Rev runs in TUN + fake-ip mode. The `dns_config.yaml` had an excessively large `fake-ip-filter` blacklist with manually added domains (Google APIs, OpenRouter, X.AI, etc.). Domains NOT in the filter get assigned fake IPs (198.18.0.x) by Clash's DNS resolver. However, the proxy rules were routing some of these domains to DIRECT instead of PROXY, causing connection timeouts.

### What was changed
`~/Library/Application Support/io.github.clash-verge-rev.clash-verge-rev/dns_config.yaml` was simplified to use geosite-based filtering instead of manual domain lists:

```yaml
dns:
  enable: true
  ipv6: false
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-filter:
  - geosite:private
  - geosite:category-ntp
  use-hosts: false
  use-system-hosts: false
  nameserver:
  - https://1.1.1.1/dns-query
  - https://8.8.8.8/dns-query
  proxy-server-nameserver:
  - https://223.5.5.5/dns-query
  - https://223.6.6.6/dns-query
  nameserver-policy:
    geosite:cn:
    - https://223.5.5.5/dns-query
    - https://223.6.6.6/dns-query
  respect-rules: true
```

### Remaining issue
After the DNS config change and Clash restart, `mcp.context7.com` still resolves to a fake IP (198.18.0.5). This is expected in fake-ip mode. The connection through the proxy works (returns 405 in ~6s), but direct TUN routing still times out. This suggests the proxy rules in the active profile (`LKZtjw1XFOWn` — "91 家宽") may route `mcp.context7.com` to DIRECT instead of the proxy group.

**Key domains confirmed blocked/polluted without proxy:**
- `api.voyageai.com` → 198.18.0.25 (fake IP) — Voyage AI embedding API
- `mcp.context7.com` → 198.18.0.5 (fake IP) — Context7 MCP HTTP endpoint
- Both work through proxy (`-x http://127.0.0.1:7897`)

**Key domains that work:**
- `context7.com` → 200 (1.3s) — main site
- `www.google.com` → 200 (1.9s) — through TUN

### Files
- `~/Library/Application Support/io.github.clash-verge-rev.clash-verge-rev/dns_config.yaml` — DNS config (modified)
- `~/Library/Application Support/io.github.clash-verge-rev.clash-verge-rev/verge.yaml` — App config (`enable_tun_mode: true`, `enable_dns_settings: true`)
- `~/Library/Application Support/io.github.clash-verge-rev.clash-verge-rev/config.yaml` — Clash core config
- `~/Library/Application Support/io.github.clash-verge-rev.clash-verge-rev/profiles/LKZtjw1XFOWn.yaml` — Active profile ("91 家宽", final rule: `MATCH,家宽`)

### Action items
- [ ] Check why `mcp.context7.com` and `api.voyageai.com` don't route through proxy under TUN despite `MATCH,家宽` being the final rule
- [ ] Consider whether Tachi and other services should have `HTTPS_PROXY` set in their environment

---

## 2. context7 MCP — All Agent Configs Updated

### Problem
`npx -y @upstash/context7-mcp@latest` hangs because npmmirror registry returns ECONNRESET. The global binary was installed via `npm install -g @upstash/context7-mcp` (v2.2.3 at `/Users/kckylechen/.npm-global/bin/context7-mcp`).

### What was changed
All 6 agent configurations were updated from `npx` or HTTP URL to local binary:

| Agent | Config file | Before | After |
|-------|-------------|--------|-------|
| Claude Code | `~/.claude.json` | `npx -y @upstash/context7-mcp@latest` | `/Users/kckylechen/.npm-global/bin/context7-mcp` |
| Codex (main) | `~/.codex/config.toml` | `url = "https://mcp.context7.com/mcp"` | `command = ".../context7-mcp"` |
| Codex (Cockpit) | `~/.antigravity_cockpit/instances/codex/cli-9c4588fc4374/config.toml` | `url = "https://mcp.context7.com/mcp"` | `command = ".../context7-mcp"` |
| OpenCode | `~/.config/opencode/opencode.json` | `npx -y @upstash/context7-mcp` | `[".../context7-mcp"]` |
| Claude Plugin | `~/.claude/plugins/marketplaces/claude-plugins-official/external_plugins/context7/.mcp.json` | `npx -y @upstash/context7-mcp` | `".../context7-mcp"` |
| MCP Porter | `~/.mcporter/mcporter.json` | `npx -y @upstash/context7-mcp` | `".../context7-mcp"` |

Also updated Tachi Hub's context7 definition in `~/.tachi/global/memory.db` → `hub_capabilities` table.

### Note
The `~/.npmrc` uses `registry=https://registry.npmmirror.com` which is unreliable. The global binary at `/Users/kckylechen/.npm-global/bin/context7-mcp` should be kept up to date manually.

---

## 3. Tachi Search Returns 0 Results — CRITICAL

### Symptom
`tachi_search` (all scopes: memory, wiki, all) returns 0 results for any query, despite having data in the database.

### Root cause chain
1. `tachi_search` needs to generate an embedding vector for the query string
2. It calls Voyage AI API at `https://api.voyageai.com/v1/embeddings`
3. `api.voyageai.com` is DNS-polluted → resolves to 198.18.0.25 (fake IP)
4. Tachi runs in stdio mode as an MCP server — it does NOT inherit `HTTPS_PROXY` from the shell
5. The embedding API call fails silently → vector search returns no results
6. FTS fallback also appears not to be working (or not triggered)

### Evidence
```
$ dig api.voyageai.com +short
198.18.0.25

$ curl -x http://127.0.0.1:7897 ... api.voyageai.com/v1/embeddings
200 0.968s  # Works through proxy

$ tachi serve 2>&1 | grep Vector
Vector search: global=true, project=false  # Tachi thinks vec0 is loaded
```

The `vec0` module IS working inside Tachi (confirmed by `Vector search: global=true` in startup output). Earlier diagnosis using system `sqlite3` CLI showed `no such module: vec0` — this was a false positive because the system sqlite3 binary doesn't have sqlite-vec. Tachi's bundled Rust sqlite has it.

### Fix
Add `HTTPS_PROXY` to Tachi's environment in the MCP server config. For example in `~/.claude.json`:

```json
"tachi": {
  "command": "/opt/homebrew/opt/tachi/bin/tachi",
  "env": {
    "MEMORY_DB_PATH": "/Users/kckylechen/.tachi/global/memory.db",
    "TACHI_PROFILE": "claude-code",
    "HTTPS_PROXY": "http://127.0.0.1:7897"
  }
}
```

This needs to be done for ALL agents that spawn Tachi:
- `~/.claude.json` (Claude Code)
- `~/.codex/config.toml` (Codex)
- `~/.config/opencode/opencode.json` (OpenCode)
- Any other Tachi MCP configuration

### Also investigate
- [ ] Does Tachi read `HTTPS_PROXY` env var and pass it to its HTTP client (reqwest)?
- [ ] Is there a fallback from vector search to FTS when embedding fails?
- [ ] Check `crates/memory-core/src/search.rs` for the search pipeline and error handling

---

## 4. Memory Database — Global vs Project

There are two memory databases:

### Global DB: `~/.tachi/global/memory.db`
- 276 memories, 50 edges, FTS has 271 entries

### Antigravity Project DB: `~/.tachi/projects/antigravity/memory.db`
- 545 memories, 6 edges, 32MB, FTS has 513 entries

Both have the same structural issues (detailed below).

---

## 5. Memory Data Quality Issues

### Global DB (`~/.tachi/global/memory.db`)

| Issue | Count | Details |
|-------|-------|---------|
| No retention_policy | 152/276 (55%) | GC/cleanup can't work properly |
| kanban entries | 57 | Task board items stored as permanent memories, all `pinned` |
| handoff entries | 28 | Session handoffs stored permanently, all `pinned` |
| Empty summary | 2 | `summary=''` breaks search/display |
| Orphan edges | 6 | Edges pointing to non-existent memory IDs |
| Ghost system | 7 messages, 0 reflections | Message bus essentially unused |
| Graph edges | 50 total, all `related_to` weight=0.5 | No semantic relationships |

### Antigravity DB (`~/.tachi/projects/antigravity/memory.db`)

| Issue | Count | Details |
|-------|-------|---------|
| No retention_policy | 293/545 (54%) | Same issue |
| fact category | 374/545 (69%) | Facts not distilled into decisions/experiences |
| extraction source | 225/545 (41%) | Auto-extracted, quality unverified |
| Empty summary | 8 | |
| Ghost system | 0 messages | Completely unused |
| Graph edges | 6 total, all `related_to` | Even worse than global |
| Average text length | 222 chars | Short, low information density |

### Category distributions

**Global:** fact(130), kanban(57), experience(35), handoff(28), decision(11), preference(8), other(5), entity(2)

**Antigravity:** fact(374), decision(83), other(51), experience(20), kanban(13), preference(2), entity(2)

### Source distributions

**Global:** extraction(106), external:mcp(65), external:agent(56), manual(19), external:user_chat(9), external:cli(5), ...

**Antigravity:** extraction(225), external:mcp(140), manual(74), external:user_chat(25), foundry_distill(23), ...

### Action items
- [ ] Audit kanban entries — should they be in a separate store or cleaned up?
- [ ] Audit handoff entries — should be ephemeral, not pinned
- [ ] Backfill retention_policy for entries missing it
- [ ] Fix orphan edges (delete or reconnect)
- [ ] Verify extraction quality for auto-extracted memories
- [ ] Implement FTS fallback when vector search fails

---

## 6. Memory Graph — Non-functional

The memory graph has the following schema:
```sql
CREATE TABLE memory_edges (
    source_id  TEXT NOT NULL,
    target_id  TEXT NOT NULL,
    relation   TEXT NOT NULL,
    weight     REAL NOT NULL DEFAULT 1.0,
    ...
);
```

**Global:** 50 edges, all `related_to` with weight=0.5
**Antigravity:** 6 edges, all `related_to` with weight=0.5

There are no semantic relationship types (e.g., `depends_on`, `contradicts`, `derived_from`, `supersedes`). The graph is effectively just a flat association table with no actionable structure.

### Action items
- [ ] Review the edge creation logic — where are edges generated?
- [ ] Implement richer relationship types
- [ ] Consider whether the graph adds value over simple vector similarity

---

## 7. Ghost System — Unused

Ghost tables exist (messages, topics, subscriptions, cursors, reflections) but are essentially empty:

**Global:** 7 messages, 1 topic, 0 reflections
**Antigravity:** 0 messages, 0 topics, 0 reflections

This is an inter-agent pub/sub message bus that was never activated in production.

### Action items
- [ ] Decide if Ghost should be activated or removed
- [ ] If keeping, integrate with agent workflows (handoffs, notifications)

---

## 8. Wiki System

`tachi_browse` shows 631 wiki entries across categories:

| Category | Count |
|----------|-------|
| /wiki/quant/strategy | 170 |
| /wiki/quant/data-pipeline | 45 |
| /wiki/engineering/devops | 73 |
| /wiki/engineering/architecture | 66 |
| /wiki/agent/tachi | 80 |
| /wiki/agent/openclaw | 53 |
| /wiki/product/hyperion | 14 |
| Other categories | ~130 |

But `tachi_search` with `scope=wiki` returns 0 results — same Voyage API issue as memory search. Wiki entries appear to be stored in the same databases but with `source='wiki'` and `category='wiki'`.

The user mentioned "wiki还在弄" (wiki is still being worked on), suggesting the wiki table/feature may be in a transitional state.

---

## 9. Source Code References

All paths relative to `~/Desktop/Sigil/`:

| File | Purpose |
|------|---------|
| `crates/memory-core/src/db/sqlite_vec.rs` | sqlite-vec registration (auto_extension) |
| `crates/memory-core/src/db/mod.rs` | DB module exports |
| `crates/memory-core/src/db/schema.rs` | Schema init (assumes register_sqlite_vec called before) |
| `crates/memory-core/src/search.rs` | Search pipeline — calls register_sqlite_vec + try_load_sqlite_vec |
| `crates/memory-core/src/lib.rs` | Core lib init — calls register_sqlite_vec in multiple places |
| `crates/memory-server/src/profiles.rs` | Tool profiles (standard=12, delegate=7, admin=148) |

Running binary: `/Users/kckylechen/bin/tachi` → symlink to `tachi.dev.202605021910` (18MB, dev build)
Homebrew binary: `/opt/homebrew/opt/tachi/bin/tachi` (not currently used)
Version: tachi 1.0.0

### Key Cargo dependencies
```toml
rusqlite = { version = "0.32", features = ["backup", "bundled", "vtab"] }
sqlite-vec = "0.1.7-alpha.10"
libsimple = { version = "0.3.0", features = ["rusqlite"] }
```

---

## 10. Summary of Action Items (Prioritized)

### P0 — Fix search
1. Add `HTTPS_PROXY=http://127.0.0.1:7897` to Tachi env for all agents
2. Verify Tachi's reqwest HTTP client respects HTTPS_PROXY
3. Test that `tachi_search` returns results after proxy fix
4. Verify FTS fallback works when embedding fails

### P1 — Data cleanup
5. Clean kanban entries from memories table (57 global, 13 antigravity)
6. Clean stale handoff entries (28 global)
7. Backfill retention_policy for ~445 entries across both DBs
8. Remove 6 orphan edges from global DB

### P2 — Architecture improvements
9. Implement richer graph relationship types beyond `related_to`
10. Decide fate of Ghost system (activate or remove)
11. Verify auto-extraction quality (225+ entries in antigravity)
12. Consider adding proxy config to Tachi's own config (not just env vars)

### P3 — Clash Verge
13. Investigate why TUN routes some domains to DIRECT despite MATCH,proxy rule
14. Consider adding `HTTPS_PROXY` to shell profile for all processes

---

## 11. OpenCode Test Execution Notes

### Voyage API Key Rotation (401 Unauthorized Fix)
Older Voyage keys that appeared in docs/git history must be treated as **compromised** — rotate again in [Voyage dashboard](https://dash.voyageai.com/) if they were ever pushed or pasted into shared files.

Store the active key **only** in `~/.tachi/config.env` (or Tachi Vault), never in the Sigil repo or investigation notes.

**OpenCode / tests:** For `tachi_search` and other embedding paths, ensure `VOYAGE_API_KEY` is set to the **current** dashboard key (`401 Unauthorized` usually means revoked or wrong key).

### GLM-4/GLM-5.1 Usage Issues
[等待舰长补充具体的 GLM 5.1 相关问题...]

---

## 12. Branch Review & Merge SOP

你现在的任务是作为一个高级代码架构审查员和版本控制管家，帮我将 `Sigil` 仓库中积压的 14 个 Feature/Fix 分支进行代码审查，并按顺序合并入 `main` 分支。

### 待合并分支清单与拓扑顺序
请严格按照以下层级顺序（从底层向高层）进行 Review 和 Merge 尝试：

**Phase 1: 底层核心修复 (Bug Fixes)**
- `origin/fix/manifest-self-lock`
- `origin/fix/doctor-checkpoint-safety`
- `origin/fix/distill-llm-fallback-swallow`
- `origin/fix/foundry-skip-reasons`

**Phase 2: 存储架构与多库支持 (Storage & Daemon)**
- `origin/feat/storage-enum-enforcement`
- `origin/feat/manifest-canonicalize`
- `origin/feat/path-routing-rules`
- `origin/feat/worker-multi-db`
- `origin/feat/tachi-repair-tool`

**Phase 3: 上层特性优化 (Features)**
- `origin/feat/split-rerank-maintenance-cache`
- `origin/feat/interactive-setup-wizard`
- `origin/feat/wiki-enhancement` (已有 PR #72，包含最新的更改，请一并审阅)
- `feat/async-dispatch` (我们今天刚写完的异步派发逻辑)

**Phase 4: 工程清理 (Chores)**
- `origin/chore/dead-code-cleanup`
- `origin/docs/storage-audit-2026-04-30`

### 执行流程与纪律 (SOP)
针对列表中的**每一个分支**，请循环执行以下操作：

1. **获取 Diff**：执行 `git diff origin/main...<branch_name>` 阅读该分支改动。阅读 PR 或最新代码。
2. **代码审查 (Code Review)**：
   - 检查是否有明显的内存泄漏、死锁逻辑，或破坏 Rust 生命周期/所有权的粗心写法。
   - 检查数据库 SQL 语句或跨库调用的安全性。
   - 如果发现严重架构缺陷，**暂停合并该分支**，向我汇报。
3. **安全合并 (Safe Merge)**：
   - 切换到 `main` 分支拉取最新代码。
   - 执行合并（或 Rebase）。
   - 如果遇到 **Merge Conflict (冲突)**，请详细向我报告冲突的文件，并提供解决冲突的具体思路或使用终端工具直接解决。
4. **编译验证**：合并后，必须使用 `cargo check -p memory-server` 或 `cargo test` 确保当前主分支不被破坏。
5. **记录状态**：成功合并后，从清单中划除，进入下一个分支。

### Phase 5: 最终编译与部署
当所有分支都成功审查并合并入 `main` 后，请执行最终编译，并将新二进制文件替换系统中的旧版本：
1. 运行 `cargo build --release`
2. 将编译好的二进制（`target/release/tachi` 或类似路径）复制到 `/Users/kckylechen/bin/tachi`。
3. 提示完成更新。

**开始指令**
请先确认你已理解上述拓扑顺序，然后从 **Phase 1** 的第一个分支 `origin/fix/manifest-self-lock` 开始获取 Diff 并进行审查。
