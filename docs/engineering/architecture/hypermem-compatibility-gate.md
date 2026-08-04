---
title: "Hypermem Compatibility Gate"
summary: "Gate for adopting the Tachi memory kernel from Hypermem without preserving a trading-specific kernel fork."
category: "engineering/architecture"
organize: true
---
# Hypermem Compatibility Gate

This gate is the compatibility contract Hypermem / Hyperion must satisfy before
cherry-pick convergence onto the shared Tachi memory kernel. It exists to keep
the durable memory kernel portable while leaving trading judgment downstream.

This is not a Quant migration plan, a fork-preservation plan, or a trading
strategy spec. Hypermem may keep a thin adapter, fixture corpus, and policy
configuration. It must not keep a copy-paste kernel fork.

This gate is not a claim that Hypermem can cut over today. It is the target
contract for convergence. The golden-corpus recall gate (#708) remains a
required cutover prerequisite, and the scorer/decay hook (#791) remains the
target non-fork path for A-share policy.

> **Status update, 2026-08-03 (#1585):** the owner-ratified boundary now
> frames convergence concretely — Hypermem embeds the `portable-kernel`
> library only, opens explicitly supplied Hyperion-owned databases under the
> `PortableKernel` schema profile, and injects policy via `KernelPolicy`
> (recall/decay/embed) instead of any `TACHI_*` environment. This gate's
> content remains the reference for what must be proven at cutover; read it
> through the #1585 boundary and `portable-kernel-split.md` § "Supported
> dependency surface".

## Boundary

Tachi owns the portable kernel:

- memory CRUD, graph edges, provenance, and schema migration safety
- hybrid vector / FTS recall and rerank integration
- scoring and decay extension points
- lifecycle metadata and recall diagnostics

Hypermem owns the downstream adapter and policy:

- `hypermemory_*` aliases and caller compatibility
- HAPI / trading-harness request and response shape
- trading categories, symbols, market/session hints, and provenance labels
- A-share-specific scoring and decay policy configuration
- product trading verdict logic outside Tachi core

The gate must run from local docs, fixtures, and memory-kernel APIs only. It must
not require Tachi dispatch, GitHub, PR, issue, ship, or release surfaces.

## Compatibility Checklist

| Area | Gate question | Required decision |
|---|---|---|
| `hypermemory_*` aliases | Do existing callers depend on function names, CLI names, env keys, or JSON field names with the `hypermemory_` prefix? | Keep aliases as an adapter shim until consumers move, then delete. Do not add alias names to the kernel. |
| HAPI / trading harness bridge | Does the bridge need request fields, response fields, or smoke hooks that are not portable memory behavior? | Allowed adapter / policy. The bridge translates to kernel calls and keeps HAPI-specific envelope fields downstream. |
| Direct `memories` table readers | Does downstream read the SQLite `memories` table directly instead of using a kernel API? | Shim for one convergence window only. Direct table reads are not a formally supported public kernel API. |
| Trading metadata and categories | Are `domain`, category, symbol, market, timeframe, source, and provenance labels preserved through save, search, and recall diagnostics? | Upstream backflow candidate when the field is domain-neutral metadata preservation; adapter policy when it is trading-only vocabulary. |
| A-share decay policy | Does recall ranking depend on market/session age, policy half-life, or stale-signal handling specific to A-share memories? | Allowed adapter / policy through the scorer / decay hook. No financial strategy defaults in portable Tachi memory policy. |

## Difference Classification

| Downstream difference | Classification | Gate requirement |
|---|---|---|
| `hypermemory_*` facade names, old env aliases, old command aliases | Delete / retire after shim window | Fixture lists aliases and their replacement Tachi kernel call. New code uses Tachi names. |
| HAPI request envelope, trading harness smoke entrypoints, Quant-local service names | Allowed adapter / policy | Adapter translates without changing kernel schema or core ranking defaults. |
| Direct SQLite `memories` table reader | Shim, then retire | Provide a compatibility reader pinned to documented columns for one convergence window; add migration warnings; move consumers to `memory_get` / search APIs. |
| Metadata preservation for category, source, symbol, market, timeframe, strategy tag, provenance | Upstream backflow candidate when generic | Add kernel tests for arbitrary metadata round-trip and provenance preservation; keep trading vocabulary downstream. |
| A-share freshness, session-aware decay, and market-closure handling | Allowed adapter / policy | Implement through domain scorer / decay policy injection, not by forking scorer code. |
| Trading verdict, position advice, strategy scoring, or product-specific risk decisions | Delete / retire from kernel | Keep out of Tachi core entirely; adapter may store evidence and labels but not encode strategy. |
| Recall diagnostics needed for downstream quality gates | Upstream backflow candidate | Kernel should expose stable score components, vector/FTS fallback mode, and metadata/provenance visibility. |

## Direct Reader Decision

Direct reads of the SQLite `memories` table are **not** a supported public kernel
API. The compatibility decision is **shim, then retire**:

1. Hypermem may ship a temporary direct-reader shim only to bridge existing
   consumers after the #786 direct-read fix.
2. The shim must pin its selected columns and must fail loudly when required
   columns or metadata keys are missing.
3. New downstream code must use current kernel bindings such as
   `tachi_memory(action="get")`, `tachi_memory(action="search")`, and
   `tachi_memory(action="recall_simulate")` where available. Rich
   `recall_diagnostics` and documented export views are target exports, not
   current public API promises.
4. The shim is removed once the convergence fixture proves callers no longer
   depend on raw table access.

The kernel may preserve storage compatibility as an implementation concern, but
it does not promise raw table stability to downstream products.

## Domain Scorer And Decay Plug-In

Hypermem can plug in domain scoring and decay without forking the kernel by
registering a policy object that receives the kernel's neutral recall candidate,
score components, timestamps, and metadata, then returns additive or bounded
multiplier adjustments with diagnostics.

Target shape:

- `DomainScorer`: receives candidate metadata and query context; may add
  domain-neutral diagnostics plus downstream-owned score adjustments.
- `DecayPolicy`: receives memory type, timestamps, and domain metadata; returns
  a decay factor and reason code.
- `RecallDiagnostics`: records score stability inputs, vector/FTS contribution,
  fallback mode, metadata fields used, and provenance fields preserved.

References only:

- OMP / Mnemopi type-specific decay is evidence that different memory types need
  different decay curves. Tachi should expose type-aware hooks, not copy OMP's
  policy constants.
- Hindsight confidence / reinforcement is evidence that confidence can be
  reinforced by later hits. Tachi may expose confidence and reinforcement fields,
  but Hypermem owns any trading interpretation.

The portable kernel may provide default no-op or generic policies once #791
lands. Until then, A-share freshness, trading-session semantics, and signal
half-lives remain downstream adapter policy and must not be implemented by
forking core scorer code.

## Minimum Recall-Quality Evidence

Before Hypermem adopts the shared kernel, #708's golden-corpus gate and its own
convergence fixture must show:

1. **Score stability:** top-k rank and score components remain within the
   approved tolerance for representative trading-memory queries. Any intentional
   movement is recorded as an adapter policy decision or upstream backflow.
2. **Vector / FTS fallback behavior:** vector-present, vector-missing,
   FTS-present, FTS-partial, and hybrid fallback cases are covered. A missing
   vector must degrade visibly in diagnostics, not silently erase the memory.
3. **Domain metadata preservation:** category, symbol, market, timeframe,
   source, strategy tag, and any domain-neutral metadata survive save, search,
   recall diagnostics, and direct-reader shim export.
4. **Trading-memory provenance:** source agent, source system, original memory
   id when present, bridge path, and adapter version are preserved or explicitly
   mapped. No convergence path may drop provenance to make scores match.
5. **Operator-surface independence:** the fixture and gate run without Tachi
   dispatch, GitHub, issue, PR, ship, release, or close-loop tools.

## Gate Fixture

The machine-readable companion fixture is
[`hypermem-compatibility-gate.fixture.json`](./hypermem-compatibility-gate.fixture.json).
Docs tests parse it and assert that:

- every checklist area required by issue #793 is represented
- every downstream difference is classified as `allowed_adapter_policy`,
  `upstream_backflow_candidate`, or `delete_retire`
- the direct-reader decision is `shim_then_retire`
- minimum recall-quality evidence covers score stability, vector/FTS fallback,
  domain metadata preservation, and trading-memory provenance
- required surfaces exclude dispatch, GitHub, ship, and other operator workflow
  surfaces

## Related

- #770 parent convergence track
- #786 direct-read fix
- #791 scorer / decay hook
- #803 Hindsight / OMP design evidence
