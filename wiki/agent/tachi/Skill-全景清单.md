# Skill 全景清单

> 扫描时间: 2026-05-03 | 路径: Tachi Hub (global), Superpowers (backup), Claude Plugins, Gstack Index, Minara

---

## 总览

| 来源 | 数量 | 位置 |
|------|------|------|
| Tachi Hub (global) — skill | 110 | `~/.tachi/global/memory.db` → `hub_capabilities` |
| Tachi Hub (global) — MCP | 8 | `~/.tachi/global/memory.db` → `hub_capabilities` |
| Tachi Hub (global) — Virtual | 1 | `~/.tachi/global/memory.db` → `hub_capabilities` |
| Superpowers (Gemini backup) | 14 | `~/antigravity-gemini-reset-backup-…/superpowers/skills/` |
| Claude Official Plugins | 26 | `~/.claude/plugins/marketplaces/claude-plugins-official/` |
| Claude Custom Skills | 2 | `~/.claude/skills/` (gstack-index, minara) |
| **总计 (去重前)** | **161** | |

---

## 一、按类型分组的完整表格

### 🧠 Brainstorm / Design / Planning (22)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| brainstorming | "You MUST use this before any creative work - explores user intent, requirements and design before implementation" | superpowers | backup `…/superpowers/skills/brainstorming/SKILL.md` |
| superpowers-brainstorming | (同上, Tachi Hub 注册版) | tachi-hub | `skill:superpowers-brainstorming` |
| office-hours | YC-style brainstorming: startup mode (forcing questions) or builder mode (design thinking) | gstack / tachi-hub | `skill:office-hours` |
| plan-ceo-review | CEO/founder-mode plan review: rethink problem, find 10-star product | gstack / tachi-hub | `skill:plan-ceo-review` |
| plan-design-review | Designer's eye plan review — rates each design dimension 0-10 | gstack / tachi-hub | `skill:plan-design-review` |
| plan-eng-review | Eng manager-mode: lock in architecture, data flow, edge cases, test coverage | gstack / tachi-hub | `skill:plan-eng-review` |
| autoplan | Auto-review pipeline: runs CEO + design + eng reviews sequentially with auto-decisions | gstack / tachi-hub | `skill:autoplan` |
| writing-plans | Use when you have a spec or requirements for a multi-step task, before touching code | superpowers | backup `…/superpowers/skills/writing-plans/SKILL.md` |
| superpowers-writing-plans | (同上, Tachi Hub 注册版) | tachi-hub | `skill:superpowers-writing-plans` |
| design-consultation | Create a complete design system (aesthetic, typography, color, layout) → DESIGN.md | gstack / tachi-hub | `skill:design-consultation` |
| design-shotgun | Generate multiple AI design variants, compare side-by-side, iterate | gstack / tachi-hub | `skill:design-shotgun` |
| design-html | Turn approved AI mockup into production-quality HTML/CSS | gstack / tachi-hub | `skill:design-html` |
| design-review | Visual QA: find spacing issues, hierarchy problems, AI slop, then fix them | gstack / tachi-hub | `skill:design-review` |
| frontend-design | Create distinctive, production-grade frontend interfaces with high design quality | anthropic-plugin / tachi-hub | `skill:frontend-design` |
| canvas-design | Create beautiful visual art in .png and .pdf documents using design philosophy | tachi-hub | `skill:canvas-design` |
| brand-guidelines | Applies Anthropic's official brand colors and typography to artifacts | tachi-hub | `skill:brand-guidelines` |
| theme-factory | Toolkit for styling artifacts with a theme — 10 pre-set themes with colors/fonts | tachi-hub | `skill:theme-factory` |
| web-artifacts-builder | Tools for creating elaborate multi-component HTML artifacts (React, Tailwind, shadcn) | tachi-hub | `skill:web-artifacts-builder` |
| playground | Creates interactive HTML playgrounds — self-contained single-file explorers | anthropic-plugin | `~/.claude/plugins/…/playground/skills/playground/SKILL.md` |
| prd | Generate a Product Requirements Document (PRD) for a new feature | tachi-hub | `skill:prd` |
| project-init | 项目初始化引导，从需求拆解到目录结构搭建、技术选型和开发规范建立 | tachi-hub | `skill:project-init` |
| long-term-plan | (长期规划模板) | tachi-hub | `skill:long-term-plan` |

