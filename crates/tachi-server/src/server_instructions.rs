/// MCP `initialize` instructions (also mirrored in agent rule blocks).
pub(crate) fn mcp_server_instructions() -> String {
    "Tachi — memory + policy/ledger copilot for coding agents, not the default worker launcher. Use the host harness's native subagent for ordinary local delegation. Tachi dispatch is only for an explicit user request, durable cross-session work, cross-device/remote pickup, or when no native subagent is available. \
WORKFLOW: (1) briefing at session start: tachi_memory(action='briefing'). \
(1b) issue/PR lifecycle navigation when a flow, issue, or PR exists: tachi_task(action='cycle_plan', flow_id=..., issue_ref=..., or pr_ref=...) before advisory recommend, PR handoff, or close-loop; call dispatch only for an explicit native-first exception and provide dispatch_reason. \
(2) save PROACTIVELY after any meaningful milestone — decision made, root cause found, sub-task done, key command confirmed. Do NOT wait until session end: tachi_memory(action='save', text=…, path='/scratch/…' or '/code-review/…', keywords=[tags], entities=[repos/modules]). Bound sessions should omit project; an explicit same-DB alias is normalized to the immutable bound identity, while other-project writes and destructive actions remain forbidden. \
(3) checkpoint for mid-task pause or handoff (not a substitute for save): action='checkpoint'. \
(4) extract_facts to atomize raw text/logs via LLM into N searchable facts: action='extract_facts'. \
Cursor/Windsurf have no auto-capture — you must call save explicitly. OpenClaw auto-captures on agent_end. \
Wiki for stable reusable knowledge: tachi_wiki(action='write'). \
Skills: tachi_skill(action='discover') before solving complex problems. \
Diagnostics: tachi_status. \
HTTP direct-connect: bind project/profile with X-Tachi-Project / X-Tachi-Profile (or initialize meta tachiProject/tachiProfile). \
On daemon restart re-run initialize (new session id); stdio adapters are a permanent compatibility layer. See docs/engineering/architecture/http-direct-connect.md."
        .to_string()
}
