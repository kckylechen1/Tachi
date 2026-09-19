/// MCP `initialize` instructions (also mirrored in agent rule blocks).
pub(crate) fn mcp_server_instructions() -> String {
    "Tachi — memory + policy/ledger copilot for coding agents. Ordinary Lead and Worker sessions expose exactly five product facades: tachi_memory, tachi_task, tachi_staff, tachi_gh, and tachi_a2a. Retained diagnostics and compatibility routes are available only to explicitly selected, authorized Ops/admin sessions. Tachi is not the default worker launcher: use the host harness's native subagent for ordinary local delegation. A Tachi-owned worker launch is only for an explicit user request, durable cross-session work, cross-device/remote pickup, or when no native subagent is available (use tachi_staff(action='start', task=..., staffing_reason=...)). \
WORKFLOW: (1) briefing at session start: tachi_memory(action='briefing'). \
    (1b) issue/PR lifecycle navigation when a flow, issue, or PR exists: tachi_task(action='status', flow_id=..., issue_ref=..., or pr_ref=...) before PR handoff or close-loop; the cycle view is nested under status; call tachi_staff(action='start', task=..., staffing_reason=...) only for an explicit native-first exception. \
(2) save PROACTIVELY after any meaningful milestone — decision made, root cause found, sub-task done, key command confirmed. Do NOT wait until session end: tachi_memory(action='save', text=…, path='/scratch/…' or '/code-review/…', keywords=[tags], entities=[repos/modules]). Bound sessions should omit project; an explicit same-DB alias is normalized to the immutable bound identity, while other-project writes and destructive actions remain forbidden. \
(3) checkpoint for mid-task pause or handoff (not a substitute for save): action='checkpoint'. \
(4) extract_facts to atomize raw text/logs via LLM into N searchable facts: action='extract_facts'. \
Cursor/Windsurf have no auto-capture — you must call save explicitly. OpenClaw auto-captures on agent_end. \
HTTP direct-connect: bind project and an ordinary profile with X-Tachi-Project / X-Tachi-Profile (or initialize meta tachiProject/tachiProfile); caller-supplied metadata cannot authorize Ops/admin. \
On daemon restart re-run initialize (new session id); stdio adapters are a permanent compatibility layer. See docs/engineering/architecture/http-direct-connect.md."
        .to_string()
}