### 🛠 Implement / Development (20)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| executing-plans | Load plan, review critically, execute all tasks, report when complete | superpowers | backup `…/superpowers/skills/executing-plans/SKILL.md` |
| superpowers-executing-plans | (同上, Tachi Hub) | tachi-hub | `skill:superpowers-executing-plans` |
| subagent-driven-development | Execute plan by dispatching fresh subagent per task, with two-stage review | superpowers / tachi-hub | `skill:superpowers-subagent-driven-development` |
| dispatching-parallel-agents | Delegate tasks to specialized agents with isolated context | superpowers / tachi-hub | `skill:superpowers-dispatching-parallel-agents` |
| test-driven-development | Write the test first. Watch it fail. Write minimal code to pass. | superpowers / tachi-hub | `skill:superpowers-test-driven-development` |
| ship | Ship workflow: merge base, run tests, review diff, bump version, create PR | gstack / tachi-hub | `skill:ship` |
| land-and-deploy | Merge PR, wait for CI, verify production health via canary checks | gstack / tachi-hub | `skill:land-and-deploy` |
| setup-deploy | Configure deployment settings (Fly.io, Render, Vercel, Netlify, etc.) | gstack / tachi-hub | `skill:setup-deploy` |
| document-release | Post-ship docs update: sync README/CHANGELOG/CONTRIBUTING with what shipped | gstack / tachi-hub | `skill:document-release` |
| using-git-worktrees | Creates isolated git worktrees with smart directory selection | superpowers / tachi-hub | `skill:superpowers-using-git-worktrees` |
| finishing-a-development-branch | Guide completion — merge, PR, or cleanup options | superpowers / tachi-hub | `skill:superpowers-finishing-a-development-branch` |
| mcp-builder | Guide for creating high-quality MCP servers | tachi-hub | `skill:mcp-builder` |
| build-mcp-server | Build an MCP server from scratch | anthropic-plugin | `~/.claude/plugins/…/mcp-server-dev/skills/build-mcp-server/SKILL.md` |
| build-mcp-app | Build an MCP app with interactive UI / widgets | anthropic-plugin | `~/.claude/plugins/…/mcp-server-dev/skills/build-mcp-app/SKILL.md` |
| build-mcpb | Package an MCP server for distribution | anthropic-plugin | `~/.claude/plugins/…/mcp-server-dev/skills/build-mcpb/SKILL.md` |
| skill-creator | Create new skills, modify and improve existing skills, measure performance | anthropic-plugin / tachi-hub | `skill:skill-creator` |
| writing-skills | TDD applied to process documentation — write and test skills | superpowers / tachi-hub | `skill:superpowers-writing-skills` |
| ralph | Convert PRDs to prd.json format for Ralph autonomous agent system | tachi-hub | `skill:ralph` |
| workflow-automator | 自动化工作流编排，CI/CD 配置、脚本生成、定时任务和 Git Hooks | tachi-hub | `skill:workflow-automator` |
| ask-codex | Ask Codex (OpenAI CLI) for second opinion / parallel implementation | tachi-hub | `skill:ask-codex` |

### 🔍 Review / QA (12)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| requesting-code-review | Dispatch code-reviewer subagent to catch issues before they cascade | superpowers / tachi-hub | `skill:superpowers-requesting-code-review` |
| receiving-code-review | Verify before implementing code review feedback — technical rigor over social comfort | superpowers / tachi-hub | `skill:superpowers-receiving-code-review` |
| review | Pre-landing PR review: SQL safety, LLM trust boundaries, conditional side effects | gstack / tachi-hub | `skill:review` |
| qa | Systematically QA test a web app and fix bugs found, with before/after evidence | gstack / tachi-hub | `skill:qa` |
| qa-only | Report-only QA testing — structured bug report without fixing anything | gstack / tachi-hub | `skill:qa-only` |
| codex | OpenAI Codex CLI wrapper: independent code review, adversarial challenge, consult | gstack / tachi-hub | `skill:codex` |
| verification-before-completion | Evidence before assertions — run verification before claiming done | superpowers / tachi-hub | `skill:superpowers-verification-before-completion` |
| design-review | Designer's eye QA: visual inconsistency, spacing, hierarchy, AI slop | gstack / tachi-hub | `skill:design-review` |
| coding-code-review-lens | Four-lens code review scoring template | tachi-hub | `skill:coding-code-review-lens` |
| retro | Weekly engineering retrospective: commit analysis, work patterns, per-person | gstack / tachi-hub | `skill:retro` |
| doc-coauthoring | Structured workflow for co-authoring documentation | tachi-hub | `skill:doc-coauthoring` |
| daily-review | 每日工作回顾与洞察分析，对前一天数据总结与建议 | tachi-hub | `skill:daily-review` |

