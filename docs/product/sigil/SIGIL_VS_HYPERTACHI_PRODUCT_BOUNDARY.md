---
title: "Sigil vs HyperTachi Product Boundary"
summary: "Defines scope separation between Sigil engineering docs and HyperTachi equity intel."
category: "product/sigil"
organize: true
---
# Sigil vs HyperTachi — Product Boundary

> **Status**: Contract | **Last reviewed**: 2026-06-01  
> **Canonical Hyperion copy**: `Hyperion-Quant-SRC/docs/Spec/TACHI_AND_SIGIL_PRODUCT_BOUNDARY.md`

Sigil (藏经阁/Tachi) and HyperTachi share Rust crates but serve **different products**. This document prevents scope creep: engineering doc organize stays in Sigil; HyperTachi handles equity intel only.

---

## Roles

| Product | Repo | Default use |
|---|---|---|
| **Sigil (Tachi)** | [github.com/kckylechen1/tachi](https://github.com/kckylechen1/tachi) | Agentic OS for coding agents: memory, wiki, dispatch, **`tachi_wiki_organize`** |
| **HyperTachi** | Hyperion product build from Quant `hypertachi/` | Finance memory for Hyperion trading agents only |

---

## Sigil owns: engineering documentation

**Tool**: `tachi_wiki_organize` (`docs_ops.rs`)

- Organize project `docs/` (frontmatter, taxonomy, `_index.md`, kanban checkbox sync)
- Distill pipeline target: Notes → `wiki/` → Hub Skills (see `wiki/agent/tachi/Tachi-图书馆架构设计.md`)
- Dogfood on Sigil `docs/`; optional for any coding repo on a **developer profile**

---

## HyperTachi owns: equity intel (not engineering docs)

**DB**: Hyperion `data/tachi/projects/hyperion/memory.db`  
**Defaults**: `project=hyperion`, `domain=equity_trading`

**In scope**

- Broker research report ingest (PDF → chunks → memory)
- News / intel summaries with TTL-friendly paths
- Trading lessons, daily handoffs, decision recall
- `extract_facts` on qualitative content

**Out of scope**

- `tachi_wiki_organize` on Hyperion engineering trees (`docs/Spec`, `InProgress`, …)
- Replacing git-managed agent contracts via memory alone

---

## Implementation checklist (Sigil / HyperTachi fork)

- [ ] **Profile gate**: Hyperion MCP/tool profile excludes `tachi_wiki_organize` for trading agents
- [ ] **`dry_run` mode** for `tachi_wiki_organize` before physical moves
- [ ] **Dogfood**: run organize on Sigil `docs/` (non-destructive dry-run first)
- [ ] **HyperTachi intel organize** (separate track): research path normalization, dedupe — **do not** reuse `handle_wiki_organize` for PDF/intel
- [ ] **Docs sync**: keep this file aligned with Quant Spec copy when boundary changes
- [ ] **Spec vs Memory**: document in setup wizard / save prompts — memory = pointer + conclusion, Spec = git authority (see Hyperion `DOC_GOVERNANCE` §4)

---

## Related (Sigil)

- `wiki/agent/tachi/Tachi-图书馆架构设计.md` — Notes / Wiki / Skill
- `crates/memory-server/src/docs_ops.rs` — organize implementation
- `crates/memory-server/src/tools.rs` — `tachi_wiki_organize` MCP registration

## Related (Hyperion)

- Quant `docs/Spec/RESEARCH_REPORTS_MODULE.md` — report ingest design
- Quant `docs/Spec/HYPERTACHI_RUNBOOK.md` — Hyperion memory operations