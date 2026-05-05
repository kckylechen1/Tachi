# tachi.agent_brief.v4_review

status.current:
- phase_1_hub_governance: done
- phase_2_sandbox_executor: done
- phase_3_ghost_persistence: done
- phase_4_virtual_capability: done
- phase_5_multi_agent_ops: done
- release: v0.11.0
- remaining.primary: [electron_shell_daemon_management, app_installer_governance_runtime_loop]

why.this.work.exists:
- current callers still depend too directly on concrete capability ids like `mcp:exa` or `mcp:context7`
- that leaks backend choice into callers and makes replacement / routing / audit harder
- app layer still lacks a full install flow: discover -> propose -> approve -> activate -> smoke_test -> rollback
- project-isolated DB creation needed hot-swap support (cannot restart daemon per project switch)
- multi-agent workflows (Claude Code, OpenClaw, Codex, Cursor) need coordination primitives
- skill prompts degrade over time; need LLM-assisted evolution with version tracking
- runaway agents need rate limiting and identity tracking

what.we.delivered.in.v0.11.0:
  feature_1_project_db_hot_activation:
    - ProjectDbState struct with hot_project_db field on MemoryServer
    - activate_project_db() / has_project_db() / with_hot_project_store()
    - replaced ~30 static project_db_path.is_some() checks
    - no daemon restart required when switching projects
  feature_2_hub_export_skills:
    - hub_export_skills tool with agent-specific exporters
    - supports claude (SKILL.md + symlinks), openclaw (plugin manifest), cursor (.mdc rules), generic (raw files)
    - visibility filtering, agent-local scope filtering, clean mode
  feature_3_rate_limiter:
    - per-session sliding window RPM + burst/loop detection
    - identical tool+args repeated > burst threshold triggers block
    - configurable via RATE_LIMIT_RPM / RATE_LIMIT_BURST env vars
    - agent profile can override per-agent
  feature_4_electron_shell:
    - NOT IMPLEMENTED — deferred (requires npm/package restructuring)
  feature_5_skill_evolve:
    - LLM-powered skill prompt improvement via skill_evolve tool
    - creates versioned capabilities (skill:name/vN)
    - optional auto-activation via hub_version_routes
    - feedback recording integrated into handle_run_skill
  feature_6_agent_profile:
    - agent_register and agent_whoami tools
    - per-session AgentProfile (name, role, rate limit overrides)
    - integrates with rate limiter for per-agent policy
  feature_7_cross_agent_handoff:
    - handoff_leave and handoff_check tools
    - target filtering, acknowledgment, priority levels
    - memory persistence (handoff: category entries)
    - LRU eviction at 50 memos

code.review.fixes.applied:
  - C-1: project_db_path clone eliminated (subsumed by hot-activation)
  - I-1: metadata error propagation in virtual_capability.rs (no more unwrap_or_default)
  - I-2: cross-scope VC shadowing guard in vc_register
  - M-1: safe version_pin i64->u32 cast via try_into().unwrap_or(0)
  - M-2: VC auto-approval comment documenting design decision
  - M-3: hub_discover_inner refactor removing double serde round-trip
  - M-6: already handled by configuredBaseUrls in frontend

tests:
  total: 32
  new_in_v0.11.0: 11
  categories:
    - rate limiter (burst detection, different args, RPM blocking, agent profile overrides)
    - agent profile (register/whoami roundtrip)
    - handoff (leave/check roundtrip, untargeted memo visibility, persistence)
    - export skills (empty, unknown agent, generic)

what.we.still.want:
- electron shell with daemon lifecycle management (Feature 4)
- app installer governance runtime loop (discover -> propose -> approve -> activate -> smoke_test -> rollback)
- vc_mapping_ui in desktop frontend
- review_queue UI
- smoke_test_rollback_flow
- M-4 (tempfile for test cleanup) — minor, deferred
- M-5 (end-to-end hub_call through VC integration test) — deferred

minimum.acceptance.for.v4:
- registry for virtual capabilities ✓
- binding table from virtual capability -> concrete capability ✓
- deterministic resolve path returning chosen backend ✓
- version pin support ✓
- review/enabled state on virtual capability ✓
- `hub_call` can accept a virtual capability id and route to concrete MCP ✓
- route result exposes both requested id and resolved concrete id ✓

minimum.acceptance.for.project_db:
- explicit tool to create/init project DB under current or target repo ✓
- tool returns path + scope + whether newly created ✓
- daemon/runtime can use the project DB path deterministically ✓
- hot-swap without daemon restart ✓ (NEW in v0.11.0)