### 🐛 Debug (10)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| systematic-debugging | ALWAYS find root cause before attempting fixes. Symptom fixes are failure. | superpowers / tachi-hub | `skill:superpowers-systematic-debugging` |
| investigate | Systematic 4-phase root cause debugging. Iron Law: no fixes without root cause | gstack / tachi-hub | `skill:investigate` |
| careful | Safety guardrails for destructive commands (rm -rf, DROP TABLE, force-push) | gstack / tachi-hub | `skill:careful` |
| freeze | Restrict file edits to a specific directory for the session | gstack / tachi-hub | `skill:freeze` |
| unfreeze | Remove freeze boundary, allow edits to all directories again | gstack / tachi-hub | `skill:unfreeze` |
| guard | Full safety mode: destructive warnings + directory-scoped edits combined | gstack / tachi-hub | `skill:guard` |
| coding-debug-pattern | Capture repeatable debug patterns for coding agents | tachi-hub | `skill:coding-debug-pattern` |
| coding-gotcha-capture | Promote a recurring coding pitfall into a permanent gotcha note | tachi-hub | `skill:coding-gotcha-capture` |
| cso | Chief Security Officer mode: OWASP Top 10 audit, STRIDE threat modeling | gstack / tachi-hub | `skill:cso` |
| trajectory-distiller | Distill execution traces into reusable skill documents | tachi-hub | `skill:trajectory-distiller` |

### 🌐 Browser / Testing (7)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| browse | Fast headless browser: navigate, interact, screenshot, verify, diff before/after | gstack / tachi-hub | `skill:browse` |
| connect-chrome | Launch real Chrome with Side Panel extension for live browser control | gstack / tachi-hub | `skill:connect-chrome` |
| setup-browser-cookies | Import cookies from real browser into headless session for auth pages | gstack / tachi-hub | `skill:setup-browser-cookies` |
| benchmark | Performance regression detection: page load times, Core Web Vitals, bundle size | gstack / tachi-hub | `skill:benchmark` |
| canary | Post-deploy canary monitoring: watch for console errors and perf regressions | gstack / tachi-hub | `skill:canary` |
| webapp-testing | Complete browser automation with Playwright | tachi-hub | `skill:playwright-skill` (webapp-testing) |
| gstack | Fast headless browser — the umbrella browse skill | gstack / tachi-hub | `skill:gstack` |

### 📝 Content / Writing / Distill (18)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| blog-post-writer | 将零散的想法或原始文章转化为指定风格的公众号文章 | tachi-hub | `skill:blog-post-writer` |
| baoyu-format-markdown | Formats plain text or markdown files with frontmatter, headings, bold, lists | tachi-hub | `skill:baoyu-format-markdown` |
| baoyu-markdown-to-html | Converts Markdown to styled HTML with WeChat-compatible themes | tachi-hub | `skill:baoyu-markdown-to-html` |
| baoyu-url-to-markdown | Fetch any URL and convert to markdown using Chrome CDP | tachi-hub | `skill:baoyu-url-to-markdown` |
| baoyu-danger-x-to-markdown | Converts X (Twitter) tweets and articles to markdown with YAML front matter | tachi-hub | `skill:baoyu-danger-x-to-markdown` |
| baoyu-post-to-x | Posts content and articles to X (Twitter) using real Chrome with CDP | tachi-hub | `skill:baoyu-post-to-x` |
| baoyu-post-to-wechat | (Post to WeChat) | tachi-hub | `skill:baoyu-post-to-wechat` |
| baoyu-cover-image | Generates article cover images with 5 dimensions | tachi-hub | `skill:baoyu-cover-image` |
| baoyu-article-illustrator | Analyzes article structure, identifies positions requiring visual aids | tachi-hub | `skill:baoyu-article-illustrator` |
| baoyu-compress-image | Compresses images to WebP or PNG with automatic tool selection | tachi-hub | `skill:baoyu-compress-image` |
| baoyu-image-gen | AI image generation with OpenAI, Google and DashScope APIs | tachi-hub | `skill:baoyu-image-gen` |
| baoyu-infographic | Generates professional infographics with 20 layout types and 17 visual styles | tachi-hub | `skill:baoyu-infographic` |
| baoyu-slide-deck | Generates professional slide deck images from content | tachi-hub | `skill:baoyu-slide-deck` |
| baoyu-comic | (Comic generation) | tachi-hub | `skill:baoyu-comic` |
| baoyu-xhs-images | (Xiaohongshu images) | tachi-hub | `skill:baoyu-xhs-images` |
| internal-comms | Write internal communications using company formats | tachi-hub | `skill:internal-comms` |
| claude-md-improver | Audit and improve CLAUDE.md files in repositories | anthropic-plugin | `~/.claude/plugins/…/claude-md-management/skills/claude-md-improver/SKILL.md` |
| deep-review | 深度工作分析与项目洞察，从更长时间维度分析工作模式 | tachi-hub | `skill:deep-review` |

