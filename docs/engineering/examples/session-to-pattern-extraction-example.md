# Session Capture → Pattern Extraction: A Real Example

**Session date:** 2026-06-23
**Participants:** user + Kimi Code CLI
**Artifact produced:** [`tachi-continuity-memory-architecture.md`](../architecture/tachi-continuity-memory-architecture.md)
**Purpose:** demonstrate how a real design conversation maps into the continuity memory pipeline.
**Implementation note:** as of the first implementation slice, projected patterns can be searched with `tachi_search scope=patterns`, attached to wiki metadata with `include_patterns=true`, and turned into disabled/pending skill candidates with `tachi_skill action=from_pattern`.

---

## 1. Session timeline

This is the credibility history of the conclusions reached. Each node is a judgment state; edges are transitions caused by new evidence or correction.

```
[START]
  │
  ▼
[Node 1] User asks about "linear memory" in docs
         → AI finds pattern-timeline-bonding-memory.md
         │
         ▼
[Node 2] AI gives cold review: direction is reasonable,
         but bonding ROI and timeline query path need scrutiny
         │
         ▼
[Node 3] User shares Gabbard/Butler case study as real-time
         verification of pattern/timeline/bonding value
         │
         ▼
[Node 4] AI misreads the case as primarily political analysis;
         user corrects: "I'm building a general-purpose memory body"
         │
         ▼
[Node 5] User reveals core insight: model alignment is vendor-defined
         and may not match user alignment; memory system must bridge
         vendor alignment → user alignment
         │
         ▼
[Node 6] User explains bonding: not emotion/RLHF, but a shared
         communication protocol; warmth is a carrier, not the thing
         │
         ▼
[Node 7] AI updates docs to reflect the alignment-bridge framing
         and the warmth-as-carrier nuance
         │
         ▼
[Node 8] AI maps current code implementation status; identifies
         9 gaps versus the architecture doc
         │
         ▼
[Node 9] User asks how backend models (DeepSeek V4 Flash/Pro, Qwen)
         should be used across the memory lanes
         │
         ▼
[Node 10] AI recommends fast models for extract/summary/matching,
          strong models for reasoning/distill/labeling
          │
          ▼
[Node 11] User asks how memory/wiki/skill integrate with pattern memory;
          AI finds Karpathy LLM Wiki reference and maps the flow
          Notes → Pattern → Wiki → Skill
          │
          ▼
[Node 12] User observes: this entire conversation is itself a
          microcosm of the memory system
          │
          ▼
[Node 13] AI writes this example document, closing the loop:
          the session becomes the first verified continuity-memory sample
```

### Key transitions

| From | To | Trigger | Evidence type |
|---|---|---|---|
| Node 2 → Node 3 | Skeptical review → stronger endorsement | Real-time WaPo verification | External anchor |
| Node 4 → Node 5 | Political case → general memory architecture | User correction | Testimonial |
| Node 6 → Node 7 | Bonding ≠ warmth → warmth is carrier | User refinement | Testimonial |
| Node 8 → Node 10 | Abstract design → concrete model allocation | Code inspection | Logic fact |
| Node 12 → Node 13 | Design conversation → self-referential sample | Meta-observation | Testimonial |

---

## 2. Core patterns extracted

The per-pattern counters in this section are human-facing evidence notes for the example. Current projector counters are shown later: candidate projection starts with `seen=1`, `hit=0`, and only later hit/miss callback events move confidence.

### Pattern 1: `alignment_bridge`

**Statement:** The memory system is a dynamic bridge between vendor alignment and user alignment, not a "remember more" cache.

**Instances in this session:**
- "模型的 alignment 不一定是对着用户想要的方向对齐的"
- "memory system is a dynamic, auditable bridge between vendor alignment and user alignment"

**Structure:**
```
base model (vendor alignment)
    ↓ gap
user alignment (values, judgment habits, evidence thresholds)
    ↓ bridge
pattern memory + timeline memory + bonding layer + safeguards
```

**Counters:**
- `seen`: 1 session
- `hit`: 1 (user confirmed and expanded)
- `confidence`: medium-high (testimonial, but repeatedly reinforced)

**Authority:** `CollectOnly` until validated across more sessions/users.

---

### Pattern 2: `bonding_protocol_warmth_carrier`

**Statement:** Bonding is a per-user shared communication protocol. Warmth/affect is a legitimate carrier that can make bonding land more effectively, but it is not bonding itself.

**Instances in this session:**
- "bonding 不是 warmth"
- "4 个字能顶几百个字的信息量"
- "情绪（affect/warmth）是让 bonding 生效的重要载体"
- "bonding 可以在 cold carrier 下存在"

**Structure:**
```
bonding = shared context + shorthand + history
    ↓ delivered via
warmth (one carrier) or cold precision (another carrier)
    ↓
communication bandwidth compression
```

**Counters:**
- `seen`: 1 session
- `hit`: 1
- `confidence`: high (user corrected AI and refined the distinction)

