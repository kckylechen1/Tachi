# Downstream Sync Surface & Cratesplit Continuation Plan

> **Status:** historical operating-plan snapshot (2026-07-09), derived from
> live trees and kernel doctrine.
> **Current implementation:** `portable-kernel` is a facade over `memcore`
> with admin features disabled, and `portable-server` serves that facade over
> MCP. The current Tachi Cargo inventory contains no direct ZeroClaw
> integration.
> **Current boundary, owner-ratified 2026-08-03 (#1585):** Hypermem is the
> supported external consumer — it embeds the `portable-kernel` library only,
> opening explicitly supplied Hyperion-owned databases; Tachi product adapters
> remain downstream. See `portable-kernel-split.md` § "Supported dependency
> surface". The 2026-07-17 (#1195) ZeroClaw candidacy stands as a separate,
> non-exclusive direction; neither design has direct Cargo integration here.
> **Historical provenance:** the HyperTachi and HyperMemory convergence plan
> below stopped being the package purpose when the fork separated on
> 2026-07-14 (Hyperion-HyperTachi `9da2015`). The body remains the 2026-07-09
> record of that sync surface and catch-up experiment, not current consumer
> guidance.
> **Implements / anchors:** #770, #771, #790–#794, #833, #847, #1195.  
> **Companion docs:** [`kernel-surface-v1.md`](./kernel-surface-v1.md),  
> [`hypermem-compatibility-gate.md`](./hypermem-compatibility-gate.md),  
> [`host-adapter-lifecycle-v1.md`](./host-adapter-lifecycle-v1.md),  
> [`component-governance-v0.fixture.json`](./component-governance-v0.fixture.json).

## 1. Why this page exists

GLM-era cratesplit (#833) cut operator leaves out of `tachi-server`. That is
healthy for **Tachi compile isolation**, but it must not be continued as
“keep carving the monorepo” without a **downstream sync surface**.

Three products share memory *ideas* but hang onto Tachi in three different ways.
Cratesplit that ignores those hang-points creates silent fork drift (Hypermem)
or wrong work (trying to path-dep zeroclaw onto Tachi crates).

## 2. Live inventory (this machine, 2026-07-09)

| Product | Path | How it relates to Tachi | Memory stack today |
|---|---|---|---|
| **Tachi (upstream)** | `~/Desktop/Sigil` → `kckylechen1/tachi` | Source of portable kernel + full operator product | `memcore` + fat `tachi-server` + extracted leaves |
| **Quant / HyperMemory** | `~/Desktop/Quant_Analyzer_2026` | Submodule `hypermemory` → `kckylechen1/Hyperion-HyperTachi` (**fork of Tachi**, `upstream = tachi`) | Vendored workspace: `memcore` + `tachi-server` only; product binary `tachi-server` / shipped as `hyperion-tachi` / `hypermemory` |
| **zeroclaw (engineering)** | `~/Projects/zeroclaw` → `kckylechen1/zeroclaw` | **No Cargo dep on Tachi.** Memory is first-party `zeroclaw-memory` backends. Product rule: route trading memory via hapi-edge / HyperMemory MCP, never host generic tachi MCP for trading | `zeroclaw-memory` (sqlite/lucid/postgres/qdrant/markdown). HyperMemory custom backend **retired** (#634 option C) |
| **RomanBath + modified zeroclaw** | `/Volumes/Storage/RomanBath` (repo `kckylechen1/romanbath`) | Product shell over zeroclaw; **chat memory is `zeroclaw-memory-sigil`** (Sigil-shaped ACT-R/FTS/hybrid), not Tachi crates | `zeroclaw-memory-sigil` (scorer, schema, dreaming, chat partition by character). Branch tip carries memory-sigil FTS/RRF fixes (`feat/vector-recall-channel`) |
| **Quant’s embedded zeroclaw** | `Quant_Analyzer_2026/zeroclaw` submodule → `kckylechen1/zeroclaw` | Same as Projects zeroclaw pin, **not** the RomanBath fork | No `zeroclaw-memory-sigil` in that pin |

### 2.1 Scale / drift signals (approximate)

| Tree | `memcore` LOC | `tachi-server` LOC | Notes |
|---|---:|---:|---|
| Tachi | ~23k | ~169k (incl. tests) | Many product modules; cratesplit leaves extracted |
| Hyperion-HyperTachi (Quant pin `21a43095`) | ~15k | ~50k | **Behind Tachi main by hundreds of commits**; still monolithic `capture_gate.rs`, still has `dispatch_ops/` |
| RomanBath `zeroclaw-memory-sigil` | n/a (own crate) | n/a | Ports **ideas** (ACT-R, hybrid, FTS Chinese) from Sigil/Tachi scorer lineage without depending on `memcore` as a crate |

HyperTachi still declares `upstream = https://github.com/kckylechen1/tachi.git`.
That is the only tree that can “feel” cratesplit as a merge conflict surface.

## 3. Hang-point model (what each downstream actually needs)

```
                    ┌─────────────────────────────┐
                    │  Tachi portable kernel      │
                    │  memcore + schema +     │
                    │  recall/vector/events/ready │
                    └─────────────┬───────────────┘
                                  │
           ┌──────────────────────┼──────────────────────┐
           │                      │                      │
           ▼                      ▼                      ▼
   Hyperion-HyperTachi     HyperMemory MCP          zeroclaw-memory-sigil
   (vendored fork)         (runtime binary)         (RomanBath chat)
           │                      │                      │
           ▼                      ▼                      ▼
   Quant hypermemory/      hapi-edge / trading      ChatMemoryStore
   build + ship            DBs under data/tachi     per-character scope
```

| Consumer | Needs from Tachi | Does **not** need |
|---|---|---|
| Hyperion-HyperTachi | Stable **`memcore` API + schema**, portable save/search/status, vector readiness, scorer hooks for A-share policy | `tachi_gh`, dispatch/ship/merge, hub CLI, rescue, card evolution, agent-md layering |
| Quant runtime | A **thin HyperMemory binary** (or MCP URL) bound to trading/project DBs | Full Tachi operator surface in-process |
| Projects zeroclaw | Contract vocabulary only (retain/recall/readiness via adapter or MCP); **not** crate path deps | Tachi monorepo layout |
| RomanBath zeroclaw | Either keep `zeroclaw-memory-sigil` as product policy **or** eventually call portable Tachi via MCP; today **owns its own SQLite stack** inspired by Sigil scorer | Auto-merge of Tachi cratesplit into monorepo |

## 4. Portable crate whitelist / blacklist

### 4.1 Whitelist — may flow to HyperTachi / portable bundles

| Crate / surface | Role | Downstream rule |
|---|---|---|
| **`memcore`** (portable: `default-features = false`) | Canonical rows, edges, store, scorer types, search primitives | **Primary sync unit.** Prefer package or subtree merge of this crate alone. |
| **`portable-kernel`** | Facade re-export of portable `memcore` (admin off) | Current package boundary. Per #1195, it is a candidate for a future ZeroClaw-native module after design and integration. Historically it was the recommended dependency for HyperTachi and adapter workspaces; see [`portable-kernel-split.md`](./portable-kernel-split.md). |
| Schema / migrations owned by core | `memories`, FTS, edges, access history, event ledger columns | Breaking changes need dual-gate: Tachi tests + Hypermem gate fixture |
| Vector / backfill readiness | Coverage, degraded mode | Via core + readiness APIs, not Foundry product UI |
| Event projection hooks | Continuity projection for adapters | Neutral events only; no GitHub/dispatch events required |
| Library identity / project routing | Correct DB binding | Required for Quant multi-DB layout |
| **MCP memory actions** on a **stripped server profile** | `tachi_memory` search/save/briefing (or HyperMemory aliases); readiness lives on `tachi_status` | Profile must deny/omit `tachi_gh`, dispatch ship, PR lifecycle |

### 4.2 Blacklist — Tachi-only; never required for Hypermem/zeroclaw/RomanBath memory

| Crate / area | Why |
|---|---|
| `tachi-dispatch`, `tachi-merge-ops` | Operator dispatch / worktree merge |
| `memory-server-hub-cli`, `memory-server-rescue`, `memory-server-manifest-audit` | Operator DB/skill governance |
| `memory-server-prompt-envelope` | Dispatch prompt overlays |
| `gh_ops`, ship, safe_merge, PR lifecycle | GitHub product |
| `component_governance_ops` as a **runtime dep** of Hypermem | Governance is Tachi control-plane; cutover uses fixtures, not a second binary |
| Full `dispatch_ops` / `shell_ops` campaign machinery | Must not be a Hypermem cutover prerequisite (#793/#794) |

### 4.3 Borderline (decide per change)

| Crate | Rule |
|---|---|
| `memory-server-capture-gate` | Gate policy is good; Hypermem still has in-tree `capture_gate.rs`. Prefer **shared logic in core or a tiny no-product crate**, not a server-only leaf that Hypermem must vend. |
| `tachi-llm` / `tachi-foundry` | Needed for embed/rerank/backfill. Hypermem may keep a thinner LLM client; do not force full Foundry product surfaces. |
| `tachi-params` | OK if limited to memory/readiness param types; not if it becomes a dump of every facade enum. |

## 5. What GLM cratesplit already did (downstream lens)

| Slice | Downstream impact |
|---|---|
| Extract rescue / hub-cli / manifest-audit / merge-ops | **Good** — clarifies operator-only code. HyperTachi should **not** re-import these when catching up. |
| Extract capture-gate / prompt-envelope / i18n | **Low compile win**, **medium fork friction** if HyperTachi is rsynced naively. |
| Rerank seam (search ↛ foundry hard edge) | **Relevant** to recall quality; Hypermem/RomanBath-sigil must re-validate hybrid+rerank behavior if they cherry-pick. |
| Drop aws-lc | **Build hygiene**; independent of API. |
| Main `tachi-server` still ~169k | **Cold-compile goal unmet**; next cuts must be large **and** portable-aware. |

**Do not** continue the “tiny leaf” campaign. Next cuts must either:

1. shrink **portable** compile units HyperTachi will actually merge, or  
2. quarantine **operator** code so HyperTachi can delete it without hunting paths.

## 6. Continuation plan — how to keep splitting

### Phase 0 — Freeze the surface (docs + gates, 1–2 days)

1. Treat this document as the cratesplit **downstream law** (#833 / #770).  
2. CI already has `downstream_dogfood` + hypermem gate fixture tests — keep them on any PR that touches recall/schema/readiness.  
3. Component governance (#771 leaves) classifies Hypermem / zeroclaw / RomanBath; use `tachi_component plan` for cutover checklists, not ad-hoc rsync.

**Exit:** every cratesplit PR template links whitelist/blacklist; HyperTachi catch-up PRs open with an explicit “portable-only” file list.

### Phase 1 — Carve a **portable workspace package** (highest ROI for Quant)

Goal: Hyperion-HyperTachi stops being “old monorepo clone” and becomes:

```
portable-workspace/
  memcore/          # path or published crate
  memory-server-lite/   # optional: MCP/CLI profile = remember(retired name)/coordinate-lite
```

Concrete Tachi-side work:

| Step | Action | Downstream effect |
|---|---|---|
| 1.1 | Define Cargo feature or package set `portable-kernel` that builds **only** whitelist crates + a server binary **without** linking dispatch/gh/merge | HyperTachi can depend on that package set instead of whole tree |
| 1.2 | Move or keep all **schema/scorer/store** changes in `memcore` first | Single merge surface for HyperTachi |
| 1.3 | Document HyperTachi catch-up procedure: merge `memcore` → rebuild Hypermem binary → run hypermem gate fixture + Quant trading smoke | Stops “sync whole tachi-server” |
| 1.4 | Explicitly delete/ignore operator modules on HyperTachi when catching up (`dispatch_ops` product lanes, ship, etc.) unless product still needs them | Aligns with #793 “no operator surface dependency” |

**Do not** start Phase 1 by extracting more 100-LOC leaves.

### Phase 2 — Split **large portable clusters** (cold compile that helps everyone)

Only extract clusters that HyperTachi would also want as units:

| Priority | Cluster (today inside Tachi) | Target crate | Portable? |
|---|---|---|---|
| P0 | Store/open/schema/migrations already in core | keep / further modularize **inside** `memcore` | yes |
| P0 | Hybrid search + scorer + rerank glue | `memcore` or `memory-recall` **without** Foundry product deps | yes |
| P1 | Vector backfill / coverage | `memory-vector` or core submodule | yes |
| P1 | Continuity event ledger + projection read models | `memory-events` (neutral events only) | yes |
| P2 | Status/readiness snapshot used by adapters | thin `memory-readiness` or core API | yes |
| — | `gh_ops`, `dispatch_ops`, `complete_ops` campaign, arena, shell ship | stay in Tachi product binary; extract only for **Tachi** compile isolation | **no** |

**Rule:** if a proposed crate cannot be described without “GitHub / worktree / PR / card evolution”, it is **Tachi-only** and must not appear in HyperTachi’s required graph.

### Phase 3 — Adapter lanes (zeroclaw / RomanBath), not monorepo isomorphism

| Downstream | Strategy |
|---|---|
| **Projects zeroclaw** | Keep first-party `zeroclaw-memory`. Integrate Tachi via **MCP adapter** (chat retain/recall/readiness) per #792; never reintroduce retired HyperMemory backend. |
| **RomanBath `zeroclaw-memory-sigil`** | Treat as **product policy memory** (character partition, dreaming, ACT-R). Port **algorithms and tests** from Tachi/Sigil scorer lineage when useful; do **not** replace with a path dep on `memcore` until a deliberate cutover. When Tachi changes hybrid/RRF/FTS Chinese behavior, open a RomanBath PR to `zeroclaw-memory-sigil` with discrimination tests (RB already has this culture: memory-sigil FTS/RRF commits). |
| **Quant trading** | Runtime path = HyperMemory binary + `data/tachi/projects/*`. Engineering memory may use Tachi project DBs; still no requirement to compile Tachi operator crates into hapi-edge. |

### Phase 4 — Cutover gates (before declaring “one kernel”)

| Gate | Owner | Evidence |
|---|---|---|
| Hypermem compatibility fixture | Tachi + HyperTachi | #793 fixture + score/decay hook |
| Downstream dogfood | Tachi CI | #794 shapes: hypermem + zeroclaw_chat_agent |
| Recall quality floor | Tachi | #708 golden corpus before trading cutover claims |
| Component plan | Tachi governance | #798 plan for `tachi-memory-kernel` → hypermem / zeroclaw / romanbath |
| RomanBath chat | RomanBath | `zeroclaw-memory-sigil` tests green; optional MCP dual-run |

## 7. Recommended near-term sequence (ordered)

1. **Stop leaf-only cratesplit** unless the leaf is clearly Tachi-operator quarantine.  
2. **✅ Landed: portable packaging (Phase 1.1)** — `memcore` feature `admin` (default on) + facade crate `portable-kernel` (`default-features = false`). See [`portable-kernel-split.md`](./portable-kernel-split.md).  
3. **✅ HyperTachi catch-up experiment (2026-07-09):** portable `memcore` only on pin `21a43095` → branch `experiment/portable-memcore-catchup` / [HyperTachi#20](https://github.com/kckylechen1/Hyperion-HyperTachi/pull/20). Result: **0** HT-only core files; **14** mechanical server API fixes; core tests 254/278 green; `tachi-server` check green; trading policy lifted to `hypermem_policy`. See [`portable-kernel-split.md`](./portable-kernel-split.md) § results.
4. **Next large extract:** recall/search stack **without** foundry product deps (extends #876).
   - ✅ Pure hybrid-floor RRF blend (`merge_rerank_order_with_hybrid_floor`,
     `apply_blend_relevance`, `HYBRID_HEAD_FRACTION`) lives in `memcore`
     (`search/rerank_blend.rs`); product crate keeps LLM Voyage I/O only.  
5. **RomanBath:** keep `zeroclaw-memory-sigil` as the chat owner; open an issue there for “sync scorer behavior from Tachi kernel surface” rather than monorepo merge.  
6. **zeroclaw Projects:** no cratesplit work required; only MCP/adapter contract when #792 consumers need updates.  
7. Update #833 body: current LOC, extracted leaves, **and** this downstream law.

## 8. Anti-patterns

| Anti-pattern | Why it hurts |
|---|---|
| Rsync entire Tachi `tachi-server` into HyperTachi after each cratesplit | Reintroduces operator surfaces; multiplies conflicts |
| Path-depending Quant or RomanBath on `~/Desktop/Sigil/crates/*` | Breaks CI, packaging, and multi-machine builds |
| Putting portable APIs only in `memory-server-*` product crates | Forces Hypermem to depend on operator graph |
| Assuming RomanBath zeroclaw == Projects zeroclaw | RB has `zeroclaw-memory-sigil` + memory-sigil commits; Projects pin does not |
| Closing #833 because leaves extracted | Main crate still huge; portable cutover unfinished |

## 9. Acceptance for “cratesplit is healthy for downstream”

- [ ] Whitelist/blacklist in this doc match what HyperTachi actually merges.  
- [ ] A HyperTachi PR can update `memcore` without importing dispatch/gh/merge crates.  
- [ ] Tachi `downstream_dogfood` + hypermem gate fixtures stay green on portable PRs.  
- [ ] RomanBath chat memory has an explicit owner (`zeroclaw-memory-sigil`) and a manual port path for scorer/FTS improvements.  
- [ ] Projects zeroclaw remains free of Tachi crate deps; trading memory still goes through HyperMemory/hapi.  
- [ ] #833 progress text cites this document.

## 10. Path cheat-sheet (operators)

| Tree | Absolute path (this workstation) |
|---|---|
| Tachi | `/Users/kckylechen/Desktop/Sigil` |
| Quant | `/Users/kckylechen/Desktop/Quant_Analyzer_2026` |
| Quant HyperMemory submodule | `…/Quant_Analyzer_2026/hypermemory` → Hyperion-HyperTachi |
| zeroclaw (engineering) | `/Users/kckylechen/Projects/zeroclaw` |
| RomanBath | `/Volumes/Storage/RomanBath` |
| RomanBath zeroclaw (modified) | `/Volumes/Storage/RomanBath/zeroclaw` (`zeroclaw-memory-sigil`) |