### 🎨 Image / Media / Art (7)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| algorithmic-art | Creating algorithmic art using p5.js with seeded randomness | tachi-hub | `skill:algorithmic-art` |
| gemini-image | 生成图片、画图、绘画、AI作图 | tachi-hub | `skill:gemini-image` |
| baoyu-danger-gemini-web | Generates images and text via reverse-engineered Gemini Web API | tachi-hub | `skill:baoyu-danger-gemini-web` |
| slack-gif-creator | Animated GIFs optimized for Slack | tachi-hub | `skill:slack-gif-creator` |
| ffmpeg-usage | 基于Ffmpeg和第三方API的音视频处理 | tachi-hub | `skill:ffmpeg-usage` |
| imagemagick-conversion | Convert and manipulate images with ImageMagick | tachi-hub | `skill:imagemagick-conversion` |
| remotion-video | 使用 Remotion 框架以编程方式创建视频 | tachi-hub | `skill:remotion-video` |

### 📄 Document Formats (4)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| pdf | Read, merge, split, rotate PDF files | tachi-hub | `skill:pdf` |
| docx | Create, read, edit Word documents | tachi-hub | `skill:docx` |
| pptx | Create slide decks, pitch decks, presentations (.pptx) | tachi-hub | `skill:pptx` |
| xlsx | Open, read, edit spreadsheet files (.xlsx, .csv, .tsv) | tachi-hub | `skill:xlsx` |

### 📊 Trading / Finance (7)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| minara | Crypto trading & wallet, AI market analysis via Minara CLI (v3.0.2) | custom / tachi-hub | `~/.claude/skills/minara/SKILL.md` |
| trading-lesson-extractor | Turn failed or costly trades into permanent lessons | tachi-hub | `skill:trading/lesson-extractor` |
| trading-position-review | Periodic position review template | tachi-hub | `skill:trading/position-review` |
| trading-position-snapshot | Ephemeral position snapshot template | tachi-hub | `skill:trading/position-snapshot` |
| trading-post-trade-review | Post-trade review template | tachi-hub | `skill:trading/post-trade-review` |
| trading-pre-market-briefing | Pre-market operating checklist for trading agents | tachi-hub | `skill:trading/pre-market-briefing` |
| trading-regime-playbook | Market regime playbook template | tachi-hub | `skill:trading/regime-playbook` |

### 🔧 Meta / Plugin Dev / Architecture (12)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| using-superpowers | Establish how to find and use skills, requiring Skill tool invocation first | superpowers / tachi-hub | `skill:superpowers-using-superpowers` |
| gstack-index | Index of 31 gstack skills — use as reference, not loaded directly | custom / tachi-hub | `~/.claude/skills/gstack-index/SKILL.md` |
| gstack-upgrade | Upgrade gstack to the latest version | gstack / tachi-hub | `skill:gstack-upgrade` |
| learn | Manage project learnings: review, search, prune, export | gstack / tachi-hub | `skill:learn` |
| plugin-structure | Create a plugin, scaffold a plugin, understand plugin architecture | anthropic-plugin | `~/.claude/plugins/…/plugin-dev/skills/plugin-structure/SKILL.md` |
| command-development | Create slash commands for plugins | anthropic-plugin | `~/.claude/plugins/…/plugin-dev/skills/command-development/SKILL.md` |
| skill-development | Create a skill, add a skill to a plugin | anthropic-plugin | `~/.claude/plugins/…/plugin-dev/skills/skill-development/SKILL.md` |
| hook-development | Create hooks (PreToolUse/PostToolUse/Stop) | anthropic-plugin | `~/.claude/plugins/…/plugin-dev/skills/hook-development/SKILL.md` |
| agent-development | Create agents, add agents, write subagent dispatchers | anthropic-plugin | `~/.claude/plugins/…/plugin-dev/skills/agent-development/SKILL.md` |
| plugin-settings | Store plugin configuration | anthropic-plugin | `~/.claude/plugins/…/plugin-dev/skills/plugin-settings/SKILL.md` |
| mcp-integration | Add MCP server, integrate MCP, configure MCP in plugins | anthropic-plugin | `~/.claude/plugins/…/plugin-dev/skills/mcp-integration/SKILL.md` |
| coding-architecture-decision | ADR template for architecture decisions | tachi-hub | `skill:coding/architecture-decision` |

