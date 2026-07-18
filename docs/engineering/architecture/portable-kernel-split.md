# Portable Kernel Split — memcore vs admin

> **Status:** landed 2026-07-09 (first cut); historical purpose retired and
> future direction recorded 2026-07-17 (#1195).
> **Anchors:** #770, #790–#794, #833 Phase 1.1, #1195.  
> **Companions:** [`kernel-surface-v1.md`](./kernel-surface-v1.md),  
> [`downstream-sync-surface.md`](./downstream-sync-surface.md) (when present).
>
> **Current implementation:** `portable-kernel` re-exports `memcore` with
> admin features disabled. `portable-server` exposes that kernel over MCP.
> The current Tachi Cargo inventory contains no direct ZeroClaw integration.
>
> **Owner ruling 2026-07-17 (#1195):** these packages stay as candidate
> boundaries for a future ZeroClaw-native memory module. The design and direct
> Cargo integration have not landed.
>
> **Historical provenance:** this split was cut for HyperTachi, HyperMemory,
> and adapter convergence. Hyperion-HyperTachi separated on 2026-07-14
> (`9da2015`), so the original consumer framing below is retained only as the
> historical record of why the split was cut.

## Why

`memcore` mixed two audiences:

1. **Portable kernel** - store, schema, hybrid search/scorer, graph, events,
   sandbox. This is the current compile boundary. Per #1195 it is also a
   candidate boundary for a future ZeroClaw-native module after design and
   integration. Historically, HyperTachi, HyperMemory, and adapters motivated
   the split (see § Catch-up experiment results for the 2026-07-09 record).
2. **Tachi admin** - vault secrets, Hub capability catalog, Foundry job queue
   types, Pack / agent_profile product surfaces. Operator product only.

Without a compile boundary, a future embedder or the historical downstream
fork either:

- rsyncs the whole monorepo and inherits operator APIs, or
- hand-picks files and drifts.

## What landed

### Cargo feature on `memcore`

| Feature | Default | Contents |
|---|---|---|
| `admin` | **on** | vault, hub, foundry job types, pack, agent_profile modules + store methods |
| _(none)_ | use `default-features = false` | portable kernel only |

Schema DDL still creates admin tables so a portable build can open DBs written
by full Tachi. Empty tables are fine; portable code simply has no typed API for
them.

### Facade crate `portable-kernel`

```
crates/portable-kernel
  └── depends on memcore with default-features = false
```

- Re-exports the portable `memcore` API.
- `ADMIN_SURFACE_ENABLED == false` by construction.
- Smoke tests: open/upsert/stats without admin types.

### Tachi product unchanged

`tachi-server`, `tachi-hub`, `tachi-foundry`, `tachi-llm`, etc. still depend on
`memcore` with **default features** (admin on). No call-site rewrites.

## How downstream should fork / sync

| Consumer | Sync unit | Feature flag |
|---|---|---|
| Hyperion-HyperTachi | `memcore` **or** `portable-kernel` | `default-features = false` on core |
| HyperMemory runtime binary | same + thin MCP/CLI profile | no dispatch/gh/merge crates |
| RomanBath `zeroclaw-memory-sigil` | algorithm/tests only (not crate path dep) | n/a |
| Projects zeroclaw | MCP adapter contract, not crates | n/a |

**Do not** rsync `tachi-server` + operator leaves into HyperTachi as a
prerequisite for memory quality. Prefer:

```text
1. Merge / vendor crates/memcore (portable build)
2. Rebuild Hypermem binary
3. Run hypermem compatibility gate + trading smoke
```

## Product policy injection (MemCore hooks)

Downstream product policy stays **outside** the kernel. MemCore exposes hooks:

| Hook | On | Default |
|---|---|---|
| `SearchOptions::precision_matchers` | `Vec<Arc<dyn PrecisionMatcher>>` | empty |
| `SearchOptions::decay_policy` | `Option<Arc<dyn DecayPolicy>>` | `None` → `DEFAULT_DECAY_POLICY` |

Example (HyperMemory trading half-lives):

```rust
let opts = SearchOptions {
    decay_policy: Some(Arc::new(TradingDecayPolicy)),
    ..Default::default()
};
store.search(query, Some(opts))?;
```

Do not re-hardcode A-share / character-card decay constants into `memcore`.

## Targeted issue lanes (after this split)

| Lane | Scope | Examples |
|---|---|---|
| **Portable kernel** | schema, CRUD, hybrid/RRF, vector readiness, events | #708 recall floor, #790 surface, #793 hypermem gate |
| **Tachi admin** | vault, hub governance, foundry queue product | vault residual, hub CLI, foundry scheduler |
| **Tachi operator product** | dispatch, gh, ship, merge, rescue | #878 predicate, merge-ops, component governance |
| **Adapter / product policy** | zeroclaw-memory-sigil, HyperMemory MCP profile | #792 adapter, RomanBath chat partition |

Issues should name their lane. A portable PR must not require
`tachi-dispatch` / `tachi_gh` / merge-ops to compile or test.

## Verify

```bash
export CARGO_TARGET_DIR=$HOME/.cache/sigil-shared-target

# Full Tachi product surface (admin on)
cargo test -p memcore

# Portable kernel only
cargo test -p memcore --no-default-features
cargo test -p portable-kernel
```

## Not in this cut

- Physical extract of vault/hub/foundry into separate crates (schema still shared;
  feature gate is enough for fork/compile isolation).
- `memory-server-lite` binary profile (next step once portable packaging is
  dogfooded on the ZeroClaw-native embedder per #1195; historically framed
  around HyperTachi dogfooding, which is no longer the target consumer).
- Moving schema admin tables behind feature flags (would break open of full DBs).

## Acceptance

- [x] `memcore` has documented `admin` feature; default keeps Tachi green.
- [x] `portable-kernel` builds and tests with admin off.
- [x] Portable build still opens a schema that includes admin tables.
- [x] HyperTachi catch-up experiment: portable `memcore` only (2026-07-09).
      See § Catch-up experiment results below.
- [ ] Optional later: extract admin into `memory-admin` crate if feature gate
      proves insufficient for packaging.

## Catch-up experiment results (2026-07-09)

Isolated worktree on Hyperion-HyperTachi pin `21a43095` → branch
`experiment/portable-memcore-catchup` → draft PR
[Hyperion-HyperTachi#20](https://github.com/kckylechen1/Hyperion-HyperTachi/pull/20).

| Metric | Result |
|---|---|
| HT-only files under old `memcore` | **0** (pure older subset) |
| Compile errors in HT `tachi-server` after core replace | **14 → 0** (mechanical API) |
| `memcore` portable tests in HT tree | **254** passed |
| `memcore` full (admin) tests in HT tree | **278** passed |
| `portable-kernel` in HT tree | **2** passed |
| `cargo check -p tachi-server` (HT) | **green** |

Product policy that correctly stayed out of the kernel:

- A-share trading half-lives + session age → HT `hypermem_policy::TradingDecayPolicy`
- Ticker metadata heuristics → HT `hypermem_policy::heuristic_metadata_from_text`

**Conclusion:** portable-only catch-up is viable. Do not rsync operator crates.
Full procedure: HyperTachi
`docs/engineering/portable-memcore-catchup.md` on that experiment branch.