**Authority:** `CollectOnly` → likely `ReviewSignalOnly` after cross-session recurrence.

---

### Pattern 3: `labeler_first`

**Statement:** In the conversation domain there is no free `fwd_return` (like market prices in quant). Outcome labels must be judged by a fallible labeler, and label quality must be validated before any counter or brake is trusted.

**Instances in this session:**
- "对话域没有量化里的 fwd_return"
- "build the session-end LLM labeler first"
- "label-quality calibration is not complete"

**Structure:**
```
conversation outcome
    ↓
LLM labeler (ReviewSignalOnly)
    ↓
label_eval vs gold labels
    ↓
trusted challenge_rate → over-fit brake
```

**Counters:**
- `seen`: repeated across this session and original design doc
- `hit`: high
- `confidence`: high (grounded in quant analogy + explicit calibration requirement)

**Authority:** `ReviewSignalOnly`.

---

### Pattern 4: `pattern_not_content_memory`

**Statement:** Pattern memory stores the user's cognitive and judgment structures, not merely a catalog of facts.

**Instances in this session:**
- "Pattern memory 不是 content memory"
- "它存的是用户的认知和判断结构"
- Gabbard/Butler case: pattern predicted the fourth instance

**Structure:**
```
content memory = "we discussed X"
pattern memory = "we identified structure Y that predicts Z"
```

**Counters:**
- `seen`: 1 session
- `hit`: 1 (case study + user confirmation)
- `confidence`: high (external verification in case study)

**Authority:** `CollectOnly`.

---

### Pattern 5: `crystallization_pipeline`

**Statement:** Raw sessions are distilled into patterns, which are reviewed and promoted into wiki pages, which are further crystallized into executable skills.

**Instances in this session:**
- Karpathy LLM Wiki flow: Raw Sources → Wiki → Schema
- Tachi mapping: Notes → Wiki → Skill
- Proposed flow: session → pattern → wiki draft → skill
- "pattern memory 是 skill 的证据驱动发现层"

**Structure:**
```
session / note / memory (raw)
    ↓ distill
pattern candidate (/user/patterns/*)
    ↓ mature + review
wiki draft (/wiki/drafts/patterns/*)
    ↓ approve
wiki page (/wiki/decision/ or /wiki/runbook/)
    ↓ executable化
skill:<name> (Hub)
```

**Counters:**
- `seen`: 1 session
- `hit`: 1
- `confidence`: medium (matches existing Tachi skill system + Karpathy reference, but promotion gate not yet exercised)

**Authority:** `CollectOnly`.

---

## 3. What the continuity pipeline would emit

### `session.captured` event

Current implementation emits `session.captured` as a raw capture marker. It is timeline/project-cycle evidence, not an outcome label and not a pattern hit.

```json
{
  "event_type": "session.captured",
  "session_id": "2026-06-23-tachi-continuity-design",
  "actor": "kimi-code-cli",
  "authority": "RawFact",
  "projection_hints": ["Timeline", "ProjectCycle"],
  "payload": {
    "conversation_id": "2026-06-23-tachi-continuity-design",
    "turn_id": "continuity-architecture",
    "agent_id": "kimi-code-cli",
    "path_prefix": "/sessions/2026-06-23",
    "captured_memory_ids": ["..."],
    "message_count": 42
  }
}
```

### `pattern.candidate` events

With `TACHI_CONTINUITY_PIPELINE=1`, the distill lane can emit five candidate events, one per pattern above, with `projection_hints = ["Pattern"]`, `authority = "CollectOnly"`, and `effects = ["None"]`.

### `session.outcome` event

```json
{
  "event_type": "session.outcome",
  "authority": "ReviewSignalOnly",
  "projection_hints": ["Outcome", "EvidenceGate"],
  "payload": {
    "outcome": "UserCorrectedAiExpanded",
    "evidence_basis": "testimonial_plus_external_reference",
    "confidence": 0.85,
    "rationale": "User corrected AI's initial framing (bonding vs warmth, political vs general memory) and provided the core alignment-bridge insight. AI expanded and documented.",
    "claims": [
      "memory system bridges vendor and user alignment",
      "bonding is protocol; warmth is carrier",
      "pattern memory stores user judgment structures"
    ],
    "open_questions": [
      "Does this alignment-bridge pattern generalize beyond this user?",
      "What is the exact promotion threshold from pattern to wiki/skill?"
    ]
  }
}
```

### After `tachi_event action=project`

The five pattern candidates would be materialized into stable memory projections:

```
/user/patterns/session/<hash12>
/user/patterns/session/<hash12>
/user/patterns/session/<hash12>
/user/patterns/session/<hash12>
/user/patterns/session/<hash12>
```

The stable human-readable key remains in `metadata.projection_key`; the current path shape is domain plus a stable hash.

Each with metadata counters:

```json
{
  "seen": 1,
  "hit": 0,
  "miss": 0,
  "confidence": 0.0,
  "last_seen": "2026-06-23T05:15:00Z"
}
```

Later `pattern.hit`, `pattern.miss`, or `callback_hit` events update hit/miss counters and make `confidence = hit / seen`. Candidate extraction alone is not treated as validation.

---

## 4. Bonding layer samples

From this session, the bonding layer would capture:

```json
{
  "/user/patterns/bonding/session/<hash12-a>": {
    "projection_key": "alignment_bridge",
    "origin": "user correction during 2026-06-23 continuity memory design",
    "meaning": "When discussing memory systems, first check whether the design bridges vendor alignment and user alignment.",
    "callback_hits": 1,
    "shorthand_triggers": ["alignment", "vendor alignment", "user alignment", "bridge"],
    "appropriate_contexts": ["memory design", "model alignment discussions", "Tachi architecture"],
    "inappropriate_contexts": ["unrelated coding tasks"]
  },
  "/user/patterns/bonding/session/<hash12-b>": {
    "projection_key": "bonding_carrier",
    "origin": "user refinement of bonding definition",
    "meaning": "Bonding is the shared protocol; warmth is one carrier. Do not conflate them, but do not dismiss warmth either.",
    "callback_hits": 1,
    "shorthand_triggers": ["bonding", "warmth", "carrier", "RLHF"],
    "appropriate_contexts": ["bonding design", "affect guardrails", "user communication style"],
    "inappropriate_contexts": ["purely factual recall"]
  }
}
```

---

## 5. Crystallization artifacts

### Wiki draft generated from pattern

```markdown
---
pattern_ref: /user/patterns/alignment_bridge
status: draft
reviewed_at: null
---

# Alignment Bridge

## Conclusion
The continuity memory system is a bridge between vendor-defined model alignment and the user's actual alignment.

## Root Cause
Base models are aligned by vendors to generic helpfulness/safety/honesty. This does not automatically match what a specific user values, how they judge evidence, or what serves their long-term interests.

## Proposal
Build pattern memory, timeline memory, and bonding layer as an auditable, per-user alignment bridge, with over-fit brake and cold seat as safeguards.

## Counter-Proposal
Do not treat memory as a "remember more" cache or as a way to make the model more agreeable.

## Verification
- Label-quality calibration must pass before trusting challenge_rate.
- Cold seat must remain independent of warm-coalition conclusions.
```

### Skill candidate generated from pattern

Current implementation path: call `tachi_skill(action="from_pattern", query=..., args={"skill_id": "...", "name": "..."})`. The generated Hub capability starts `enabled=false`, `review_status=pending`, and `policy.visibility=discoverable`; promotion to `listed` remains a human/maturity-gated step.

```json
{
  "id": "skill:continuity-memory-design-check",
  "name": "continuity-memory-design-check",
  "description": "Verify that a proposed memory feature bridges vendor alignment and user alignment.",
  "system": "You are reviewing a proposed Tachi memory feature. Check whether it bridges vendor alignment and user alignment, and whether it includes safeguards against over-fitting.",
  "prompt": "Review the following memory-system proposal. Does it explicitly address:\n1. Vendor alignment vs user alignment gap?\n2. Pattern / timeline / bonding separation?\n3. Over-fit brake and cold seat?\n4. How pattern memory differs from content memory?\n\nProposal:\n{{proposal}}",
  "policy": { "visibility": "discoverable" },
  "tags": ["pattern-derived", "memory", "alignment", "review"],
  "pattern_ref": "/user/patterns/alignment_bridge",
  "inputSchema": {
    "type": "object",
    "required": ["proposal"],
    "properties": {
      "proposal": { "type": "string" }
    }
  }
}
```

---

## 6. Falsifiability / next checks

To verify these patterns are real and not session-specific hallucinations:

1. **alignment_bridge**
   - Next time the user discusses a model-behavior feature, check if this framing appears spontaneously.
   - If the user never uses it again, demote confidence.

2. **bonding_protocol_warmth_carrier**
   - Test whether warmth-only replies without shared history feel shallow to the user.
   - Test whether cold-carrier replies with strong shared history feel efficient.

3. **labeler_first**
   - Run `tachi_event action=label_eval` on held-out transcripts.
   - If labeler agreement is low, this pattern becomes even more important, not less.

4. **pattern_not_content_memory**
   - Monitor whether pattern memory predicts future instances better than content memory alone.

5. **crystallization_pipeline**
   - Try to generate a skill from this pattern and see if it is useful in a future session.

---

## 7. What this example proves

This session is not a hypothetical. It is a real design conversation that:

- Started with a vague request ("look at linear memory in docs")
- Evolved through correction and external reference
- Produced a concrete architecture document
- Generated extractable, reusable patterns
- Became self-referential ("this conversation is a memory microcosm")

If the continuity memory system cannot capture and crystallize this session, it cannot capture the work it is meant to support.