### 📚 Other / Utility (14)

| Skill Name | Description (≤100字) | 来源 | 文件路径 |
|---|---|---|---|
| claude-automation-recommender | Analyze codebase and recommend Claude Code automations | anthropic-plugin | `~/.claude/plugins/…/claude-code-setup/skills/claude-automation-recommender/SKILL.md` |
| math-olympiad | Math olympiad problem solving | anthropic-plugin | `~/.claude/plugins/…/math-olympiad/skills/math-olympiad/SKILL.md` |
| session-report | Generate HTML report of Claude Code session usage | anthropic-plugin | `~/.claude/plugins/…/session-report/skills/session-report/SKILL.md` |
| writing-rules | Create hookify rules for Claude | anthropic-plugin | `~/.claude/plugins/…/hookify/skills/writing-rules/SKILL.md` |
| example-skill | Demonstrate skill format | anthropic-plugin | `~/.claude/plugins/…/example-plugin/skills/example-skill/SKILL.md` |
| example-command | Example user-invoked skill demonstrating frontmatter | anthropic-plugin | `~/.claude/plugins/…/example-plugin/skills/example-command/SKILL.md` |
| deepl | Translate texts, documents, XLIFF via DeepL API | tachi-hub | `skill:deepl` |
| ppocrv5 | Extract text from images, PDFs with PPOCRv5 OCR | tachi-hub | `skill:ppocrv5` |
| niuma-help | 牛马AI 产品使用引导与帮助 | tachi-hub | `skill:niuma-help` |
| feishu-doc-reader | Read Feishu (Lark) documents via official API | tachi-hub | `skill:feishu-doc-reader` |
| data-analysis | (数据分析) | tachi-hub | `skill:data-analysis` |
| claude-skills-zh-cn | 中文版本的 Anthropic Skills 技能集合 | tachi-hub | `skill:claude-skills-zh-cn` |
| coding-refactor-checklist | Checklist for safe refactors | tachi-hub | `skill:coding/refactor-checklist` |
| coding-test-strategy | Testing strategy template by code type | tachi-hub | `skill:coding/test-strategy` |

### 🔌 MCP Servers (8 + 1 virtual)

| MCP Name | Description | 来源 | 
|---|---|---|
| mcp:exa | Exa AI MCP server for web search and content extraction | tachi-hub |
| mcp:tavily | Tavily web search & extract MCP server | tachi-hub |
| mcp:context7 | Resolve library IDs and query up-to-date documentation | tachi-hub |
| mcp:longbridge | Hong Kong / US stock market data, quotes, positions, orders | tachi-hub |
| mcp:vision | BigModel vision MCP for screenshot and chart analysis | tachi-hub |
| mcp:web-reader | BigModel reader MCP for turning URLs into markdown | tachi-hub |
| mcp:web-search | BigModel search MCP for web search results | tachi-hub |
| mcp:zread | BigModel zread MCP for repo/document reading | tachi-hub |
| vc:web_search | Virtual: Routes web search across Exa + Tavily | tachi-hub |

---

## 二、去重分析

以下是同一个 skill 在多处出现的清单:

