# DeepSeek Harness headless adapter

`tachi_staff(action="start", profile="dsh_executor", ...)` resolves to the
native `dsh` worker and `dsh_headless` transport. The server mints its
`LaunchSpec` from the resolved assignment and `ExecutionGrant`, then launches
`dsh --profile headless --json -- <prompt>` through the existing managed
subprocess lifecycle. Staff exposes no command, model, credential, cwd, or
patch input. A worker override cannot substitute OpenCode or another carrier.

The profile registry is the source of truth for selection. The execution grant
is the source of truth for authority, timeout and cwd. A managed cwd retains
the existing descriptor binding; an absent cwd is the canonical Default grant
and inherits the daemon's working directory. Default does not claim a managed
lease. DSH's operator-owned native profile selects model and authentication;
Tachi records their identity as unconfirmed because this headless JSON protocol
does not acknowledge the executed model. No model or auth config is copied into
a new Tachi config entry.

The workspace-write ceiling is **advisory**, without a new sandbox primitive or
certification. Every explicit sandbox request, tool allowlist, permission
bypass, model/command override, max-turn grant, and runtime MCP injection is
refused. The authority compiler treats DSH as shell-capable, so a read-only
profile cannot turn its absent qualification into an advisory read-only claim.
Server cancellation retains the existing same-daemon, revision-checked managed
subprocess control and process-group termination proof. `managed_custom` in a
lifecycle receipt names the reused subprocess control class; the selected
backend and execution backend remain `dsh` and `dsh_headless`.

Completion requires process exit zero, an opening session event, the last
terminal turn's `reason.kind="completed"`, and a terminal final-text event.
A failed turn's final event or an exit-zero malformed stream cannot become
success. `result.md` contains the final answer on success; failed exits retain
their output and protocol failures return a diagnostic. Owner-only
`dsh-events.jsonl` and `dsh-stderr.log` preserve carrier evidence at the same
postflight publication boundary as the result. Completion predicates and
acceptance remain the existing Tachi authorities.

This contract was checked against the public installed `@deepseek-ai/dsh`
`0.2.0-rc.2` package and the
[official CLI README](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/cli/README.md).
The shipped headless composition mounts no approval answerer: a required ask
falls back immediately to `unavailable`, which the tool layer denies. Native
profile and home overrides can change that composition; Tachi does not attest
their permissions. The launcher reads the invoking directory's `.env` before
profile overlays. Operators running an isolated probe should use an isolated
`DSH_HOME` and an empty daemon working directory, and authorize any overlay
separately. The adapter itself creates no native profile, stores no provider
credentials and installs no dependency.

Focused regression coverage checks fixed argv, Default cwd preservation,
unsupported grants, profile selection and override refusal, failed/final-only
JSON streams, and the actual Staff runner's completed/error/nonzero terminal
states using a fake executable. Live authenticated provider execution and
sandbox qualification are separate evidence obligations. Rollback removes the
profile, registry row and adapter wiring; no migration is required.
