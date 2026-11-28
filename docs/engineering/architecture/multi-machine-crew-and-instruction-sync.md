# Multi-Machine Crew & Layered Instruction Sync

Status: umbrella design contract under #1129 and #871.
Slice 1 (local layered instruction bundle preflight and atomic projection via
`tachi harness plan` / `tachi harness sync` over `tachi.instruction_bundle.v1`)
is implemented. Slices 2–3 (`tachi.presence_sync/v1`,
`tachi.coordination_mail/v1`, and bundle transport inside
`tachi-state-v1.bundle`) are specified here for subsequent bounded leaves and
are **not** claimed as live network surfaces.

## 1. Four-pillar architecture for Multi-Machine Crew

Owner-operated machines (laptops, workstations, build servers, cloud dev hosts)
coordinate through Tachi across four bounded pillars rather than an open remote
tool mesh:

| Pillar | Contract | Scope & status |
| --- | --- | --- |
| 1. Shared instruction & constitution layering | `tachi.instruction_bundle.v1` | **Slice 1 (implemented)**: deterministic local rendering of shared constitution + carrier-scoped adapters + non-synced `machine.local.md` overlay into host global instruction targets. |
| 2. Signed federated presence | `tachi.presence_sync/v1` | **Slice 2 (planned, #1171)**: bounded, signed `WorkClaim` board projections (`agent_identity_id`, `host_id`, `canonical_project`, `claim_id`, branch/HEAD, mode, state, heartbeat) exchanged over `mtls-peer/v1` so `board` / `peer_query` surfaces cross-host active work without replicating private memory stores. |
| 3. Cross-host coordination mailbox | `tachi.coordination_mail/v1` | **Slice 3 (planned, #1940 / #2232)**: store-and-forward Outbox → mTLS `/rpc` → destination `a2a_envelopes` for bounded (<= 4 KiB), secret-scrubbed advisory messages addressed to `(recipient_agent_identity_id, host_id | trust_domain)` and consumed at turn boundaries via `tachi_a2a(action="receive")`. |
| 4. Bounded remote jobs & host receipts | `tachi.remote_job/v1` | **Existing contract ([remote-execution-v1.md](remote-execution-v1.md), #1940 / #1934)**: closed job envelopes (`review_revision`, `verify_pr`) with host-emitted provenance receipts. |

## 2. Explicit non-goals (why Tachi is not a transparent service mesh)

General peer-to-peer agent meshes (for example, sidecar proxies that mount
remote MCP servers on loopback and delegate offline capability tokens across
multiple hops) optimize for transparent RPC reachability. Tachi deliberately
rejects that boundary:

- **No remote `/mcp` tool tunneling.** Remote peers never mount or call a
  destination host's local MCP router or Vault routes.
- **No onward multi-hop delegation.** Enrolled peers submit bounded envelopes
  to one destination; workers cannot mint or forward attenuated credentials.
- **No live revocation blind window.** Remote authority continues to terminate
  at mutually authenticated TLS (`mtls-peer/v1`) backed by live SQLite
  revocation checks on every read and write.
- **Dumb TLS pass-through relays only.** Transport relays cannot inspect,
  terminate, or authorize application payloads.
- **No implicit memory replication.** Private memory databases stay local;
  only explicit state-portability bundles or bounded redacted snapshots cross
  hosts.

## 3. Layered instruction bundle contract (`tachi.instruction_bundle.v1`)

### 3.1 Three-layer composition

Global host instruction files drift when portable rules, carrier mechanics, and
machine-specific hardware limits are mixed into one hand-edited file. Tachi
splits them into three strict layers:

1. **Layer 1 — Shared constitution (`constitution/core.md`)**:
   carrier-neutral operating baseline projected to
   `<home>/.agents/constitution/core.md`. Contains no host paths, no machine
   hardware specs, and no carrier-specific tool names.
2. **Layer 2 — Carrier-scoped shared adapters (`targets[]`)**:
   slimmed turn-zero adapters and carrier-owned helper definitions, projected
   only under their host's allowlisted prefix:
   - `constitution`: `.agents/constitution/*.md`
   - `codex`: `.codex/...` (e.g. `.codex/AGENTS.md`)
   - `claude`: `.claude/...` (e.g. `.claude/CLAUDE.md`, `.claude/agents/*.md`,
     `.claude/skills/*/SKILL.md`)
   - `antigravity`: `.gemini/config/...` or `.gemini/antigravity/...`
     (e.g. `.gemini/config/AGENTS.md`)
   - `gemini`: `.gemini/...` (excluding `.gemini/config/` and
     `.gemini/antigravity/`)
   - `cursor`: `.cursor/rules/...`
3. **Layer 3 — Machine-local overlay (`<home>/.tachi/machine.local.md`)**:
   strictly host-local hardware, build-gate, and offload facts. Never copied
   into cross-machine bundles (excluded alongside `.tachi/runs/**` in
   [state-portability.md](state-portability.md)). When present and non-empty,
   it is appended between `<!-- tachi:machine-local:start -->` and
   `<!-- tachi:machine-local:end -->` on targets that opt in via
   `include_machine_local: true`.

Repository-specific `AGENTS.md` / `CLAUDE.md` files continue to travel with
their own Git repositories and are never managed by global instruction sync.

### 3.2 Bundle manifest schema (`bundle.json`)

Default bundle directory: `<home>/.tachi/instructions/` (override with
`--source-dir <PATH>`). Unknown fields fail closed.

```json
{
  "schema_version": "tachi.instruction_bundle.v1",
  "revision": "2026-11-28.1",
  "constitution": {
    "source": "constitution/core.md",
    "target": ".agents/constitution/core.md",
    "max_bytes": 4096
  },
  "targets": [
    {
      "id": "codex-global-agents",
      "host": "codex",
      "source": "adapters/codex/AGENTS.md",
      "target": ".codex/AGENTS.md",
      "include_constitution_import": false,
      "include_machine_local": true,
      "max_bytes": 12288
    },
    {
      "id": "claude-global-md",
      "host": "claude",
      "source": "adapters/claude/CLAUDE.md",
      "target": ".claude/CLAUDE.md",
      "include_constitution_import": true,
      "include_machine_local": true,
      "max_bytes": 12288
    },
    {
      "id": "claude-agent-wizard",
      "host": "claude",
      "source": "adapters/claude/agents/wizard.md",
      "target": ".claude/agents/wizard.md",
      "include_constitution_import": false,
      "include_machine_local": true,
      "max_bytes": 16384
    },
    {
      "id": "claude-skill-dispatch",
      "host": "claude",
      "source": "adapters/claude/skills/dispatch/SKILL.md",
      "target": ".claude/skills/dispatch/SKILL.md",
      "include_constitution_import": false,
      "include_machine_local": false,
      "max_bytes": 12288
    },
    {
      "id": "antigravity-global-agents",
      "host": "antigravity",
      "source": "adapters/antigravity/AGENTS.md",
      "target": ".gemini/config/AGENTS.md",
      "include_constitution_import": false,
      "include_machine_local": true,
      "max_bytes": 12288
    }
  ]
}
```

### 3.3 Portability, byte-0 preservation, and provenance stamp

- **Portable paths only in shared sources**: shared `constitution` and
  `targets[*].source` files must not contain literal `/Users/` or `/home/`
  prefixes. Use `~/` or the single closed placeholder `{{HOME}}` (expanded to
  the resolved target `<home>`). `machine.local.md` may contain host-local
  absolute paths.
- **Byte-0 preservation**:
  - When `include_constitution_import: true`, Tachi prepends
    `@<home>/<constitution.target>\n\n` at byte 0. Setting
    `include_constitution_import: true` on a source starting with YAML
    frontmatter (`---\n`) is rejected so frontmatter parsers are never broken.
  - The provenance stamp is always a single trailing HTML comment line:
    `<!-- tachi:instruction-sync v1 id=<id> revision=<rev> source_sha256=<hex> body_sha256=<hex> -->\n`
    so byte-0 YAML frontmatter (`SKILL.md`, subagent `.md`) and byte-0
    `@import` lines stay intact.
- **Secret and density gates**:
  - Rendered output size (including overlay and stamp) must not exceed the
    declared `max_bytes` (hard ceiling `65536` bytes per target).
  - Shared sources and `machine.local.md` are screened for common credential
    prefixes and private-key blocks before any write.

### 3.4 No-clobber preflight, atomic writes, and `--adopt`

- `tachi harness plan` is strictly read-only. It classifies every selected
  target as `in_sync`, `create`, `update_managed`, `conflict_unmanaged`,
  `conflict_symlink`, or `conflict_modified_managed` (when a stamped file's
  body bytes no longer match `body_sha256`).
- `tachi harness sync` preflights **every** selected target before writing any
  byte:
  - Symlinked `--home`, symlinked source files, symlinked target parents, and
    non-regular targets (directories, FIFOs, sockets) fail closed unconditionally.
  - Any `conflict_*` target blocks the entire sync unless `--adopt` is passed.
  - With `--adopt`, regular unmanaged/edited files are backed up verbatim under
    `<home>/.tachi/backups/harness-sync/<run_id>/files/<rel_target>` and
    replaced symlinks have their `read_link` targets (lossy display + lossless
    base64 bytes on Unix) recorded in a single manifest file
    `<home>/.tachi/backups/harness-sync/<run_id>/symlinks.json`
    (`0700`/`0600`) before atomic sibling temp-file + `rename` replacement.
    Renaming over a symlink replaces the link itself without dereferencing or
    modifying its former target.
  - On completion, Tachi atomically writes
    `<home>/.tachi/harness-sync-receipt.json` (`tachi.harness_sync_receipt.v1`).
- `tachi harness status` remains strictly read-only and recognizes valid
  `tachi:instruction-sync v1` stamps as `managed` (flagging local edits as
  `instruction_sync_local_edit_drift`).

## 4. Future transport integration

Once Slice 1 is stable on local bundles, the same `instructions/` directory
becomes an explicit portable member of `tachi-state-v1.bundle`
([state-portability.md](state-portability.md)) and an optional signed bundle
push over `mtls-peer/v1`. `machine.local.md`, `backups/`, and
`harness-sync-receipt.json` remain strictly host-local.

## References

- [Remote execution V1](remote-execution-v1.md)
- [One mailbox: project coordination](coordination-mailbox.md)
- [Identity and WorkClaim spine](identity-workclaim-spine-v1.md)
- [State portability](state-portability.md)
- [Tachi Cloud Sync Space & Agent Network (future draft)](tachi-cloud-agent-network-future-spec.md)