| Skill | Superpowers (backup) | Tachi Hub | Claude Plugin | Custom (~/.claude/skills) |
|---|---|---|---|---|
| brainstorming | ✅ SKILL.md | ✅ `skill:superpowers-brainstorming` | — | — |
| writing-plans | ✅ SKILL.md | ✅ `skill:superpowers-writing-plans` | — | — |
| executing-plans | ✅ SKILL.md | ✅ `skill:superpowers-executing-plans` | — | — |
| subagent-driven-dev | ✅ SKILL.md | ✅ `skill:superpowers-subagent-driven-development` | — | — |
| dispatching-parallel-agents | ✅ SKILL.md | ✅ `skill:superpowers-dispatching-parallel-agents` | — | — |
| test-driven-development | ✅ SKILL.md | ✅ `skill:superpowers-test-driven-development` | — | — |
| systematic-debugging | ✅ SKILL.md | ✅ `skill:superpowers-systematic-debugging` | — | — |
| writing-skills | ✅ SKILL.md | ✅ `skill:superpowers-writing-skills` | — | — |
| using-git-worktrees | ✅ SKILL.md | ✅ `skill:superpowers-using-git-worktrees` | — | — |
| using-superpowers | ✅ SKILL.md | ✅ `skill:superpowers-using-superpowers` | — | — |
| verification-before-completion | ✅ SKILL.md | ✅ `skill:superpowers-verification-before-completion` | — | — |
| finishing-a-dev-branch | ✅ SKILL.md | ✅ `skill:superpowers-finishing-a-development-branch` | — | — |
| requesting-code-review | ✅ SKILL.md | ✅ `skill:superpowers-requesting-code-review` | — | — |
| receiving-code-review | ✅ SKILL.md | ✅ `skill:superpowers-receiving-code-review` | — | — |
| frontend-design | — | ✅ | ✅ claude-plugins-official | — |
| skill-creator | — | ✅ | ✅ claude-plugins-official | — |
| gstack-index | — | ✅ `skill:gstack-index` | — | ✅ `~/.claude/skills/gstack-index/` |
| minara | — | — | — | ✅ `~/.claude/skills/minara/` |

**去重后独立 skill 数量: ~119 个** (110 hub skill + 8 MCP + 1 virtual - overlap with superpowers)

---

## 三、映射到 Tachi 图书馆的核心 Skill 列表

以下按优先级列出值得作为 Tachi Hub 核心保留的 skill:

### 🏆 Tier 1 — 核心工作流 (必须保留)

| Skill | 理由 |
|---|---|
| brainstorming | 所有创意工作的入口，强制设计先行 |
| writing-plans | 多步任务的标准规划流程 |
| executing-plans / subagent-driven-development | 执行层核心，子 agent 分发 |
| test-driven-development | TDD 铁律，所有实现前必用 |
| systematic-debugging / investigate | 调试双核: superpowers 版 + gstack 版 |
| verification-before-completion | 完成前验证铁律 |
| ship | 标准发布流程 |
| review | PR review 核心 |
| frontend-design | 前端高质量实现 |
| minara | 金融/加密核心 skill |
| gstack-index | gstack 31 skill 的入口索引 |

### 🥈 Tier 2 — 高价值专业能力

| Skill | 理由 |
|---|---|
| design-consultation / design-shotgun / design-html / design-review | 完整设计工作流4件套 |
| plan-ceo-review / plan-eng-review / plan-design-review / autoplan | 规划审核3+1件套 |
| qa / qa-only / browse / connect-chrome | QA 与浏览器测试 |
| careful / freeze / unfreeze / guard | 安全防护4件套 |
| office-hours | YC 风格头脑风暴 |
| cso | 安全审计 |
| skill-creator / writing-skills | 元 skill: 创建新 skill |
| mcp-builder + build-mcp-server/app/mcpb | MCP 开发工具链 |
| blog-post-writer + baoyu-* 系列 | 内容创作完整链 |
| trading-* (6个) | 交易复盘体系 |
| docx / pdf / pptx / xlsx | 文档格式处理 |

### 🥉 Tier 3 — 辅助 / 实验性

| Skill | 理由 |
|---|---|
| algorithmic-art, gemini-image, remotion-video | 创意/媒体生成 |
| deepl, claude-skills-zh-cn | 多语言支持 |
| feishu-doc-reader, data-analysis | 工具集成 |
| coding-* 系列 (5个) | 编码模式库 |
| trajectory-distiller, learn, retro | 学习与回顾 |
| 所有 external plugin (discord/telegram/imessage) | 通讯集成 |

---

## 四、Tachi Hub 统计

| 指标 | 数值 |
|---|---|
| Total hub_capabilities (global) | 119 |
| — skill type | 110 |
| — mcp type | 8 |
| — virtual type | 1 |
| 项目级 hub_capabilities | 0 (所有项目 DB 为空) |
| Superpowers backup skills | 14 |
| Claude official plugin skills | 26 |
| Custom skills (~/.claude/skills) | 2 |

---

*生成时间: 2026-05-03T07:50 UTC*
